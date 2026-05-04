//! Granite — Geiss-inspired plasma visualizer.
//!
//! Both frontends (GTK + macOS) call [`Granite::render`] each frame to fill a
//! caller-owned RGBA8 buffer at the granite-internal resolution; the windowing
//! system's GPU compositor handles the upscale to display size.
//!
//! Five effect kernels share the same warp-and-feedback pipeline:
//! plasma (three irrational sines), tunnel, swirl, radial sweep, and cells.
//! When `auto_switch` is on, the scheduler rotates between effects every
//! 12–24 seconds with a one-second crossfade, mimicking the original Geiss's
//! preset rotation.
//!
//! Per-pixel math is f32 and rows are processed in parallel via rayon.

use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;
use rayon::prelude::*;
use std::f32::consts::{PI, SQRT_2};

/// Internal render height. Frontends pass this to [`Granite::render`] together
/// with a width derived from the viewport's aspect ratio. Single-line change
/// to bump or shrink — no schema or FFI references this constant elsewhere.
pub const GRANITE_INTERNAL_HEIGHT: u32 = 360;

const PHI: f32 = 1.618_034;
const TAU: f32 = 2.0 * PI;

// ---------------------------------------------------------------------------
// Public configuration
// ---------------------------------------------------------------------------

/// Which colour palette the visualizer is rendered through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GranitePalette {
    Granite,
    Fire,
    Neon,
}

impl Default for GranitePalette {
    fn default() -> Self { GranitePalette::Granite }
}

/// Which per-pixel effect kernel to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraniteEffect {
    Plasma,
    Tunnel,
    Swirl,
    #[serde(rename = "radial_sweep")]
    RadialSweep,
    Cells,
}

impl Default for GraniteEffect {
    fn default() -> Self { GraniteEffect::Plasma }
}

const ALL_EFFECTS: [GraniteEffect; 5] = [
    GraniteEffect::Plasma,
    GraniteEffect::Tunnel,
    GraniteEffect::Swirl,
    GraniteEffect::RadialSweep,
    GraniteEffect::Cells,
];

/// Shape of the PCM-driven waveform line that's drawn over each frame and
/// then dissolved by the warp on the next frame (Geiss "scope" flow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveShape {
    Line,
    VerticalLine,
    Circle,
    Square,
    Lissajous,
    DiagonalX,
}

const ALL_SHAPES: [WaveShape; 6] = [
    WaveShape::Line,
    WaveShape::VerticalLine,
    WaveShape::Circle,
    WaveShape::Square,
    WaveShape::Lissajous,
    WaveShape::DiagonalX,
];

fn default_true() -> bool { true }

/// User-tunable settings that feed the kernel.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct GraniteConfig {
    pub speed:    f32,
    pub palette:  GranitePalette,
    pub feedback: f32,
    /// Currently-selected effect when `auto_switch` is off. When `auto_switch`
    /// is on, this records whatever the scheduler last landed on so the UI
    /// can reflect the live state.
    #[serde(default)]
    pub effect: GraniteEffect,
    /// When true, the scheduler rotates effects every 12–24 s.
    #[serde(default = "default_true")]
    pub auto_switch: bool,
}

impl Default for GraniteConfig {
    fn default() -> Self {
        GraniteConfig {
            speed: 1.0,
            palette: GranitePalette::Granite,
            feedback: 0.35,
            effect: GraniteEffect::Plasma,
            auto_switch: true,
        }
    }
}

impl GraniteConfig {
    fn clamped(&self) -> (f32, GranitePalette, f32) {
        (
            self.speed.clamp(0.1, 5.0),
            self.palette,
            self.feedback.clamp(0.0, 0.9),
        )
    }
}

// ---------------------------------------------------------------------------
// Renderer state
// ---------------------------------------------------------------------------

/// Holds the previous-frame buffer driving the Geiss-style feedback term, the
/// effect-rotation scheduler, and two scratch buffers used during crossfade.
pub struct Granite {
    prev:  Vec<u8>,
    w:     u32,
    h:     u32,
    frame: u64,
    // Scheduler.
    current: GraniteEffect,
    next: Option<GraniteEffect>,
    current_palette: GranitePalette,
    next_palette: Option<GranitePalette>,
    current_shape: WaveShape,
    switch_at_frame: u64,
    crossfade_remaining: u8,
    rng: StdRng,
    // Scratch buffers, allocated on first crossfade and reused.
    scratch_a: Vec<u8>,
    scratch_b: Vec<u8>,
}

const CROSSFADE_FRAMES: u8 = 30; // ~1 s at 30 fps
const SWITCH_INTERVAL_MIN: u64 = 360;  // 12 s
const SWITCH_INTERVAL_MAX: u64 = 720;  // 24 s

impl Granite {
    /// Allocate a renderer for `w × h` pixels.
    pub fn new(w: u32, h: u32) -> Self {
        let mut g = Granite {
            prev: Vec::new(),
            w: 0,
            h: 0,
            frame: 0,
            current: GraniteEffect::Plasma,
            next: None,
            current_palette: GranitePalette::Granite,
            next_palette: None,
            current_shape: WaveShape::Line,
            // Seeded for reproducible unit tests; switch cadence is still
            // perceived as "random" over a multi-minute play session.
            switch_at_frame: SWITCH_INTERVAL_MIN,
            crossfade_remaining: 0,
            rng: StdRng::seed_from_u64(0xC0FFEE),
            scratch_a: Vec::new(),
            scratch_b: Vec::new(),
        };
        g.resize(w, h);
        g
    }

    /// Reallocate the previous-frame and scratch buffers if dimensions changed.
    pub fn resize(&mut self, w: u32, h: u32) {
        if self.w == w && self.h == h && !self.prev.is_empty() {
            return;
        }
        self.w = w;
        self.h = h;
        let need = (w as usize) * (h as usize) * 4;
        self.prev = vec![0u8; need];
        // Scratch buffers are sized lazily on first crossfade.
        self.scratch_a.clear();
        self.scratch_b.clear();
    }

    /// Manually pin the active effect (used when the user picks one in
    /// Settings while `auto_switch` is on, to keep that selection visible
    /// for at least one full switch interval before the scheduler resumes).
    pub fn set_effect(&mut self, effect: GraniteEffect) {
        self.current = effect;
        self.next = None;
        self.next_palette = None;
        self.crossfade_remaining = 0;
        // Push the next auto-switch out by ~20 s.
        self.switch_at_frame = self.frame + 600;
    }

    /// Render one frame into `dst` (RGBA8, length `w*h*4`).
    ///
    /// `waveform` is PCM samples in `[-1, 1]`; when non-empty, the active
    /// scope shape (line / circle / square / etc.) is drawn on top of the
    /// warp output as a thick palette-accent stroke. The next frame's warp
    /// then dissolves that line into the plasma — Geiss-style flow.
    pub fn render(
        &mut self,
        dst: &mut [u8],
        w: u32,
        h: u32,
        t_seconds: f32,
        is_active: bool,
        waveform: &[f32],
        cfg: &GraniteConfig,
    ) {
        if w != self.w || h != self.h {
            self.resize(w, h);
            dst.fill(0);
        }
        debug_assert_eq!(dst.len(), (w as usize) * (h as usize) * 4);

        // Inactive: decay both buffers toward black instead of advancing.
        if !is_active {
            decay_buffer(dst);
            decay_buffer(&mut self.prev);
            return;
        }

        self.frame = self.frame.wrapping_add(1);
        let (speed, cfg_palette, feedback) = cfg.clamped();
        let palette_phase = palette_phase_at(t_seconds, speed);

        // Scheduler: pick / advance / start crossfades.
        if cfg.auto_switch {
            self.tick_scheduler();
        } else {
            // User-pinned: snap to the configured effect + palette, drop any
            // in-flight crossfade.
            if cfg.effect != self.current {
                self.current = cfg.effect;
                self.next = None;
                self.crossfade_remaining = 0;
            }
            self.current_palette = cfg_palette;
            self.next_palette = None;
        }

        // Choose which palettes to drive this frame. With auto-switch, the
        // scheduler picks one per effect; without, it's whatever's in cfg.
        let palette_curr = if cfg.auto_switch { self.current_palette } else { cfg_palette };
        let palette_next = if cfg.auto_switch {
            self.next_palette.unwrap_or(self.current_palette)
        } else {
            cfg_palette
        };

        // Render the active effect (or crossfade) into dst.
        if self.crossfade_remaining > 0 && self.next.is_some() {
            self.render_crossfade(
                dst, w, h, t_seconds,
                palette_curr, palette_next, palette_phase, feedback,
            );
        } else {
            render_effect_into(
                dst, &self.prev, self.current,
                w, h, t_seconds, self.frame, palette_curr, palette_phase, feedback,
            );
        }

        // Save this frame as the next call's prev BEFORE the waveform shape
        // is drawn, so the line dissolves into the plasma over subsequent
        // frames (Geiss flow): each new frame redraws a fresh shape that the
        // warp then carries away.
        self.prev.copy_from_slice(dst);

        // Composite the active waveform shape on top of the warp output.
        if !waveform.is_empty() {
            draw_waveform_shape_overlay(
                dst, w, h, waveform, self.current_shape,
                palette_curr, palette_phase,
            );
        }
    }

    /// Read the active effect (after scheduler tick). Frontends use this to
    /// reflect what's actually on screen in the Settings dropdown when
    /// `auto_switch` is on.
    #[allow(dead_code)] // used by tests + macOS FFI; GTK reads config.effect instead.
    pub fn active_effect(&self) -> GraniteEffect { self.current }

    // -----------------------------------------------------------------------
    // Scheduler
    // -----------------------------------------------------------------------

    fn tick_scheduler(&mut self) {
        // Advance an in-flight crossfade.
        if self.crossfade_remaining > 0 {
            self.crossfade_remaining -= 1;
            if self.crossfade_remaining == 0 {
                if let Some(n) = self.next.take() {
                    self.current = n;
                }
                if let Some(p) = self.next_palette.take() {
                    self.current_palette = p;
                }
                // Schedule the next switch.
                let interval = self.rng.gen_range(SWITCH_INTERVAL_MIN..=SWITCH_INTERVAL_MAX);
                self.switch_at_frame = self.frame + interval;
            }
            return;
        }
        // Time for a new switch?
        if self.frame >= self.switch_at_frame {
            self.next = Some(random_other_effect(self.current, &mut self.rng));
            // Switching effects also rolls a new palette so the colour scheme
            // changes alongside the kernel — closer to original Geiss feel.
            self.next_palette = Some(random_other_palette(
                self.current_palette, &mut self.rng,
            ));
            // Snap the scope shape immediately too. We don't crossfade the
            // shape — the waveform line dissolves into the plasma each
            // frame, so changing the shape mid-warp just looks like the next
            // few frames trace a new figure.
            self.current_shape = random_other_shape(self.current_shape, &mut self.rng);
            self.crossfade_remaining = CROSSFADE_FRAMES;
        }
    }

    // -----------------------------------------------------------------------
    // Crossfade (renders both effects then lerps into dst)
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn render_crossfade(
        &mut self,
        dst: &mut [u8],
        w: u32,
        h: u32,
        t_seconds: f32,
        palette_curr: GranitePalette,
        palette_next: GranitePalette,
        palette_phase: f32,
        feedback: f32,
    ) {
        let need = (w as usize) * (h as usize) * 4;
        if self.scratch_a.len() != need { self.scratch_a.resize(need, 0); }
        if self.scratch_b.len() != need { self.scratch_b.resize(need, 0); }

        let next = self.next.unwrap_or(self.current);
        render_effect_into(
            &mut self.scratch_a, &self.prev, self.current,
            w, h, t_seconds, self.frame, palette_curr, palette_phase, feedback,
        );
        render_effect_into(
            &mut self.scratch_b, &self.prev, next,
            w, h, t_seconds, self.frame, palette_next, palette_phase, feedback,
        );

        // alpha = 0 at start of crossfade (all current); 1 at end (all next).
        let alpha = 1.0 - (self.crossfade_remaining as f32 / CROSSFADE_FRAMES as f32);
        let inv_alpha = 1.0 - alpha;
        for ((d, a), b) in dst.chunks_exact_mut(4)
            .zip(self.scratch_a.chunks_exact(4))
            .zip(self.scratch_b.chunks_exact(4))
        {
            d[0] = lerp_u8(a[0], b[0], alpha, inv_alpha);
            d[1] = lerp_u8(a[1], b[1], alpha, inv_alpha);
            d[2] = lerp_u8(a[2], b[2], alpha, inv_alpha);
            d[3] = 255;
        }
    }
}

#[inline]
fn lerp_u8(a: u8, b: u8, alpha: f32, inv_alpha: f32) -> u8 {
    (a as f32 * inv_alpha + b as f32 * alpha + 0.5).clamp(0.0, 255.0) as u8
}

fn decay_buffer(buf: &mut [u8]) {
    // Multiply by ≈ 0.977 per frame.
    for byte in buf.iter_mut() {
        *byte = ((*byte as u32) * 250 / 256) as u8;
    }
}

fn random_other_effect(current: GraniteEffect, rng: &mut StdRng) -> GraniteEffect {
    loop {
        let idx = rng.gen_range(0..ALL_EFFECTS.len());
        let candidate = ALL_EFFECTS[idx];
        if candidate != current { return candidate; }
    }
}

const ALL_PALETTES: [GranitePalette; 3] = [
    GranitePalette::Granite,
    GranitePalette::Fire,
    GranitePalette::Neon,
];

fn random_other_palette(current: GranitePalette, rng: &mut StdRng) -> GranitePalette {
    loop {
        let idx = rng.gen_range(0..ALL_PALETTES.len());
        let candidate = ALL_PALETTES[idx];
        if candidate != current { return candidate; }
    }
}

fn random_other_shape(current: WaveShape, rng: &mut StdRng) -> WaveShape {
    loop {
        let idx = rng.gen_range(0..ALL_SHAPES.len());
        let candidate = ALL_SHAPES[idx];
        if candidate != current { return candidate; }
    }
}

// ---------------------------------------------------------------------------
// Waveform-shape overlay (Geiss-style scope)
// ---------------------------------------------------------------------------

/// Draw the active scope shape on top of `dst`, using PCM samples in
/// `[-1, 1]` to modulate the shape's amplitude. The line is a thick
/// palette-accent stroke (~2.5 px); the next frame's warp dissolves it
/// into the plasma, producing the signature Geiss flow.
fn draw_waveform_shape_overlay(
    dst: &mut [u8],
    w: u32,
    h: u32,
    samples: &[f32],
    shape: WaveShape,
    palette: GranitePalette,
    palette_phase: f32,
) {
    if samples.is_empty() || w < 4 || h < 4 { return; }
    let (cr_f, cg_f, cb_f) = palette_rgb(palette, 0.98, palette_phase);
    let r8 = (cr_f * 255.0).clamp(0.0, 255.0) as u8;
    let g8 = (cg_f * 255.0).clamp(0.0, 255.0) as u8;
    let b8 = (cb_f * 255.0).clamp(0.0, 255.0) as u8;

    // Stamp radius ≈ 1.25 px → drawn diameter ≈ 2.5 px after AA.
    let radius: f32 = 1.4;
    let n = samples.len();
    let n_f = n as f32;
    let wf = w as f32;
    let hf = h as f32;
    let cx = wf * 0.5;
    let cy = hf * 0.5;

    let mut prev_xy: Option<(f32, f32)> = None;
    let mut close_loop_pt: Option<(f32, f32)> = None;

    for (i, s) in samples.iter().enumerate() {
        let s = s.clamp(-1.0, 1.0);
        let t = i as f32 / (n_f - 1.0).max(1.0); // 0..1
        let (x, y) = match shape {
            WaveShape::Line => {
                let x = t * wf;
                let y = cy + s * hf * 0.4;
                (x, y)
            }
            WaveShape::VerticalLine => {
                let x = cx + s * wf * 0.4;
                let y = t * hf;
                (x, y)
            }
            WaveShape::Circle => {
                let angle = t * std::f32::consts::TAU;
                let base = wf.min(hf) * 0.30;
                let r = base + s * base * 0.45;
                let x = cx + r * angle.cos();
                let y = cy + r * angle.sin();
                if i == 0 { close_loop_pt = Some((x, y)); }
                (x, y)
            }
            WaveShape::Square => {
                // Walk the unit square's perimeter; modulate inset by sample.
                let p = (t * 4.0).fract();
                let side = (t * 4.0).floor() as i32 % 4;
                let half = wf.min(hf) * 0.36;
                let inset = s * half * 0.30;
                let (ux, uy) = match side {
                    0 => (-1.0 + 2.0 * p, -1.0),
                    1 => ( 1.0,            -1.0 + 2.0 * p),
                    2 => ( 1.0 - 2.0 * p,   1.0),
                    _ => (-1.0,             1.0 - 2.0 * p),
                };
                // Push perimeter inward proportional to |sample|, signed.
                let nx = -uy;
                let ny =  ux;
                let x = cx + ux * half + nx * inset;
                let y = cy + uy * half + ny * inset;
                if i == 0 { close_loop_pt = Some((x, y)); }
                (x, y)
            }
            WaveShape::Lissajous => {
                // x = cos(3t), y = sin(2t) * sample; classic 2:3 figure with
                // y-axis modulated by audio so the figure breathes.
                let phase = t * std::f32::consts::TAU;
                let amp = wf.min(hf) * 0.36;
                let x = cx + (3.0 * phase).cos() * amp;
                let y = cy + (2.0 * phase).sin() * amp * (0.5 + 0.5 * s.abs());
                (x, y)
            }
            WaveShape::DiagonalX => {
                // Two crossing diagonals; each half of the buffer traces one.
                let span = wf.min(hf) * 0.40;
                if t < 0.5 {
                    let p = t * 2.0;          // 0..1
                    let x = cx + (-1.0 + 2.0 * p) * span;
                    let y = cy + (-1.0 + 2.0 * p) * span + s * hf * 0.10;
                    (x, y)
                } else {
                    let p = (t - 0.5) * 2.0;  // 0..1
                    let x = cx + (-1.0 + 2.0 * p) * span;
                    let y = cy + ( 1.0 - 2.0 * p) * span + s * hf * 0.10;
                    if (t - 0.5).abs() < 1e-3 {
                        // Lift the pen across the discontinuity at t == 0.5.
                        prev_xy = None;
                    }
                    (x, y)
                }
            }
        };

        if let Some((px, py)) = prev_xy {
            stamp_line(dst, w, h, px, py, x, y, radius, r8, g8, b8);
        }
        prev_xy = Some((x, y));
    }

    // Closed shapes: connect last sample back to first.
    if let (Some(prev), Some(first)) = (prev_xy, close_loop_pt) {
        stamp_line(dst, w, h, prev.0, prev.1, first.0, first.1, radius, r8, g8, b8);
    }
}

/// Rasterise a thick line `(x0, y0) → (x1, y1)` by stamping filled discs of
/// `radius` pixels along the segment at half-pixel steps. Cheap and produces
/// a smooth ~2-3 px stroke without any AA library.
#[inline]
fn stamp_line(
    dst: &mut [u8],
    w: u32, h: u32,
    x0: f32, y0: f32, x1: f32, y1: f32,
    radius: f32, r: u8, g: u8, b: u8,
) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len = (dx * dx + dy * dy).sqrt();
    let steps = (len * 2.0).ceil().max(1.0) as i32; // 0.5-px granularity
    let inv_steps = 1.0 / steps as f32;
    for s in 0..=steps {
        let t = s as f32 * inv_steps;
        let cx = x0 + dx * t;
        let cy = y0 + dy * t;
        stamp_disc(dst, w, h, cx, cy, radius, r, g, b);
    }
}

#[inline]
fn stamp_disc(
    dst: &mut [u8],
    w: u32, h: u32,
    cx: f32, cy: f32,
    radius: f32, r: u8, g: u8, b: u8,
) {
    let r2 = radius * radius;
    let lo_x = (cx - radius).floor().max(0.0) as i32;
    let hi_x = (cx + radius).ceil().min(w as f32 - 1.0) as i32;
    let lo_y = (cy - radius).floor().max(0.0) as i32;
    let hi_y = (cy + radius).ceil().min(h as f32 - 1.0) as i32;
    for y in lo_y..=hi_y {
        for x in lo_x..=hi_x {
            let dxp = x as f32 + 0.5 - cx;
            let dyp = y as f32 + 0.5 - cy;
            if dxp * dxp + dyp * dyp <= r2 {
                let off = (y as usize * w as usize + x as usize) * 4;
                dst[off    ] = r;
                dst[off + 1] = g;
                dst[off + 2] = b;
                dst[off + 3] = 255;
            }
        }
    }
}

fn palette_phase_at(t_seconds: f32, speed: f32) -> f32 {
    let palette_t = t_seconds * 0.125 * speed;
    (palette_t * TAU).sin() * 0.5 + 0.5
}

// ---------------------------------------------------------------------------
// Rendering pipeline (shared across all effects)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn render_effect_into(
    dst: &mut [u8],
    prev: &[u8],
    effect: GraniteEffect,
    w: u32,
    h: u32,
    t_seconds: f32,
    frame: u64,
    palette: GranitePalette,
    palette_phase: f32,
    feedback: f32,
) {
    let inv_feedback = 1.0 - feedback;

    // Slow zoom-breathing for the warp sample (Geiss-style feedback warp).
    let zoom = 1.0 + 0.04 * (frame as f32 * 0.003).sin();
    let inv_zoom = 1.0 / zoom;

    let wf = (w as f32 - 1.0).max(1.0);
    let hf = (h as f32 - 1.0).max(1.0);
    let prev_w = w as i32;
    let prev_h = h as i32;
    let row_bytes = (w as usize) * 4;

    // Phase shared by all effects.
    let t_phase = t_seconds * 0.7 * 1.0_f32;

    // Parallel over rows. Each row is independent: it reads `prev` (immutable
    // borrow) and writes its own slice of `dst`.
    dst.par_chunks_mut(row_bytes)
        .enumerate()
        .for_each(|(y_idx, row)| {
            let y = y_idx as u32;
            let v = y as f32 / hf;
            for x in 0..w {
                let u = x as f32 / wf;

                // Effect-specific intensity field in [-1, 1] ish.
                let intensity = match effect {
                    GraniteEffect::Plasma      => plasma_intensity(u, v, t_phase),
                    GraniteEffect::Tunnel      => tunnel_intensity(u, v, t_seconds),
                    GraniteEffect::Swirl       => swirl_intensity(u, v, t_seconds),
                    GraniteEffect::RadialSweep => radial_intensity(u, v, t_seconds),
                    GraniteEffect::Cells       => cells_intensity(u, v, t_seconds),
                };
                let tone = palette_modulate(palette, intensity, palette_phase);
                let (cr, cg, cb) = palette_rgb(palette, tone, palette_phase);

                // Sample the previous frame at a slightly zoomed coordinate.
                let su = 0.5 + (u - 0.5) * inv_zoom;
                let sv = 0.5 + (v - 0.5) * inv_zoom;
                let (pr, pg, pb) = sample_prev_bilinear(prev, prev_w, prev_h, su, sv);

                let r = (cr * inv_feedback + pr * feedback).clamp(0.0, 1.0);
                let g = (cg * inv_feedback + pg * feedback).clamp(0.0, 1.0);
                let b = (cb * inv_feedback + pb * feedback).clamp(0.0, 1.0);

                let off = (x as usize) * 4;
                row[off    ] = (r * 255.0 + 0.5) as u8;
                row[off + 1] = (g * 255.0 + 0.5) as u8;
                row[off + 2] = (b * 255.0 + 0.5) as u8;
                row[off + 3] = 255;
            }
        });
}

// ---------------------------------------------------------------------------
// Effect kernels — each returns an "intensity" scalar in roughly [0, 1]
// ---------------------------------------------------------------------------

#[inline]
fn plasma_intensity(u: f32, v: f32, t_phase: f32) -> f32 {
    let w1 = (TAU * (u           + t_phase * 1.00 + v * 0.13)).sin();
    let w2 = (TAU * (u * PHI     + t_phase * 0.73 - v * 0.21)).sin();
    let w3 = (TAU * (u * SQRT_2  + t_phase * 0.51 + v * 0.07)).sin();
    let plasma = (w1 + w2 * 0.6 + w3 * 0.4) / 2.0;
    (0.5 + 0.5 * plasma).clamp(0.0, 1.0)
}

/// Forward-flying log-radial tunnel. Stripes spiral inward toward (0.5, 0.5).
#[inline]
fn tunnel_intensity(u: f32, v: f32, t: f32) -> f32 {
    let cx = u - 0.5;
    let cy = v - 0.5;
    let r = (cx * cx + cy * cy).sqrt().max(1e-4);
    let log_r = r.ln(); // negative for r<1; that's fine, sin wraps
    let stripe = (log_r * 6.0 + t * 1.4).sin() * 0.5 + 0.5;
    let darken_centre = (r * 4.0).clamp(0.0, 1.0); // black hole at centre
    (stripe * darken_centre).clamp(0.0, 1.0)
}

/// Galactic spiral whose arm count breathes with t.
#[inline]
fn swirl_intensity(u: f32, v: f32, t: f32) -> f32 {
    let cx = u - 0.5;
    let cy = v - 0.5;
    let r = (cx * cx + cy * cy).sqrt();
    let a = cy.atan2(cx);
    let arms = 3.0;
    let twist = (t * 0.3).sin() * 4.0;
    let phi = a + twist * r;
    let s = (phi * arms + t * 0.6).sin() * 0.5 + 0.5;
    (s * (1.0 - (r * 1.6).clamp(0.0, 1.0)) + 0.05).clamp(0.0, 1.0)
}

/// Radial wedges sweeping outward.
#[inline]
fn radial_intensity(u: f32, v: f32, t: f32) -> f32 {
    let cx = u - 0.5;
    let cy = v - 0.5;
    let r = (cx * cx + cy * cy).sqrt();
    let a = cy.atan2(cx);
    let n_wedges = 8.0;
    let wedge = ((a * n_wedges / TAU + t * 0.25).fract() * TAU).sin() * 0.5 + 0.5;
    let pulse = ((r * 5.0 - t * 1.2).sin() * 0.5 + 0.5).clamp(0.0, 1.0);
    (wedge * pulse).clamp(0.0, 1.0)
}

/// Cellular lattice — coloured tiles drifting on time.
#[inline]
fn cells_intensity(u: f32, v: f32, t: f32) -> f32 {
    let scale = 12.0;
    let sx = (u * scale + (t * 0.5).sin()).floor();
    let sy = (v * scale + (t * 0.5).cos()).floor();
    // Cheap hash-ish: irrational dot product wrapped to [0, 1] via sin.
    let h = (sx * 12.9898 + sy * 78.233 + t * 0.7).sin().abs();
    h.clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Palette: scalar intensity envelope and gradient sampling
// ---------------------------------------------------------------------------

fn palette_modulate(palette: GranitePalette, intensity: f32, palette_phase: f32) -> f32 {
    match palette {
        GranitePalette::Granite => 0.15 + 0.70 * intensity * (0.7 + 0.3 * palette_phase),
        GranitePalette::Fire    => 0.05 + 0.85 * (intensity * intensity) * (0.6 + 0.4 * palette_phase),
        GranitePalette::Neon    => {
            let sharp = (intensity * PI).sin().abs();
            0.05 + 0.90 * sharp * (0.8 + 0.2 * palette_phase)
        }
    }
}

fn palette_rgb(palette: GranitePalette, tone: f32, palette_phase: f32) -> (f32, f32, f32) {
    let t = tone.clamp(0.0, 1.0);
    match palette {
        GranitePalette::Granite => {
            let stops = [
                (0.10, 0.12, 0.18),
                (0.45, 0.45, 0.50),
                (0.85, 0.65, 0.30),
                (0.55 + 0.10 * palette_phase, 0.25, 0.55 + 0.05 * palette_phase),
            ];
            gradient_lerp(&stops, t)
        }
        GranitePalette::Fire => {
            let stops = [
                (0.00, 0.00, 0.00),
                (0.50, 0.05, 0.00),
                (0.95, 0.45, 0.05),
                (1.00, 0.95, 0.30 + 0.10 * palette_phase),
            ];
            gradient_lerp(&stops, t)
        }
        GranitePalette::Neon => {
            let stops = [
                (0.02, 0.05, 0.10),
                (0.05, 0.55, 0.65),
                (0.10, 0.85, 0.30),
                (0.70 * palette_phase + 0.30, 1.00, 0.50),
            ];
            gradient_lerp(&stops, t)
        }
    }
}

fn gradient_lerp(stops: &[(f32, f32, f32); 4], t: f32) -> (f32, f32, f32) {
    let t = t.clamp(0.0, 1.0);
    let scaled = t * 3.0;
    let idx = scaled.floor() as usize;
    let local = scaled - idx as f32;
    let (a, b) = if idx >= 3 {
        (stops[2], stops[3])
    } else {
        (stops[idx], stops[idx + 1])
    };
    (
        a.0 + (b.0 - a.0) * local,
        a.1 + (b.1 - a.1) * local,
        a.2 + (b.2 - a.2) * local,
    )
}

// ---------------------------------------------------------------------------
// Bilinear sample of `prev` at fractional UV in [0, 1]^2
// ---------------------------------------------------------------------------

#[inline]
fn sample_prev_bilinear(prev: &[u8], w: i32, h: i32, u: f32, v: f32) -> (f32, f32, f32) {
    let u = u.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let fx = u * (w - 1) as f32;
    let fy = v * (h - 1) as f32;
    let x0 = fx as i32;
    let y0 = fy as i32;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let dx = fx - x0 as f32;
    let dy = fy - y0 as f32;

    let load = |x: i32, y: i32| -> (f32, f32, f32) {
        let i = (y as usize * w as usize + x as usize) * 4;
        (
            prev[i    ] as f32 * (1.0 / 255.0),
            prev[i + 1] as f32 * (1.0 / 255.0),
            prev[i + 2] as f32 * (1.0 / 255.0),
        )
    };

    let (r00, g00, b00) = load(x0, y0);
    let (r10, g10, b10) = load(x1, y0);
    let (r01, g01, b01) = load(x0, y1);
    let (r11, g11, b11) = load(x1, y1);

    let mix = |a: f32, b: f32, c: f32, d: f32| -> f32 {
        let top = a + (b - a) * dx;
        let bot = c + (d - c) * dx;
        top + (bot - top) * dy
    };

    (mix(r00, r10, r01, r11), mix(g00, g10, g01, g11), mix(b00, b10, b01, b11))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn buf_for(w: u32, h: u32) -> Vec<u8> {
        vec![0u8; (w * h * 4) as usize]
    }

    fn luminance_total(buf: &[u8]) -> u64 {
        buf.chunks_exact(4)
            .map(|p| p[0] as u64 + p[1] as u64 + p[2] as u64)
            .sum()
    }

    #[test]
    fn render_active_writes_nonzero() {
        let mut g = Granite::new(64, 36);
        let mut dst = buf_for(64, 36);
        g.render(&mut dst, 64, 36, 1.0, true, &[], &GraniteConfig::default());
        assert!(luminance_total(&dst) > 0);
    }

    #[test]
    fn inactive_decays_to_black() {
        let mut g = Granite::new(32, 18);
        let mut dst = buf_for(32, 18);
        g.render(&mut dst, 32, 18, 1.0, true, &[], &GraniteConfig::default());
        let initial = luminance_total(&dst);
        assert!(initial > 0);
        for _ in 0..60 {
            g.render(&mut dst, 32, 18, 1.0, false, &[], &GraniteConfig::default());
        }
        let final_lum = luminance_total(&dst);
        assert!(final_lum * 10 < initial,
                "expected ≥ 90% decay; initial = {initial}, final = {final_lum}");
    }

    #[test]
    fn palettes_produce_in_range_bytes() {
        let mut g = Granite::new(48, 27);
        let mut dst = buf_for(48, 27);
        for palette in [GranitePalette::Granite, GranitePalette::Fire, GranitePalette::Neon] {
            let cfg = GraniteConfig { palette, auto_switch: false, ..Default::default() };
            for f in 0..30 {
                g.render(&mut dst, 48, 27, f as f32 / 30.0, true, &[], &cfg);
            }
            for px in dst.chunks_exact(4) {
                assert_eq!(px[3], 255, "alpha drift on palette {palette:?}");
            }
        }
    }

    #[test]
    fn resize_clears_prev_and_no_panic() {
        let mut g = Granite::new(64, 36);
        let mut dst1 = buf_for(64, 36);
        g.render(&mut dst1, 64, 36, 1.0, true, &[], &GraniteConfig::default());

        let mut dst2 = buf_for(96, 54);
        g.render(&mut dst2, 96, 54, 1.0, true, &[], &GraniteConfig::default());
        assert_eq!(g.w, 96);
        assert_eq!(g.h, 54);

        let mut dst3 = buf_for(64, 36);
        g.render(&mut dst3, 64, 36, 1.0, true, &[], &GraniteConfig::default());
        assert_eq!(g.w, 64);
        assert_eq!(g.h, 36);
    }

    #[test]
    fn render_is_deterministic() {
        let cfg = GraniteConfig { auto_switch: false, ..Default::default() };
        let mut g1 = Granite::new(32, 18);
        let mut g2 = Granite::new(32, 18);
        let mut a = buf_for(32, 18);
        let mut b = buf_for(32, 18);
        for f in 0..10 {
            g1.render(&mut a, 32, 18, f as f32 * 0.1, true, &[], &cfg);
            g2.render(&mut b, 32, 18, f as f32 * 0.1, true, &[], &cfg);
        }
        assert_eq!(a, b);
    }

    #[test]
    fn each_effect_renders_distinct() {
        // Pin auto_switch off; iterate explicit effects; expect each to produce
        // a non-black output and a different output from the others.
        use std::collections::HashSet;
        let mut hashes: HashSet<u64> = HashSet::new();
        for effect in ALL_EFFECTS.iter().copied() {
            let mut g = Granite::new(48, 27);
            let mut dst = buf_for(48, 27);
            let cfg = GraniteConfig { auto_switch: false, effect, ..Default::default() };
            // Burn a few frames so feedback settles.
            for f in 0..5 {
                g.render(&mut dst, 48, 27, f as f32 * 0.1, true, &[], &cfg);
            }
            assert!(luminance_total(&dst) > 0, "effect {effect:?} produced black");
            // Cheap content hash.
            let h = dst.iter().fold(0u64, |acc, &b| acc.wrapping_mul(31).wrapping_add(b as u64));
            assert!(hashes.insert(h), "effect {effect:?} duplicates another");
        }
    }

    #[test]
    fn auto_switch_changes_effect_within_max_interval() {
        let cfg = GraniteConfig { auto_switch: true, ..Default::default() };
        let mut g = Granite::new(16, 9);
        let mut dst = buf_for(16, 9);
        let start = g.active_effect();
        // Run enough frames to guarantee at least one switch + crossfade
        // completion: max interval (720) + crossfade (30) + slack.
        for f in 0..800 {
            g.render(&mut dst, 16, 9, f as f32 * 0.033, true, &[], &cfg);
        }
        assert_ne!(g.active_effect(), start, "scheduler never advanced");
    }
}
