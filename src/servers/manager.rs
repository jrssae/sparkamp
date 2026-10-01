//! Everything a frontend needs from its servers, in one object.
//!
//! The manager owns one client per enabled server, each server's health, and
//! the song source the player resolves server songs through. Its update
//! calls block on the network, so frontends call them from a background
//! thread; each opens its own database connection because SQLite
//! connections cannot cross threads.

use super::client::ServerClient;
use super::playback::ServerSongSource;
use super::request::Credentials;
use super::status::Health;
use super::sync::{self, UpdateError, UpdateReport};
use super::transport::Transport;
use crate::config::{ServerConfig, ServerSyncConfig};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Where server passwords live. The real ones are the OS keychains; tests
/// and headless sessions use [`MemorySecrets`].
pub trait SecretStore: Send + Sync {
    /// The password for server `server_id`, if one is stored.
    fn get(&self, server_id: &str) -> Option<String>;
    fn set(&self, server_id: &str, secret: &str) -> anyhow::Result<()>;
    fn delete(&self, server_id: &str) -> anyhow::Result<()>;
}

/// Secrets held in memory for this session only: a headless machine without
/// a keyring, which asks for the password each session.
#[derive(Default)]
pub struct MemorySecrets(Mutex<HashMap<String, String>>);

impl SecretStore for MemorySecrets {
    fn get(&self, server_id: &str) -> Option<String> {
        self.0.lock().unwrap().get(server_id).cloned()
    }
    fn set(&self, server_id: &str, secret: &str) -> anyhow::Result<()> {
        self.0.lock().unwrap().insert(server_id.to_string(), secret.to_string());
        Ok(())
    }
    fn delete(&self, server_id: &str) -> anyhow::Result<()> {
        self.0.lock().unwrap().remove(server_id);
        Ok(())
    }
}

/// The password store a frontend should use: the macOS Keychain, or the
/// session-only store elsewhere. Debug builds also honour
/// `SPARKAMP_TEST_PASSWORDS="<server id>=<password>,…"`, which fills the
/// session-only store instead; end-to-end tests use it to run against a fake
/// server without touching the Keychain. Release builds never read it.
pub fn platform_secrets() -> std::sync::Arc<dyn SecretStore> {
    #[cfg(debug_assertions)]
    if let Ok(list) = std::env::var("SPARKAMP_TEST_PASSWORDS") {
        return std::sync::Arc::new(MemorySecrets::from_list(&list));
    }
    #[cfg(target_os = "macos")]
    {
        std::sync::Arc::new(KeychainSecrets)
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::sync::Arc::new(MemorySecrets::default())
    }
}

impl MemorySecrets {
    /// A store holding `"<id>=<password>,…"`.
    pub fn from_list(list: &str) -> Self {
        let store = MemorySecrets::default();
        for pair in list.split(',') {
            if let Some((id, password)) = pair.split_once('=') {
                let _ = store.set(id.trim(), password);
            }
        }
        store
    }
}

/// Passwords in the macOS Keychain, one generic password per server id.
/// Works inside the App Sandbox: the items belong to the app's own access
/// group.
#[cfg(target_os = "macos")]
pub struct KeychainSecrets;

#[cfg(target_os = "macos")]
impl KeychainSecrets {
    /// The Keychain "service" the items are filed under.
    pub const SERVICE: &'static str = "Sparkamp server";
}

#[cfg(target_os = "macos")]
impl SecretStore for KeychainSecrets {
    fn get(&self, server_id: &str) -> Option<String> {
        security_framework::passwords::get_generic_password(Self::SERVICE, server_id)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
    }
    fn set(&self, server_id: &str, secret: &str) -> anyhow::Result<()> {
        security_framework::passwords::set_generic_password(Self::SERVICE, server_id, secret.as_bytes())
            .map_err(|e| anyhow::anyhow!("keychain: {e}"))
    }
    fn delete(&self, server_id: &str) -> anyhow::Result<()> {
        match security_framework::passwords::delete_generic_password(Self::SERVICE, server_id) {
            Ok(()) => Ok(()),
            // Already gone is what delete wanted.
            Err(e) if e.code() == -25300 => Ok(()),
            Err(e) => Err(anyhow::anyhow!("keychain: {e}")),
        }
    }
}

/// The servers of one running Sparkamp.
pub struct ServerManager<T: Transport + 'static> {
    db_path: PathBuf,
    /// Enabled servers in priority order.
    servers: Vec<ServerConfig>,
    clients: HashMap<String, Arc<ServerClient<T>>>,
    health: Mutex<HashMap<String, Health>>,
    sync_config: ServerSyncConfig,
    /// The song source handed out, so a network change can clear its
    /// failed downloads.
    source: Mutex<Option<Arc<ServerSongSource<T>>>>,
}

impl<T: Transport + 'static> ServerManager<T> {
    /// Build clients for the enabled servers in `servers`. A server with no
    /// stored password starts as [`Health::NoPassword`].
    pub fn new(
        db_path: PathBuf,
        servers: &[ServerConfig],
        sync_config: ServerSyncConfig,
        secrets: &dyn SecretStore,
        make_transport: impl Fn(&ServerConfig) -> T,
    ) -> Self {
        let mut enabled: Vec<ServerConfig> = servers.iter().filter(|s| s.enabled).cloned().collect();
        enabled.sort_by_key(|s| s.priority);
        let mut clients = HashMap::new();
        let mut health = HashMap::new();
        for s in &enabled {
            match secrets.get(&s.id) {
                Some(password) => {
                    let creds = Credentials::Password { username: s.username.clone(), password };
                    clients.insert(
                        s.id.clone(),
                        Arc::new(ServerClient::new(
                            s.lan_url.clone(),
                            s.remote_url.clone(),
                            creds,
                            make_transport(s),
                        )),
                    );
                }
                None => {
                    health.insert(s.id.clone(), Health::NoPassword);
                }
            }
        }
        ServerManager {
            db_path,
            servers: enabled,
            clients,
            health: Mutex::new(health),
            sync_config,
            source: Mutex::new(None),
        }
    }

    /// The health of `server_id`.
    pub fn health(&self, server_id: &str) -> Health {
        self.health.lock().unwrap().get(server_id).cloned().unwrap_or(Health::Unknown)
    }

    /// Update every server whose periodic update is due (or all of them
    /// when `on_launch` is set and this is the launch). Queued ratings and
    /// plays go first. One server failing never stops the others.
    pub fn run_due_updates(&self, launching: bool) -> Vec<(String, Result<UpdateReport, UpdateError>)> {
        let now = std::time::SystemTime::now();
        let mut results = Vec::new();
        for s in &self.servers {
            if !self.clients.contains_key(&s.id) || !self.health(&s.id).allows_automatic_contact() {
                continue;
            }
            let last = self.open_lib().ok().and_then(|lib| lib.server_last_success(&s.id).ok().flatten());
            let due = super::status::update_due(last, self.sync_config.update_interval_hours, now)
                || (launching && self.sync_config.update_on_launch);
            if due {
                results.push((s.id.clone(), self.update_one(&s.id, false)));
            }
        }
        results
    }

    /// An explicit refresh of one server, or all: pulls even when the server
    /// has not scanned since.
    pub fn refresh(&self, server_id: Option<&str>) -> Vec<(String, Result<UpdateReport, UpdateError>)> {
        self.servers
            .iter()
            .filter(|s| server_id.is_none_or(|id| id == s.id))
            .filter(|s| self.clients.contains_key(&s.id))
            .map(|s| (s.id.clone(), self.update_one(&s.id, true)))
            .collect()
    }

    /// Send the queue, then update the catalog, recording the server's
    /// health either way.
    fn update_one(&self, server_id: &str, force: bool) -> Result<UpdateReport, UpdateError> {
        let client = &self.clients[server_id];
        let lib = self.open_lib().map_err(UpdateError::Storage)?;
        let outcome = sync::send_pending(client, &lib, server_id)
            .and_then(|_| sync::update_catalog(client, &lib, server_id, force));
        if outcome.is_ok() {
            // Covers are a bonus: a failure here never fails the update, and
            // what is missing is fetched next time.
            let covers = dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("sparkamp")
                .join("server-covers");
            if let Err(e) = sync::fetch_covers(client, &lib, server_id, &covers) {
                eprintln!("[servers] covers for {server_id}: {e}");
            }
        }
        let health = match &outcome {
            Ok(report) if report.postponed_for_scan => Health::Scanning,
            Ok(_) => Health::Online,
            Err(UpdateError::Server(e)) => Health::after_error(e, false),
            Err(UpdateError::Storage(_) | UpdateError::LocalFile(_)) => self.health(server_id),
        };
        self.health.lock().unwrap().insert(server_id.to_string(), health);
        outcome
    }

    /// One status-bar line per enabled server.
    pub fn status_lines(&self) -> Vec<String> {
        let now = std::time::SystemTime::now();
        let lib = self.open_lib().ok();
        self.servers
            .iter()
            .map(|s| {
                let since = lib
                    .as_ref()
                    .and_then(|l| l.server_last_success(&s.id).ok().flatten())
                    .and_then(|t| now.duration_since(t).ok());
                super::status::status_line(&s.name, &self.health(&s.id), since)
            })
            .collect()
    }

    /// A song source over these servers, in priority order, for
    /// [`super::playback::install`].
    pub fn song_source(&self, cache: super::cache::PlaybackCache) -> Arc<ServerSongSource<T>> {
        let clients = self
            .servers
            .iter()
            .filter_map(|s| self.clients.get(&s.id).map(|c| (s.id.clone(), c.clone())))
            .collect();
        let source = Arc::new(ServerSongSource::new(self.db_path.clone(), clients, cache));
        *self.source.lock().unwrap() = Some(source.clone());
        source
    }

    /// The OS reports the network changed: servers that were offline may be
    /// back, so forget what failed and let the next attempt try.
    pub fn network_changed(&self) {
        for h in self.health.lock().unwrap().values_mut() {
            if matches!(h, Health::Offline { .. }) {
                *h = Health::Unknown;
            }
        }
        if let Some(source) = self.source.lock().unwrap().as_ref() {
            source.forget_failures();
        }
    }

    fn open_lib(&self) -> anyhow::Result<crate::media_library::MediaLibrary> {
        crate::media_library::MediaLibrary::open_at(&self.db_path)
    }
}

/// A request to the background worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerRequest {
    /// Explicit refresh of one server, or all (`None`).
    Refresh(Option<String>),
    /// The OS reports a network change: retry what failed.
    NetworkChanged,
    Stop,
}

/// What the worker reports after each round.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkerEvent {
    /// `(server id, what happened)`; the text is safe to show.
    pub results: Vec<(String, Result<UpdateReport, String>)>,
    /// Fresh status lines for every enabled server.
    pub status_lines: Vec<String>,
}

/// The worker's two ends, held by the frontend.
pub struct Worker {
    pub requests: std::sync::mpsc::Sender<WorkerRequest>,
    pub events: std::sync::mpsc::Receiver<WorkerEvent>,
}

/// Run `manager` on a background thread: due updates every `check_every`
/// (and at once, as the launch check), explicit refreshes on request.
pub fn spawn_worker<T: Transport + 'static>(
    manager: Arc<ServerManager<T>>,
    check_every: std::time::Duration,
) -> Worker {
    use std::sync::mpsc::RecvTimeoutError;
    let (requests, rx) = std::sync::mpsc::channel();
    let (tx, events) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let event = |results: Vec<(String, Result<UpdateReport, UpdateError>)>| WorkerEvent {
            results: results.into_iter().map(|(id, r)| (id, r.map_err(|e| e.to_string()))).collect(),
            status_lines: manager.status_lines(),
        };
        if tx.send(event(manager.run_due_updates(true))).is_err() {
            return;
        }
        loop {
            let results = match rx.recv_timeout(check_every) {
                Ok(WorkerRequest::Refresh(id)) => manager.refresh(id.as_deref()),
                Ok(WorkerRequest::NetworkChanged) => {
                    manager.network_changed();
                    manager.run_due_updates(false)
                }
                Err(RecvTimeoutError::Timeout) => manager.run_due_updates(false),
                Ok(WorkerRequest::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            };
            if tx.send(event(results)).is_err() {
                return;
            }
        }
    });
    Worker { requests, events }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::servers::error::ServerError;
    use crate::servers::transport::{Download, HttpResponse};
    use serde_json::json;

    /// A small Subsonic server: `songs` songs, reachable or not.
    struct Fake {
        songs: usize,
        offline: bool,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Transport for Fake {
        fn get(&self, url: &str, _: u64) -> Result<HttpResponse, ServerError> {
            let endpoint = url.split("/rest/").nth(1).unwrap().split('?').next().unwrap().to_string();
            self.calls.lock().unwrap().push(endpoint.clone());
            if self.offline {
                return Err(ServerError::unreachable("refused"));
            }
            let inner = match endpoint.as_str() {
                "getScanStatus" => json!({"scanStatus": {"scanning": false, "lastScan": "2026-09-29T00:00:00Z"}}),
                "search3" => {
                    let offset: usize = url.split("songOffset=").nth(1).unwrap().split('&').next().unwrap().parse().unwrap();
                    let songs: Vec<_> = (offset..self.songs)
                        .map(|i| json!({"id": format!("s{i}"), "title": format!("T{i}"), "path": format!("/music/{i}.mp3")}))
                        .collect();
                    json!({"searchResult3": {"song": songs}})
                }
                _ => json!({}),
            };
            let mut r = json!({"status": "ok", "version": "1.16.1"});
            r.as_object_mut().unwrap().extend(inner.as_object().unwrap().clone());
            Ok(HttpResponse { status: 200, body: json!({"subsonic-response": r}).to_string().into_bytes() })
        }
        fn get_to_file(&self, _: &str, _: u64, _: &std::path::Path) -> Result<Download, ServerError> {
            Err(ServerError::unreachable("no downloads here"))
        }
    }

    fn server(id: &str, enabled: bool) -> ServerConfig {
        ServerConfig {
            id: id.into(),
            name: id.into(),
            lan_url: Some(format!("http://{id}.local:4533")),
            enabled,
            username: "me".into(),
            ..ServerConfig::default()
        }
    }

    struct World {
        _db: tempfile::NamedTempFile,
        manager: ServerManager<Fake>,
        calls: HashMap<String, Arc<Mutex<Vec<String>>>>,
    }

    /// Servers by (id, enabled, has password, offline).
    fn world(servers: &[(&str, bool, bool, bool)]) -> World {
        let db = tempfile::NamedTempFile::with_suffix(".db").unwrap();
        crate::media_library::MediaLibrary::open_at(db.path()).unwrap();
        let secrets = MemorySecrets::default();
        let mut calls = HashMap::new();
        let mut offline = HashMap::new();
        for (id, _, has_pw, off) in servers {
            if *has_pw {
                secrets.set(id, "pw").unwrap();
            }
            calls.insert(id.to_string(), Arc::new(Mutex::new(Vec::new())));
            offline.insert(id.to_string(), *off);
        }
        let configs: Vec<ServerConfig> = servers.iter().map(|(id, en, _, _)| server(id, *en)).collect();
        let calls2 = calls.clone();
        let manager = ServerManager::new(
            db.path().to_path_buf(),
            &configs,
            ServerSyncConfig::default(),
            &secrets,
            move |cfg| Fake { songs: 3, offline: offline[&cfg.id], calls: calls2[&cfg.id].clone() },
        );
        World { _db: db, manager, calls }
    }

    fn pulls(w: &World, id: &str) -> usize {
        w.calls[id].lock().unwrap().iter().filter(|e| *e == "search3").count()
    }

    #[test]
    fn a_password_list_fills_the_session_store() {
        let s = MemorySecrets::from_list("a=one,b=two=too");
        assert_eq!(s.get("a").as_deref(), Some("one"));
        assert_eq!(s.get("b").as_deref(), Some("two=too"), "only the first = splits");
        assert_eq!(s.get("c"), None);
    }

    /// Touches the real login keychain, so it only runs when asked:
    /// `cargo test -- --ignored keychain`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn keychain_round_trips_a_password() {
        let k = KeychainSecrets;
        let id = format!("sparkamp-test-{}", std::process::id());
        k.set(&id, "sesame").unwrap();
        assert_eq!(k.get(&id).as_deref(), Some("sesame"));
        k.delete(&id).unwrap();
        assert_eq!(k.get(&id), None);
        k.delete(&id).unwrap();
    }

    #[test]
    fn a_server_never_updated_is_updated_and_then_not_again_until_due() {
        let w = world(&[("oscar", true, true, false)]);
        let first = w.manager.run_due_updates(false);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].1.as_ref().unwrap().added, 3);
        assert!(w.manager.run_due_updates(false).is_empty(), "not due again for a day");
        assert_eq!(pulls(&w, "oscar"), 1);
        assert_eq!(w.manager.health("oscar"), Health::Online);
    }

    #[test]
    fn an_explicit_refresh_pulls_even_when_not_due() {
        let w = world(&[("oscar", true, true, false)]);
        w.manager.run_due_updates(false);
        let r = w.manager.refresh(Some("oscar"));
        assert_eq!(r.len(), 1);
        assert_eq!(pulls(&w, "oscar"), 2);
    }

    #[test]
    fn one_server_down_does_not_stop_the_others() {
        let w = world(&[("oscar", true, true, true), ("server2", true, true, false)]);
        let results = w.manager.run_due_updates(false);
        assert_eq!(results.len(), 2);
        assert!(results.iter().find(|(id, _)| id == "oscar").unwrap().1.as_ref().unwrap_err().is_offline());
        assert!(results.iter().find(|(id, _)| id == "server2").unwrap().1.is_ok());
        assert_eq!(w.manager.health("oscar"), Health::Offline { network_down: false });
        assert_eq!(w.manager.health("server2"), Health::Online);
    }

    #[test]
    fn disabled_servers_and_servers_without_a_password_are_left_alone() {
        let w = world(&[("off", false, true, false), ("nopw", true, false, false)]);
        assert!(w.manager.run_due_updates(false).is_empty());
        assert!(w.calls["off"].lock().unwrap().is_empty());
        assert!(w.calls["nopw"].lock().unwrap().is_empty());
        assert_eq!(w.manager.health("nopw"), Health::NoPassword);
    }

    #[test]
    fn the_worker_updates_at_launch_and_refreshes_on_request() {
        let w = world(&[("oscar", true, true, false)]);
        let calls = w.calls["oscar"].clone();
        let worker = spawn_worker(Arc::new(w.manager), std::time::Duration::from_secs(3600));
        let first = worker.events.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(first.results.len(), 1, "the launch check updated the never-updated server");
        assert_eq!(first.status_lines.len(), 1);

        worker.requests.send(WorkerRequest::Refresh(None)).unwrap();
        let second = worker.events.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(second.results[0].1.is_ok());
        assert_eq!(calls.lock().unwrap().iter().filter(|e| *e == "search3").count(), 2);
        worker.requests.send(WorkerRequest::Stop).unwrap();
    }

    #[test]
    fn every_enabled_server_has_a_status_line() {
        let w = world(&[("oscar", true, true, false), ("server2", true, true, true)]);
        w.manager.run_due_updates(false);
        let lines = w.manager.status_lines();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("oscar: updated 0m ago"), "{lines:?}");
        assert_eq!(lines[1], "server2: not responding, never updated");
    }
}
