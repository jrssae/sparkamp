//! Bringing one server's catalog cache up to date.
//!
//! This is the periodic update and the explicit refresh. It asks the server
//! whether anything changed since the last complete pull, pulls the whole
//! catalog page by page if so, and then links new server songs to local
//! files. Removals are applied only when every page arrived.

use super::client::ServerClient;
use super::error::ServerError;
use super::matcher::{self, LinkReason};
use super::transport::Transport;
use crate::media_library::MediaLibrary;
use crate::media_library::servers::PullOutcome;

/// Songs asked for per `search3` page.
pub const PAGE_SIZE: u64 = 500;

/// What an update did.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UpdateReport {
    /// The server was scanning; comparing against a half-scanned library
    /// would look like mass deletion, so nothing was pulled.
    pub postponed_for_scan: bool,
    /// The server has not scanned since the last complete pull.
    pub up_to_date: bool,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    /// Removals held for the user: `(would_remove, cached, pull_id)`.
    pub held: Option<(usize, usize, i64)>,
    /// New links, by how they were made.
    pub linked: Vec<(LinkReason, usize)>,
    /// Server songs with more than one plausible local file.
    pub possible_matches: usize,
    /// A server playlist appeared, went, was renamed or had its songs read
    /// again.
    pub playlists_changed: bool,
}

/// Why an update stopped.
#[derive(Debug)]
pub enum UpdateError {
    /// The server failed; see [`ServerError::is_offline`].
    Server(ServerError),
    /// The local database failed.
    Storage(anyhow::Error),
    /// A local file refused a change (e.g. a rating on a read-only file or
    /// a format without a rating tag). Nothing else was changed.
    LocalFile(crate::rating::RatingError),
}

impl UpdateError {
    /// Whether the server was simply not available.
    pub fn is_offline(&self) -> bool {
        matches!(self, UpdateError::Server(e) if e.is_offline())
    }
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::Server(e) => e.fmt(f),
            UpdateError::Storage(e) => write!(f, "library database: {e}"),
            UpdateError::LocalFile(e) => e.fmt(f),
        }
    }
}

impl From<ServerError> for UpdateError {
    fn from(e: ServerError) -> Self {
        UpdateError::Server(e)
    }
}

impl From<anyhow::Error> for UpdateError {
    fn from(e: anyhow::Error) -> Self {
        UpdateError::Storage(e)
    }
}

/// What "Test connection" found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionReport {
    pub server: super::api::ServerInfo,
    pub song_count: Option<u64>,
    /// Whether the server reports real file paths (Navidrome's "Report Real
    /// Path"). `None` when it has no songs to tell by.
    pub real_paths: Option<bool>,
    pub api_key_auth: bool,
}

impl UpdateReport {
    /// Whether the update changed what the song or playlist lists show, so
    /// a frontend should reload them.
    pub fn changed_lists(&self) -> bool {
        self.added + self.updated + self.removed > 0 || !self.linked.is_empty() || self.playlists_changed
    }
}

impl ConnectionReport {
    /// One line for the user.
    pub fn summary(&self) -> String {
        let who = match (&self.server.server_type, &self.server.server_version) {
            (Some(t), Some(v)) => format!("Connected to {t} {v}."),
            (Some(t), None) => format!("Connected to {t}."),
            _ => format!("Connected (Subsonic API {}).", self.server.api_version),
        };
        let mut parts = vec![who];
        if let Some(n) = self.song_count {
            parts.push(format!("{} songs.", super::status::thousands(n)));
        }
        match self.real_paths {
            Some(true) => parts.push("Real paths: yes.".into()),
            Some(false) => parts.push(
                "Real paths: no. Turn on Report Real Path for Sparkamp's player on the server; \
                 matching and export work better with it."
                    .into(),
            ),
            None => {}
        }
        parts.push(if self.api_key_auth {
            "API keys: supported.".into()
        } else {
            "API keys: not supported yet.".into()
        });
        parts.join(" ")
    }
}

/// Check a server the user is adding: who it is, how big, and whether its
/// paths are real (without them, matching and export work less well).
pub fn test_connection<T: Transport>(client: &ServerClient<T>) -> Result<ConnectionReport, ServerError> {
    let server = client.ping()?;
    let song_count = client.scan_status().ok().and_then(|s| s.count);
    // Plain Subsonic has no extensions endpoint; that is "none", not a failure.
    let extensions = client.extensions().unwrap_or_default();
    let sample = client.search3_songs(0, 1).ok().and_then(|songs| songs.into_iter().next());
    Ok(ConnectionReport {
        server,
        song_count,
        real_paths: sample.map(|s| s.path.as_deref().is_some_and(|p| p.starts_with('/'))),
        api_key_auth: extensions.iter().any(|e| e.name == "apiKeyAuthentication"),
    })
}

/// Which of a server's addresses a check used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Address {
    /// The LAN URL, for use at home.
    Home,
    /// The remote URL, for use from anywhere.
    Remote,
}

impl Address {
    pub fn label(self) -> &'static str {
        match self {
            Address::Home => "Home network",
            Address::Remote => "Remote",
        }
    }
}

/// One address of a server, tried on its own.
#[derive(Debug)]
pub struct AddressCheck {
    pub address: Address,
    pub url: String,
    /// What the server said, or why it could not be reached; the text is
    /// safe to show.
    pub outcome: Result<ConnectionReport, String>,
}

impl AddressCheck {
    /// One line for the user, naming the address.
    pub fn line(&self) -> String {
        let what = match &self.outcome {
            Ok(report) => report.summary(),
            Err(why) => why.clone(),
        };
        format!("{} ({}): {what}", self.address.label(), self.url)
    }
}

/// Test each configured address of a server separately, home first, so the
/// user sees which one answered and what each said. `make_transport` gets
/// the address it is for. A remote address that is not HTTPS is refused
/// without being contacted: the password must never cross the internet in
/// the clear.
pub fn test_addresses<T: Transport>(
    config: &crate::config::ServerConfig,
    password: &str,
    make_transport: impl Fn(&str) -> T,
) -> Vec<AddressCheck> {
    use super::request::Credentials;
    let creds = || Credentials::Password { username: config.username.clone(), password: password.to_string() };
    let mut checks = Vec::new();
    if let Some(url) = config.lan_url.as_deref().filter(|u| !u.trim().is_empty()) {
        let client = ServerClient::new(Some(url.to_string()), None, creds(), make_transport(url));
        checks.push(AddressCheck {
            address: Address::Home,
            url: url.to_string(),
            outcome: test_connection(&client).map_err(|e| e.to_string()),
        });
    }
    if let Some(url) = config.remote_url.as_deref().filter(|u| !u.trim().is_empty()) {
        let outcome = if url.trim().to_ascii_lowercase().starts_with("https://") {
            let client = ServerClient::new(None, Some(url.to_string()), creds(), make_transport(url));
            test_connection(&client).map_err(|e| e.to_string())
        } else {
            Err("not tried: a remote address must start with https://, so the password never \
                 crosses the internet in the clear"
                .to_string())
        };
        checks.push(AddressCheck { address: Address::Remote, url: url.to_string(), outcome });
    }
    checks
}

/// Cover thumbnails are fetched at this size (pixels): sharp on a Retina
/// gallery tile up to 256 pt, about 60 KB each.
pub const COVER_SIZE: u32 = 512;

/// Fetch cover art for `server_id`'s songs that have none cached yet, one
/// download per album, into `dir`. Returns how many covers were fetched.
/// Stops at the first failure; what is missing is fetched next time.
pub fn fetch_covers<T: Transport>(
    client: &ServerClient<T>,
    lib: &MediaLibrary,
    server_id: &str,
    dir: &std::path::Path,
) -> Result<usize, UpdateError> {
    std::fs::create_dir_all(dir).map_err(|e| UpdateError::Storage(e.into()))?;
    let mut fetched = 0;
    for (key, cover) in lib.covers_needed(server_id)? {
        // Named by a hash, like the playback cache: no path or id in names.
        let mut h: u64 = 0xcbf29ce484222325;
        for b in server_id.bytes().chain([0]).chain(key.bytes()).chain([0]).chain(cover.bytes()) {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let dest = dir.join(format!("{h:016x}.jpg"));
        match client.download_cover(&cover, COVER_SIZE, &dest) {
            Ok(_) => {
                lib.set_cover_path(server_id, &key, &dest.to_string_lossy())?;
                fetched += 1;
            }
            Err(e) if e.is_offline() => return Err(e.into()),
            // This one cover is broken on the server; the rest can still come.
            Err(_) => {}
        }
    }
    Ok(fetched)
}

/// Plays sent per `scrobble` request. Each play adds an `id` and a `time`
/// to the URL, and a GET URL should stay well under a few kilobytes.
pub const SCROBBLE_BATCH: usize = 50;

/// What sending the queue did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SendReport {
    pub ratings_sent: usize,
    /// Queued ratings not sent because the server's rating changed too; the
    /// merge shows them as conflicts instead.
    pub ratings_withheld: usize,
    pub scrobbles_sent: usize,
}

/// Send `server_id`'s queued ratings, then its queued plays. Anything not
/// sent stays queued for the next update, refresh or play.
pub fn send_pending<T: Transport>(
    client: &ServerClient<T>,
    lib: &MediaLibrary,
    server_id: &str,
) -> Result<SendReport, UpdateError> {
    let mut report = SendReport::default();
    for (row_id, song_id, rating) in lib.pending_ratings(server_id)? {
        if lib.rating_changed_on_server(row_id, rating)? {
            lib.clear_pending_rating(row_id)?;
            report.ratings_withheld += 1;
            continue;
        }
        match client.set_rating(&song_id, rating) {
            Ok(()) => report.ratings_sent += 1,
            Err(e) if e.is_offline() => return Err(e.into()),
            // The server refused this one (e.g. the song is gone): drop it
            // rather than retry it forever.
            Err(_) => {}
        }
        lib.clear_pending_rating(row_id)?;
    }
    loop {
        let batch = lib.pending_scrobble_batch(server_id, SCROBBLE_BATCH)?;
        if batch.is_empty() {
            break;
        }
        let plays: Vec<(String, i64)> =
            batch.iter().map(|(_, song, at)| (song.clone(), *at)).collect();
        match client.scrobble(&plays) {
            Ok(()) => report.scrobbles_sent += plays.len(),
            Err(e) if e.is_offline() => return Err(e.into()),
            Err(_) => {}
        }
        lib.clear_scrobbles(&batch.iter().map(|(id, _, _)| *id).collect::<Vec<_>>())?;
    }
    Ok(report)
}

/// The user rated a song: set the local rating, send it at once to every
/// server holding the song, and queue it only for a server that could not
/// be reached. `clients` maps server id to client.
pub fn rate_song<T: Transport>(
    clients: &std::collections::HashMap<String, ServerClient<T>>,
    lib: &MediaLibrary,
    member: crate::media_library::servers::Member,
    rating: u8,
) -> Result<(), UpdateError> {
    use crate::media_library::servers::Member;
    let members: Vec<Member> = match lib.song_copies(member)? {
        Some(copies) => copies.members.into_iter().map(|m| m.member).collect(),
        None => vec![member],
    };
    // The file first: if it will not take the rating, nothing changes
    // anywhere, not the library and not the servers.
    for m in &members {
        if let Member::Local(id) = m {
            let Some(track) = lib.tracks_by_ids(&[*id])?.remove(id) else { continue };
            crate::rating::write_rating(std::path::Path::new(&track.path), rating)
                .map_err(UpdateError::LocalFile)?;
        }
    }
    for m in members {
        match m {
            Member::Local(id) => lib.set_local_rating(id, rating)?,
            Member::Server(id) => {
                let Some(row) = lib.server_row(id)? else { continue };
                let sent = match clients.get(&row.server_id) {
                    Some(client) => match client.set_rating(&row.song.id, rating) {
                        Ok(()) => true,
                        Err(e) if e.is_offline() => false,
                        // Refused outright (e.g. the song is gone): nothing
                        // to retry.
                        Err(_) => continue,
                    },
                    None => false,
                };
                if sent {
                    lib.set_cached_server_rating(id, rating)?;
                } else {
                    lib.queue_rating(id, rating)?;
                }
            }
        }
    }
    Ok(())
}

/// Update `server_id`'s cache from `client`. `force` skips the "nothing
/// changed since the last scan" shortcut (explicit refresh).
pub fn update_catalog<T: Transport>(
    client: &ServerClient<T>,
    lib: &MediaLibrary,
    server_id: &str,
    force: bool,
) -> Result<UpdateReport, UpdateError> {
    update_catalog_with_progress(client, lib, server_id, force, &|_| {})
}

/// [`update_catalog`], telling `progress` how far a pull has got: once as
/// it starts and after every page, against the song count the server
/// reports. Nothing is reported when there is nothing to pull.
pub fn update_catalog_with_progress<T: Transport>(
    client: &ServerClient<T>,
    lib: &MediaLibrary,
    server_id: &str,
    force: bool,
    progress: &dyn Fn(super::status::PullProgress),
) -> Result<UpdateReport, UpdateError> {
    use super::status::PullProgress;
    let mut report = UpdateReport::default();
    // Record agreement reached since the last update (a local scan that
    // finished later) against the server data already cached, before new
    // server data arrives. Otherwise a server change would read as an
    // unresolved difference instead of "changed on the server".
    lib.settle_all_songs()?;
    let status = client.scan_status()?;
    if status.scanning {
        report.postponed_for_scan = true;
        return Ok(report);
    }
    if !force && status.last_scan.is_some() && lib.server_last_scan(server_id)? == status.last_scan
    {
        report.up_to_date = true;
        // Nothing to pull still counts as an update: the next one is due a
        // full interval from now.
        lib.record_server_update_success(server_id, None)?;
        // New local files (a fresh rip) may still match the cached catalog.
        link_new_matches(lib, server_id, &mut report)?;
        lib.settle_all_songs()?;
        report.playlists_changed = pull_playlists(client, lib, server_id)?;
        return Ok(report);
    }

    let pull = lib.begin_server_pull(server_id)?;
    let total = status.count;
    progress(PullProgress { fetched: 0, total });
    let mut offset = 0;
    loop {
        let page = client.search3_songs(offset, PAGE_SIZE)?;
        let outcome = lib.apply_server_songs(server_id, pull, &page)?;
        report.added += outcome.added;
        report.updated += outcome.updated;
        offset += page.len() as u64;
        progress(PullProgress { fetched: offset, total });
        if (page.len() as u64) < PAGE_SIZE {
            break;
        }
    }
    let moved = carry_links_across_moves(lib, server_id, pull)?;
    match lib.finish_server_pull_after_moves(server_id, pull, moved)? {
        PullOutcome::Removed(gone) => {
            report.removed = gone.len();
            // Only a pull whose result was applied counts as "seen this
            // scan". A held one is pulled again next time, so it asks again.
            lib.record_server_pull_complete(server_id, status.last_scan.as_deref())?;
        }
        PullOutcome::Held { would_remove, cached } => {
            report.held = Some((would_remove, cached, pull));
        }
    }

    link_new_matches(lib, server_id, &mut report)?;
    // Copies that agree now have that recorded, so the next change on either
    // side reads as a change there, not as an unresolved difference.
    lib.settle_all_songs()?;
    report.playlists_changed = pull_playlists(client, lib, server_id)?;
    Ok(report)
}

/// Bring `server_id`'s playlists up to date: the list every time, since a
/// playlist can change without a library scan, and the songs only of those
/// whose `changed` stamp moved since they were last read. Returns whether
/// anything changed.
fn pull_playlists<T: Transport>(
    client: &ServerClient<T>,
    lib: &MediaLibrary,
    server_id: &str,
) -> Result<bool, UpdateError> {
    let heads = client.playlists()?;
    let stamps = lib.server_playlist_stamps(server_id)?;
    let mut songs = std::collections::HashMap::new();
    for head in &heads {
        let known = stamps.get(&head.id).cloned().flatten();
        if head.changed.is_none() || known != head.changed {
            songs.insert(head.id.clone(), client.playlist(&head.id)?.1);
        }
    }
    Ok(lib.store_server_playlists(server_id, &heads, &songs)?)
}

/// Before a pull's removals apply: a song whose path vanished while a
/// matching song appeared elsewhere was moved or renamed on the server. A
/// linked one takes its link and history along. Returns how many moved, so
/// the mass-removal guard counts only songs that truly disappeared.
fn carry_links_across_moves(lib: &MediaLibrary, server_id: &str, pull: i64) -> Result<usize, UpdateError> {
    let gone = lib.unseen_rows(server_id, pull)?;
    if gone.is_empty() {
        return Ok(0);
    }
    let added = lib.unlinked_rows_added_in(server_id, pull)?;
    if added.is_empty() {
        return Ok(0);
    }
    let old: Vec<matcher::LocalCandidate> = gone
        .iter()
        .map(|r| matcher::LocalCandidate {
            id: r.id,
            rel_path: r.song.path.clone().unwrap_or_default(),
            title: r.song.title.clone(),
            artist: r.song.artist.clone(),
            album: r.song.album.clone(),
            duration_secs: r.song.duration_secs.map(|d| d as f64),
            musicbrainz_id: r.song.musicbrainz_id.clone(),
            isrc: r.song.isrc.clone(),
        })
        .collect();
    let new: Vec<matcher::ServerCandidate> = added
        .iter()
        .map(|r| matcher::ServerCandidate {
            key: r.id,
            path: r.song.path.clone().unwrap_or_default(),
            title: r.song.title.clone(),
            artist: r.song.artist.clone(),
            album: r.song.album.clone(),
            duration_secs: r.song.duration_secs.map(|d| d as f64),
            musicbrainz_id: r.song.musicbrainz_id.clone(),
            isrc: r.song.isrc.clone(),
        })
        .collect();
    let moves = matcher::match_songs(&old, &new, &[]).matches;
    for m in &moves {
        if lib.is_linked(m.local)? {
            lib.move_member(m.local, m.server)?;
        }
    }
    Ok(moves.len())
}

/// Match unlinked local files against `server_id`'s unlinked songs and link
/// the confident matches. Local work only; no network.
fn link_new_matches(
    lib: &MediaLibrary,
    server_id: &str,
    report: &mut UpdateReport,
) -> Result<(), UpdateError> {
    let (locals, servers) = lib.match_candidates(server_id)?;
    let never = lib.never_link_pairs(server_id)?;
    let found = matcher::match_songs(&locals, &servers, &never);
    for m in &found.matches {
        lib.link_copies(
            crate::media_library::servers::Member::Local(m.local),
            crate::media_library::servers::Member::Server(m.server),
            m.how,
        )?;
        match report.linked.iter_mut().find(|(how, _)| *how == m.how) {
            Some((_, n)) => *n += 1,
            None => report.linked.push((m.how, 1)),
        }
    }
    lib.record_possible_matches(server_id, &found.possible)?;
    report.possible_matches = found.possible.len();

    // What is still unlinked may be a song another server holds and no
    // local file does: one song, so one row. Ambiguous candidates link
    // nothing and are not flagged; the ≈ mark is for local files.
    let (others, mine) = lib.server_match_candidates(server_id)?;
    let never = lib.never_link_server_pairs(server_id)?;
    for m in matcher::match_songs(&others, &mine, &never).matches {
        lib.link_copies(
            crate::media_library::servers::Member::Server(m.server),
            crate::media_library::servers::Member::Server(m.local),
            m.how,
        )?;
        match report.linked.iter_mut().find(|(how, _)| *how == m.how) {
            Some((_, n)) => *n += 1,
            None => report.linked.push((m.how, 1)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::servers::request::Credentials;
    use crate::servers::transport::{Download, HttpResponse};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A Subsonic server in memory, answering at the HTTP boundary.
    struct FakeServer {
        scanning: bool,
        last_scan: String,
        songs: Vec<serde_json::Value>,
        /// Fail every `search3` request at or past this offset.
        drop_at_offset: Option<u64>,
        endpoints: Mutex<Vec<String>>,
        /// Query strings of every write, in order.
        writes: Mutex<Vec<String>>,
        offline_writes: bool,
        /// Answer nothing at all, as a server that cannot be reached.
        unreachable: bool,
        /// `getPlaylists` heads, and each playlist's songs by id.
        playlists: Mutex<Vec<serde_json::Value>>,
        playlist_songs: HashMap<String, Vec<serde_json::Value>>,
    }

    impl FakeServer {
        fn new(songs: Vec<serde_json::Value>) -> Self {
            FakeServer {
                scanning: false,
                last_scan: "2026-09-29T03:00:00Z".into(),
                songs,
                drop_at_offset: None,
                endpoints: Mutex::new(Vec::new()),
                writes: Mutex::new(Vec::new()),
                offline_writes: false,
                unreachable: false,
                playlists: Mutex::new(Vec::new()),
                playlist_songs: HashMap::new(),
            }
        }

        fn calls(&self, endpoint: &str) -> usize {
            self.endpoints.lock().unwrap().iter().filter(|e| *e == endpoint).count()
        }
    }

    fn param(url: &str, key: &str) -> Option<String> {
        url.split_once('?')?.1.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == key).then(|| v.to_string())
        })
    }

    fn ok(inner: serde_json::Value) -> Result<HttpResponse, ServerError> {
        let mut r = json!({"status": "ok", "version": "1.16.1"});
        r.as_object_mut().unwrap().extend(inner.as_object().unwrap().clone());
        let body = json!({ "subsonic-response": r }).to_string();
        Ok(HttpResponse { status: 200, body: body.into_bytes() })
    }

    impl Transport for FakeServer {
        fn get(&self, url: &str, _timeout: u64) -> Result<HttpResponse, ServerError> {
            let endpoint =
                url.split("/rest/").nth(1).unwrap().split('?').next().unwrap().to_string();
            self.endpoints.lock().unwrap().push(endpoint.clone());
            if self.unreachable {
                return Err(ServerError::unreachable("connection refused"));
            }
            match endpoint.as_str() {
                "ping" => ok(json!({"type": "navidrome", "serverVersion": "0.64.2", "openSubsonic": true})),
                "getOpenSubsonicExtensions" => ok(json!({"openSubsonicExtensions": [
                    {"name": "songLyrics", "versions": [1]}]})),
                "getScanStatus" => ok(json!({"scanStatus": {
                    "scanning": self.scanning, "lastScan": self.last_scan,
                    "count": self.songs.len()}})),
                "setRating" | "scrobble" => {
                    if self.offline_writes {
                        return Err(ServerError::unreachable("connection refused"));
                    }
                    self.writes.lock().unwrap().push(url.split_once('?').unwrap().1.to_string());
                    ok(json!({}))
                }
                "search3" => {
                    let offset: u64 = param(url, "songOffset").unwrap().parse().unwrap();
                    let count: u64 = param(url, "songCount").unwrap().parse().unwrap();
                    if self.drop_at_offset.is_some_and(|d| offset >= d) {
                        return Err(ServerError::unreachable("connection reset"));
                    }
                    let page: Vec<_> =
                        self.songs.iter().skip(offset as usize).take(count as usize).cloned().collect();
                    ok(json!({"searchResult3": {"song": page}}))
                }
                "getPlaylists" => ok(json!({"playlists": {"playlist": *self.playlists.lock().unwrap()}})),
                "getPlaylist" => {
                    let id = param(url, "id").unwrap();
                    let mut head = self.playlists.lock().unwrap().iter().find(|p| p["id"] == id.as_str()).unwrap().clone();
                    head["entry"] = json!(self.playlist_songs[&id]);
                    ok(json!({"playlist": head}))
                }
                other => panic!("unexpected endpoint {other}"),
            }
        }

        fn get_to_file(&self, url: &str, _: u64, dest: &std::path::Path) -> Result<Download, ServerError> {
            assert!(url.contains("/rest/getCoverArt"), "no song downloads during an update");
            if self.offline_writes {
                return Err(ServerError::unreachable("connection refused"));
            }
            self.endpoints.lock().unwrap().push("getCoverArt".into());
            std::fs::write(dest, b"JFIF").unwrap();
            Ok(Download { status: 200, content_type: Some("image/jpeg".into()), bytes: 4 })
        }
    }

    fn song(i: usize, path: &str) -> serde_json::Value {
        json!({"id": format!("s{i}"), "title": format!("Song {i}"), "artist": "Artist",
               "album": "Album", "duration": 200, "path": path})
    }

    fn many(n: usize) -> Vec<serde_json::Value> {
        (0..n).map(|i| song(i, &format!("/music/Artist/Album/{i:04}.mp3"))).collect()
    }

    fn client(server: FakeServer) -> ServerClient<FakeServer> {
        ServerClient::new(
            Some("http://oscar.local:4533".into()),
            None,
            Credentials::Password { username: "me".into(), password: "pw".into() },
            server,
        )
    }

    fn temp_lib() -> (MediaLibrary, tempfile::NamedTempFile) {
        let db = tempfile::NamedTempFile::with_suffix(".db").unwrap();
        (MediaLibrary::open_at(db.path()).unwrap(), db)
    }

    /// A watched folder holding `Artist/Album/<name>` files.
    fn local_files(lib: &MediaLibrary, names: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album).unwrap();
        for n in names {
            std::fs::write(album.join(n), b"fake audio").unwrap();
        }
        let root = dir.path().canonicalize().unwrap();
        let root = root.to_str().unwrap();
        let folder = lib.add_folder(root).unwrap().id();
        lib.rescan_folder_fast(folder, root, true).unwrap();
        dir
    }

    #[test]
    fn a_first_update_pulls_every_page_and_links_local_files() {
        let (lib, _db) = temp_lib();
        let _dir = local_files(&lib, &["0007.mp3"]);
        let c = client(FakeServer::new(many(1203)));

        let report = update_catalog(&c, &lib, "oscar", false).unwrap();

        assert_eq!(report.added, 1203);
        assert_eq!(report.linked, vec![(LinkReason::Path, 1)]);
        assert_eq!(c_calls(&c, "search3"), 3, "pages of 500: 500, 500, 203");
        assert_eq!(lib.server_songs("oscar").unwrap().len(), 1203);
        assert_eq!(lib.shown_server_track_ids().unwrap().len(), 1202, "the linked one is the local row");
    }

    #[test]
    fn an_update_brings_the_servers_playlists_and_rereads_only_changed_ones() {
        let (lib, _db) = temp_lib();
        let songs = many(3);
        let mut server = FakeServer::new(songs.clone());
        *server.playlists.lock().unwrap() = vec![
            json!({"id": "p1", "name": "Road Trip", "songCount": 2, "changed": "t1"}),
            json!({"id": "p2", "name": "Chill", "songCount": 1, "changed": "t1"}),
        ];
        server.playlist_songs = HashMap::from([
            ("p1".to_string(), vec![songs[2].clone(), songs[0].clone()]),
            ("p2".to_string(), vec![songs[1].clone()]),
        ]);
        let c = client(server);

        assert!(update_catalog(&c, &lib, "oscar", false).unwrap().playlists_changed);
        let names: Vec<String> = lib.listed_playlists().unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["Chill", "Road Trip"]);
        let road_trip = lib.listed_playlists().unwrap().into_iter().find(|p| p.name == "Road Trip").unwrap();
        let tracks = lib.load_playlist_tracks(&lib.playlist_by_id(road_trip.id).unwrap()).unwrap();
        let titles: Vec<_> = tracks.iter().map(|t| t.title.clone().unwrap_or_default()).collect();
        assert_eq!(titles, vec!["Song 2", "Song 0"]);
        assert_eq!(c_calls(&c, "getPlaylist"), 2);

        // Nothing new on the server: the list is asked for, no songs are.
        assert!(!update_catalog(&c, &lib, "oscar", false).unwrap().playlists_changed);
        assert_eq!(c_calls(&c, "getPlaylists"), 2, "playlists change without a library scan");
        assert_eq!(c_calls(&c, "getPlaylist"), 2);

        // One playlist edited on the server: only its songs are read again.
        c.transport().playlists.lock().unwrap()[1]["changed"] = json!("t2");
        assert!(update_catalog(&c, &lib, "oscar", false).unwrap().playlists_changed);
        assert_eq!(c_calls(&c, "getPlaylist"), 3);

        // A rename alone shows too.
        c.transport().playlists.lock().unwrap()[0]["name"] = json!("Road Trip 2");
        assert!(update_catalog(&c, &lib, "oscar", false).unwrap().playlists_changed);
    }

    fn c_calls(c: &ServerClient<FakeServer>, endpoint: &str) -> usize {
        c.transport().calls(endpoint)
    }

    #[test]
    fn an_update_while_the_server_is_scanning_is_postponed() {
        let (lib, _db) = temp_lib();
        let mut server = FakeServer::new(many(10));
        server.scanning = true;
        let c = client(server);
        let report = update_catalog(&c, &lib, "oscar", false).unwrap();
        assert!(report.postponed_for_scan);
        assert_eq!(c_calls(&c, "search3"), 0);
    }

    #[test]
    fn nothing_is_pulled_when_the_server_has_not_scanned_since() {
        let (lib, _db) = temp_lib();
        let c = client(FakeServer::new(many(10)));
        update_catalog(&c, &lib, "oscar", false).unwrap();
        let report = update_catalog(&c, &lib, "oscar", false).unwrap();
        assert!(report.up_to_date);
        assert_eq!(c_calls(&c, "search3"), 1, "only the first update pulled");
    }

    #[test]
    fn an_explicit_refresh_pulls_even_without_a_new_scan() {
        let (lib, _db) = temp_lib();
        let c = client(FakeServer::new(many(10)));
        update_catalog(&c, &lib, "oscar", false).unwrap();
        let report = update_catalog(&c, &lib, "oscar", true).unwrap();
        assert!(!report.up_to_date);
        assert_eq!(c_calls(&c, "search3"), 2);
    }

    #[test]
    fn a_connection_lost_mid_pull_keeps_what_arrived_and_removes_nothing() {
        let (lib, _db) = temp_lib();
        let c = client(FakeServer::new(many(1000)));
        update_catalog(&c, &lib, "oscar", false).unwrap();

        let mut server = FakeServer::new(many(1000)[..940].to_vec());
        server.last_scan = "2026-09-30T03:00:00Z".into();
        server.drop_at_offset = Some(500);
        let c = client(server);
        assert!(update_catalog(&c, &lib, "oscar", false).unwrap_err().is_offline());
        assert_eq!(lib.server_songs("oscar").unwrap().len(), 1000);

        // And the next attempt is not skipped as "up to date".
        let mut server = FakeServer::new(many(1000)[..940].to_vec());
        server.last_scan = "2026-09-30T03:00:00Z".into();
        let report = update_catalog(&client(server), &lib, "oscar", false).unwrap();
        assert_eq!(report.removed, 60);
    }

    /// oscar's catalog cached, one local file linked to song `s0`.
    fn with_linked_song(lib: &MediaLibrary) -> (tempfile::TempDir, i64, i64) {
        let dir = local_files(lib, &["0000.mp3"]);
        update_catalog(&client(FakeServer::new(many(3))), lib, "oscar", false).unwrap();
        let local = lib.all_tracks().unwrap()[0].id;
        let server = lib.server_songs("oscar").unwrap()[0].id;
        (dir, local, server)
    }

    fn writes(c: &ServerClient<FakeServer>) -> Vec<String> {
        c.transport()
            .writes
            .lock()
            .unwrap()
            .iter()
            .map(|q| {
                q.split('&')
                    .filter(|kv| !["u=", "t=", "s=", "v=", "c=", "f="].iter().any(|p| kv.starts_with(p)))
                    .collect::<Vec<_>>()
                    .join("&")
            })
            .collect()
    }

    #[test]
    fn queued_ratings_go_first_then_plays_and_both_leave_the_queue() {
        let (lib, _db) = temp_lib();
        let (_dir, local, server) = with_linked_song(&lib);
        let path = lib.tracks_by_ids(&[local]).unwrap()[&local].path.clone();
        lib.record_play_at(&path, 1_759_000_000_000).unwrap();
        lib.queue_rating(server, 4).unwrap();

        let c = client(FakeServer::new(many(3)));
        let report = send_pending(&c, &lib, "oscar").unwrap();

        assert_eq!(report, SendReport { ratings_sent: 1, ratings_withheld: 0, scrobbles_sent: 1 });
        assert_eq!(
            writes(&c),
            vec![
                "id=s0&rating=4".to_string(),
                "id=s0&time=1759000000000&submission=true".to_string(),
            ]
        );
        assert!(lib.pending_ratings("oscar").unwrap().is_empty());
        assert!(lib.pending_scrobbles("oscar").unwrap().is_empty());
    }

    #[test]
    fn nothing_leaves_the_queue_while_the_server_is_unreachable() {
        let (lib, _db) = temp_lib();
        let (_dir, local, server) = with_linked_song(&lib);
        let path = lib.tracks_by_ids(&[local]).unwrap()[&local].path.clone();
        lib.record_play_at(&path, 5).unwrap();
        lib.queue_rating(server, 4).unwrap();

        let mut down = FakeServer::new(many(3));
        down.offline_writes = true;
        assert!(send_pending(&client(down), &lib, "oscar").unwrap_err().is_offline());
        assert_eq!(lib.pending_ratings("oscar").unwrap().len(), 1);
        assert_eq!(lib.pending_scrobbles("oscar").unwrap().len(), 1);
    }

    #[test]
    fn many_queued_plays_go_in_batches() {
        let (lib, _db) = temp_lib();
        let (_dir, local, _server) = with_linked_song(&lib);
        let path = lib.tracks_by_ids(&[local]).unwrap()[&local].path.clone();
        for i in 0..120 {
            lib.record_play_at(&path, i).unwrap();
        }
        let c = client(FakeServer::new(many(3)));
        assert_eq!(send_pending(&c, &lib, "oscar").unwrap().scrobbles_sent, 120);
        assert_eq!(writes(&c).len(), 3, "50 + 50 + 20");
        assert!(lib.pending_scrobbles("oscar").unwrap().is_empty());
    }

    #[test]
    fn a_queued_rating_is_withheld_when_the_server_changed_it_too() {
        let (lib, _db) = temp_lib();
        let (_dir, local, server) = with_linked_song(&lib);
        lib.accept_differences(crate::media_library::servers::Member::Local(local)).unwrap();
        lib.queue_rating(server, 5).unwrap();
        // Meanwhile someone rated it 2 on the server, and the update saw it.
        let mut rated = many(3);
        rated[0]["userRating"] = json!(2);
        let mut srv = FakeServer::new(rated);
        srv.last_scan = "2026-09-30T03:00:00Z".into();
        update_catalog(&client(srv), &lib, "oscar", false).unwrap();

        let c = client(FakeServer::new(many(3)));
        let report = send_pending(&c, &lib, "oscar").unwrap();
        assert_eq!(report.ratings_withheld, 1);
        assert!(writes(&c).is_empty());
        assert!(lib.pending_ratings("oscar").unwrap().is_empty());
    }

    #[test]
    fn a_rating_goes_to_the_server_at_once_and_counts_as_agreed() {
        let (lib, _db) = temp_lib();
        let (_dir, local, server) = with_linked_song(&lib);
        let mut clients = std::collections::HashMap::new();
        clients.insert("oscar".to_string(), client(FakeServer::new(many(3))));

        rate_song(&clients, &lib, crate::media_library::servers::Member::Local(local), 4).unwrap();

        assert_eq!(writes(&clients["oscar"]), vec!["id=s0&rating=4".to_string()]);
        assert!(lib.pending_ratings("oscar").unwrap().is_empty());
        assert_eq!(lib.server_row(server).unwrap().unwrap().song.user_rating, 4);
        let merged = lib.song_merge(crate::media_library::servers::Member::Local(local)).unwrap().unwrap();
        assert_eq!(
            *merged.outcome(crate::servers::merge::Field::Rating),
            crate::servers::merge::FieldOutcome::Agreed
        );
    }

    #[test]
    fn a_rating_is_written_into_the_local_file_first() {
        let (lib, _db) = temp_lib();
        let (_dir, local, _server) = with_linked_song(&lib);
        let mut clients = std::collections::HashMap::new();
        clients.insert("oscar".to_string(), client(FakeServer::new(many(3))));
        rate_song(&clients, &lib, crate::media_library::servers::Member::Local(local), 4).unwrap();
        let path = lib.tracks_by_ids(&[local]).unwrap()[&local].path.clone();
        assert_eq!(crate::rating::read_rating(std::path::Path::new(&path)), Some(4));
        assert_eq!(lib.local_rating(local).unwrap(), 4);
    }

    #[test]
    fn a_file_that_refuses_the_rating_changes_nothing_anywhere() {
        let (lib, _db) = temp_lib();
        let (_dir, local, _server) = with_linked_song(&lib);
        let path = lib.tracks_by_ids(&[local]).unwrap()[&local].path.clone();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();
        let mut clients = std::collections::HashMap::new();
        clients.insert("oscar".to_string(), client(FakeServer::new(many(3))));

        let err = rate_song(&clients, &lib, crate::media_library::servers::Member::Local(local), 4);

        assert!(err.is_err());
        assert_eq!(lib.local_rating(local).unwrap(), 0, "library unchanged");
        assert!(writes(&clients["oscar"]).is_empty(), "server not told");
        assert!(lib.pending_ratings("oscar").unwrap().is_empty(), "nothing queued");
    }

    #[test]
    fn a_rating_for_an_unreachable_server_waits_in_the_queue() {
        let (lib, _db) = temp_lib();
        let (_dir, local, server) = with_linked_song(&lib);
        let mut down = FakeServer::new(many(3));
        down.offline_writes = true;
        let mut clients = std::collections::HashMap::new();
        clients.insert("oscar".to_string(), client(down));

        rate_song(&clients, &lib, crate::media_library::servers::Member::Local(local), 4).unwrap();

        assert_eq!(lib.pending_ratings("oscar").unwrap(), vec![(server, "s0".to_string(), 4)]);
    }

    #[test]
    fn a_pull_reports_its_progress_page_by_page_against_the_servers_count() {
        let (lib, _db) = temp_lib();
        let seen = Mutex::new(Vec::new());
        update_catalog_with_progress(&client(FakeServer::new(many(1_200))), &lib, "oscar", false, &|p| {
            seen.lock().unwrap().push((p.fetched, p.total))
        })
        .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [(0, Some(1_200)), (500, Some(1_200)), (1_000, Some(1_200)), (1_200, Some(1_200))]
        );
    }

    fn two_addresses(remote: &str) -> crate::config::ServerConfig {
        crate::config::ServerConfig {
            lan_url: Some("http://oscar.local:4533".into()),
            remote_url: Some(remote.into()),
            username: "me".into(),
            ..crate::config::ServerConfig::new("oscar")
        }
    }

    #[test]
    fn each_address_is_tested_on_its_own_and_says_which_it_was() {
        let cfg = two_addresses("https://music.example.com");
        let checks = test_addresses(&cfg, "pw", |url| {
            let mut f = FakeServer::new(many(3));
            f.unreachable = url.starts_with("https://");
            f
        });
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].address, Address::Home);
        assert_eq!(checks[0].url, "http://oscar.local:4533");
        assert_eq!(checks[0].outcome.as_ref().unwrap().song_count, Some(3));
        assert_eq!(checks[1].address, Address::Remote);
        assert!(checks[1].outcome.is_err());
        let home = checks[0].line();
        let remote = checks[1].line();
        assert!(home.starts_with("Home network (http://oscar.local:4533): Connected to navidrome 0.64.2."), "{home}");
        assert!(remote.starts_with("Remote (https://music.example.com): server not reachable"), "{remote}");
    }

    #[test]
    fn a_remote_address_without_https_is_refused_without_being_contacted() {
        let mut cfg = two_addresses("http://music.example.com");
        cfg.lan_url = None;
        let contacted = std::sync::Arc::new(Mutex::new(false));
        let c = contacted.clone();
        let checks = test_addresses(&cfg, "pw", move |_| {
            *c.lock().unwrap() = true;
            FakeServer::new(many(1))
        });
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].address, Address::Remote);
        let err = checks[0].outcome.as_ref().unwrap_err();
        assert!(err.contains("https://"), "{err}");
        assert!(!*contacted.lock().unwrap());
    }

    #[test]
    fn test_connection_reports_version_size_and_real_paths() {
        let mut server = FakeServer::new(many(3));
        server.last_scan = "2026-09-29T03:00:00Z".into();
        let report = test_connection(&client(server)).unwrap();
        assert_eq!(report.real_paths, Some(true));
        assert_eq!(
            report.summary(),
            "Connected to navidrome 0.64.2. 3 songs. Real paths: yes. API keys: not supported yet."
        );
    }

    #[test]
    fn a_big_library_reads_with_thousands_separators() {
        let report = test_connection(&client(FakeServer::new(many(12_006)))).unwrap();
        assert!(report.summary().contains("12,006 songs."), "{}", report.summary());
    }

    #[test]
    fn test_connection_notices_made_up_paths() {
        let mut songs = many(1);
        songs[0]["path"] = json!("Artist/Album/01 - Song 0.mp3");
        let report = test_connection(&client(FakeServer::new(songs))).unwrap();
        assert_eq!(report.real_paths, Some(false));
        assert!(report.summary().contains("Report Real Path"), "{}", report.summary());
    }

    #[test]
    fn a_local_file_added_later_links_even_when_the_server_has_not_scanned() {
        let (lib, _db) = temp_lib();
        let dir = local_files(&lib, &["0000.mp3"]);
        update_catalog(&client(FakeServer::new(many(3))), &lib, "oscar", false).unwrap();
        // A new rip lands in the watched folder.
        let album = dir.path().join("Artist").join("Album");
        std::fs::write(album.join("0001.mp3"), b"fake audio").unwrap();
        let root = dir.path().canonicalize().unwrap();
        let folder = lib.list_folders().unwrap()[0].0;
        lib.rescan_folder_fast(folder, root.to_str().unwrap(), true).unwrap();

        let report = update_catalog(&client(FakeServer::new(many(3))), &lib, "oscar", false).unwrap();
        assert!(report.up_to_date);
        assert_eq!(report.linked, vec![(LinkReason::Path, 1)]);
    }

    #[test]
    fn a_file_moved_on_the_server_keeps_its_link_and_history() {
        use crate::media_library::servers::Member;
        let (lib, _db) = temp_lib();
        let (_dir, local, _server) = with_linked_song(&lib);
        lib.accept_differences(Member::Local(local)).unwrap();
        let before = lib.song_copies(Member::Local(local)).unwrap().unwrap();

        let mut moved = many(3);
        moved[0]["path"] = json!("/music/Reorganized/Artist - Album/0000.mp3");
        moved[0]["id"] = json!("s0-moved");
        let mut server = FakeServer::new(moved);
        server.last_scan = "2026-09-30T03:00:00Z".into();
        let report = update_catalog(&client(server), &lib, "oscar", false).unwrap();

        assert_eq!(report.removed, 1, "the old path is gone");
        let after = lib.song_copies(Member::Local(local)).unwrap().expect("still linked");
        let server_member = after.members.iter().find(|m| matches!(m.member, Member::Server(_))).unwrap();
        let Member::Server(row) = server_member.member else { unreachable!() };
        assert_eq!(
            lib.server_row(row).unwrap().unwrap().song.path.as_deref(),
            Some("/music/Reorganized/Artist - Album/0000.mp3")
        );
        assert_eq!(server_member.baseline, before.members[1].baseline, "history kept");
        assert_eq!(server_member.how, before.members[1].how);
    }

    fn album_songs() -> Vec<serde_json::Value> {
        vec![
            json!({"id": "a1", "title": "One", "album": "Blue", "albumId": "al-blue",
                   "coverArt": "mf-a1", "path": "/music/Blue/1.mp3"}),
            json!({"id": "a2", "title": "Two", "album": "Blue", "albumId": "al-blue",
                   "coverArt": "mf-a2", "path": "/music/Blue/2.mp3"}),
            json!({"id": "b1", "title": "Three", "album": "Green", "albumId": "al-green",
                   "coverArt": "mf-b1", "path": "/music/Green/1.mp3"}),
        ]
    }

    #[test]
    fn covers_are_fetched_once_per_album_and_shared_by_its_songs() {
        let (lib, _db) = temp_lib();
        let c = client(FakeServer::new(album_songs()));
        update_catalog(&c, &lib, "oscar", false).unwrap();
        let covers = tempfile::tempdir().unwrap();

        assert_eq!(fetch_covers(&c, &lib, "oscar", covers.path()).unwrap(), 2);
        assert_eq!(c_calls(&c, "getCoverArt"), 2);
        let rows = lib.server_songs("oscar").unwrap();
        let art: Vec<Option<String>> = rows.iter().map(|r| lib.server_artwork_path(r.id).unwrap()).collect();
        assert!(art.iter().all(|a| a.as_deref().is_some_and(|p| std::path::Path::new(p).exists())), "{art:?}");
        assert_eq!(art[0], art[1], "one file for the album");

        assert_eq!(fetch_covers(&c, &lib, "oscar", covers.path()).unwrap(), 0, "nothing new");
    }

    #[test]
    fn albums_you_have_in_full_locally_need_no_server_cover() {
        use crate::media_library::servers::Member;
        let (lib, _db) = temp_lib();
        let c = client(FakeServer::new(album_songs()));
        update_catalog(&c, &lib, "oscar", false).unwrap();
        // Local copies of both songs of "Blue".
        let dir = tempfile::tempdir().unwrap();
        for n in ["b1.mp3", "b2.mp3"] {
            std::fs::write(dir.path().join(n), b"fake").unwrap();
        }
        let root = dir.path().canonicalize().unwrap();
        let folder = lib.add_folder(root.to_str().unwrap()).unwrap().id();
        lib.rescan_folder_fast(folder, root.to_str().unwrap(), true).unwrap();
        let locals: Vec<i64> = lib.all_tracks().unwrap().iter().map(|t| t.id).collect();
        let rows = lib.server_songs("oscar").unwrap();
        for (local, row) in locals.iter().zip(rows.iter().filter(|r| r.song.album == "Blue")) {
            lib.link_copies(Member::Local(*local), Member::Server(row.id), LinkReason::Manual).unwrap();
        }

        let covers = tempfile::tempdir().unwrap();
        assert_eq!(fetch_covers(&c, &lib, "oscar", covers.path()).unwrap(), 1, "only Green");
    }

    #[test]
    fn covers_are_asked_for_at_512_pixels() {
        assert_eq!(COVER_SIZE, 512, "sharp on Retina gallery tiles up to 256 pt");
    }

    #[test]
    fn a_changed_cover_is_fetched_again() {
        let (lib, _db) = temp_lib();
        let covers = tempfile::tempdir().unwrap();
        let c = client(FakeServer::new(album_songs()));
        update_catalog(&c, &lib, "oscar", false).unwrap();
        fetch_covers(&c, &lib, "oscar", covers.path()).unwrap();

        let mut songs = album_songs();
        songs[2]["coverArt"] = json!("mf-b1-new-art");
        let mut srv = FakeServer::new(songs);
        srv.last_scan = "2026-09-30T03:00:00Z".into();
        let c = client(srv);
        update_catalog(&c, &lib, "oscar", false).unwrap();
        assert_eq!(fetch_covers(&c, &lib, "oscar", covers.path()).unwrap(), 1);
    }

    #[test]
    fn covers_resume_after_the_server_goes_away() {
        let (lib, _db) = temp_lib();
        let covers = tempfile::tempdir().unwrap();
        update_catalog(&client(FakeServer::new(album_songs())), &lib, "oscar", false).unwrap();
        let mut down = FakeServer::new(album_songs());
        down.offline_writes = true;
        assert!(fetch_covers(&client(down), &lib, "oscar", covers.path()).unwrap_err().is_offline());
        assert_eq!(fetch_covers(&client(FakeServer::new(album_songs())), &lib, "oscar", covers.path()).unwrap(), 2);
    }

    #[test]
    fn a_big_reorganization_on_the_server_is_not_held() {
        let (lib, _db) = temp_lib();
        update_catalog(&client(FakeServer::new(many(600))), &lib, "oscar", false).unwrap();
        // Every song moves to a new folder on oscar.
        let moved: Vec<serde_json::Value> = (0..600)
            .map(|i| song(i, &format!("/music/Reorganized/{i:04}.mp3")))
            .collect();
        let mut server = FakeServer::new(moved);
        server.last_scan = "2026-09-30T03:00:00Z".into();
        let report = update_catalog(&client(server), &lib, "oscar", false).unwrap();
        assert_eq!(report.held, None, "moves are not disappearances");
        assert_eq!(report.removed, 600, "the old paths are dropped");
        assert_eq!(lib.server_songs("oscar").unwrap().len(), 600);
    }

    #[test]
    fn songs_that_truly_vanish_are_still_held() {
        let (lib, _db) = temp_lib();
        update_catalog(&client(FakeServer::new(many(600))), &lib, "oscar", false).unwrap();
        let mut server = FakeServer::new(many(600)[..50].to_vec());
        server.last_scan = "2026-09-30T03:00:00Z".into();
        let report = update_catalog(&client(server), &lib, "oscar", false).unwrap();
        assert_eq!(report.held.map(|(w, c, _)| (w, c)), Some((550, 600)));
        assert_eq!(lib.server_songs("oscar").unwrap().len(), 600, "nothing dropped yet");
    }

    #[test]
    fn after_copies_agree_a_server_change_reads_as_server_changed() {
        use crate::media_library::servers::Member;
        use crate::servers::merge::SongStatus;
        let (lib, _db) = temp_lib();
        let (_dir, local, _server) = with_linked_song(&lib);
        // The local copy carries the same tags as the server's.
        lib.conn_for_tests()
            .execute(
                "UPDATE tracks SET title = 'Song 0', artist = 'Artist', album = 'Album' WHERE id = ?1",
                [local],
            )
            .unwrap();
        let mut srv = FakeServer::new(many(3));
        srv.last_scan = "2026-09-30T01:00:00Z".into();
        update_catalog(&client(srv), &lib, "oscar", false).unwrap();
        assert_eq!(lib.song_merge(Member::Local(local)).unwrap().unwrap().status(), SongStatus::InSync);

        // Then the server retags it.
        let mut retagged = many(3);
        retagged[0]["title"] = json!("Song 0 (Live)");
        let mut srv = FakeServer::new(retagged);
        srv.last_scan = "2026-09-30T02:00:00Z".into();
        update_catalog(&client(srv), &lib, "oscar", false).unwrap();

        assert_eq!(
            lib.song_merge(Member::Local(local)).unwrap().unwrap().status(),
            SongStatus::ServerChanged,
            "the agreement was recorded, so the server is known to have changed"
        );
    }

    /// Local tags that arrive after the update that linked the song (a scan
    /// still running) are recorded as agreement before the next update
    /// applies new server data.
    #[test]
    fn agreement_reached_between_updates_is_recorded_before_the_next_pull() {
        use crate::media_library::servers::Member;
        use crate::servers::merge::SongStatus;
        let (lib, _db) = temp_lib();
        let (_dir, local, _server) = with_linked_song(&lib);
        // The local scan finishes after the linking update.
        lib.conn_for_tests()
            .execute(
                "UPDATE tracks SET title = 'Song 0', artist = 'Artist', album = 'Album' WHERE id = ?1",
                [local],
            )
            .unwrap();
        // The server retags before the next update.
        let mut retagged = many(3);
        retagged[0]["title"] = json!("Song 0 (Live)");
        let mut srv = FakeServer::new(retagged);
        srv.last_scan = "2026-09-30T02:00:00Z".into();
        update_catalog(&client(srv), &lib, "oscar", false).unwrap();

        assert_eq!(
            lib.song_merge(Member::Local(local)).unwrap().unwrap().status(),
            SongStatus::ServerChanged
        );
    }

    #[test]
    fn a_retag_on_the_server_keeps_the_link() {
        let (lib, _db) = temp_lib();
        let _dir = local_files(&lib, &["0000.mp3"]);
        update_catalog(&client(FakeServer::new(many(3))), &lib, "oscar", false).unwrap();
        let local = lib.all_tracks().unwrap()[0].id;
        let before = lib
            .song_copies(crate::media_library::servers::Member::Local(local))
            .unwrap()
            .expect("linked by path");

        let mut retagged = many(3);
        retagged[0]["id"] = json!("s0-after-retag");
        retagged[0]["title"] = json!("Song 0 (Live)");
        let mut server = FakeServer::new(retagged);
        server.last_scan = "2026-09-30T03:00:00Z".into();
        update_catalog(&client(server), &lib, "oscar", false).unwrap();

        let after = lib
            .song_copies(crate::media_library::servers::Member::Local(local))
            .unwrap()
            .expect("still linked");
        assert_eq!(after, before);
    }
}
