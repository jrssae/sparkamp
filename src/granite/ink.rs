//! Waveform "ink" — the scope shapes stamped into the feedback buffer.
//!
//! The shape drawn this frame is carried, smeared and dissolved by the warp
//! on every following frame; that interplay (fresh ink + iterative warp) is
//! the entire Geiss flow. Drawing happens directly in the single-channel
//! intensity buffer with max-blend so self-crossing figures don't blow out.

use rand::Rng;
use rand::rngs::StdRng;
use std::f32::consts::TAU;

/// Waveform ink levels. With beat-linked brightness on (the default), ink
/// sits at the quiet level and snaps to full on detected beats — Geiss
/// v4.00's "wave brightness sharply linked to the beat". With it off, ink
/// is a constant mid-bright stroke (Geiss made this optional in 4.24b).
pub(super) const INK_QUIET: f32 = 0.80;
pub(super) const INK_BEAT:  f32 = 1.00;
pub(super) const INK_FLAT:  f32 = 0.92;

/// Shape of the PCM-driven waveform line that's drawn into each frame and
/// then dissolved by the warp on subsequent frames (Geiss "scope" flow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WaveShape {
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

pub(super) fn random_other_shape(current: WaveShape, rng: &mut StdRng) -> WaveShape {
    loop {
        let idx = rng.gen_range(0..ALL_SHAPES.len());
        let candidate = ALL_SHAPES[idx];
        if candidate != current { return candidate; }
    }
}

/// Stamp the active scope shape into the intensity buffer, using PCM samples
/// in `[-1, 1]` to modulate the shape's amplitude. `radius` is the stamp
/// radius in pixels (≈ 1.4 normally; widened briefly on downbeats).
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_waveform_ink(
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
#[allow(clippy::too_many_arguments)]
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
                // Max-blend (not additive) so self-crossing figures don't
                // blow out.
                buf[off] = buf[off].max(ink);
            }
        }
    }
}
