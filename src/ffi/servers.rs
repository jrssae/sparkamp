//! JSON-over-FFI server API for the macOS frontend.
//!
//! Server management (list, add, remove, test), the background update
//! worker, the Media Library source filter and the per-row source marks.
//! Same conventions as the device API: JSON in and out through `*mut c_char`
//! freed with [`super::sparkamp_free_string`], and nothing ever panics
//! across the boundary.
//!
//! The logic lives in [`ServersState`], a plain struct the context owns, so
//! it can be tested without a context (which would load the user's config
//! and start the audio engine).
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::{Config, ServerConfig};
use crate::media_library::MediaLibrary;
use crate::media_library::servers::SourceFilter;
use crate::servers::manager::{self, SecretStore, Worker, WorkerRequest};

use super::SparkampCtx;

/// The context's servers: the update worker and the Files source filter.
pub(crate) struct ServersState {
    secrets: Arc<dyn SecretStore>,
    worker: Option<Worker>,
    pub(crate) filter: SourceFilter,
}

/// What a poll reports to Swift.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct PollJson {
    pub status_lines: Vec<String>,
    pub catalog_changed: bool,
    /// Catalog downloads under way; empty when none is.
    pub progress: Vec<ProgressJson>,
}

/// One catalog download under way.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct ProgressJson {
    pub server_id: String,
    pub fetched: u64,
    pub total: Option<u64>,
}

/// The source filter as Swift sends it.
#[derive(Debug, Deserialize)]
struct FilterJson {
    kind: String,
    #[serde(default)]
    server: Option<String>,
}

impl ServersState {
    pub(crate) fn new(secrets: Arc<dyn SecretStore>) -> Self {
        ServersState { secrets, worker: None, filter: SourceFilter::All }
    }

    /// A state for test contexts: in-memory secrets, nothing running.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self::new(Arc::new(manager::MemorySecrets::default()))
    }

    /// Whether any server is running (and the Files list is the merged one).
    pub(crate) fn active(&self) -> bool {
        self.worker.is_some()
    }

    /// (Re)start the enabled servers of `config`, and make their songs
    /// playable.
    pub(crate) fn start(&mut self, config: &Config) {
        let cache = crate::servers::cache::PlaybackCache::in_os_cache_dir(
            crate::servers::cache::max_bytes_from_mb(config.server_sync.cache_max_mb),
        );
        let source = self.start_at(config, MediaLibrary::db_path_pub(), cache);
        crate::servers::playback::install(source);
    }

    /// Start the update worker on the library at `db_path`, downloading
    /// into `cache`. Returns the song source to install, or `None` when no
    /// server is enabled. Installing it is the caller's job: the source is
    /// process-wide.
    pub(crate) fn start_at(
        &mut self,
        config: &Config,
        db_path: std::path::PathBuf,
        cache: crate::servers::cache::PlaybackCache,
    ) -> Option<Arc<dyn crate::servers::playback::SongSource>> {
        self.stop_worker();
        if !config.servers.iter().any(|s| s.enabled) {
            return None;
        }
        let mgr = Arc::new(manager::ServerManager::new(
            db_path,
            &config.servers,
            config.server_sync.clone(),
            self.secrets.as_ref(),
            |_| crate::servers::transport::PlatformTransport::default(),
        ));
        let source = mgr.song_source(cache);
        self.worker = Some(manager::spawn_worker(mgr, std::time::Duration::from_secs(600)));
        Some(source)
    }

    fn stop_worker(&mut self) {
        if let Some(w) = self.worker.take() {
            let _ = w.requests.send(WorkerRequest::Stop);
        }
    }

    /// Add `new` to `config` with its password. Returns the new id, or why
    /// not. The caller saves the config and restarts.
    pub(crate) fn add(&self, config: &mut Config, new: ServerConfig, password: &str) -> Result<String, String> {
        crate::servers::validate::validate_new_server(&new, &config.servers)?;
        self.secrets
            .set(&new.id, password)
            .map_err(|e| format!("Could not store the password: {e}"))?;
        let id = new.id.clone();
        config.servers.push(new);
        Ok(id)
    }

    /// Remove server `id`: its password, its cached catalog, its entry.
    /// Local files are never touched.
    pub(crate) fn remove(&mut self, config: &mut Config, lib: Option<&MediaLibrary>, id: &str) -> bool {
        let before = config.servers.len();
        config.servers.retain(|s| s.id != id);
        if config.servers.len() == before {
            return false;
        }
        let _ = self.secrets.delete(id);
        if let Some(lib) = lib {
            let _ = lib.forget_server(id);
        }
        if self.filter == SourceFilter::Server(id.to_string()) {
            self.filter = SourceFilter::All;
        }
        true
    }

    /// Drain the worker's reports since the last poll.
    pub(crate) fn poll(&mut self) -> Option<PollJson> {
        let w = self.worker.as_ref()?;
        let mut out: Option<PollJson> = None;
        while let Ok(event) = w.events.try_recv() {
            let changed = event.results.iter().any(|(_, r)| {
                r.as_ref().is_ok_and(|u| u.added + u.updated + u.removed > 0 || !u.linked.is_empty())
            });
            let prev = out.as_ref().is_some_and(|p| p.catalog_changed);
            let progress = event
                .progress
                .into_iter()
                .map(|(server_id, p)| ProgressJson { server_id, fetched: p.fetched, total: p.total })
                .collect();
            out = Some(PollJson { status_lines: event.status_lines, catalog_changed: prev || changed, progress });
        }
        out
    }

    pub(crate) fn refresh(&self, id: Option<String>) {
        if let Some(w) = &self.worker {
            let _ = w.requests.send(WorkerRequest::Refresh(id));
        }
    }
}

/// The filter a JSON object from Swift names; `All` for anything unknown.
fn parse_filter(json: &str) -> SourceFilter {
    let Ok(f) = serde_json::from_str::<FilterJson>(json) else { return SourceFilter::All };
    match (f.kind.as_str(), f.server) {
        ("local", _) => SourceFilter::Local,
        ("server", Some(id)) => SourceFilter::Server(id),
        ("local_changes", _) => SourceFilter::LocalChanges,
        ("needs_attention", _) => SourceFilter::NeedsAttention,
        _ => SourceFilter::All,
    }
}

// ─────────────────────────── JSON helpers ───────────────────────────

fn json_out<T: Serialize>(v: &T) -> *mut c_char {
    match serde_json::to_string(v) {
        Ok(s) => CString::new(s).map(|c| c.into_raw()).unwrap_or(std::ptr::null_mut()),
        Err(_) => std::ptr::null_mut(),
    }
}

unsafe fn str_in<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok()
}

// ─────────────────────────── entry points ───────────────────────────

/// Start (or restart) the configured servers. Call once the library is open,
/// and after adding or removing a server.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_servers_start(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.servers.start(&ctx.config);
}

/// The configured servers as a JSON array of `ServerConfig`. No passwords.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_servers_list_json(ctx: *const SparkampCtx) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    json_out(&(*ctx).config.servers)
}

/// Add a server from a `ServerConfig` JSON object (its `id` is ignored and a
/// fresh one generated) and its password. Returns `{"id": …}` or
/// `{"error": …}`. Swift then calls `sparkamp_save_config` and
/// `sparkamp_servers_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_server_add_json(
    ctx: *mut SparkampCtx,
    config_json: *const c_char,
    password: *const c_char,
) -> *mut c_char {
    #[derive(Serialize)]
    struct Out {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    }
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &mut *ctx;
    let parsed: Option<ServerConfig> = str_in(config_json).and_then(|s| serde_json::from_str(s).ok());
    let (Some(mut new), Some(password)) = (parsed, str_in(password)) else {
        return json_out(&Out { id: None, error: Some("Invalid server details.".into()) });
    };
    new.id = ServerConfig::new(&new.name).id;
    match ctx.servers.add(&mut ctx.config, new, password) {
        Ok(id) => json_out(&Out { id: Some(id), error: None }),
        Err(e) => json_out(&Out { id: None, error: Some(e) }),
    }
}

/// Remove server `id`. Returns 1 if it existed. Swift then saves the config
/// and restarts the servers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_server_remove(ctx: *mut SparkampCtx, id: *const c_char) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &mut *ctx;
    let Some(id) = str_in(id) else { return 0 };
    let id = id.to_string();
    let lib = ctx.media_library.as_ref();
    ctx.servers.remove(&mut ctx.config, lib, &id) as c_int
}

/// Test a server: `config_json` is a `ServerConfig`, `password` its
/// password, or null to use the one stored for its id. Each address is tried
/// on its own, so the user sees which answered. Blocks on the network, so
/// call it off the main thread. Returns `{"ok": bool, "message": …,
/// "checks": [{"address": "home" | "remote", "label", "url", "ok",
/// "message"}]}`: `ok` when any address answered, `message` one line per
/// address. Needs no context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_server_test_json(
    config_json: *const c_char,
    password: *const c_char,
) -> *mut c_char {
    let out = |ok: bool, message: String, checks: Vec<CheckJson>| json_out(&TestJson { ok, message, checks });
    let Some(cfg) = str_in(config_json).and_then(|s| serde_json::from_str::<ServerConfig>(s).ok()) else {
        return out(false, "Invalid server details.".into(), Vec::new());
    };
    let password = match str_in(password) {
        Some(p) => Some(p.to_string()),
        None => default_secrets().get(&cfg.id),
    };
    let Some(password) = password else {
        return out(false, "No password stored for this server.".into(), Vec::new());
    };
    let report = test_report(&cfg, &password, |_| crate::servers::transport::PlatformTransport::default());
    out(report.ok, report.message, report.checks)
}

/// What "Test" reports.
#[derive(Debug, Serialize)]
pub(crate) struct TestJson {
    pub ok: bool,
    pub message: String,
    pub checks: Vec<CheckJson>,
}

/// One address's answer.
#[derive(Debug, Serialize)]
pub(crate) struct CheckJson {
    pub address: crate::servers::sync::Address,
    pub label: String,
    pub url: String,
    pub ok: bool,
    pub message: String,
}

/// Test every address of `cfg`, for [`sparkamp_server_test_json`].
pub(crate) fn test_report<T: crate::servers::transport::Transport>(
    cfg: &ServerConfig,
    password: &str,
    make_transport: impl Fn(&str) -> T,
) -> TestJson {
    let checks: Vec<CheckJson> = crate::servers::sync::test_addresses(cfg, password, make_transport)
        .into_iter()
        .map(|c| CheckJson {
            address: c.address,
            label: c.address.label().to_string(),
            url: c.url.clone(),
            ok: c.outcome.is_ok(),
            message: match &c.outcome {
                Ok(r) => r.summary(),
                Err(why) => why.clone(),
            },
        })
        .collect();
    if checks.is_empty() {
        return TestJson { ok: false, message: "Add a home network or remote address.".into(), checks };
    }
    TestJson {
        ok: checks.iter().any(|c| c.ok),
        message: checks.iter().map(|c| format!("{} ({}): {}", c.label, c.url, c.message)).collect::<Vec<_>>().join("\n"),
        checks,
    }
}

/// Ask for an explicit refresh of server `id`, or of all when null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_servers_refresh(ctx: *const SparkampCtx, id: *const c_char) {
    if ctx.is_null() {
        return;
    }
    (*ctx).servers.refresh(str_in(id).map(str::to_string));
}

/// What the worker reported since the last poll, as `{"status_lines": [...],
/// "catalog_changed": bool}`, or null when nothing new. Call from the tick.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_servers_poll_json(ctx: *mut SparkampCtx) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    match (*ctx).servers.poll() {
        Some(p) => json_out(&p),
        None => std::ptr::null_mut(),
    }
}

/// Set the Files source filter: `{"kind": "all" | "local" | "server" |
/// "local_changes" | "needs_attention", "server": id}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_set_source_filter(ctx: *mut SparkampCtx, json: *const c_char) {
    if ctx.is_null() {
        return;
    }
    (*ctx).servers.filter = str_in(json).map(parse_filter).unwrap_or(SourceFilter::All);
}

/// The source marks (three cells each) for the same page
/// `sparkamp_ml_get_tracks` returns with these arguments, as a JSON array of
/// strings. Empty when no servers run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_get_marks_json(
    ctx: *const SparkampCtx,
    query: *const c_char,
    sort_col: *const c_char,
    sort_desc: c_int,
    offset: c_int,
    limit: c_int,
) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let marks: Vec<String> = match merged_rows(ctx, str_in(query), str_in(sort_col), sort_desc != 0) {
        Some(rows) => {
            let start = (offset.max(0) as usize).min(rows.len());
            let end = (start + limit.max(0) as usize).min(rows.len());
            rows[start..end].iter().map(mark).collect()
        }
        None => Vec::new(),
    };
    json_out(&marks)
}

/// Take the server changes for the song of local track `track_id` into its
/// file. Returns the number of fields written, or -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_apply_server_changes(ctx: *const SparkampCtx, track_id: i64) -> c_int {
    if ctx.is_null() {
        return -1;
    }
    let Some(ml) = &(*ctx).media_library else { return -1 };
    match crate::servers::apply::apply_server_changes(ml, crate::media_library::servers::Member::Local(track_id)) {
        Ok(outcome) => outcome.taken.len() as c_int,
        Err(_) => -1,
    }
}

/// Undo the last apply. Returns how many files were put back, or -1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_undo_last_apply(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return -1;
    }
    let Some(ml) = &(*ctx).media_library else { return -1 };
    crate::servers::apply::undo_last_apply(ml).map(|n| n as c_int).unwrap_or(-1)
}

/// The merged Files rows for the context's filter, or `None` when no
/// servers run (the plain library applies).
pub(crate) fn merged_rows(
    ctx: &SparkampCtx,
    query: Option<&str>,
    sort_col: Option<&str>,
    desc: bool,
) -> Option<Vec<crate::media_library::servers::LibraryRow>> {
    if !ctx.servers.active() {
        return None;
    }
    let ml = ctx.media_library.as_ref()?;
    let col = sort_col.filter(|c| !c.is_empty()).unwrap_or("artist");
    ml.library_rows(&ctx.servers.filter, query.filter(|q| !q.is_empty()), col, desc).ok()
}

/// The mark the macOS app turns into one SF Symbol: always the symbol set,
/// which it parses (see `MLFilesTable.sourceIcon`).
fn mark(r: &crate::media_library::servers::LibraryRow) -> String {
    crate::servers::indicator::cells(
        &crate::servers::indicator::Indicator {
            has_local: r.has_local,
            has_server: !r.servers.is_empty(),
            status: r.status,
            possible_match: r.possible_match,
            unreachable: false,
        },
        crate::servers::indicator::MarkStyle::Symbols,
    )
}

/// The platform's password store: the Keychain on macOS, the session only
/// elsewhere.
pub(crate) fn default_secrets() -> Arc<dyn SecretStore> {
    manager::platform_secrets()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::servers::manager::MemorySecrets;

    fn state() -> (ServersState, Arc<MemorySecrets>) {
        let secrets = Arc::new(MemorySecrets::default());
        (ServersState::new(secrets.clone()), secrets)
    }

    fn oscar() -> ServerConfig {
        ServerConfig {
            lan_url: Some("http://oscar.local:4533".into()),
            username: "me".into(),
            ..ServerConfig::new("oscar")
        }
    }

    #[test]
    fn adding_a_server_stores_its_password_and_entry() {
        let (s, secrets) = state();
        let mut config = Config::default();
        let id = s.add(&mut config, oscar(), "sesame").unwrap();
        assert_eq!(config.servers.len(), 1);
        assert_eq!(config.servers[0].id, id);
        assert_eq!(secrets.get(&id).as_deref(), Some("sesame"));
    }

    #[test]
    fn an_invalid_server_is_refused_with_the_reason() {
        let (s, secrets) = state();
        let mut config = Config::default();
        let mut bad = oscar();
        bad.remote_url = Some("http://music.example.com".into());
        let err = s.add(&mut config, bad.clone(), "pw").unwrap_err();
        assert!(err.contains("https"), "{err}");
        assert!(config.servers.is_empty());
        assert_eq!(secrets.get(&bad.id), None, "no password stored for a refused server");
    }

    #[test]
    fn removing_a_server_drops_its_password_and_cached_catalog() {
        let (mut s, secrets) = state();
        let mut config = Config::default();
        let id = s.add(&mut config, oscar(), "sesame").unwrap();
        let db = tempfile::NamedTempFile::with_suffix(".db").unwrap();
        let lib = MediaLibrary::open_at(db.path()).unwrap();
        let pull = lib.begin_server_pull(&id).unwrap();
        lib.apply_server_songs(
            &id,
            pull,
            &[crate::servers::api::ServerSong { id: "s1".into(), title: "x".into(), ..Default::default() }],
        )
        .unwrap();

        assert!(s.remove(&mut config, Some(&lib), &id));
        assert!(config.servers.is_empty());
        assert_eq!(secrets.get(&id), None);
        assert!(lib.server_songs(&id).unwrap().is_empty());
        assert!(!s.remove(&mut config, Some(&lib), &id), "already gone");
    }

    // ───────────── through the FFI, as the macOS app calls it ─────────────

    /// A small Navidrome on a loopback port: two songs, a cover, audio.
    /// Counts `search3` calls; `down` makes it answer 503 like a server in
    /// maintenance.
    struct FakeNavidrome {
        base: String,
        pulls: Arc<std::sync::atomic::AtomicUsize>,
        down: Arc<std::sync::atomic::AtomicBool>,
    }

    fn fake_navidrome() -> FakeNavidrome {
        use std::io::{Read, Write};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let pulls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let down = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (p, d) = (pulls.clone(), down.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&req).lines().next().unwrap_or("").to_string();
                let path = line.split(' ').nth(1).unwrap_or("");
                let endpoint = path.split("/rest/").nth(1).unwrap_or("").split(['?', '.']).next().unwrap_or("");
                let ok = |inner: &str| {
                    format!(
                        r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1","type":"navidrome","serverVersion":"0.64.2","openSubsonic":true{inner}}}}}"#
                    )
                };
                let song = |id: &str, title: &str, path: &str| {
                    format!(
                        r#"{{"id":"{id}","title":"{title}","artist":"Artist A","album":"Album One","albumId":"al-1","track":1,"duration":2,"suffix":"mp3","size":1000,"path":"{path}","coverArt":"mf-{id}"}}"#
                    )
                };
                let (ctype, body): (&str, Vec<u8>) = if d.load(Ordering::SeqCst) {
                    ("text/html", b"<html>maintenance</html>".to_vec())
                } else {
                    match endpoint {
                        "getScanStatus" => (
                            "application/json",
                            ok(r#","scanStatus":{"scanning":false,"count":2,"lastScan":"2026-09-29T03:00:00Z"}"#).into_bytes(),
                        ),
                        "search3" => {
                            p.fetch_add(1, Ordering::SeqCst);
                            let songs = if path.contains("songOffset=0") {
                                format!(
                                    "{},{}",
                                    song("s1", "Alpha", "/music/Artist A/Album One/01 Alpha.mp3"),
                                    song("s2", "Beta", "/music/Artist A/Album One/02 Beta.mp3")
                                )
                            } else {
                                String::new()
                            };
                            let songs = if path.contains("songCount=1&") { song("s1", "Alpha", "/music/a.mp3") } else { songs };
                            ("application/json", ok(&format!(r#","searchResult3":{{"song":[{songs}]}}"#)).into_bytes())
                        }
                        "getOpenSubsonicExtensions" => {
                            ("application/json", ok(r#","openSubsonicExtensions":[]"#).into_bytes())
                        }
                        "getCoverArt" => ("image/png", b"\x89PNG\r\n\x1a\nfake".to_vec()),
                        "stream" => ("audio/mpeg", vec![0u8; 1000]),
                        _ => ("application/json", ok("").into_bytes()),
                    }
                };
                let status = if d.load(Ordering::SeqCst) { "503 Service Unavailable" } else { "200 OK" };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        FakeNavidrome { base, pulls, down }
    }

    /// A real context with a library in `dir`, no servers running yet.
    fn ctx_in(dir: &std::path::Path) -> SparkampCtx {
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().expect("GStreamer must be available for tests");
        let (meta_tx, meta_rx) = std::sync::mpsc::channel();
        let (duration_tx, duration_rx) = std::sync::mpsc::channel();
        SparkampCtx {
            servers: ServersState::for_tests(),
            player: crate::engine::Player::new().expect("Player::new"),
            playlist: crate::model::Playlist::new(),
            config: Config::default(),
            shuffle_state: crate::shuffle::ShuffleState::new(),
            queue: crate::queue::Queue::new(),
            meta_tx,
            meta_rx,
            duration_tx,
            duration_rx,
            dirty_count: 0,
            last_known_duration: None,
            pending_seek: None,
            eos_cb: None,
            eos_userdata: std::ptr::null_mut(),
            error_cb: None,
            error_userdata: std::ptr::null_mut(),
            position_cb: None,
            position_userdata: std::ptr::null_mut(),
            media_library: Some(MediaLibrary::open_at(&dir.join("library.db")).unwrap()),
            ml_progress: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            ml_scanning: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            ml_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rg_progress: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            rg_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rg_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            watch: None,
            watch_rx: None,
        }
    }

    /// Add a server through the FFI with the JSON Swift's encoder writes
    /// (snake_case, no `remote_url` when unset), and return its id.
    fn add_via_ffi(ctx: &mut SparkampCtx, name: &str, url: &str) -> String {
        let json = CString::new(format!(
            r#"{{"id":"","name":"{name}","lan_url":"{url}","username":"tester","enabled":true,"priority":0}}"#
        ))
        .unwrap();
        let pw = CString::new("testpw").unwrap();
        let out = take(unsafe { sparkamp_server_add_json(ctx, json.as_ptr(), pw.as_ptr()) });
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        v["id"].as_str().unwrap_or_else(|| panic!("add refused: {out}")).to_string()
    }

    fn take(p: *mut c_char) -> String {
        assert!(!p.is_null());
        unsafe { CString::from_raw(p) }.into_string().unwrap()
    }

    /// Start the servers on the context's library, keeping the process-wide
    /// song source out of it (other tests share that).
    fn start(ctx: &mut SparkampCtx, dir: &std::path::Path) {
        let config = ctx.config.clone();
        ctx.servers.start_at(
            &config,
            dir.join("library.db"),
            crate::servers::cache::PlaybackCache::new(dir.join("cache"), 1 << 20),
        );
    }

    /// Poll as the app's tick does until the worker reports, or panic.
    fn poll_until_reported(ctx: &mut SparkampCtx) -> serde_json::Value {
        for _ in 0..200 {
            let p = unsafe { sparkamp_servers_poll_json(ctx) };
            if !p.is_null() {
                return serde_json::from_str(&take(p)).unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("the worker never reported");
    }

    fn tracks(ctx: &SparkampCtx) -> Vec<(i64, String, String)> {
        use crate::ffi::media_library::{SparkampLibTrack, sparkamp_ml_get_tracks};
        let mut buf: Vec<SparkampLibTrack> = Vec::with_capacity(50);
        let q = CString::new("").unwrap();
        let col = CString::new("title").unwrap();
        let n = unsafe { sparkamp_ml_get_tracks(ctx, q.as_ptr(), col.as_ptr(), 0, 0, 50, buf.as_mut_ptr()) };
        unsafe { buf.set_len(n as usize) };
        let text = |b: &[u8]| String::from_utf8_lossy(&b[..b.iter().position(|c| *c == 0).unwrap_or(b.len())]).into_owned();
        buf.iter().map(|t| (t.id, text(&t.title), text(&t.path))).collect()
    }

    fn scanned_flags(ctx: &SparkampCtx) -> Vec<c_int> {
        use crate::ffi::media_library::{SparkampLibTrack, sparkamp_ml_get_tracks};
        let mut buf: Vec<SparkampLibTrack> = Vec::with_capacity(50);
        let q = CString::new("").unwrap();
        let n = unsafe { sparkamp_ml_get_tracks(ctx, q.as_ptr(), std::ptr::null(), 0, 0, 50, buf.as_mut_ptr()) };
        unsafe { buf.set_len(n as usize) };
        buf.iter().map(|t| t.scanned).collect()
    }

    fn marks(ctx: &SparkampCtx) -> Vec<String> {
        let q = CString::new("").unwrap();
        let col = CString::new("title").unwrap();
        let out = take(unsafe { sparkamp_ml_get_marks_json(ctx, q.as_ptr(), col.as_ptr(), 0, 0, 50) });
        serde_json::from_str(&out).unwrap()
    }

    #[test]
    fn the_app_adds_a_server_and_its_catalog_shows_in_files_with_marks() {
        let fake = fake_navidrome();
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ctx_in(dir.path());
        let id = add_via_ffi(&mut ctx, "fakeoscar", &fake.base);
        let listed = take(unsafe { sparkamp_servers_list_json(&ctx) });
        assert!(listed.contains("fakeoscar") && !listed.contains("testpw"), "{listed}");

        start(&mut ctx, dir.path());
        let polled = poll_until_reported(&mut ctx);
        assert_eq!(polled["catalog_changed"], true, "{polled}");
        let line = polled["status_lines"][0].as_str().unwrap();
        assert!(line.starts_with("fakeoscar: updated"), "{line}");

        let rows = tracks(&ctx);
        let titles: Vec<&str> = rows.iter().map(|r| r.1.as_str()).collect();
        assert_eq!(titles, ["Alpha", "Beta"]);
        assert!(rows.iter().all(|r| r.0 < 0), "server-only rows carry negative ids: {rows:?}");
        assert!(
            scanned_flags(&ctx).iter().all(|s| *s == 1),
            "a server song's tags came from the server, so it is not shown as waiting for a scan"
        );
        assert_eq!(rows[0].2, format!("subsonic://{id}//music/Artist%20A/Album%20One/01%20Alpha.mp3"));
        assert_eq!(marks(&ctx), [" ☁ ", " ☁ "], "one mark per row, server only");

        let local = CString::new(r#"{"kind":"local"}"#).unwrap();
        unsafe { sparkamp_ml_set_source_filter(&mut ctx, local.as_ptr()) };
        assert!(tracks(&ctx).is_empty(), "no local copies, so the Local filter is empty");
        assert!(marks(&ctx).is_empty());
        let this_server = CString::new(format!(r#"{{"kind":"server","server":"{id}"}}"#)).unwrap();
        unsafe { sparkamp_ml_set_source_filter(&mut ctx, this_server.as_ptr()) };
        assert_eq!(tracks(&ctx).len(), 2);
        ctx.servers.stop_worker();
    }

    #[test]
    fn a_refresh_from_the_app_pulls_again_and_a_down_server_keeps_its_catalog() {
        use std::sync::atomic::Ordering;
        let fake = fake_navidrome();
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ctx_in(dir.path());
        add_via_ffi(&mut ctx, "fakeoscar", &fake.base);
        start(&mut ctx, dir.path());
        poll_until_reported(&mut ctx);
        assert_eq!(fake.pulls.load(Ordering::SeqCst), 1);

        unsafe { sparkamp_servers_refresh(&ctx, std::ptr::null()) };
        poll_until_reported(&mut ctx);
        assert_eq!(fake.pulls.load(Ordering::SeqCst), 2, "an explicit refresh pulls even when up to date");

        fake.down.store(true, Ordering::SeqCst);
        unsafe { sparkamp_servers_refresh(&ctx, std::ptr::null()) };
        let polled = poll_until_reported(&mut ctx);
        let line = polled["status_lines"][0].as_str().unwrap();
        assert!(line.starts_with("fakeoscar: not responding"), "{line}");
        assert_eq!(tracks(&ctx).len(), 2, "the cached catalog stays listed");
        ctx.servers.stop_worker();
    }

    #[test]
    fn test_connection_from_the_app_reports_the_server() {
        let fake = fake_navidrome();
        let json = CString::new(format!(
            r#"{{"id":"x","name":"fakeoscar","lan_url":"{}","username":"tester","enabled":true,"priority":0}}"#,
            fake.base
        ))
        .unwrap();
        let pw = CString::new("testpw").unwrap();
        let out: serde_json::Value =
            serde_json::from_str(&take(unsafe { sparkamp_server_test_json(json.as_ptr(), pw.as_ptr()) })).unwrap();
        assert_eq!(out["ok"], true, "{out}");
        let check = &out["checks"][0];
        assert_eq!(check["address"], "home", "{out}");
        assert_eq!(check["label"], "Home network");
        assert_eq!(check["ok"], true);
        let message = check["message"].as_str().unwrap();
        assert!(message.contains("navidrome") && message.contains("Real paths: yes"), "{message}");
        assert!(out["message"].as_str().unwrap().starts_with("Home network (http://127.0.0.1:"), "{out}");
    }

    #[test]
    fn test_reports_each_address_apart_and_names_the_one_that_failed() {
        let fake = fake_navidrome();
        // Nothing listens on this port.
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let cfg = ServerConfig {
            lan_url: Some(fake.base.clone()),
            remote_url: Some(format!("https://127.0.0.1:{dead}")),
            username: "tester".into(),
            ..ServerConfig::new("fakeoscar")
        };
        let report = test_report(&cfg, "testpw", |_| crate::servers::transport::MinreqTransport);
        assert!(report.ok, "the home address answered");
        assert_eq!(report.checks.len(), 2);
        assert!(report.checks[0].ok && !report.checks[1].ok);
        let lines: Vec<&str> = report.message.lines().collect();
        assert!(lines[0].starts_with("Home network (") && lines[0].contains("Connected"), "{lines:?}");
        assert!(lines[1].starts_with(&format!("Remote (https://127.0.0.1:{dead}): ")), "{lines:?}");
    }

    #[test]
    fn a_catalog_download_reports_progress_to_the_app() {
        let fake = fake_navidrome();
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ctx_in(dir.path());
        add_via_ffi(&mut ctx, "fakeoscar", &fake.base);
        start(&mut ctx, dir.path());
        let polled = poll_until_reported(&mut ctx);
        // The download may finish before the first poll; either way the key
        // is there, and empty once nothing is under way.
        assert!(polled["progress"].is_array(), "{polled}");
        ctx.servers.stop_worker();
    }

    #[test]
    fn removing_a_server_through_the_app_empties_files() {
        let fake = fake_navidrome();
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ctx_in(dir.path());
        let id = add_via_ffi(&mut ctx, "fakeoscar", &fake.base);
        start(&mut ctx, dir.path());
        poll_until_reported(&mut ctx);
        assert_eq!(tracks(&ctx).len(), 2);

        let cid = CString::new(id).unwrap();
        assert_eq!(unsafe { sparkamp_server_remove(&mut ctx, cid.as_ptr()) }, 1);
        let config = ctx.config.clone();
        ctx.servers.start_at(&config, dir.path().join("library.db"), crate::servers::cache::PlaybackCache::new(dir.path().join("cache"), 1 << 20));
        assert!(!ctx.servers.active(), "no servers left, nothing runs");
        assert!(tracks(&ctx).is_empty(), "the cached catalog went with the server");
        assert_eq!(take(unsafe { sparkamp_servers_list_json(&ctx) }), "[]");
    }

    /// The macOS app plays a song still downloading from its tick: a jump
    /// waits on the download, and the first tick after it lands plays it.
    #[test]
    fn the_tick_plays_a_server_song_once_its_download_lands() {
        use crate::engine::PlayerState;
        crate::servers::playback::install_test_answers();
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ctx_in(dir.path());
        ctx.config.playback.volume = 0.0;
        let uri = "subsonic://oscar//music/ffi-tick-wait.mp3";
        ctx.playlist.add(crate::model::Track {
            path: std::path::PathBuf::from(uri),
            title: "waiting".into(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            duration: None,
            broken: false,
            read_only: false,
            id: 0,
        });
        unsafe { crate::ffi::playlist::sparkamp_playlist_jump(&mut ctx, 0) };
        assert_eq!(*ctx.player.state(), PlayerState::Stopped, "still downloading");
        assert_eq!(ctx.player.waiting_for_download(), Some(uri));

        // A real, silent WAV for the engine to open.
        let wav = dir.path().join("landed.wav");
        let frames: u32 = 44_100;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + frames * 2).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&44_100u32.to_le_bytes());
        bytes.extend_from_slice(&88_200u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(frames * 2).to_le_bytes());
        bytes.resize(bytes.len() + (frames * 2) as usize, 0);
        std::fs::write(&wav, bytes).unwrap();
        crate::servers::playback::finish_download_at_for_tests(uri, wav);

        std::thread::sleep(crate::engine::DOWNLOAD_RETRY);
        unsafe { crate::ffi::sparkamp_tick(&mut ctx) };
        assert_eq!(*ctx.player.state(), PlayerState::Playing, "played on the tick after it landed");
        assert_eq!(ctx.player.waiting_for_download(), None);
        let _ = ctx.player.stop();
    }

    #[test]
    fn filters_parse_from_swift_json() {
        assert_eq!(parse_filter(r#"{"kind":"all"}"#), SourceFilter::All);
        assert_eq!(parse_filter(r#"{"kind":"local"}"#), SourceFilter::Local);
        assert_eq!(parse_filter(r#"{"kind":"server","server":"abc"}"#), SourceFilter::Server("abc".into()));
        assert_eq!(parse_filter(r#"{"kind":"local_changes"}"#), SourceFilter::LocalChanges);
        assert_eq!(parse_filter(r#"{"kind":"needs_attention"}"#), SourceFilter::NeedsAttention);
        assert_eq!(parse_filter(r#"{"kind":"server"}"#), SourceFilter::All, "no server named");
        assert_eq!(parse_filter("nonsense"), SourceFilter::All);
    }
}
