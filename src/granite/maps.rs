//! Warp maps — the displacement fields that give Granite its motion.
//!
//! For every destination pixel a map stores the fractional source position
//! to sample the previous frame at. This is the modern equivalent of
//! Geiss's 6-bytes-per-pixel offset+weight tables: storing coordinates
//! instead lets the warp kernel derive bilinear weights inline and lets two
//! maps be lerped during a crossfade (blending displacement fields is
//! itself a valid field). A static map applied every frame produces
//! continuous motion — Geiss's core trick.

use rand::Rng;
use rand::rngs::StdRng;
use rayon::prelude::*;
use std::f32::consts::{PI, SQRT_2, TAU};

const PHI: f32 = 1.618_034;

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

pub(super) const ALL_EFFECTS: [GraniteEffect; 12] = [
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

pub(super) fn random_other_effect(current: GraniteEffect, rng: &mut StdRng) -> GraniteEffect {
    loop {
        let idx = rng.gen_range(0..ALL_EFFECTS.len());
        let candidate = ALL_EFFECTS[idx];
        if candidate != current { return candidate; }
    }
}

/// Per-pixel fractional source coordinates.
pub(super) struct WarpMap {
    sx: Vec<f32>,
    sy: Vec<f32>,
}

impl WarpMap {
    pub(super) fn empty() -> Self {
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
/// every frame.
pub(super) fn generate_warp_map(
    effect: GraniteEffect,
    w: u32,
    h: u32,
    rng: &mut StdRng,
) -> WarpMap {
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

/// `curr[p] = bilinear(prev, map[p]) * decay` for every pixel, in parallel.
///
/// `speed` scales the stored displacement away from identity at sample time,
/// so the speed slider acts live without regenerating maps. During a
/// crossfade `map_b = Some((incoming_map, alpha))` and the two displacement
/// fields are lerped per pixel — blending fields morphs the flow smoothly.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_warp(
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
