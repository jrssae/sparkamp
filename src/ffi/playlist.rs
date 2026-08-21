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

/// Move several rows to `dest` as one block, keeping their relative order.
///
/// `indices` is a pointer to `count` 0-based row indices in any order;
/// out-of-range and duplicate entries are ignored. `dest` is the insertion
/// slot in pre-move coordinates — the index the block should land before.
///
/// Returns the index the block landed at, or -1 when nothing moved. The
/// caller wants that to re-select what it just moved.
///
/// Exists because a multi-row drag is not a loop over
/// `sparkamp_playlist_move`: each single move shifts every later index, so
/// replaying one per row walks them into the wrong places. macOS's `moveTrack`
/// took a whole selection and moved only its first row.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_move_many(
    ctx: *mut SparkampCtx,
    indices: *const c_int,
    count: c_int,
    dest: c_int,
) -> c_int {
    if ctx.is_null() || indices.is_null() || count <= 0 || dest < 0 {
        return -1;
    }
    let ctx = &mut *ctx;
    let rows: Vec<usize> = std::slice::from_raw_parts(indices, count as usize)
        .iter()
        .filter(|&&i| i >= 0)
        .map(|&i| i as usize)
        .collect();
    match ctx.playlist.move_tracks(&rows, dest as usize) {
        Some((start, _)) => start as c_int,
        None => -1,
    }
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
    super::queue::sync_queue_to_playlist(&mut *ctx);
}

/// Remove the track at `index` from the playlist.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_remove(ctx: *mut SparkampCtx, index: c_int) {
    if ctx.is_null() {
        return;
    }
    (*ctx).playlist.remove(index as usize);
    super::queue::sync_queue_to_playlist(&mut *ctx);
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

/// Sort the active playlist (phase 7). kind: 0=Title 1=Artist 2=Album
/// 3=Filename 4=Path. Keeps the playing track current; resets shuffle history.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_sort(ctx: *mut SparkampCtx, kind: c_int) {
    if ctx.is_null() {
        return;
    }
    let key = match kind {
        0 => crate::model::SortKey::Title,
        1 => crate::model::SortKey::Artist,
        2 => crate::model::SortKey::Album,
        3 => crate::model::SortKey::Filename,
        _ => crate::model::SortKey::Path,
    };
    (*ctx).playlist.sort_by(key);
    (*ctx).shuffle_state.reset();
}

/// Reverse the active playlist (phase 7). Keeps the playing track current;
/// resets shuffle history.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_reverse(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    (*ctx).playlist.reverse();
    (*ctx).shuffle_state.reset();
}

/// Randomize the active playlist order (phase 7). Keeps the playing track
/// current; resets shuffle history.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_randomize(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    (*ctx).playlist.randomize();
    (*ctx).shuffle_state.reset();
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
    // Optical media is never writable in place, which is a fact rather than a
    // question — `sparkamp_playlist_add_entry` already records it on the track
    // for exactly this reason. Answering it here without the `access(2)` keeps
    // a playlist rebuild off the drive: this is called once PER ROW, on the UI
    // thread, and paired with `sparkamp_playlist_file_missing` it put two
    // syscalls per row on the optical mount every time the list was rebuilt.
    if crate::disc::detect::path_is_on_optical_media(path) {
        return 1;
    }
    if crate::media_library::is_read_only(path) { 1 } else { 0 }
}

/// Jump to `index`, load the track, and begin playing.
///
/// This is the frontend's only "play that track now" seam — the playlist
/// double-click, the jump window, the Media Library and the disc view all
/// funnel through it — so it is where a pending stop-after-current gets
/// cancelled, mirroring GTK's `AppState::play_current`. Clearing here instead
/// of at each Swift call site means a new caller cannot forget to do it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_jump(ctx: *mut SparkampCtx, index: c_int) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.last_known_duration = None;
    ctx.player.set_stop_after_current(false);
    if ctx.playlist.jump_to(index as usize).is_some() {
        let uri = ctx.playlist.current().map(|t| t.uri()).unwrap_or_default();
        super::prime_rg_for_current(ctx);
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
    let read = move || {
        if let Ok(track) = crate::model::Track::from_path(&path) {
            let _ = tx.send((i, track.title, track.artist, track.album_artist));
        }
    };
    // The bounded pool, not the global one. Callers invoke this once per path,
    // so replacing a playlist with a 40-file selection asked the global pool
    // for 40 concurrent file reads — one per rayon worker, every one of them
    // seeking on the same device. On an optical drive that is audible: the
    // head thrashes, the reads take minutes, and because the global pool also
    // serves every other `rayon::spawn` in the FFI, the rest of the app waits
    // behind them. See `duration_probe::PROBE_THREADS`.
    match crate::duration_probe::shared_probe_pool() {
        Some(pool) => pool.spawn(read),
        None => read(),
    }
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

// ---------------------------------------------------------------------------
// Playlist add behavior
// ---------------------------------------------------------------------------

/// Whether an add in `mode` should clear the playlist first.
///
/// `mode`: 0 = honour the user's setting, 1 = always append, 2 = always
/// replace. An unknown value is treated as 0.
///
/// Exists so the macOS frontend stops deciding for itself. `addFiles` used to
/// compare `sparkamp_get_playlist_add_behavior` against 1 inline, which was
/// correct but was a second copy of a rule that GTK also held five copies of.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_should_replace_on_add(
    ctx: *const SparkampCtx,
    mode: c_int,
) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    let mode = match mode {
        1 => crate::playlist_add::AddMode::Enqueue,
        2 => crate::playlist_add::AddMode::Replace,
        _ => crate::playlist_add::AddMode::Behavior,
    };
    crate::playlist_add::should_replace(&ctx.config.behavior.playlist_add_behavior, mode) as c_int
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

    /// `sparkamp_playlist_sort` (and reverse/randomize) must reset shuffle
    /// history at the FFI seam — the mac frontend has no other place to do
    /// this, unlike GTK/TUI which reset through the shared `Controller`.
    /// Locks in the phase 7 behaviour: after a sort, stale shuffle history
    /// (which would otherwise describe positions that no longer match the
    /// reordered rows) must be gone.
    #[test]
    fn playlist_sort_resets_shuffle_history() {
        let mut ctx = fake_ctx(3);
        // Seed non-empty shuffle history the same way real playback does
        // (`sparkamp_playlist_jump` → `ShuffleState::record_played`), but
        // directly here since we don't have a loadable URI for a fake path.
        ctx.shuffle_state.enabled = true;
        ctx.shuffle_state.record_played(0);
        ctx.shuffle_state.record_played(2);
        assert!(
            ctx.shuffle_state.has_history(),
            "fixture must start with non-empty shuffle history"
        );

        unsafe {
            sparkamp_playlist_sort(&mut ctx as *mut SparkampCtx, 0);
        }

        assert!(
            !ctx.shuffle_state.has_history(),
            "sort must reset shuffle history"
        );
    }

    /// Picking a track to play cancels a pending stop-after-current (phase 6).
    /// The clear lives in the FFI rather than in Swift because the Media
    /// Library and disc views call `sparkamp_playlist_jump` directly instead
    /// of going through the model's `jumpTo`, so a Swift-side clear would miss
    /// them. GTK gets the same behaviour from `AppState::play_current`.
    #[test]
    fn jumping_to_a_track_cancels_stop_after_current() {
        let mut ctx = fake_ctx(3);
        ctx.player.set_stop_after_current(true);

        unsafe {
            sparkamp_playlist_jump(&mut ctx as *mut SparkampCtx, 1);
        }

        assert!(
            !ctx.player.stop_after_current(),
            "choosing a track to play must clear the arming"
        );
    }

    /// `sparkamp_should_replace_on_add` maps its `mode` argument onto
    /// `playlist_add::AddMode` (0=Behavior, 1=Enqueue, 2=Replace, unknown→
    /// Behavior) and defers the actual decision to `playlist_add::should_replace`,
    /// which is unit-tested on its own. This locks in the mapping so the FFI
    /// seam can't silently drift from the C doc comment ("0 = honour setting,
    /// 1 = always append, 2 = always replace").
    #[test]
    fn should_replace_on_add_maps_mode_onto_add_mode() {
        let mut ctx = fake_ctx(1);

        ctx.config.behavior.playlist_add_behavior = crate::config::PlaylistAddBehavior::Replace;
        unsafe {
            // 0 = Behavior: follows the configured setting (Replace).
            assert_eq!(sparkamp_should_replace_on_add(&ctx, 0), 1);
            // 1 = Enqueue: always appends, even though the setting says Replace.
            assert_eq!(sparkamp_should_replace_on_add(&ctx, 1), 0);
            // 2 = Replace: always replaces.
            assert_eq!(sparkamp_should_replace_on_add(&ctx, 2), 1);
            // Unknown mode falls back to Behavior.
            assert_eq!(sparkamp_should_replace_on_add(&ctx, 99), 1);
        }

        ctx.config.behavior.playlist_add_behavior = crate::config::PlaylistAddBehavior::Append;
        unsafe {
            // 0 = Behavior now follows Append.
            assert_eq!(sparkamp_should_replace_on_add(&ctx, 0), 0);
            // 2 = Replace still always replaces, regardless of the setting.
            assert_eq!(sparkamp_should_replace_on_add(&ctx, 2), 1);
        }
    }

    /// A null `ctx` must not be dereferenced — returns 0 (append), matching
    /// every other read-only accessor's null-safety convention in this file.
    #[test]
    fn should_replace_on_add_null_ctx_returns_zero() {
        unsafe {
            assert_eq!(
                sparkamp_should_replace_on_add(std::ptr::null(), 2),
                0
            );
        }
    }

    /// A row on a disc must be answered from what is already known, never by
    /// asking the filesystem.
    ///
    /// Both getters are called once PER ROW on every playlist rebuild, on the
    /// UI thread, so together they put two syscalls per row on the optical
    /// mount every time the list was rebuilt.
    ///
    /// The optical case is driven through the mount-list fallback with a path
    /// that does not exist: `path_is_on_optical_media` asks `statfs` first and
    /// that is authoritative, so a real temp directory correctly answers
    /// "apfs" no matter what the test seeds. Only a path `statfs` cannot
    /// resolve reaches the seeded list. Proving the guard on a *real* optical
    /// file needs a disc in the drive — see `live_optical_row_is_read_only`.
    #[test]
    fn optical_rows_are_answered_without_touching_the_drive() {
        let _lock = crate::disc::detect::exclusive_read_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("1 Audio Track.aiff");
        std::fs::write(&present, b"x").expect("temp track");
        let absent = dir.path().join("gone.aiff");

        let mut ctx = fake_ctx(2);
        ctx.playlist.tracks[0].path = absent.clone();
        ctx.playlist.tracks[1].path = present.clone();

        // Ordinary storage: the honest answers, each costing a syscall.
        crate::disc::detect::set_optical_mounts_for_test(Vec::new());
        unsafe {
            assert_eq!(
                crate::ffi::media_library::sparkamp_playlist_file_missing(&ctx, 0),
                1,
                "a file that is not there reads as missing"
            );
            assert_eq!(
                sparkamp_playlist_is_read_only(&ctx, 1),
                0,
                "a writable file is not read-only"
            );
        }

        // The same absent row, now claimed by a mounted disc.
        crate::disc::detect::set_optical_mounts_for_test(vec![dir.path().to_path_buf()]);
        unsafe {
            assert_eq!(
                crate::ffi::media_library::sparkamp_playlist_file_missing(&ctx, 0),
                0,
                "the mount is what makes a disc row visible; losing it loses the volume"
            );
        }

        // A real file on real storage is never claimed, whatever is seeded —
        // `statfs` overrules the list, which is what stops the guard from
        // swallowing genuine answers.
        unsafe {
            assert_eq!(
                sparkamp_playlist_is_read_only(&ctx, 1),
                0,
                "statfs says apfs, so this row is not on a disc"
            );
        }
        crate::disc::detect::set_optical_mounts_for_test(Vec::new());
    }

    /// The read-only half against a real disc, which is the only place a
    /// writable-looking file can genuinely sit on optical media.
    ///
    /// `cargo test --lib live_optical_row_is_read_only -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_optical_row_is_read_only() {
        let _lock = crate::disc::detect::exclusive_read_test_guard();
        let drives = crate::disc::detect::list_drives_shared();
        let Some(d) = drives.iter().find(|d| d.media.is_audio_cd) else {
            eprintln!("no audio CD in any drive — skipping");
            return;
        };
        let entries = crate::disc::toc::track_entries(d);
        let Some(e) = entries.first() else { return };

        let mut ctx = fake_ctx(1);
        ctx.playlist.tracks[0].path = std::path::PathBuf::from(&e.path);
        unsafe {
            assert_eq!(
                sparkamp_playlist_is_read_only(&ctx, 0),
                1,
                "disc media is never writable in place"
            );
            assert_eq!(
                crate::ffi::media_library::sparkamp_playlist_file_missing(&ctx, 0),
                0,
                "a mounted disc track is present"
            );
        }
    }

    /// A ctx holding `n` fake playlist entries. The paths do not exist, so any
    /// load/play the call under test attempts fails harmlessly — enough for
    /// bookkeeping assertions, which is all these tests make.
    fn fake_ctx(n: usize) -> SparkampCtx {
        gstreamer::init().expect("GStreamer must be available for tests");

        let mut playlist = crate::model::Playlist::new();
        for i in 0..n {
            playlist.add(Track {
                path: std::path::PathBuf::from(format!("/fake/{i}.mp3")),
                title: format!("T{i}"),
                artist: String::new(),
                album_artist: String::new(),
                album: String::new(),
                duration: None,
                broken: false,
                read_only: false,
                id: 0,
            });
        }

        let (meta_tx, meta_rx) = std::sync::mpsc::channel();
        let (duration_tx, duration_rx) = std::sync::mpsc::channel();

        SparkampCtx {
            player: crate::engine::Player::new().expect("Player::new"),
            playlist,
            config: crate::config::Config::default(),
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
            media_library: None,
            ml_progress: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            ml_scanning: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            ml_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rg_progress: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            rg_running: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rg_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            watch: None,
            watch_rx: None,
        }
    }
}
