//! Turning a song URI into a file the audio engine can open.
//!
//! A local copy always wins. Otherwise the song plays from the playback
//! cache, and a song not cached yet is downloaded on a background thread:
//! the UI thread never waits on the network. Until the download lands the
//! answer is [`Readiness::Downloading`]; if no server holding the song can be
//! reached it is [`Readiness::Unavailable`], which the player treats as
//! "skip for now", not as a broken file.

use super::cache::PlaybackCache;
use super::client::ServerClient;
use super::transport::Transport;
use crate::media_library::MediaLibrary;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Whether a song can play right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// Open this file.
    Ready(PathBuf),
    /// A download is under way; ask again shortly.
    Downloading,
    /// A download is under way and playback can start from it now: see
    /// [`super::progressive`].
    Streaming(Arc<super::progressive::Partial>),
    /// No copy can be reached (offline, server down, song gone).
    Unavailable(String),
}

/// Why [`crate::engine::Player::load`] could not open a server song. The
/// controller tells these apart from a broken file: neither marks the track
/// broken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SongNotReady {
    /// Still downloading; play it again shortly.
    Downloading,
    /// No copy can be reached right now.
    Unavailable(String),
}

impl std::fmt::Display for SongNotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SongNotReady::Downloading => write!(f, "downloading from the server"),
            SongNotReady::Unavailable(why) => write!(f, "not available: {why}"),
        }
    }
}

impl std::error::Error for SongNotReady {}

impl SongNotReady {
    /// The song cannot load because no server is set up at all.
    pub fn means_no_servers(&self) -> bool {
        matches!(self, SongNotReady::Unavailable(why) if why == NO_SERVERS)
    }
}

static SOURCE: std::sync::RwLock<Option<Arc<dyn SongSource>>> = std::sync::RwLock::new(None);

/// Install the process-wide song source, or remove it (`None`) when no
/// servers are configured. Frontends call this at startup and whenever the
/// server list changes.
pub fn install(source: Option<Arc<dyn SongSource>>) {
    *SOURCE.write().unwrap() = source;
    // A new source has heard nothing yet.
    LAST_CONTEXT.lock().unwrap().reset();
}

/// The last play context sent, so the frontends can offer it on every tick
/// and the source only hears about a change.
#[derive(Default)]
pub(crate) struct ContextDedupe {
    last: Option<(Vec<String>, Vec<String>)>,
}

impl ContextDedupe {
    /// Whether `(keep, ahead)` differs from what was sent last; it becomes
    /// the last either way.
    pub(crate) fn changed(&mut self, keep: &[String], ahead: &[String]) -> bool {
        if self.last.as_ref().is_some_and(|(k, a)| k == keep && a == ahead) {
            return false;
        }
        self.last = Some((keep.to_vec(), ahead.to_vec()));
        true
    }

    pub(crate) fn reset(&mut self) {
        self.last = None;
    }
}

static LAST_CONTEXT: Mutex<ContextDedupe> = Mutex::new(ContextDedupe { last: None });

/// Why a server song is unavailable when no song source is installed: no
/// server is set up. Expected after a server is removed, so not worth
/// reporting as a failure (see [`SongNotReady::means_no_servers`]).
pub const NO_SERVERS: &str = "no servers are configured";

/// Ask the installed source about `uri`.
pub fn prepare(uri: &str) -> Readiness {
    match SOURCE.read().unwrap().as_ref() {
        Some(source) => source.prepare(uri),
        None => Readiness::Unavailable(NO_SERVERS.into()),
    }
}

/// Test support shared by every test that loads a server song through the
/// process-wide source. Tests run in parallel, so they all install the same
/// answers, chosen by the URI's last component: `…ready.mp3` is ready (at a
/// path no backend can open), `…wait.mp3` is downloading, anything else is
/// unavailable.
#[cfg(test)]
pub(crate) fn install_test_answers() {
    struct Answers;
    impl SongSource for Answers {
        fn play_context(&self, keep: &[String], ahead: &[String]) {
            RECORDED.lock().unwrap().push((keep.to_vec(), ahead.to_vec()));
        }

        fn prepare(&self, uri: &str) -> Readiness {
            if let Some((_, at)) = ARRIVED.lock().unwrap().iter().find(|(u, _)| u == uri) {
                Readiness::Ready(at.clone())
            } else if uri.ends_with("ready.mp3") {
                Readiness::Ready(PathBuf::from("/cache/ab12 cd.mp3"))
            } else if uri.ends_with("wait.mp3") {
                Readiness::Downloading
            } else {
                Readiness::Unavailable("oscar: not responding".into())
            }
        }
    }
    install(Some(Arc::new(Answers)));
}

#[cfg(test)]
static RECORDED: Mutex<Vec<(Vec<String>, Vec<String>)>> = Mutex::new(Vec::new());

#[cfg(test)]
static ARRIVED: Mutex<Vec<(String, PathBuf)>> = Mutex::new(Vec::new());

/// Make the shared test source answer "ready" for `uri` from now on, as if
/// its download had just finished.
#[cfg(test)]
pub(crate) fn finish_download_for_tests(uri: &str) {
    finish_download_at_for_tests(uri, PathBuf::from("/cache/ab12 cd.mp3"));
}

/// [`finish_download_for_tests`], landing at `path`, a file an engine can
/// really open.
#[cfg(test)]
pub(crate) fn finish_download_at_for_tests(uri: &str, path: PathBuf) {
    ARRIVED.lock().unwrap().push((uri.to_string(), path));
}

/// Every play context the shared test source was told about, oldest first.
/// Tests run in parallel, so each looks only for its own song names.
#[cfg(test)]
pub(crate) fn recorded_play_contexts() -> Vec<(Vec<String>, Vec<String>)> {
    RECORDED.lock().unwrap().clone()
}

#[cfg(test)]
mod dedupe_tests {
    use super::ContextDedupe;

    #[test]
    fn the_same_context_twice_is_sent_once() {
        let mut d = ContextDedupe::default();
        let (k, a) = (vec!["a".to_string()], vec!["b".to_string()]);
        assert!(d.changed(&k, &a));
        assert!(!d.changed(&k, &a));
        assert!(d.changed(&k, &[]), "a removed song is a change");
        d.reset();
        assert!(d.changed(&k, &[]), "a new source hears it again");
    }
}

/// Something that can make song URIs playable.
pub trait SongSource: Send + Sync {
    /// Where `uri` can be played from, starting a download if needed.
    fn prepare(&self, uri: &str) -> Readiness;

    /// A song started: keep the songs in `keep` cached, fetch those in
    /// `ahead`. Both are song URIs.
    fn play_context(&self, keep: &[String], ahead: &[String]) {
        let _ = (keep, ahead);
    }
}

/// Tell the installed source what is playing and what comes next.
pub fn note_play_context(keep: &[String], ahead: &[String]) {
    if !LAST_CONTEXT.lock().unwrap().changed(keep, ahead) {
        return;
    }
    if let Some(source) = SOURCE.read().unwrap().as_ref() {
        source.play_context(keep, ahead);
    }
}

/// Plays songs from the library database, the playback cache and the
/// configured servers.
pub struct ServerSongSource<T: Transport + 'static> {
    db_path: PathBuf,
    /// Clients in priority order: a song on several servers is fetched from
    /// the first that answers.
    clients: Arc<Vec<(String, Arc<ServerClient<T>>)>>,
    cache: Arc<PlaybackCache>,
    /// URIs being downloaded, and URIs whose last download failed.
    state: Arc<Mutex<HashMap<String, Attempt>>>,
    /// Cache files the queue still needs; never evicted.
    keep: Arc<Mutex<HashSet<PathBuf>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Attempt {
    /// Downloading; the download once its bytes have started to flow.
    InFlight(Option<Arc<super::progressive::Partial>>),
    Failed(String),
}

impl<T: Transport + 'static> ServerSongSource<T> {
    pub fn new(
        db_path: PathBuf,
        clients: Vec<(String, Arc<ServerClient<T>>)>,
        cache: PlaybackCache,
    ) -> Self {
        ServerSongSource {
            db_path,
            clients: Arc::new(clients),
            cache: Arc::new(cache),
            state: Arc::new(Mutex::new(HashMap::new())),
            keep: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Forget failed attempts, e.g. when the network comes back, so the
    /// next `prepare` tries again.
    pub fn forget_failures(&self) {
        self.state.lock().unwrap().retain(|_, a| matches!(a, Attempt::InFlight(_)));
    }
}

impl<T: Transport + 'static> SongSource for ServerSongSource<T> {
    fn play_context(&self, keep: &[String], ahead: &[String]) {
        let Ok(lib) = MediaLibrary::open_at(&self.db_path) else { return };
        let mut files = HashSet::new();
        for uri in keep.iter().chain(ahead) {
            for copy in server_copies(&lib, uri) {
                let key = crate::media_library::servers::path_key(&copy.song);
                files.insert(self.cache.file_for(&copy.server_id, &key));
            }
        }
        *self.keep.lock().unwrap() = files.clone();
        // What is no longer needed goes now, not only at the next download.
        self.cache.trim(&files);
        for uri in ahead {
            let _ = self.prepare(uri);
        }
    }

    fn prepare(&self, uri: &str) -> Readiness {
        match self.state.lock().unwrap().get(uri) {
            Some(Attempt::InFlight(Some(partial))) => return Readiness::Streaming(Arc::clone(partial)),
            Some(Attempt::InFlight(None)) => return Readiness::Downloading,
            Some(Attempt::Failed(why)) => return Readiness::Unavailable(why.clone()),
            None => {}
        }
        // SQLite connections are not Send, so each call opens its own.
        let lib = match MediaLibrary::open_at(&self.db_path) {
            Ok(lib) => lib,
            Err(e) => return Readiness::Unavailable(format!("library database: {e}")),
        };
        let Some((server_id, key)) = super::uri::parse_song_uri(uri) else {
            return Readiness::Unavailable("not a server song".into());
        };
        let Ok(Some(row)) = lib.server_row_by_key(&server_id, &key) else {
            return Readiness::Unavailable("the server no longer has this song".into());
        };

        // A local copy always wins.
        if let Ok(Some(local)) = lib.local_copy_of(row.id) {
            if let Ok(Some(t)) = lib.tracks_by_ids(&[local]).map(|mut m| m.remove(&local)) {
                let path = PathBuf::from(&t.path);
                if path.is_file() {
                    return Readiness::Ready(path);
                }
            }
        }

        // Every server copy of the song, so any server holding it will do.
        let copies = server_copies(&lib, uri);
        for c in &copies {
            let key = crate::media_library::servers::path_key(&c.song);
            if let Some(hit) = self.cache.cached(&c.server_id, &key) {
                return Readiness::Ready(hit);
            }
        }

        self.state.lock().unwrap().insert(uri.to_string(), Attempt::InFlight(None));
        let (clients, cache, state, keep) =
            (self.clients.clone(), self.cache.clone(), self.state.clone(), self.keep.clone());
        let uri = uri.to_string();
        std::thread::spawn(move || {
            let mut why = "no configured server holds this song".to_string();
            for (server_id, client) in clients.iter() {
                let Some(copy) = copies.iter().find(|c| &c.server_id == server_id) else { continue };
                let key = crate::media_library::servers::path_key(&copy.song);
                let keep = keep.lock().unwrap().clone();
                // Once bytes start to flow, playback can start from them.
                let partial: Mutex<Option<Arc<super::progressive::Partial>>> = Mutex::new(None);
                let started = |url: &str, part: &std::path::Path| {
                    let p = super::progressive::Partial::new(
                        part.to_path_buf(),
                        cache.file_for(server_id, &key),
                        url.to_string(),
                        copy.song.duration_secs.map(|d| d as f64),
                    );
                    state.lock().unwrap().insert(uri.clone(), Attempt::InFlight(Some(Arc::clone(&p))));
                    *partial.lock().unwrap() = Some(p);
                };
                let outcome = cache.fetch_observed(client, server_id, &key, &copy.song.id, &keep, &started);
                if let Some(p) = partial.lock().unwrap().take() {
                    p.finish(outcome.as_ref().map(|_| ()).map_err(|e| e.to_string()));
                }
                match outcome {
                    Ok(_) => {
                        state.lock().unwrap().remove(&uri);
                        return;
                    }
                    Err(e) => {
                        state.lock().unwrap().insert(uri.clone(), Attempt::InFlight(None));
                        why = e.to_string();
                    }
                }
            }
            state.lock().unwrap().insert(uri, Attempt::Failed(why));
        });
        Readiness::Downloading
    }
}

/// Every server copy of the song `uri` names: the named copy first, then
/// the same song on other servers.
fn server_copies(lib: &MediaLibrary, uri: &str) -> Vec<crate::media_library::servers::ServerTrackRow> {
    use crate::media_library::servers::Member;
    let Some((server_id, key)) = super::uri::parse_song_uri(uri) else { return Vec::new() };
    let Ok(Some(row)) = lib.server_row_by_key(&server_id, &key) else { return Vec::new() };
    let mut copies = vec![row.clone()];
    if let Ok(Some(song)) = lib.song_copies(Member::Server(row.id)) {
        for m in song.members {
            if let Member::Server(id) = m.member {
                if id != row.id {
                    if let Ok(Some(other)) = lib.server_row(id) {
                        copies.push(other);
                    }
                }
            }
        }
    }
    copies
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::servers::api::ServerSong;
    use crate::servers::error::ServerError;
    use crate::servers::request::Credentials;
    use crate::servers::transport::{Download, HttpResponse};
    use crate::servers::uri::song_uri;
    use std::path::Path;

    struct Fake {
        offline: bool,
        downloads: Mutex<Vec<String>>,
    }

    impl Transport for Fake {
        fn get(&self, _: &str, _: u64) -> Result<HttpResponse, ServerError> {
            if self.offline {
                return Err(ServerError::unreachable("refused"));
            }
            Ok(HttpResponse {
                status: 200,
                body: br#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#.to_vec(),
            })
        }
        fn get_to_file(&self, url: &str, _: u64, dest: &Path) -> Result<Download, ServerError> {
            self.downloads.lock().unwrap().push(url.split("/rest/").next().unwrap().to_string());
            std::fs::write(dest, b"audio").unwrap();
            Ok(Download { status: 200, content_type: Some("audio/mpeg".into()), bytes: 5 })
        }
    }

    fn client(base: &str, offline: bool) -> ServerClient<Fake> {
        ServerClient::new(
            Some(base.into()),
            None,
            Credentials::Password { username: "me".into(), password: "pw".into() },
            Fake { offline, downloads: Mutex::new(Vec::new()) },
        )
    }

    struct Setup {
        _db: tempfile::NamedTempFile,
        _cache_dir: tempfile::TempDir,
        db_path: PathBuf,
        cache_root: PathBuf,
    }

    /// A library where oscar (and server2) cache the song `/music/A/01.mp3`.
    fn setup(servers: &[&str]) -> Setup {
        let db = tempfile::NamedTempFile::with_suffix(".db").unwrap();
        let lib = MediaLibrary::open_at(db.path()).unwrap();
        for server in servers {
            let pull = lib.begin_server_pull(server).unwrap();
            let song = ServerSong {
                id: format!("{server}-s1"),
                path: Some("/music/A/01.mp3".into()),
                title: "One".into(),
                ..ServerSong::default()
            };
            lib.apply_server_songs(server, pull, &[song]).unwrap();
            lib.finish_server_pull(server, pull).unwrap();
        }
        let cache_dir = tempfile::tempdir().unwrap();
        Setup {
            db_path: db.path().to_path_buf(),
            cache_root: cache_dir.path().to_path_buf(),
            _db: db,
            _cache_dir: cache_dir,
        }
    }

    fn source(s: &Setup, clients: Vec<(String, ServerClient<Fake>)>) -> ServerSongSource<Fake> {
        ServerSongSource::new(
            s.db_path.clone(),
            clients.into_iter().map(|(id, c)| (id, Arc::new(c))).collect(),
            PlaybackCache::new(s.cache_root.clone(), crate::servers::cache::DEFAULT_MAX_BYTES),
        )
    }

    /// Ask until the answer is no longer "downloading".
    fn settle(src: &dyn SongSource, uri: &str) -> Readiness {
        for _ in 0..200 {
            match src.prepare(uri) {
                Readiness::Downloading | Readiness::Streaming(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                other => return other,
            }
        }
        panic!("download never finished");
    }

    #[test]
    fn a_song_not_yet_cached_downloads_in_the_background_then_plays_from_the_cache() {
        let s = setup(&["oscar"]);
        let src = source(&s, vec![("oscar".into(), client("http://oscar", false))]);
        let uri = song_uri("oscar", "/music/A/01.mp3");
        assert_eq!(src.prepare(&uri), Readiness::Downloading);
        match settle(&src, &uri) {
            Readiness::Ready(path) => {
                assert!(path.starts_with(&s.cache_root));
                assert_eq!(std::fs::read(path).unwrap(), b"audio");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_song_with_a_local_copy_plays_the_local_file_without_the_network() {
        let s = setup(&["oscar"]);
        let lib = MediaLibrary::open_at(&s.db_path).unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("01.mp3"), b"local").unwrap();
        let root = dir.path().canonicalize().unwrap();
        let folder = lib.add_folder(root.to_str().unwrap()).unwrap().id();
        lib.rescan_folder_fast(folder, root.to_str().unwrap(), true).unwrap();
        let local = lib.all_tracks().unwrap()[0].clone();
        let server = lib.server_songs("oscar").unwrap()[0].id;
        lib.link_copies(
            crate::media_library::servers::Member::Local(local.id),
            crate::media_library::servers::Member::Server(server),
            crate::servers::matcher::LinkReason::Filename,
        )
        .unwrap();

        let src = source(&s, vec![("oscar".into(), client("http://oscar", true))]);
        assert_eq!(
            src.prepare(&song_uri("oscar", "/music/A/01.mp3")),
            Readiness::Ready(PathBuf::from(local.path))
        );
    }

    #[test]
    fn when_no_server_can_be_reached_the_song_is_unavailable() {
        let s = setup(&["oscar"]);
        let src = source(&s, vec![("oscar".into(), client("http://oscar", true))]);
        assert!(matches!(settle(&src, &song_uri("oscar", "/music/A/01.mp3")), Readiness::Unavailable(_)));
    }

    #[test]
    fn a_song_on_two_servers_comes_from_the_one_that_answers() {
        let s = setup(&["oscar", "server2"]);
        let src = source(
            &s,
            vec![
                ("oscar".into(), client("http://oscar", true)),
                ("server2".into(), client("http://server2", false)),
            ],
        );
        let lib = MediaLibrary::open_at(&s.db_path).unwrap();
        let a = lib.server_songs("oscar").unwrap()[0].id;
        let b = lib.server_songs("server2").unwrap()[0].id;
        lib.link_copies(
            crate::media_library::servers::Member::Server(a),
            crate::media_library::servers::Member::Server(b),
            crate::servers::matcher::LinkReason::Tags,
        )
        .unwrap();
        assert!(matches!(settle(&src, &song_uri("oscar", "/music/A/01.mp3")), Readiness::Ready(_)));
    }

    #[test]
    fn a_song_the_server_no_longer_has_is_unavailable_at_once() {
        let s = setup(&["oscar"]);
        let src = source(&s, vec![("oscar".into(), client("http://oscar", false))]);
        assert!(matches!(src.prepare(&song_uri("oscar", "/music/Gone.mp3")), Readiness::Unavailable(_)));
    }

    #[test]
    fn upcoming_songs_are_fetched_and_the_previous_one_outlives_older_ones() {
        let db = tempfile::NamedTempFile::with_suffix(".db").unwrap();
        let lib = MediaLibrary::open_at(db.path()).unwrap();
        let pull = lib.begin_server_pull("oscar").unwrap();
        let songs: Vec<ServerSong> = (0..3)
            .map(|i| ServerSong {
                id: format!("s{i}"),
                path: Some(format!("/music/{i}.mp3")),
                title: format!("T{i}"),
                ..ServerSong::default()
            })
            .collect();
        lib.apply_server_songs("oscar", pull, &songs).unwrap();
        lib.finish_server_pull("oscar", pull).unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        // Room for two 5-byte songs beyond what is kept.
        let src = ServerSongSource::new(
            db.path().to_path_buf(),
            vec![("oscar".into(), Arc::new(client("http://oscar", false)))],
            PlaybackCache::new(cache_dir.path().to_path_buf(), 10),
        );
        let uri = |i: usize| song_uri("oscar", &format!("/music/{i}.mp3"));
        let cached = |i: usize| {
            PlaybackCache::new(cache_dir.path().to_path_buf(), 10)
                .file_for("oscar", &format!("/music/{i}.mp3"))
                .exists()
        };

        assert!(matches!(settle(&src, &uri(0)), Readiness::Ready(_)));
        // Song 0 plays; song 1 comes next.
        src.play_context(&[uri(0)], &[uri(1)]);
        assert!(matches!(settle(&src, &uri(1)), Readiness::Ready(_)), "fetched ahead");
        // Song 1 plays; 0 is the previous one; 2 comes next.
        std::thread::sleep(std::time::Duration::from_millis(20));
        src.play_context(&[uri(0), uri(1)], &[uri(2)]);
        assert!(matches!(settle(&src, &uri(2)), Readiness::Ready(_)));
        assert!(cached(0) && cached(1) && cached(2), "all three kept while in use");
        // Song 2 plays; 1 is the previous one; 0 is no longer needed.
        src.play_context(&[uri(1), uri(2)], &[]);
        assert!(!cached(0) && cached(1) && cached(2));
    }

    #[test]
    fn after_a_failure_a_retry_waits_until_failures_are_forgotten() {
        let s = setup(&["oscar"]);
        let src = source(&s, vec![("oscar".into(), client("http://oscar", true))]);
        let uri = song_uri("oscar", "/music/A/01.mp3");
        assert!(matches!(settle(&src, &uri), Readiness::Unavailable(_)));
        assert!(matches!(src.prepare(&uri), Readiness::Unavailable(_)), "no retry storm");
        src.forget_failures();
        assert_eq!(src.prepare(&uri), Readiness::Downloading);
    }
}
