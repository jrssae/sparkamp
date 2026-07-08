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

/// The configured gnudb submission email, or "" when effectively unset
/// (blank, or the retired app-wide default an older config may carry) — the
/// frontends prompt for a real address before the first submission. Heap C
/// string — free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_gnudb_email(ctx: *const SparkampCtx) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let email = &ctx.config.disc.gnudb_email;
    let out = if crate::disc::gnudb::is_unset_email(email) {
        ""
    } else {
        email.as_str()
    };
    CString::new(out)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Set the gnudb email (ignored when empty after trimming).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_gnudb_email(ctx: *mut SparkampCtx, email: *const c_char) {
    if ctx.is_null() || email.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let s = std::ffi::CStr::from_ptr(email)
        .to_string_lossy()
        .trim()
        .to_string();
    if !s.is_empty() {
        ctx.config.disc.gnudb_email = s;
    }
}

/// Whether gnudb submissions run in test mode (validated, not published).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_gnudb_submit_test(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return true;
    }
    let ctx = &*ctx;
    ctx.config.disc.gnudb_submit_mode_test
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_gnudb_submit_test(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.disc.gnudb_submit_mode_test = value;
}

/// Last chosen rip destination directory ("" when unset — the UI then
/// defaults to the first watched folder and prompts before the first rip).
/// Heap C string — free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_rip_dest(ctx: *const SparkampCtx) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let s = ctx
        .config
        .disc
        .rip_dest_dir
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    CString::new(s)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_rip_dest(ctx: *mut SparkampCtx, dir: *const c_char) {
    if ctx.is_null() || dir.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let s = std::ffi::CStr::from_ptr(dir).to_string_lossy().trim().to_string();
    ctx.config.disc.rip_dest_dir = if s.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(s))
    };
}

/// MP3 rip preset: 0 = VBR V0, 1 = VBR V2 (default), 2 = 320 CBR.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_rip_quality(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 1;
    }
    (&*ctx).config.disc.rip_mp3_quality as c_int
}

/// Verify discs after burning where the tool supports it (default true).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_burn_verify(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return true;
    }
    (&*ctx).config.disc.burn_verify
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_burn_verify(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.disc.burn_verify = value;
}

/// Auto-open the Media Library to a drive when it receives an audio CD
/// (default true). Only takes effect once the app is running — OS-level
/// default-handler registration is a separate manual step.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_auto_show_inserted_cd(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return true;
    }
    (&*ctx).config.disc.auto_show_inserted_audio_cd
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_auto_show_inserted_cd(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.disc.auto_show_inserted_audio_cd = value;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_rip_quality(ctx: *mut SparkampCtx, preset: c_int) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.disc.rip_mp3_quality = match preset {
        0 => 0,
        2 => 2,
        _ => 1,
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

