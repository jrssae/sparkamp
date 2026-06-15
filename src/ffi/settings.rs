//! Behavior / settings accessors and the read-only audio-extension list
//! (mirrors `model::AUDIO_EXTENSIONS`).
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::CString;
use std::os::raw::{c_char, c_int};

use super::SparkampCtx;

// ---------------------------------------------------------------------------
// Audio extensions (read-only, mirrors model::AUDIO_EXTENSIONS)
// ---------------------------------------------------------------------------
//
// Exposed so frontends building file pickers can use the canonical list
// instead of maintaining their own (drift-prone) copy.  Strings are static
// and null-terminated; the returned pointer is valid for the lifetime of
// the process and must not be freed.

/// Number of supported audio file extensions.
#[unsafe(no_mangle)]
pub extern "C" fn sparkamp_audio_extension_count() -> c_int {
    crate::model::AUDIO_EXTENSIONS.len() as c_int
}

/// Get the audio extension at `idx` as a null-terminated lowercase ASCII
/// string (no leading dot — e.g. "mp3", "flac").  Returns NULL if `idx` is
/// out of range.  The returned pointer is static and must not be freed.
#[unsafe(no_mangle)]
pub extern "C" fn sparkamp_audio_extension(idx: c_int) -> *const c_char {
    use std::sync::OnceLock;
    // OnceLock so each extension gets one stable CString pointer for the
    // process lifetime (callers may cache them).
    static CACHE: OnceLock<Vec<CString>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        crate::model::AUDIO_EXTENSIONS
            .iter()
            .map(|s| CString::new(*s).expect("audio extensions are static ASCII"))
            .collect()
    });
    if idx < 0 {
        return std::ptr::null();
    }
    cache
        .get(idx as usize)
        .map(|cs| cs.as_ptr())
        .unwrap_or(std::ptr::null())
}

// ---------------------------------------------------------------------------
// Behavior / Settings
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_playlist_add_behavior(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    match ctx.config.behavior.playlist_add_behavior {
        crate::config::PlaylistAddBehavior::Append => 0,
        crate::config::PlaylistAddBehavior::Replace => 1,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_playlist_add_behavior(
    ctx: *mut SparkampCtx,
    value: c_int,
) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.behavior.playlist_add_behavior = match value {
        1 => crate::config::PlaylistAddBehavior::Replace,
        _ => crate::config::PlaylistAddBehavior::Append,
    };
}

/// Preferred new-playlist format: 0 = m3u8 (default), 1 = m3u.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_playlist_format(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    match ctx.config.media_library.playlist_format {
        crate::config::PlaylistFormat::M3u8 => 0,
        crate::config::PlaylistFormat::M3u => 1,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_playlist_format(ctx: *mut SparkampCtx, value: c_int) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.media_library.playlist_format = match value {
        1 => crate::config::PlaylistFormat::M3u,
        _ => crate::config::PlaylistFormat::M3u8,
    };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_autoplay_on_add(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.config.behavior.autoplay_on_add
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_autoplay_on_add(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.behavior.autoplay_on_add = value;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_ml_rescan_interval(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    ctx.config.media_library.rescan_interval_mins as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_ml_rescan_interval(ctx: *mut SparkampCtx, mins: c_int) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.media_library.rescan_interval_mins = if mins <= 0 {
        0
    } else {
        (mins as u64).max(1)
    };
}

