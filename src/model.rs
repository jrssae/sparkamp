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

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTagKey, Value};
use symphonia::core::probe::Hint;

// ---------------------------------------------------------------------------
// String sanitization
// ---------------------------------------------------------------------------

/// Remove NUL bytes from a string.
///
/// ID3 tags can contain malformed data with embedded NUL bytes.  These cause
/// crashes when passed to GTK APIs which use C-style NUL-terminated strings.
/// This function strips any NUL bytes so the string is safe for UI display.
fn sanitize(s: &str) -> String {
    // First, remove any actual NUL bytes
    let result = if s.contains('\0') {
        s.replace('\0', "")
    } else {
        s.to_owned()
    };
    // Also remove the TOML escape sequence \u0000 which becomes NUL on deserialization
    // Check if the string contains literal "\u0000" as text
    if result.contains("\\u0000") {
        result.replace("\\u0000", "")
    } else {
        result
    }
}

// ---------------------------------------------------------------------------
// Audio file extension detection
// ---------------------------------------------------------------------------

/// All audio file extensions Sparkamp will recognise when scanning directories.
///
/// The list covers the formats most commonly encountered in personal music
/// libraries.  Matching is done case-insensitively so `.MP3`, `.Flac`, etc.
/// are all accepted.  GStreamer ultimately determines whether the file is
/// truly playable; this list is only used to filter out obvious non-audio
/// files (images, playlists, lyrics, etc.) during directory scans.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "opus", "wav", "aac", "m4a", "wma", "ape", "mpc", "tta", "wv", "aiff",
    "aif",
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
#[allow(dead_code)]
pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let lower = ext.to_lowercase();
            AUDIO_EXTENSIONS.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

/// Like [`is_audio_file`] but also accepts any extension in `extra_extensions`.
///
/// `extra_extensions` contains lower-case extension strings without the
/// leading dot (e.g. `"xyz"`).  This is used at runtime to include extensions
/// registered by loaded filetype plugins without modifying the static
/// [`AUDIO_EXTENSIONS`] slice.
#[allow(dead_code)]
pub fn is_audio_file_extended(path: &Path, extra_extensions: &[String]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let lower = ext.to_lowercase();
            AUDIO_EXTENSIONS.contains(&lower.as_str())
                || extra_extensions.iter().any(|e| e == &lower)
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Symphonia metadata fallback
// ---------------------------------------------------------------------------

/// Try to read title, artist, album-artist, and album from a file using
/// Symphonia's generic tag reader.
///
/// This succeeds for formats that don't use ID3 tags but are supported by
/// Symphonia — in particular OGG/Vorbis (Vorbis Comments), FLAC, and Opus.
/// Returns `None` when the file cannot be opened, the format is unrecognised,
/// or no relevant tags are present.
pub fn read_symphonia_metadata(path: &Path) -> Option<(String, String, String, String)> {
    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;

    // Symphonia can surface tags from two places:
    // 1. The outer container metadata log (e.g. ID3 tags in MP3 streams).
    // 2. The format reader's internal metadata (Vorbis Comments, FLAC tags).
    // We try both and merge, giving the format-reader priority.
    let mut title = String::new();
    let mut artist = String::new();
    let mut album_artist = String::new();
    let mut album = String::new();

    let apply_tags = |tags: &[symphonia::core::meta::Tag],
                      title: &mut String,
                      artist: &mut String,
                      album_artist: &mut String,
                      album: &mut String| {
        for tag in tags {
            let text = match &tag.value {
                Value::String(s) => s.as_str(),
                _ => continue,
            };
            // Sanitize to remove NUL bytes that can crash GTK.
            let safe_text = sanitize(text);
            match tag.std_key {
                Some(StandardTagKey::TrackTitle) => *title = safe_text,
                Some(StandardTagKey::Artist) => *artist = safe_text,
                Some(StandardTagKey::AlbumArtist) => *album_artist = safe_text,
                Some(StandardTagKey::Album) => *album = safe_text,
                _ => {}
            }
        }
    };

    // Pass 1: format-reader metadata (Vorbis Comments, FLAC tags, etc.).
    if let Some(rev) = probed.format.metadata().current() {
        apply_tags(
            rev.tags(),
            &mut title,
            &mut artist,
            &mut album_artist,
            &mut album,
        );
    }

    if title.is_empty() {
        None
    } else {
        Some((title, artist, album_artist, album))
    }
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
    /// Construct a `Track` from a path without reading metadata (fast).
    ///
    /// Only sets the path and title from the filename. Metadata (artist, album, etc.)
    /// must be read separately via `from_path()`. Use this for fast UI population
    /// when adding files to a playlist.
    pub fn from_path_fast(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .with_context(|| format!("Cannot resolve path: {}", path.display()))?;
        let title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string();
        Ok(Track {
            path,
            title,
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            duration: None,
            broken: false,
        })
    }

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

        // Strategy: try ID3 tags first (fast, works for MP3).  Fall back to
        // Symphonia's generic reader for formats that use Vorbis Comments or
        // other non-ID3 tag containers (OGG/Vorbis, FLAC, Opus).  If neither
        // succeeds, use the filename stem as a last resort.
        let (title, artist, album_artist, album) = match id3::Tag::read_from_path(&path) {
            Ok(tag) => {
                let title = sanitize(tag.title().unwrap_or(""));
                let artist = sanitize(tag.artist().unwrap_or(""));
                let album_artist = sanitize(tag.album_artist().unwrap_or(""));
                let album = sanitize(tag.album().unwrap_or(""));
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
                // No readable ID3 tag — try Symphonia for Vorbis Comments,
                // FLAC tags, etc.  This handles OGG/Vorbis, FLAC, and Opus
                // files which store metadata in format-specific tag blocks.
                if let Some((t, ar, aa, al)) = read_symphonia_metadata(&path) {
                    let title = if t.is_empty() {
                        path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("Unknown")
                            .to_string()
                    } else {
                        t
                    };
                    (title, ar, aa, al)
                } else {
                    // No metadata at all — use the filename stem.
                    let title = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Unknown")
                        .to_string();
                    (title, String::new(), String::new(), String::new())
                }
            }
        };

        Ok(Track {
            path,
            title,
            artist,
            album_artist,
            album,
            duration: None,
            broken: false,
        })
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

/// Create a Track from a media library LibTrack, copying the duration directly
/// without re-probing the file. This is much faster when adding tracks from ML.
impl From<&crate::media_library::LibTrack> for Track {
    fn from(lib: &crate::media_library::LibTrack) -> Self {
        Track {
            path: PathBuf::from(&lib.path),
            title: lib.title.clone().unwrap_or_else(|| lib.filename.clone()),
            artist: lib.artist.clone().unwrap_or_default(),
            album_artist: String::new(),
            album: lib.album.clone().unwrap_or_default(),
            duration: lib.length_secs.map(Duration::from_secs_f64),
            broken: false,
        }
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
    #[allow(dead_code)]
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
            let after_remove = if current_was > from {
                current_was - 1
            } else {
                current_was
            };
            if after_remove >= to {
                after_remove + 1
            } else {
                after_remove
            }
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
        // Sanitize all track metadata before saving to prevent corrupted data
        let mut clean_self = self.clone();
        for track in &mut clean_self.tracks {
            track.title = sanitize(&track.title);
            track.artist = sanitize(&track.artist);
            track.album_artist = sanitize(&track.album_artist);
            track.album = sanitize(&track.album);
        }
        std::fs::write(&path, toml::to_string_pretty(&clean_self)?)?;
        Ok(())
    }

    /// Load the last-saved playlist from [`Self::data_path()`].
    ///
    /// Returns an error if the file does not exist or cannot be parsed.
    /// Callers should treat an error as "no saved playlist" and start empty.
    ///
    /// On the first run after the GnomAmp → Sparkamp rename, migrates the
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
        let mut playlist: Self = toml::from_str(&content)?;
        // Sanitize all track metadata to remove NUL bytes that may have been
        // stored by older versions or corrupted ID3 tags.
        for track in &mut playlist.tracks {
            track.title = sanitize(&track.title);
            track.artist = sanitize(&track.artist);
            track.album_artist = sanitize(&track.album_artist);
            track.album = sanitize(&track.album);
        }
        Ok(playlist)
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
        Self::collect_audio_files_inner(dir, &[], &mut files);
        files
    }

    /// Like [`collect_audio_files`] but also recognises extensions registered
    /// by filetype plugins at runtime.
    pub fn collect_audio_files_extended(dir: &Path, extra_exts: &[String]) -> Vec<PathBuf> {
        let mut files = Vec::new();
        Self::collect_audio_files_inner(dir, extra_exts, &mut files);
        files
    }

    /// Core scanning logic shared by [`scan_folder_for_ui`] and [`scan_files_for_ui`].
    ///
    /// Must be called from inside a background thread — never on the GTK main thread.
    ///
    /// ## Phase 1 — fast tracks
    /// Each audio file produces a `Track` with only its path and filename stem set
    /// (no ID3 reads).  Tracks are sent one at a time via `fast_tx` as they are
    /// created so the receiver can start displaying them immediately without waiting
    /// for the whole directory to be walked.  Successfully-created fast tracks are
    /// collected in `successful` to drive Phase 2.
    ///
    /// ## Phase 2 — metadata
    /// Full ID3/Vorbis tags are read for each file in `successful`.  Results are
    /// sent as `(scan_index, title, artist, album_artist, album)` where `scan_index`
    /// is the 0-based position within the fast tracks that were sent.  This lets the
    /// receiver patch `playlist.tracks[scan_start + scan_index]` in O(1) without any
    /// search.
    ///
    /// Phase 2 only starts after all of Phase 1 has been sent, so when the first
    /// metadata message arrives the receiver can be certain every fast track is
    /// already in the channel (or has been received).
    fn scan_paths_in_thread(
        files: Vec<PathBuf>,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        fast_tx: std::sync::mpsc::Sender<Track>,
        metadata_tx: std::sync::mpsc::Sender<(usize, String, String, String, String)>,
        done_tx: std::sync::mpsc::Sender<usize>,
        phase1_done_tx: std::sync::mpsc::Sender<usize>,
    ) {
        if files.is_empty() {
            // Signal Phase 1 complete (with 0 tracks) before the final done.
            let _ = phase1_done_tx.send(0);
            let _ = done_tx.send(0);
            return;
        }

        // Phase 1: stream fast tracks one at a time so the UI can start showing
        // them without waiting for the full directory walk to finish.
        let mut successful: Vec<PathBuf> = Vec::with_capacity(files.len());
        for f in &files {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                // Signal Phase 1 complete with however many tracks were sent.
                let _ = phase1_done_tx.send(successful.len());
                let _ = done_tx.send(successful.len());
                return;
            }
            if let Ok(t) = Track::from_path_fast(f) {
                let _ = fast_tx.send(t);
                successful.push(f.clone());
            }
        }

        // All Phase 1 tracks have been sent; tell the poller it can now treat an
        // empty fast_rx as "exhausted" rather than "not started yet".
        let _ = phase1_done_tx.send(successful.len());

        // Phase 2: read full metadata for each successfully-added fast track and
        // send by index so the receiver can patch the playlist in O(1).
        for (idx, path) in successful.iter().enumerate() {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            if let Ok(track) = Track::from_path(path) {
                let _ = metadata_tx.send((
                    idx,
                    track.title,
                    track.artist,
                    track.album_artist,
                    track.album,
                ));
            }
        }

        let _ = done_tx.send(successful.len());
    }

    /// Scan a folder for audio files and stream results to the playlist UI.
    ///
    /// Spawns a background thread that recursively walks `folder`, then runs
    /// the two-phase fast-track / metadata scan.  Results arrive via four
    /// channels; the caller must poll them with `glib::timeout_add_local` —
    /// never block the GTK main thread waiting on them.
    ///
    /// See [`scan_paths_in_thread`] for channel semantics.
    pub fn scan_folder_for_ui(
        folder: PathBuf,
        extra_extensions: Vec<String>,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        fast_tx: std::sync::mpsc::Sender<Track>,
        metadata_tx: std::sync::mpsc::Sender<(usize, String, String, String, String)>,
        done_tx: std::sync::mpsc::Sender<usize>,
        phase1_done_tx: std::sync::mpsc::Sender<usize>,
    ) {
        std::thread::spawn(move || {
            let files = Self::collect_audio_files_extended(&folder, &extra_extensions);
            Self::scan_paths_in_thread(files, cancel, fast_tx, metadata_tx, done_tx, phase1_done_tx);
        });
    }

    /// Scan an explicit list of audio file paths and stream results to the playlist UI.
    ///
    /// Identical to [`scan_folder_for_ui`] but takes a pre-collected list of paths
    /// rather than walking a directory.  Used by "Add Files" to handle large
    /// multi-file selections without blocking the GTK main thread.
    ///
    /// See [`scan_paths_in_thread`] for channel semantics.
    pub fn scan_files_for_ui(
        files: Vec<PathBuf>,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        fast_tx: std::sync::mpsc::Sender<Track>,
        metadata_tx: std::sync::mpsc::Sender<(usize, String, String, String, String)>,
        done_tx: std::sync::mpsc::Sender<usize>,
        phase1_done_tx: std::sync::mpsc::Sender<usize>,
    ) {
        std::thread::spawn(move || {
            Self::scan_paths_in_thread(files, cancel, fast_tx, metadata_tx, done_tx, phase1_done_tx);
        });
    }

    /// Internal recursive helper for [`collect_audio_files`].
    ///
    /// Populates `files` with audio file paths found under `dir`.  Entries in
    /// each directory are sorted alphabetically before recursion so that the
    /// final order is stable across runs and platforms.  `extra_exts` contains
    /// additional lower-case extension strings (without dots) to recognise
    /// beyond the built-in [`AUDIO_EXTENSIONS`] list.
    fn collect_audio_files_inner(dir: &Path, extra_exts: &[String], files: &mut Vec<PathBuf>) {
        // Attempt to read the directory; silently skip on any error (e.g.
        // permission denied) to keep the scan robust.
        let read_dir = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return,
        };

        // Collect all valid entries first so we can sort them.
        let mut entries: Vec<PathBuf> = read_dir.filter_map(|e| e.ok().map(|e| e.path())).collect();

        // Sort alphabetically by the full path so sub-directories and files
        // are ordered consistently regardless of filesystem traversal order.
        entries.sort_unstable_by(|a, b| a.file_name().cmp(&b.file_name()));

        for path in entries {
            if path.is_dir() {
                // Recurse depth-first into sub-directories.
                Self::collect_audio_files_inner(&path, extra_exts, files);
            } else if is_audio_file_extended(&path, extra_exts) {
                // Include files whose extension is a known audio type, plus
                // any extra extensions contributed by filetype plugins.
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
                            errors.push(format!("Cannot load '{}': {}", audio_path.display(), e));
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

    #[test]
    fn sanitize_passes_through_normal_strings() {
        assert_eq!(sanitize("hello world"), "hello world");
        assert_eq!(sanitize(""), "");
        assert_eq!(sanitize("🎵 Artist — Album"), "🎵 Artist — Album");
    }

    #[test]
    fn sanitize_removes_nul_bytes() {
        assert_eq!(sanitize("hello\x00world"), "helloworld");
        assert_eq!(sanitize("\x00start"), "start");
        assert_eq!(sanitize("end\x00"), "end");
        assert_eq!(sanitize("\x00\x00\x00"), "");
    }

    #[test]
    fn sanitize_removes_toml_unicode_escape() {
        assert_eq!(sanitize("hello\\u0000world"), "helloworld");
        assert_eq!(sanitize("\\u0000start"), "start");
        assert_eq!(sanitize("end\\u0000"), "end");
        assert_eq!(sanitize("\\u0000"), "");
    }

    #[test]
    fn sanitize_handles_both_nul_and_toml_escape() {
        assert_eq!(sanitize("a\x00b\\u0000c"), "abc");
    }

    #[test]
    fn track_from_libtrack_copies_all_fields() {
        use crate::media_library::{LibTrack, SortKeys};

        let lib = LibTrack {
            id: 42,
            path: "/music/test.mp3".into(),
            artist: Some("Test Artist".into()),
            title: Some("Test Title".into()),
            album: Some("Test Album".into()),
            track_num: Some(3),
            genre: Some("Rock".into()),
            year: Some(2024),
            bpm: Some("120".into()),
            length_secs: Some(180.5),
            bitrate: Some(320),
            channels: Some(2),
            filetype: Some("mp3".into()),
            filename: "test.mp3".into(),
            play_count: 5,
            last_played: Some("2024-01-15T10:30:00".into()),
            comment: None,
            album_artist: None,
            disc_num: None,
            disc_total: None,
            composer: None,
            original_artist: None,
            copyright: None,
            url: None,
            encoded_by: None,
            lyric: None,
            artwork_path: None,
            last_scanned: None,
            sort_keys: SortKeys::default(),
        };

        let track = Track::from(&lib);

        assert_eq!(track.path, PathBuf::from("/music/test.mp3"));
        assert_eq!(track.title, "Test Title");
        assert_eq!(track.artist, "Test Artist");
        assert_eq!(track.album, "Test Album");
        assert_eq!(track.duration, Some(Duration::from_secs_f64(180.5)));
        assert!(!track.broken);
    }

    #[test]
    fn track_from_libtrack_falls_back_to_filename_when_title_is_none() {
        use crate::media_library::{LibTrack, SortKeys};

        let lib = LibTrack {
            id: 1,
            path: "/music/no_tags.mp3".into(),
            artist: None,
            title: None,
            album: None,
            track_num: None,
            genre: None,
            year: None,
            bpm: None,
            length_secs: Some(60.0),
            bitrate: None,
            channels: None,
            filetype: None,
            filename: "no_tags.mp3".into(),
            play_count: 0,
            last_played: None,
            comment: None,
            album_artist: None,
            disc_num: None,
            disc_total: None,
            composer: None,
            original_artist: None,
            copyright: None,
            url: None,
            encoded_by: None,
            lyric: None,
            artwork_path: None,
            last_scanned: None,
            sort_keys: SortKeys::default(),
        };

        let track = Track::from(&lib);

        assert_eq!(track.title, "no_tags.mp3");
        assert_eq!(track.artist, "");
        assert_eq!(track.album, "");
        assert_eq!(track.duration, Some(Duration::from_secs_f64(60.0)));
    }

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
        let files =
            Playlist::collect_audio_files(Path::new("/nonexistent_dir_that_does_not_exist"));
        assert!(files.is_empty());
    }

    #[test]
    fn collect_audio_files_extended_includes_extra_extensions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("song.mp3"), b"mp3").unwrap();
        std::fs::write(dir.path().join("song.ogg"), b"ogg").unwrap();
        std::fs::write(dir.path().join("song.custom"), b"custom").unwrap();
        std::fs::write(dir.path().join("song.unknown"), b"unknown").unwrap();

        let built_in = Playlist::collect_audio_files(dir.path());
        assert!(
            built_in.iter().all(|p| p.extension().unwrap() != "custom"),
            "built-in scan must not include .custom files"
        );

        let with_extra =
            Playlist::collect_audio_files_extended(dir.path(), &["custom".to_string()]);
        assert!(
            with_extra
                .iter()
                .any(|p| p.extension().unwrap() == "custom"),
            "extended scan must include .custom files"
        );
        assert!(
            with_extra
                .iter()
                .all(|p| p.extension().unwrap() != "unknown"),
            "extended scan must not include unknown extensions"
        );
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
        assert_eq!(
            p.tracks
                .iter()
                .map(|t| t.title.as_str())
                .collect::<Vec<_>>(),
            ["A", "C"]
        );
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
            p.tracks
                .iter()
                .map(|t| t.title.as_str())
                .collect::<Vec<_>>(),
            ["A", "C", "D", "B", "E"]
        );
    }

    #[test]
    fn move_track_backward() {
        let mut p = playlist_of(&["A", "B", "C", "D", "E"]);
        assert!(p.move_track(3, 1)); // move D to position 1
        assert_eq!(
            p.tracks
                .iter()
                .map(|t| t.title.as_str())
                .collect::<Vec<_>>(),
            ["A", "D", "B", "C", "E"]
        );
    }

    #[test]
    fn move_track_same_position_is_noop() {
        let mut p = playlist_of(&["A", "B", "C"]);
        assert!(p.move_track(1, 1));
        assert_eq!(
            p.tracks
                .iter()
                .map(|t| t.title.as_str())
                .collect::<Vec<_>>(),
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

    // -----------------------------------------------------------------------
    // Track::from_path_fast()
    // -----------------------------------------------------------------------

    #[test]
    fn from_path_fast_uses_file_stem_as_title() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("my_song.mp3");
        std::fs::write(&path, b"fake").unwrap();
        let track = Track::from_path_fast(&path).unwrap();
        assert_eq!(track.title, "my_song");
    }

    #[test]
    fn from_path_fast_leaves_metadata_fields_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("track.mp3");
        std::fs::write(&path, b"fake").unwrap();
        let track = Track::from_path_fast(&path).unwrap();
        assert!(track.artist.is_empty());
        assert!(track.album_artist.is_empty());
        assert!(track.album.is_empty());
        assert!(track.duration.is_none());
        assert!(!track.broken);
    }

    #[test]
    fn from_path_fast_fails_on_nonexistent_path() {
        let result = Track::from_path_fast(Path::new("/nonexistent/ghost.mp3"));
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // Playlist::scan_files_for_ui() and scan_folder_for_ui()
    //
    // These share the same scan_paths_in_thread core.  scan_files_for_ui is
    // tested directly (simpler setup); one test verifies scan_folder_for_ui
    // discovers files via directory walk.
    // -----------------------------------------------------------------------

    /// Receive all fast tracks and all metadata from a completed scan.
    fn drain_scan(
        fast_rx: std::sync::mpsc::Receiver<Track>,
        meta_rx: std::sync::mpsc::Receiver<(usize, String, String, String, String)>,
        done_rx: std::sync::mpsc::Receiver<usize>,
    ) -> (Vec<Track>, Vec<(usize, String, String, String, String)>, usize) {
        let timeout = std::time::Duration::from_secs(5);
        let total = done_rx.recv_timeout(timeout).expect("scan did not complete");
        let fast: Vec<_> = fast_rx.try_iter().collect();
        let meta: Vec<_> = meta_rx.try_iter().collect();
        (fast, meta, total)
    }

    #[test]
    fn scan_files_for_ui_empty_input_completes_with_zero() {
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (fast_tx, fast_rx) = std::sync::mpsc::channel();
        let (meta_tx, meta_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let (phase1_done_tx, _phase1_done_rx) = std::sync::mpsc::channel::<usize>();
        Playlist::scan_files_for_ui(vec![], cancel, fast_tx, meta_tx, done_tx, phase1_done_tx);
        let (fast, meta, total) = drain_scan(fast_rx, meta_rx, done_rx);
        assert_eq!(total, 0);
        assert!(fast.is_empty());
        assert!(meta.is_empty());
    }

    #[test]
    fn scan_files_for_ui_sends_one_fast_track_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = ["a.mp3", "b.mp3", "c.mp3"]
            .iter()
            .map(|name| {
                let p = dir.path().join(name);
                std::fs::write(&p, b"fake").unwrap();
                p
            })
            .collect();

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (fast_tx, fast_rx) = std::sync::mpsc::channel();
        let (meta_tx, meta_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let (phase1_done_tx, _phase1_done_rx) = std::sync::mpsc::channel::<usize>();
        Playlist::scan_files_for_ui(paths.clone(), cancel, fast_tx, meta_tx, done_tx, phase1_done_tx);
        let (fast, _meta, total) = drain_scan(fast_rx, meta_rx, done_rx);

        assert_eq!(total, 3);
        assert_eq!(fast.len(), 3);
    }

    #[test]
    fn scan_files_for_ui_metadata_indices_match_fast_track_order() {
        let dir = tempfile::tempdir().unwrap();
        // Named so alphabetical order is deterministic: 1, 2, 3
        let paths: Vec<PathBuf> = ["1_track.mp3", "2_track.mp3", "3_track.mp3"]
            .iter()
            .map(|name| {
                let p = dir.path().join(name);
                std::fs::write(&p, b"fake").unwrap();
                p
            })
            .collect();

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (fast_tx, fast_rx) = std::sync::mpsc::channel();
        let (meta_tx, meta_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let (phase1_done_tx, _phase1_done_rx) = std::sync::mpsc::channel::<usize>();
        Playlist::scan_files_for_ui(paths.clone(), cancel, fast_tx, meta_tx, done_tx, phase1_done_tx);
        let (fast, meta, _total) = drain_scan(fast_rx, meta_rx, done_rx);

        assert_eq!(fast.len(), 3, "expected 3 fast tracks");
        assert_eq!(meta.len(), 3, "expected 3 metadata updates");

        // Indices must be 0, 1, 2 in the order the background thread processed them.
        let indices: Vec<usize> = meta.iter().map(|(idx, ..)| *idx).collect();
        assert_eq!(indices, vec![0, 1, 2]);
    }

    #[test]
    fn scan_files_for_ui_metadata_index_addresses_corresponding_fast_track() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = ["alpha.mp3", "beta.mp3"]
            .iter()
            .map(|name| {
                let p = dir.path().join(name);
                std::fs::write(&p, b"fake").unwrap();
                p
            })
            .collect();

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (fast_tx, fast_rx) = std::sync::mpsc::channel();
        let (meta_tx, meta_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let (phase1_done_tx, _phase1_done_rx) = std::sync::mpsc::channel::<usize>();
        Playlist::scan_files_for_ui(paths.clone(), cancel, fast_tx, meta_tx, done_tx, phase1_done_tx);
        let (fast, meta, _total) = drain_scan(fast_rx, meta_rx, done_rx);

        // For each metadata update, the index should point at the fast track for
        // the same file (fast tracks arrive in the same order as paths).
        for (idx, title, ..) in &meta {
            let fast_title = &fast[*idx].title;
            // Both title (from from_path) and fast title (from from_path_fast)
            // fall back to the filename stem, so they must match.
            assert_eq!(fast_title, title, "metadata index {idx} should match fast track title");
        }
    }

    #[test]
    fn scan_files_for_ui_cancel_stops_scan() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = (0..10)
            .map(|i| {
                let p = dir.path().join(format!("{:02}.mp3", i));
                std::fs::write(&p, b"fake").unwrap();
                p
            })
            .collect();

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (fast_tx, fast_rx) = std::sync::mpsc::channel();
        let (meta_tx, meta_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();

        // Set cancel before starting — scan should abort and still send done.
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        let (phase1_done_tx, _phase1_done_rx) = std::sync::mpsc::channel::<usize>();
        Playlist::scan_files_for_ui(paths, cancel, fast_tx, meta_tx, done_tx, phase1_done_tx);

        let timeout = std::time::Duration::from_secs(5);
        let total = done_rx.recv_timeout(timeout).expect("scan did not send done");
        // With cancel pre-set, at most 0 fast tracks should have been sent.
        let fast: Vec<_> = fast_rx.try_iter().collect();
        assert!(fast.len() <= total, "fast tracks must not exceed done count");
        // No metadata should have been sent since Phase 1 was aborted.
        let meta: Vec<_> = meta_rx.try_iter().collect();
        assert!(meta.is_empty(), "no metadata expected when cancelled before Phase 2");
    }

    #[test]
    fn scan_folder_for_ui_empty_folder_completes_with_zero() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (fast_tx, fast_rx) = std::sync::mpsc::channel();
        let (meta_tx, meta_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let (phase1_done_tx, _phase1_done_rx) = std::sync::mpsc::channel::<usize>();
        Playlist::scan_folder_for_ui(dir.path().to_path_buf(), vec![], cancel, fast_tx, meta_tx, done_tx, phase1_done_tx);
        let (fast, meta, total) = drain_scan(fast_rx, meta_rx, done_rx);
        assert_eq!(total, 0);
        assert!(fast.is_empty());
        assert!(meta.is_empty());
    }

    #[test]
    fn scan_folder_for_ui_discovers_audio_files_in_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("song1.mp3"), b"fake").unwrap();
        std::fs::write(dir.path().join("song2.flac"), b"fake").unwrap();
        std::fs::write(dir.path().join("cover.jpg"), b"fake").unwrap(); // not audio

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (fast_tx, fast_rx) = std::sync::mpsc::channel();
        let (meta_tx, meta_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let (phase1_done_tx, _phase1_done_rx) = std::sync::mpsc::channel::<usize>();
        Playlist::scan_folder_for_ui(dir.path().to_path_buf(), vec![], cancel, fast_tx, meta_tx, done_tx, phase1_done_tx);
        let (fast, meta, total) = drain_scan(fast_rx, meta_rx, done_rx);

        assert_eq!(total, 2, "only the two audio files should be scanned");
        assert_eq!(fast.len(), 2);
        assert_eq!(meta.len(), 2);
        // Non-audio files must not appear in fast tracks.
        assert!(
            fast.iter().all(|t| !t.title.contains("cover")),
            "cover.jpg must not appear as a track"
        );
    }
}
