//! Media Library FFI — C-compatible track struct, library lifecycle, folder
//! management, track queries, playlist operations and CRUD.
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::media_library::MediaLibrary;
use crate::model::Track;

use super::SparkampCtx;

// ---------------------------------------------------------------------------
// Media Library — C-compatible track struct
// ---------------------------------------------------------------------------

/// A single track row returned from the media library.
///
/// All string fields are null-terminated and UTF-8.  Fixed-size arrays avoid
/// heap allocation on every row — callers should treat them as opaque blobs
/// and copy out what they need.
#[repr(C)]
pub struct SparkampLibTrack {
    pub id: i64,
    pub path: [u8; 512],
    pub title: [u8; 256],
    pub artist: [u8; 256],
    pub album: [u8; 256],
    pub genre: [u8; 64],
    pub year: c_int,
    pub track_num: c_int,
    pub length_secs: f64,
    pub bitrate: c_int,
    pub play_count: c_int,
    /// 1 if full metadata has been read; 0 if only filename is available.
    pub scanned: c_int,
    // Extended fields (all present in the DB after a full scan)
    pub album_artist: [u8; 256],
    pub disc_num: c_int,
    pub bpm: [u8; 32],
    pub comment: [u8; 512],
    pub composer: [u8; 256],
    /// 1 if the file is read-only on disk; 0 otherwise.
    pub read_only: c_int,
    /// 1 if cached album artwork exists for this track; 0 otherwise.
    pub has_art: c_int,
    /// Path to the cached/resolved artwork file for this track, or empty if
    /// `has_art` is 0. Same source as the ID3 editor / now-playing panel.
    pub artwork_path: [u8; 512],
    /// 1 if the file no longer exists at its recorded path; 0 otherwise.
    pub file_missing: c_int,
    /// ISO-8601 UTC timestamp of the last time this track was played
    /// ("YYYY-MM-DDTHH:MM:SSZ"), or empty string if never played.
    pub last_played: [u8; 32],
    /// Sample rate in Hz, read from the codec header; 0 if unknown.
    pub sample_rate: c_int,
    /// File size in bytes, captured at scan time; 0 if unknown.
    pub file_size: i64,
    /// ISO-8601 UTC timestamp of the row's first INSERT, or empty if unknown.
    pub added_at: [u8; 32],
    /// ISO-8601 UTC timestamp of the file's on-disk modification time, or empty if unknown.
    pub file_mtime: [u8; 32],
    /// "Variable" / "Constant", or empty when the container does not say.
    ///
    /// Sixteen bytes rather than eight: `copy_str` reserves one for the
    /// terminator, and "Variable" is eight characters, so the old width
    /// delivered "Variabl". Keep this and `sparkamp_bridge.h` in step.
    pub bitrate_mode: [u8; 16],
    /// Channel count (1 = mono, 2 = stereo, ...); 0 if unknown. Not one of
    /// Task 7's five listed fields, but required so the mac ID3 tech line
    /// (Step 3) can match core `tech_summary`'s "channels" part exactly.
    pub channels: c_int,
    /// ReplayGain track/album gain (dB) and peak (linear). Meaningful only
    /// when `rg_analyzed` is 1; all four are 0.0 otherwise. Peaks are the
    /// sample peak in [0.0, ~1.0]; gains are typically negative for loud
    /// tracks.
    pub rg_track_gain: f64,
    pub rg_track_peak: f64,
    pub rg_album_gain: f64,
    pub rg_album_peak: f64,
    /// 1 if a ReplayGain track gain has been computed/stored; 0 otherwise.
    /// Frontends should gate any gain display on this rather than testing the
    /// gain value (0.0 dB is a legitimate result for a reference-level track).
    pub rg_analyzed: c_int,
}

impl SparkampLibTrack {
    fn from_lib_track(t: &crate::media_library::LibTrack) -> Self {
        let mut out = Self {
            id: t.id,
            path: [0u8; 512],
            title: [0u8; 256],
            artist: [0u8; 256],
            album: [0u8; 256],
            genre: [0u8; 64],
            year: t.year.unwrap_or(0) as c_int,
            track_num: t.track_num.unwrap_or(0) as c_int,
            length_secs: t.length_secs.unwrap_or(0.0),
            bitrate: t.bitrate.unwrap_or(0) as c_int,
            play_count: t.play_count as c_int,
            scanned: if t.last_scanned.is_some() { 1 } else { 0 },
            album_artist: [0u8; 256],
            disc_num: t.disc_num.unwrap_or(0) as c_int,
            bpm: [0u8; 32],
            comment: [0u8; 512],
            composer: [0u8; 256],
            read_only: 0,
            has_art: if t.artwork_path.is_some() { 1 } else { 0 },
            artwork_path: [0u8; 512],
            file_missing: 0,
            last_played: [0u8; 32],
            sample_rate: t.sample_rate.unwrap_or(0) as c_int,
            file_size: t.file_size.unwrap_or(0),
            added_at: [0u8; 32],
            file_mtime: [0u8; 32],
            bitrate_mode: [0u8; 16],
            channels: t.channels.unwrap_or(0) as c_int,
            rg_track_gain: t.rg_track_gain.unwrap_or(0.0),
            rg_track_peak: t.rg_track_peak.unwrap_or(0.0),
            rg_album_gain: t.rg_album_gain.unwrap_or(0.0),
            rg_album_peak: t.rg_album_peak.unwrap_or(0.0),
            rg_analyzed: if t.rg_track_gain.is_some() { 1 } else { 0 },
        };
        fn copy_str(dst: &mut [u8], src: &str) {
            let bytes = src.as_bytes();
            let n = bytes.len().min(dst.len() - 1);
            dst[..n].copy_from_slice(&bytes[..n]);
            dst[n] = 0;
        }
        copy_str(&mut out.path, &t.path);
        copy_str(&mut out.artwork_path, t.artwork_path.as_deref().unwrap_or(""));
        copy_str(
            &mut out.title,
            t.title.as_deref().unwrap_or(&t.filename),
        );
        copy_str(&mut out.artist, t.artist.as_deref().unwrap_or(""));
        copy_str(&mut out.album, t.album.as_deref().unwrap_or(""));
        copy_str(&mut out.genre, t.genre.as_deref().unwrap_or(""));
        copy_str(&mut out.album_artist, t.album_artist.as_deref().unwrap_or(""));
        copy_str(&mut out.bpm, t.bpm.as_deref().unwrap_or(""));
        copy_str(&mut out.comment, t.comment.as_deref().unwrap_or(""));
        copy_str(&mut out.composer, t.composer.as_deref().unwrap_or(""));
        copy_str(&mut out.last_played, t.last_played.as_deref().unwrap_or(""));
        copy_str(&mut out.added_at, t.added_at.as_deref().unwrap_or(""));
        copy_str(&mut out.file_mtime, t.file_mtime.as_deref().unwrap_or(""));
        // Normalised here so the Swift side never has to know that rows
        // scanned before this was generalised say "VBR" and "CBR".
        copy_str(
            &mut out.bitrate_mode,
            &t.bitrate_mode
                .as_deref()
                .map(|m| crate::technical_probe::normalize_bitrate_mode(m).to_string())
                .unwrap_or_default(),
        );
        let p = std::path::Path::new(&t.path);
        out.read_only    = if crate::media_library::is_read_only(p) { 1 } else { 0 };
        out.file_missing = if p.exists() { 0 } else { 1 };
        out
    }
}

/// One album (or the single "no album" bucket) as returned by the album
/// gallery view. All string fields are null-terminated and UTF-8, using the
/// same fixed-buffer + `copy_str` idiom as [`SparkampLibTrack`].
#[repr(C)]
pub struct SparkampAlbum {
    pub album: [u8; 256],
    pub album_artist: [u8; 256],
    /// Path to a representative track's cached/resolved artwork, or empty if
    /// none of the album's tracks have artwork.
    pub artwork_path: [u8; 512],
    /// Release year, meaningful only when `has_year` is 1.
    pub year: i64,
    pub track_count: i64,
    /// 1 if `year` is a known value; 0 otherwise (year is 0 in that case).
    pub has_year: u8,
    /// 1 if this is the synthetic "(no album)" bucket that collapses every
    /// blank-album track regardless of artist; 0 for a normal album group.
    pub is_no_album: u8,
    /// Explicit padding to keep the layout predictable across the C
    /// boundary (aligns the trailing flags out to an 8-byte boundary).
    _pad: [u8; 6],
}

impl SparkampAlbum {
    fn from_group(g: &crate::media_library::AlbumGroup) -> Self {
        let mut out = Self {
            album: [0u8; 256],
            album_artist: [0u8; 256],
            artwork_path: [0u8; 512],
            year: g.year.unwrap_or(0),
            track_count: g.track_count,
            has_year: if g.year.is_some() { 1 } else { 0 },
            is_no_album: if g.is_no_album { 1 } else { 0 },
            _pad: [0u8; 6],
        };
        fn copy_str(dst: &mut [u8], src: &str) {
            let bytes = src.as_bytes();
            let n = bytes.len().min(dst.len() - 1);
            dst[..n].copy_from_slice(&bytes[..n]);
            dst[n] = 0;
        }
        copy_str(&mut out.album, &g.album);
        copy_str(&mut out.album_artist, &g.album_artist);
        copy_str(&mut out.artwork_path, g.artwork_path.as_deref().unwrap_or(""));
        out
    }
}

/// Map the mac-side `sort: u32` wire value to [`AlbumSort`]. Unknown values
/// (including anything beyond 0/1/2) default to `Artist`, mirroring every
/// other FFI sort-column fallback in this file.
fn album_sort_from_u32(sort: u32) -> crate::media_library::AlbumSort {
    use crate::media_library::AlbumSort;
    match sort {
        1 => AlbumSort::Album,
        2 => AlbumSort::Year,
        _ => AlbumSort::Artist,
    }
}

// ---------------------------------------------------------------------------
// Media Library — lifecycle
// ---------------------------------------------------------------------------

/// Open (or create) the media library database.
///
/// Must be called before any other `sparkamp_ml_*` function.  Safe to call
/// multiple times — subsequent calls are no-ops if the DB is already open.
/// mac never calls this eagerly at startup — only from first-demand sites
/// (`SparkampModel+MediaLibrary.swift`'s `openMediaLibrary()`), which is
/// exactly the deferred-open behaviour `config.media_library.skip_db_load`
/// (F12.3) asks GTK to opt into; mac has worked this way since Phase 8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_open(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    if ctx.media_library.is_none() {
        match MediaLibrary::open() {
            Ok(ml) => {
                let _ = ml.cleanup_on_startup();
                ctx.media_library = Some(ml);
            }
            Err(e) => eprintln!("[sparkamp_ml_open] {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Media Library — folder management
// ---------------------------------------------------------------------------

/// Return the number of watched folders, or 0 if the ML is not open.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_folder_count(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    let Some(ml) = &ctx.media_library else { return 0 };
    ml.list_folders().map(|v| v.len() as c_int).unwrap_or(0)
}

/// Return the path of the folder at `index` as a heap-allocated C string.
///
/// The caller must free it with `sparkamp_free_string`.
/// Returns null if the index is out of range or the ML is not open.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_folder_path(
    ctx: *const SparkampCtx,
    index: c_int,
) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let Some(ml) = &ctx.media_library else {
        return std::ptr::null_mut();
    };
    let folders = ml.list_folders().unwrap_or_default();
    let idx = index as usize;
    if idx >= folders.len() {
        return std::ptr::null_mut();
    }
    CString::new(folders[idx].1.as_str())
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Add a folder to the media library and start a two-phase scan.
///
/// Phase 1 (fast, synchronous on calling thread): registers the folder and
/// adds all audio file paths to the DB with filename-only metadata.
///
/// Phase 2 (background): reads ID3/Vorbis/Opus/FLAC tags for every new file.
/// `progress_cb(userdata, done, total)` is called from the background thread
/// on each file.  `done_cb(userdata)` is called when the scan completes.
/// Both callbacks may be null.
///
/// The background thread opens a **separate** DB connection, so the main
/// thread can continue querying while the scan runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_add_folder(
    ctx: *mut SparkampCtx,
    path: *const c_char,
    progress_cb: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int)>,
    done_cb: Option<unsafe extern "C" fn(*mut c_void)>,
    userdata: *mut c_void,
) {
    if ctx.is_null() || path.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return };
    let path_str = match CStr::from_ptr(path).to_str() {
        Ok(s) => s.to_owned(),
        Err(_) => return,
    };

    // Phase 1 — fast: register folder + filename-only entries (synchronous).
    let folder_id = match ml.add_folder(&path_str) {
        Ok(res) => res.id(),
        Err(e) => {
            eprintln!("[sparkamp_ml_add_folder] add_folder: {e}");
            return;
        }
    };
    if let Err(e) = ml.rescan_folder_fast(
        folder_id,
        &path_str,
        ctx.config.media_library.remove_missing_on_rescan,
    ) {
        eprintln!("[sparkamp_ml_add_folder] rescan_fast: {e}");
        return;
    }

    // Phase 2 — background: full metadata scan.
    let cancel = Arc::clone(&ctx.ml_cancel);
    let scanning = Arc::clone(&ctx.ml_scanning);
    let progress_atomic = Arc::clone(&ctx.ml_progress);
    cancel.store(false, Ordering::Relaxed);
    scanning.store(true, Ordering::Relaxed);

    // Cast userdata to usize so the closure is Send (raw pointers are not Send).
    let ud_addr = userdata as usize;

    rayon::spawn(move || {
        let ud: *mut c_void = ud_addr as *mut c_void;
        let result = MediaLibrary::open_at(&MediaLibrary::db_path_pub()).and_then(|bg_ml| {
            let atomic = &progress_atomic;
            bg_ml.scan_folder(folder_id, &cancel, |done, total| {
                let packed = ((total as u64) << 32) | (done as u64);
                atomic.store(packed, Ordering::Relaxed);
                if let Some(cb) = progress_cb {
                    unsafe { cb(ud, done as c_int, total as c_int) };
                }
            })
        });
        if let Err(e) = result {
            eprintln!("[sparkamp_ml_add_folder] background scan: {e}");
        }
        scanning.store(false, Ordering::Relaxed);
        if let Some(cb) = done_cb {
            unsafe { cb(ud) };
        }
    });
}

/// Remove a watched folder and all its tracks from the media library.
///
/// The folder is matched by path string.  No-op if the path is not in the DB.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_remove_folder(
    ctx: *mut SparkampCtx,
    path: *const c_char,
) {
    if ctx.is_null() || path.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return };
    let path_str = match CStr::from_ptr(path).to_str() {
        Ok(s) => s,
        Err(_) => return,
    };
    let folders = ml.list_folders().unwrap_or_default();
    if let Some((folder_id, _)) = folders.into_iter().find(|(_, p)| p == path_str) {
        if let Err(e) = ml.remove_folder(folder_id) {
            eprintln!("[sparkamp_ml_remove_folder] {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Media Library — per-folder recurse
// ---------------------------------------------------------------------------

/// Whether the watched folder at `path` is scanned recursively into
/// subdirectories. Returns `true` (the schema default) if the ML isn't
/// open, `path` is null, or no folder matches `path`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_folder_recurse(
    ctx: *const SparkampCtx,
    path: *const c_char,
) -> bool {
    if ctx.is_null() || path.is_null() {
        return true;
    }
    let ctx = &*ctx;
    let Some(ml) = &ctx.media_library else { return true };
    let Ok(path_str) = CStr::from_ptr(path).to_str() else { return true };
    let folders = ml.list_folders().unwrap_or_default();
    match folders.into_iter().find(|(_, p)| p == path_str) {
        Some((id, _)) => ml.folder_recurse(id).unwrap_or(true),
        None => true,
    }
}

/// Set whether the watched folder at `path` is scanned recursively. No-op
/// if the ML isn't open, `path` is null, or no folder matches `path`.
///
/// Does NOT rebuild the live watcher itself — call
/// `sparkamp_ml_watch_rebuild` afterward so a running watch picks up the
/// new recurse mode (mirrors the add/remove-folder contract).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_set_folder_recurse(
    ctx: *mut SparkampCtx,
    path: *const c_char,
    recurse: bool,
) {
    if ctx.is_null() || path.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return };
    let Ok(path_str) = CStr::from_ptr(path).to_str() else { return };
    let folders = ml.list_folders().unwrap_or_default();
    if let Some((id, _)) = folders.into_iter().find(|(_, p)| p == path_str) {
        if let Err(e) = ml.set_folder_recurse(id, recurse) {
            eprintln!("[sparkamp_ml_set_folder_recurse] {path_str}: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Media Library — filesystem watcher lifecycle (Phase 8 Task 9)
// ---------------------------------------------------------------------------
//
// The watcher itself (`crate::watch::FolderWatcher`) is pure OS-notify
// plumbing; this section is the only place that knows how to turn "current
// config + folder list" into a running watcher, and how to drain its event
// channel into library DB writes. Kept in this file (not settings.rs)
// because it needs `MediaLibrary::{list_folders, folder_recurse}` and
// `apply_watch_action`, all of which live in this FFI domain.

/// (Re)build the folder watcher from the current `watch_folders` config flag
/// and folder list, or tear it down if watching is off / the library isn't
/// open. Always drops any existing watcher first (its `Drop` stops the
/// debouncer thread) before possibly starting a new one, so this is safe to
/// call repeatedly (e.g. after every folder add/remove).
///
/// Never panics: a watcher-start failure (e.g. inotify watch limit) is
/// logged and degrades to `None` — callers keep working via manual/interval
/// rescans, exactly like a platform with no watcher support at all.
///
/// `pub(super)` so `settings::sparkamp_set_watch_folders` (the toggle) can
/// call it too; not `#[no_mangle]` itself, only the two public entry points
/// below (the toggle setter and `sparkamp_ml_watch_rebuild`) are.
pub(super) unsafe fn rebuild_watcher(ctx: &mut SparkampCtx) {
    // Drop any existing watcher unconditionally — cheapest way to guarantee
    // no stale watch survives a config/folder-list change.
    ctx.watch = None;
    ctx.watch_rx = None;

    if !ctx.config.media_library.watch_folders {
        return;
    }
    let Some(ml) = &ctx.media_library else { return };

    let folder_rows = ml.list_folders().unwrap_or_default();
    let folders: Vec<(PathBuf, bool)> = folder_rows
        .iter()
        .map(|(id, path)| {
            let recurse = ml.folder_recurse(*id).unwrap_or(true);
            (PathBuf::from(path), recurse)
        })
        .collect();

    // Nothing to watch — don't spin up a debouncer thread with no watches on
    // it, same early return GTK's and the TUI's rebuild_watcher make.
    if folders.is_empty() {
        return;
    }

    let audio_exts: Vec<String> = crate::model::AUDIO_EXTENSIONS
        .iter()
        .map(|s| s.to_string())
        .collect();
    // Same cache directory tag/artwork writers use (tags.rs, now_playing.rs)
    // — the watcher filters out paths under this prefix so it never treats
    // Sparkamp's own cached artwork as a library change.
    let cache_prefix = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("sparkamp");

    match crate::watch::FolderWatcher::start(folders, audio_exts, cache_prefix) {
        Ok((watcher, rx)) => {
            ctx.watch = Some(watcher);
            ctx.watch_rx = Some(rx);
        }
        Err(e) => {
            eprintln!("[sparkamp] watch start failed (degraded to manual rescan): {e}");
            ctx.watch = None;
            ctx.watch_rx = None;
        }
    }
}

/// Public entry point for the mac frontend to call after any folder
/// add/remove (or recurse change) so the watch set stays current. The
/// `watch_folders` toggle itself calls `rebuild_watcher` directly (see
/// `sparkamp_set_watch_folders`); everything else that can invalidate the
/// watch set goes through this symbol instead.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_watch_rebuild(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    rebuild_watcher(ctx);
}

/// Drain ONE pending filesystem-watch event, apply it to the library DB, and
/// return the affected path so the UI can refresh its row.
///
/// Returns NULL if no watcher is running or no event is queued right now —
/// callers should poll this in a timer/tick loop the same way `sparkamp_tick`
/// drains the metadata/duration channels. On success, `*out_kind` is set to
/// 0 (file added/changed) or 1 (file removed) and the return value is a heap
/// C string (free with `sparkamp_free_string`) holding the absolute path.
///
/// A DB-apply error is logged, not surfaced — the path is still returned so
/// the frontend can react (e.g. refresh) even if the underlying library
/// write failed; this mirrors every other FFI function's "never block Swift
/// on a library error" convention.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_poll_watch_event(
    ctx: *mut SparkampCtx,
    out_kind: *mut c_int,
) -> *mut c_char {
    if ctx.is_null() || out_kind.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &mut *ctx;
    let Some(rx) = &ctx.watch_rx else {
        return std::ptr::null_mut();
    };
    let action = match rx.try_recv() {
        Ok(a) => a,
        Err(_) => return std::ptr::null_mut(),
    };

    // Kind 2 is new (a playlist file appeared). A mac frontend that only
    // knows 0 and 1 still behaves: apply_watch_action below has already
    // registered the playlist, and an unrecognised kind means it refreshes
    // nothing rather than refreshing the wrong thing.
    let (kind, path) = match &action {
        crate::watch::WatchAction::Upsert(p) => (0, p.clone()),
        crate::watch::WatchAction::Remove(p) => (1, p.clone()),
        crate::watch::WatchAction::PlaylistUpsert(p) => (2, p.clone()),
    };

    if let Some(ml) = &ctx.media_library {
        let remove_missing = ctx.config.media_library.remove_missing_on_rescan;
        if let Err(e) = ml.apply_watch_action(&action, remove_missing) {
            eprintln!("[sparkamp] apply_watch_action {}: {e}", path.display());
        }
    }

    *out_kind = kind;
    CString::new(path.to_string_lossy().into_owned())
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Auto-add-played (Phase 8 Task 12): make sure `path`, which the mac
/// frontend just started playing, has a row in the media library — mirrors
/// GTK's `State::maybe_auto_add_played` (`frontends/gtk/window/state.rs`)
/// and the TUI's `App::maybe_auto_add_played`, but as an FFI entry point so
/// Swift (which cannot call `add_played_track` in-process) can trigger the
/// same policy. Encapsulates the whole decision here so Swift stays dumb —
/// it just calls this once per track start.
///
/// No-op (`false`) if `auto_add_played` is off, the library isn't open, or
/// `path` resolves inside a watched folder — the watcher/rescan already
/// owns paths inside watched folders, and (per the GTK doc comment this
/// mirrors) the library's scan paths are stored un-canonicalized while a
/// frontend's now-playing path may be canonicalized, so an inside-folder
/// path can't be reliably matched against `add_played_track`'s exact-string
/// dedup check — skip entirely rather than risk a duplicate row. Also
/// `false` if `owning_folder_id`'s lookup itself errors (logged) or if
/// `add_played_track` errors (logged).
///
/// Returns `true` only when `add_played_track` actually inserted a new row
/// — the caller (Swift) can use that as a "mark the Files view stale"
/// signal.
///
/// `path` is used exactly as passed — NOT re-canonicalized here — matching
/// the GTK/TUI call sites, which pass the path the player was just loaded
/// with.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_note_played(
    ctx: *mut SparkampCtx,
    path: *const c_char,
) -> bool {
    if ctx.is_null() || path.is_null() {
        return false;
    }
    let ctx = &mut *ctx;
    if !ctx.config.media_library.auto_add_played {
        return false;
    }
    let Some(ml) = &ctx.media_library else {
        return false;
    };
    let Ok(path_str) = CStr::from_ptr(path).to_str() else {
        return false;
    };
    match ml.owning_folder_id(path_str) {
        // Inside a watched folder — already managed by the watcher/rescan;
        // skip to avoid a duplicate row (see doc comment above).
        Ok(Some(_)) => false,
        // Outside every watched folder — the case auto-add-played exists
        // for.
        Ok(None) => match ml.add_played_track(path_str) {
            Ok(created) => created,
            Err(e) => {
                eprintln!("[sparkamp_ml_note_played] add_played_track failed for {path_str}: {e}");
                false
            }
        },
        Err(e) => {
            eprintln!("[sparkamp_ml_note_played] owning_folder_id lookup failed for {path_str}: {e}");
            false
        }
    }
}

/// Remove a single track from the media library by its database ID.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_remove_track(
    ctx: *mut SparkampCtx,
    track_id: i64,
) {
    if ctx.is_null() { return; }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return };
    if let Err(e) = ml.remove_track(track_id) {
        eprintln!("[sparkamp_ml_remove_track] {e}");
    }
}

/// Rescan all watched folders.
///
/// Same two-phase pattern as `sparkamp_ml_add_folder`.  `progress_cb` and
/// `done_cb` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_rescan_all(
    ctx: *mut SparkampCtx,
    progress_cb: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int)>,
    done_cb: Option<unsafe extern "C" fn(*mut c_void)>,
    userdata: *mut c_void,
) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    if ctx.media_library.is_none() {
        return;
    }

    let remove_missing = ctx.config.media_library.remove_missing_on_rescan;
    // Read before spawning: `ctx` (and its `config`) isn't available inside
    // the background closure below. Compact only after this FULL rescan
    // completes, gated on the setting — mirrors GTK/TUI (VACUUM is too
    // heavy to run after every fast folder-add).
    let compact_after = ctx.config.media_library.compact_on_rescan;

    let cancel = Arc::clone(&ctx.ml_cancel);
    let scanning = Arc::clone(&ctx.ml_scanning);
    let progress_atomic = Arc::clone(&ctx.ml_progress);
    cancel.store(false, Ordering::Relaxed);
    scanning.store(true, Ordering::Relaxed);

    let ud_addr = userdata as usize;

    rayon::spawn(move || {
        let ud: *mut c_void = ud_addr as *mut c_void;
        let result = MediaLibrary::open_at(&MediaLibrary::db_path_pub()).and_then(|bg_ml| {
            // Fast phase: walk every folder to pick up files added or
            // removed since the last scan. On this thread, not the caller's
            // — a full walk of a large library is seconds of filesystem I/O,
            // and the caller is the main thread (the Rescan button, or the
            // rescan-on-startup trigger, where it would delay launch). Same
            // shape as GTK's startup rescan, which also walks inside its
            // worker thread. The DB is WAL with a busy timeout, so the main
            // thread's reads run alongside these writes.
            for (folder_id, folder_path) in bg_ml.list_folders().unwrap_or_default() {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if let Err(e) = bg_ml.rescan_folder_fast(folder_id, &folder_path, remove_missing) {
                    eprintln!("[sparkamp_ml_rescan_all] fast rescan {folder_path}: {e}");
                }
            }
            // Recover rows an earlier scan marked done but wrote no metadata
            // for, so a Rescan actually re-reads them — the same call GTK
            // makes ahead of every full rescan (window/watch.rs,
            // media_library.rs, settings.rs). Cheap, and this is the button
            // a user reaches for when rows look empty.
            let _ = bg_ml.reset_unscanned_metadata();
            let atomic = &progress_atomic;
            let scan_result = bg_ml.scan_all_folders(&cancel, |done, total| {
                let packed = ((total as u64) << 32) | (done as u64);
                atomic.store(packed, Ordering::Relaxed);
                if let Some(cb) = progress_cb {
                    unsafe { cb(ud, done as c_int, total as c_int) };
                }
            });
            if scan_result.is_ok() && compact_after {
                if let Err(e) = bg_ml.compact() {
                    eprintln!("[sparkamp_ml_rescan_all] compact_on_rescan: VACUUM failed: {e}");
                }
            }
            scan_result
        });
        if let Err(e) = result {
            eprintln!("[sparkamp_ml_rescan_all] background scan: {e}");
        }
        scanning.store(false, Ordering::Relaxed);
        if let Some(cb) = done_cb {
            unsafe { cb(ud) };
        }
    });
}

/// Cancel a running background scan.  No-op if no scan is running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_cancel_scan(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    (*ctx).ml_cancel.store(true, Ordering::Relaxed);
}

/// Returns 1 if a background scan is running, 0 otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_scan_is_running(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    (*ctx).ml_scanning.load(Ordering::Relaxed) as c_int
}

/// Reads the scan progress atomically.
///
/// `done_out` and `total_out` are set to the number of files processed and
/// the total number of files to process, respectively.  Both are set to 0
/// if no scan is running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_scan_progress(
    ctx: *const SparkampCtx,
    done_out: *mut c_int,
    total_out: *mut c_int,
) {
    if ctx.is_null() || done_out.is_null() || total_out.is_null() {
        return;
    }
    let packed = (*ctx).ml_progress.load(Ordering::Relaxed);
    *done_out = (packed & 0xFFFF_FFFF) as c_int;
    *total_out = (packed >> 32) as c_int;
}

// ---------------------------------------------------------------------------
// Media Library — ReplayGain analysis (background)
// ---------------------------------------------------------------------------
//
// Mirrors the metadata-scan background pattern (rg_progress / rg_running /
// rg_cancel atomics, rayon::spawn, separate DB connection) but computes and
// stores ReplayGain instead of reading tags. Analysis decodes whole files, so
// it always runs off the main thread. `write_tags` is taken from config
// (a container with no ReplayGain representation is skipped). Only one RG job runs at
// a time — a second call while `rg_running` is set is ignored.

/// Shared worker: analyze `tracks` (already the exact set) and report progress
/// through the `rg_*` atomics + optional callbacks. Consumes the atomics/flags
/// (cloned Arcs) so it is `Send` for `rayon::spawn`.
unsafe fn rg_spawn_analysis(
    ctx: &mut SparkampCtx,
    tracks: Vec<crate::media_library::LibTrack>,
    progress_cb: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int)>,
    done_cb: Option<unsafe extern "C" fn(*mut c_void)>,
    userdata: *mut c_void,
) {
    // Refuse a second job (matches sparkamp_ml_add_folder's implicit
    // single-job model, but explicit here since the caller may spam it).
    if ctx.rg_running.load(Ordering::Relaxed) {
        return;
    }
    if !crate::replaygain::rg_analysis_available() {
        // rganalysis plugin missing — report an immediate "done" so the UI
        // doesn't wait on a job that will never start.
        if let Some(cb) = done_cb {
            cb(userdata);
        }
        return;
    }
    let write_tags = ctx.config.playback.replaygain.write_tags;
    let cancel = Arc::clone(&ctx.rg_cancel);
    let running = Arc::clone(&ctx.rg_running);
    let progress_atomic = Arc::clone(&ctx.rg_progress);
    cancel.store(false, Ordering::Relaxed);
    running.store(true, Ordering::Relaxed);
    progress_atomic.store(0, Ordering::Relaxed);

    // usize so the closure is Send (raw pointers are not).
    let ud_addr = userdata as usize;

    rayon::spawn(move || {
        let ud: *mut c_void = ud_addr as *mut c_void;
        match MediaLibrary::open_at(&MediaLibrary::db_path_pub()) {
            Ok(bg_ml) => {
                let atomic = &progress_atomic;
                let _ = crate::replaygain::analyze_and_store(
                    &bg_ml,
                    &tracks,
                    write_tags,
                    &cancel,
                    |p| {
                        let packed = ((p.total as u64) << 32) | (p.done as u64);
                        atomic.store(packed, Ordering::Relaxed);
                        if let Some(cb) = progress_cb {
                            unsafe { cb(ud, p.done as c_int, p.total as c_int) };
                        }
                    },
                );
            }
            Err(e) => eprintln!("[sparkamp_rg_analyze] DB open: {e}"),
        }
        running.store(false, Ordering::Relaxed);
        if let Some(cb) = done_cb {
            unsafe { cb(ud) };
        }
    });
}

/// Analyze every track that is missing a ReplayGain value or whose file has
/// changed since its last scan (the "Analyze ReplayGain" bulk action). Skips
/// already-analyzed, unchanged tracks. No-op if the library isn't open or an
/// RG job is already running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_rg_analyze_missing(
    ctx: *mut SparkampCtx,
    progress_cb: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int)>,
    done_cb: Option<unsafe extern "C" fn(*mut c_void)>,
    userdata: *mut c_void,
) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return };
    let tracks: Vec<crate::media_library::LibTrack> = ml
        .all_tracks()
        .unwrap_or_default()
        .into_iter()
        .filter(crate::replaygain::needs_analysis)
        .collect();
    rg_spawn_analysis(ctx, tracks, progress_cb, done_cb, userdata);
}

/// Force a ReplayGain recompute of the tracks whose DB ids are in
/// `ids` (length `count`) — the per-selection "Calculate ReplayGain" action.
/// Recomputes regardless of any stored value. No-op if the library isn't open,
/// an RG job is already running, or `ids` is null/empty.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_rg_analyze_selection(
    ctx: *mut SparkampCtx,
    ids: *const i64,
    count: c_int,
    progress_cb: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int)>,
    done_cb: Option<unsafe extern "C" fn(*mut c_void)>,
    userdata: *mut c_void,
) {
    if ctx.is_null() || ids.is_null() || count <= 0 {
        return;
    }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return };
    let id_slice = std::slice::from_raw_parts(ids, count as usize);
    let tracks: Vec<crate::media_library::LibTrack> = ml
        .tracks_by_ids(id_slice)
        .unwrap_or_default()
        .into_values()
        .collect();
    rg_spawn_analysis(ctx, tracks, progress_cb, done_cb, userdata);
}

/// Cancel a running ReplayGain analysis. No-op if none is running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_rg_analyze_cancel(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    (*ctx).rg_cancel.store(true, Ordering::Relaxed);
}

/// Returns 1 if a ReplayGain analysis is running, 0 otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_rg_analyze_is_running(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    (*ctx).rg_running.load(Ordering::Relaxed) as c_int
}

/// Reads ReplayGain analysis progress atomically. `done_out`/`total_out` get
/// the number of tracks analyzed and the total to analyze; both 0 if idle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_rg_analyze_progress(
    ctx: *const SparkampCtx,
    done_out: *mut c_int,
    total_out: *mut c_int,
) {
    if ctx.is_null() || done_out.is_null() || total_out.is_null() {
        return;
    }
    let packed = (*ctx).rg_progress.load(Ordering::Relaxed);
    *done_out = (packed & 0xFFFF_FFFF) as c_int;
    *total_out = (packed >> 32) as c_int;
}

// ---------------------------------------------------------------------------
// Media Library — track queries
// ---------------------------------------------------------------------------

/// Fetch a page of tracks into a caller-allocated array.
///
/// - `query`: UTF-8 search string; null or empty means all tracks.
/// - `sort_col`: column name ("title", "artist", "album", "duration", "num",
///   "year", "genre", "bitrate", "filename"); null means default ordering.
/// - `sort_desc`: 1 for descending, 0 for ascending.
/// - `offset` / `limit`: pagination parameters.
/// - `out`: caller-allocated array of at least `limit` `SparkampLibTrack` elements.
///
/// Returns the number of elements actually written.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_get_tracks(
    ctx: *const SparkampCtx,
    query: *const c_char,
    sort_col: *const c_char,
    sort_desc: c_int,
    offset: c_int,
    limit: c_int,
    out: *mut SparkampLibTrack,
) -> c_int {
    if ctx.is_null() || out.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    let Some(ml) = &ctx.media_library else { return 0 };

    let q = if query.is_null() {
        String::new()
    } else {
        CStr::from_ptr(query).to_str().unwrap_or("").to_owned()
    };
    let col = if sort_col.is_null() {
        String::new()
    } else {
        CStr::from_ptr(sort_col).to_str().unwrap_or("").to_owned()
    };
    let desc = sort_desc != 0;

    let tracks = if col.is_empty() {
        if q.is_empty() {
            ml.all_tracks().unwrap_or_default()
        } else {
            ml.search_tracks(&q).unwrap_or_default()
        }
    } else {
        #[allow(clippy::collapsible_else_if)]
        if q.is_empty() {
            ml.all_tracks_sorted(&col, desc).unwrap_or_default()
        } else {
            ml.search_tracks_sorted(&q, &col, desc).unwrap_or_default()
        }
    };

    let start = (offset as usize).min(tracks.len());
    let end = (start + limit as usize).min(tracks.len());
    let page = &tracks[start..end];

    for (i, t) in page.iter().enumerate() {
        let slot = out.add(i);
        slot.write(SparkampLibTrack::from_lib_track(t));
    }
    page.len() as c_int
}

// ---------------------------------------------------------------------------
// Media Library — album gallery (Phase 11 Task 3)
// ---------------------------------------------------------------------------

/// Return the number of album groups (or 0 if the ML is not open).
///
/// `sort` maps 0=Artist, 1=Album, 2=Year (see [`album_sort_from_u32`]).
/// The "artist as album artist" toggle is read from config, not passed in.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_album_count(
    ctx: *const SparkampCtx,
    sort: u32,
) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    let Some(ml) = &ctx.media_library else { return 0 };
    let artist_as_album = ctx.config.media_library.artist_as_album_artist;
    ml.albums(album_sort_from_u32(sort), artist_as_album)
        .map(|v| v.len() as c_int)
        .unwrap_or(0)
}

/// Fetch up to `limit` album groups into a caller-allocated array.
///
/// `sort` maps 0=Artist, 1=Album, 2=Year. Returns the number of elements
/// actually written; 0 if `ctx`/`out` is null or the ML is not open.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_albums(
    ctx: *const SparkampCtx,
    sort: u32,
    out: *mut SparkampAlbum,
    limit: c_int,
) -> c_int {
    if ctx.is_null() || out.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    let Some(ml) = &ctx.media_library else { return 0 };
    let artist_as_album = ctx.config.media_library.artist_as_album_artist;
    let groups = ml
        .albums(album_sort_from_u32(sort), artist_as_album)
        .unwrap_or_default();
    let n = (limit.max(0) as usize).min(groups.len());
    let page = &groups[..n];
    for (i, g) in page.iter().enumerate() {
        let slot = out.add(i);
        slot.write(SparkampAlbum::from_group(g));
    }
    page.len() as c_int
}

/// Fetch up to `limit` tracks belonging to the album `(album, album_artist)`
/// into a caller-allocated array.
///
/// Null `album`/`album_artist` are treated as empty strings, so the
/// "(no album)" bucket is reachable by passing `album = ""`. Returns the
/// number of elements actually written; 0 if `ctx`/`out` is null or the ML
/// is not open.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_album_tracks(
    ctx: *const SparkampCtx,
    album: *const c_char,
    album_artist: *const c_char,
    out: *mut SparkampLibTrack,
    limit: c_int,
) -> c_int {
    if ctx.is_null() || out.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    let Some(ml) = &ctx.media_library else { return 0 };
    let album_str = if album.is_null() {
        String::new()
    } else {
        CStr::from_ptr(album).to_str().unwrap_or("").to_owned()
    };
    let album_artist_str = if album_artist.is_null() {
        String::new()
    } else {
        CStr::from_ptr(album_artist).to_str().unwrap_or("").to_owned()
    };
    let artist_as_album = ctx.config.media_library.artist_as_album_artist;
    let tracks = ml
        .album_tracks(&album_str, &album_artist_str, artist_as_album)
        .unwrap_or_default();
    let n = (limit.max(0) as usize).min(tracks.len());
    let page = &tracks[..n];
    for (i, t) in page.iter().enumerate() {
        let slot = out.add(i);
        slot.write(SparkampLibTrack::from_lib_track(t));
    }
    page.len() as c_int
}

// ---------------------------------------------------------------------------
// Media Library — playlist operations
// ---------------------------------------------------------------------------

/// Add tracks (identified by their library IDs) to the active playlist.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_add_tracks_to_playlist(
    ctx: *mut SparkampCtx,
    ids: *const i64,
    count: c_int,
) {
    if ctx.is_null() || ids.is_null() || count <= 0 {
        return;
    }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return };
    let id_slice = std::slice::from_raw_parts(ids, count as usize);

    // One query for exactly the rows asked for. This used to read the whole
    // table and filter here, on the reasoning that N individual queries would
    // be worse — true, but the alternative was never "fetch everything".
    // Measured on a 36,329-track library: all_tracks() 370-390 ms, this 116 us.
    let by_id = ml.tracks_by_ids(id_slice).unwrap_or_default();

    let start_idx = ctx.playlist.tracks.len();
    for &id in id_slice {
        if let Some(t) = by_id.get(&id) {
            // Build the active-playlist Track directly from the ML row so
            // duration + tags are inherited synchronously.  The background
            // probe below still runs to refine values for tracks the ML
            // hasn't scanned yet (length_secs == None) and to catch any
            // file-vs-DB drift.
            ctx.playlist.tracks.push(Track::from(t));
        }
    }
    // These pushed straight into `tracks`, so the new entries still hold the
    // id-0 sentinel — stamp them before anything reads ids for the queue.
    super::queue::sync_queue_to_playlist(ctx);
    // Finish only the rows the library could not describe.
    let n = ctx.playlist.tracks.len();
    let unfinished: Vec<(u64, std::path::PathBuf)> = (start_idx..n)
        .filter(|&idx| needs_probe(&ctx.playlist.tracks[idx]))
        .map(|idx| (ctx.playlist.tracks[idx].id, ctx.playlist.tracks[idx].path.clone()))
        .collect();
    spawn_row_probes(ctx, unfinished);
}

/// Return the number of saved playlists in the library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_playlist_count(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    let Some(ml) = &ctx.media_library else { return 0 };
    ml.all_playlists().map(|v| v.len() as c_int).unwrap_or(0)
}

/// Return the name of the playlist at `index` as a heap-allocated C string.
///
/// Caller must free with `sparkamp_free_string`.  Returns null on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_playlist_name(
    ctx: *const SparkampCtx,
    index: c_int,
) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let Some(ml) = &ctx.media_library else {
        return std::ptr::null_mut();
    };
    let playlists = ml.all_playlists().unwrap_or_default();
    let idx = index as usize;
    if idx >= playlists.len() {
        return std::ptr::null_mut();
    }
    CString::new(playlists[idx].name.as_str())
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Load the saved playlist at `index` as the active playlist, replacing it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_set_current_playlist(
    ctx: *mut SparkampCtx,
    index: c_int,
) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return };
    let playlists = ml.all_playlists().unwrap_or_default();
    let idx = index as usize;
    if idx >= playlists.len() {
        return;
    }
    let tracks = ml
        .load_playlist_tracks(&playlists[idx])
        .unwrap_or_default();
    ctx.playlist.tracks.clear();
    ctx.playlist.current_index = 0;
    for t in &tracks {
        // Inherit duration + tags from the ML row (or the EXTINF data the
        // loader fell back to for stub entries).  Background probes below
        // still refine missing values.
        ctx.playlist.tracks.push(Track::from(t));
    }
    // Wholesale replacement — every previously queued entry is gone.
    super::queue::sync_queue_to_playlist(ctx);
    // Finish only the rows the library could not describe. The playlist was
    // cleared above, so a track's index is its position in `tracks`.
    let unfinished: Vec<(u64, std::path::PathBuf)> = ctx
        .playlist
        .tracks
        .iter()
        .filter(|t| needs_probe(t))
        .map(|t| (t.id, t.path.clone()))
        .collect();
    spawn_row_probes(ctx, unfinished);
}

/// Whether a playlist row still needs to be read off disk.
///
/// A row built from a scanned library record already carries its title,
/// artist, album, album artist and duration, so reading the file again spends
/// ~24 ms (cold, on rotational storage) to learn what is already in hand. Only
/// a row the library could not describe — a path-only entry from the fast
/// insert, or one whose duration probe failed — has anything left to find.
///
/// Duration is the signal: the fast insert writes path and filename and
/// nothing else, so an unscanned row has none. Title is not, because
/// `Track::from` falls back to the filename and is therefore never empty.
///
/// # The trade this makes
///
/// The code replaced here probed every added row unconditionally, and its
/// comment named "catch any file-vs-DB drift" as a second purpose alongside
/// filling in unscanned rows. That second purpose is given up deliberately: a
/// track whose ID3 tags were edited outside Sparkamp now shows the library's
/// older values until a scan updates them, where before the next add would
/// have quietly corrected the row.
///
/// It is the right trade — the drift correction cost ~24 ms of cold disk read
/// per row on *every* add, to fix a case that only arises when another program
/// writes the file — but it is a user-visible change, not a pure optimisation,
/// and belongs in the release notes.
fn needs_probe(t: &crate::model::Track) -> bool {
    t.duration.is_none()
}

/// Read the given rows off disk and report tags and duration back.
///
/// Rows are named by stable entry id (`Track::id`), the same key GTK and the
/// TUI use. Keying by playlist index — which this did until drag-and-drop
/// started inserting at a drop position — landed tags on the wrong row
/// whenever a reorder or a remove happened while probes were in flight.
///
/// Both probes for one file run on a single task rather than two: they open
/// the same file, so doing them back to back keeps the second read on a warm
/// page cache and halves the opens. Runs on the shared bounded pool rather
/// than the global one, so a large add cannot gang up on the disk with the
/// duration probes already running there.
fn spawn_row_probes(ctx: &SparkampCtx, rows: Vec<(u64, std::path::PathBuf)>) {
    for (id, path) in rows {
        let meta_tx = ctx.meta_tx.clone();
        let duration_tx = ctx.duration_tx.clone();
        let probe = move || {
            if let Ok(track) = crate::model::Track::from_path(&path) {
                let _ = meta_tx.send((
                    id,
                    track.title.clone(),
                    track.artist.clone(),
                    track.album_artist.clone(),
                ));
            }
            if let Some(dur) = crate::duration_probe::probe_duration(&path) {
                let _ = duration_tx.send((id, dur));
            }
        };
        match crate::duration_probe::shared_probe_pool() {
            Some(pool) => pool.spawn(probe),
            None => probe(),
        }
    }
}

// ---------------------------------------------------------------------------
// Media Library — playlist CRUD
// ---------------------------------------------------------------------------

/// Return the row ID of the playlist at `index`, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_playlist_id(
    ctx: *const SparkampCtx,
    index: c_int,
) -> i64 {
    if ctx.is_null() { return -1; }
    let ctx = &*ctx;
    let Some(ml) = &ctx.media_library else { return -1 };
    let playlists = ml.all_playlists().unwrap_or_default();
    let idx = index as usize;
    if idx >= playlists.len() { return -1; }
    playlists[idx].id
}

/// Create a new empty playlist with `name`.
///
/// Writes `~/.config/sparkamp/playlists/<name>.m3u8` and registers it in the
/// library DB.  Returns the new playlist row id, or -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_create_playlist(
    ctx: *mut SparkampCtx,
    name: *const c_char,
) -> i64 {
    if ctx.is_null() || name.is_null() { return -1; }
    let ctx = &mut *ctx;
    let ext = ctx.config.media_library.playlist_format.extension();
    let Some(ml) = &ctx.media_library else { return -1 };
    let Ok(name_str) = CStr::from_ptr(name).to_str() else { return -1 };
    match ml.create_playlist(name_str, ext) {
        Ok(id) => id,
        Err(e) => { eprintln!("[sparkamp] create_playlist: {e}"); -1 }
    }
}

/// Append raw track paths to an existing saved playlist's file
/// (`.m3u8` or legacy `.m3u`).  Each entry gets an `#EXTINF` line.
///
/// Used by the active-playlist right-click "Add to Playlist" menu so the
/// user can grow a saved playlist with the currently-selected rows.  The
/// `paths` array must contain `count` valid null-terminated UTF-8 C
/// strings.  No-op if any pointer is null or `count <= 0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_append_paths_to_playlist(
    ctx: *mut SparkampCtx,
    playlist_id: i64,
    paths: *const *const c_char,
    count: c_int,
) {
    if ctx.is_null() || paths.is_null() || count <= 0 { return; }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return };
    let mut owned: Vec<String> = Vec::with_capacity(count as usize);
    for i in 0..count as isize {
        let p = *paths.offset(i);
        if p.is_null() { return; }
        if let Ok(s) = CStr::from_ptr(p).to_str() {
            owned.push(s.to_string());
        }
    }
    if let Err(e) = ml.append_paths_to_playlist(playlist_id, &owned) {
        eprintln!("[sparkamp] append_paths_to_playlist {playlist_id}: {e}");
    }
}

/// Write a playlist `.m3u8` file at exactly `target_path` (caller-chosen
/// directory + filename) populated with `paths`, then register it in the
/// library.  Each path is looked up in the library so the file gets
/// `#EXTINF` lines (duration / artist / title) for every track the library
/// has metadata for; unknown paths get a `-1` EXTINF + filename fallback.
///
/// Use this for the macOS NSSavePanel "Save As…" flow when the user picks
/// a destination outside Sparkamp's managed playlists folder.  Returns
/// the new playlist row id, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_save_playlist_to_path(
    ctx: *mut SparkampCtx,
    target_path: *const c_char,
    paths: *const *const c_char,
    count: c_int,
) -> i64 {
    if ctx.is_null() || target_path.is_null() || count < 0 { return -1; }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return -1 };
    let Ok(target) = CStr::from_ptr(target_path).to_str() else { return -1 };
    let track_paths: Vec<String> = if paths.is_null() || count == 0 {
        Vec::new()
    } else {
        let slice = std::slice::from_raw_parts(paths, count as usize);
        slice.iter()
            .filter_map(|&p| if p.is_null() {
                None
            } else {
                CStr::from_ptr(p).to_str().ok().map(|s| s.to_owned())
            })
            .collect()
    };
    match ml.save_playlist_tracks_to_path(Path::new(target), &track_paths) {
        Ok(id) => id,
        Err(e) => { eprintln!("[sparkamp] save_playlist_to_path: {e}"); -1 }
    }
}

/// Delete the playlist with `id` from the DB.  The playlist file is not removed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_delete_playlist(
    ctx: *mut SparkampCtx,
    playlist_id: i64,
) {
    if ctx.is_null() { return; }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return };
    if let Err(e) = ml.remove_playlist(playlist_id) {
        eprintln!("[sparkamp] delete_playlist {playlist_id}: {e}");
    }
}

/// Rename playlist `id`.  Updates both the DB record and the playlist file
/// on disk (extension preserved — legacy `.m3u` stays `.m3u`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_rename_playlist(
    ctx: *mut SparkampCtx,
    playlist_id: i64,
    new_name: *const c_char,
) {
    if ctx.is_null() || new_name.is_null() { return; }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return };
    let Ok(name_str) = CStr::from_ptr(new_name).to_str() else { return };
    if let Err(e) = ml.rename_playlist(playlist_id, name_str) {
        eprintln!("[sparkamp] rename_playlist {playlist_id}: {e}");
    }
}

/// Overwrite playlist `id` with the given track IDs (in order).
///
/// Writes the new track list to the playlist file on disk (`.m3u8` or legacy
/// `.m3u`), emitting `#EXTINF` metadata per entry.  Track IDs not found in
/// the library are silently skipped.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_save_playlist(
    ctx: *mut SparkampCtx,
    playlist_id: i64,
    track_ids: *const i64,
    count: c_int,
) {
    if ctx.is_null() || track_ids.is_null() || count < 0 { return; }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return };
    let ids = std::slice::from_raw_parts(track_ids, count as usize);
    if let Err(e) = ml.save_playlist_tracks(playlist_id, ids) {
        eprintln!("[sparkamp] save_playlist {playlist_id}: {e}");
    }
}

/// Create a new playlist named `new_name` and write the given track paths to
/// it (in order).  Unlike `sparkamp_ml_save_playlist`, this accepts raw path
/// strings so that missing/stub entries are preserved verbatim.
///
/// `paths` is a pointer to `count` C-string pointers (null-terminated).
/// Returns the new playlist row id, or -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_save_playlist_as(
    ctx: *mut SparkampCtx,
    new_name: *const c_char,
    paths: *const *const c_char,
    count: c_int,
) -> i64 {
    if ctx.is_null() || new_name.is_null() || count < 0 { return -1; }
    let ctx = &mut *ctx;
    let ext = ctx.config.media_library.playlist_format.extension();
    let Some(ml) = &ctx.media_library else { return -1 };
    let Ok(name_str) = CStr::from_ptr(new_name).to_str() else { return -1 };
    let track_paths: Vec<String> = if paths.is_null() || count == 0 {
        Vec::new()
    } else {
        let slice = std::slice::from_raw_parts(paths, count as usize);
        slice.iter()
            .filter_map(|&p| if p.is_null() { None } else { CStr::from_ptr(p).to_str().ok().map(|s| s.to_owned()) })
            .collect()
    };
    match ml.save_playlist_tracks_as(name_str, &track_paths, ext) {
        Ok(id) => id,
        Err(e) => { eprintln!("[sparkamp] save_playlist_as: {e}"); -1 }
    }
}

/// Return 1 if the playlist lives in Sparkamp's managed playlists directory,
/// 0 if it is an external playlist (scanned from a watched folder).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_playlist_is_managed(
    ctx: *const SparkampCtx,
    playlist_id: i64,
) -> c_int {
    if ctx.is_null() { return 0; }
    let ctx = &*ctx;
    let Some(ml) = &ctx.media_library else { return 0 };
    if ml.playlist_is_managed(playlist_id) { 1 } else { 0 }
}

/// Return the file path of the playlist as a heap-allocated C string.
///
/// Caller must free with `sparkamp_free_string`.  Returns null on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_playlist_path(
    ctx: *const SparkampCtx,
    playlist_id: i64,
) -> *mut c_char {
    if ctx.is_null() { return std::ptr::null_mut(); }
    let ctx = &*ctx;
    let Some(ml) = &ctx.media_library else { return std::ptr::null_mut(); };
    match ml.playlist_by_id(playlist_id) {
        Ok(pl) => CString::new(pl.path.as_str())
            .map(|s| s.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Fill `buf` with up to `limit` tracks from playlist `playlist_id`.
///
/// Returns the number of tracks written.  Returns 0 on error or if the
/// playlist is empty.  Caller must allocate `buf` with at least `limit`
/// elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_get_playlist_tracks(
    ctx: *const SparkampCtx,
    playlist_id: i64,
    buf: *mut SparkampLibTrack,
    limit: c_int,
) -> c_int {
    if ctx.is_null() || buf.is_null() || limit <= 0 { return 0; }
    let ctx = &*ctx;
    let Some(ml) = &ctx.media_library else { return 0 };
    let pl = match ml.playlist_by_id(playlist_id) {
        Ok(p) => p,
        Err(_) => return 0,
    };
    let tracks = ml.load_playlist_tracks(&pl).unwrap_or_default();
    let n = tracks.len().min(limit as usize);
    let slice = std::slice::from_raw_parts_mut(buf, n);
    for (i, t) in tracks[..n].iter().enumerate() {
        slice[i] = SparkampLibTrack::from_lib_track(t);
    }
    n as c_int
}

/// Returns 1 if the file at playlist index `index` is missing from disk.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_file_missing(
    ctx: *const SparkampCtx,
    index: c_int,
) -> c_int {
    if ctx.is_null() { return 0; }
    let ctx = &*ctx;
    let i = index as usize;
    if i >= ctx.playlist.tracks.len() { return 0; }
    let path = std::path::Path::new(&ctx.playlist.tracks[i].path);
    // A track on a mounted disc is there by definition — the mount is what
    // makes it visible, and losing the disc loses the whole volume, which the
    // drive list reports on its own. Answering from that instead of `stat`
    // matters because the macOS frontend calls this once PER ROW on every
    // playlist rebuild, on the UI thread: adding one track to a playlist of
    // disc tracks fired a syscall at the optical drive for each of them, and
    // the head leaving the stream to service them was audible as a skip in the
    // track already playing. GTK never had this — its equivalent marker comes
    // from the background `file_status` worker, and only for rows on screen.
    if crate::disc::detect::path_is_on_optical_media(path) {
        return 0;
    }
    if path.exists() { 0 } else { 1 }
}

/// Record a play event for the track at `path`.
///
/// Increments the play count and updates `last_played` in the library DB.
/// No-op if the ML is not open or the path is not in the DB.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_record_play(
    ctx: *mut SparkampCtx,
    path: *const c_char,
) {
    if ctx.is_null() || path.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return };
    if let Ok(p) = CStr::from_ptr(path).to_str() {
        let _ = ml.record_play(p);
    }
}

/// Force a single track to be re-scanned (tags + duration upserted into the
/// library DB).  Used after the ID3 editor saves so the Files view shows the
/// new metadata without a full library rescan.  No-op when ML is not open
/// or the file is missing / not in a watched folder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_rescan_track(
    ctx: *mut SparkampCtx,
    path: *const c_char,
) {
    if ctx.is_null() || path.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return };
    if let Ok(p) = CStr::from_ptr(path).to_str() {
        if let Err(e) = ml.rescan_track(p) {
            eprintln!("[sparkamp] rescan_track {p}: {e}");
        }
    }
}

/// Add a batch of file paths to the library DB.  Each path is upserted
/// under the deepest watched folder whose path is its prefix; paths that
/// don't fall inside any watched folder are silently skipped.  Returns
/// the number of paths actually inserted/updated.
///
/// Used by the macOS frontend when the user drags tracks onto the Files
/// view (scenarios 5 & 8): we add the files to the library DB but do
/// NOT register a new watched folder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_add_files(
    ctx: *mut SparkampCtx,
    paths: *const *const c_char,
    count: i32,
) -> i32 {
    if ctx.is_null() || paths.is_null() || count <= 0 {
        return 0;
    }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return 0 };
    let slice = std::slice::from_raw_parts(paths, count as usize);
    let mut owned: Vec<String> = Vec::with_capacity(slice.len());
    for &p in slice {
        if p.is_null() { continue }
        if let Ok(s) = CStr::from_ptr(p).to_str() {
            owned.push(s.to_owned());
        }
    }
    match ml.add_files_to_library(&owned) {
        Ok(n) => n as i32,
        Err(e) => {
            eprintln!("[sparkamp] add_files_to_library: {e}");
            0
        }
    }
}

#[cfg(test)]
mod album_gallery_tests {
    use super::*;
    use crate::media_library::{AlbumGroup, AlbumSort};

    fn decode(buf: &[u8]) -> &str {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        std::str::from_utf8(&buf[..end]).unwrap()
    }

    #[test]
    fn album_sort_from_u32_maps_known_values_and_defaults_to_artist() {
        assert_eq!(album_sort_from_u32(0), AlbumSort::Artist);
        assert_eq!(album_sort_from_u32(1), AlbumSort::Album);
        assert_eq!(album_sort_from_u32(2), AlbumSort::Year);
        assert_eq!(album_sort_from_u32(99), AlbumSort::Artist);
    }

    #[test]
    fn from_group_round_trips_a_normal_album_with_year_and_artwork() {
        let g = AlbumGroup {
            album: "Best Hits".to_string(),
            album_artist: "Artist A".to_string(),
            year: Some(1999),
            track_count: 2,
            artwork_path: Some("/art/best-hits.jpg".to_string()),
            is_no_album: false,
        };
        let ffi = SparkampAlbum::from_group(&g);
        assert_eq!(decode(&ffi.album), "Best Hits");
        assert_eq!(decode(&ffi.album_artist), "Artist A");
        assert_eq!(decode(&ffi.artwork_path), "/art/best-hits.jpg");
        assert_eq!(ffi.year, 1999);
        assert_eq!(ffi.has_year, 1);
        assert_eq!(ffi.track_count, 2);
        assert_eq!(ffi.is_no_album, 0);
    }

    #[test]
    fn from_group_round_trips_the_no_album_bucket() {
        let g = AlbumGroup {
            album: String::new(),
            album_artist: String::new(),
            year: None,
            track_count: 5,
            artwork_path: None,
            is_no_album: true,
        };
        let ffi = SparkampAlbum::from_group(&g);
        assert_eq!(decode(&ffi.album), "");
        assert_eq!(decode(&ffi.album_artist), "");
        assert_eq!(decode(&ffi.artwork_path), "");
        assert_eq!(ffi.year, 0);
        assert_eq!(ffi.has_year, 0);
        assert_eq!(ffi.track_count, 5);
        assert_eq!(ffi.is_no_album, 1);
    }

    #[test]
    fn from_group_truncates_and_nul_terminates_an_oversized_album_name() {
        let long_name = "x".repeat(300); // exceeds the 256-byte album buffer
        let g = AlbumGroup {
            album: long_name.clone(),
            album_artist: "Band".to_string(),
            year: Some(2020),
            track_count: 1,
            artwork_path: None,
            is_no_album: false,
        };
        let ffi = SparkampAlbum::from_group(&g);
        // Truncated to fit dst.len() - 1 bytes, then NUL-terminated.
        assert_eq!(decode(&ffi.album).len(), 255);
        assert!(long_name.starts_with(decode(&ffi.album)));
        assert_eq!(ffi.album[255], 0);
    }
}


#[cfg(test)]
mod probe_gating_tests {
    use super::*;
    use crate::model::Track;

    fn row(duration: Option<std::time::Duration>) -> Track {
        Track {
            path: std::path::PathBuf::from("/music/a.mp3"),
            title: "Song".to_string(),
            artist: "Artist".to_string(),
            album_artist: "Various".to_string(),
            album: "Album".to_string(),
            duration,
            broken: false,
            read_only: false,
            id: 1,
        }
    }

    /// A row the library described needs nothing read off disk. This is the
    /// whole point: the mac bridge used to spawn two rayon tasks per track for
    /// answers already in hand, ~24 ms each on cold rotational storage.
    #[test]
    fn a_row_with_a_duration_is_not_probed() {
        assert!(!needs_probe(&row(Some(std::time::Duration::from_secs(210)))));
    }

    /// The fast insert writes path and filename and nothing else, so an
    /// unscanned row has no duration — that is the one that must be read.
    #[test]
    fn a_row_without_a_duration_is_probed() {
        assert!(needs_probe(&row(None)));
    }

    /// Title is never the signal: `Track::from` falls back to the filename, so
    /// it is never empty and gating on it would probe nothing at all.
    #[test]
    fn a_blank_title_alone_does_not_trigger_a_probe() {
        let mut t = row(Some(std::time::Duration::from_secs(1)));
        t.title = String::new();
        assert!(
            !needs_probe(&t),
            "duration is the signal; a title can be blank on a fully scanned row"
        );
    }
}

#[cfg(test)]
mod bitrate_mode_tests {
    use super::*;

    /// The bitrate mode reaches Swift whole, in today's words.
    ///
    /// `copy_str` reserves a byte for the terminator, so an eight-byte buffer
    /// holds seven characters and "Variable" arrived as "Variabl". The buffer
    /// and the C header both have to be wide enough for the longest value this
    /// field can carry.
    #[test]
    fn a_variable_bitrate_mode_survives_the_ffi_buffer() {
        let mut t = crate::media_library::LibTrack::default();
        t.path = "/tmp/x.flac".to_string();
        t.bitrate_mode = Some("Variable".to_string());
        let out = SparkampLibTrack::from_lib_track(&t);
        let end = out
            .bitrate_mode
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(out.bitrate_mode.len());
        assert_eq!(
            std::str::from_utf8(&out.bitrate_mode[..end]).unwrap(),
            "Variable"
        );
    }

    /// A row scanned before the mode was generalised crosses in words too,
    /// rather than making the Swift side translate abbreviations of its own.
    #[test]
    fn a_legacy_abbreviation_crosses_as_a_word() {
        let mut t = crate::media_library::LibTrack::default();
        t.path = "/tmp/x.mp3".to_string();
        t.bitrate_mode = Some("VBR".to_string());
        let out = SparkampLibTrack::from_lib_track(&t);
        let end = out
            .bitrate_mode
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(out.bitrate_mode.len());
        assert_eq!(
            std::str::from_utf8(&out.bitrate_mode[..end]).unwrap(),
            "Variable"
        );
    }
}
