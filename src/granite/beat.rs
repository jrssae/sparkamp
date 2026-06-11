//! Beat, tempo, and meter detection for the Granite visualizer.
//!
//! Beats trigger on **onset flux** — the frame-to-frame *rise* in band
//! energy — not on the energy level itself. A sustained bass note has zero
//! flux after its attack, so it cannot re-trigger; a level-triggered
//! detector machine-gunned at the refractory rate (exactly 200 BPM)
//! through quiet intros. Two bands (one-pole low-pass ≈ kick, remainder ≈
//! snare/mids) feed both the trigger and the per-beat accent salience.
//!
//! Meter: the salience sequence is autocorrelated at lags 3 and 4 —
//! accents repeat every measure, so the better-correlating lag is the
//! beats-per-measure estimate (vote-hysteresis smoothed). The downbeat
//! phase is the offset class with the highest mean salience.

/// Frames of flux history the trigger compares against — ~1.4 s at 30 fps,
/// long enough to track the song's current loudness, short enough to
/// follow section changes.
const BEAT_HISTORY: usize = 43;

/// Per-beat accent saliences kept for meter (time-signature) analysis —
/// 24 beats ≈ 6–8 bars, enough for the 3-vs-4 autocorrelation to settle.
const ACCENT_RING: usize = 24;

/// What one frame of audio produced: nothing, a beat, or a beat that lands
/// on the estimated downbeat (the "1" of the measure).
#[derive(Clone, Copy)]
pub(super) struct BeatTick {
    pub(super) is_beat: bool,
    pub(super) is_downbeat: bool,
}

const NO_TICK: BeatTick = BeatTick { is_beat: false, is_downbeat: false };

/// Online beat + meter detector fed the same PCM window the scope ink uses.
pub(super) struct BeatDetector {
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
    pub(super) fn new() -> Self {
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
    pub(super) fn process(&mut self, pcm: &[f32], sensitivity: f32) -> BeatTick {
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
    pub(super) fn meter(&self) -> u8 {
        if self.meter_known() { self.meter } else { 0 }
    }

    /// Estimated tempo from the median of recent inter-beat intervals.
    /// 0.0 until at least two intervals exist or after ~5 s without a beat.
    /// Assumes the 30 fps cadence both frontends drive the renderer at.
    /// Estimates above 180 fold down an octave — a visualizer wants the
    /// felt tempo, and >180 readings are nearly always double-time locks.
    pub(super) fn bpm(&self) -> f32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Quiet high-frequency hiss: the low-pass strips it, so its energy
    /// baseline is tiny and steady.
    fn quiet() -> Vec<f32> {
        (0..512).map(|i| (i as f32 * 0.5).sin() * 0.05).collect()
    }

    /// Loud low-frequency kick: passes the low-pass nearly intact.
    fn kick() -> Vec<f32> {
        (0..512).map(|i| (i as f32 * 0.02).sin() * 0.9).collect()
    }

    #[test]
    fn beat_detector_fires_on_kicks_only() {
        let mut d = BeatDetector::new();
        let quiet = quiet();
        let kick = kick();
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
        let quiet = quiet();
        let drone = kick();
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
    fn bpm_tracks_kick_interval() {
        let mut d = BeatDetector::new();
        let quiet = quiet();
        let kick = kick();
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
}
