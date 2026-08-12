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
    /// A playlist file (`.m3u8` / `.m3u`) appeared or changed on disk.
    ///
    /// Separate from [`WatchAction::Upsert`] because a playlist is not a
    /// track: it registers in the `playlists` table, not `tracks`, and the
    /// frontends refresh a different part of their UI for it. Without this
    /// the watcher discarded playlist files entirely, so one created outside
    /// Sparkamp stayed invisible in the sidebar until the next restart even
    /// though it was already in the database.
    PlaylistUpsert(PathBuf),
}

/// Playlist file extensions the watcher reacts to. Matches the pair the
/// playlist loader already accepts (`.m3u8` preferred, `.m3u` legacy).
pub const PLAYLIST_EXTENSIONS: &[&str] = &["m3u8", "m3u"];

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
            let is_playlist = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| {
                    PLAYLIST_EXTENSIONS.iter().any(|p| p.eq_ignore_ascii_case(ext))
                });
            if path.exists() {
                if is_playlist {
                    return Some(WatchAction::PlaylistUpsert(path.clone()));
                }
                has_audio_ext(path).then(|| WatchAction::Upsert(path.clone()))
            } else {
                // A deleted playlist is deliberately not actioned: playlist
                // rows are removed through the library's own delete path,
                // which also honours the Deletion Rule.
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

/// Accessor for the same global self-write registry used by
/// [`register_self_write`] / [`is_path_suppressed`], for use by the live
/// filesystem watcher's classifier calls.
pub fn global_guard() -> &'static SelfWriteGuard {
    guard()
}

/// A running debounced OS filesystem watcher over one or more folders.
///
/// Holds the underlying [`notify_debouncer_mini::Debouncer`] alive for as
/// long as this value lives; dropping it (or calling [`FolderWatcher::stop`])
/// tears down the debouncer's internal thread and stops watching.
pub struct FolderWatcher {
    _debouncer: notify_debouncer_mini::Debouncer<notify_debouncer_mini::notify::RecommendedWatcher>,
}

impl FolderWatcher {
    /// Start watching `folders` (each a directory plus whether to watch it
    /// recursively) for changes to files with one of `audio_exts`. Debounces
    /// OS events over a 2-second window, classifies the resulting paths via
    /// [`classify_paths`] (suppressing Sparkamp's own recent writes via the
    /// global [`SelfWriteGuard`]), and sends a [`WatchAction`] per surviving
    /// path on the returned channel.
    ///
    /// Returns `Err` if the underlying watcher fails to initialise or fails
    /// to register any of the requested folders (e.g. the inotify
    /// `max_user_watches` limit is exhausted) — callers should treat this as
    /// a signal to fall back to manual rescans rather than panicking.
    pub fn start(
        folders: Vec<(PathBuf, bool)>,
        audio_exts: Vec<String>,
        cache_prefix: PathBuf,
    ) -> std::io::Result<(FolderWatcher, std::sync::mpsc::Receiver<WatchAction>)> {
        use notify_debouncer_mini::notify::RecursiveMode;
        use notify_debouncer_mini::{new_debouncer, DebounceEventResult};

        let (tx, rx) = std::sync::mpsc::channel::<WatchAction>();

        let mut debouncer = new_debouncer(Duration::from_secs(2), move |result: DebounceEventResult| {
            match result {
                Ok(events) => {
                    let paths: Vec<PathBuf> = events.into_iter().map(|e| e.path).collect();
                    let ext_refs: Vec<&str> = audio_exts.iter().map(|s| s.as_str()).collect();
                    let actions = classify_paths(&paths, &ext_refs, &cache_prefix, global_guard());
                    for action in actions {
                        let _ = tx.send(action);
                    }
                }
                Err(e) => {
                    eprintln!("[sparkamp_watch] debouncer error: {e:?}");
                }
            }
        })
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        for (path, recurse) in folders {
            let mode = if recurse {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            debouncer
                .watcher()
                .watch(&path, mode)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        }

        Ok((FolderWatcher { _debouncer: debouncer }, rx))
    }

    /// Stop watching. Consumes `self`; dropping the held debouncer stops its
    /// internal thread and ends all active watches.
    pub fn stop(self) {
        drop(self._debouncer);
    }
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

    /// A playlist file created outside Sparkamp must reach the library.
    ///
    /// It is not an audio file, so before this it was discarded here and the
    /// sidebar's Playlists sub-rows stayed stale until the next restart —
    /// even though the Send-to menu, which re-reads the database on every
    /// open, already listed it.
    #[test]
    fn playlist_files_are_classified_for_upsert() {
        let dir = tempfile::tempdir().unwrap();
        let guard = SelfWriteGuard::new(Duration::from_secs(5));
        let cache = Path::new("/no-cache");

        for name in ["new.m3u8", "legacy.m3u", "SHOUTY.M3U8"] {
            let pl = dir.path().join(name);
            std::fs::write(&pl, b"#EXTM3U\n").unwrap();
            let actions = classify_paths(&[pl.clone()], &exts(), cache, &guard);
            assert_eq!(
                actions,
                vec![WatchAction::PlaylistUpsert(pl)],
                "{name} should classify as a playlist upsert"
            );
        }

        // A deleted playlist is not actioned: playlist rows are removed
        // through the library's own delete path, which honours the Deletion
        // Rule. Classifying it here would route a delete around that.
        let gone = dir.path().join("gone.m3u8");
        assert!(classify_paths(&[gone], &exts(), cache, &guard).is_empty());
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

    #[test]
    fn watcher_emits_upsert_on_new_file() {
        use std::sync::mpsc::RecvTimeoutError;
        let dir = tempfile::tempdir().unwrap();
        let (watcher, rx) = FolderWatcher::start(
            vec![(dir.path().to_path_buf(), true)],
            vec!["mp3".into()],
            std::path::PathBuf::from("/no-cache"),
        )
        .expect("watcher start");
        std::thread::sleep(std::time::Duration::from_millis(300));
        std::fs::write(dir.path().join("new.mp3"), b"x").unwrap();
        let mut got = false;
        loop {
            match rx.recv_timeout(std::time::Duration::from_secs(8)) {
                Ok(WatchAction::Upsert(p)) if p.ends_with("new.mp3") => {
                    got = true;
                    break;
                }
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => break,
                Err(_) => break,
            }
        }
        watcher.stop();
        assert!(got, "expected an Upsert for the new file");
    }
}
