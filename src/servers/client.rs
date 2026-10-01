//! One server, reached over its LAN URL or its remote URL.
//!
//! Every call tries the URL that worked last, then the other one. A LAN URL
//! gets a short timeout because at home it answers at once and away from
//! home it never will. A remote URL must be HTTPS: token auth over plain HTTP
//! on the internet would hand out a replayable token, so a plain-HTTP remote
//! URL is never contacted.

use super::api;
use super::error::ServerError;
use super::request::{self, Credentials};
use super::transport::Transport;
use std::path::Path;
use std::sync::Mutex;

/// Seconds to wait for the LAN URL.
pub const LAN_TIMEOUT_SECS: u64 = 2;
/// Seconds to wait for the remote URL.
pub const REMOTE_TIMEOUT_SECS: u64 = 5;
/// Seconds a whole-file download may take. minreq's timeout covers the entire
/// request, so a download cannot use the short timeouts above; the route is
/// picked with a quick `ping` first instead.
pub const DOWNLOAD_TIMEOUT_SECS: u64 = 1800;

/// Which of a server's two URLs answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Lan,
    Remote,
}

/// Talks to one server.
pub struct ServerClient<T: Transport> {
    lan_url: Option<String>,
    remote_url: Option<String>,
    creds: Credentials,
    transport: T,
    last_good: Mutex<Option<Route>>,
}

impl<T: Transport> ServerClient<T> {
    pub fn new(
        lan_url: Option<String>,
        remote_url: Option<String>,
        creds: Credentials,
        transport: T,
    ) -> Self {
        ServerClient { lan_url, remote_url, creds, transport, last_good: Mutex::new(None) }
    }

    /// The transport, for tests that inspect what was sent.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// The route that answered most recently, if any.
    pub fn last_route(&self) -> Option<Route> {
        *self.last_good.lock().unwrap()
    }

    /// `ping`: who the server is.
    pub fn ping(&self) -> Result<api::ServerInfo, ServerError> {
        self.call("ping", &[], api::parse_ping)
    }

    /// One page of the catalog: `search3` with an empty query, songs only.
    pub fn search3_songs(
        &self,
        offset: u64,
        count: u64,
    ) -> Result<Vec<api::ServerSong>, ServerError> {
        let (count, offset) = (count.to_string(), offset.to_string());
        self.call(
            "search3",
            &[
                ("query", ""),
                ("artistCount", "0"),
                ("albumCount", "0"),
                ("songCount", &count),
                ("songOffset", &offset),
            ],
            api::parse_search3_songs,
        )
    }

    /// `getScanStatus`.
    pub fn scan_status(&self) -> Result<api::ScanStatus, ServerError> {
        self.call("getScanStatus", &[], api::parse_scan_status)
    }

    /// `getOpenSubsonicExtensions`.
    pub fn extensions(&self) -> Result<Vec<api::Extension>, ServerError> {
        self.call("getOpenSubsonicExtensions", &[], api::parse_extensions)
    }

    /// `getMusicFolders`.
    pub fn music_folders(&self) -> Result<Vec<api::MusicFolder>, ServerError> {
        self.call("getMusicFolders", &[], api::parse_music_folders)
    }

    /// `getPlaylists`.
    pub fn playlists(&self) -> Result<Vec<api::ServerPlaylist>, ServerError> {
        self.call("getPlaylists", &[], api::parse_playlists)
    }

    /// `getPlaylist`.
    pub fn playlist(
        &self,
        id: &str,
    ) -> Result<(api::ServerPlaylist, Vec<api::ServerSong>), ServerError> {
        self.call("getPlaylist", &[("id", id)], api::parse_playlist)
    }

    /// `setRating`: 1 to 5, or 0 to clear.
    pub fn set_rating(&self, song_id: &str, rating: u8) -> Result<(), ServerError> {
        let rating = rating.min(5).to_string();
        self.call("setRating", &[("id", song_id), ("rating", &rating)], api::parse_ok)
    }

    /// `scrobble` with `submission=true`: one play per entry, each with its
    /// play time in milliseconds since the Unix epoch, so plays queued while
    /// offline land at the time they happened.
    pub fn scrobble(&self, plays: &[(String, i64)]) -> Result<(), ServerError> {
        let mut params: Vec<(&str, String)> = Vec::new();
        for (id, at_ms) in plays {
            params.push(("id", id.clone()));
            params.push(("time", at_ms.to_string()));
        }
        params.push(("submission", "true".into()));
        self.call("scrobble", &borrowed(&params), api::parse_ok)
    }

    /// `createPlaylist` with a `playlistId`: replace the whole song list.
    pub fn replace_playlist(
        &self,
        playlist_id: &str,
        song_ids: &[String],
    ) -> Result<(), ServerError> {
        let mut params: Vec<(&str, String)> = vec![("playlistId", playlist_id.to_string())];
        params.extend(song_ids.iter().map(|id| ("songId", id.clone())));
        self.call("createPlaylist", &borrowed(&params), api::parse_ok)
    }

    /// `createPlaylist` with a name: a new playlist. Returns its ID when the
    /// server reports it (API 1.14 and later do).
    pub fn create_playlist(
        &self,
        name: &str,
        song_ids: &[String],
    ) -> Result<Option<String>, ServerError> {
        let mut params: Vec<(&str, String)> = vec![("name", name.to_string())];
        params.extend(song_ids.iter().map(|id| ("songId", id.clone())));
        self.call("createPlaylist", &borrowed(&params), api::parse_created_playlist_id)
    }

    /// `deletePlaylist`. Removes the list only; songs are untouched.
    pub fn delete_playlist(&self, playlist_id: &str) -> Result<(), ServerError> {
        self.call("deletePlaylist", &[("id", playlist_id)], api::parse_ok)
    }

    /// Download the original file of `song_id` (`stream` with `format=raw`,
    /// which needs no download permission) to `dest`.
    ///
    /// The body goes to `dest` plus `.part` and is renamed only when
    /// complete, so a dropped connection never leaves half a song under the
    /// real name.
    pub fn download_original(&self, song_id: &str, dest: &Path) -> Result<u64, ServerError> {
        self.download("stream", &[("id", song_id), ("format", "raw")], dest)
    }

    /// Download cover art `cover_id` scaled to `size` pixels to `dest`.
    pub fn download_cover(&self, cover_id: &str, size: u32, dest: &Path) -> Result<u64, ServerError> {
        let size = size.to_string();
        self.download("getCoverArt", &[("id", cover_id), ("size", &size)], dest)
    }

    /// Stream `endpoint`'s body to `dest` via a `.part` file.
    fn download(
        &self,
        endpoint: &str,
        params: &[(&str, &str)],
        dest: &Path,
    ) -> Result<u64, ServerError> {
        self.ping()?;
        let base = match self.last_route() {
            Some(Route::Lan) => self.lan_url.as_deref(),
            Some(Route::Remote) => self.remote_url.as_deref(),
            None => None,
        }
        .ok_or_else(|| ServerError::unreachable("no usable URL configured"))?;
        let url = request::build_url(base, endpoint, &self.creds, &request::new_salt(), params);
        let mut part = dest.as_os_str().to_owned();
        part.push(".part");
        let part = std::path::PathBuf::from(part);

        let outcome = match self.transport.get_to_file(&url, DOWNLOAD_TIMEOUT_SECS, &part) {
            Err(e) => Err(e),
            Ok(dl) if dl.status != 200 => Err(ServerError::Http(dl.status)),
            // Subsonic reports a failed stream as a normal response body.
            Ok(dl)
                if dl
                    .content_type
                    .as_deref()
                    .is_some_and(|t| t.contains("json") || t.contains("xml")) =>
            {
                let body = std::fs::read_to_string(&part).unwrap_or_default();
                Err(api::parse_ok(&body).err().unwrap_or(ServerError::NotSubsonic))
            }
            Ok(dl) => std::fs::rename(&part, dest)
                .map(|_| dl.bytes)
                .map_err(|e| ServerError::unreachable(&format!("cannot save download: {e}"))),
        };
        if outcome.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        outcome
    }

    /// The URLs to try, in order: the one that answered last goes first.
    fn routes(&self) -> Vec<(Route, &str, u64)> {
        let mut routes = Vec::new();
        if let Some(url) = &self.lan_url {
            routes.push((Route::Lan, url.as_str(), LAN_TIMEOUT_SECS));
        }
        if let Some(url) = &self.remote_url {
            if url.to_ascii_lowercase().starts_with("https://") {
                routes.push((Route::Remote, url.as_str(), REMOTE_TIMEOUT_SECS));
            }
        }
        if self.last_route() == Some(Route::Remote) {
            routes.reverse();
        }
        routes
    }

    /// GET `endpoint` on the first route that answers and parse the body.
    ///
    /// A failure that means "not available" moves on to the next route. Any
    /// other failure came from a server that did answer, so it is returned
    /// at once: a wrong password is just as wrong on the other URL.
    fn call<R>(
        &self,
        endpoint: &str,
        params: &[(&str, &str)],
        parse: fn(&str) -> Result<R, ServerError>,
    ) -> Result<R, ServerError> {
        let mut last_err = ServerError::unreachable("no usable URL configured");
        for (route, base, timeout) in self.routes() {
            let url = request::build_url(base, endpoint, &self.creds, &request::new_salt(), params);
            let result = self.transport.get(&url, timeout).and_then(|resp| {
                if resp.status != 200 {
                    return Err(ServerError::Http(resp.status));
                }
                let body = String::from_utf8(resp.body).map_err(|_| ServerError::NotSubsonic)?;
                parse(&body)
            });
            match result {
                Err(e) if e.is_offline() => last_err = e,
                other => {
                    *self.last_good.lock().unwrap() = Some(route);
                    return other;
                }
            }
        }
        Err(last_err)
    }
}

fn borrowed<'a>(params: &'a [(&'a str, String)]) -> Vec<(&'a str, &'a str)> {
    params.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::servers::transport::{Download, HttpResponse};

    const PING_OK: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
        "type":"navidrome","serverVersion":"0.64.2","openSubsonic":true}}"#;
    const BAD_PASSWORD: &str = r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
        "error":{"code":40,"message":"Wrong username or password"}}}"#;

    /// Answers by URL prefix and records every call (redacted URL, timeout).
    struct Fake {
        answers: Vec<(&'static str, Result<HttpResponse, ServerError>)>,
        calls: Mutex<Vec<(String, u64)>>,
        full_urls: Mutex<Vec<String>>,
        /// Download answers by prefix: status, content type, body, and
        /// whether the connection drops after writing the body.
        downloads: Vec<(&'static str, (u16, &'static str, Vec<u8>, bool))>,
    }

    impl Fake {
        fn new(answers: Vec<(&'static str, Result<HttpResponse, ServerError>)>) -> Self {
            Fake {
                answers,
                calls: Mutex::new(Vec::new()),
                full_urls: Mutex::new(Vec::new()),
                downloads: Vec::new(),
            }
        }

        fn with_download(
            mut self,
            prefix: &'static str,
            answer: (u16, &'static str, Vec<u8>, bool),
        ) -> Self {
            self.downloads.push((prefix, answer));
            self
        }
    }

    /// The request parameters of the last call, minus auth and the fixed
    /// client parameters, in order.
    fn last_params(c: &ServerClient<Fake>) -> Vec<(String, String)> {
        let url = c.transport.full_urls.lock().unwrap().last().cloned().unwrap();
        let query = url.split_once('?').unwrap().1;
        query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .filter(|(k, _)| !["u", "t", "s", "v", "c", "f", "apiKey"].contains(k))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn p(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    impl Transport for Fake {
        fn get(&self, url: &str, timeout_secs: u64) -> Result<HttpResponse, ServerError> {
            self.full_urls.lock().unwrap().push(url.to_string());
            self.calls.lock().unwrap().push((request::redact_url(url), timeout_secs));
            for (prefix, answer) in &self.answers {
                if url.starts_with(prefix) {
                    return answer.clone();
                }
            }
            Err(ServerError::unreachable("no route"))
        }

        fn get_to_file(
            &self,
            url: &str,
            timeout_secs: u64,
            dest: &Path,
        ) -> Result<Download, ServerError> {
            self.full_urls.lock().unwrap().push(url.to_string());
            self.calls.lock().unwrap().push((request::redact_url(url), timeout_secs));
            for (prefix, answer) in &self.downloads {
                if url.starts_with(prefix) {
                    let (status, ctype, body, fail_after) = answer;
                    std::fs::write(dest, body).unwrap();
                    if *fail_after {
                        return Err(ServerError::unreachable("connection reset"));
                    }
                    return Ok(Download {
                        status: *status,
                        content_type: Some(ctype.to_string()),
                        bytes: body.len() as u64,
                    });
                }
            }
            Err(ServerError::unreachable("no route"))
        }
    }

    fn ok(body: &str) -> Result<HttpResponse, ServerError> {
        Ok(HttpResponse { status: 200, body: body.as_bytes().to_vec() })
    }

    fn down() -> Result<HttpResponse, ServerError> {
        Err(ServerError::unreachable("connection refused"))
    }

    const LAN: &str = "http://oscar.local:4533";
    const REMOTE: &str = "https://music.example.com";

    fn client(fake: Fake) -> ServerClient<Fake> {
        ServerClient::new(
            Some(LAN.into()),
            Some(REMOTE.into()),
            Credentials::Password { username: "me".into(), password: "sesame".into() },
            fake,
        )
    }

    fn calls(c: &ServerClient<Fake>) -> Vec<(String, u64)> {
        c.transport.calls.lock().unwrap().clone()
    }

    #[test]
    fn at_home_only_the_lan_url_is_contacted() {
        let c = client(Fake::new(vec![(LAN, ok(PING_OK)), (REMOTE, ok(PING_OK))]));
        assert_eq!(c.ping().unwrap().server_type.as_deref(), Some("navidrome"));
        assert_eq!(calls(&c), vec![("http://oscar.local:4533/rest/ping".into(), 2)]);
        assert_eq!(c.last_route(), Some(Route::Lan));
    }

    #[test]
    fn away_from_home_falls_back_to_remote_and_remembers_it() {
        let c = client(Fake::new(vec![(LAN, down()), (REMOTE, ok(PING_OK))]));
        c.ping().unwrap();
        c.ping().unwrap();
        assert_eq!(
            calls(&c),
            vec![
                ("http://oscar.local:4533/rest/ping".into(), 2),
                ("https://music.example.com/rest/ping".into(), 5),
                ("https://music.example.com/rest/ping".into(), 5),
            ]
        );
        assert_eq!(c.last_route(), Some(Route::Remote));
    }

    #[test]
    fn server_in_maintenance_behind_a_proxy_falls_back_then_reports_offline() {
        let c = client(Fake::new(vec![
            (LAN, down()),
            (REMOTE, Ok(HttpResponse { status: 503, body: b"<html>maintenance</html>".to_vec() })),
        ]));
        let err = c.ping().unwrap_err();
        assert_eq!(err, ServerError::Http(503));
        assert!(err.is_offline());
    }

    #[test]
    fn a_captive_portal_on_the_lan_url_falls_through_to_remote() {
        let c = client(Fake::new(vec![(LAN, ok("<html>hotel wifi</html>")), (REMOTE, ok(PING_OK))]));
        assert!(c.ping().is_ok());
        assert_eq!(c.last_route(), Some(Route::Remote));
    }

    #[test]
    fn a_refused_password_is_not_retried_on_the_other_url() {
        let c = client(Fake::new(vec![(LAN, ok(BAD_PASSWORD)), (REMOTE, ok(PING_OK))]));
        assert!(matches!(c.ping(), Err(ServerError::Auth { code: 40, .. })));
        assert_eq!(calls(&c).len(), 1);
    }

    #[test]
    fn a_plain_http_remote_url_is_never_contacted() {
        let c = ServerClient::new(
            Some(LAN.into()),
            Some("http://music.example.com".into()),
            Credentials::Password { username: "me".into(), password: "sesame".into() },
            Fake::new(vec![(LAN, down()), ("http://music.example.com", ok(PING_OK))]),
        );
        assert!(c.ping().unwrap_err().is_offline());
        assert_eq!(calls(&c), vec![("http://oscar.local:4533/rest/ping".into(), 2)]);
    }

    const SONG_PAGE: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
        "searchResult3":{"song":[{"id":"s1","title":"One"},{"id":"s2","title":"Two"}]}}}"#;

    #[test]
    fn a_catalog_page_asks_for_songs_only_from_an_offset() {
        let c = client(Fake::new(vec![(LAN, ok(SONG_PAGE))]));
        let songs = c.search3_songs(1000, 500).unwrap();
        assert_eq!(songs.len(), 2);
        assert_eq!(
            last_params(&c),
            p(&[
                ("query", ""),
                ("artistCount", "0"),
                ("albumCount", "0"),
                ("songCount", "500"),
                ("songOffset", "1000"),
            ])
        );
    }

    #[test]
    fn scan_status_extensions_folders_and_playlists_hit_their_endpoints() {
        let scan = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "scanStatus":{"scanning":false,"lastScan":"2026-09-29T03:00:00Z"}}}"#;
        let c = client(Fake::new(vec![(LAN, ok(scan))]));
        assert_eq!(c.scan_status().unwrap().last_scan.as_deref(), Some("2026-09-29T03:00:00Z"));
        assert!(calls(&c)[0].0.ends_with("/rest/getScanStatus"));

        let ext = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "openSubsonicExtensions":[{"name":"apiKeyAuthentication","versions":[1]}]}}"#;
        let c = client(Fake::new(vec![(LAN, ok(ext))]));
        assert_eq!(c.extensions().unwrap()[0].name, "apiKeyAuthentication");
        assert!(calls(&c)[0].0.ends_with("/rest/getOpenSubsonicExtensions"));

        let folders = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "musicFolders":{"musicFolder":[{"id":1,"name":"Music"}]}}}"#;
        let c = client(Fake::new(vec![(LAN, ok(folders))]));
        assert_eq!(c.music_folders().unwrap()[0].name, "Music");
        assert!(calls(&c)[0].0.ends_with("/rest/getMusicFolders"));

        let lists = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "playlists":{"playlist":[{"id":"pl1","name":"Road Trip"}]}}}"#;
        let c = client(Fake::new(vec![(LAN, ok(lists))]));
        assert_eq!(c.playlists().unwrap()[0].id, "pl1");
        assert!(calls(&c)[0].0.ends_with("/rest/getPlaylists"));

        let one = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "playlist":{"id":"pl1","name":"Road Trip","entry":[{"id":"s1","title":"One"}]}}}"#;
        let c = client(Fake::new(vec![(LAN, ok(one))]));
        assert_eq!(c.playlist("pl1").unwrap().1[0].id, "s1");
        assert_eq!(last_params(&c), p(&[("id", "pl1")]));
    }

    const EMPTY_OK: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;

    #[test]
    fn set_rating_sends_the_song_and_stars() {
        let c = client(Fake::new(vec![(LAN, ok(EMPTY_OK))]));
        c.set_rating("s1", 4).unwrap();
        assert!(calls(&c)[0].0.ends_with("/rest/setRating"));
        assert_eq!(last_params(&c), p(&[("id", "s1"), ("rating", "4")]));
    }

    #[test]
    fn scrobble_sends_each_play_with_its_time() {
        let c = client(Fake::new(vec![(LAN, ok(EMPTY_OK))]));
        c.scrobble(&[("s1".into(), 1759000000000), ("s2".into(), 1759000300000)]).unwrap();
        assert!(calls(&c)[0].0.ends_with("/rest/scrobble"));
        assert_eq!(
            last_params(&c),
            p(&[
                ("id", "s1"),
                ("time", "1759000000000"),
                ("id", "s2"),
                ("time", "1759000300000"),
                ("submission", "true"),
            ])
        );
    }

    #[test]
    fn replacing_a_playlist_sends_every_song_in_order() {
        let c = client(Fake::new(vec![(LAN, ok(EMPTY_OK))]));
        c.replace_playlist("pl1", &["s2".into(), "s1".into()]).unwrap();
        assert!(calls(&c)[0].0.ends_with("/rest/createPlaylist"));
        assert_eq!(
            last_params(&c),
            p(&[("playlistId", "pl1"), ("songId", "s2"), ("songId", "s1")])
        );
    }

    #[test]
    fn creating_a_playlist_returns_the_new_id() {
        let created = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "playlist":{"id":"pl9","name":"New","entry":[{"id":"s1","title":"One"}]}}}"#;
        let c = client(Fake::new(vec![(LAN, ok(created))]));
        assert_eq!(c.create_playlist("New", &["s1".into()]).unwrap(), Some("pl9".into()));
        assert_eq!(last_params(&c), p(&[("name", "New"), ("songId", "s1")]));
    }

    #[test]
    fn creating_on_an_old_server_that_returns_nothing_is_still_success() {
        let c = client(Fake::new(vec![(LAN, ok(EMPTY_OK))]));
        assert_eq!(c.create_playlist("New", &[]).unwrap(), None);
    }

    #[test]
    fn deleting_a_playlist_names_it() {
        let c = client(Fake::new(vec![(LAN, ok(EMPTY_OK))]));
        c.delete_playlist("pl1").unwrap();
        assert!(calls(&c)[0].0.ends_with("/rest/deletePlaylist"));
        assert_eq!(last_params(&c), p(&[("id", "pl1")]));
    }

    #[test]
    fn download_writes_the_original_file_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("song.mp3");
        let c = client(
            Fake::new(vec![(LAN, ok(PING_OK))])
                .with_download(LAN, (200, "audio/mpeg", b"ID3 audio bytes".to_vec(), false)),
        );
        assert_eq!(c.download_original("s1", &dest).unwrap(), 15);
        assert_eq!(std::fs::read(&dest).unwrap(), b"ID3 audio bytes");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no .part left behind");
        assert_eq!(last_params(&c), p(&[("id", "s1"), ("format", "raw")]));
        let (url, timeout) = calls(&c).last().cloned().unwrap();
        assert_eq!(url, "http://oscar.local:4533/rest/stream");
        assert_eq!(timeout, DOWNLOAD_TIMEOUT_SECS);
    }

    #[test]
    fn download_picks_the_working_route_before_starting() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("song.mp3");
        let c = client(
            Fake::new(vec![(LAN, down()), (REMOTE, ok(PING_OK))])
                .with_download(REMOTE, (200, "audio/flac", b"fLaC".to_vec(), false)),
        );
        c.download_original("s1", &dest).unwrap();
        assert_eq!(
            calls(&c).iter().map(|(u, _)| u.as_str()).collect::<Vec<_>>(),
            vec![
                "http://oscar.local:4533/rest/ping",
                "https://music.example.com/rest/ping",
                "https://music.example.com/rest/stream",
            ]
        );
    }

    #[test]
    fn a_json_error_instead_of_audio_is_an_error_and_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("song.mp3");
        let not_found = br#"{"subsonic-response":{"status":"failed","version":"1.16.1",
            "error":{"code":70,"message":"Song not found"}}}"#;
        let c = client(
            Fake::new(vec![(LAN, ok(PING_OK))])
                .with_download(LAN, (200, "application/json", not_found.to_vec(), false)),
        );
        assert_eq!(
            c.download_original("gone", &dest),
            Err(ServerError::Api { code: 70, message: "Song not found".into() })
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_connection_dropped_mid_download_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("song.mp3");
        let c = client(
            Fake::new(vec![(LAN, ok(PING_OK))])
                .with_download(LAN, (200, "audio/mpeg", b"half a so".to_vec(), true)),
        );
        assert!(c.download_original("s1", &dest).unwrap_err().is_offline());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn cover_art_downloads_as_a_sized_thumbnail() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("cover.jpg");
        let c = client(
            Fake::new(vec![(LAN, ok(PING_OK))])
                .with_download(LAN, (200, "image/jpeg", b"JFIF".to_vec(), false)),
        );
        c.download_cover("al-1", 300, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"JFIF");
        assert!(calls(&c).last().unwrap().0.ends_with("/rest/getCoverArt"));
        assert_eq!(last_params(&c), p(&[("id", "al-1"), ("size", "300")]));
    }

    #[test]
    fn download_from_an_unreachable_server_is_offline() {
        let dir = tempfile::tempdir().unwrap();
        let c = client(Fake::new(vec![(LAN, down()), (REMOTE, down())]));
        assert!(c.download_original("s1", &dir.path().join("x.mp3")).unwrap_err().is_offline());
    }

    #[test]
    fn a_remote_only_server_uses_its_remote_url() {
        let c = ServerClient::new(
            None,
            Some(REMOTE.into()),
            Credentials::Password { username: "me".into(), password: "sesame".into() },
            Fake::new(vec![(REMOTE, ok(PING_OK))]),
        );
        assert!(c.ping().is_ok());
        assert_eq!(c.last_route(), Some(Route::Remote));
    }
}
