//! The playback cache: server songs downloaded whole before they play.
//!
//! Every frontend plays a server song from a local file. macOS has to,
//! because `AVAudioFile` only opens local files and `AVPlayer` would bypass
//! the equalizer. Linux and the TUI do the same so there is one code path,
//! so a dropped connection never cuts a song in half, and so the next songs
//! can be fetched ahead.
//!
//! Files live in the OS cache directory, never under `~/.config/sparkamp`,
//! named by a hash so no path or token ever appears in a file name. The
//! least recently played files go first when the cache grows past its cap,
//! except files the caller is still going to play.

use super::client::ServerClient;
use super::error::ServerError;
use super::transport::Transport;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Default cap on what the cache keeps after use: 128 MB, a few CD-quality
/// FLACs. The cache is for playing, not storing; offline listening is "Make
/// available offline", which puts the file in the library.
pub const DEFAULT_MAX_BYTES: u64 = 128 * 1024 * 1024;

/// The cap from the `cache_max_mb` setting.
pub fn max_bytes_from_mb(mb: u32) -> u64 {
    mb as u64 * 1024 * 1024
}

/// The playback cache in one directory.
pub struct PlaybackCache {
    root: PathBuf,
    max_bytes: u64,
}

impl PlaybackCache {
    /// A cache in `root`, created if missing.
    pub fn new(root: PathBuf, max_bytes: u64) -> Self {
        let _ = std::fs::create_dir_all(&root);
        PlaybackCache { root, max_bytes }
    }

    /// The cache in the OS cache directory: `~/.cache/sparkamp/server-cache`
    /// on Linux, `~/Library/Caches/sparkamp/server-cache` on macOS (inside
    /// the sandbox container when sandboxed).
    pub fn in_os_cache_dir(max_bytes: u64) -> Self {
        let root = crate::home::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sparkamp")
            .join("server-cache");
        Self::new(root, max_bytes)
    }

    /// Where the song at `path_key` on `server_id` is (or would be) cached.
    pub fn file_for(&self, server_id: &str, path_key: &str) -> PathBuf {
        // FNV-1a over server and path: stable across runs, and the name says
        // nothing about the song.
        let mut h: u64 = 0xcbf29ce484222325;
        for b in server_id.bytes().chain(std::iter::once(0)).chain(path_key.bytes()) {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        // Keep the extension: some decoders go by it.
        let ext: String = Path::new(path_key)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .filter(|e| !e.is_empty() && e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or_else(|| "bin".into());
        self.root.join(format!("{h:016x}.{ext}"))
    }

    /// The cached file for this song, if it is already here. Counts as a use
    /// for the least-recently-used order.
    pub fn cached(&self, server_id: &str, path_key: &str) -> Option<PathBuf> {
        let path = self.file_for(server_id, path_key);
        if !path.is_file() {
            return None;
        }
        if let Ok(f) = std::fs::File::options().write(true).open(&path) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
        Some(path)
    }

    /// The cached file for this song, downloading it first if needed. After
    /// a download the cache is trimmed to its cap, sparing `keep`.
    pub fn fetch<T: Transport>(
        &self,
        client: &ServerClient<T>,
        server_id: &str,
        path_key: &str,
        song_id: &str,
        keep: &HashSet<PathBuf>,
    ) -> Result<PathBuf, ServerError> {
        self.fetch_observed(client, server_id, path_key, song_id, keep, &|_, _| {})
    }

    /// [`Self::fetch`], telling `started` the stream URL and the `.part` file
    /// when a download begins (not when the song is already cached).
    pub fn fetch_observed<T: Transport>(
        &self,
        client: &ServerClient<T>,
        server_id: &str,
        path_key: &str,
        song_id: &str,
        keep: &HashSet<PathBuf>,
        started: &dyn Fn(&str, &Path),
    ) -> Result<PathBuf, ServerError> {
        if let Some(hit) = self.cached(server_id, path_key) {
            return Ok(hit);
        }
        let dest = self.file_for(server_id, path_key);
        client.download_original_observed(song_id, &dest, started)?;
        let mut keep = keep.clone();
        keep.insert(dest.clone());
        self.trim(&keep);
        Ok(dest)
    }

    /// Delete least recently used files until the cache fits its cap. Files
    /// in `keep` are never deleted. Returns the number of files deleted.
    pub fn trim(&self, keep: &HashSet<PathBuf>) -> usize {
        let mut files = self.files();
        let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
        files.sort_by_key(|(_, _, modified)| *modified);
        let mut deleted = 0;
        for (path, len, _) in files {
            if total <= self.max_bytes {
                break;
            }
            if keep.contains(&path) {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                total -= len;
                deleted += 1;
            }
        }
        deleted
    }

    /// Total bytes in the cache.
    pub fn size(&self) -> u64 {
        self.files().iter().map(|(_, len, _)| len).sum()
    }

    /// Finished cache files with their size and last use. Downloads still in
    /// progress (`.part`) are not cache files yet.
    fn files(&self) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
        let Ok(entries) = std::fs::read_dir(&self.root) else { return Vec::new() };
        entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_none_or(|x| x != "part"))
            .filter_map(|e| {
                let md = e.metadata().ok()?;
                let modified = md.modified().ok()?;
                md.is_file().then(|| (e.path(), md.len(), modified))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::servers::request::Credentials;
    use crate::servers::transport::{Download, HttpResponse};
    use std::sync::Mutex;

    /// Serves every song as `body`, counting downloads.
    struct Fake {
        body: Vec<u8>,
        offline: bool,
        downloads: Mutex<usize>,
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

        fn get_to_file(&self, _: &str, _: u64, dest: &Path) -> Result<Download, ServerError> {
            *self.downloads.lock().unwrap() += 1;
            std::fs::write(dest, &self.body).unwrap();
            Ok(Download {
                status: 200,
                content_type: Some("audio/mpeg".into()),
                bytes: self.body.len() as u64,
            })
        }
    }

    fn client(body: &[u8], offline: bool) -> ServerClient<Fake> {
        ServerClient::new(
            Some("http://oscar.local:4533".into()),
            None,
            Credentials::Password { username: "me".into(), password: "pw".into() },
            Fake { body: body.to_vec(), offline, downloads: Mutex::new(0) },
        )
    }

    fn downloads(c: &ServerClient<Fake>) -> usize {
        *c.transport().downloads.lock().unwrap()
    }

    #[test]
    fn a_cache_file_name_hides_the_path_and_keeps_the_extension() {
        let dir = tempfile::tempdir().unwrap();
        let cache = PlaybackCache::new(dir.path().to_path_buf(), DEFAULT_MAX_BYTES);
        let f = cache.file_for("oscar", "/music/Miles Davis/Kind of Blue/01 So What.flac");
        assert_eq!(f.parent().unwrap(), dir.path());
        assert_eq!(f.extension().unwrap(), "flac");
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!name.contains("Miles") && !name.contains(' '), "{name}");
        assert_ne!(f, cache.file_for("server2", "/music/Miles Davis/Kind of Blue/01 So What.flac"));
        assert_eq!(f, cache.file_for("oscar", "/music/Miles Davis/Kind of Blue/01 So What.flac"));
    }

    #[test]
    fn a_song_is_downloaded_once_and_then_played_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = PlaybackCache::new(dir.path().to_path_buf(), DEFAULT_MAX_BYTES);
        let c = client(b"audio", false);
        let keep = HashSet::new();
        let first = cache.fetch(&c, "oscar", "/music/a.mp3", "s1", &keep).unwrap();
        let second = cache.fetch(&c, "oscar", "/music/a.mp3", "s1", &keep).unwrap();
        assert_eq!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), b"audio");
        assert_eq!(downloads(&c), 1);
        assert_eq!(cache.cached("oscar", "/music/a.mp3"), Some(first));
    }

    #[test]
    fn an_unreachable_server_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let cache = PlaybackCache::new(dir.path().to_path_buf(), DEFAULT_MAX_BYTES);
        let c = client(b"audio", true);
        assert!(cache.fetch(&c, "oscar", "/music/a.mp3", "s1", &HashSet::new()).unwrap_err().is_offline());
        assert_eq!(cache.cached("oscar", "/music/a.mp3"), None);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_least_recently_played_go_first_but_never_a_kept_file() {
        let dir = tempfile::tempdir().unwrap();
        // Room for two 10-byte files.
        let cache = PlaybackCache::new(dir.path().to_path_buf(), 25);
        let c = client(b"0123456789", false);
        let none = HashSet::new();
        let a = cache.fetch(&c, "oscar", "/a.mp3", "a", &none).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let b = cache.fetch(&c, "oscar", "/b.mp3", "b", &none).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Playing `a` again makes `b` the least recently used.
        cache.cached("oscar", "/a.mp3").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let keep: HashSet<PathBuf> = [b.clone()].into_iter().collect();
        let c3 = cache.fetch(&c, "oscar", "/c.mp3", "c", &keep).unwrap();

        assert!(b.exists(), "kept for the queue");
        assert!(!a.exists(), "oldest not kept");
        assert!(c3.exists());
        assert_eq!(cache.size(), 20);
    }
}
