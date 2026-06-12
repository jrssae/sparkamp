//! Granite plasma visualizer settings — speed, palette, trails (feedback),
//! effects, auto-switching, beat sensitivity/brightness, BPM and meter
//! readouts.
#![allow(unsafe_op_in_unsafe_fn)]

use std::os::raw::c_int;

use super::SparkampCtx;

// ---------------------------------------------------------------------------
// Granite plasma settings (speed / palette / feedback)
// ---------------------------------------------------------------------------

/// Get Granite animation speed multiplier (clamped 0.1–5.0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_granite_speed(ctx: *const SparkampCtx) -> f32 {
    if ctx.is_null() {
        return 1.0;
    }
    (*ctx).config.visualizer.granite.speed
}

/// Set Granite animation speed (clamped 0.1–5.0 on read).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_granite_speed(ctx: *mut SparkampCtx, speed: f32) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.visualizer.granite.speed = speed.clamp(0.1, 5.0);
}

/// Get Granite palette: 0 = Granite, 1 = Fire, 2 = Neon.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_granite_palette(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    match (*ctx).config.visualizer.granite.palette {
        crate::granite::GranitePalette::Granite  => 0,
        crate::granite::GranitePalette::Fire     => 1,
        crate::granite::GranitePalette::Neon     => 2,
        crate::granite::GranitePalette::Ocean    => 3,
        crate::granite::GranitePalette::Violet   => 4,
        crate::granite::GranitePalette::Sunset   => 5,
        crate::granite::GranitePalette::Crt      => 6,
        crate::granite::GranitePalette::Spectrum => 7,
    }
}

/// Set Granite palette: 0 = Granite, 1 = Fire, 2 = Neon, 3 = Ocean,
/// 4 = Violet, 5 = Sunset, 6 = CRT, 7 = Spectrum.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_granite_palette(ctx: *mut SparkampCtx, palette: c_int) {
    if ctx.is_null() {
        return;
    }
    let chosen = match palette {
        1 => crate::granite::GranitePalette::Fire,
        2 => crate::granite::GranitePalette::Neon,
        3 => crate::granite::GranitePalette::Ocean,
        4 => crate::granite::GranitePalette::Violet,
        5 => crate::granite::GranitePalette::Sunset,
        6 => crate::granite::GranitePalette::Crt,
        7 => crate::granite::GranitePalette::Spectrum,
        _ => crate::granite::GranitePalette::Granite,
    };
    (*ctx).config.visualizer.granite.palette = chosen;
    // Push it into the live renderer too: with auto_switch on, the
    // scheduler owns the palette and the config value alone is never read.
    (*ctx).player.granite_set_palette(chosen);
}

/// Get Granite feedback strength (clamped 0.0–0.9).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_granite_feedback(ctx: *const SparkampCtx) -> f32 {
    if ctx.is_null() {
        return 0.35;
    }
    (*ctx).config.visualizer.granite.feedback
}

/// Set Granite feedback strength (clamped 0.0–0.9 on read).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_granite_feedback(ctx: *mut SparkampCtx, fb: f32) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.visualizer.granite.feedback = fb.clamp(0.0, 0.9);
}

/// Get Granite effect: 0 = Plasma, 1 = Tunnel, 2 = Swirl, 3 = Spin (radial
/// sweep), 4 = Cells, 5 = Explode, 6 = Ripple, 7 = Shear, 8 = Kaleido,
/// 9 = Gravity Well, 10 = Drain, 11 = Flag. When `auto_switch` is on, this
/// reflects the live scheduler state so the UI can show what's on screen.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_granite_effect(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let cfg = &(*ctx).config.visualizer.granite;
    let live = if cfg.auto_switch {
        (*ctx).player.granite_active_effect().unwrap_or(cfg.effect)
    } else {
        cfg.effect
    };
    match live {
        crate::granite::GraniteEffect::Plasma      => 0,
        crate::granite::GraniteEffect::Tunnel      => 1,
        crate::granite::GraniteEffect::Swirl       => 2,
        crate::granite::GraniteEffect::RadialSweep => 3,
        crate::granite::GraniteEffect::Cells       => 4,
        crate::granite::GraniteEffect::Explode     => 5,
        crate::granite::GraniteEffect::Ripple      => 6,
        crate::granite::GraniteEffect::Shear       => 7,
        crate::granite::GraniteEffect::Kaleido     => 8,
        crate::granite::GraniteEffect::GravityWell => 9,
        crate::granite::GraniteEffect::Drain       => 10,
        crate::granite::GraniteEffect::Flag        => 11,
    }
}

/// Set Granite effect. Same numbering as `sparkamp_get_granite_effect`.
/// When `auto_switch` is on, the scheduler's next switch is pushed out so
/// the user's selection stays visible for ~20 s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_granite_effect(ctx: *mut SparkampCtx, effect: c_int) {
    if ctx.is_null() {
        return;
    }
    let chosen = match effect {
        1  => crate::granite::GraniteEffect::Tunnel,
        2  => crate::granite::GraniteEffect::Swirl,
        3  => crate::granite::GraniteEffect::RadialSweep,
        4  => crate::granite::GraniteEffect::Cells,
        5  => crate::granite::GraniteEffect::Explode,
        6  => crate::granite::GraniteEffect::Ripple,
        7  => crate::granite::GraniteEffect::Shear,
        8  => crate::granite::GraniteEffect::Kaleido,
        9  => crate::granite::GraniteEffect::GravityWell,
        10 => crate::granite::GraniteEffect::Drain,
        11 => crate::granite::GraniteEffect::Flag,
        _  => crate::granite::GraniteEffect::Plasma,
    };
    (*ctx).config.visualizer.granite.effect = chosen;
    (*ctx).player.granite_set_effect(chosen);
}

/// Trigger an immediate switch to a random other Granite effect (the `n`
/// keyboard shortcut). Also records the new effect in the config so pinned
/// mode (`auto_switch` off) follows along instead of snapping back. Returns
/// the new effect index, or -1 when the renderer hasn't drawn a frame yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_granite_random_effect(ctx: *mut SparkampCtx) -> c_int {
    if ctx.is_null() {
        return -1;
    }
    match (*ctx).player.granite_random_effect() {
        Some(e) => {
            (*ctx).config.visualizer.granite.effect = e;
            match e {
                crate::granite::GraniteEffect::Plasma      => 0,
                crate::granite::GraniteEffect::Tunnel      => 1,
                crate::granite::GraniteEffect::Swirl       => 2,
                crate::granite::GraniteEffect::RadialSweep => 3,
                crate::granite::GraniteEffect::Cells       => 4,
                crate::granite::GraniteEffect::Explode     => 5,
                crate::granite::GraniteEffect::Ripple      => 6,
                crate::granite::GraniteEffect::Shear       => 7,
                crate::granite::GraniteEffect::Kaleido     => 8,
                crate::granite::GraniteEffect::GravityWell => 9,
                crate::granite::GraniteEffect::Drain       => 10,
                crate::granite::GraniteEffect::Flag        => 11,
            }
        }
        None => -1,
    }
}

/// Get whether the display is kept awake during the fullscreen visualizer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_keep_screen_awake(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return true;
    }
    (*ctx).config.visualizer.keep_screen_awake
}

/// Set whether the display is kept awake during the fullscreen visualizer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_keep_screen_awake(ctx: *mut SparkampCtx, on: bool) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.visualizer.keep_screen_awake = on;
}

/// Estimated tempo (BPM) from the Granite beat detector — median of recent
/// inter-beat intervals. 0.0 while unknown (warming up, silence, or no
/// Granite frame rendered yet). Debug aid for the fullscreen overlay.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_granite_bpm(ctx: *const SparkampCtx) -> f32 {
    if ctx.is_null() {
        return 0.0;
    }
    (*ctx).player.granite_bpm()
}

/// Estimated beats-per-measure from the Granite beat detector: 3 or 4,
/// or 0 while unknown. Debug aid for the fullscreen overlay.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_granite_meter(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    (*ctx).player.granite_meter() as c_int
}

/// Get Granite beat sensitivity (1.05–3.0; lower = more beats).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_granite_beat_sensitivity(ctx: *const SparkampCtx) -> f32 {
    if ctx.is_null() {
        return 1.5;
    }
    (*ctx).config.visualizer.granite.beat_sensitivity
}

/// Set Granite beat sensitivity (clamped 1.05–3.0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_granite_beat_sensitivity(ctx: *mut SparkampCtx, s: f32) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.visualizer.granite.beat_sensitivity = s.clamp(1.05, 3.0);
}

/// Get whether the waveform ink brightens on detected beats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_granite_beat_brightness(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return true;
    }
    (*ctx).config.visualizer.granite.beat_brightness
}

/// Set whether the waveform ink brightens on detected beats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_granite_beat_brightness(ctx: *mut SparkampCtx, on: bool) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.visualizer.granite.beat_brightness = on;
}

/// Get whether Granite auto-switches between effects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_granite_auto_switch(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return true;
    }
    (*ctx).config.visualizer.granite.auto_switch
}

/// Set whether Granite auto-switches between effects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_granite_auto_switch(ctx: *mut SparkampCtx, on: bool) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.visualizer.granite.auto_switch = on;
}

