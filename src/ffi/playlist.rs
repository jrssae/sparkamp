//! Playlist manipulation, background metadata scanning, and the playlist
//! path accessor.
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_double, c_int};
use std::path::Path;

use crate::model::Track;

use super::SparkampCtx;

// ---------------------------------------------------------------------------
// Playlist
// ---------------------------------------------------------------------------

/// Add an audio file or folder (recursively scanned) to the playlist.
///
/// Uses the full `Track::from_path` path — reads ID3 tags synchronously.
/// Prefer `sparkamp_playlist_add_fast` when adding many files and following
/// up with `sparkamp_scan_metadata` to fill tags in the background.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_add(ctx: *mut SparkampCtx, path: *const c_char) {
    if ctx.is_null() || path.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let s = CStr::from_ptr(path).to_string_lossy();
    let p = Path::new(s.as_ref());
    if p.is_dir() {
        ctx.playlist.add_paths(&[p]);
    } else if let Ok(track) = Track::from_path(p) {
        ctx.playlist.add(track);
    }
}

/// Fast-add a single audio file to the playlist using only the filename as a
/// temporary title (no disk I/O beyond path validation).
///
/// Returns the 0-based playlist index of the newly added track, or -1 on
/// failure (file not found, not audio, etc.).  Immediately call
/// `sparkamp_scan_metadata` and `sparkamp_probe_duration` on the returned
/// index to fill in real tags and duration in the background.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_add_fast(
    ctx: *mut SparkampCtx,
    path: *const c_char,
) -> c_int {
    if ctx.is_null() || path.is_null() {
        return -1;
    }
    let ctx = &mut *ctx;
    let s = CStr::from_ptr(path).to_string_lossy();
    let p = Path::new(s.as_ref());
    match Track::from_path_fast(p) {
        Ok(track) => {
            let idx = ctx.playlist.tracks.len() as c_int;
            ctx.playlist.add(track);
            idx
        }
        Err(_) => -1,
    }
}

/// Add a playlist entry with caller-supplied metadata and a known duration —
/// used for disc tracks, whose display data ("Track N" or gnudb tags) and
/// duration come from the TOC rather than tags on the file. `path` may be a
/// plain file path (macOS mounted AIFF) or a `cdda://` pseudo-URI (Linux);
/// no tag read or duration probe is performed. `artist`/`album` may be null
/// or empty (the playlist then shows the bare title).
///
/// Returns the 0-based playlist index of the new entry, or -1 on bad input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_add_entry(
    ctx: *mut SparkampCtx,
    path: *const c_char,
    title: *const c_char,
    artist: *const c_char,
    album: *const c_char,
    duration_secs: c_int,
) -> c_int {
    if ctx.is_null() || path.is_null() || title.is_null() {
        return -1;
    }
    let ctx = &mut *ctx;
    let path = CStr::from_ptr(path).to_string_lossy().into_owned();
    let title = CStr::from_ptr(title).to_string_lossy().into_owned();
    let opt = |p: *const c_char| {
        if p.is_null() {
            String::new()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    };
    if path.is_empty() || title.is_empty() {
        return -1;
    }
    let track = Track {
        path: std::path::PathBuf::from(path),
        title,
        artist: opt(artist),
        album_artist: String::new(),
        album: opt(album),
        duration: (duration_secs > 0)
            .then(|| std::time::Duration::from_secs(duration_secs as u64)),
        broken: false,
        read_only: true, // disc media is never writable in place
        id: 0,
    };
    let idx = ctx.playlist.tracks.len() as c_int;
    ctx.playlist.add(track);
    idx
}

/// Synchronously re-read tags for every playlist row holding `path` and
/// update those rows in place. Paths are compared canonically (both sides
/// canonicalized), so callers holding a differently-spelled path to the same
/// file (Media Library row vs playlist row) still match. The file was
/// typically just written by the tag editor, so one synchronous read is
/// cheap and the caller can refresh its view immediately after.
///
/// Returns how many rows were updated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_rescan_path(
    ctx: *mut SparkampCtx,
    path: *const c_char,
) -> c_int {
    if ctx.is_null() || path.is_null() {
        return 0;
    }
    let ctx = &mut *ctx;
    let raw = CStr::from_ptr(path).to_string_lossy();
    rescan_rows_by_path(&mut ctx.playlist.tracks, &raw) as c_int
}

/// The path-matching + tag-refresh core of `sparkamp_playlist_rescan_path`,
/// separated so it's directly unit-testable against real temp files.
fn rescan_rows_by_path(tracks: &mut [Track], raw: &str) -> usize {
    if raw.is_empty() {
        return 0;
    }
    let target = Path::new(raw)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(raw));

    let mut fresh: Option<Track> = None;
    let mut updated = 0;
    for track in tracks {
        let row = track
            .path
            .canonicalize()
            .unwrap_or_else(|_| track.path.clone());
        if row != target {
            continue;
        }
        if fresh.is_none() {
            fresh = Track::from_path(&target).ok();
        }
        let Some(f) = &fresh else { break };
        track.title = f.title.clone();
        track.artist = f.artist.clone();
        track.album_artist = f.album_artist.clone();
        track.album = f.album.clone();
        updated += 1;
    }
    updated
}

/// Update the display metadata of every playlist entry whose path equals
/// `path` — used when a disc's tags are edited so already-added rows change
/// immediately (disc entries share exact path strings with the drive view).
/// Empty/null `artist`/`album` clear those fields; `title` must be non-empty.
///
/// Returns how many rows were updated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_update_entry_meta(
    ctx: *mut SparkampCtx,
    path: *const c_char,
    title: *const c_char,
    artist: *const c_char,
    album: *const c_char,
) -> c_int {
    if ctx.is_null() || path.is_null() || title.is_null() {
        return 0;
    }
    let ctx = &mut *ctx;
    let path = CStr::from_ptr(path).to_string_lossy().into_owned();
    let title = CStr::from_ptr(title).to_string_lossy().into_owned();
    let opt = |p: *const c_char| {
        if p.is_null() {
            String::new()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    };
    if path.is_empty() || title.is_empty() {
        return 0;
    }
    let artist = opt(artist);
    let album = opt(album);
    let mut updated = 0;
    for track in &mut ctx.playlist.tracks {
        if track.path.display().to_string() == path {
            track.title = title.clone();
            track.artist = artist.clone();
            track.album = album.clone();
            updated += 1;
        }
    }
    updated
}

/// Remove all tracks from the playlist.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_clear(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    (*ctx).playlist.clear();
}

/// Remove the track at `index` from the playlist.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_remove(ctx: *mut SparkampCtx, index: c_int) {
    if ctx.is_null() {
        return;
    }
    (*ctx).playlist.remove(index as usize);
}

/// Move the track at `from` to position `to` (drag-reorder).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_move(
    ctx: *mut SparkampCtx,
    from: c_int,
    to: c_int,
) {
    if ctx.is_null() {
        return;
    }
    (*ctx).playlist.move_track(from as usize, to as usize);
}

/// Return the number of tracks in the playlist.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_len(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    (*ctx).playlist.len() as c_int
}

/// Return the index of the currently selected track, or -1 if the playlist is empty.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_current_index(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return -1;
    }
    let ctx = &*ctx;
    if ctx.playlist.is_empty() {
        -1
    } else {
        ctx.playlist.current_index as c_int
    }
}

/// Return the title of the track at `index`. The caller must free the string
/// with `sparkamp_free_string`. Returns null if `index` is out of range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_get_title(
    ctx: *const SparkampCtx,
    index: c_int,
) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let i = index as usize;
    if i >= ctx.playlist.tracks.len() {
        return std::ptr::null_mut();
    }
    CString::new(ctx.playlist.tracks[i].title.as_str())
        .unwrap_or_default()
        .into_raw()
}

/// Return the artist of the track at `index`. Caller must free with
/// `sparkamp_free_string`. Returns null if `index` is out of range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_get_artist(
    ctx: *const SparkampCtx,
    index: c_int,
) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let i = index as usize;
    if i >= ctx.playlist.tracks.len() {
        return std::ptr::null_mut();
    }
    CString::new(ctx.playlist.tracks[i].artist.as_str())
        .unwrap_or_default()
        .into_raw()
}

/// Return the album artist (TPE2) of the track at `index`. Caller must free with
/// `sparkamp_free_string`. Returns null if `index` is out of range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_get_album_artist(
    ctx: *const SparkampCtx,
    index: c_int,
) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let i = index as usize;
    if i >= ctx.playlist.tracks.len() {
        return std::ptr::null_mut();
    }
    CString::new(ctx.playlist.tracks[i].album_artist.as_str())
        .unwrap_or_default()
        .into_raw()
}

/// Return the duration of the track at `index` in seconds, or -1 if unknown.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_get_duration(
    ctx: *const SparkampCtx,
    index: c_int,
) -> c_double {
    if ctx.is_null() {
        return -1.0;
    }
    let ctx = &*ctx;
    let i = index as usize;
    if i >= ctx.playlist.tracks.len() {
        return -1.0;
    }
    ctx.playlist.tracks[i]
        .duration
        .map(|d| d.as_secs_f64())
        .unwrap_or(-1.0)
}

/// Mark the track at `index` as broken (file missing or unreadable).
///
/// Broken tracks are skipped by navigation and shown with an error indicator
/// in the playlist.  Call this from the error callback before advancing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_mark_broken(ctx: *mut SparkampCtx, index: c_int) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let i = index as usize;
    if let Some(track) = ctx.playlist.tracks.get_mut(i) {
        track.broken = true;
    }
}

/// Return 1 if the track at `index` is marked broken (file missing or unreadable),
/// 0 otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_is_broken(
    ctx: *const SparkampCtx,
    index: c_int,
) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    let i = index as usize;
    if i >= ctx.playlist.tracks.len() {
        return 0;
    }
    ctx.playlist.tracks[i].broken as c_int
}

/// Returns 1 if the file at `index` is read-only on disk, 0 otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_is_read_only(
    ctx: *const SparkampCtx,
    index: c_int,
) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    let i = index as usize;
    if i >= ctx.playlist.tracks.len() {
        return 0;
    }
    let path = std::path::Path::new(&ctx.playlist.tracks[i].path);
    if crate::media_library::is_read_only(path) { 1 } else { 0 }
}

/// Jump to `index`, load the track, and begin playing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_jump(ctx: *mut SparkampCtx, index: c_int) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.last_known_duration = None;
    if ctx.playlist.jump_to(index as usize).is_some() {
        let uri = ctx.playlist.current().map(|t| t.uri()).unwrap_or_default();
        ctx.player.load(&uri).ok();
        ctx.player.play().ok();
        let idx = index as usize;
        ctx.shuffle_state.record_played(idx);
    }
}

// ---------------------------------------------------------------------------
// Background metadata scanning
// ---------------------------------------------------------------------------

/// Scan full ID3/Vorbis metadata for the track at `index` on a Rayon worker
/// thread.  When done, queues `(index, title, artist, album_artist)` into
/// `pending_metadata`; the next `sparkamp_tick` call applies it to the
/// playlist and increments `dirty_count`.
///
/// Call immediately after `sparkamp_playlist_add` for each newly added track
/// so the quick-added filename placeholder is replaced by real tag data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_scan_metadata(ctx: *mut SparkampCtx, index: c_int) {
    if ctx.is_null() {
        return;
    }
    let ctx = &*ctx;
    let i = index as usize;
    if i >= ctx.playlist.tracks.len() {
        return;
    }
    let path = ctx.playlist.tracks[i].path.clone();
    let tx = ctx.meta_tx.clone();
    rayon::spawn(move || {
        if let Ok(track) = crate::model::Track::from_path(&path) {
            let _ = tx.send((i, track.title, track.artist, track.album_artist));
        }
    });
}

/// Return the number of playlist updates applied by `sparkamp_tick` since the
/// last call to this function, then reset the counter to zero.
///
/// A non-zero return means at least one track's title, artist, or duration
/// changed — Swift should re-read the affected items and refresh the playlist
/// display.  Returns 0 when no background work is pending.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_take_playlist_dirty_count(ctx: *mut SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &mut *ctx;
    let n = ctx.dirty_count as c_int;
    ctx.dirty_count = 0;
    n
}

// ---------------------------------------------------------------------------
// Playlist path accessor
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_get_path(
    ctx: *const SparkampCtx,
    index: c_int,
) -> *mut c_char {
    if ctx.is_null() || index < 0 {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let idx = index as usize;
    if idx >= ctx.playlist.tracks.len() {
        return std::ptr::null_mut();
    }
    let path_str = ctx.playlist.tracks[idx].path.to_string_lossy().into_owned();
    CString::new(path_str).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut())
}


#[cfg(test)]
mod tests {
    use super::*;

    /// The rescan must match rows canonically: an ML-spelled path (extra
    /// "./" segment here; symlinks in real life) still hits the playlist
    /// row, updates ALL duplicates, and re-reads tags from the file.
    #[test]
    fn rescan_rows_matches_canonically_and_updates_duplicates() {
        let dir = std::env::temp_dir().join(format!("sparkamp-rescan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("song.mp3");
        std::fs::write(&file, b"not really audio").unwrap();
        let canonical = file.canonicalize().unwrap();

        let make_row = || Track {
            path: canonical.clone(),
            title: "Stale".into(),
            artist: "Stale Artist".into(),
            album_artist: String::new(),
            album: "Stale Album".into(),
            duration: None,
            broken: false,
            read_only: false,
            id: 0,
        };
        let mut tracks = vec![make_row(), make_row()];

        // Differently-spelled path to the same file.
        let alt = format!("{}/./song.mp3", dir.display());
        let updated = rescan_rows_by_path(&mut tracks, &alt);
        assert_eq!(updated, 2);
        // No readable tags in the fake file → title falls back to the stem,
        // artist/album reset — proving the rows were rewritten from the file.
        assert_eq!(tracks[0].title, "song");
        assert!(tracks[0].artist.is_empty());
        assert_eq!(tracks[1].title, "song");

        // Non-matching path touches nothing.
        let other = dir.join("other.mp3");
        std::fs::write(&other, b"x").unwrap();
        tracks[0].title = "Keep".into();
        assert_eq!(
            rescan_rows_by_path(&mut tracks, &other.display().to_string()),
            0
        );
        assert_eq!(tracks[0].title, "Keep");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
