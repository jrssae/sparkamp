//! Deduplication FFI — C-compatible duplicate-group structs, opaque scan
//! context, and the background dedup scan.
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::model::Track;

use super::SparkampCtx;

// ---------------------------------------------------------------------------
// Deduplication — C-compatible structs and opaque context
// ---------------------------------------------------------------------------

/// A single track entry inside a duplicate group.
#[repr(C)]
pub struct SparkampDedupTrack {
    pub path: [u8; 512],
    pub title: [u8; 256],
    pub artist: [u8; 256],
    pub duration_secs: f64,
}

/// A group of duplicate tracks found by the deduplication scan.
///
/// `tracks` points to a heap-allocated array of `track_count` elements.
/// The array is owned by the `SparkampDedupCtx` and must **not** be freed
/// by the caller; it is freed when `sparkamp_dedup_free` is called.
#[repr(C)]
pub struct SparkampDedupGroup {
    /// 0 = Probable duplicate, 1 = Less likely duplicate.
    pub confidence: c_int,
    pub track_count: c_int,
    /// Pointer to a heap-allocated array; valid until `sparkamp_dedup_free`.
    pub tracks: *mut SparkampDedupTrack,
}

/// Opaque context for a deduplication scan.
#[allow(dead_code)]
pub struct SparkampDedupCtx {
    cancel: Arc<AtomicBool>,
    /// Dismissed track paths ("not a duplicate").
    dismissed: Mutex<std::collections::HashSet<String>>,
}

// ---------------------------------------------------------------------------
// Deduplication — FFI
// ---------------------------------------------------------------------------

/// Start a deduplication scan in the background.
///
/// Loads all scanned tracks from the media library, then calls
/// `find_duplicates()` on a Rayon thread.
///
/// - `group_cb(userdata, group)` fires for each group found.  The `group`
///   pointer is valid only for the duration of the callback — copy any data
///   you need before returning.
/// - `done_cb(userdata, group_count)` fires when the scan finishes.
///
/// Returns an opaque `SparkampDedupCtx*` that must be freed with
/// `sparkamp_dedup_free`.  Returns null if the ML is not open.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_dedup_start(
    ctx: *mut SparkampCtx,
    group_cb: Option<unsafe extern "C" fn(*mut c_void, *const SparkampDedupGroup)>,
    done_cb: Option<unsafe extern "C" fn(*mut c_void, c_int)>,
    userdata: *mut c_void,
) -> *mut SparkampDedupCtx {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &mut *ctx;
    let Some(ml) = &ctx.media_library else {
        return std::ptr::null_mut();
    };

    // Load all tracks that have been scanned (have metadata).
    let lib_tracks = match ml.scanned_tracks() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[sparkamp_dedup_start] {e}");
            return std::ptr::null_mut();
        }
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let dedup_ctx = Box::new(SparkampDedupCtx {
        cancel: Arc::clone(&cancel),
        dismissed: Mutex::new(std::collections::HashSet::new()),
    });
    let dedup_ptr = Box::into_raw(dedup_ctx);

    let ud_addr = userdata as usize;

    rayon::spawn(move || {
        let ud: *mut c_void = ud_addr as *mut c_void;
        let groups = crate::dedupe::find_duplicates(lib_tracks);
        let mut total = 0i32;

        for group in &groups {
            if cancel.load(Ordering::Relaxed) {
                break;
            }

            fn copy(dst: &mut [u8], src: &str) {
                let b = src.as_bytes();
                let n = b.len().min(dst.len() - 1);
                dst[..n].copy_from_slice(&b[..n]);
                dst[n] = 0;
            }

            // Build C struct on the stack for the callback.
            let mut c_tracks: Vec<SparkampDedupTrack> = group
                .tracks
                .iter()
                .map(|info| {
                    let t = &info.track;
                    let mut ct = SparkampDedupTrack {
                        path: [0u8; 512],
                        title: [0u8; 256],
                        artist: [0u8; 256],
                        duration_secs: t.length_secs.unwrap_or(0.0),
                    };
                    copy(&mut ct.path, &t.path);
                    copy(
                        &mut ct.title,
                        t.title.as_deref().unwrap_or(&t.filename),
                    );
                    copy(&mut ct.artist, t.artist.as_deref().unwrap_or(""));
                    ct
                })
                .collect();

            let confidence = match group.confidence {
                crate::dedupe::DupeConfidence::Probable => 0,
                crate::dedupe::DupeConfidence::LessLikely => 1,
            };

            let c_group = SparkampDedupGroup {
                confidence,
                track_count: c_tracks.len() as c_int,
                tracks: c_tracks.as_mut_ptr(),
            };

            if let Some(cb) = group_cb {
                unsafe { cb(ud, &c_group as *const _) };
            }

            total += 1;
        }

        if let Some(cb) = done_cb {
            unsafe { cb(ud, total) };
        }
    });

    dedup_ptr
}

/// Cancel a running deduplication scan.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_dedup_cancel(dedup_ctx: *mut SparkampDedupCtx) {
    if dedup_ctx.is_null() {
        return;
    }
    (*dedup_ctx).cancel.store(true, Ordering::Relaxed);
}

/// Free a deduplication context created by `sparkamp_dedup_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_dedup_free(dedup_ctx: *mut SparkampDedupCtx) {
    if dedup_ctx.is_null() {
        return;
    }
    drop(Box::from_raw(dedup_ctx));
}

/// Add all tracks in a group to the active playlist (append).
///
/// `paths` is a null-terminated array of C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_dedup_add_to_playlist(
    ctx: *mut SparkampCtx,
    paths: *const *const c_char,
    count: c_int,
) {
    if ctx.is_null() || paths.is_null() || count <= 0 {
        return;
    }
    let ctx = &mut *ctx;
    let path_ptrs = std::slice::from_raw_parts(paths, count as usize);
    for &ptr in path_ptrs {
        if ptr.is_null() {
            continue;
        }
        if let Ok(s) = CStr::from_ptr(ptr).to_str() {
            if let Ok(t) = Track::from_path_fast(Path::new(s)) {
                ctx.playlist.tracks.push(t);
            }
        }
    }
}

/// Replace the active playlist with all tracks in a group.
///
/// `paths` is a C array of `count` path strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_dedup_replace_playlist(
    ctx: *mut SparkampCtx,
    paths: *const *const c_char,
    count: c_int,
) {
    if ctx.is_null() || paths.is_null() || count <= 0 {
        return;
    }
    let ctx = &mut *ctx;
    ctx.playlist.tracks.clear();
    ctx.playlist.current_index = 0;
    let path_ptrs = std::slice::from_raw_parts(paths, count as usize);
    for &ptr in path_ptrs {
        if ptr.is_null() {
            continue;
        }
        if let Ok(s) = CStr::from_ptr(ptr).to_str() {
            if let Ok(t) = Track::from_path_fast(Path::new(s)) {
                ctx.playlist.tracks.push(t);
            }
        }
    }
}

/// Open the containing folder of `path` in Finder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_open_file_location(path: *const c_char) {
    if path.is_null() {
        return;
    }
    if let Ok(s) = CStr::from_ptr(path).to_str() {
        let p = Path::new(s);
        let dir = p.parent().unwrap_or(p);
        let _ = std::process::Command::new("open")
            .arg(dir.as_os_str())
            .spawn();
    }
}
