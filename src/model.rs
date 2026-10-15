//! Core data model: tracks and playlists.
//!
//! This module intentionally has no UI or audio dependencies — it only holds
//! data and logic that is shared between the TUI and the GTK4 GUI.  Both
//! frontends own a single [`Playlist`] instance and operate on it through the
//! methods defined here.

use anyhow::{Context, Result};
use id3::TagLike;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::textutil::sanitize;

// ---------------------------------------------------------------------------
// Audio file extension detection
// ---------------------------------------------------------------------------

/// All audio file extensions Sparkamp will recognise when scanning directories.
///
/// Matching is case-insensitive, so `.MP3` and `.Flac` are both accepted.
///
/// **The list differs by platform, because the decoder does.** Linux decodes
/// through GStreamer, which the GNOME runtime supplies with plugins for every
/// format below. macOS decodes through AVFoundation, and CoreAudio simply does
/// not know some of these containers: it fails at
/// `AudioFileOpenURL`/`kAudioFileUnsupportedFileTypeError` rather than
/// producing poor audio. Listing a format the platform cannot open means a
/// scan adds rows that can never play, and a burn list that refuses itself at
/// transcode time.
///
/// Measured on macOS 26 on 2026-09-05, one 30-second tone per container
/// through the real burn path, and cross-checked with `afinfo`, which uses the
/// same CoreAudio stack and agreed on all eleven:
///
/// | Reads | Refuses |
/// |---|---|
/// | mp3, flac, ogg, opus, wav, aac, m4a, aiff | tta, wma, wv, ape |
///
/// `ogg` and `opus` are on the reading side, which is worth stating because
/// the obvious assumption is that Apple decodes neither. Measure before
/// removing anything else from this list.
#[cfg(not(target_os = "macos"))]
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "opus", "wav", "aac", "m4a", "wma", "ape", "mpc", "tta", "wv", "aiff",
    "aif",
];

/// See the note above. `mpc` is excluded without having been measured: no
/// Musepack encoder exists on this machine to make a fixture with, and
/// CoreAudio refuses every other container in its family. If a Musepack file
/// ever turns up, test it rather than trusting this line.
#[cfg(target_os = "macos")]
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "opus", "wav", "aac", "m4a", "aiff", "aif",
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

/// Whether a track path is a Linux disc pseudo-URI (`cdda://N?device=…`)
/// rather than a real file. Disc media is always read-only and has no file to
/// stat, so callers use this to keep the read-only lock indicator across a
/// reload (permission checks would otherwise report the missing "file" as
/// writable). macOS disc tracks are real AIFF files on a read-only mount and
/// don't need this.
pub fn is_disc_uri(path: &Path) -> bool {
    path.to_string_lossy().starts_with("cdda://")
}

// ---------------------------------------------------------------------------
// SortKey
// ---------------------------------------------------------------------------

/// Sort criterion for the active-playlist Sort menu (phase 7).
// Consumed by the frontend Sort menus (phase-7, later tasks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Title,
    Artist,
    Album,
    Filename,
    Path,
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
    /// Set at runtime when the file has read-only permissions.
    /// Used to display a lock indicator in the UI.
    #[serde(skip)]
    pub read_only: bool,
    /// Session-only stable identity for the manual play queue (phase 5).
    /// Distinct even for duplicate paths; reassigned at load. Never persisted
    /// (`serde(skip)`) — the queue it keys is session-only (Winamp behavior).
    /// `0` is the unstamped sentinel; the owning `Playlist` stamps a real id
    /// via `add` / `assign_ids` / `ensure_ids`.
    #[serde(skip)]
    pub id: u64,
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
        let read_only = crate::media_library::is_read_only(&path);
        Ok(Track {
            path,
            title,
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            duration: None,
            broken: false,
            read_only,
            id: 0,
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
                if let Some((t, ar, aa, al)) = crate::tags::read_symphonia_basic(&path) {
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

        let read_only = crate::media_library::is_read_only(&path);
        Ok(Track {
            path,
            title,
            artist,
            album_artist,
            album,
            duration: None,
            broken: false,
            read_only,
            id: 0,
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
        // Disc tracks store a ready-made URI (`cdda://3?device=/dev/sr0`)
        // instead of a file path — pass it through untouched: the engine
        // understands the scheme, and file-URI escaping would corrupt it.
        if path_str.starts_with("cdda://") {
            return path_str;
        }
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
/// without re-probing the file.  Much faster than re-parsing tags + duration
/// from disk and lets the active playlist show known values immediately.
///
/// Uses `Duration::try_from_secs_f64` (not the panicking variant) because
/// `length_secs` originates in the DB and could conceivably be NaN /
/// negative for legacy rows; we fall back to `Duration::ZERO` in that case
/// rather than aborting playback.
impl From<&crate::media_library::LibTrack> for Track {
    fn from(lib: &crate::media_library::LibTrack) -> Self {
        let path = PathBuf::from(&lib.path);
        // Deliberately no filesystem access. This used to `stat` twice here —
        // once for `exists`, once for `is_read_only` — so that the ⚠ and 🔒
        // markers were correct the instant a row appeared. That is two
        // syscalls per track on the GTK main thread, which is 72,000 of them
        // when the user selects a 36k library and adds it to the playlist.
        //
        // Both flags start clear and are filled in by the background status
        // pass the add helper schedules (`window/playlist_add.rs`), so the
        // rows appear immediately and the markers a moment later.
        Track {
            path,
            title: lib.title.clone().unwrap_or_else(|| lib.filename.clone()),
            artist: lib.artist.clone().unwrap_or_default(),
            // The library knows this; it used to be dropped here and then
            // backfilled by a background file read — the row-check worker on
            // Linux, a rayon probe in the macOS bridge. Copying it means the
            // field is right the moment the row appears, and means those
            // probes have nothing left to discover for a scanned track.
            album_artist: lib.album_artist.clone().unwrap_or_default(),
            album: lib.album.clone().unwrap_or_default(),
            duration: lib
                .length_secs
                .and_then(|s| Duration::try_from_secs_f64(s).ok()),
            broken: false,
            read_only: false,
            id: 0,
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

/// The same display, for a length already held as seconds.
///
/// The media library stores `length_secs` as an `f64`, so every view over
/// library rows had open-coded the format and the `-:--` fallback rather than
/// convert to a `Duration` first. Four copies agreed; several others nearby
/// deliberately do not (an em dash for unknown, right-padding for column
/// alignment, a leading minus for remaining time), so only the ones that
/// already matched call this.
///
/// A negative or non-finite length is treated as unknown rather than
/// panicking, which is what `Duration::from_secs_f64` would do.
pub fn fmt_secs(secs: Option<f64>) -> String {
    fmt_duration(secs.and_then(|s| Duration::try_from_secs_f64(s).ok()))
}

/// The tags a background scan read for one file.
#[derive(Debug, Clone, Default)]
pub struct ScannedTags {
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
}

/// One message from a background playlist scan
/// ([`Playlist::scan_folder_for_ui`], [`Playlist::scan_files_for_ui`]).
///
/// They arrive in this order: every `Found`, then `AllFound`, then the
/// `Tags`, then `Done`.
#[derive(Debug)]
pub enum ScanEvent {
    /// A file to add, with only its path and filename stem filled in.
    Found(Track),
    /// Every file has been found. Tags follow.
    AllFound,
    /// Tags for the `index`th `Found` track of this scan, counting from 0.
    ///
    /// Matched by that position in the scan, not by playlist row. The rows
    /// can move while tags are still being read, so a receiver keeps its
    /// own map from `index` to the entry it added for it, and writes them
    /// with [`Playlist::apply_scanned_tags`].
    Tags { index: usize, tags: ScannedTags },
    /// The scan is over, finished or cancelled. Nothing follows.
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Playlist {
    /// All tracks in display order.
    pub tracks: Vec<Track>,
    /// Zero-based index of the track that is currently selected (and usually
    /// playing).  Always within `[0, tracks.len())` when `tracks` is
    /// non-empty; fixed at `0` when the playlist is empty.
    pub current_index: usize,
    /// Monotonic counter handing out `Track.id` values for the play queue.
    /// Session-only; never persisted (starts at 0 each run, ids reassigned at
    /// load via `assign_ids`).
    #[serde(skip)]
    next_entry_id: u64,
}

impl Playlist {
    /// Create an empty playlist with `current_index` at 0.
    pub fn new() -> Self {
        Playlist::default()
    }

    /// Append a track to the end of the playlist, stamping it with a fresh
    /// queue id. This is the canonical single-append path; bulk paths that
    /// push straight into `tracks` should call [`ensure_ids`](Self::ensure_ids)
    /// afterward so no entry keeps the id-0 sentinel.
    pub fn add(&mut self, mut track: Track) {
        self.next_entry_id += 1;
        track.id = self.next_entry_id;
        self.tracks.push(track);
    }

    /// Reassign a fresh queue id to EVERY entry. Call after any bulk load or
    /// wholesale replacement of `tracks` (e.g. `load_last`, opening a saved
    /// playlist, replace-current-with). The queue is session-only, so ids do
    /// not need to survive across a load.
    pub fn assign_ids(&mut self) {
        for t in &mut self.tracks {
            self.next_entry_id += 1;
            t.id = self.next_entry_id;
        }
    }

    /// Stamp a fresh queue id onto any entry still holding the id-0 sentinel,
    /// leaving already-stamped entries (and their queue membership) untouched.
    /// Cheap idempotent safety net for insertion sites that push straight into
    /// `tracks` without going through [`add`](Self::add); frontends call this
    /// in their playlist-rebuild seam before reading `Track.id` for the queue.
    // Consumed by the frontend queue wiring (phase-5 tasks 5/7/8).
    pub fn ensure_ids(&mut self) {
        for t in &mut self.tracks {
            if t.id == 0 {
                self.next_entry_id += 1;
                t.id = self.next_entry_id;
            }
        }
    }

    /// Clear all tracks from the playlist, resetting current_index to 0.
    ///
    /// Used by GTK and the macOS FFI bridge; not reached from the macOS
    /// `bin "sparkamp"` build.
    pub fn clear(&mut self) {
        self.tracks.clear();
        self.current_index = 0;
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
    ///
    /// Used by the GTK frontend's linear-back logic; macOS uses the shared
    /// `Controller::nav_prev` instead and does not call this directly.
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

    /// Move several rows to `to` as one block, keeping their relative order.
    ///
    /// Returns `(start, count)` — where the block landed — so a frontend can
    /// re-select exactly what it moved, or `None` when nothing moved.
    ///
    /// Not a loop over [`move_track`]: each single move shifts every index
    /// after it, so feeding it a multi-row selection walks rows into the wrong
    /// places. The sources are removed highest-first (so an earlier removal
    /// cannot invalidate a later index), the destination shrinks by one for
    /// each row removed from in front of it, and the block is reinserted in
    /// its original order.
    ///
    /// The playing track is re-pointed by id rather than by index arithmetic,
    /// which is what keeps it playing when it is inside — or displaced by —
    /// the moved block.
    ///
    /// This is GTK's `dnd.rs` reorder, lifted into core so the frontends share
    /// one implementation. macOS had none: its `moveTrack` took a set of rows
    /// and moved only the first, leaving the rest behind while everything
    /// between them shifted — which read as rows moving at random.
    // Reached only through `sparkamp_playlist_move_many`, which is lib-only,
    // so the binary sees it as dead. GTK still runs its own inline copy of
    // this algorithm in `dnd.rs`; collapsing that onto this would remove both
    // the duplication and this attribute, but it also has to decide what GTK
    // does about `current_index` — the inline version never touched it, and
    // this one re-points the playing track by id.
    pub fn move_tracks(&mut self, from: &[usize], to: usize) -> Option<(usize, usize)> {
        let mut sorted: Vec<usize> = from
            .iter()
            .copied()
            .filter(|&i| i < self.tracks.len())
            .collect();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.is_empty() {
            return None;
        }
        let playing = self.stamped_current_id();

        let mut adjusted_dst = to;
        let mut removed: Vec<Track> = Vec::with_capacity(sorted.len());
        // Highest-first: removing a low index would shift every higher one.
        for &src in sorted.iter().rev() {
            let t = self.tracks.remove(src);
            if src < adjusted_dst {
                adjusted_dst -= 1;
            }
            removed.push(t);
        }
        // `removed` is highest-first; put it back the way the user sees it.
        removed.reverse();

        let insert_at = adjusted_dst.min(self.tracks.len());
        let count = removed.len();
        for (i, t) in removed.into_iter().enumerate() {
            self.tracks.insert(insert_at + i, t);
        }
        if let Some(id) = playing {
            self.repoint_current_to(id);
        }
        Some((insert_at, count))
    }

    /// Id of the current track, if any — used to keep the playing track
    /// selected across a reorder.
    // Consumed by the frontend Sort menus (phase-7, later tasks).
    pub fn current_id(&self) -> Option<u64> {
        self.tracks.get(self.current_index).map(|t| t.id)
    }

    /// Point `current_index` at the track with `id`; unchanged if not found.
    ///
    /// Id 0 is the "not stamped yet" sentinel, never a real entry, so it is
    /// rejected outright — searching for it would match the first unstamped
    /// row and silently move `current_index` onto the wrong track.
    // Consumed by the frontend Sort menus (phase-7, later tasks).
    pub fn repoint_current_to(&mut self, id: u64) {
        if id == 0 {
            return;
        }
        if let Some(pos) = self.tracks.iter().position(|t| t.id == id) {
            self.current_index = pos;
        }
    }

    /// Write tags a background scan read onto the entry with `id`, and return
    /// the row it is on now, or `None` if it has been removed meanwhile.
    ///
    /// `row_hint` is the row it was added at. It is checked first and is
    /// usually still right, but rows can be deleted, dragged or sorted while
    /// a scan reads tags, so the entry is found again by id when it is not.
    /// Writing to the hinted row regardless is what once put a whole scan's
    /// titles one row out after a delete above them.
    pub fn apply_scanned_tags(
        &mut self,
        id: u64,
        row_hint: usize,
        tags: ScannedTags,
    ) -> Option<usize> {
        let row = if self.tracks.get(row_hint).is_some_and(|t| t.id == id) {
            row_hint
        } else {
            self.tracks.iter().position(|t| t.id == id)?
        };
        let track = &mut self.tracks[row];
        track.title = tags.title;
        track.artist = tags.artist;
        track.album_artist = tags.album_artist;
        track.album = tags.album;
        Some(row)
    }

    /// Case-insensitive, stable sort of the active playlist. The playing track
    /// stays current (re-pointed by id afterwards). Blank sort fields fall
    /// back to the filename so untitled rows sort by their visible label.
    // Consumed by the frontend Sort menus (phase-7, later tasks).
    pub fn sort_by(&mut self, key: SortKey) {
        let playing = self.stamped_current_id();
        // `sort_by_cached_key` computes each lowercase key once instead of on
        // every comparison, and is stable — both of which this sort needs.
        self.tracks.sort_by_cached_key(|t| sort_field(t, key));
        if let Some(id) = playing {
            self.repoint_current_to(id);
        }
    }

    /// Reverse the active playlist; the playing track stays current.
    // Consumed by the frontend Sort menus (phase-7, later tasks).
    pub fn reverse(&mut self) {
        let playing = self.stamped_current_id();
        self.tracks.reverse();
        if let Some(id) = playing {
            self.repoint_current_to(id);
        }
    }

    /// Randomly permute the active playlist once (a one-shot reorder, distinct
    /// from shuffle PLAYBACK). The playing track stays current.
    // Consumed by the frontend Sort menus (phase-7, later tasks).
    pub fn randomize(&mut self) {
        use rand::seq::SliceRandom;
        let playing = self.stamped_current_id();
        self.tracks.shuffle(&mut rand::thread_rng());
        if let Some(id) = playing {
            self.repoint_current_to(id);
        }
    }

    /// Stamp every entry, then return the playing track's id.
    ///
    /// The reorder ops find the playing track again by id afterwards, so they
    /// need one that actually identifies it. Several paths push straight into
    /// `tracks` rather than going through `add` — the mac dedupe and Media
    /// Library bulk adds, GTK's drag-drop and disc-track adds — and those
    /// entries carry the id-0 sentinel until something stamps them. Left
    /// unstamped, a sort would look up id 0, land on whichever unstamped row
    /// came first, and move the playing highlight (and the next auto-advance)
    /// onto the wrong track. Stamping here means the ops hold regardless of
    /// how the entries arrived.
    fn stamped_current_id(&mut self) -> Option<u64> {
        self.ensure_ids();
        self.current_id()
    }

    /// Return the indices of all tracks whose `title`, `artist`, or `album`
    /// contain `query` (case-insensitive substring match).
    ///
    /// An empty `query` returns all indices.  Results are in playlist order.
    /// Return playlist indices whose tracks match every word in `query`.
    ///
    /// Each whitespace-delimited word must appear somewhere in the combined
    /// "title artist album_artist album filename" string, so cross-field
    /// queries like "ed sheeran don't" correctly match a track titled
    /// "Don't" by "Ed Sheeran".  An empty query returns an empty vec —
    /// callers decide what to show when there is no input.
    pub fn search_indices(&self, query: &str) -> Vec<usize> {
        let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
        if words.is_empty() {
            return Vec::new();
        }
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                // Include the filename stem so bare filename searches work
                // even when metadata is absent.
                let filename = t
                    .path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                let combined = format!(
                    "{} {} {} {} {}",
                    t.title.to_lowercase(),
                    t.artist.to_lowercase(),
                    t.album_artist.to_lowercase(),
                    t.album.to_lowercase(),
                    filename,
                );
                words.iter().all(|w| combined.contains(w.as_str()))
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
            // Disc tracks (cdda://) have no file to stat but live on read-only
            // media — keep the lock indicator across reloads.
            track.read_only =
                is_disc_uri(&track.path) || crate::media_library::is_read_only(&track.path);
        }
        // Stamp fresh queue ids — `id` is serde(skip) so every loaded entry
        // arrives with the id-0 sentinel.
        playlist.assign_ids();
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
        Self::collect_audio_files_inner(dir, &mut files, &mut HashSet::new());
        files
    }

    /// Read every file in `files` on the calling thread, reporting through
    /// `tx` in two passes.
    ///
    /// The first pass sends a [`ScanEvent::Found`] per file as soon as it is
    /// stat'ed, path and filename only, so a frontend can list the files
    /// before any tags are read. Then [`ScanEvent::AllFound`]. The second
    /// pass reads each found file's tags and sends [`ScanEvent::Tags`]. Last
    /// comes [`ScanEvent::Done`], also after a cancel.
    ///
    /// One channel rather than one per kind of message: the receiver gets
    /// them in exactly this order, so it never has to work out whether the
    /// first pass is over or whether a late tag message is still on its way.
    fn scan_paths_in_thread(
        files: Vec<PathBuf>,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        tx: std::sync::mpsc::Sender<ScanEvent>,
    ) {
        let cancelled = || cancel.load(std::sync::atomic::Ordering::Relaxed);

        let mut found: Vec<PathBuf> = Vec::with_capacity(files.len());
        for f in &files {
            if cancelled() {
                break;
            }
            if let Ok(t) = Track::from_path_fast(f) {
                let _ = tx.send(ScanEvent::Found(t));
                found.push(f.clone());
            }
        }
        let _ = tx.send(ScanEvent::AllFound);

        for (index, path) in found.iter().enumerate() {
            if cancelled() {
                break;
            }
            if let Ok(track) = Track::from_path(path) {
                let tags = ScannedTags {
                    title: track.title,
                    artist: track.artist,
                    album_artist: track.album_artist,
                    album: track.album,
                };
                let _ = tx.send(ScanEvent::Tags { index, tags });
            }
        }

        let _ = tx.send(ScanEvent::Done);
    }

    /// Scan a folder for audio files on a background thread. Results arrive
    /// on the returned channel as described in [`ScanEvent`]; poll it from
    /// the UI's timer, never block on it from the main thread.
    pub fn scan_folder_for_ui(
        folder: PathBuf,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> std::sync::mpsc::Receiver<ScanEvent> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let files = Self::collect_audio_files(&folder);
            Self::scan_paths_in_thread(files, cancel, tx);
        });
        rx
    }

    /// Scan an explicit list of audio files on a background thread, the
    /// same way as [`scan_folder_for_ui`](Self::scan_folder_for_ui). Used by
    /// "Add Files" so a large selection does not block the main thread.
    pub fn scan_files_for_ui(
        files: Vec<PathBuf>,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> std::sync::mpsc::Receiver<ScanEvent> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || Self::scan_paths_in_thread(files, cancel, tx));
        rx
    }

    /// Internal recursive helper for [`collect_audio_files`].
    ///
    /// Populates `files` with audio file paths found under `dir`.  Entries in
    /// each directory are sorted alphabetically before recursion so that the
    /// final order is stable across runs and platforms.
    ///
    /// Symlinked directories are followed, since music is often kept on
    /// another disk and linked in. `visited` holds the real path of every
    /// directory walked so far, and one reached a second time is skipped.
    /// Without that, a link back up the tree (`Music/all -> ..`, or a link to
    /// `.`) listed every file again at each level until the kernel's 40-link
    /// limit stopped it, and two such links made the walk effectively endless.
    fn collect_audio_files_inner(
        dir: &Path,
        files: &mut Vec<PathBuf>,
        visited: &mut HashSet<PathBuf>,
    ) {
        let Ok(real) = dir.canonicalize() else {
            return;
        };
        if !visited.insert(real) {
            return;
        }
        // Attempt to read the directory; silently skip on any error (e.g.
        // permission denied) to keep the scan robust.
        let read_dir = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return,
        };

        // Collect all valid entries first so we can sort them.
        let mut entries: Vec<PathBuf> = read_dir.filter_map(|e| e.ok().map(|e| e.path())).collect();

        // Sort by file name so sub-directories and files are ordered
        // consistently regardless of filesystem traversal order.
        entries.sort_unstable_by(|a, b| a.file_name().cmp(&b.file_name()));

        for path in entries {
            if path.is_dir() {
                // Recurse depth-first into sub-directories.
                Self::collect_audio_files_inner(&path, files, visited);
            } else if is_audio_file(&path) {
                // Include files whose extension is a known audio type.
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
    ///
    /// **Reads every file.** Prefer [`crate::playlist_ingest::resolve`], which
    /// answers from the media library where it can and reads only what it must;
    /// the frontends and the CLI all go through that now. This remains for
    /// `sparkamp_playlist_add`, whose documented contract is the synchronous
    /// full read (macOS callers wanting the fast path already have
    /// `sparkamp_playlist_add_fast`), and so is dead code in the binary while
    /// still live in the library.
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

/// The lowercase string a track sorts on for `key`. Title/Artist/Album fall
/// back to the filename when blank (mirrors the row display fallback).
// Consumed by `Playlist::sort_by` only; unused warning until the frontend
// Sort menus land (phase-7, later tasks) and exercise it end-to-end.
fn sort_field(t: &Track, key: SortKey) -> String {
    let filename = || {
        t.path
            .file_name()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    match key {
        SortKey::Title => {
            if t.title.trim().is_empty() {
                filename()
            } else {
                t.title.to_lowercase()
            }
        }
        SortKey::Artist => {
            if t.artist.trim().is_empty() {
                filename()
            } else {
                t.artist.to_lowercase()
            }
        }
        SortKey::Album => {
            if t.album.trim().is_empty() {
                filename()
            } else {
                t.album.to_lowercase()
            }
        }
        SortKey::Filename => filename(),
        SortKey::Path => t.path.to_string_lossy().to_lowercase(),
    }
}

// ---------------------------------------------------------------------------
// Spectrum data
// ---------------------------------------------------------------------------

/// Stores the frequency spectrum data from GStreamer's spectrum element.
///
/// This data is populated from GStreamer bus messages and read by the visualizer
/// to display real audio frequency bars instead of synthetic animation.
#[derive(Debug, Clone)]
pub struct SpectrumData {
    /// Raw frequency band magnitudes, normalized to [0.0, 1.0].
    /// Each entry represents one frequency band from low to high frequencies.
    pub bands: Vec<f64>,
    /// Smoothed band magnitudes for display. Uses attack/release ballistics.
    smoothed_bands: Vec<f64>,
    /// Whether real spectrum data has been received from GStreamer.
    /// Used to distinguish between "no data yet" vs "data received but all zeros".
    has_received_data: bool,
}

impl SpectrumData {
    /// Create a new SpectrumData with the given number of bands initialized to zero.
    pub fn new(num_bands: usize) -> Self {
        Self {
            bands: vec![0.0; num_bands],
            smoothed_bands: vec![0.0; num_bands],
            has_received_data: false,
        }
    }

    /// Update the spectrum bands with new values from GStreamer.
    /// Applies attack/release smoothing for smooth bar animation.
    /// Sets has_received_data to true.
    pub fn update(&mut self, new_bands: Vec<f64>) {
        let attack = 0.7_f64;
        let release = 0.92_f64;

        // Ensure smoothed_bands is the right size
        if self.smoothed_bands.len() != new_bands.len() {
            self.smoothed_bands = vec![0.0; new_bands.len()];
        }

        for (i, &new_val) in new_bands.iter().enumerate() {
            let current = self.smoothed_bands[i];
            self.smoothed_bands[i] = if new_val > current {
                // Attack: rise quickly toward target
                current + (new_val - current) * attack
            } else {
                // Release: fall slowly toward target
                current + (new_val - current) * release
            };
        }

        self.bands = new_bands;
        self.has_received_data = true;
    }

    /// Return the smoothed band values for display.
    pub fn smoothed(&self) -> &[f64] {
        &self.smoothed_bands
    }

    /// Return true if real spectrum data has been received from GStreamer.
    /// Distinguishes between "no data yet" and "data received but all zeros".
    pub fn has_received_data(&self) -> bool {
        self.has_received_data
    }

    /// Zero buffers + clear `has_received_data` so the visualizer
    /// collapses to flat/no-bars after Stop instead of freezing on the
    /// last received frame.
    pub fn clear(&mut self) {
        for v in self.bands.iter_mut() {
            *v = 0.0;
        }
        for v in self.smoothed_bands.iter_mut() {
            *v = 0.0;
        }
        self.has_received_data = false;
    }
}

// ---------------------------------------------------------------------------
// Waveform ring buffer
// ---------------------------------------------------------------------------

/// Fixed-capacity ring buffer that stores recent raw PCM samples for the
/// waveform visualizer.
///
/// Samples are written from the GStreamer streaming thread (via a pad probe)
/// and read from the draw/UI thread.  Access is guarded by the caller via
/// `Arc<RwLock<WaveformBuffer>>`.
pub struct WaveformBuffer {
    samples: std::collections::VecDeque<f64>,
    capacity: usize,
}

impl WaveformBuffer {
    /// Create a new buffer that holds at most `capacity` samples.
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: std::collections::VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Append PCM samples, evicting the oldest when full.
    pub fn push_samples(&mut self, new_samples: &[f64]) {
        for &s in new_samples {
            if self.samples.len() >= self.capacity {
                self.samples.pop_front();
            }
            self.samples.push_back(s);
        }
    }

    /// Drop buffered samples so `get_samples` returns silence until new
    /// audio arrives — the waveform collapses to a flat line on Stop.
    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// Return `count` evenly-spaced samples with light smoothing applied.
    ///
    /// Returns silence (all zeros) when not enough samples have been buffered.
    /// Samples are in `[-1.0, 1.0]`.
    pub fn get_samples(&self, count: usize) -> Vec<f64> {
        let n = self.samples.len();
        if n < count / 2 {
            return vec![0.0; count];
        }

        // Linearly interpolate from the ring buffer to `count` output points.
        let resampled: Vec<f64> = (0..count)
            .map(|i| {
                let src_f = i as f64 * (n - 1).max(1) as f64 / (count - 1).max(1) as f64;
                let lo = src_f.floor() as usize;
                let hi = (src_f.ceil() as usize).min(n - 1);
                let t = src_f.fract();
                self.samples[lo] * (1.0 - t) + self.samples[hi] * t
            })
            .collect();

        // Gaussian-style 5-tap moving average for Winamp-style smoothness.
        let half = 2usize;
        (0..count)
            .map(|i| {
                let lo = i.saturating_sub(half);
                let hi = (i + half + 1).min(count);
                resampled[lo..hi].iter().sum::<f64>() / (hi - lo) as f64
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_disc_uri_matches_cdda_paths_only() {
        assert!(is_disc_uri(Path::new("cdda://3?device=/dev/sr0")));
        assert!(is_disc_uri(Path::new("cdda://1")));
        assert!(!is_disc_uri(Path::new("/home/u/music/song.mp3")));
        // A real file whose name merely contains "cdda" is not a disc URI.
        assert!(!is_disc_uri(Path::new("/music/cdda-rip.flac")));
    }

    #[test]
    fn reloaded_disc_track_stays_read_only() {
        // Mirrors Playlist::load's per-track read_only recompute: a cdda:// disc
        // path has no file to stat, so is_disc_uri must keep it read-only.
        let disc = Path::new("cdda://5?device=/dev/sr0");
        let read_only = is_disc_uri(disc) || crate::media_library::is_read_only(disc);
        assert!(read_only, "disc tracks must reload as read-only");
    }

    #[test]
    fn track_from_libtrack_copies_all_fields() {
        use crate::media_library::{LibTrack, SortKeys};

        // Use a real file so `broken` (existence-derived) is false; the test
        // is about field copying, not the missing-file path.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let tmp_path = tmp.path().to_string_lossy().into_owned();

        let lib = LibTrack {
            id: 42,
            path: tmp_path.clone(),
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
            album_artist: Some("Test Album Artist".into()),
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
            sample_rate: None,
            file_size: None,
            file_mtime: None,
            added_at: None,
            bitrate_mode: None,
            rg_track_gain: None,
            rg_track_peak: None,
            rg_album_gain: None,
            rg_album_peak: None,
            sort_keys: SortKeys::default(),
        };

        let track = Track::from(&lib);

        assert_eq!(track.path, PathBuf::from(&tmp_path));
        assert_eq!(track.title, "Test Title");
        assert_eq!(track.artist, "Test Artist");
        assert_eq!(track.album, "Test Album");
        // This test was named "copies_all_fields" while passing an
        // album_artist of None, so it never noticed that the conversion
        // dropped the field — leaving it to be backfilled by a background
        // file read, and left empty entirely on paths that skip one.
        assert_eq!(track.album_artist, "Test Album Artist");
        assert_eq!(track.duration, Some(Duration::from_secs_f64(180.5)));
        assert!(!track.broken);
    }

    /// `display_name` falls back to the album artist when there is no artist,
    /// which is the same rule the media library applies to its own rows. While
    /// the conversion dropped album_artist, library rows silently lost that
    /// fallback and showed a bare title.
    #[test]
    fn track_from_libtrack_keeps_the_album_artist_display_fallback() {
        use crate::media_library::{LibTrack, SortKeys};
        let lib = LibTrack {
            path: "/music/a.mp3".into(),
            filename: "a.mp3".into(),
            title: Some("Song".into()),
            artist: None,
            album_artist: Some("Various Artists".into()),
            ..LibTrack {
                id: 0,
                path: String::new(),
                artist: None,
                title: None,
                album: None,
                track_num: None,
                genre: None,
                year: None,
                bpm: None,
                length_secs: None,
                bitrate: None,
                channels: None,
                filetype: None,
                filename: String::new(),
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
                sample_rate: None,
                file_size: None,
                file_mtime: None,
                added_at: None,
                bitrate_mode: None,
                rg_track_gain: None,
                rg_track_peak: None,
                rg_album_gain: None,
                rg_album_peak: None,
                sort_keys: SortKeys::default(),
            }
        };
        assert_eq!(Track::from(&lib).display_name(), "Various Artists - Song");
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
            sample_rate: None,
            file_size: None,
            file_mtime: None,
            added_at: None,
            bitrate_mode: None,
            rg_track_gain: None,
            rg_track_peak: None,
            rg_album_gain: None,
            rg_album_peak: None,
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
            read_only: false,
            id: 0,
        }
    }

    #[test]
    fn add_assigns_distinct_ids_even_for_duplicate_paths() {
        let mut pl = Playlist::new();
        pl.add(make_track("same"));
        pl.add(make_track("same"));
        assert_eq!(pl.tracks.len(), 2);
        assert_ne!(
            pl.tracks[0].id, pl.tracks[1].id,
            "duplicates get distinct ids"
        );
        assert_ne!(pl.tracks[0].id, 0, "id 0 is the unstamped sentinel");
    }

    #[test]
    fn assign_ids_restamps_all_entries_uniquely() {
        let mut pl = Playlist::new();
        for _ in 0..3 {
            pl.tracks.push(make_track("raw")); // raw push, unstamped (id 0)
        }
        pl.assign_ids();
        let ids: std::collections::HashSet<u64> = pl.tracks.iter().map(|t| t.id).collect();
        assert_eq!(ids.len(), 3, "all ids distinct");
        assert!(!ids.contains(&0), "no unstamped entries remain");
    }

    #[test]
    fn ensure_ids_only_stamps_zero_id_entries() {
        let mut pl = Playlist::new();
        pl.add(make_track("kept")); // stamped id 1
        let kept_id = pl.tracks[0].id;
        pl.tracks.push(make_track("raw")); // unstamped id 0
        pl.ensure_ids();
        assert_eq!(pl.tracks[0].id, kept_id, "already-stamped id untouched");
        assert_ne!(pl.tracks[1].id, 0, "zero-id entry stamped");
        assert_ne!(pl.tracks[1].id, kept_id, "new id is distinct");
    }

    fn playlist_of(titles: &[&str]) -> Playlist {
        let mut p = Playlist::new();
        for t in titles {
            p.add(make_track(t));
        }
        p
    }

    fn track_named(id: u64, title: &str, path: &str) -> Track {
        Track {
            path: PathBuf::from(path),
            title: title.to_string(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            duration: None,
            broken: false,
            read_only: false,
            id,
        }
    }

    // -----------------------------------------------------------------------
    // sort_by()
    // -----------------------------------------------------------------------

    #[test]
    fn sort_by_title_is_case_insensitive_and_stable_with_filename_fallback() {
        let mut pl = Playlist::new();
        pl.add(track_named(1, "banana", "/z/1.mp3"));
        pl.add(track_named(2, "Apple", "/a/2.mp3"));
        pl.add(track_named(3, "", "/m/aaa.mp3")); // blank title → "aaa.mp3"
        pl.sort_by(SortKey::Title);
        let order: Vec<u64> = pl.tracks.iter().map(|t| t.id).collect();
        assert_eq!(order, vec![3, 2, 1]); // aaa.mp3, apple, banana
    }

    #[test]
    fn sort_keeps_the_playing_track_current() {
        let mut pl = Playlist::new();
        pl.add(track_named(1, "banana", "/1.mp3"));
        pl.add(track_named(2, "apple", "/2.mp3"));
        pl.jump_to(0); // playing "banana"
        pl.sort_by(SortKey::Title);
        assert_eq!(pl.current_id(), Some(1), "still on banana after sort");
        assert_eq!(pl.current_index, 1, "banana moved to the end");
    }

    /// Entries that arrive by a direct `tracks.push` rather than through `add`
    /// carry the id-0 sentinel — the mac dedupe and Media Library bulk adds and
    /// GTK's drag-drop and disc-track adds all do this. A reorder finds the
    /// playing track again by id, so before this was fixed the lookup matched
    /// id 0 against the FIRST unstamped row and moved the playing highlight
    /// (and the next auto-advance) onto a track the user was not listening to.
    #[test]
    fn sort_keeps_the_playing_track_when_entries_were_never_stamped() {
        let mut pl = Playlist::new();
        for (title, path) in [
            ("delta", "/4.mp3"),
            ("charlie", "/3.mp3"),
            ("bravo", "/2.mp3"),
        ] {
            pl.tracks.push(track_named(0, title, path)); // id 0, as bulk adds leave it
        }
        pl.jump_to(2); // playing "bravo", last row
        assert_eq!(pl.tracks[pl.current_index].title, "bravo");

        pl.sort_by(SortKey::Title);

        assert_eq!(
            pl.tracks[pl.current_index].title, "bravo",
            "playing track still current after sorting unstamped entries"
        );
        assert_eq!(pl.current_index, 0, "bravo sorted to the front");
        assert!(
            pl.tracks.iter().all(|t| t.id != 0),
            "the reorder stamped every entry on its way through"
        );
    }

    // -----------------------------------------------------------------------
    // reverse() / randomize()
    // -----------------------------------------------------------------------

    #[test]
    fn reverse_twice_is_identity_and_keeps_current() {
        let mut pl = Playlist::new();
        for i in 0..4 {
            pl.add(track_named(i, &format!("t{i}"), &format!("/{i}.mp3")));
        }
        pl.jump_to(1);
        // `Playlist::add` stamps a fresh sequential id on append (ignoring the
        // id passed into `track_named`), so capture the real playing id here
        // rather than assuming it equals the loop index.
        let playing = pl.current_id();
        let before: Vec<u64> = pl.tracks.iter().map(|t| t.id).collect();
        pl.reverse();
        pl.reverse();
        let after: Vec<u64> = pl.tracks.iter().map(|t| t.id).collect();
        assert_eq!(before, after);
        assert_eq!(pl.current_id(), playing);
    }

    #[test]
    fn randomize_preserves_membership_and_current() {
        let mut pl = Playlist::new();
        for i in 0..20 {
            pl.add(track_named(i, &format!("t{i}"), &format!("/{i}.mp3")));
        }
        pl.jump_to(7);
        // Same rationale as reverse_twice_is_identity_and_keeps_current: the
        // playing id is whatever `add` actually stamped, not the loop index.
        let playing = pl.current_id();
        let before: std::collections::HashSet<u64> = pl.tracks.iter().map(|t| t.id).collect();
        pl.randomize();
        let after: std::collections::HashSet<u64> = pl.tracks.iter().map(|t| t.id).collect();
        assert_eq!(before, after, "same multiset of tracks");
        assert_eq!(pl.current_id(), playing, "playing track still current");
    }

    // -----------------------------------------------------------------------
    // is_audio_file()
    // -----------------------------------------------------------------------

    /// `fmt_secs` is the seconds-taking face of `fmt_duration`; four library
    /// views open-coded this. It must agree with its sibling exactly —
    /// `fmt_duration_none_returns_placeholder` above already pins the absent
    /// case, so this is about the conversion.
    #[test]
    fn fmt_secs_agrees_with_fmt_duration() {
        for secs in [0.0f64, 1.0, 59.9, 60.0, 61.4, 599.99, 3600.0, 7325.5] {
            assert_eq!(
                fmt_secs(Some(secs)),
                fmt_duration(Some(Duration::from_secs_f64(secs))),
                "mismatch at {secs}s"
            );
        }
        assert_eq!(fmt_secs(None), fmt_duration(None));
    }

    /// A length that cannot be a duration reads as unknown rather than
    /// panicking — `Duration::from_secs_f64` panics on both of these, and a
    /// corrupt tag should not take the window down.
    #[test]
    fn fmt_secs_treats_an_impossible_length_as_unknown() {
        assert_eq!(fmt_secs(Some(-1.0)), "-:--");
        assert_eq!(fmt_secs(Some(f64::NAN)), "-:--");
    }

    #[test]
    fn is_audio_file_recognises_mp3() {
        assert!(is_audio_file(Path::new("song.mp3")));
    }

    #[test]
    fn is_audio_file_is_case_insensitive() {
        assert!(is_audio_file(Path::new("TRACK.MP3")));
        assert!(is_audio_file(Path::new("album.Flac")));
    }

    /// The formats every platform decodes. If one of these ever drops off
    /// AUDIO_EXTENSIONS, a user's library silently loses rows.
    #[test]
    fn the_formats_that_work_everywhere_are_always_recognised() {
        for ext in ["mp3", "flac", "ogg", "opus", "wav", "aac", "m4a", "aiff"] {
            assert!(
                is_audio_file(Path::new(&format!("track.{ext}"))),
                "{ext} must be scannable on every platform"
            );
        }
    }

    /// CoreAudio cannot open these containers, so a scan that added them would
    /// fill the library with rows that can never play and burn lists that
    /// refuse themselves at transcode time. Measured on macOS 26, 2026-09-05:
    /// each fails at AudioFileOpenURL with kAudioFileUnsupportedFileTypeError.
    ///
    /// They stay scannable on Linux, where GStreamer decodes all of them, so
    /// this asserts in both directions rather than only removing.
    #[test]
    fn containers_coreaudio_cannot_open_are_skipped_on_macos_only() {
        for ext in ["wma", "tta", "wv", "ape"] {
            let p = format!("track.{ext}");
            #[cfg(target_os = "macos")]
            assert!(
                !is_audio_file(Path::new(&p)),
                "{ext} must not be scanned on macOS: CoreAudio refuses to open it"
            );
            #[cfg(not(target_os = "macos"))]
            assert!(
                is_audio_file(Path::new(&p)),
                "{ext} must stay scannable off macOS, where GStreamer decodes it"
            );
        }
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

    /// Titles in order — the existing move tests spell this out inline; the
    /// block-move cases below compare whole orderings often enough to name it.
    fn titles(p: &Playlist) -> Vec<&str> {
        p.tracks.iter().map(|t| t.title.as_str()).collect()
    }

    // move_tracks() — multi-row block reorder
    // -----------------------------------------------------------------------

    /// One row behaves exactly like `move_track`, so the block path can be
    /// used for every drag without special-casing a single selection.
    #[test]
    fn move_tracks_of_one_matches_move_track() {
        let mut a = playlist_of(&["A", "B", "C", "D"]);
        let mut b = playlist_of(&["A", "B", "C", "D"]);
        a.move_tracks(&[0], 2);
        b.move_track(0, 1);
        assert_eq!(titles(&a), titles(&b));
    }

    /// A contiguous block keeps its internal order when moved down.
    #[test]
    fn move_tracks_moves_a_block_down_keeping_its_order() {
        let mut pl = playlist_of(&["A", "B", "C", "D", "E"]);
        // Take A,B and drop them before E.
        let moved = pl.move_tracks(&[0, 1], 4);
        assert_eq!(titles(&pl), ["C", "D", "A", "B", "E"]);
        assert_eq!(moved, Some((2, 2)), "the block's landing spot is reported");
    }

    /// And when moved up.
    #[test]
    fn move_tracks_moves_a_block_up_keeping_its_order() {
        let mut pl = playlist_of(&["A", "B", "C", "D", "E"]);
        // Take D,E and drop them at the top.
        let moved = pl.move_tracks(&[3, 4], 0);
        assert_eq!(titles(&pl), ["D", "E", "A", "B", "C"]);
        assert_eq!(moved, Some((0, 2)));
    }

    /// A non-contiguous selection collapses into one block at the drop point,
    /// in the order the rows appear — not the order they were clicked.
    #[test]
    fn move_tracks_collapses_a_scattered_selection_into_one_block() {
        let mut pl = playlist_of(&["A", "B", "C", "D", "E"]);
        let moved = pl.move_tracks(&[4, 0, 2], 2);
        assert_eq!(titles(&pl), ["B", "A", "C", "E", "D"]);
        assert_eq!(moved, Some((1, 3)));
    }

    /// The playing track keeps playing when it is one of the moved rows.
    #[test]
    fn move_tracks_keeps_the_playing_track_current_when_it_moves() {
        let mut pl = playlist_of(&["A", "B", "C", "D"]);
        pl.ensure_ids();
        pl.current_index = 0; // A
        pl.move_tracks(&[0, 1], 4);
        assert_eq!(titles(&pl), ["C", "D", "A", "B"]);
        assert_eq!(pl.tracks[pl.current_index].title, "A");
    }

    /// And when it is merely displaced by them.
    #[test]
    fn move_tracks_keeps_the_playing_track_current_when_displaced() {
        let mut pl = playlist_of(&["A", "B", "C", "D"]);
        pl.ensure_ids();
        pl.current_index = 3; // D
        pl.move_tracks(&[0, 1], 4);
        assert_eq!(pl.tracks[pl.current_index].title, "D");
    }

    /// Out-of-range rows are dropped rather than panicking, and duplicates in
    /// the selection move the row once.
    #[test]
    fn move_tracks_ignores_bad_indices_and_duplicates() {
        let mut pl = playlist_of(&["A", "B", "C"]);
        assert_eq!(pl.move_tracks(&[1, 1, 99], 0), Some((0, 1)));
        assert_eq!(titles(&pl), ["B", "A", "C"]);
        assert_eq!(pl.move_tracks(&[], 0), None, "nothing to move");
        assert_eq!(pl.move_tracks(&[42], 0), None, "no valid row");
    }

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

    fn tags_titled(title: &str) -> ScannedTags {
        ScannedTags {
            title: title.to_string(),
            ..ScannedTags::default()
        }
    }

    /// Tags land on the entry they were read for even after a row above it
    /// was removed mid-scan. Written by row, they went to its neighbour.
    #[test]
    fn scanned_tags_follow_the_entry_when_rows_move() {
        let mut p = playlist_of(&["old", "new1", "new2"]);
        let (id1, id2) = (p.tracks[1].id, p.tracks[2].id);
        p.remove(0);
        assert_eq!(p.apply_scanned_tags(id1, 1, tags_titled("One")), Some(0));
        assert_eq!(p.apply_scanned_tags(id2, 2, tags_titled("Two")), Some(1));
        let titles: Vec<&str> = p.tracks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["One", "Two"]);
    }

    #[test]
    fn scanned_tags_for_a_removed_entry_are_dropped() {
        let mut p = playlist_of(&["a", "b"]);
        let id = p.tracks[1].id;
        p.remove(1);
        assert_eq!(p.apply_scanned_tags(id, 1, tags_titled("B")), None);
        assert_eq!(p.tracks[0].title, "a", "the remaining row is untouched");
    }

    /// A link back to the folder itself is walked once, so each file is
    /// listed once. It used to be walked again at every level, ~40 deep.
    #[cfg(unix)]
    #[test]
    fn collect_audio_files_lists_each_file_once_through_a_link_to_itself() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.mp3"), b"x").unwrap();
        std::os::unix::fs::symlink(".", dir.path().join("again")).unwrap();
        let files = Playlist::collect_audio_files(dir.path());
        assert_eq!(files, vec![dir.path().join("a.mp3")]);
    }

    /// A link from a subfolder back up to its parent is a loop too.
    #[cfg(unix)]
    #[test]
    fn collect_audio_files_stops_at_a_link_back_up_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(dir.path().join("a.mp3"), b"x").unwrap();
        std::fs::write(sub.join("b.mp3"), b"x").unwrap();
        std::os::unix::fs::symlink("..", sub.join("up")).unwrap();
        let files = Playlist::collect_audio_files(dir.path());
        assert_eq!(files.len(), 2, "{files:?}");
    }

    /// Two links into one loop made the walk branch at every level: 2^40
    /// paths, which never finishes. This returns at once.
    #[cfg(unix)]
    #[test]
    fn collect_audio_files_finishes_with_two_links_into_a_loop() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.mp3"), b"x").unwrap();
        std::os::unix::fs::symlink(".", dir.path().join("one")).unwrap();
        std::os::unix::fs::symlink(".", dir.path().join("two")).unwrap();
        assert_eq!(Playlist::collect_audio_files(dir.path()).len(), 1);
    }

    /// Two links to the same folder list it once, not twice. A linked
    /// folder elsewhere on disk is still walked.
    #[cfg(unix)]
    #[test]
    fn collect_audio_files_follows_a_linked_folder_once() {
        let music = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("linked.mp3"), b"x").unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), music.path().join("Disk A")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), music.path().join("Disk B")).unwrap();
        let files = Playlist::collect_audio_files(music.path());
        assert_eq!(files, vec![music.path().join("Disk A").join("linked.mp3")]);
    }

    // -----------------------------------------------------------------------
    // Playlist::scan_files_for_ui() and scan_folder_for_ui()
    //
    // These share the same scan_paths_in_thread core.  scan_files_for_ui is
    // tested directly (simpler setup); one test verifies scan_folder_for_ui
    // discovers files via directory walk.
    // -----------------------------------------------------------------------

    /// Everything a finished scan sent, in order. Panics if it never finishes.
    fn drain_scan(rx: std::sync::mpsc::Receiver<ScanEvent>) -> Vec<ScanEvent> {
        let mut events = Vec::new();
        loop {
            let ev = rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("scan did not complete");
            let done = matches!(ev, ScanEvent::Done);
            events.push(ev);
            if done {
                return events;
            }
        }
    }

    fn found(events: &[ScanEvent]) -> Vec<&Track> {
        events
            .iter()
            .filter_map(|e| match e {
                ScanEvent::Found(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    fn tags(events: &[ScanEvent]) -> Vec<(usize, &str)> {
        events
            .iter()
            .filter_map(|e| match e {
                ScanEvent::Tags { index, tags } => Some((*index, tags.title.as_str())),
                _ => None,
            })
            .collect()
    }

    fn fake_files(dir: &Path, names: &[&str]) -> Vec<PathBuf> {
        names
            .iter()
            .map(|name| {
                let p = dir.join(name);
                std::fs::write(&p, b"fake").unwrap();
                p
            })
            .collect()
    }

    fn no_cancel() -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))
    }

    #[test]
    fn scan_files_for_ui_empty_input_completes_with_nothing_found() {
        let events = drain_scan(Playlist::scan_files_for_ui(vec![], no_cancel()));
        assert!(matches!(events[..], [ScanEvent::AllFound, ScanEvent::Done]));
    }

    #[test]
    fn scan_files_for_ui_sends_one_found_track_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = fake_files(dir.path(), &["a.mp3", "b.mp3", "c.mp3"]);
        let events = drain_scan(Playlist::scan_files_for_ui(paths, no_cancel()));
        assert_eq!(found(&events).len(), 3);
    }

    /// The order is the contract a receiver relies on: every `Found`, then
    /// `AllFound`, then the tags, then `Done` last.
    #[test]
    fn scan_events_arrive_in_their_documented_order() {
        let dir = tempfile::tempdir().unwrap();
        let paths = fake_files(dir.path(), &["1_track.mp3", "2_track.mp3", "3_track.mp3"]);
        let events = drain_scan(Playlist::scan_files_for_ui(paths, no_cancel()));
        let phase = |e: &ScanEvent| match e {
            ScanEvent::Found(_) => 0,
            ScanEvent::AllFound => 1,
            ScanEvent::Tags { .. } => 2,
            ScanEvent::Done => 3,
        };
        let phases: Vec<u8> = events.iter().map(phase).collect();
        assert_eq!(phases, vec![0, 0, 0, 1, 2, 2, 2, 3]);
        let indices: Vec<usize> = tags(&events).iter().map(|(i, _)| *i).collect();
        assert_eq!(indices, vec![0, 1, 2]);
    }

    #[test]
    fn scan_tags_index_addresses_the_matching_found_track() {
        let dir = tempfile::tempdir().unwrap();
        let paths = fake_files(dir.path(), &["alpha.mp3", "beta.mp3"]);
        let events = drain_scan(Playlist::scan_files_for_ui(paths, no_cancel()));
        let found = found(&events);
        for (index, title) in tags(&events) {
            // Both the fast and the full read fall back to the filename stem
            // for these tagless files, so the titles must match.
            assert_eq!(
                found[index].title, title,
                "tags {index} belong to found {index}"
            );
        }
    }

    #[test]
    fn scan_files_for_ui_cancel_stops_before_tags_and_still_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let names: Vec<String> = (0..10).map(|i| format!("{i:02}.mp3")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let paths = fake_files(dir.path(), &names);
        let cancel = no_cancel();
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        let events = drain_scan(Playlist::scan_files_for_ui(paths, cancel));
        assert!(found(&events).is_empty(), "cancelled before the first file");
        assert!(tags(&events).is_empty(), "no tags after a cancel");
        assert!(matches!(events.last(), Some(ScanEvent::Done)));
    }

    #[test]
    fn scan_folder_for_ui_empty_folder_completes_with_nothing_found() {
        let dir = tempfile::tempdir().unwrap();
        let events = drain_scan(Playlist::scan_folder_for_ui(
            dir.path().to_path_buf(),
            no_cancel(),
        ));
        assert!(found(&events).is_empty());
        assert!(tags(&events).is_empty());
    }

    #[test]
    fn scan_folder_for_ui_discovers_audio_files_in_directory() {
        let dir = tempfile::tempdir().unwrap();
        fake_files(dir.path(), &["song1.mp3", "song2.flac", "cover.jpg"]);
        let events = drain_scan(Playlist::scan_folder_for_ui(
            dir.path().to_path_buf(),
            no_cancel(),
        ));
        let found = found(&events);
        assert_eq!(found.len(), 2, "only the two audio files should be scanned");
        assert_eq!(tags(&events).len(), 2);
        assert!(
            found.iter().all(|t| !t.title.contains("cover")),
            "cover.jpg must not appear as a track"
        );
    }
}
