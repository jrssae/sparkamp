//! The ReplayGain 1.0 loudness algorithm.
//!
//! Pure arithmetic over PCM, and deliberately compiled on every platform even
//! though only macOS currently routes to it. Linux measures with GStreamer's
//! `rganalysis`; this exists because the App Store build ships no GStreamer.
//!
//! Compiled everywhere so it cannot rot unnoticed: an implementation gated to
//! one platform is one that the other platform's CI never builds, and the
//! first anyone hears of a break is a user with wrong gains. The tests below
//! run wherever `cargo test` runs.
//!
//! ## What the algorithm is
//!
//! Three steps, from the 2001 specification:
//!
//! 1. **Equal-loudness filtering.** A 10th-order Yule-Walker IIR that applies
//!    the inverse of the ear's frequency response, then a 2nd-order
//!    Butterworth high-pass. Coefficients are per sample rate; see
//!    [`super::coefficients`].
//! 2. **Windowed RMS.** The filtered signal is measured in 50 ms windows, each
//!    reduced to one loudness figure in dB.
//! 3. **A percentile, not a mean.** Those figures go into a histogram at
//!    0.01 dB resolution and the **95th percentile** is taken — the level the
//!    track is at when it is loud, ignoring the quietest 95%… no: ignoring the
//!    *loudest 5%*, so that a few peaks do not decide the whole track's gain.
//!    The answer is `64.82 dB` minus that level, 64.82 being the calibrated
//!    reference the specification fixes.
//!
//! ## Why an album is not the average of its tracks
//!
//! Album gain accumulates every track's windows into **one** histogram and
//! takes the percentile once. Averaging the per-track gains gives a different
//! and wrong answer, because the percentile is not a linear operator — a
//! quiet track contributes few loud windows to the album, but averaging gives
//! it equal weight with a loud one. The difference is small enough to look
//! plausible, which is what makes it worth a test.

use super::coefficients::{BUTTER, RATES, YULE};

/// Calibration constant from the specification: the reference level, in dB,
/// that a gain is computed against.
const PINK_REF: f64 = 64.82;
/// Histogram resolution. 100 bins per dB is 0.01 dB, and is why two correct
/// implementations should agree to about that.
const STEPS_PER_DB: f64 = 100.0;
/// The histogram spans 0..120 dB.
const MAX_DB: f64 = 120.0;
const HISTOGRAM_BINS: usize = (MAX_DB * STEPS_PER_DB) as usize;
/// The loudest 5% of windows do not decide the gain.
const RMS_PERCENTILE: f64 = 0.95;
/// 50 ms.
const WINDOW_SECONDS: f64 = 0.050;
/// 16-bit full scale. The reference implementation measures integer PCM, and
/// its dB scale is anchored to that; a normalised sample must be scaled to it
/// before filtering or every measurement is ~90 dB too quiet.
const FULL_SCALE: f64 = 32768.0;
const YULE_ORDER: usize = 10;
const BUTTER_ORDER: usize = 2;

/// What one analysis produced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gain {
    /// Suggested gain in dB.
    pub gain_db: f64,
    /// Highest absolute sample seen, 1.0 being full scale. Can exceed 1.0 for
    /// material that already clips.
    pub peak: f64,
}

/// Accumulates loudness across one or more tracks.
///
/// One analyzer per album: feed a track, take its gain, feed the next. The
/// album histogram keeps accumulating underneath, so [`Self::album`] at the
/// end is the whole album measured as one stream — which is what it must be.
pub struct Analyzer {
    rate_index: usize,
    window_len: usize,

    /// Filter history. The reference keeps `order` samples of context either
    /// side of each buffer; this keeps the same history explicitly, which is
    /// the same thing said without pointer arithmetic.
    yule_in: [[f64; YULE_ORDER]; 2],
    yule_out: [[f64; YULE_ORDER]; 2],
    butter_in: [[f64; BUTTER_ORDER]; 2],
    butter_out: [[f64; BUTTER_ORDER]; 2],

    /// Sum of squares in the window being filled, per channel.
    window_sum: [f64; 2],
    window_filled: usize,

    track: Vec<u32>,
    album: Vec<u32>,
    track_peak: f64,
    album_peak: f64,
}

impl Analyzer {
    /// An analyzer for `sample_rate`, or `None` if ReplayGain defines no
    /// filter for it.
    ///
    /// `None` is not a failure to handle gracefully by guessing: an unlisted
    /// rate has no coefficients, and analysing it with the wrong ones would
    /// produce a confident wrong number. Resample first, or do not analyse.
    pub fn new(sample_rate: u32) -> Option<Self> {
        let rate_index = RATES.iter().position(|&r| r == sample_rate)?;
        Some(Analyzer {
            rate_index,
            window_len: (f64::from(sample_rate) * WINDOW_SECONDS).ceil() as usize,
            yule_in: [[0.0; YULE_ORDER]; 2],
            yule_out: [[0.0; YULE_ORDER]; 2],
            butter_in: [[0.0; BUTTER_ORDER]; 2],
            butter_out: [[0.0; BUTTER_ORDER]; 2],
            window_sum: [0.0; 2],
            window_filled: 0,
            track: vec![0; HISTOGRAM_BINS],
            album: vec![0; HISTOGRAM_BINS],
            track_peak: 0.0,
            album_peak: 0.0,
        })
    }

    /// Feed interleaved stereo samples, each in `-1.0..=1.0` for full scale.
    ///
    /// Mono is fed by passing the same value as both channels, which is what
    /// the reference does — the algorithm measures the mean of the two.
    ///
    /// Callers work in normalised floats because that is what every decoder
    /// hands over. The reference works in 16-bit integer scale, and its
    /// histogram covers 0..120 dB on that basis — normalised samples measure
    /// about 90 dB quieter and land every window in bin 0, which reads as a
    /// constant 64.82 dB gain for every input. [`FULL_SCALE`] is the
    /// conversion, applied here so no caller has to know about it.
    pub fn feed(&mut self, frames: &[[f64; 2]]) {
        for frame in frames {
            for ch in 0..2 {
                let x = frame[ch];
                self.track_peak = self.track_peak.max(x.abs());
                let stepped = self.filter_yule(ch, x * FULL_SCALE);
                let out = self.filter_butter(ch, stepped);
                self.window_sum[ch] += out * out;
            }
            self.window_filled += 1;
            if self.window_filled == self.window_len {
                self.close_window();
            }
        }
    }

    /// One sample through the Yule-Walker stage.
    ///
    /// `1e-10` is the reference's own guard against denormals, which on some
    /// hardware make this loop orders of magnitude slower. It is far below the
    /// quantisation of any real signal and does not move the result.
    fn filter_yule(&mut self, ch: usize, x: f64) -> f64 {
        let k = &YULE[self.rate_index];
        let mut y = 1e-10 + x * k[0];
        for i in 0..YULE_ORDER {
            y -= self.yule_out[ch][i] * k[2 * i + 1];
            y += self.yule_in[ch][i] * k[2 * i + 2];
        }
        shift_in(&mut self.yule_in[ch], x);
        shift_in(&mut self.yule_out[ch], y);
        y
    }

    fn filter_butter(&mut self, ch: usize, x: f64) -> f64 {
        let k = &BUTTER[self.rate_index];
        let mut y = x * k[0];
        for i in 0..BUTTER_ORDER {
            y -= self.butter_out[ch][i] * k[2 * i + 1];
            y += self.butter_in[ch][i] * k[2 * i + 2];
        }
        shift_in(&mut self.butter_in[ch], x);
        shift_in(&mut self.butter_out[ch], y);
        y
    }

    /// Reduce the finished window to one dB figure and file it.
    fn close_window(&mut self) {
        let mean = (self.window_sum[0] + self.window_sum[1]) / self.window_filled as f64 * 0.5;
        // 1e-37 keeps digital silence off the edge of log10 rather than at
        // negative infinity; it lands in bin 0, which is where silence belongs.
        let db = STEPS_PER_DB * 10.0 * (mean + 1e-37).log10();
        // Truncated, not rounded — the reference casts to int, and rounding
        // would shift every measurement half a bin against it.
        let bin = (db as i64).clamp(0, HISTOGRAM_BINS as i64 - 1) as usize;
        self.track[bin] += 1;
        self.album[bin] += 1;
        self.window_sum = [0.0; 2];
        self.window_filled = 0;
    }

    /// The gain and peak for the track just fed, and reset for the next one.
    ///
    /// The album accumulation is deliberately *not* reset: that is the whole
    /// difference between an album gain and an average of track gains.
    pub fn finish_track(&mut self) -> Option<Gain> {
        let gain_db = percentile_gain(&self.track)?;
        let peak = self.track_peak;
        self.track.fill(0);
        self.album_peak = self.album_peak.max(peak);
        self.track_peak = 0.0;
        // A part-filled window is discarded, as the reference discards it:
        // a window shorter than 50 ms is not a measurement of the same thing.
        self.window_sum = [0.0; 2];
        self.window_filled = 0;
        Some(Gain { gain_db, peak })
    }

    /// The gain and peak for every track fed so far, measured as one stream.
    pub fn album(&self) -> Option<Gain> {
        Some(Gain {
            gain_db: percentile_gain(&self.album)?,
            peak: self.album_peak,
        })
    }
}

/// Push `value` into a history buffer, oldest out.
fn shift_in(history: &mut [f64], value: f64) {
    for i in (1..history.len()).rev() {
        history[i] = history[i - 1];
    }
    history[0] = value;
}

/// The 95th-percentile level of a histogram, as a gain against the reference.
///
/// Walks down from the loudest bin discarding 5% of the windows, and reports
/// where it stopped. `None` when nothing was measured — a track shorter than
/// one 50 ms window has no loudness to report, and inventing 0.0 dB for it
/// would be a confident answer to a question that was never asked.
fn percentile_gain(histogram: &[u32]) -> Option<f64> {
    let total: u64 = histogram.iter().map(|&n| u64::from(n)).sum();
    if total == 0 {
        return None;
    }
    let mut remaining = (total as f64 * (1.0 - RMS_PERCENTILE)).ceil() as i64;
    for bin in (0..histogram.len()).rev() {
        remaining -= i64::from(histogram[bin]);
        if remaining <= 0 {
            return Some(PINK_REF - bin as f64 / STEPS_PER_DB);
        }
    }
    Some(PINK_REF - 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read a 16-bit PCM WAV into frames. Test-only, and deliberately minimal:
    /// the fixtures are written by `afconvert`/`gst-launch` and are canonical.
    pub(crate) fn read_wav(path: &std::path::Path) -> (u32, Vec<[f64; 2]>) {
        let b = std::fs::read(path).expect("read wav");
        let (mut rate, mut channels, mut bits, mut at, mut data) = (0u32, 0u16, 0u16, 12usize, None);
        while at + 8 <= b.len() {
            let id = &b[at..at + 4];
            let len = u32::from_le_bytes([b[at + 4], b[at + 5], b[at + 6], b[at + 7]]) as usize;
            let body = at + 8;
            if id == b"fmt " {
                channels = u16::from_le_bytes([b[body + 2], b[body + 3]]);
                rate = u32::from_le_bytes([b[body + 4], b[body + 5], b[body + 6], b[body + 7]]);
                bits = u16::from_le_bytes([b[body + 14], b[body + 15]]);
            } else if id == b"data" {
                data = Some(&b[body..(body + len).min(b.len())]);
            }
            at = body + len + (len & 1);
        }
        assert_eq!(bits, 16, "fixture must be 16-bit");
        let data = data.expect("no data chunk");
        let step = channels as usize * 2;
        let frames = data
            .chunks_exact(step)
            .map(|f| {
                let l = f64::from(i16::from_le_bytes([f[0], f[1]])) / 32768.0;
                let r = if channels > 1 {
                    f64::from(i16::from_le_bytes([f[2], f[3]])) / 32768.0
                } else {
                    l
                };
                [l, r]
            })
            .collect();
        (rate, frames)
    }

    /// Against `rganalysis`, on lossless audio, which is the only comparison
    /// that measures the algorithm rather than the decoder.
    /// `SPARKAMP_FORMAT_DIR=<dir> cargo test --lib matches_rganalysis -- --ignored --nocapture`
    ///
    /// Lossless deliberately: MP3 decoders are not bit-exact with each other
    /// (measured here: mean 4.7 LSB, max 2809 between CoreAudio and
    /// GStreamer), so comparing on a lossy file measures decoder disagreement
    /// as much as anything this file does.
    ///
    /// No reference-level correction is applied, and an earlier draft that
    /// applied one was wrong by exactly 6.0000 dB — which is how it was
    /// caught. `rganalysis` reports against an 89 dB reference level, and the
    /// specification's `PINK_REF` of 64.82 is already anchored to the same
    /// place. The two are directly comparable.
    #[test]
    #[ignore]
    fn matches_rganalysis() {
        let Some(dir) = std::env::var_os("SPARKAMP_FORMAT_DIR") else {
            println!("set SPARKAMP_FORMAT_DIR to a directory of samples");
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let wav = dir.join("t.wav");
        if !wav.exists() {
            println!("no t.wav — skipping");
            return;
        }
        // The reference, from the element Linux ships.
        let out = std::process::Command::new("gst-launch-1.0")
            .args([
                "-t",
                "filesrc",
                &format!("location={}", wav.display()),
                "!",
                "decodebin",
                "!",
                "audioconvert",
                "!",
                "rganalysis",
                "!",
                "fakesink",
            ])
            .output();
        let Ok(out) = out else {
            println!("gst-launch-1.0 not available — skipping");
            return;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let field = |name: &str| -> Option<f64> {
            text.lines()
                .find(|l| l.contains(name))?
                .rsplit(':')
                .next()?
                .trim()
                .parse()
                .ok()
        };
        let (Some(ref_gain), Some(ref_peak), Some(ref_level)) = (
            field("replaygain track gain"),
            field("replaygain track peak"),
            field("replaygain reference level"),
        ) else {
            println!("rganalysis produced no values — skipping\n{text}");
            return;
        };

        let (rate, frames) = read_wav(&wav);
        let mut a = Analyzer::new(rate).expect("fixture rate must be supported");
        a.feed(&frames);
        let mine = a.finish_track().expect("fixture must be long enough");

        println!(
            "rganalysis: {ref_gain:+.4} dB (ref {ref_level}), peak {ref_peak:.6}\n\
             this:       {:+.4} dB, peak {:.6}\n\
             delta:      {:+.4} dB, peak {:+.6}",
            mine.gain_db,
            mine.peak,
            mine.gain_db - ref_gain,
            mine.peak - ref_peak
        );
        assert!(
            (mine.gain_db - ref_gain).abs() < 0.01,
            "gain differs from rganalysis by {:.4} dB",
            mine.gain_db - ref_gain
        );
        assert!(
            (mine.peak - ref_peak).abs() < 0.001,
            "peak differs from rganalysis by {:.6}",
            mine.peak - ref_peak
        );
    }

    /// A rate ReplayGain defines no filter for must be refused, not guessed at.
    #[test]
    fn an_undefined_sample_rate_is_refused() {
        assert!(Analyzer::new(44100).is_some());
        assert!(Analyzer::new(48000).is_some());
        assert!(Analyzer::new(37000).is_none());
        assert!(Analyzer::new(0).is_none());
    }

    /// Nothing measured is not zero gain. A track too short to fill one 50 ms
    /// window has no loudness, and answering 0.0 dB would be a confident
    /// answer to a question never asked.
    #[test]
    fn too_short_to_measure_reports_nothing() {
        let mut a = Analyzer::new(44100).unwrap();
        a.feed(&[[0.5, 0.5]; 100]);
        assert!(a.finish_track().is_none());
        assert!(a.album().is_none());
    }

    /// Digital silence lands in the quietest bin, and the gain is the maximum
    /// the scale allows rather than an infinity.
    #[test]
    fn silence_is_measurable_and_finite() {
        let mut a = Analyzer::new(44100).unwrap();
        a.feed(&[[0.0, 0.0]; 44100]);
        let g = a.finish_track().expect("a second of silence fills windows");
        assert!(g.gain_db.is_finite(), "gain was {}", g.gain_db);
        assert_eq!(g.peak, 0.0);
    }

    /// Louder in, less gain out — and by the amount the change in level was.
    /// Halving amplitude is -6.02 dB, so the suggested gain must rise by that.
    #[test]
    fn halving_the_level_raises_the_gain_by_six_db() {
        let tone = |amp: f64| -> f64 {
            let mut a = Analyzer::new(44100).unwrap();
            let frames: Vec<[f64; 2]> = (0..44100)
                .map(|i| {
                    let v = amp
                        * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 44100.0).sin();
                    [v, v]
                })
                .collect();
            a.feed(&frames);
            a.finish_track().unwrap().gain_db
        };
        let loud = tone(0.5);
        let quiet = tone(0.25);
        assert!(
            (quiet - loud - 6.02).abs() < 0.05,
            "expected about 6.02 dB more gain for half the amplitude, got {:.3}",
            quiet - loud
        );
    }

    /// Album gain accumulates every window into one histogram; it is not the
    /// mean of the track gains. With one loud track and one quiet one the two
    /// answers differ, and this pins which is being computed.
    #[test]
    fn album_gain_is_not_the_average_of_track_gains() {
        let tone = |amp: f64, n: usize| -> Vec<[f64; 2]> {
            (0..n)
                .map(|i| {
                    let v = amp
                        * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 44100.0).sin();
                    [v, v]
                })
                .collect()
        };
        let mut a = Analyzer::new(44100).unwrap();
        a.feed(&tone(0.5, 44100));
        let loud = a.finish_track().unwrap().gain_db;
        a.feed(&tone(0.05, 44100));
        let quiet = a.finish_track().unwrap().gain_db;
        let album = a.album().unwrap().gain_db;
        let mean = (loud + quiet) / 2.0;
        println!("loud {loud:.2} quiet {quiet:.2} album {album:.2} mean {mean:.2}");
        assert!(
            (album - mean).abs() > 0.5,
            "album {album:.2} is suspiciously close to the mean {mean:.2} — \
             averaging track gains would look like this"
        );
        // The quiet track's windows are a minority of the album's, so the
        // album sits nearer the loud track than halfway.
        assert!(
            (album - loud).abs() < (album - quiet).abs(),
            "album {album:.2} should sit nearer the loud track {loud:.2} than the quiet {quiet:.2}"
        );
    }

    /// Peak is the largest absolute sample, and survives being asked for after
    /// the track is finished.
    #[test]
    fn peak_is_the_largest_absolute_sample() {
        let mut a = Analyzer::new(44100).unwrap();
        let mut frames = vec![[0.1, 0.1]; 44100];
        frames[500] = [-0.75, 0.2];
        a.feed(&frames);
        let g = a.finish_track().unwrap();
        assert!((g.peak - 0.75).abs() < 1e-9, "peak was {}", g.peak);
        assert!((a.album().unwrap().peak - 0.75).abs() < 1e-9);
    }
}
