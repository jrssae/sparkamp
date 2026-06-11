//! Granite — Geiss-inspired plasma visualizer.
//!
//! Faithful re-creation of the original Geiss rendering model
//! (geisswerks.com/geiss/secrets.html): a single-channel f32 intensity
//! buffer is pushed through a precomputed per-pixel displacement map every
//! frame (warp), multiplied by a decay just under 1.0, and the audio
//! waveform is stamped into it as fresh "ink". A 256-entry palette LUT maps
//! intensity to RGBA at display time. All motion comes from repeated
//! application of the static warp map — the ink drawn this frame is carried,
//! smeared and dissolved by the warp on every following frame, which is the
//! signature Geiss flow.
//!
//! Differences from the 1998 original are deliberate modernisations with the
//! same purpose: f32 math instead of integer weights + error-diffusion
//! (kills quantisation sticking), rayon rows instead of handwritten MMX
//! assembly, and synchronous map generation instead of row-by-row background
//! builds (a full 640×360 map takes a few milliseconds today).
//!
//! Both frontends (GTK + macOS) call [`Granite::render`] each frame to fill
//! a caller-owned RGBA8 buffer at the granite-internal resolution; the
//! windowing system's GPU compositor handles the upscale to display size.

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

/// Waveform ink levels. With beat-linked brightness on (the default), ink
/// sits at the quiet level and snaps to full on detected beats — Geiss
/// v4.00's "wave brightness sharply linked to the beat". With it off, ink
/// is a constant mid-bright stroke (Geiss made this optional in 4.24b).
const INK_QUIET: f32 = 0.80;
const INK_BEAT:  f32 = 1.00;
const INK_FLAT:  f32 = 0.92;

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

/// Which warp-map family drives the motion. Each is a displacement field;
/// activating one randomises its parameters, so the same family never plays
/// exactly the same twice (mirrors Geiss's per-activation map variation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraniteEffect {
    /// Multi-sine turbulence — ink wanders and folds like smoke.
    Plasma,
    /// Contraction toward centre — everything streams into the middle.
    Tunnel,
    /// Radius-falloff twist — centre spins faster than the rim.
    Swirl,
    /// Uniform rotation with slight zoom (UI label "Spin"; serde name kept
    /// from the pre-warp-map era for config compatibility).
    #[serde(rename = "radial_sweep")]
    RadialSweep,
    /// Per-cell mosaic shear — blocky directional drift.
    Cells,
    /// Expansion from centre — ink blooms outward.
    Explode,
    /// Radial sine rings — alternating bands flow in and out.
    Ripple,
    /// Axis-aligned sine shear waves.
    Shear,
    /// Mirror-fold into sectors plus slow rotation.
    Kaleido,
    /// Suction toward a randomly-placed off-centre point, with spin.
    #[serde(rename = "gravity_well")]
    GravityWell,
    /// Rotation plus contraction — spiral down the plughole.
    Drain,
    /// Undulating vertical wave with constant sideways scroll.
    Flag,
}

impl Default for GraniteEffect {
    fn default() -> Self { GraniteEffect::Plasma }
}

const ALL_EFFECTS: [GraniteEffect; 12] = [
    GraniteEffect::Plasma,
    GraniteEffect::Tunnel,
    GraniteEffect::Swirl,
    GraniteEffect::RadialSweep,
    GraniteEffect::Cells,
    GraniteEffect::Explode,
    GraniteEffect::Ripple,
    GraniteEffect::Shear,
    GraniteEffect::Kaleido,
    GraniteEffect::GravityWell,
    GraniteEffect::Drain,
    GraniteEffect::Flag,
];

/// Shape of the PCM-driven waveform line that's drawn into each frame and
/// then dissolved by the warp on subsequent frames (Geiss "scope" flow).
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
fn default_beat_sensitivity() -> f32 { 1.5 }

/// User-tunable settings that feed the kernel.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct GraniteConfig {
    /// Flow speed multiplier: scales the warp displacement per frame.
    pub speed:    f32,
    pub palette:  GranitePalette,
    /// Trail persistence. Maps to the per-frame decay factor (0.92–0.995);
    /// the original tuned this as the bilinear weight sum (251–256 of 256).
    pub feedback: f32,
    /// Currently-selected effect when `auto_switch` is off. When `auto_switch`
    /// is on, this records whatever the scheduler last landed on so the UI
    /// can reflect the live state.
    #[serde(default)]
    pub effect: GraniteEffect,
    /// When true, the scheduler rotates effects every 12–24 s.
    #[serde(default = "default_true")]
    pub auto_switch: bool,
    /// Beat detection threshold: bass energy must exceed its own recent
    /// average by this factor to count as a beat. Lower = more beats.
    /// Clamped to 1.05–3.0 at use.
    #[serde(default = "default_beat_sensitivity")]
    pub beat_sensitivity: f32,
    /// Brighten the waveform ink on detected beats.
    #[serde(default = "default_true")]
    pub beat_brightness: bool,
}

impl Default for GraniteConfig {
    fn default() -> Self {
        GraniteConfig {
            speed: 1.0,
            palette: GranitePalette::Granite,
            feedback: 0.6,
            effect: GraniteEffect::Plasma,
            auto_switch: true,
            beat_sensitivity: default_beat_sensitivity(),
            beat_brightness: true,
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

/// Per-frame intensity retention. feedback 0.0 → 0.92 (ink gone in ~1 s),
/// 0.9 → 0.995 (trails linger ~15 s at 30 fps).
fn trail_decay(feedback: f32) -> f32 {
    0.92 + feedback * (0.075 / 0.9)
}

// ---------------------------------------------------------------------------
// Warp map — precomputed per-pixel source coordinates
// ---------------------------------------------------------------------------

/// For every destination pixel, the fractional source position to sample the
/// previous frame at. The modern equivalent of Geiss's 6-bytes-per-pixel
/// offset+weight tables: storing coordinates instead lets the warp kernel
/// derive bilinear weights inline and lets two maps be lerped during a
/// crossfade (blending displacement fields is itself a valid field).
struct WarpMap {
    sx: Vec<f32>,
    sy: Vec<f32>,
}

impl WarpMap {
    fn empty() -> Self {
        WarpMap { sx: Vec::new(), sy: Vec::new() }
    }
}

/// Build a map by evaluating `f(u, v) -> (su, sv)` in normalized [0,1] space
/// for every pixel, storing the result as fractional pixel coordinates.
/// Coordinates may land outside the buffer; sampling treats outside as black,
/// which feeds darkness in from the borders (how Geiss frames stayed clean).
fn fill_map(w: u32, h: u32, f: impl Fn(f32, f32) -> (f32, f32) + Sync) -> WarpMap {
    let wu = w as usize;
    let hu = h as usize;
    let wf = (w as f32 - 1.0).max(1.0);
    let hf = (h as f32 - 1.0).max(1.0);
    let mut sx = vec![0.0f32; wu * hu];
    let mut sy = vec![0.0f32; wu * hu];
    sx.par_chunks_mut(wu)
        .zip(sy.par_chunks_mut(wu))
        .enumerate()
        .for_each(|(y, (row_x, row_y))| {
            let v = y as f32 / hf;
            for x in 0..wu {
                let u = x as f32 / wf;
                let (su, sv) = f(u, v);
                row_x[x] = su * wf;
                row_y[x] = sv * hf;
            }
        });
    WarpMap { sx, sy }
}

/// Random ±1.0 — many families flip direction per activation.
fn rsign(rng: &mut StdRng) -> f32 {
    if rng.gen_bool(0.5) { 1.0 } else { -1.0 }
}

/// Generate a freshly-parameterised displacement map for `effect`.
///
/// All families work in centred, aspect-corrected coordinates so circles stay
/// circles on non-square viewports. Displacements are a fraction of a percent
/// of the frame per application; motion accumulates because the map is applied
/// every frame (Geiss's core trick: static map, iterative application).
fn generate_warp_map(effect: GraniteEffect, w: u32, h: u32, rng: &mut StdRng) -> WarpMap {
    let aspect = (w as f32 / h.max(1) as f32).max(0.1);
    let to_c = move |u: f32, v: f32| ((u - 0.5) * aspect, v - 0.5);
    let from_c = move |ax: f32, ay: f32| (ax / aspect + 0.5, ay + 0.5);

    match effect {
        GraniteEffect::Plasma => {
            let a1 = rng.gen_range(0.002..0.006f32);
            let a2 = rng.gen_range(0.001..0.004f32);
            let k1 = rng.gen_range(1.0..3.0f32);
            let k2 = k1 * PHI;
            let k3 = rng.gen_range(1.0..2.5f32);
            let k4 = rng.gen_range(1.0..3.0f32);
            let k5 = k4 * SQRT_2;
            let p1 = rng.gen_range(0.0..TAU);
            let p2 = rng.gen_range(0.0..TAU);
            let p3 = rng.gen_range(0.0..TAU);
            let p4 = rng.gen_range(0.0..TAU);
            fill_map(w, h, move |u, v| {
                let du = a1 * (v * k1 * TAU + p1).sin()
                       + a2 * ((v * k2 + u * k3) * TAU + p2).sin();
                let dv = a1 * (u * k4 * TAU + p3).sin()
                       + a2 * ((u * k5 + v * k3) * TAU + p4).sin();
                (u + du, v + dv)
            })
        }
        GraniteEffect::Tunnel => {
            let s = rng.gen_range(1.008..1.025f32);
            let th = rng.gen_range(-0.012..0.012f32);
            let (sin_t, cos_t) = th.sin_cos();
            fill_map(w, h, move |u, v| {
                let (ax, ay) = to_c(u, v);
                let rx = ax * cos_t - ay * sin_t;
                let ry = ax * sin_t + ay * cos_t;
                from_c(rx * s, ry * s)
            })
        }
        GraniteEffect::Swirl => {
            let sigma = rng.gen_range(0.18..0.40f32);
            let th_max = rng.gen_range(0.05..0.11f32) * rsign(rng);
            let s = rng.gen_range(0.998..1.006f32);
            let inv_sigma2 = 1.0 / (sigma * sigma);
            fill_map(w, h, move |u, v| {
                let (ax, ay) = to_c(u, v);
                let r2 = ax * ax + ay * ay;
                let th = th_max * (-r2 * inv_sigma2).exp();
                let (sin_t, cos_t) = th.sin_cos();
                let rx = ax * cos_t - ay * sin_t;
                let ry = ax * sin_t + ay * cos_t;
                from_c(rx * s, ry * s)
            })
        }
        GraniteEffect::RadialSweep => {
            let th = rng.gen_range(0.018..0.05f32) * rsign(rng);
            let s = rng.gen_range(0.995..1.005f32);
            let (sin_t, cos_t) = th.sin_cos();
            fill_map(w, h, move |u, v| {
                let (ax, ay) = to_c(u, v);
                let rx = ax * cos_t - ay * sin_t;
                let ry = ax * sin_t + ay * cos_t;
                from_c(rx * s, ry * s)
            })
        }
        GraniteEffect::Cells => {
            let n = rng.gen_range(6.0..18.0f32).floor();
            let mag = rng.gen_range(0.002..0.006f32);
            let seed = rng.gen_range(0.0..100.0f32);
            fill_map(w, h, move |u, v| {
                let cx = (u * n).floor();
                let cy = (v * n).floor();
                // Cheap hash: irrational dot product through sin, like the
                // classic GLSL one-liner. Only needs to look uncorrelated.
                let hsh = (cx * 12.9898 + cy * 78.233 + seed).sin() * 43_758.547;
                let ang = hsh.fract() * TAU;
                (u + ang.cos() * mag, v + ang.sin() * mag)
            })
        }
        GraniteEffect::Explode => {
            let s = rng.gen_range(0.978..0.994f32);
            let th = rng.gen_range(-0.010..0.010f32);
            let (sin_t, cos_t) = th.sin_cos();
            fill_map(w, h, move |u, v| {
                let (ax, ay) = to_c(u, v);
                let rx = ax * cos_t - ay * sin_t;
                let ry = ax * sin_t + ay * cos_t;
                from_c(rx * s, ry * s)
            })
        }
        GraniteEffect::Ripple => {
            let k = rng.gen_range(4.0..9.0f32);
            let a = rng.gen_range(0.0025..0.006f32);
            let ph = rng.gen_range(0.0..TAU);
            let s = rng.gen_range(0.999..1.004f32);
            fill_map(w, h, move |u, v| {
                let (ax, ay) = to_c(u, v);
                let r = (ax * ax + ay * ay).sqrt();
                if r < 1e-4 {
                    return (u, v);
                }
                let dr = a * (r * k * TAU + ph).sin();
                let rs = (r + dr) * s / r;
                from_c(ax * rs, ay * rs)
            })
        }
        GraniteEffect::Shear => {
            let amp_x = rng.gen_range(0.003..0.008f32) * rsign(rng);
            let amp_y = rng.gen_range(0.003..0.008f32) * rsign(rng);
            let kx = rng.gen_range(1.0..3.0f32);
            let ky = rng.gen_range(1.0..3.0f32);
            let p1 = rng.gen_range(0.0..TAU);
            let p2 = rng.gen_range(0.0..TAU);
            fill_map(w, h, move |u, v| {
                (
                    u + amp_x * (v * ky * TAU + p1).sin(),
                    v + amp_y * (u * kx * TAU + p2).sin(),
                )
            })
        }
        GraniteEffect::Kaleido => {
            let sectors = [3.0f32, 4.0, 5.0, 6.0][rng.gen_range(0..4)];
            let th = rng.gen_range(0.015..0.035f32) * rsign(rng);
            let s = rng.gen_range(1.002..1.012f32);
            let sec = PI / sectors;
            fill_map(w, h, move |u, v| {
                let (ax, ay) = to_c(u, v);
                let r = (ax * ax + ay * ay).sqrt();
                let a = ay.atan2(ax);
                // Mirror-fold the angle into one sector, then rotate the
                // wedge slowly so the folded content keeps moving.
                let am = ((a % (2.0 * sec)) + 2.0 * sec) % (2.0 * sec);
                let folded = if am > sec { 2.0 * sec - am } else { am };
                let a2 = folded + th;
                from_c(r * a2.cos() * s, r * a2.sin() * s)
            })
        }
        GraniteEffect::GravityWell => {
            let px = rng.gen_range(-0.25..0.25f32) * aspect;
            let py = rng.gen_range(-0.22..0.22f32);
            let g = rng.gen_range(0.0012..0.0035f32);
            let spin = rng.gen_range(0.010..0.030f32) * rsign(rng);
            let (sin_t, cos_t) = spin.sin_cos();
            fill_map(w, h, move |u, v| {
                let (ax, ay) = to_c(u, v);
                let dx = ax - px;
                let dy = ay - py;
                let d = (dx * dx + dy * dy).sqrt().max(0.03);
                // Rotate about the well, then sample slightly further from it
                // so content streams inward over successive frames.
                let rx = dx * cos_t - dy * sin_t;
                let ry = dx * sin_t + dy * cos_t;
                let pull = (g / d).min(0.02);
                let scale = 1.0 + pull / d;
                from_c(px + rx * scale, py + ry * scale)
            })
        }
        GraniteEffect::Drain => {
            let th = rng.gen_range(0.03..0.07f32) * rsign(rng);
            let s = rng.gen_range(1.012..1.030f32);
            let (sin_t, cos_t) = th.sin_cos();
            fill_map(w, h, move |u, v| {
                let (ax, ay) = to_c(u, v);
                let rx = ax * cos_t - ay * sin_t;
                let ry = ax * sin_t + ay * cos_t;
                from_c(rx * s, ry * s)
            })
        }
        GraniteEffect::Flag => {
            let a = rng.gen_range(0.004..0.010f32);
            let k = rng.gen_range(1.0..2.5f32);
            let drift = rng.gen_range(0.002..0.005f32) * rsign(rng);
            let ph = rng.gen_range(0.0..TAU);
            fill_map(w, h, move |u, v| {
                (u + drift, v + a * (u * k * TAU + ph).sin())
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Beat detection
// ---------------------------------------------------------------------------

/// Frames of bass-energy history the detector compares against — ~1.4 s at
/// 30 fps, long enough to track the song's current loudness, short enough
/// to follow section changes.
const BEAT_HISTORY: usize = 43;

/// Per-beat accent saliences kept for meter (time-signature) analysis —
/// 24 beats ≈ 6–8 bars, enough for the 3-vs-4 autocorrelation to settle.
const ACCENT_RING: usize = 24;

/// What one frame of audio produced: nothing, a beat, or a beat that lands
/// on the estimated downbeat (the "1" of the measure).
#[derive(Clone, Copy)]
struct BeatTick {
    is_beat: bool,
    is_downbeat: bool,
}

const NO_TICK: BeatTick = BeatTick { is_beat: false, is_downbeat: false };

/// Online beat + meter detector fed the same PCM window the scope ink uses.
///
/// Beats trigger on **onset flux** — the frame-to-frame *rise* in band
/// energy — not on the energy level itself. A sustained bass note has zero
/// flux after its attack, so it cannot re-trigger; the level-triggered
/// first version machine-gunned at the refractory rate (exactly 200 BPM)
/// through quiet intros. Two bands (one-pole low-pass ≈ kick, remainder ≈
/// snare/mids) feed both the trigger and the per-beat accent salience.
///
/// Meter: the salience sequence is autocful at lags 3 and 4 — accents
/// repeat every measure, so the better-correlating lag is the beats-per-
/// measure estimate (vote-hysteresis smoothed). The downbeat phase is the
/// offset class with the highest mean salience.
struct BeatDetector {
    bass_lpf: f32,
    prev_bass: f32,
    prev_mid: f32,
    flux_hist: [f32; BEAT_HISTORY],
    fh_idx: usize,
    fh_filled: usize,
    refractory: u8,
    // BPM estimation: recent inter-beat intervals in frames (30 fps).
    intervals: [u16; 8],
    interval_idx: usize,
    interval_count: usize,
    frames_since_beat: u32,
    // Meter / downbeat estimation.
    salience: [f32; ACCENT_RING],
    beat_index: usize,
    beat_count: usize,
    meter: u8,
    meter_votes: i32,
    anchor: usize,
}

impl BeatDetector {
    fn new() -> Self {
        BeatDetector {
            bass_lpf: 0.0,
            prev_bass: 0.0,
            prev_mid: 0.0,
            flux_hist: [0.0; BEAT_HISTORY],
            fh_idx: 0,
            fh_filled: 0,
            refractory: 0,
            intervals: [0; 8],
            interval_idx: 0,
            interval_count: 0,
            frames_since_beat: 0,
            salience: [0.0; ACCENT_RING],
            beat_index: 0,
            beat_count: 0,
            meter: 4,
            meter_votes: 0,
            anchor: 0,
        }
    }

    /// Feed one frame's PCM window.
    fn process(&mut self, pcm: &[f32], sensitivity: f32) -> BeatTick {
        self.frames_since_beat = self.frames_since_beat.saturating_add(1);
        if pcm.is_empty() {
            self.refractory = self.refractory.saturating_sub(1);
            return NO_TICK;
        }
        // One-pole low-pass keeps roughly the bottom ~150 Hz of a 44.1 kHz
        // window (kick); everything above it is the mid band (snare et al).
        let mut lpf = self.bass_lpf;
        let mut bass_e = 0.0f32;
        let mut full_e = 0.0f32;
        for &x in pcm {
            lpf += 0.02 * (x - lpf);
            bass_e += lpf * lpf;
            full_e += x * x;
        }
        self.bass_lpf = lpf;
        let n = pcm.len() as f32;
        bass_e /= n;
        full_e /= n;
        let mid_e = (full_e - bass_e).max(0.0);

        // Onset flux: only energy RISES count. Kick-weighted because the
        // beat grid lives in the low end.
        let bass_flux = (bass_e - self.prev_bass).max(0.0);
        let mid_flux = (mid_e - self.prev_mid).max(0.0);
        self.prev_bass = bass_e;
        self.prev_mid = mid_e;
        let flux = bass_flux * 2.0 + mid_flux;

        let mean = if self.fh_filled > 0 {
            self.flux_hist[..self.fh_filled].iter().sum::<f32>() / self.fh_filled as f32
        } else {
            0.0
        };
        self.flux_hist[self.fh_idx] = flux;
        self.fh_idx = (self.fh_idx + 1) % BEAT_HISTORY;
        self.fh_filled = (self.fh_filled + 1).min(BEAT_HISTORY);

        if self.refractory > 0 {
            self.refractory -= 1;
            return NO_TICK;
        }
        // The 1e-6 floor keeps near-silence from "beating" on noise; the
        // warm-up guard keeps the first frames from comparing against an
        // empty history.
        let is_beat = self.fh_filled >= 10
            && flux > 1e-6
            && flux > mean * sensitivity;
        if !is_beat {
            return NO_TICK;
        }
        self.refractory = 8; // ~270 ms at 30 fps — caps at ~220 BPM

        // Inter-beat interval for BPM; gaps over ~3 s mean playback paused
        // or the song broke down — not an interval worth averaging.
        if self.frames_since_beat <= 90 && self.frames_since_beat > 0 {
            self.intervals[self.interval_idx] = self.frames_since_beat as u16;
            self.interval_idx = (self.interval_idx + 1) % self.intervals.len();
            self.interval_count = (self.interval_count + 1).min(self.intervals.len());
        }
        self.frames_since_beat = 0;

        // Accent bookkeeping for meter + downbeat.
        self.beat_index = self.beat_index.wrapping_add(1);
        self.salience[self.beat_index % ACCENT_RING] = flux;
        self.beat_count = (self.beat_count + 1).min(ACCENT_RING);
        self.update_meter();

        let is_downbeat = self.meter_known()
            && self.beat_index % self.meter as usize == self.anchor;
        BeatTick { is_beat: true, is_downbeat }
    }

    /// Salience of the beat `j` beats before the latest one.
    fn sal(&self, j: usize) -> f32 {
        self.salience[(self.beat_index + ACCENT_RING - j) % ACCENT_RING]
    }

    fn meter_known(&self) -> bool {
        self.beat_count >= 12
    }

    /// Re-estimate beats-per-measure (3 vs 4) and the downbeat phase from
    /// the recent accent saliences. Vote hysteresis keeps the meter from
    /// flapping frame to frame; the anchor only moves on a clear win.
    fn update_meter(&mut self) {
        if !self.meter_known() {
            return;
        }
        let n = self.beat_count;
        let score = |lag: usize| -> f32 {
            let mut s = 0.0;
            let mut c = 0;
            for j in 0..n.saturating_sub(lag) {
                s += self.sal(j) * self.sal(j + lag);
                c += 1;
            }
            if c == 0 { 0.0 } else { s / c as f32 }
        };
        let s3 = score(3);
        let s4 = score(4);
        if s4 > s3 * 1.05 {
            self.meter_votes = (self.meter_votes + 1).min(8);
        } else if s3 > s4 * 1.05 {
            self.meter_votes = (self.meter_votes - 1).max(-8);
        }
        self.meter = if self.meter_votes >= 0 { 4 } else { 3 };

        // Downbeat phase: the offset class with the highest mean salience.
        let m = self.meter as usize;
        let mut best_r = 0usize;
        let mut best = -1.0f32;
        let mut current = -1.0f32;
        for r in 0..m {
            let mut s = 0.0;
            let mut c = 0;
            for j in 0..n {
                // `+ m * ACCENT_RING` keeps the subtraction positive; it is
                // a multiple of m so it never changes the offset class.
                if (self.beat_index + m * ACCENT_RING - j) % m == r {
                    s += self.sal(j);
                    c += 1;
                }
            }
            let avg = if c > 0 { s / c as f32 } else { 0.0 };
            if r == self.anchor % m {
                current = avg;
            }
            if avg > best {
                best = avg;
                best_r = r;
            }
        }
        if best_r != self.anchor % m && best > current * 1.15 {
            self.anchor = best_r;
        }
        self.anchor %= m;
    }

    /// Estimated beats per measure (3 or 4); 0 until enough beats analysed.
    fn meter(&self) -> u8 {
        if self.meter_known() { self.meter } else { 0 }
    }

    /// Estimated tempo from the median of recent inter-beat intervals.
    /// 0.0 until at least two intervals exist or after ~5 s without a beat.
    /// Assumes the 30 fps cadence both frontends drive the renderer at.
    /// Estimates above 180 fold down an octave — a visualizer wants the
    /// felt tempo, and >180 readings are nearly always double-time locks.
    fn bpm(&self) -> f32 {
        if self.interval_count < 2 || self.frames_since_beat > 150 {
            return 0.0;
        }
        let mut v = [0u16; 8];
        v[..self.interval_count].copy_from_slice(&self.intervals[..self.interval_count]);
        let v = &mut v[..self.interval_count];
        v.sort_unstable();
        let median = v[v.len() / 2] as f32;
        if median <= 0.0 {
            return 0.0;
        }
        let mut bpm = 60.0 * 30.0 / median;
        if bpm > 180.0 {
            bpm *= 0.5;
        }
        bpm
    }
}

// ---------------------------------------------------------------------------
// Renderer state
// ---------------------------------------------------------------------------

/// Holds the f32 intensity feedback buffers, the active warp map (plus the
/// incoming one during a crossfade), and the effect-rotation scheduler.
pub struct Granite {
    prev: Vec<f32>,
    curr: Vec<f32>,
    w:    u32,
    h:    u32,
    frame: u64,
    // Scheduler.
    current: GraniteEffect,
    next: Option<GraniteEffect>,
    map_current: WarpMap,
    map_next: Option<WarpMap>,
    current_palette: GranitePalette,
    next_palette: Option<GranitePalette>,
    current_shape: WaveShape,
    switch_at_frame: u64,
    crossfade_remaining: u8,
    rng: StdRng,
    // Beat reactions.
    beat: BeatDetector,
    /// 1.0 on a beat, decaying ~5 frames; drives the ink brightness boost.
    beat_glow: f32,
    /// 1.0 on a downbeat (the "1"), decaying ~5 frames; widens the ink
    /// stroke so measure starts visibly punch harder than ordinary beats.
    downbeat_glow: f32,
    /// Beat colour change: frames left in the LUT fade from
    /// `palette_fade_from` to `current_palette`.
    palette_fade_remaining: u8,
    palette_fade_from: GranitePalette,
    /// Dwell guard so beat-triggered switches never machine-gun.
    last_switch_frame: u64,
}

/// ~0.5 s LUT fade when a beat rolls a new palette.
const PALETTE_FADE_FRAMES: u8 = 15;

const CROSSFADE_FRAMES: u8 = 30; // ~1 s at 30 fps
const SWITCH_INTERVAL_MIN: u64 = 360;  // 12 s
const SWITCH_INTERVAL_MAX: u64 = 720;  // 24 s

impl Granite {
    /// Allocate a renderer for `w × h` pixels.
    pub fn new(w: u32, h: u32) -> Self {
        let mut g = Granite {
            prev: Vec::new(),
            curr: Vec::new(),
            w: 0,
            h: 0,
            frame: 0,
            current: GraniteEffect::Plasma,
            next: None,
            map_current: WarpMap::empty(),
            map_next: None,
            current_palette: GranitePalette::Granite,
            next_palette: None,
            current_shape: WaveShape::Line,
            // Seeded for reproducible unit tests; switch cadence is still
            // perceived as "random" over a multi-minute play session.
            switch_at_frame: SWITCH_INTERVAL_MIN,
            crossfade_remaining: 0,
            rng: StdRng::seed_from_u64(0xC0FFEE),
            beat: BeatDetector::new(),
            beat_glow: 0.0,
            downbeat_glow: 0.0,
            palette_fade_remaining: 0,
            palette_fade_from: GranitePalette::Granite,
            last_switch_frame: 0,
        };
        g.resize(w, h);
        g
    }

    /// Reallocate buffers and regenerate the active warp map if dimensions
    /// changed. Cancels any in-flight crossfade; the scheduler re-triggers
    /// it on the next tick because `switch_at_frame` is already in the past.
    pub fn resize(&mut self, w: u32, h: u32) {
        if self.w == w && self.h == h && !self.prev.is_empty() {
            return;
        }
        self.w = w;
        self.h = h;
        let need = (w as usize) * (h as usize);
        self.prev = vec![0.0; need];
        self.curr = vec![0.0; need];
        self.map_current = generate_warp_map(self.current, w, h, &mut self.rng);
        self.map_next = None;
        self.next = None;
        self.crossfade_remaining = 0;
    }

    /// Manually pin the active effect (used when the user picks one in
    /// Settings while `auto_switch` is on, to keep that selection visible
    /// for at least one full switch interval before the scheduler resumes).
    pub fn set_effect(&mut self, effect: GraniteEffect) {
        self.current = effect;
        self.next = None;
        self.next_palette = None;
        self.map_next = None;
        self.crossfade_remaining = 0;
        self.map_current = generate_warp_map(effect, self.w, self.h, &mut self.rng);
        self.last_switch_frame = self.frame;
        // Push the next auto-switch out by ~20 s.
        self.switch_at_frame = self.frame + 600;
    }

    /// Render one frame into `dst` (RGBA8, length `w*h*4`).
    ///
    /// `waveform` is PCM samples in `[-1, 1]`; when non-empty, the active
    /// scope shape (line / circle / square / etc.) is stamped INTO the
    /// feedback buffer as bright ink. The warp then carries that ink on every
    /// following frame — the dissolve is structural, not an overlay.
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
        }
        debug_assert_eq!(dst.len(), (w as usize) * (h as usize) * 4);

        let (speed, cfg_palette, feedback) = cfg.clamped();
        let palette_phase = palette_phase_at(t_seconds, speed);

        // Inactive: decay the feedback buffer toward black and present it.
        if !is_active {
            for v in self.prev.iter_mut() {
                *v *= 0.94;
            }
            let pal = if cfg.auto_switch { self.current_palette } else { cfg_palette };
            let lut = build_lut(pal, palette_phase);
            emit_rgba(&self.prev, dst, &lut, self.w as usize);
            return;
        }

        self.frame = self.frame.wrapping_add(1);

        // Beat tick: drives the ink brightness boost; downbeats (the "1" of
        // the estimated measure) drive colour changes and early map switches.
        let tick = self
            .beat
            .process(waveform, cfg.beat_sensitivity.clamp(1.05, 3.0));
        if tick.is_beat {
            self.beat_glow = 1.0;
        }
        if tick.is_downbeat {
            self.downbeat_glow = 1.0;
            // Downbeat colour change: about a third of measure starts roll a
            // new palette and fade the LUT over ~half a second. The geometry
            // is left alone — only the plasma's colours move. Auto mode only
            // (a pinned palette is the user's explicit choice), and skipped
            // during an effect crossfade, which is already blending palettes.
            if cfg.auto_switch
                && self.crossfade_remaining == 0
                && self.palette_fade_remaining == 0
                && self.rng.gen_bool(0.33)
            {
                self.palette_fade_from = self.current_palette;
                self.current_palette =
                    random_other_palette(self.current_palette, &mut self.rng);
                self.palette_fade_remaining = PALETTE_FADE_FRAMES;
            }
        }
        self.beat_glow *= 0.80; // ~5-frame tail
        self.downbeat_glow *= 0.80;

        // Scheduler: pick / advance / start crossfades.
        if cfg.auto_switch {
            // "Map now changes on beat" (Geiss v4.00), quantised to measure
            // starts: some downbeats pull the next switch forward, with a
            // ≥4 s dwell so it can't thrash. The 12–24 s timer remains the
            // fallback when no beats land.
            if tick.is_downbeat
                && self.crossfade_remaining == 0
                && self.frame.saturating_sub(self.last_switch_frame) > 120
                && self.rng.gen_bool(0.25)
            {
                self.switch_at_frame = self.frame;
            }
            self.tick_scheduler();
        } else {
            // User-pinned: snap to the configured effect + palette, drop any
            // in-flight crossfade.
            if cfg.effect != self.current {
                self.set_effect(cfg.effect);
            }
            self.next = None;
            self.map_next = None;
            self.next_palette = None;
            self.crossfade_remaining = 0;
            self.palette_fade_remaining = 0;
            self.current_palette = cfg_palette;
        }

        // Warp: pull the previous frame through the displacement map (lerped
        // toward the incoming map during a crossfade) and decay it.
        let alpha = if self.crossfade_remaining > 0 {
            1.0 - (self.crossfade_remaining as f32 / CROSSFADE_FRAMES as f32)
        } else {
            0.0
        };
        let map_b = if alpha > 0.0 { self.map_next.as_ref().map(|m| (m, alpha)) } else { None };
        apply_warp(
            &mut self.curr,
            &self.prev,
            &self.map_current,
            map_b,
            w,
            h,
            speed,
            trail_decay(feedback),
        );

        // Stamp fresh ink into the feedback buffer so the next frame's warp
        // carries it (this ordering is the entire Geiss flow). Downbeats
        // widen the stroke for a few frames so the "1" lands harder.
        if !waveform.is_empty() {
            let ink = if cfg.beat_brightness {
                INK_QUIET + (INK_BEAT - INK_QUIET) * self.beat_glow
            } else {
                INK_FLAT
            };
            let radius = 1.4 + 0.9 * self.downbeat_glow;
            draw_waveform_ink(
                &mut self.curr, w, h, waveform, self.current_shape, ink, radius,
            );
        }

        // Present through the palette LUT: effect crossfades blend the two
        // scheduled palettes; beat colour changes fade from the previous
        // palette; otherwise the active palette straight through.
        let pal_a = if cfg.auto_switch { self.current_palette } else { cfg_palette };
        let lut = if alpha > 0.0 && self.next_palette.is_some() {
            let pb = self.next_palette.unwrap_or(pal_a);
            let la = build_lut(pal_a, palette_phase);
            let lb = build_lut(pb, palette_phase);
            lerp_lut(&la, &lb, alpha)
        } else if self.palette_fade_remaining > 0 {
            self.palette_fade_remaining -= 1;
            let t = 1.0 - (self.palette_fade_remaining as f32 / PALETTE_FADE_FRAMES as f32);
            let la = build_lut(self.palette_fade_from, palette_phase);
            let lb = build_lut(pal_a, palette_phase);
            lerp_lut(&la, &lb, t)
        } else {
            build_lut(pal_a, palette_phase)
        };
        emit_rgba(&self.curr, dst, &lut, self.w as usize);
        std::mem::swap(&mut self.prev, &mut self.curr);
    }

    /// Read the active effect (after scheduler tick). Frontends use this to
    /// reflect what's actually on screen in the Settings dropdown when
    /// `auto_switch` is on.
    #[allow(dead_code)] // used by tests + macOS FFI; GTK reads config.effect instead.
    pub fn active_effect(&self) -> GraniteEffect { self.current }

    /// Estimated tempo of the playing audio (median of recent inter-beat
    /// intervals). 0.0 while the detector has too little data or the music
    /// stopped beating. Debug aid for the fullscreen FPS/BPM overlay.
    #[allow(dead_code)] // used by tests + macOS FFI; GTK doesn't surface BPM yet.
    pub fn bpm(&self) -> f32 {
        self.beat.bpm()
    }

    /// Estimated beats per measure (3 or 4); 0 until enough beats analysed.
    #[allow(dead_code)] // used by tests + macOS FFI; GTK doesn't surface it yet.
    pub fn meter(&self) -> u8 {
        self.beat.meter()
    }

    /// Begin an immediate switch to a random other effect (bound to a
    /// keyboard shortcut in the frontends). With `auto_switch` on the normal
    /// one-second crossfade plays out; in pinned mode the caller must also
    /// write the returned effect into its config, otherwise the pinned-snap
    /// path reverts the change on the next frame.
    pub fn random_switch(&mut self) -> GraniteEffect {
        let next_eff = random_other_effect(self.current, &mut self.rng);
        self.map_next = Some(generate_warp_map(next_eff, self.w, self.h, &mut self.rng));
        self.next = Some(next_eff);
        self.next_palette = Some(random_other_palette(self.current_palette, &mut self.rng));
        self.current_shape = random_other_shape(self.current_shape, &mut self.rng);
        self.crossfade_remaining = CROSSFADE_FRAMES;
        self.last_switch_frame = self.frame;
        next_eff
    }

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
                if let Some(m) = self.map_next.take() {
                    self.map_current = m;
                }
                if let Some(p) = self.next_palette.take() {
                    self.current_palette = p;
                }
                self.last_switch_frame = self.frame;
                // Schedule the next switch.
                let interval = self.rng.gen_range(SWITCH_INTERVAL_MIN..=SWITCH_INTERVAL_MAX);
                self.switch_at_frame = self.frame + interval;
            }
            return;
        }
        // Time for a new switch?
        if self.frame >= self.switch_at_frame {
            // An effect crossfade owns palette blending; drop any beat
            // colour fade still in flight.
            self.palette_fade_remaining = 0;
            let next_eff = random_other_effect(self.current, &mut self.rng);
            self.map_next = Some(generate_warp_map(next_eff, self.w, self.h, &mut self.rng));
            self.next = Some(next_eff);
            // Switching effects also rolls a new palette so the colour scheme
            // changes alongside the map — closer to original Geiss feel.
            self.next_palette = Some(random_other_palette(
                self.current_palette, &mut self.rng,
            ));
            // Snap the scope shape immediately too. We don't crossfade the
            // shape — the waveform ink dissolves into the plasma each frame,
            // so changing the shape mid-warp just looks like the next few
            // frames trace a new figure.
            self.current_shape = random_other_shape(self.current_shape, &mut self.rng);
            self.crossfade_remaining = CROSSFADE_FRAMES;
        }
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
// Warp kernel
// ---------------------------------------------------------------------------

/// `curr[p] = bilinear(prev, map[p]) * decay` for every pixel, in parallel.
///
/// `speed` scales the stored displacement away from identity at sample time,
/// so the speed slider acts live without regenerating maps. During a
/// crossfade `map_b = Some((incoming_map, alpha))` and the two displacement
/// fields are lerped per pixel — blending fields morphs the flow smoothly.
#[allow(clippy::too_many_arguments)]
fn apply_warp(
    curr: &mut [f32],
    prev: &[f32],
    map_a: &WarpMap,
    map_b: Option<(&WarpMap, f32)>,
    w: u32,
    h: u32,
    speed: f32,
    decay: f32,
) {
    let wi = w as i32;
    let hi = h as i32;
    let wu = w as usize;
    curr.par_chunks_mut(wu)
        .enumerate()
        .for_each(|(y, row)| {
            let base = y * wu;
            let yf = y as f32;
            for x in 0..wu {
                let i = base + x;
                let mut sx = map_a.sx[i];
                let mut sy = map_a.sy[i];
                if let Some((mb, alpha)) = map_b {
                    sx += (mb.sx[i] - sx) * alpha;
                    sy += (mb.sy[i] - sy) * alpha;
                }
                let xf = x as f32;
                let sxe = xf + (sx - xf) * speed;
                let sye = yf + (sy - yf) * speed;
                row[x] = bilinear1(prev, wi, hi, sxe, sye) * decay;
            }
        });
}

/// Bilinear sample of a single-channel buffer at fractional pixel coords.
/// Out-of-bounds taps read 0 (black), so inward-flowing maps pull darkness
/// in from the frame borders instead of smearing the edge pixels.
#[inline]
fn bilinear1(buf: &[f32], w: i32, h: i32, x: f32, y: f32) -> f32 {
    let x0 = x.floor();
    let y0 = y.floor();
    let dx = x - x0;
    let dy = y - y0;
    let xi = x0 as i32;
    let yi = y0 as i32;
    let tap = |tx: i32, ty: i32| -> f32 {
        if tx < 0 || ty < 0 || tx >= w || ty >= h {
            0.0
        } else {
            buf[(ty as usize) * (w as usize) + tx as usize]
        }
    };
    let v00 = tap(xi, yi);
    let v10 = tap(xi + 1, yi);
    let v01 = tap(xi, yi + 1);
    let v11 = tap(xi + 1, yi + 1);
    let top = v00 + (v10 - v00) * dx;
    let bot = v01 + (v11 - v01) * dx;
    top + (bot - top) * dy
}

// ---------------------------------------------------------------------------
// Waveform ink (Geiss-style scope, stamped into the feedback buffer)
// ---------------------------------------------------------------------------

/// Stamp the active scope shape into the intensity buffer, using PCM samples
/// in `[-1, 1]` to modulate the shape's amplitude. Max-blend (not additive)
/// so self-crossing figures don't blow out. `radius` is the stamp radius in
/// pixels (≈ 1.4 normally; widened briefly on downbeats).
#[allow(clippy::too_many_arguments)]
fn draw_waveform_ink(
    buf: &mut [f32],
    w: u32,
    h: u32,
    samples: &[f32],
    shape: WaveShape,
    ink: f32,
    radius: f32,
) {
    if samples.is_empty() || w < 4 || h < 4 { return; }

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
                let angle = t * TAU;
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
                let phase = t * TAU;
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
            stamp_line(buf, w, h, px, py, x, y, radius, ink);
        }
        prev_xy = Some((x, y));
    }

    // Closed shapes: connect last sample back to first.
    if let (Some(prev), Some(first)) = (prev_xy, close_loop_pt) {
        stamp_line(buf, w, h, prev.0, prev.1, first.0, first.1, radius, ink);
    }
}

/// Rasterise a thick line `(x0, y0) → (x1, y1)` by stamping filled discs of
/// `radius` pixels along the segment at half-pixel steps. Cheap and produces
/// a smooth ~2-3 px stroke without any AA library.
#[inline]
fn stamp_line(
    buf: &mut [f32],
    w: u32, h: u32,
    x0: f32, y0: f32, x1: f32, y1: f32,
    radius: f32, ink: f32,
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
        stamp_disc(buf, w, h, cx, cy, radius, ink);
    }
}

#[inline]
fn stamp_disc(
    buf: &mut [f32],
    w: u32, h: u32,
    cx: f32, cy: f32,
    radius: f32, ink: f32,
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
                let off = y as usize * w as usize + x as usize;
                buf[off] = buf[off].max(ink);
            }
        }
    }
}

fn palette_phase_at(t_seconds: f32, speed: f32) -> f32 {
    let palette_t = t_seconds * 0.125 * speed;
    (palette_t * TAU).sin() * 0.5 + 0.5
}

// ---------------------------------------------------------------------------
// Palette LUT — intensity [0,1] → RGB, built fresh each frame (cheap: 256
// entries) so palette phase drift animates the whole screen at once.
// ---------------------------------------------------------------------------

type Lut = [[u8; 3]; 256];

fn build_lut(palette: GranitePalette, palette_phase: f32) -> Lut {
    let mut lut = [[0u8; 3]; 256];
    for (i, entry) in lut.iter_mut().enumerate() {
        // 10% gamma lift — the Geiss 4.01 default — keeps dim trails visible.
        let t = (i as f32 / 255.0).powf(1.0 / 1.1);
        let tone = palette_modulate(palette, t, palette_phase);
        let (r, g, b) = palette_rgb(palette, tone, palette_phase);
        *entry = [
            (r * 255.0 + 0.5).clamp(0.0, 255.0) as u8,
            (g * 255.0 + 0.5).clamp(0.0, 255.0) as u8,
            (b * 255.0 + 0.5).clamp(0.0, 255.0) as u8,
        ];
    }
    lut
}

fn lerp_lut(a: &Lut, b: &Lut, alpha: f32) -> Lut {
    let mut out = [[0u8; 3]; 256];
    for i in 0..256 {
        for c in 0..3 {
            out[i][c] =
                (a[i][c] as f32 + (b[i][c] as f32 - a[i][c] as f32) * alpha + 0.5) as u8;
        }
    }
    out
}

/// Map raw intensity to a palette tone. Monotonic with `curve(0) = 0` so the
/// background stays true black and decayed trails actually reach it.
fn palette_modulate(palette: GranitePalette, intensity: f32, palette_phase: f32) -> f32 {
    let i = intensity.clamp(0.0, 1.0);
    match palette {
        GranitePalette::Granite => 0.85 * i * (0.7 + 0.3 * palette_phase),
        GranitePalette::Fire    => 0.90 * (i * i) * (0.6 + 0.4 * palette_phase),
        GranitePalette::Neon    => 0.95 * i.powf(0.7) * (0.8 + 0.2 * palette_phase),
    }
}

fn palette_rgb(palette: GranitePalette, tone: f32, palette_phase: f32) -> (f32, f32, f32) {
    let t = tone.clamp(0.0, 1.0);
    match palette {
        GranitePalette::Granite => {
            let stops = [
                (0.00, 0.00, 0.00),
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
                (0.00, 0.00, 0.00),
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

/// Map the intensity buffer to RGBA8 through the LUT, in parallel rows.
fn emit_rgba(curr: &[f32], dst: &mut [u8], lut: &Lut, w: usize) {
    dst.par_chunks_mut(w * 4)
        .zip(curr.par_chunks(w))
        .for_each(|(drow, srow)| {
            for (d, &v) in drow.chunks_exact_mut(4).zip(srow.iter()) {
                let idx = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as usize;
                let c = lut[idx.min(255)];
                d[0] = c[0];
                d[1] = c[1];
                d[2] = c[2];
                d[3] = 255;
            }
        });
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

    /// Synthetic PCM so the scope shape stamps ink (without ink the screen
    /// is structurally black — there is no procedural colour field anymore).
    fn test_wave() -> Vec<f32> {
        (0..256).map(|i| ((i as f32) * 0.13).sin() * 0.8).collect()
    }

    #[test]
    fn render_active_writes_nonzero() {
        let mut g = Granite::new(64, 36);
        let mut dst = buf_for(64, 36);
        g.render(&mut dst, 64, 36, 1.0, true, &test_wave(), &GraniteConfig::default());
        assert!(luminance_total(&dst) > 0);
    }

    #[test]
    fn ink_dissolves_over_following_frames() {
        // The defining Geiss behaviour: ink stamped once must persist and
        // decay over many frames (carried by the warp), not vanish next frame.
        let cfg = GraniteConfig {
            auto_switch: false,
            effect: GraniteEffect::Tunnel,
            feedback: 0.8,
            ..Default::default()
        };
        let mut g = Granite::new(64, 36);
        let mut dst = buf_for(64, 36);
        g.render(&mut dst, 64, 36, 0.0, true, &test_wave(), &cfg);
        let initial = luminance_total(&dst);
        assert!(initial > 0);

        // No new ink from here on; the warp must carry and fade the old ink.
        let mut lums = Vec::new();
        for f in 1..=15 {
            g.render(&mut dst, 64, 36, f as f32 / 30.0, true, &[], &cfg);
            lums.push(luminance_total(&dst));
        }
        assert!(
            lums[2] > initial / 20,
            "ink vanished immediately: frame3 = {} vs initial = {}",
            lums[2], initial
        );
        assert!(lums[14] > 0, "trails fully gone after 15 frames");
        assert!(lums[14] < lums[0], "trails must decay over time");
    }

    #[test]
    fn inactive_decays_to_black() {
        let mut g = Granite::new(32, 18);
        let mut dst = buf_for(32, 18);
        for f in 0..3 {
            g.render(&mut dst, 32, 18, f as f32 / 30.0, true, &test_wave(),
                     &GraniteConfig::default());
        }
        let initial = luminance_total(&dst);
        assert!(initial > 0);
        for _ in 0..90 {
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
                g.render(&mut dst, 48, 27, f as f32 / 30.0, true, &test_wave(), &cfg);
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
        g.render(&mut dst1, 64, 36, 1.0, true, &test_wave(), &GraniteConfig::default());

        let mut dst2 = buf_for(96, 54);
        g.render(&mut dst2, 96, 54, 1.0, true, &test_wave(), &GraniteConfig::default());
        assert_eq!(g.w, 96);
        assert_eq!(g.h, 54);

        let mut dst3 = buf_for(64, 36);
        g.render(&mut dst3, 64, 36, 1.0, true, &test_wave(), &GraniteConfig::default());
        assert_eq!(g.w, 64);
        assert_eq!(g.h, 36);
    }

    #[test]
    fn render_is_deterministic() {
        let cfg = GraniteConfig { auto_switch: false, ..Default::default() };
        let wave = test_wave();
        let mut g1 = Granite::new(32, 18);
        let mut g2 = Granite::new(32, 18);
        let mut a = buf_for(32, 18);
        let mut b = buf_for(32, 18);
        for f in 0..10 {
            g1.render(&mut a, 32, 18, f as f32 * 0.1, true, &wave, &cfg);
            g2.render(&mut b, 32, 18, f as f32 * 0.1, true, &wave, &cfg);
        }
        assert_eq!(a, b);
    }

    #[test]
    fn each_effect_renders_distinct() {
        // Pin auto_switch off; iterate explicit effects; expect each to produce
        // a non-black output and a different output from the others.
        use std::collections::HashSet;
        let mut hashes: HashSet<u64> = HashSet::new();
        let wave = test_wave();
        for effect in ALL_EFFECTS.iter().copied() {
            let mut g = Granite::new(48, 27);
            let mut dst = buf_for(48, 27);
            let cfg = GraniteConfig { auto_switch: false, effect, ..Default::default() };
            // Several frames so each map's flow visibly diverges.
            for f in 0..6 {
                g.render(&mut dst, 48, 27, f as f32 * 0.1, true, &wave, &cfg);
            }
            assert!(luminance_total(&dst) > 0, "effect {effect:?} produced black");
            // Cheap content hash.
            let h = dst.iter().fold(0u64, |acc, &b| acc.wrapping_mul(31).wrapping_add(b as u64));
            assert!(hashes.insert(h), "effect {effect:?} duplicates another");
        }
    }

    #[test]
    #[ignore] // perf probe: cargo test --release granite_kernel_speed -- --ignored --nocapture
    fn granite_kernel_speed() {
        // Fullscreen-sized internal buffer; must stay well inside the 33 ms
        // frame budget in release. Debug builds are 10-30× slower — that is
        // why the Xcode build phase always compiles the Rust core --release.
        let mut g = Granite::new(576, 360);
        let mut dst = buf_for(576, 360);
        let wave = test_wave();
        let cfg = GraniteConfig::default();
        for f in 0..10 {
            g.render(&mut dst, 576, 360, f as f32 / 30.0, true, &wave, &cfg);
        }
        let t0 = std::time::Instant::now();
        let frames = 300;
        for f in 0..frames {
            g.render(&mut dst, 576, 360, f as f32 / 30.0, true, &wave, &cfg);
        }
        let ms = t0.elapsed().as_secs_f64() * 1000.0 / frames as f64;
        println!("granite 576x360: {ms:.2} ms/frame");
        assert!(ms < 33.0, "kernel too slow: {ms:.2} ms/frame");
    }

    #[test]
    fn beat_detector_fires_on_kicks_only() {
        let mut d = BeatDetector::new();
        // Quiet high-frequency hiss: the low-pass strips it, so its energy
        // baseline is tiny and steady.
        let quiet: Vec<f32> = (0..512).map(|i| (i as f32 * 0.5).sin() * 0.05).collect();
        // Loud low-frequency kick: passes the low-pass nearly intact.
        let kick: Vec<f32> = (0..512).map(|i| (i as f32 * 0.02).sin() * 0.9).collect();

        let mut fired = 0;
        for _ in 0..40 {
            if d.process(&quiet, 1.5).is_beat { fired += 1; }
        }
        assert_eq!(fired, 0, "steady quiet signal must not register beats");
        assert!(d.process(&kick, 1.5).is_beat, "kick against quiet history must trigger");
        assert!(!d.process(&kick, 1.5).is_beat, "refractory window must suppress the next frame");
    }

    #[test]
    fn sustained_bass_does_not_machine_gun() {
        // Regression for the 200-BPM readout: a sustained loud bass note
        // must fire at most once (its attack), never at the refractory rate.
        // Flux triggering makes this structural — no rise, no beat.
        let mut d = BeatDetector::new();
        let quiet: Vec<f32> = (0..512).map(|i| (i as f32 * 0.5).sin() * 0.05).collect();
        let drone: Vec<f32> = (0..512).map(|i| (i as f32 * 0.02).sin() * 0.9).collect();
        for _ in 0..40 {
            d.process(&quiet, 1.5);
        }
        let mut fired = 0;
        for _ in 0..80 {
            if d.process(&drone, 1.5).is_beat { fired += 1; }
        }
        assert!(fired <= 1, "sustained bass fired {fired} times");
        assert_eq!(d.bpm(), 0.0, "no repeating interval may produce a BPM");
    }

    #[test]
    fn meter_detects_waltz_vs_common_time() {
        // Accented kick every `period` beats (strong-weak-weak[-weak]);
        // the autocorrelation must pick the right beats-per-measure and the
        // downbeat must land on the strong kicks once locked.
        fn run(period: usize) -> (u8, usize, usize) {
            let mut d = BeatDetector::new();
            let quiet: Vec<f32> = (0..512).map(|i| (i as f32 * 0.5).sin() * 0.05).collect();
            let strong: Vec<f32> = (0..512).map(|i| (i as f32 * 0.02).sin() * 0.9).collect();
            let weak: Vec<f32> = (0..512).map(|i| (i as f32 * 0.02).sin() * 0.45).collect();
            let mut beats = 0usize;
            let mut down_on_strong = 0usize;
            let mut down_total = 0usize;
            for f in 0..1500 {
                let (pcm, is_strong) = if f % 15 == 0 {
                    beats += 1;
                    if (beats - 1) % period == 0 { (&strong, true) } else { (&weak, false) }
                } else {
                    (&quiet, false)
                };
                let tick = d.process(pcm, 1.5);
                if tick.is_downbeat && f > 750 {
                    down_total += 1;
                    if is_strong { down_on_strong += 1; }
                }
            }
            (d.meter(), down_on_strong, down_total)
        }

        let (m4, hit4, tot4) = run(4);
        assert_eq!(m4, 4, "4-beat accent pattern must read as common time");
        assert!(tot4 > 0 && hit4 == tot4,
                "downbeats must land on accents: {hit4}/{tot4}");

        let (m3, hit3, tot3) = run(3);
        assert_eq!(m3, 3, "3-beat accent pattern must read as waltz");
        assert!(tot3 > 0 && hit3 == tot3,
                "downbeats must land on accents: {hit3}/{tot3}");
    }

    #[test]
    fn bpm_tracks_kick_interval() {
        let mut d = BeatDetector::new();
        let quiet: Vec<f32> = (0..512).map(|i| (i as f32 * 0.5).sin() * 0.05).collect();
        let kick: Vec<f32> = (0..512).map(|i| (i as f32 * 0.02).sin() * 0.9).collect();
        // 120 BPM at the renderer's 30 fps cadence = a kick every 15 frames.
        for f in 0..200 {
            let pcm = if f >= 45 && f % 15 == 0 { &kick } else { &quiet };
            d.process(pcm, 1.5);
        }
        let bpm = d.bpm();
        assert!(
            (bpm - 120.0).abs() < 12.0,
            "expected ~120 BPM from 15-frame kicks, got {bpm}"
        );
    }

    #[test]
    fn beats_keep_render_deterministic() {
        // Alternating quiet/kick PCM exercises the beat paths (glow, shift,
        // early switches) on both instances; outputs must stay identical.
        let cfg = GraniteConfig::default();
        let quiet: Vec<f32> = (0..512).map(|i| (i as f32 * 0.5).sin() * 0.05).collect();
        let kick: Vec<f32> = (0..512).map(|i| (i as f32 * 0.02).sin() * 0.9).collect();
        let mut g1 = Granite::new(32, 18);
        let mut g2 = Granite::new(32, 18);
        let mut a = buf_for(32, 18);
        let mut b = buf_for(32, 18);
        for f in 0..60 {
            let pcm = if f % 15 == 0 { &kick } else { &quiet };
            g1.render(&mut a, 32, 18, f as f32 / 30.0, true, pcm, &cfg);
            g2.render(&mut b, 32, 18, f as f32 / 30.0, true, pcm, &cfg);
        }
        assert_eq!(a, b);
    }

    #[test]
    fn random_switch_changes_effect() {
        let cfg = GraniteConfig { auto_switch: true, ..Default::default() };
        let wave = test_wave();
        let mut g = Granite::new(32, 18);
        let mut dst = buf_for(32, 18);
        g.render(&mut dst, 32, 18, 0.0, true, &wave, &cfg);
        let before = g.active_effect();
        let chosen = g.random_switch();
        assert_ne!(chosen, before);
        // Crossfade completes within CROSSFADE_FRAMES ticks.
        for f in 0..(CROSSFADE_FRAMES as usize + 2) {
            g.render(&mut dst, 32, 18, f as f32 / 30.0, true, &wave, &cfg);
        }
        assert_eq!(g.active_effect(), chosen);
    }

    #[test]
    fn auto_switch_changes_effect_within_max_interval() {
        let cfg = GraniteConfig { auto_switch: true, ..Default::default() };
        let wave = test_wave();
        let mut g = Granite::new(16, 9);
        let mut dst = buf_for(16, 9);
        let start = g.active_effect();
        // Run enough frames to guarantee at least one switch + crossfade
        // completion: max interval (720) + crossfade (30) + slack.
        for f in 0..800 {
            g.render(&mut dst, 16, 9, f as f32 * 0.033, true, &wave, &cfg);
        }
        assert_ne!(g.active_effect(), start, "scheduler never advanced");
    }
}
