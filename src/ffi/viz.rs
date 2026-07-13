//! Visualizer data feeds (spectrum / waveform) and visualizer configuration:
//! mode, waveform style, bars zones, waveform zones.
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};

use super::SparkampCtx;

// ---------------------------------------------------------------------------
// Visualizer data
// ---------------------------------------------------------------------------

/// Fill `out` with `len` spectrum display-band amplitudes, normalised to 0–1.
///
/// `len` should equal `sparkamp_get_spectrum_bands()`.  Returns zeros when no
/// audio data is available.  Caller provides the output buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_spectrum(
    ctx: *const SparkampCtx,
    out: *mut f32,
    len: c_int,
) {
    if ctx.is_null() || out.is_null() || len <= 0 {
        return;
    }
    let ctx = &*ctx;
    let n = len as usize;
    let bands = ctx.player.get_spectrum_display_bands(n as u32);
    let slice = std::slice::from_raw_parts_mut(out, n);
    for (dst, src) in slice.iter_mut().zip(bands.iter()) {
        *dst = *src as f32;
    }
}

/// Return the number of spectrum display bands currently configured.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_spectrum_bands(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 16;
    }
    (*ctx).config.visualizer.display_bands as c_int
}

/// Fill `out` with `len` waveform PCM samples in `[-1, 1]`.
///
/// Returns zeros when not enough audio has been buffered yet.
/// Caller provides the output buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_waveform(
    ctx: *const SparkampCtx,
    out: *mut f32,
    len: c_int,
) {
    if ctx.is_null() || out.is_null() || len <= 0 {
        return;
    }
    let ctx = &*ctx;
    let n = len as usize;
    let samples = ctx.player.get_waveform_samples(n);
    let slice = std::slice::from_raw_parts_mut(out, n);
    for (dst, src) in slice.iter_mut().zip(samples.iter()) {
        *dst = *src as f32;
    }
}

/// Render one frame of the Granite plasma visualizer into a caller-owned
/// RGBA8 buffer.
///
/// `out` must point to at least `(w * h * 4)` bytes. Pass the same `(w, h)`
/// across calls; if the caller resizes the viewport, the renderer drops its
/// previous-frame buffer and the trail effect restarts.
///
/// Safe to call when paused/stopped — the buffer fades to black.
/// No-op on null `ctx` or null `out`.
///
/// `dt` is the elapsed time since the previous frame in 30 fps frame units
/// (1.0 = 33 ms; pass `elapsed_seconds * 30.0`). The plasma's speed and
/// trail feel stay identical at any refresh rate — a 60 fps caller passes
/// ~0.5. Values are clamped to a sane range internally.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_render_granite(
    ctx: *mut SparkampCtx,
    out: *mut u8,
    w: u32,
    h: u32,
    dt: f32,
) {
    if ctx.is_null() || out.is_null() || w == 0 || h == 0 {
        return;
    }
    let ctx = &mut *ctx;
    let len = (w as usize).saturating_mul(h as usize).saturating_mul(4);
    let dst = std::slice::from_raw_parts_mut(out, len);
    let cfg = ctx.config.visualizer.granite;
    ctx.player.render_granite(dst, w, h, &cfg, dt);
}

// ---------------------------------------------------------------------------
// Visualizer mode
// ---------------------------------------------------------------------------

/// Return the current visualizer mode: 0 = Bars, 1 = Waveform, 2 = Granite.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_viz_mode(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    match (*ctx).config.visualizer.mode {
        crate::config::VisualizerMode::Bars     => 0,
        crate::config::VisualizerMode::Waveform => 1,
        crate::config::VisualizerMode::Granite  => 2,
    }
}

/// Set the visualizer mode. 0 = Bars, 1 = Waveform, 2 = Granite.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_viz_mode(ctx: *mut SparkampCtx, mode: c_int) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.visualizer.mode = match mode {
        1 => crate::config::VisualizerMode::Waveform,
        2 => crate::config::VisualizerMode::Granite,
        _ => crate::config::VisualizerMode::Bars,
    };
}

/// Cycle visualizer mode: Bars → Waveform → Granite → Bars → …
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_cycle_viz_mode(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.visualizer.mode = match ctx.config.visualizer.mode {
        crate::config::VisualizerMode::Bars     => crate::config::VisualizerMode::Waveform,
        crate::config::VisualizerMode::Waveform => crate::config::VisualizerMode::Granite,
        crate::config::VisualizerMode::Granite  => crate::config::VisualizerMode::Bars,
    };
}

/// Return whether bars mirror mode is enabled (bar extends above and below center).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_viz_mirror(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return true;
    }
    (*ctx).config.visualizer.bars_mirror
}

/// Set bars mirror mode. `true` = mirrored (above+below center), `false` = normal (grow from bottom).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_viz_mirror(ctx: *mut SparkampCtx, mirror: bool) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.visualizer.bars_mirror = mirror;
}

// ---------------------------------------------------------------------------
// Waveform style
// ---------------------------------------------------------------------------

/// Return the waveform rendering style: 0 = Lines, 1 = Filled.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_waveform_style(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    match (*ctx).config.visualizer.waveform_style {
        crate::config::WaveformStyle::Lines => 0,
        crate::config::WaveformStyle::Filled => 1,
    }
}

/// Set the waveform rendering style. 0 = Lines, 1 = Filled.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_waveform_style(ctx: *mut SparkampCtx, style: c_int) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.visualizer.waveform_style = match style {
        1 => crate::config::WaveformStyle::Filled,
        _ => crate::config::WaveformStyle::Lines,
    };
}

// ---------------------------------------------------------------------------
// Bars zone config
// ---------------------------------------------------------------------------

/// Return the number of color zones for the bars visualizer (1–6).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_viz_zones(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 5;
    }
    (*ctx).config.visualizer.color_zones as c_int
}

/// Set the number of color zones for the bars visualizer (clamped to 1–6).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_viz_zones(ctx: *mut SparkampCtx, count: c_int) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.visualizer.color_zones = (count as u8).clamp(1, 6);
}

/// Return the hex color string for bars zone `zone_index` (0 = bottom zone).
///
/// Caller must free the returned string with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_zone_color(
    ctx: *const SparkampCtx,
    zone_index: c_int,
) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let i = zone_index as usize;
    let color = ctx
        .config
        .visualizer
        .zone_colors
        .get(i)
        .cloned()
        .unwrap_or_else(|| "#006600".to_string());
    CString::new(color).unwrap_or_default().into_raw()
}

/// Set the hex color for bars zone `zone_index`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_zone_color(
    ctx: *mut SparkampCtx,
    zone_index: c_int,
    hex: *const c_char,
) {
    if ctx.is_null() || hex.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let i = zone_index as usize;
    let s = CStr::from_ptr(hex).to_string_lossy().into_owned();
    if i < ctx.config.visualizer.zone_colors.len() {
        ctx.config.visualizer.zone_colors[i] = s;
    }
}

// ---------------------------------------------------------------------------
// Waveform zone config
// ---------------------------------------------------------------------------

/// Return the number of color zones for the waveform visualizer (1–6).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_waveform_zones(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 5;
    }
    (*ctx).config.visualizer.waveform_color_zones as c_int
}

/// Set the number of color zones for the waveform visualizer (clamped to 1–6).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_waveform_zones(ctx: *mut SparkampCtx, count: c_int) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.visualizer.waveform_color_zones = (count as u8).clamp(1, 6);
}

/// Return the hex color string for waveform zone `zone_index` (0 = bottom zone).
///
/// Caller must free the returned string with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_waveform_zone_color(
    ctx: *const SparkampCtx,
    zone_index: c_int,
) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let i = zone_index as usize;
    let color = ctx
        .config
        .visualizer
        .waveform_zone_colors
        .get(i)
        .cloned()
        .unwrap_or_else(|| "#006600".to_string());
    CString::new(color).unwrap_or_default().into_raw()
}

/// Set the hex color for waveform zone `zone_index`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_waveform_zone_color(
    ctx: *mut SparkampCtx,
    zone_index: c_int,
    hex: *const c_char,
) {
    if ctx.is_null() || hex.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let i = zone_index as usize;
    let s = CStr::from_ptr(hex).to_string_lossy().into_owned();
    if i < ctx.config.visualizer.waveform_zone_colors.len() {
        ctx.config.visualizer.waveform_zone_colors[i] = s;
    }
}

