#![allow(dead_code)]
// Nothing calls into this module yet — the OS watcher (Task 3) and the
// write-site registration calls (Task 8) land in later phases. Allow
// dead-code at the module level so both the lib and bin builds stay
// warning-free in the meantime, matching the precedent in dedupe.rs.

//! Filesystem-watch support — pure classification logic and a self-write
//! suppression registry.
//!
//! This module deliberately knows nothing about `notify` or any other OS
//! watcher crate; it just turns a batch of candidate paths into
//! [`WatchAction`]s, and tracks paths Sparkamp itself just wrote so the
//! (later) watcher doesn't re-import files we wrote ourselves. Keeping the
//! classification pure means it can be unit-tested without touching a real
//! filesystem watcher or spinning up background threads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What the watcher should do with a path once classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchAction {
    Upsert(PathBuf),
    Remove(PathBuf),
}

/// Tracks paths Sparkamp itself just wrote (e.g. tag edits, cached artwork)
/// so a filesystem watcher doesn't treat our own writes as external changes.
/// Entries older than the configured window are pruned lazily on lookup.
pub struct SelfWriteGuard {
    window: Duration,
    entries: Mutex<HashMap<PathBuf, Instant>>,
}

impl SelfWriteGuard {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Record that Sparkamp just wrote `path`; suppress it until the window
    /// elapses.
    pub fn register(&self, path: &Path) {
        let mut entries = self.entries.lock().unwrap();
        entries.insert(path.to_path_buf(), Instant::now());
    }

    /// True if `path` was registered within the suppression window. Also
    /// prunes any entries that have expired, so the map doesn't grow
    /// unbounded over a long-running watch session.
    pub fn is_suppressed(&self, path: &Path) -> bool {
        let mut entries = self.entries.lock().unwrap();
        let now = Instant::now();
        entries.retain(|_, registered_at| now.duration_since(*registered_at) < self.window);
        entries.contains_key(path)
    }
}

/// Turn a batch of candidate filesystem paths into watch actions.
///
/// Pure function: no I/O beyond `Path::exists`, no global state touched
/// except through the passed-in `guard`. A path is dropped if it lives
/// under `cache_prefix` (Sparkamp's own cache/artwork directory) or if
/// `guard` says it's a suppressed self-write. Otherwise: a path that exists
/// on disk with an audio extension becomes `Upsert`; a path that no longer
/// exists but had an audio extension becomes `Remove`. Extension matching
/// is case-insensitive. Everything else (non-audio extensions, and audio
/// extension checks that fail because there's no extension) is dropped.
pub fn classify_paths(
    paths: &[PathBuf],
    audio_exts: &[&str],
    cache_prefix: &Path,
    guard: &SelfWriteGuard,
) -> Vec<WatchAction> {
    let has_audio_ext = |path: &Path| -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| audio_exts.iter().any(|a| a.eq_ignore_ascii_case(ext)))
    };

    paths
        .iter()
        .filter(|path| !path.starts_with(cache_prefix))
        .filter(|path| !guard.is_suppressed(path))
        .filter_map(|path| {
            if path.exists() {
                has_audio_ext(path).then(|| WatchAction::Upsert(path.clone()))
            } else {
                has_audio_ext(path).then(|| WatchAction::Remove(path.clone()))
            }
        })
        .collect()
}

/// Global self-write registry shared by write sites (tag edits, cached
/// artwork writes, etc.) and the filesystem watcher. Lazily initialised
/// with a 5-second suppression window.
static GUARD: OnceLock<SelfWriteGuard> = OnceLock::new();

fn guard() -> &'static SelfWriteGuard {
    GUARD.get_or_init(|| SelfWriteGuard::new(Duration::from_secs(5)))
}

/// Convenience for write sites: record that Sparkamp itself just wrote
/// `path`, so the watcher (once running) suppresses the resulting fs event.
pub fn register_self_write(path: &Path) {
    guard().register(path);
}

/// Convenience for the watcher (and its tests) to check whether a path is
/// currently suppressed as a self-write.
pub fn is_path_suppressed(path: &Path) -> bool {
    guard().is_suppressed(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn exts() -> Vec<&'static str> {
        vec!["mp3", "flac", "ogg"]
    }

    #[test]
    fn classify_existing_audio_is_upsert_missing_is_remove() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("a.mp3");
        std::fs::write(&present, b"x").unwrap();
        let missing = dir.path().join("gone.mp3");
        let guard = SelfWriteGuard::new(Duration::from_secs(5));
        let cache = Path::new("/nonexistent-cache");
        let actions = classify_paths(&[present.clone(), missing.clone()], &exts(), cache, &guard);
        assert!(actions.contains(&WatchAction::Upsert(present)));
        assert!(actions.contains(&WatchAction::Remove(missing)));
    }

    #[test]
    fn non_audio_extension_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("notes.txt");
        std::fs::write(&txt, b"x").unwrap();
        let guard = SelfWriteGuard::new(Duration::from_secs(5));
        let actions = classify_paths(&[txt], &exts(), Path::new("/no-cache"), &guard);
        assert!(actions.is_empty());
    }

    #[test]
    fn cache_prefix_paths_dropped() {
        let cache = PathBuf::from("/home/u/.cache/sparkamp");
        let inside = cache.join("deadbeef.jpg");
        let guard = SelfWriteGuard::new(Duration::from_secs(5));
        let actions = classify_paths(&[inside], &exts(), &cache, &guard);
        assert!(actions.is_empty());
    }

    #[test]
    fn suppressed_path_dropped_then_processed_after_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("s.mp3");
        std::fs::write(&f, b"x").unwrap();
        let guard = SelfWriteGuard::new(Duration::from_millis(50));
        guard.register(&f);
        assert!(classify_paths(&[f.clone()], &exts(), Path::new("/no-cache"), &guard).is_empty());
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(
            classify_paths(&[f.clone()], &exts(), Path::new("/no-cache"), &guard),
            vec![WatchAction::Upsert(f)]
        );
    }
}
