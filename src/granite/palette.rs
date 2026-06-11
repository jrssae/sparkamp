//! Palettes and LUT presentation for the Granite visualizer.
//!
//! The renderer's feedback buffer is single-channel intensity; colour only
//! exists at display time, when a 256-entry LUT maps intensity to RGB.
//! Rebuilding the LUT every frame (cheap: 256 entries) is what lets the
//! palette phase drift animate the whole screen at once — the classic
//! 8-bit-palette trick the original Geiss leaned on.

use rand::Rng;
use rand::rngs::StdRng;
use rayon::prelude::*;
use std::f32::consts::TAU;

/// Which colour palette the visualizer is rendered through. All palettes
/// map intensity 0 to true black so decayed trails reach the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GranitePalette {
    Granite,
    Fire,
    Neon,
    /// Deep blue through cyan to foam white.
    Ocean,
    /// Black through purple to magenta-pink.
    Violet,
    /// Dusk blues into burnt orange and warm highlights.
    Sunset,
    /// Mono green phosphor — terminal nostalgia.
    Crt,
    /// Full-hue rainbow sweep; the phase slowly rotates the wheel.
    Spectrum,
}

impl Default for GranitePalette {
    fn default() -> Self { GranitePalette::Granite }
}

pub(super) const ALL_PALETTES: [GranitePalette; 8] = [
    GranitePalette::Granite,
    GranitePalette::Fire,
    GranitePalette::Neon,
    GranitePalette::Ocean,
    GranitePalette::Violet,
    GranitePalette::Sunset,
    GranitePalette::Crt,
    GranitePalette::Spectrum,
];

pub(super) fn random_other_palette(current: GranitePalette, rng: &mut StdRng) -> GranitePalette {
    loop {
        let idx = rng.gen_range(0..ALL_PALETTES.len());
        let candidate = ALL_PALETTES[idx];
        if candidate != current { return candidate; }
    }
}

/// Slow sine drift in [0, 1] that shifts palette colours over time.
pub(super) fn palette_phase_at(t_seconds: f32, speed: f32) -> f32 {
    let palette_t = t_seconds * 0.125 * speed;
    (palette_t * TAU).sin() * 0.5 + 0.5
}

pub(super) type Lut = [[u8; 3]; 256];

pub(super) fn build_lut(palette: GranitePalette, palette_phase: f32) -> Lut {
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

pub(super) fn lerp_lut(a: &Lut, b: &Lut, alpha: f32) -> Lut {
    let mut out = [[0u8; 3]; 256];
    for i in 0..256 {
        for c in 0..3 {
            out[i][c] =
                (a[i][c] as f32 + (b[i][c] as f32 - a[i][c] as f32) * alpha + 0.5) as u8;
        }
    }
    out
}

/// Map the intensity buffer to RGBA8 through the LUT, in parallel rows.
pub(super) fn emit_rgba(curr: &[f32], dst: &mut [u8], lut: &Lut, w: usize) {
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

/// Map raw intensity to a palette tone. Monotonic with `curve(0) = 0` so the
/// background stays true black and decayed trails actually reach it.
fn palette_modulate(palette: GranitePalette, intensity: f32, palette_phase: f32) -> f32 {
    let i = intensity.clamp(0.0, 1.0);
    match palette {
        GranitePalette::Granite  => 0.85 * i * (0.7 + 0.3 * palette_phase),
        GranitePalette::Fire     => 0.90 * (i * i) * (0.6 + 0.4 * palette_phase),
        GranitePalette::Neon     => 0.95 * i.powf(0.7) * (0.8 + 0.2 * palette_phase),
        GranitePalette::Ocean    => 0.85 * i.powf(1.1) * (0.7 + 0.3 * palette_phase),
        GranitePalette::Violet   => 0.90 * i.powf(1.3) * (0.7 + 0.3 * palette_phase),
        GranitePalette::Sunset   => 0.85 * i * (0.65 + 0.35 * palette_phase),
        GranitePalette::Crt      => 0.95 * i.powf(0.8) * (0.85 + 0.15 * palette_phase),
        GranitePalette::Spectrum => 0.90 * i.powf(0.9) * (0.8 + 0.2 * palette_phase),
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
        GranitePalette::Ocean => {
            let stops = [
                (0.00, 0.00, 0.00),
                (0.00, 0.15, 0.35),
                (0.00, 0.55, 0.75),
                (0.65 + 0.10 * palette_phase, 0.95, 1.00),
            ];
            gradient_lerp(&stops, t)
        }
        GranitePalette::Violet => {
            let stops = [
                (0.00, 0.00, 0.00),
                (0.25, 0.05, 0.45),
                (0.65, 0.15, 0.75),
                (1.00, 0.55 + 0.15 * palette_phase, 0.95),
            ];
            gradient_lerp(&stops, t)
        }
        GranitePalette::Sunset => {
            let stops = [
                (0.00, 0.00, 0.00),
                (0.20, 0.05, 0.30),
                (0.90, 0.35, 0.20),
                (1.00, 0.70 + 0.10 * palette_phase, 0.50),
            ];
            gradient_lerp(&stops, t)
        }
        GranitePalette::Crt => {
            let stops = [
                (0.00, 0.00, 0.00),
                (0.00, 0.25, 0.05),
                (0.10, 0.70, 0.15),
                (0.50 + 0.10 * palette_phase, 1.00, 0.60),
            ];
            gradient_lerp(&stops, t)
        }
        GranitePalette::Spectrum => {
            // Hue sweeps with tone; the phase slowly rotates the whole wheel.
            // Value tracks tone so 0 stays black like every other palette.
            let hue = (t * 0.83 + palette_phase * 0.17).fract();
            hsv_to_rgb(hue, 0.85, t.powf(0.8))
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

/// Standard HSV→RGB, h/s/v all in [0, 1].
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let h6 = (h.fract() + 1.0).fract() * 6.0;
    let i = h6.floor();
    let f = h6 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let u = v * (1.0 - s * (1.0 - f));
    match i as i32 % 6 {
        0 => (v, u, p),
        1 => (q, v, p),
        2 => (p, v, u),
        3 => (p, q, v),
        4 => (u, p, v),
        _ => (v, p, q),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_palettes_reach_black_at_zero_intensity() {
        // Every palette must map intensity 0 to (near) black, otherwise
        // decayed trails never disappear and Stop leaves a glowing pane.
        for palette in ALL_PALETTES {
            for phase in [0.0f32, 0.5, 1.0] {
                let lut = build_lut(palette, phase);
                let floor = lut[0][0] as u32 + lut[0][1] as u32 + lut[0][2] as u32;
                assert!(floor <= 9, "palette {palette:?} floor = {floor}");
            }
        }
    }
}
