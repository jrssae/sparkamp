//! Media Library FFI — C-compatible track struct, library lifecycle, folder
//! management, track queries, playlist operations and CRUD.
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;
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
    /// 1 if the file no longer exists at its recorded path; 0 otherwise.
    pub file_missing: c_int,
    /// ISO-8601 UTC timestamp of the last time this track was played
    /// ("YYYY-MM-DDTHH:MM:SSZ"), or empty string if never played.
    pub last_played: [u8; 32],
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
            file_missing: 0,
            last_played: [0u8; 32],
        };
        fn copy_str(dst: &mut [u8], src: &str) {
            let bytes = src.as_bytes();
            let n = bytes.len().min(dst.len() - 1);
            dst[..n].copy_from_slice(&bytes[..n]);
            dst[n] = 0;
        }
        copy_str(&mut out.path, &t.path);
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
        let p = std::path::Path::new(&t.path);
        out.read_only    = if crate::media_library::is_read_only(p) { 1 } else { 0 };
        out.file_missing = if p.exists() { 0 } else { 1 };
        out
    }
}

// ---------------------------------------------------------------------------
// Media Library — lifecycle
// ---------------------------------------------------------------------------

/// Open (or create) the media library database.
///
/// Must be called before any other `sparkamp_ml_*` function.  Safe to call
/// multiple times — subsequent calls are no-ops if the DB is already open.
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
    if let Err(e) = ml.rescan_folder_fast(folder_id, &path_str) {
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
    let Some(ml) = &ctx.media_library else { return };

    // Fast phase: re-discover any new files in all folders.
    let folders = ml.list_folders().unwrap_or_default();
    for (folder_id, folder_path) in &folders {
        if let Err(e) = ml.rescan_folder_fast(*folder_id, folder_path) {
            eprintln!("[sparkamp_ml_rescan_all] fast rescan {folder_path}: {e}");
        }
    }

    let cancel = Arc::clone(&ctx.ml_cancel);
    let scanning = Arc::clone(&ctx.ml_scanning);
    let progress_atomic = Arc::clone(&ctx.ml_progress);
    cancel.store(false, Ordering::Relaxed);
    scanning.store(true, Ordering::Relaxed);

    let ud_addr = userdata as usize;

    rayon::spawn(move || {
        let ud: *mut c_void = ud_addr as *mut c_void;
        let result = MediaLibrary::open_at(&MediaLibrary::db_path_pub()).and_then(|bg_ml| {
            let atomic = &progress_atomic;
            bg_ml.scan_all_folders(&cancel, |done, total| {
                let packed = ((total as u64) << 32) | (done as u64);
                atomic.store(packed, Ordering::Relaxed);
                if let Some(cb) = progress_cb {
                    unsafe { cb(ud, done as c_int, total as c_int) };
                }
            })
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
// Media Library — track queries
// ---------------------------------------------------------------------------

/// Return the number of tracks matching `query` (UTF-8 search string).
///
/// Pass an empty string or null to count all tracks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_track_count(
    ctx: *const SparkampCtx,
    query: *const c_char,
) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    let Some(ml) = &ctx.media_library else { return 0 };
    let q = if query.is_null() {
        ""
    } else {
        match CStr::from_ptr(query).to_str() {
            Ok(s) => s,
            Err(_) => "",
        }
    };
    let tracks = if q.is_empty() {
        ml.all_tracks()
    } else {
        ml.search_tracks(q)
    };
    tracks.map(|v| v.len() as c_int).unwrap_or(0)
}

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

    // Fetch all tracks then filter by id — avoids N individual queries.
    let all = ml.all_tracks().unwrap_or_default();
    let by_id: std::collections::HashMap<i64, &crate::media_library::LibTrack> =
        all.iter().map(|t| (t.id, t)).collect();

    let start_idx = ctx.playlist.tracks.len();
    for &id in id_slice {
        if let Some(t) = by_id.get(&id) {
            // Build the active-playlist Track directly from the ML row so
            // duration + tags are inherited synchronously.  The background
            // probe below still runs to refine values for tracks the ML
            // hasn't scanned yet (length_secs == None) and to catch any
            // file-vs-DB drift.
            ctx.playlist.tracks.push(Track::from(*t));
        }
    }
    // Kick off metadata + duration probing for the newly added tracks.
    let n = ctx.playlist.tracks.len();
    for idx in start_idx..n {
        let meta_tx = ctx.meta_tx.clone();
        let duration_tx = ctx.duration_tx.clone();
        let path = ctx.playlist.tracks[idx].path.clone();
        rayon::spawn(move || {
            if let Ok(track) = crate::model::Track::from_path(&path) {
                let _ = meta_tx.send((
                    idx,
                    track.title.clone(),
                    track.artist.clone(),
                    track.album_artist.clone(),
                ));
            }
        });
        let path2 = ctx.playlist.tracks[idx].path.clone();
        rayon::spawn(move || {
            if let Some(dur) = crate::duration_probe::probe_duration(&path2) {
                let _ = duration_tx.send((idx, dur));
            }
        });
    }
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
    // Kick off background metadata + duration probing.
    for (i, t) in tracks.iter().enumerate() {
        let meta_tx = ctx.meta_tx.clone();
        let duration_tx = ctx.duration_tx.clone();
        let path = std::path::PathBuf::from(&t.path);
        rayon::spawn(move || {
            if let Ok(track) = crate::model::Track::from_path(&path) {
                let _ = meta_tx.send((
                    i,
                    track.title.clone(),
                    track.artist.clone(),
                    track.album_artist.clone(),
                ));
            }
        });
        let path2 = std::path::PathBuf::from(&t.path);
        rayon::spawn(move || {
            if let Some(dur) = crate::duration_probe::probe_duration(&path2) {
                let _ = duration_tx.send((i, dur));
            }
        });
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
    let Some(ml) = &ctx.media_library else { return -1 };
    let Ok(name_str) = CStr::from_ptr(name).to_str() else { return -1 };
    match ml.create_playlist(name_str) {
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

/// Register an existing `.m3u` / `.m3u8` file on disk as a playlist in
/// the library.
///
/// Use after the frontend has written the file itself, so the new
/// playlist appears in the sidebar without a full library rescan.  Returns
/// the new playlist row id, or -1 on error (including malformed UTF-8 in
/// `path`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_ml_add_playlist_file(
    ctx: *mut SparkampCtx,
    path: *const c_char,
) -> i64 {
    if ctx.is_null() || path.is_null() { return -1; }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else { return -1 };
    let Ok(p) = CStr::from_ptr(path).to_str() else { return -1 };
    match ml.add_playlist_file(p) {
        Ok(id) => id,
        Err(e) => { eprintln!("[sparkamp] add_playlist_file: {e}"); -1 }
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
    match ml.save_playlist_tracks_as(name_str, &track_paths) {
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

