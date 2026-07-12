//! Persistent on-disk cache of audio file durations.
//!
//! Durations are stored as nanoseconds keyed by canonical file path in a TOML
//! file under the OS cache directory (`~/.cache/sparkamp/duration_cache.toml`).
//!
//! On each launch the cache is loaded before any probing begins, so files
//! already measured in a previous session are available instantly.  New results
//! are written on a periodic timer and always flushed on `Drop`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Serialised representation
// ---------------------------------------------------------------------------

/// The raw data structure written to disk.  Using a named sub-table keeps the
/// TOML file readable and easy to inspect manually.
#[derive(Serialize, Deserialize, Default)]
struct CacheData {
    /// Maps canonical absolute path → duration in nanoseconds.
    durations: HashMap<String, u64>,
}

// ---------------------------------------------------------------------------
// DurationCache
// ---------------------------------------------------------------------------

/// In-memory view of the duration cache with deferred writes.
///
/// Call [`save_if_dirty`] periodically (e.g. every 30 s from the UI tick loop)
/// and it will be saved automatically on `Drop` as well.
pub struct DurationCache {
    data: CacheData,
    path: PathBuf,
    /// Set whenever an entry is inserted; cleared on a successful write.
    pub dirty: bool,
}

impl DurationCache {
    /// Load the cache from the standard OS location, or return an empty cache
    /// on any I/O or parse error.
    ///
    /// On the first run after the GnomAmp → Sparkamp rename, migrates the
    /// existing cache from `~/.cache/gnomamp/` so probed durations are not lost.
    pub fn load() -> Self {
        let path = Self::cache_path();
        if !path.exists() {
            let old = dirs::cache_dir()
                .unwrap_or_default()
                .join("gnomamp")
                .join("duration_cache.toml");
            crate::config::migrate_legacy_file(&old, &path);
        }
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str::<CacheData>(&s).ok())
            .unwrap_or_default();
        DurationCache { data, path, dirty: false }
    }

    /// Look up the cached duration for a file.  `path` must be canonical.
    pub fn get(&self, path: &Path) -> Option<Duration> {
        let key = path.to_string_lossy();
        self.data.durations.get(key.as_ref()).copied().map(Duration::from_nanos)
    }

    /// Store a duration and mark the cache dirty.  `path` must be canonical.
    pub fn insert(&mut self, path: &Path, dur: Duration) {
        let key = path.to_string_lossy().into_owned();
        self.data.durations.insert(key, dur.as_nanos() as u64);
        self.dirty = true;
    }

    /// Write the cache to disk only if it has changed since the last save.
    /// Silently ignores all I/O errors (non-critical).
    pub fn save_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = toml::to_string(&self.data) {
            if std::fs::write(&self.path, s).is_ok() {
                self.dirty = false;
            }
        }
    }

    fn cache_path() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sparkamp")
            .join("duration_cache.toml")
    }
}

impl Drop for DurationCache {
    /// Flush any unsaved entries when the application exits.
    fn drop(&mut self) {
        self.save_if_dirty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_at(path: PathBuf) -> DurationCache {
        DurationCache {
            data: CacheData::default(),
            path,
            dirty: false,
        }
    }

    #[test]
    fn insert_then_get_round_trips_and_sets_dirty() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cache_at(dir.path().join("cache.toml"));
        let p = Path::new("/music/a.mp3");
        assert!(c.get(p).is_none());
        c.insert(p, Duration::from_secs(185));
        assert!(c.dirty);
        assert_eq!(c.get(p), Some(Duration::from_secs(185)));
        assert!(c.get(Path::new("/music/other.mp3")).is_none());
    }

    #[test]
    fn save_if_dirty_writes_once_and_survives_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub").join("cache.toml");
        let mut c = cache_at(file.clone());
        c.insert(Path::new("/music/a.mp3"), Duration::from_nanos(1_500_000_000));
        c.save_if_dirty();
        assert!(!c.dirty, "successful save clears the dirty flag");
        assert!(file.exists());

        // A clean cache doesn't rewrite: truncate, save again, still empty.
        std::fs::write(&file, "").unwrap();
        c.save_if_dirty();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "");

        // The written TOML parses back to the same entry.
        c.dirty = true;
        c.save_if_dirty();
        let reloaded: CacheData =
            toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(reloaded.durations.get("/music/a.mp3"), Some(&1_500_000_000));
    }
}
