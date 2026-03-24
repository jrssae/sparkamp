//! Granite — Geiss-inspired plasma visualizer plugin for Sparkamp (ABI v2).
//!
//! # Algorithm
//!
//! - **Plasma field**: three sine waves with irrational frequency ratios
//!   (1, φ, √2) driven by `playback_pos_secs` for non-repeating colour patterns.
//! - **Feedback blur**: each frame blends the current plasma with a
//!   slightly zoomed copy of the previous frame so colour trails linger.
//! - **Granite palette**: cycles through blue-grey → amber → violet.
//! - **Inactive decay**: when playback is paused/stopped, the buffer
//!   fades to black instead of freezing on the last frame.
//!
//! # Settings
//!
//! | Key        | Type   | Default | Range        | Description           |
//! |------------|--------|---------|-------------|------------------------|
//! | `speed`    | Float  | `1.0`   | 0.1 – 5.0   | Animation speed        |
//! | `palette`  | Choice | `Granite` | —         | Colour palette         |
//! | `feedback` | Float  | `0.35`  | 0.0 – 0.9   | Feedback blur strength |
//!
//! # ABI
//!
//! Exports the v2 entry point:
//! ```c
//! const SparkPluginAbi *sparkamp_plugin(void);
//! ```
//! The plugin mirrors the `SparkPluginAbi` struct layout locally so it does
//! not depend on the host crate at build time.

#![allow(unsafe_code)]

use std::f64::consts::PI;
use std::ffi::CStr;
use std::os::raw::{c_char, c_double, c_int, c_void};

// ---------------------------------------------------------------------------
// Local mirror of the Sparkamp plugin ABI v2 types
//
// These must exactly match the definitions in src/plugin_abi.rs.
// A plugin SDK crate / header would normally provide these.
// ---------------------------------------------------------------------------

const ABI_VERSION: u32 = 2;

#[repr(u32)]
#[allow(dead_code)]
enum SparkSettingType {
    End    = 0,
    Bool   = 1,
    Int    = 2,
    Float  = 3,
    String = 4,
    Choice = 5,
}

#[repr(C)]
struct SparkSettingDef {
    value_type:    SparkSettingType,
    key:           *const c_char,
    label:         *const c_char,
    description:   *const c_char,
    default_value: *const c_char,
    choices:       *const c_char,
    min_value:     *const c_char,
    max_value:     *const c_char,
}

unsafe impl Sync for SparkSettingDef {}

#[repr(u32)]
#[allow(dead_code)]
enum SparkPluginKind {
    Visualizer = 1,
    Filetype   = 2,
}

#[repr(C)]
struct SparkVizCallbacks {
    render:     Option<unsafe extern "C" fn(*mut c_void, c_double, c_int, *mut c_double, u32)>,
    fullscreen: Option<unsafe extern "C" fn(*mut c_void)>,
}

#[repr(C)]
struct SparkFiletypeCallbacks {
    extensions:    *const *const c_char,
    read_metadata: Option<unsafe extern "C" fn(*const c_char, *mut *mut c_char, *mut *mut c_char) -> c_int>,
    free_string:   Option<unsafe extern "C" fn(*mut c_char)>,
}

#[repr(C)]
struct SparkPluginAbi {
    abi_version:        u32,
    kind:               SparkPluginKind,
    plugin_id:          *const c_char,
    name:               *const c_char,
    version:            *const c_char,
    description:        *const c_char,
    author:             *const c_char,
    settings_schema:    *const SparkSettingDef,
    init:               Option<unsafe extern "C" fn(*const *const c_char, *const *const c_char) -> *mut c_void>,
    destroy:            Option<unsafe extern "C" fn(*mut c_void)>,
    on_setting_changed: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    viz:                SparkVizCallbacks,
    filetype:           SparkFiletypeCallbacks,
}

unsafe impl Sync for SparkPluginAbi {}

// ---------------------------------------------------------------------------
// Static identity strings
// ---------------------------------------------------------------------------

static PLUGIN_ID:    &[u8] = b"dev.sparkamp.viz.granite\0";
static PLUGIN_NAME:  &[u8] = b"Granite\0";
static PLUGIN_VER:   &[u8] = b"2.0.0\0";
static PLUGIN_DESC:  &[u8] = b"Geiss-inspired plasma visualizer with granite colour palette\0";
static PLUGIN_AUTH:  &[u8] = b"Sparkamp Project\0";

// Setting keys / labels / hints
static KEY_SPEED:    &[u8] = b"speed\0";
static KEY_PALETTE:  &[u8] = b"palette\0";
static KEY_FEEDBACK: &[u8] = b"feedback\0";

static LABEL_SPEED:    &[u8] = b"Animation speed\0";
static LABEL_PALETTE:  &[u8] = b"Colour palette\0";
static LABEL_FEEDBACK: &[u8] = b"Feedback strength\0";

static DESC_SPEED:    &[u8] = b"Multiplier applied to the plasma phase velocity (0.1 = slow, 5.0 = fast)\0";
static DESC_PALETTE:  &[u8] = b"Granite colour palette\0";
static DESC_FEEDBACK: &[u8] = b"How strongly previous frames bleed into the current one\0";

static DEFAULT_SPEED:    &[u8] = b"1.0\0";
static DEFAULT_PALETTE:  &[u8] = b"Granite\0";
static DEFAULT_FEEDBACK: &[u8] = b"0.35\0";

static MIN_SPEED:    &[u8] = b"0.1\0";
static MAX_SPEED:    &[u8] = b"5.0\0";
static MIN_FEEDBACK: &[u8] = b"0.0\0";
static MAX_FEEDBACK: &[u8] = b"0.9\0";
static PALETTE_CHOICES: &[u8] = b"Granite|Fire|Neon\0";

// ---------------------------------------------------------------------------
// Settings schema (null-terminated array)
// ---------------------------------------------------------------------------

static SETTINGS_SCHEMA: [SparkSettingDef; 4] = [
    SparkSettingDef {
        value_type:    SparkSettingType::Float,
        key:           KEY_SPEED.as_ptr()    as *const c_char,
        label:         LABEL_SPEED.as_ptr()  as *const c_char,
        description:   DESC_SPEED.as_ptr()   as *const c_char,
        default_value: DEFAULT_SPEED.as_ptr() as *const c_char,
        choices:       std::ptr::null(),
        min_value:     MIN_SPEED.as_ptr()    as *const c_char,
        max_value:     MAX_SPEED.as_ptr()    as *const c_char,
    },
    SparkSettingDef {
        value_type:    SparkSettingType::Choice,
        key:           KEY_PALETTE.as_ptr()     as *const c_char,
        label:         LABEL_PALETTE.as_ptr()   as *const c_char,
        description:   DESC_PALETTE.as_ptr()    as *const c_char,
        default_value: DEFAULT_PALETTE.as_ptr() as *const c_char,
        choices:       PALETTE_CHOICES.as_ptr() as *const c_char,
        min_value:     std::ptr::null(),
        max_value:     std::ptr::null(),
    },
    SparkSettingDef {
        value_type:    SparkSettingType::Float,
        key:           KEY_FEEDBACK.as_ptr()     as *const c_char,
        label:         LABEL_FEEDBACK.as_ptr()   as *const c_char,
        description:   DESC_FEEDBACK.as_ptr()    as *const c_char,
        default_value: DEFAULT_FEEDBACK.as_ptr() as *const c_char,
        choices:       std::ptr::null(),
        min_value:     MIN_FEEDBACK.as_ptr()     as *const c_char,
        max_value:     MAX_FEEDBACK.as_ptr()     as *const c_char,
    },
    // Sentinel: value_type = End (0)
    SparkSettingDef {
        value_type:    SparkSettingType::End,
        key:           std::ptr::null(),
        label:         std::ptr::null(),
        description:   std::ptr::null(),
        default_value: std::ptr::null(),
        choices:       std::ptr::null(),
        min_value:     std::ptr::null(),
        max_value:     std::ptr::null(),
    },
];

// ---------------------------------------------------------------------------
// Palette definitions
// ---------------------------------------------------------------------------

/// Which colour palette is active.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Palette {
    /// Blue-grey → amber → violet (default granite tones).
    Granite,
    /// Red → orange → yellow (warm fire).
    Fire,
    /// Cyan → green → lime (cold neon glow).
    Neon,
}

impl Palette {
    fn from_str(s: &str) -> Self {
        match s {
            "Fire" => Palette::Fire,
            "Neon" => Palette::Neon,
            _      => Palette::Granite,
        }
    }

    /// Map a normalised intensity [0,1] and phase [0,1] to an output amplitude.
    /// The three palettes just shift the colour modulation differently.
    fn modulate(&self, intensity: f64, palette_phase: f64) -> f64 {
        match self {
            Palette::Granite => {
                // Mid-range bump: never fully black or fully bright.
                0.15 + 0.70 * intensity * (0.7 + 0.3 * palette_phase)
            }
            Palette::Fire => {
                // Warm tones: stronger low-end, orange highlight.
                0.05 + 0.85 * (intensity * intensity) * (0.6 + 0.4 * palette_phase)
            }
            Palette::Neon => {
                // Sharp peaks with dark valleys.
                let sharp = (intensity * PI).sin().abs();
                0.05 + 0.90 * sharp * (0.8 + 0.2 * palette_phase)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin context
// ---------------------------------------------------------------------------

/// Per-session mutable state owned by the plugin.
struct GraniteCtx {
    /// Frame counter for temporal animation.
    frame:    u64,
    /// Animation speed multiplier (user setting `speed`).
    speed:    f64,
    /// Active colour palette (user setting `palette`).
    palette:  Palette,
    /// Feedback blur weight (user setting `feedback`).
    feedback: f64,
}

impl GraniteCtx {
    fn new() -> Self {
        GraniteCtx { frame: 0, speed: 1.0, palette: Palette::Granite, feedback: 0.35 }
    }

    /// Apply a key/value setting update.
    fn apply_setting(&mut self, key: &str, value: &str) {
        match key {
            "speed"    => { if let Ok(v) = value.parse::<f64>() { self.speed    = v.clamp(0.1, 5.0); } }
            "palette"  => { self.palette  = Palette::from_str(value); }
            "feedback" => { if let Ok(v) = value.parse::<f64>() { self.feedback = v.clamp(0.0, 0.9); } }
            _          => {}
        }
    }

    /// Parse and apply all settings from a null-terminated key/value pair array.
    unsafe fn apply_settings_array(
        &mut self,
        keys:   *const *const c_char,
        values: *const *const c_char,
    ) {
        if keys.is_null() || values.is_null() {
            return;
        }
        let mut i = 0usize;
        loop {
            let kp = unsafe { *keys.add(i) };
            let vp = unsafe { *values.add(i) };
            if kp.is_null() || vp.is_null() {
                break;
            }
            let key = unsafe { CStr::from_ptr(kp) }.to_string_lossy();
            let val = unsafe { CStr::from_ptr(vp) }.to_string_lossy();
            self.apply_setting(&key, &val);
            i += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin callbacks
// ---------------------------------------------------------------------------

/// Allocate a `GraniteCtx` and apply initial settings.
unsafe extern "C" fn plugin_init(
    keys:   *const *const c_char,
    values: *const *const c_char,
) -> *mut c_void {
    let mut ctx = Box::new(GraniteCtx::new());
    unsafe { ctx.apply_settings_array(keys, values); }
    Box::into_raw(ctx) as *mut c_void
}

/// Free the `GraniteCtx` allocated by `plugin_init`.
unsafe extern "C" fn plugin_destroy(ctx: *mut c_void) {
    if !ctx.is_null() {
        unsafe { drop(Box::from_raw(ctx as *mut GraniteCtx)); }
    }
}

/// Apply a live setting change without restarting the plugin.
unsafe extern "C" fn plugin_on_setting_changed(
    ctx:   *mut c_void,
    key:   *const c_char,
    value: *const c_char,
) {
    if ctx.is_null() || key.is_null() || value.is_null() {
        return;
    }
    let ctx_ref = unsafe { &mut *(ctx as *mut GraniteCtx) };
    let k = unsafe { CStr::from_ptr(key)   }.to_string_lossy();
    let v = unsafe { CStr::from_ptr(value) }.to_string_lossy();
    ctx_ref.apply_setting(&k, &v);
}

/// Generate `count` visualizer samples using the Geiss-inspired plasma algorithm.
///
/// Each output sample is the visual intensity at a horizontal position along
/// a virtual scanline at the centre of the visualizer area.  The computation:
///
/// 1. **Plasma field**: three sine waves with irrational ratios (1, φ, √2).
/// 2. **Feedback**: weighted mix with a zoomed copy of the previous frame.
/// 3. **Palette modulation**: colour tone from the selected palette.
unsafe extern "C" fn plugin_render(
    ctx:               *mut c_void,
    playback_pos_secs: c_double,
    is_active:         c_int,
    out:               *mut c_double,
    count:             u32,
) {
    let (frame, speed, palette, feedback) = if ctx.is_null() {
        (0u64, 1.0f64, Palette::Granite, 0.35f64)
    } else {
        let ctx_ref = unsafe { &mut *(ctx as *mut GraniteCtx) };
        ctx_ref.frame += 1;
        (ctx_ref.frame, ctx_ref.speed, ctx_ref.palette, ctx_ref.feedback)
    };

    if count == 0 || out.is_null() {
        return;
    }

    let n = count as usize;
    let samples = unsafe { std::slice::from_raw_parts_mut(out, n) };

    // When paused/stopped, decay the existing buffer toward zero (fade-out).
    if is_active == 0 {
        for s in samples.iter_mut() {
            *s *= 0.92;
        }
        return;
    }

    let phi:   f64 = 1.618_033_988_749_895;
    let sqrt2: f64 = std::f64::consts::SQRT_2;

    // Phase evolves with playback position, scaled by animation speed.
    let t_phase   = playback_pos_secs * 0.7 * speed;
    // Colour-palette cycle period is ~8 s (at speed 1.0).
    let palette_t = playback_pos_secs * 0.125 * speed;
    // Slow breathe: slightly zooms the feedback pattern in and out.
    let zoom = 1.0 + 0.04 * (frame as f64 * 0.003).sin();

    for i in 0..n {
        let x = i as f64 / (n - 1).max(1) as f64;

        // ── Plasma field ──────────────────────────────────────────────────
        let w1 = (2.0 * PI * (x         + t_phase * 1.00)).sin();
        let w2 = (2.0 * PI * (x * phi   + t_phase * 0.73)).sin();
        let w3 = (2.0 * PI * (x * sqrt2 + t_phase * 0.51)).sin();
        // Normalise sum to [-1, +1].
        let plasma = (w1 + w2 * 0.6 + w3 * 0.4) / 2.0;

        // ── Feedback blend ────────────────────────────────────────────────
        // Look up the previous sample at a zoomed x coordinate so the
        // pattern appears to breathe.  Reading partially-written samples
        // in the same pass is intentional — it creates a cascade effect.
        let prev_x = ((x * zoom - 0.5 * (zoom - 1.0)).clamp(0.0, 1.0)
            * (n - 1) as f64) as usize;
        let prev_sample = samples[prev_x];

        // ── Palette modulation ────────────────────────────────────────────
        let palette_phase = (palette_t * 2.0 * PI).sin() * 0.5 + 0.5; // [0, 1]
        let intensity     = (0.5 + 0.5 * plasma).clamp(0.0, 1.0);
        let tone          = palette.modulate(intensity, palette_phase);

        // ── Combine ───────────────────────────────────────────────────────
        samples[i] = (tone * (1.0 - feedback) + prev_sample * feedback).clamp(0.0, 1.0);
    }
}

/// Blocking fullscreen render loop (terminal ANSI fallback).
///
/// Called by the host when the user presses `f` or double-clicks the
/// visualizer.  In a full implementation this would open a native GL window;
/// here we render a 2-D plasma field using ANSI 256-colour escape codes.
///
/// Exits after the 60-minute safety limit (or when killed with Ctrl-C).
unsafe extern "C" fn plugin_fullscreen(ctx: *mut c_void) {
    let (speed, palette, _feedback) = if ctx.is_null() {
        (1.0f64, Palette::Granite, 0.35f64)
    } else {
        let ctx_ref = unsafe { &*(ctx as *mut GraniteCtx) };
        (ctx_ref.speed, ctx_ref.palette, ctx_ref.feedback)
    };

    // ANSI 256-colour palettes (16 entries each, cycling value → index).
    let palette_entries: &[u8] = match palette {
        Palette::Granite => &[237, 238, 240, 242, 244, 246, 178, 214, 208, 202, 246, 244,  97, 91, 55, 237],
        Palette::Fire    => &[  0,  52,  88, 124, 160, 196, 202, 208, 214, 220, 226, 190, 154, 118, 82, 46],
        Palette::Neon    => &[ 16,  17,  18,  19,  20,  21,  27,  33,  39,  45,  51,  86, 121, 156, 191, 226],
    };

    const W: usize = 80;
    const H: usize = 24;

    let phi:   f64 = 1.618_033_988_749_895;
    let sqrt2: f64 = std::f64::consts::SQRT_2;

    // Hide cursor and switch to alternate screen buffer.
    print!("\x1b[?25l\x1b[?1049h\x1b[2J");

    let start = std::time::Instant::now();
    let mut frame = 0u64;

    loop {
        let t = start.elapsed().as_secs_f64();
        if t > 3600.0 {
            break;
        }
        frame += 1;

        let t_phase   = t * 0.7 * speed;
        let palette_t = t * 0.125 * speed;
        let zoom      = 1.0 + 0.04 * (frame as f64 * 0.003).sin();

        let mut buf = String::with_capacity(W * H * 16);
        buf.push_str("\x1b[H");

        for y in 0..H {
            for x in 0..W {
                let fx = x as f64 / W as f64;
                let fy = y as f64 / H as f64;
                let _ = zoom;

                let w1 = (2.0 * PI * (fx         + fy         + t_phase       )).sin();
                let w2 = (2.0 * PI * (fx * phi   - fy * phi   + t_phase * 0.73)).sin();
                let w3 = (2.0 * PI * (fx * sqrt2 + fy * sqrt2 + t_phase * 0.51)).sin();
                let w4 = (2.0 * PI * ((fx * fx + fy * fy).sqrt() * 3.0 + t_phase * 0.31)).sin();
                let plasma = (w1 + w2 * 0.6 + w3 * 0.4 + w4 * 0.3) / 2.3;

                let palette_phase = (palette_t * 2.0 * PI).sin() * 0.5 + 0.5;
                let intensity     = (0.5 + 0.5 * plasma).clamp(0.0, 1.0);
                let tone          = palette.modulate(intensity, palette_phase);
                let palette_idx   = (tone * 15.0) as usize;
                let colour        = palette_entries[palette_idx.min(15)];

                buf.push_str(&format!("\x1b[48;5;{}m ", colour));
            }
            buf.push_str("\x1b[0m\n");
        }

        print!("{}", buf);

        // ~30 fps
        std::thread::sleep(std::time::Duration::from_millis(33));
    }

    // Restore terminal.
    print!("\x1b[?1049l\x1b[?25h");
}

// ---------------------------------------------------------------------------
// Static plugin descriptor
// ---------------------------------------------------------------------------

static PLUGIN: SparkPluginAbi = SparkPluginAbi {
    abi_version:        ABI_VERSION,
    kind:               SparkPluginKind::Visualizer,
    plugin_id:          PLUGIN_ID.as_ptr()   as *const c_char,
    name:               PLUGIN_NAME.as_ptr() as *const c_char,
    version:            PLUGIN_VER.as_ptr()  as *const c_char,
    description:        PLUGIN_DESC.as_ptr() as *const c_char,
    author:             PLUGIN_AUTH.as_ptr() as *const c_char,
    settings_schema:    SETTINGS_SCHEMA.as_ptr(),
    init:               Some(plugin_init),
    destroy:            Some(plugin_destroy),
    on_setting_changed: Some(plugin_on_setting_changed),
    viz: SparkVizCallbacks {
        render:     Some(plugin_render),
        fullscreen: Some(plugin_fullscreen),
    },
    filetype: SparkFiletypeCallbacks {
        extensions:    std::ptr::null(),
        read_metadata: None,
        free_string:   None,
    },
};

// ---------------------------------------------------------------------------
// C entry point
// ---------------------------------------------------------------------------

/// Called by Sparkamp immediately after `dlopen` to obtain the plugin descriptor.
#[unsafe(no_mangle)]
pub extern "C" fn sparkamp_plugin() -> *const SparkPluginAbi {
    &PLUGIN
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_point_returns_non_null() {
        let ptr = sparkamp_plugin();
        assert!(!ptr.is_null());
    }

    #[test]
    fn abi_version_is_2() {
        let abi = unsafe { &*sparkamp_plugin() };
        assert_eq!(abi.abi_version, 2);
    }

    #[test]
    fn render_callback_is_present() {
        let abi = unsafe { &*sparkamp_plugin() };
        assert!(abi.viz.render.is_some());
    }

    #[test]
    fn fullscreen_callback_is_present() {
        let abi = unsafe { &*sparkamp_plugin() };
        assert!(abi.viz.fullscreen.is_some());
    }

    #[test]
    fn settings_schema_has_three_entries_plus_sentinel() {
        let abi = unsafe { &*sparkamp_plugin() };
        assert!(!abi.settings_schema.is_null());
        // Walk the schema counting non-End entries.
        let mut count = 0usize;
        let mut i = 0usize;
        loop {
            let def = unsafe { &*abi.settings_schema.add(i) };
            if matches!(def.value_type, SparkSettingType::End) {
                break;
            }
            count += 1;
            i += 1;
        }
        assert_eq!(count, 3, "expected 3 settings (speed, palette, feedback)");
    }

    #[test]
    fn render_output_in_range() {
        let ctx = unsafe { plugin_init(std::ptr::null(), std::ptr::null()) };
        let count: u32 = 64;
        let mut buf = vec![0.0f64; count as usize];
        unsafe { plugin_render(ctx, 1.23, 1, buf.as_mut_ptr(), count); }
        for &v in &buf {
            assert!(v >= 0.0 && v <= 1.0, "sample {v} out of [0.0, 1.0] range");
        }
        unsafe { plugin_destroy(ctx); }
    }

    #[test]
    fn render_inactive_decays() {
        let ctx = unsafe { plugin_init(std::ptr::null(), std::ptr::null()) };
        let count: u32 = 32;
        let mut buf = vec![0.5f64; count as usize];
        for _ in 0..200 {
            unsafe { plugin_render(ctx, 0.0, 0, buf.as_mut_ptr(), count); }
        }
        let max = buf.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(max < 0.3, "expected decay, got max = {max}");
        unsafe { plugin_destroy(ctx); }
    }

    #[test]
    fn render_zero_count_is_noop() {
        let ctx = unsafe { plugin_init(std::ptr::null(), std::ptr::null()) };
        unsafe { plugin_render(ctx, 0.0, 1, std::ptr::null_mut(), 0); }
        unsafe { plugin_destroy(ctx); }
    }

    #[test]
    fn render_null_ctx_is_safe() {
        let count: u32 = 8;
        let mut buf = vec![0.0f64; count as usize];
        unsafe { plugin_render(std::ptr::null_mut(), 2.5, 1, buf.as_mut_ptr(), count); }
        for &v in &buf {
            assert!(v >= 0.0 && v <= 1.0);
        }
    }

    #[test]
    fn on_setting_changed_updates_speed() {
        let ctx = unsafe { plugin_init(std::ptr::null(), std::ptr::null()) };
        assert!(!ctx.is_null());
        let key   = b"speed\0".as_ptr() as *const c_char;
        let value = b"2.5\0".as_ptr()   as *const c_char;
        unsafe { plugin_on_setting_changed(ctx, key, value); }
        let ctx_ref = unsafe { &*(ctx as *mut GraniteCtx) };
        assert!((ctx_ref.speed - 2.5).abs() < 1e-9, "speed not updated");
        unsafe { plugin_destroy(ctx); }
    }

    #[test]
    fn on_setting_changed_ignores_unknown_key() {
        let ctx = unsafe { plugin_init(std::ptr::null(), std::ptr::null()) };
        let key   = b"nonexistent\0".as_ptr() as *const c_char;
        let value = b"whatever\0".as_ptr()    as *const c_char;
        // Must not panic.
        unsafe { plugin_on_setting_changed(ctx, key, value); }
        unsafe { plugin_destroy(ctx); }
    }
}
