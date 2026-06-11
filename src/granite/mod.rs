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
//! Module layout (kept deliberately small per file):
//! - [`maps`]: warp-map families, generation, and the warp kernel
//! - [`beat`]: beat / tempo (BPM) / meter (3-vs-4) detection
//! - [`palette`]: palettes, LUT build/lerp, RGBA emission
//! - [`ink`]: waveform scope shapes stamped into the feedback buffer
//! - this file: configuration, renderer state, scheduler, the render loop
//!
//! Both frontends (GTK + macOS) call [`Granite::render`] each frame to fill
//! a caller-owned RGBA8 buffer at the granite-internal resolution; the
//! windowing system's GPU compositor handles the upscale to display size.

mod beat;
mod ink;
mod maps;
mod palette;

pub use maps::GraniteEffect;
pub use palette::GranitePalette;

use beat::BeatDetector;
use ink::{draw_waveform_ink, random_other_shape, WaveShape, INK_BEAT, INK_FLAT, INK_QUIET};
use maps::{apply_warp, generate_warp_map, random_other_effect, WarpMap};
use palette::{build_lut, emit_rgba, lerp_lut, palette_phase_at, random_other_palette};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// Internal render height. Frontends pass this to [`Granite::render`] together
/// with a width derived from the viewport's aspect ratio. Single-line change
/// to bump or shrink — no schema or FFI references this constant elsewhere.
pub const GRANITE_INTERNAL_HEIGHT: u32 = 360;

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
    /// While `frame` is below this, the scheduler and beat reactions leave
    /// the palette alone — a user just picked one in Settings.
    palette_hold_until: u64,
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
            palette_hold_until: 0,
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

    /// Apply a user-picked palette immediately (Settings). With
    /// `auto_switch` on, the scheduler normally owns the palette — without
    /// this, picking one in Settings does nothing visible. The choice is
    /// held for ~20 s (mirroring [`Self::set_effect`]) before auto palette
    /// rolling resumes.
    pub fn set_palette(&mut self, palette: GranitePalette) {
        self.current_palette = palette;
        self.next_palette = None;
        self.palette_fade_remaining = 0;
        self.palette_hold_until = self.frame + 600;
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
                && self.frame >= self.palette_hold_until
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
            // Unless the user just picked a palette in Settings: honour it.
            self.next_palette = if self.frame >= self.palette_hold_until {
                Some(random_other_palette(self.current_palette, &mut self.rng))
            } else {
                None
            };
            // Snap the scope shape immediately too. We don't crossfade the
            // shape — the waveform ink dissolves into the plasma each frame,
            // so changing the shape mid-warp just looks like the next few
            // frames trace a new figure.
            self.current_shape = random_other_shape(self.current_shape, &mut self.rng);
            self.crossfade_remaining = CROSSFADE_FRAMES;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — render-pipeline level. Detector and palette unit tests live in
// their own modules.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::maps::ALL_EFFECTS;
    use super::palette::ALL_PALETTES;
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
        for palette in ALL_PALETTES {
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
    fn beats_keep_render_deterministic() {
        // Alternating quiet/kick PCM exercises the beat paths (glow, colour
        // changes, early switches) on both instances; outputs must stay
        // identical.
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
    fn set_palette_holds_through_beats_and_switches() {
        // A user-picked palette must survive beat colour changes and the
        // scheduler's switch-time palette roll for the ~20 s hold window.
        let cfg = GraniteConfig::default(); // auto_switch on
        let quiet: Vec<f32> = (0..512).map(|i| (i as f32 * 0.5).sin() * 0.05).collect();
        let kick: Vec<f32> = (0..512).map(|i| (i as f32 * 0.02).sin() * 0.9).collect();
        let mut g = Granite::new(32, 18);
        let mut dst = buf_for(32, 18);
        g.render(&mut dst, 32, 18, 0.0, true, &quiet, &cfg);
        g.set_palette(GranitePalette::Crt);
        // 500 frames < 600 hold; includes kicks (beat colour pressure) and
        // at least one scheduled effect switch (interval min 360).
        for f in 0..500 {
            let pcm = if f % 15 == 0 { &kick } else { &quiet };
            g.render(&mut dst, 32, 18, f as f32 / 30.0, true, pcm, &cfg);
        }
        assert_eq!(g.current_palette, GranitePalette::Crt,
                   "palette must hold while the user's choice is fresh");
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
