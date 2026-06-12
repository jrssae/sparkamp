//! Equalizer — enable, band gains, pre-amp, presets — plus the read-only
//! EQ/pre-amp limit constants mirrored from the core's clamp ranges.
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::CString;
use std::os::raw::{c_char, c_int};

use super::SparkampCtx;

// ---------------------------------------------------------------------------
// Equalizer
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_has_eq(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.player.has_eq()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_eq_enabled(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.config.equalizer.enabled
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_eq_enabled(ctx: *mut SparkampCtx, enabled: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.equalizer.enabled = enabled;
    if enabled {
        let bands = ctx.config.equalizer.effective_bands();
        ctx.player.apply_eq_bands(&bands);
        let preamp = ctx.config.equalizer.effective_preamp();
        ctx.player.set_preamp(preamp);
    } else {
        ctx.player.apply_eq_bands(&[0.0f64; 10]);
        ctx.player.set_preamp(1.0);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_eq_band(ctx: *const SparkampCtx, band: c_int) -> f32 {
    if ctx.is_null() || band < 0 || band >= 10 {
        return 0.0;
    }
    let ctx = &*ctx;
    // Read directly from the configured bands (not effective_bands which returns
    // all zeros when EQ is disabled) so the UI always shows the true values.
    let idx = band as usize;
    if idx < ctx.config.equalizer.bands.len() {
        ctx.config.equalizer.bands[idx] as f32
    } else {
        0.0
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_eq_band(ctx: *mut SparkampCtx, band: c_int, db: f32) {
    if ctx.is_null() || band < 0 || band >= 10 {
        return;
    }
    let ctx = &mut *ctx;
    let clamped = ctx.config.equalizer.set_band_gain(band as usize, db as f64);
    if ctx.config.equalizer.enabled {
        ctx.player.set_eq_band(band as usize, clamped);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_apply_eq_preset(ctx: *mut SparkampCtx, preset_index: c_int) {
    if ctx.is_null() || preset_index < 0 {
        return;
    }
    let idx = preset_index as usize;
    if idx >= crate::config::EQ_PRESETS.len() {
        return;
    }
    let ctx = &mut *ctx;
    let (name, bands) = crate::config::EQ_PRESETS[idx];
    ctx.config.equalizer.preset = name.to_string();
    ctx.config.equalizer.bands = bands.to_vec();
    if ctx.config.equalizer.enabled {
        ctx.player.apply_eq_bands(&bands);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_eq_preset_count(_ctx: *const SparkampCtx) -> c_int {
    crate::config::EQ_PRESETS.len() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_eq_preset_name(
    _ctx: *const SparkampCtx,
    preset_index: c_int,
) -> *mut c_char {
    if preset_index < 0 || preset_index as usize >= crate::config::EQ_PRESETS.len() {
        return CString::new("").unwrap().into_raw();
    }
    let name = crate::config::EQ_PRESETS[preset_index as usize].0;
    CString::new(name).unwrap_or_default().into_raw()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_preamp(ctx: *const SparkampCtx) -> f32 {
    if ctx.is_null() {
        return 1.0;
    }
    let ctx = &*ctx;
    ctx.config.equalizer.effective_preamp() as f32
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_preamp(ctx: *mut SparkampCtx, multiplier: f32) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.equalizer.preamp = (multiplier as f64).clamp(0.5, 1.5);
    if ctx.config.equalizer.enabled {
        ctx.player.set_preamp(ctx.config.equalizer.preamp);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_reset_eq(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.equalizer.bands = vec![0.0f64; 10];
    ctx.config.equalizer.preset = String::new();
    ctx.config.equalizer.preamp = 1.0;
    if ctx.config.equalizer.enabled {
        ctx.player.apply_eq_bands(&[0.0f64; 10]);
        ctx.player.set_preamp(1.0);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_eq_band_label(band: c_int) -> *mut c_char {
    if band < 0 || band as usize >= crate::config::EQ_BAND_FREQS.len() {
        return CString::new("").unwrap().into_raw();
    }
    let label = crate::config::EQ_BAND_FREQS[band as usize];
    CString::new(label).unwrap_or_default().into_raw()
}

// ---------------------------------------------------------------------------
// EQ / pre-amp limit constants (read-only, mirror core's clamp ranges)
// ---------------------------------------------------------------------------
//
// Exposed so frontends don't have to re-hardcode the same numeric ranges
// in their slider UIs and risk drifting from the core's clamp logic.

/// Maximum absolute dB value any EQ band can be set to (symmetric: ±limit).
#[unsafe(no_mangle)]
pub extern "C" fn sparkamp_eq_band_db_limit() -> f64 {
    crate::config::EQ_BAND_DB_LIMIT
}

/// Minimum allowed pre-amp multiplier.
#[unsafe(no_mangle)]
pub extern "C" fn sparkamp_preamp_min() -> f64 {
    crate::config::PREAMP_MIN
}

/// Maximum allowed pre-amp multiplier.
#[unsafe(no_mangle)]
pub extern "C" fn sparkamp_preamp_max() -> f64 {
    crate::config::PREAMP_MAX
}

