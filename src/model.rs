//! Core data model: tracks and playlists.
//!
//! This module intentionally has no UI or audio dependencies — it only holds
//! data and logic that is shared between the TUI and the GTK4 GUI.  Both
//! frontends own a single [`Playlist`] instance and operate on it through the
//! methods defined here.

use anyhow::{Context, Result};
use id3::TagLike;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Audio file extension detection
// ---------------------------------------------------------------------------

/// All audio file extensions SparkAmp will recognise when scanning directories.
///
/// The list covers the formats most commonly encountered in personal music
/// libraries.  Matching is done case-insensitively so `.MP3`, `.Flac`, etc.
/// are all accepted.  GStreamer ultimately determines whether the file is
/// truly playable; this list is only used to filter out obvious non-audio
/// files (images, playlists, lyrics, etc.) during directory scans.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "opus", "wav", "aac", "m4a",
    "wma", "ape", "mpc", "tta", "wv", "aiff", "aif",
];

/// Return `true` if `path`'s extension (case-insensitive) is in
/// [`AUDIO_EXTENSIONS`].
///
/// Files with no extension or an unrecognised extension return `false`.
///
/// # Examples
/// ```ignore
/// assert!(is_audio_file(Path::new("song.MP3")));
/// assert!(is_audio_file(Path::new("album/track.flac")));
/// assert!(!is_audio_file(Path::new("cover.jpg")));
/// assert!(!is_audio_file(Path::new("README")));
/// ```
pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let lower = ext.to_lowercase();
            AUDIO_EXTENSIONS.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Track
// ---------------------------------------------------------------------------

/// A single audio file together with its metadata.
///
/// Metadata is read from ID3 tags when the track is added to the playlist.
/// If no tags are present the filename stem is used as the title and artist /
/// album are left empty.  The `duration` field is intentionally `None` at
/// construction time and may be filled in by the audio engine once the file
/// has been loaded for playback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    /// Canonicalised absolute path to the audio file.
    pub path: PathBuf,
    /// Track title from the ID3 `TIT2` tag, or the filename stem as fallback.
    pub title: String,
    /// Artist name from the ID3 `TPE1` tag, or empty if not available.
    pub artist: String,
    /// Album artist from the ID3 `TPE2` tag; used as fallback when `artist` is empty.
    #[serde(default)]
    pub album_artist: String,
    /// Album name from the ID3 `TALB` tag, or empty if not available.
    pub album: String,
    /// Populated lazily by the engine once the file is loaded for playback.
    pub duration: Option<Duration>,
    /// Set at runtime when the file cannot be loaded or played.
    /// Not persisted — always starts as `false` on a fresh session so the
    /// user can try a repaired or re-mounted file next launch.
    #[serde(skip)]
    pub broken: bool,
}

impl Track {
    /// Construct a `Track` from an arbitrary filesystem path.
    ///
    /// `path` is canonicalised (resolved to an absolute path with symlinks
    /// expanded) so that the resulting `Track` is unambiguous regardless of
    /// the current working directory.  Returns an error if the path does not
    /// exist or the caller lacks read permission.
    ///
    /// ID3 tag parsing failures are non-fatal: the track is still created with
    /// the filename as its title.
    pub fn from_path(path: &Path) -> Result<Self> {
        // canonicalize() both validates existence and gives us a stable path.
        let path = path
            .canonicalize()
            .with_context(|| format!("Cannot resolve path: {}", path.display()))?;

        let (title, artist, album_artist, album) = match id3::Tag::read_from_path(&path) {
            Ok(tag) => {
                let title        = tag.title().unwrap_or("").to_string();
                let artist       = tag.artist().unwrap_or("").to_string();
                let album_artist = tag.album_artist().unwrap_or("").to_string();
                let album        = tag.album().unwrap_or("").to_string();
                // Fall back to filename stem only if the title tag is also empty.
                let title = if title.is_empty() {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Unknown")
                        .to_string()
                } else {
                    title
                };
                (title, artist, album_artist, album)
            }
            Err(_) => {
                // No readable ID3 tag — use the filename stem as the display name.
                let title = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Unknown")
                    .to_string();
                (title, String::new(), String::new(), String::new())
            }
        };

        Ok(Track { path, title, artist, album_artist, album, duration: None, broken: false })
    }

    /// Return a single human-readable label for the track.
    ///
    /// Format is `"Artist - Title"` using `artist` (TPE1), falling back to
    /// `album_artist` (TPE2) if `artist` is empty.  When neither is present
    /// (no ID3 tags or both fields empty) returns just the title, which for
    /// untagged files is already the filename stem.
    pub fn display_name(&self) -> String {
        let effective = if !self.artist.is_empty() {
            self.artist.as_str()
        } else if !self.album_artist.is_empty() {
            self.album_artist.as_str()
        } else {
            ""
        };
        if effective.is_empty() {
            self.title.clone()
        } else {
            format!("{} - {}", effective, self.title)
        }
    }

    /// Build a GStreamer-compatible `file://` URI for this track.
    ///
    /// Percent-encodes the characters most likely to appear in real-world
    /// filenames that would otherwise be misinterpreted by a URI parser:
    /// `%` (must be first to prevent double-encoding), space, `#`, and `?`.
    /// The path is always absolute (guaranteed by `canonicalize()` in
    /// `from_path`), so no base-URI resolution is needed.
    pub fn uri(&self) -> String {
        let path_str = self.path.display().to_string();
        // Encode in this specific order: % must come first so that literal
        // percent signs in filenames are encoded before we add any new ones.
        let encoded = path_str
            .replace('%', "%25")
            .replace(' ', "%20")
            .replace('#', "%23")
            .replace('?', "%3F");
        format!("file://{}", encoded)
    }
}

// ---------------------------------------------------------------------------
// Playlist
// ---------------------------------------------------------------------------

/// An ordered list of tracks with a single "current" position.
///
/// All navigation methods (`next`, `previous`, `jump_to`) update
/// `current_index` and return a reference to the new current track so that
/// the caller can immediately start playback.  Editing methods (`add`,
/// `remove`, `move_track`) keep `current_index` pointing at the same
/// *logical* track even when its position in the list changes.
/// Format an optional duration as `"M:SS"` for display in both UIs.
///
/// Returns `"-:--"` when the duration is not yet known so callers never need
/// to special-case `None`.
pub fn fmt_duration(dur: Option<Duration>) -> String {
    match dur {
        Some(d) => {
            let s = d.as_secs();
            format!("{}:{:02}", s / 60, s % 60)
        }
        None => "-:--".to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Playlist {
    /// All tracks in display order.
    pub tracks: Vec<Track>,
    /// Zero-based index of the track that is currently selected (and usually
    /// playing).  Always within `[0, tracks.len())` when `tracks` is
    /// non-empty; fixed at `0` when the playlist is empty.
    pub current_index: usize,
}

impl Playlist {
    /// Create an empty playlist with `current_index` at 0.
    pub fn new() -> Self {
        Playlist::default()
    }

    /// Append a track to the end of the playlist.
    pub fn add(&mut self, track: Track) {
        self.tracks.push(track);
    }

    /// Return `true` if the playlist contains no tracks.
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Return the number of tracks in the playlist.
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// Return a reference to the track at `current_index`, or `None` if the
    /// playlist is empty.
    pub fn current(&self) -> Option<&Track> {
        self.tracks.get(self.current_index)
    }

    /// Advance `current_index` by one and return the new current track.
    ///
    /// Returns `None` without changing the index if we are already at the
    /// last track (no wrap-around).
    pub fn next(&mut self) -> Option<&Track> {
        if self.current_index + 1 < self.tracks.len() {
            self.current_index += 1;
            self.tracks.get(self.current_index)
        } else {
            None
        }
    }

    /// Step `current_index` back by one (floor at 0) and return the new
    /// current track.  Always succeeds even when already at track 0.
    pub fn previous(&mut self) -> Option<&Track> {
        self.current_index = self.current_index.saturating_sub(1);
        self.tracks.get(self.current_index)
    }

    /// Set `current_index` to `index` and return the track there.
    ///
    /// Returns `None` without changing the index if `index` is out of bounds.
    pub fn jump_to(&mut self, index: usize) -> Option<&Track> {
        if index < self.tracks.len() {
            self.current_index = index;
            self.tracks.get(index)
        } else {
            None
        }
    }

    /// Remove the track at `index` (0-based).
    ///
    /// `current_index` is adjusted so that it continues to point at the same
    /// *logical* track after the removal:
    /// - If the removed track was *before* `current_index`, the index
    ///   decrements by one.
    /// - If the removed track *was* the current track and was the last one in
    ///   the list, the index clamps to the new last position.
    /// - Otherwise the index is unchanged (the track that follows the gap
    ///   slides into the current slot).
    ///
    /// Returns the removed `Track`, or `None` if `index` is out of bounds.
    pub fn remove(&mut self, index: usize) -> Option<Track> {
        if index >= self.tracks.len() {
            return None;
        }
        let track = self.tracks.remove(index);
        if self.tracks.is_empty() {
            self.current_index = 0;
        } else if index < self.current_index {
            self.current_index -= 1;
        } else if self.current_index >= self.tracks.len() {
            self.current_index = self.tracks.len() - 1;
        }
        Some(track)
    }

    /// Move the track at `from` (0-based) to `to` (0-based), shifting all
    /// tracks between the two positions by one slot.
    ///
    /// `current_index` follows whichever track was current before the move,
    /// so playback is unaffected.  Returns `false` if either index is out of
    /// bounds (the playlist is unchanged in that case).
    pub fn move_track(&mut self, from: usize, to: usize) -> bool {
        if from >= self.tracks.len() || to >= self.tracks.len() {
            return false;
        }
        if from == to {
            return true;
        }
        let current_was = self.current_index;
        let track = self.tracks.remove(from);
        self.tracks.insert(to, track);

        // Recalculate where current_index ended up after the two-step
        // remove-then-insert operation.
        self.current_index = if current_was == from {
            // The track that was playing is the one we just moved.
            to
        } else {
            // Another track was playing.  Figure out where it went.
            let after_remove = if current_was > from { current_was - 1 } else { current_was };
            if after_remove >= to { after_remove + 1 } else { after_remove }
        };
        true
    }

    /// Return the indices of all tracks whose `title`, `artist`, or `album`
    /// contain `query` (case-insensitive substring match).
    ///
    /// An empty `query` returns all indices.  Results are in playlist order.
    pub fn search_indices(&self, query: &str) -> Vec<usize> {
        let q = query.to_lowercase();
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.title.to_lowercase().contains(&q)
                    || t.artist.to_lowercase().contains(&q)
                    || t.album.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Return the path to the last-playlist file:
    /// `$XDG_DATA_HOME/sparkamp/last_playlist.toml`
    /// (defaults to `~/.local/share/sparkamp/last_playlist.toml` on Linux).
    pub fn data_path() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sparkamp")
            .join("last_playlist.toml")
    }

    /// Serialize the playlist to TOML and write it to [`Self::data_path()`].
    ///
    /// Called on application exit so the playlist can be restored on the next
    /// launch.  Creates the parent directory if it does not exist.
    pub fn save_last(&self) -> Result<()> {
        let path = Self::data_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Load the last-saved playlist from [`Self::data_path()`].
    ///
    /// Returns an error if the file does not exist or cannot be parsed.
    /// Callers should treat an error as "no saved playlist" and start empty.
    ///
    /// On the first run after the GnomAmp → SparkAmp rename, migrates the
    /// saved playlist from the old `gnomamp` data directory automatically.
    pub fn load_last() -> Result<Self> {
        let path = Self::data_path();
        if !path.exists() {
            let old = dirs::data_dir()
                .unwrap_or_default()
                .join("gnomamp")
                .join("last_playlist.toml");
            crate::config::migrate_legacy_file(&old, &path);
        }
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    // -----------------------------------------------------------------------
    // Directory scanning
    // -----------------------------------------------------------------------

    /// Recursively collect all audio files under `dir`, returning them as a
    /// sorted `Vec<PathBuf>`.
    ///
    /// The traversal is depth-first, with entries within each directory sorted
    /// alphabetically (by `OsStr` comparison) before descending.  This gives a
    /// deterministic, human-friendly ordering that mirrors how a file manager
    /// would display the tree.
    ///
    /// Directories that cannot be read (permission denied, broken symlinks,
    /// etc.) are silently skipped so that one inaccessible folder does not
    /// abort the entire scan.
    ///
    /// Only paths whose extension matches [`AUDIO_EXTENSIONS`]
    /// (case-insensitively) are included.
    pub fn collect_audio_files(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        Self::collect_audio_files_inner(dir, &mut files);
        files
    }

    /// Internal recursive helper for [`collect_audio_files`].
    ///
    /// Populates `files` with audio file paths found under `dir`.  Entries in
    /// each directory are sorted alphabetically before recursion so that the
    /// final order is stable across runs and platforms.
    fn collect_audio_files_inner(dir: &Path, files: &mut Vec<PathBuf>) {
        // Attempt to read the directory; silently skip on any error (e.g.
        // permission denied) to keep the scan robust.
        let read_dir = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return,
        };

        // Collect all valid entries first so we can sort them.
        let mut entries: Vec<PathBuf> = read_dir
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();

        // Sort alphabetically by the full path so sub-directories and files
        // are ordered consistently regardless of filesystem traversal order.
        entries.sort_unstable_by(|a, b| a.file_name().cmp(&b.file_name()));

        for path in entries {
            if path.is_dir() {
                // Recurse depth-first into sub-directories.
                Self::collect_audio_files_inner(&path, files);
            } else if is_audio_file(&path) {
                // Only include files whose extension is a known audio type.
                files.push(path);
            }
        }
    }

    /// Add audio content from each path in `paths` to the playlist.
    ///
    /// For each path:
    /// - If it is a **directory**, the directory is scanned recursively using
    ///   [`collect_audio_files`] and every discovered audio file is added.
    /// - If it is a **file**, it is added directly as a single track.
    ///
    /// Paths that cannot be resolved (file not found, no read permission,
    /// or `Track::from_path` fails for any other reason) produce an error
    /// message in the returned `Vec<String>` rather than aborting the whole
    /// operation — the caller decides whether to surface these to the user.
    ///
    /// # Returns
    /// `(added_count, error_messages)` where:
    /// - `added_count` is the total number of tracks successfully added.
    /// - `error_messages` contains one human-readable string per failed path.
    pub fn add_paths(&mut self, paths: &[&Path]) -> (usize, Vec<String>) {
        let mut added = 0usize;
        let mut errors: Vec<String> = Vec::new();

        for &path in paths {
            if path.is_dir() {
                // Recursively collect all audio files under this directory.
                let audio_files = Self::collect_audio_files(path);
                for audio_path in audio_files {
                    match Track::from_path(&audio_path) {
                        Ok(track) => {
                            self.add(track);
                            added += 1;
                        }
                        Err(e) => {
                            // Record the error but continue scanning the rest.
                            errors.push(format!(
                                "Cannot load '{}': {}",
                                audio_path.display(),
                                e
                            ));
                        }
                    }
                }
            } else {
                // Treat as a single audio file.
                match Track::from_path(path) {
                    Ok(track) => {
                        self.add(track);
                        added += 1;
                    }
                    Err(e) => {
                        errors.push(format!("Cannot add '{}': {}", path.display(), e));
                    }
                }
            }
        }

        (added, errors)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_track(title: &str) -> Track {
        Track {
            path: PathBuf::from(format!("/fake/{}.mp3", title)),
            title: title.to_string(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            duration: None,
            broken: false,
        }
    }

    fn playlist_of(titles: &[&str]) -> Playlist {
        let mut p = Playlist::new();
        for t in titles {
            p.add(make_track(t));
        }
        p
    }

    // -----------------------------------------------------------------------
    // is_audio_file()
    // -----------------------------------------------------------------------

    #[test]
    fn is_audio_file_recognises_mp3() {
        assert!(is_audio_file(Path::new("song.mp3")));
    }

    #[test]
    fn is_audio_file_is_case_insensitive() {
        assert!(is_audio_file(Path::new("TRACK.MP3")));
        assert!(is_audio_file(Path::new("album.Flac")));
    }

    #[test]
    fn is_audio_file_rejects_non_audio() {
        assert!(!is_audio_file(Path::new("cover.jpg")));
        assert!(!is_audio_file(Path::new("README.md")));
        assert!(!is_audio_file(Path::new("noextension")));
    }

    #[test]
    fn is_audio_file_recognises_all_listed_extensions() {
        for ext in AUDIO_EXTENSIONS {
            let p = PathBuf::from(format!("track.{}", ext));
            assert!(
                is_audio_file(&p),
                "extension '{}' should be recognised as audio",
                ext
            );
        }
    }

    // -----------------------------------------------------------------------
    // collect_audio_files()
    // -----------------------------------------------------------------------

    #[test]
    fn collect_audio_files_on_nonexistent_dir_returns_empty() {
        let files = Playlist::collect_audio_files(Path::new("/nonexistent_dir_that_does_not_exist"));
        assert!(files.is_empty());
    }

    // -----------------------------------------------------------------------
    // add_paths()
    // -----------------------------------------------------------------------

    #[test]
    fn add_paths_on_nonexistent_file_returns_error_and_zero_added() {
        let mut p = Playlist::new();
        let (added, errors) = p.add_paths(&[Path::new("/nonexistent/file.mp3")]);
        assert_eq!(added, 0);
        assert!(!errors.is_empty());
    }

    #[test]
    fn add_paths_on_nonexistent_dir_returns_zero_added_and_error() {
        let mut p = Playlist::new();
        // A non-existent path — is_dir() returns false, so add_paths treats it
        // as a single file.  Track::from_path fails, producing one error.
        let path = Path::new("/nonexistent_scan_dir_abc");
        let (added, errors) = p.add_paths(&[path]);
        assert_eq!(added, 0);
        assert_eq!(errors.len(), 1);
    }

    // -----------------------------------------------------------------------
    // remove()
    // -----------------------------------------------------------------------

    #[test]
    fn remove_returns_correct_track() {
        let mut p = playlist_of(&["A", "B", "C"]);
        let removed = p.remove(1).unwrap();
        assert_eq!(removed.title, "B");
        assert_eq!(p.tracks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["A", "C"]);
    }

    #[test]
    fn remove_out_of_bounds_returns_none() {
        let mut p = playlist_of(&["A", "B"]);
        assert!(p.remove(5).is_none());
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn remove_before_current_decrements_current_index() {
        let mut p = playlist_of(&["A", "B", "C", "D"]);
        p.current_index = 2; // C
        p.remove(1); // remove B
        assert_eq!(p.current_index, 1); // C is now at 1
        assert_eq!(p.tracks[p.current_index].title, "C");
    }

    #[test]
    fn remove_after_current_leaves_current_index_unchanged() {
        let mut p = playlist_of(&["A", "B", "C", "D"]);
        p.current_index = 1; // B
        p.remove(3); // remove D
        assert_eq!(p.current_index, 1);
        assert_eq!(p.tracks[p.current_index].title, "B");
    }

    #[test]
    fn remove_current_track_clamps_to_last() {
        let mut p = playlist_of(&["A", "B", "C"]);
        p.current_index = 2; // C (last)
        p.remove(2);
        assert_eq!(p.current_index, 1); // clamped to new last
        assert_eq!(p.tracks[p.current_index].title, "B");
    }

    #[test]
    fn remove_current_track_advances_to_next() {
        let mut p = playlist_of(&["A", "B", "C"]);
        p.current_index = 1; // B
        p.remove(1);
        // index stays 1, which now points at C
        assert_eq!(p.current_index, 1);
        assert_eq!(p.tracks[p.current_index].title, "C");
    }

    #[test]
    fn remove_last_remaining_track_resets_index() {
        let mut p = playlist_of(&["A"]);
        p.remove(0);
        assert!(p.is_empty());
        assert_eq!(p.current_index, 0);
    }

    // -----------------------------------------------------------------------
    // move_track()
    // -----------------------------------------------------------------------

    #[test]
    fn move_track_forward() {
        let mut p = playlist_of(&["A", "B", "C", "D", "E"]);
        assert!(p.move_track(1, 3)); // move B to position 3
        assert_eq!(
            p.tracks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
            ["A", "C", "D", "B", "E"]
        );
    }

    #[test]
    fn move_track_backward() {
        let mut p = playlist_of(&["A", "B", "C", "D", "E"]);
        assert!(p.move_track(3, 1)); // move D to position 1
        assert_eq!(
            p.tracks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
            ["A", "D", "B", "C", "E"]
        );
    }

    #[test]
    fn move_track_same_position_is_noop() {
        let mut p = playlist_of(&["A", "B", "C"]);
        assert!(p.move_track(1, 1));
        assert_eq!(
            p.tracks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
            ["A", "B", "C"]
        );
    }

    #[test]
    fn move_track_out_of_bounds_returns_false() {
        let mut p = playlist_of(&["A", "B"]);
        assert!(!p.move_track(0, 5));
        assert!(!p.move_track(5, 0));
    }

    #[test]
    fn move_track_current_follows_moved_track_forward() {
        let mut p = playlist_of(&["A", "B", "C", "D", "E"]);
        p.current_index = 1; // B
        p.move_track(1, 3);
        assert_eq!(p.current_index, 3); // B moved to 3
        assert_eq!(p.tracks[p.current_index].title, "B");
    }

    #[test]
    fn move_track_current_follows_moved_track_backward() {
        let mut p = playlist_of(&["A", "B", "C", "D", "E"]);
        p.current_index = 3; // D
        p.move_track(3, 1);
        assert_eq!(p.current_index, 1); // D moved to 1
        assert_eq!(p.tracks[p.current_index].title, "D");
    }

    #[test]
    fn move_track_current_adjusts_when_displaced_forward() {
        // Moving a track from before current to after current shifts current back
        let mut p = playlist_of(&["A", "B", "C", "D", "E"]);
        p.current_index = 2; // C
        p.move_track(1, 3); // move B (before C) to after C → [A,C,D,B,E]
        assert_eq!(p.current_index, 1); // C shifted from 2 to 1
        assert_eq!(p.tracks[p.current_index].title, "C");
    }

    #[test]
    fn move_track_current_adjusts_when_displaced_backward() {
        // Moving a track from after current to before current shifts current forward
        let mut p = playlist_of(&["A", "B", "C", "D", "E"]);
        p.current_index = 2; // C
        p.move_track(3, 1); // move D (after C) to before C → [A,D,B,C,E]
        assert_eq!(p.current_index, 3); // C shifted from 2 to 3
        assert_eq!(p.tracks[p.current_index].title, "C");
    }
}
