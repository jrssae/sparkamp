//! Parsers for Subsonic JSON responses (`f=json`).
//!
//! Every parser takes the raw body and returns typed data or a
//! [`ServerError`]. They are pure, so the fixtures in the tests are the whole
//! contract with the server.

use super::error::ServerError;

/// What `ping` says about the server.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerInfo {
    /// Subsonic API version the server speaks, e.g. "1.16.1".
    pub api_version: String,
    /// OpenSubsonic `type`, e.g. "navidrome". `None` on plain Subsonic.
    pub server_type: Option<String>,
    /// OpenSubsonic `serverVersion`, e.g. "0.64.2".
    pub server_version: Option<String>,
    /// Whether the server declares OpenSubsonic support.
    pub open_subsonic: bool,
}

/// Parse a `ping` response.
pub fn parse_ping(body: &str) -> Result<ServerInfo, ServerError> {
    let r = envelope(body)?;
    Ok(ServerInfo {
        api_version: str_field(&r, "version").unwrap_or_default(),
        server_type: str_field(&r, "type"),
        server_version: str_field(&r, "serverVersion"),
        open_subsonic: r.get("openSubsonic").and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

/// What `getScanStatus` says. Navidrome adds `lastScan` and `folderCount`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanStatus {
    pub scanning: bool,
    pub count: Option<u64>,
    pub folder_count: Option<u64>,
    /// ISO-8601 time of the last completed scan, as the server wrote it.
    pub last_scan: Option<String>,
}

/// Parse a `getScanStatus` response.
pub fn parse_scan_status(body: &str) -> Result<ScanStatus, ServerError> {
    let r = envelope(body)?;
    let st = r.get("scanStatus").ok_or(ServerError::NotSubsonic)?;
    Ok(ScanStatus {
        scanning: st.get("scanning").and_then(|v| v.as_bool()).unwrap_or(false),
        count: st.get("count").and_then(|v| v.as_u64()),
        folder_count: st.get("folderCount").and_then(|v| v.as_u64()),
        last_scan: str_field(st, "lastScan"),
    })
}

/// One OpenSubsonic extension the server declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    pub name: String,
    pub versions: Vec<u32>,
}

/// Parse a `getOpenSubsonicExtensions` response.
pub fn parse_extensions(body: &str) -> Result<Vec<Extension>, ServerError> {
    let r = envelope(body)?;
    Ok(array(&r, &["openSubsonicExtensions"])
        .iter()
        .filter_map(|e| {
            Some(Extension {
                name: str_field(e, "name")?,
                versions: e
                    .get("versions")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|n| n.as_u64()).map(|n| n as u32).collect())
                    .unwrap_or_default(),
            })
        })
        .collect())
}

/// One library (music folder) on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MusicFolder {
    /// Numeric on most servers, a string on some; kept as text.
    pub id: String,
    pub name: String,
}

/// Parse a `getMusicFolders` response.
pub fn parse_music_folders(body: &str) -> Result<Vec<MusicFolder>, ServerError> {
    let r = envelope(body)?;
    Ok(array(&r, &["musicFolders", "musicFolder"])
        .iter()
        .filter_map(|f| {
            Some(MusicFolder { id: id_text(f.get("id")?)?, name: str_field(f, "name")? })
        })
        .collect())
}

/// ReplayGain values from the OpenSubsonic `replayGain` object.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReplayGain {
    pub track_gain: Option<f64>,
    pub track_peak: Option<f64>,
    pub album_gain: Option<f64>,
    pub album_peak: Option<f64>,
}

/// One song as the server describes it (a Subsonic `Child`). Text fields
/// the server leaves out are empty strings, matching how local tags are
/// compared.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ServerSong {
    /// The server's song ID. Not stable: Navidrome derives it from tags.
    pub id: String,
    /// The path as reported: absolute when Report Real Path is on, made up
    /// from tags otherwise.
    pub path: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub genre: String,
    pub comment: String,
    pub track: Option<i64>,
    pub disc: Option<i64>,
    pub year: Option<i64>,
    pub bpm: Option<i64>,
    pub duration_secs: Option<i64>,
    pub size: Option<i64>,
    pub suffix: Option<String>,
    pub bit_rate: Option<i64>,
    pub cover_art: Option<String>,
    /// The album's id on the server; covers are fetched once per album.
    pub album_id: Option<String>,
    /// 0 to 5; 0 means unrated.
    pub user_rating: u8,
    pub play_count: u64,
    pub played: Option<String>,
    pub musicbrainz_id: Option<String>,
    pub isrc: Vec<String>,
    pub replay_gain: Option<ReplayGain>,
}

/// Parse the songs of a `search3` response. A page past the end has no
/// `song` key at all, which is an empty page, not an error.
pub fn parse_search3_songs(body: &str) -> Result<Vec<ServerSong>, ServerError> {
    let r = envelope(body)?;
    Ok(array(&r, &["searchResult3", "song"]).iter().filter_map(parse_song).collect())
}

/// One `Child` object. `None` only when it has no id.
fn parse_song(v: &serde_json::Value) -> Option<ServerSong> {
    let text = |key: &str| str_field(v, key).unwrap_or_default();
    let int = |key: &str| v.get(key).and_then(|n| n.as_i64());
    let float = |o: &serde_json::Value, key: &str| o.get(key).and_then(|n| n.as_f64());
    let isrc = match v.get("isrc") {
        Some(serde_json::Value::Array(a)) => {
            a.iter().filter_map(|s| s.as_str()).map(str::to_string).collect()
        }
        Some(serde_json::Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    };
    Some(ServerSong {
        id: id_text(v.get("id")?)?,
        path: str_field(v, "path"),
        title: text("title"),
        artist: text("artist"),
        album: text("album"),
        album_artist: str_field(v, "displayAlbumArtist")
            .or_else(|| str_field(v, "albumArtist"))
            .unwrap_or_default(),
        genre: text("genre"),
        comment: text("comment"),
        track: int("track"),
        disc: int("discNumber"),
        year: int("year"),
        bpm: int("bpm"),
        duration_secs: int("duration"),
        size: int("size"),
        suffix: str_field(v, "suffix"),
        bit_rate: int("bitRate"),
        cover_art: str_field(v, "coverArt"),
        album_id: v.get("albumId").and_then(id_text),
        user_rating: int("userRating").unwrap_or(0).clamp(0, 5) as u8,
        play_count: v.get("playCount").and_then(|n| n.as_u64()).unwrap_or(0),
        played: str_field(v, "played"),
        musicbrainz_id: str_field(v, "musicBrainzId").filter(|s| !s.is_empty()),
        isrc,
        replay_gain: v.get("replayGain").filter(|g| g.is_object()).map(|g| ReplayGain {
            track_gain: float(g, "trackGain"),
            track_peak: float(g, "trackPeak"),
            album_gain: float(g, "albumGain"),
            album_peak: float(g, "albumPeak"),
        }),
    })
}

/// A playlist on the server.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerPlaylist {
    pub id: String,
    pub name: String,
    pub comment: String,
    pub owner: String,
    pub public: bool,
    pub song_count: u64,
    pub changed: Option<String>,
    /// OpenSubsonic `readonly`: smart playlists, playlists owned by someone
    /// else, auto-synced `.m3u` imports. `false` when the server does not say.
    pub readonly: bool,
    /// OpenSubsonic `validUntil`: how long a smart playlist's contents stay
    /// fresh.
    pub valid_until: Option<String>,
}

/// Parse a `getPlaylists` response.
pub fn parse_playlists(body: &str) -> Result<Vec<ServerPlaylist>, ServerError> {
    let r = envelope(body)?;
    Ok(array(&r, &["playlists", "playlist"]).iter().filter_map(playlist_head).collect())
}

/// Parse a `getPlaylist` response: the playlist and its songs in order.
pub fn parse_playlist(body: &str) -> Result<(ServerPlaylist, Vec<ServerSong>), ServerError> {
    let r = envelope(body)?;
    let pl = r.get("playlist").ok_or(ServerError::NotSubsonic)?;
    let head = playlist_head(pl).ok_or(ServerError::NotSubsonic)?;
    let songs = array(pl, &["entry"]).iter().filter_map(parse_song).collect();
    Ok((head, songs))
}

fn playlist_head(v: &serde_json::Value) -> Option<ServerPlaylist> {
    let flag = |key: &str| v.get(key).and_then(|b| b.as_bool()).unwrap_or(false);
    Some(ServerPlaylist {
        id: id_text(v.get("id")?)?,
        name: str_field(v, "name").unwrap_or_default(),
        comment: str_field(v, "comment").unwrap_or_default(),
        owner: str_field(v, "owner").unwrap_or_default(),
        public: flag("public"),
        song_count: v.get("songCount").and_then(|n| n.as_u64()).unwrap_or(0),
        changed: str_field(v, "changed"),
        readonly: flag("readonly"),
        valid_until: str_field(v, "validUntil"),
    })
}

/// Parse a response that carries nothing but success or an error.
pub fn parse_ok(body: &str) -> Result<(), ServerError> {
    envelope(body).map(|_| ())
}

/// Parse a `createPlaylist` response: the new playlist's ID if the server
/// sends the playlist back (API 1.14 and later), `None` if it does not.
pub fn parse_created_playlist_id(body: &str) -> Result<Option<String>, ServerError> {
    let r = envelope(body)?;
    Ok(r.get("playlist").and_then(|pl| pl.get("id")).and_then(id_text))
}

/// Subsonic error codes that mean the credentials, or the way they were sent,
/// were refused: 40 wrong username or password, 41 token auth not supported,
/// 42 auth mechanism not supported, 43 conflicting mechanisms, 44 invalid
/// API key.
const AUTH_ERROR_CODES: [u32; 5] = [40, 41, 42, 43, 44];

/// The inner `subsonic-response` object of a successful response, or the
/// error it carries.
fn envelope(body: &str) -> Result<serde_json::Value, ServerError> {
    let root: serde_json::Value =
        serde_json::from_str(body).map_err(|_| ServerError::NotSubsonic)?;
    let r = root.get("subsonic-response").ok_or(ServerError::NotSubsonic)?;
    match r.get("status").and_then(|s| s.as_str()) {
        Some("ok") => Ok(r.clone()),
        Some("failed") => {
            let err = r.get("error");
            let code = err
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_u64())
                .unwrap_or(0) as u32;
            let message = err
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or_default()
                .to_string();
            if AUTH_ERROR_CODES.contains(&code) {
                Err(ServerError::Auth { code, message })
            } else {
                Err(ServerError::Api { code, message })
            }
        }
        _ => Err(ServerError::NotSubsonic),
    }
}

/// The array at `path`, or empty. Subsonic omits empty lists entirely rather
/// than sending `[]`, so a missing key is not an error.
fn array<'a>(v: &'a serde_json::Value, path: &[&str]) -> &'a [serde_json::Value] {
    let mut cur = v;
    for key in path {
        match cur.get(key) {
            Some(next) => cur = next,
            None => return &[],
        }
    }
    cur.as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// An id that may arrive as a JSON number or string.
fn id_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn str_field(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|s| s.as_str()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAVIDROME_PING: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
        "type":"navidrome","serverVersion":"0.64.2 (abc1234)","openSubsonic":true}}"#;

    #[test]
    fn ping_reads_the_server_identity() {
        assert_eq!(
            parse_ping(NAVIDROME_PING).unwrap(),
            ServerInfo {
                api_version: "1.16.1".into(),
                server_type: Some("navidrome".into()),
                server_version: Some("0.64.2 (abc1234)".into()),
                open_subsonic: true,
            }
        );
    }

    #[test]
    fn plain_subsonic_ping_has_no_opensubsonic_fields() {
        let info =
            parse_ping(r#"{"subsonic-response":{"status":"ok","version":"1.15.0"}}"#).unwrap();
        assert_eq!(info.api_version, "1.15.0");
        assert_eq!(info.server_type, None);
        assert!(!info.open_subsonic);
    }

    #[test]
    fn wrong_password_is_an_auth_error() {
        let body = r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
            "error":{"code":40,"message":"Wrong username or password"}}}"#;
        assert_eq!(
            parse_ping(body),
            Err(ServerError::Auth { code: 40, message: "Wrong username or password".into() })
        );
    }

    #[test]
    fn invalid_api_key_is_an_auth_error() {
        let body = r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
            "error":{"code":44,"message":"Invalid API key"}}}"#;
        assert!(matches!(parse_ping(body), Err(ServerError::Auth { code: 44, .. })));
    }

    #[test]
    fn other_subsonic_errors_keep_code_and_message() {
        let body = r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
            "error":{"code":70,"message":"Playlist not found"}}}"#;
        assert_eq!(
            parse_ping(body),
            Err(ServerError::Api { code: 70, message: "Playlist not found".into() })
        );
    }

    #[test]
    fn scan_status_reads_navidromes_extra_fields() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "scanStatus":{"scanning":false,"count":37012,"folderCount":3101,
            "lastScan":"2026-09-29T03:14:15.926Z"}}}"#;
        assert_eq!(
            parse_scan_status(body).unwrap(),
            ScanStatus {
                scanning: false,
                count: Some(37012),
                folder_count: Some(3101),
                last_scan: Some("2026-09-29T03:14:15.926Z".into()),
            }
        );
    }

    #[test]
    fn scan_status_while_scanning_without_extras() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "scanStatus":{"scanning":true}}}"#;
        let st = parse_scan_status(body).unwrap();
        assert!(st.scanning);
        assert_eq!(st.last_scan, None);
    }

    #[test]
    fn extensions_list_names_and_versions() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "openSubsonicExtensions":[{"name":"songLyrics","versions":[1,2]},
            {"name":"apiKeyAuthentication","versions":[1]}]}}"#;
        assert_eq!(
            parse_extensions(body).unwrap(),
            vec![
                Extension { name: "songLyrics".into(), versions: vec![1, 2] },
                Extension { name: "apiKeyAuthentication".into(), versions: vec![1] },
            ]
        );
    }

    #[test]
    fn music_folder_ids_are_text_whether_numeric_or_not() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "musicFolders":{"musicFolder":[{"id":1,"name":"Music"},{"id":"lib2","name":"Audiobooks"}]}}}"#;
        assert_eq!(
            parse_music_folders(body).unwrap(),
            vec![
                MusicFolder { id: "1".into(), name: "Music".into() },
                MusicFolder { id: "lib2".into(), name: "Audiobooks".into() },
            ]
        );
    }

    const NAVIDROME_SONG_PAGE: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
        "type":"navidrome","serverVersion":"0.64.2","openSubsonic":true,
        "searchResult3":{"song":[{
            "id":"2kFgmM1wLqz9LUb3sHw8Yk","parent":"al-1","isDir":false,"title":"So What",
            "album":"Kind of Blue","artist":"Miles Davis","track":1,"year":1959,"genre":"Jazz",
            "coverArt":"mf-2kFgmM1wLqz9LUb3sHw8Yk_65a1b2c3","size":22553344,
            "contentType":"audio/mpeg","suffix":"mp3","duration":562,"bitRate":320,
            "path":"/music/Miles Davis/Kind of Blue/01 So What.mp3","discNumber":1,
            "created":"2026-01-02T03:04:05Z","albumId":"al-1","artistId":"ar-1","type":"music",
            "userRating":4,"playCount":17,"played":"2026-09-20T21:00:00Z","bpm":136,
            "comment":"remastered","musicBrainzId":"0b4e6f0f-5c4f-4c7a-9e2b-1d7e7c1a0a11",
            "isrc":["USSM15900113"],"displayAlbumArtist":"Miles Davis",
            "replayGain":{"trackGain":-6.5,"trackPeak":0.98,"albumGain":-7.1,"albumPeak":1.0}
        }]}}}"#;

    #[test]
    fn a_navidrome_song_reads_every_compared_field() {
        let songs = parse_search3_songs(NAVIDROME_SONG_PAGE).unwrap();
        assert_eq!(
            songs,
            vec![ServerSong {
                id: "2kFgmM1wLqz9LUb3sHw8Yk".into(),
                path: Some("/music/Miles Davis/Kind of Blue/01 So What.mp3".into()),
                title: "So What".into(),
                artist: "Miles Davis".into(),
                album: "Kind of Blue".into(),
                album_artist: "Miles Davis".into(),
                genre: "Jazz".into(),
                comment: "remastered".into(),
                track: Some(1),
                disc: Some(1),
                year: Some(1959),
                bpm: Some(136),
                duration_secs: Some(562),
                size: Some(22553344),
                suffix: Some("mp3".into()),
                bit_rate: Some(320),
                cover_art: Some("mf-2kFgmM1wLqz9LUb3sHw8Yk_65a1b2c3".into()),
                album_id: Some("al-1".into()),
                user_rating: 4,
                play_count: 17,
                played: Some("2026-09-20T21:00:00Z".into()),
                musicbrainz_id: Some("0b4e6f0f-5c4f-4c7a-9e2b-1d7e7c1a0a11".into()),
                isrc: vec!["USSM15900113".into()],
                replay_gain: Some(ReplayGain {
                    track_gain: Some(-6.5),
                    track_peak: Some(0.98),
                    album_gain: Some(-7.1),
                    album_peak: Some(1.0),
                }),
            }]
        );
    }

    #[test]
    fn a_minimal_legacy_subsonic_song_leaves_the_rest_empty() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.15.0",
            "searchResult3":{"song":[{"id":"42","isDir":false,"title":"test"}]}}}"#;
        let song = &parse_search3_songs(body).unwrap()[0];
        assert_eq!(song.id, "42");
        assert_eq!(song.title, "test");
        assert_eq!(song.artist, "");
        assert_eq!(song.user_rating, 0);
        assert_eq!(song.path, None);
        assert!(song.isrc.is_empty());
    }

    #[test]
    fn isrc_sent_as_a_single_string_is_accepted() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "searchResult3":{"song":[{"id":"1","title":"t","isrc":"GBAYE0601498"}]}}}"#;
        assert_eq!(parse_search3_songs(body).unwrap()[0].isrc, vec!["GBAYE0601498".to_string()]);
    }

    #[test]
    fn a_page_past_the_end_is_empty() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","searchResult3":{}}}"#;
        assert_eq!(parse_search3_songs(body).unwrap(), vec![]);
    }

    #[test]
    fn playlists_list_reads_ownership_and_readonly() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "playlists":{"playlist":[
              {"id":"pl1","name":"Road Trip","comment":"summer","owner":"me","public":false,
               "songCount":42,"duration":9000,"created":"2026-01-01T00:00:00Z",
               "changed":"2026-09-01T12:00:00Z","readonly":false},
              {"id":"pl2","name":"Recently Added","owner":"me","public":true,"songCount":100,
               "duration":20000,"created":"2026-01-01T00:00:00Z","changed":"2026-09-30T00:00:00Z",
               "readonly":true,"validUntil":"2026-09-30T00:05:00Z"}]}}}"#;
        assert_eq!(
            parse_playlists(body).unwrap(),
            vec![
                ServerPlaylist {
                    id: "pl1".into(),
                    name: "Road Trip".into(),
                    comment: "summer".into(),
                    owner: "me".into(),
                    public: false,
                    song_count: 42,
                    changed: Some("2026-09-01T12:00:00Z".into()),
                    readonly: false,
                    valid_until: None,
                },
                ServerPlaylist {
                    id: "pl2".into(),
                    name: "Recently Added".into(),
                    comment: "".into(),
                    owner: "me".into(),
                    public: true,
                    song_count: 100,
                    changed: Some("2026-09-30T00:00:00Z".into()),
                    readonly: true,
                    valid_until: Some("2026-09-30T00:05:00Z".into()),
                },
            ]
        );
    }

    #[test]
    fn no_playlists_is_an_empty_list() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playlists":{}}}"#;
        assert_eq!(parse_playlists(body).unwrap(), vec![]);
    }

    #[test]
    fn one_playlist_keeps_its_entries_in_order() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "playlist":{"id":"pl1","name":"Road Trip","owner":"me","public":false,"songCount":2,
              "duration":400,"created":"2026-01-01T00:00:00Z","changed":"2026-09-01T12:00:00Z",
              "entry":[{"id":"s2","title":"Second"},{"id":"s1","title":"First"}]}}}"#;
        let (pl, songs) = parse_playlist(body).unwrap();
        assert_eq!(pl.name, "Road Trip");
        assert_eq!(
            songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["s2", "s1"]
        );
    }

    #[test]
    fn a_captive_portal_page_is_not_subsonic() {
        let body = "<html><body>Please accept the terms to use hotel wifi</body></html>";
        assert_eq!(parse_ping(body), Err(ServerError::NotSubsonic));
    }

    #[test]
    fn json_without_the_envelope_is_not_subsonic() {
        assert_eq!(parse_ping(r#"{"status":"ok"}"#), Err(ServerError::NotSubsonic));
    }
}
