//! The audio-backend seam.
//!
//! Everything `Player` cannot do without knowing which audio stack it is
//! talking to, and the vocabulary that crosses that line. `Player` itself stays
//! one concrete struct: roughly half its methods (the visualizer, the fadeout
//! ramp, stop-after-current, the output composition rule) never touch an audio
//! stack and must not be written twice.
//!
//! Design of record: `docs/superpowers/plans/2026-08-31-audio-backend-seam-design.md`.
//! Measurements it rests on: `docs/superpowers/plans/2026-08-31-macos-audio-backend-spike.md`.
//!
//! This module is vocabulary only. The adapters live beside it, in
//! [`crate::engine::gst`] and (for tests) `crate::engine::null`.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;

use crate::config::EQ_BAND_DB_LIMIT;
use crate::engine::{BusEvent, PlayerState};
use crate::model::{SpectrumData, WaveformBuffer};

/// A linear output multiplier, never dB.
///
/// The two units are both `f64` and mixing them is silent and audible. That is
/// exactly the class of mistake behind the measured -3.01 dB discrepancy on
/// AVFoundation's mixer, so the linear one gets a name.
///
/// Non-negative by construction; there is no way to build one from dB.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Amplitude(f64);

impl Amplitude {
    pub const UNITY: Amplitude = Amplitude(1.0);
    /// No production caller: a fadeout ramps toward silence but stops the
    /// player rather than pushing zero, and nothing else mutes. Kept as the
    /// name for "no output" that a backend contract can be stated in, and used
    /// by this module's own tests.
    #[allow(dead_code)]
    pub const SILENT: Amplitude = Amplitude(0.0);

    /// Clamps negatives to zero. The only constructor, so no adapter can be
    /// handed a negative multiplier.
    pub fn linear(v: f64) -> Self {
        Amplitude(if v.is_nan() { 0.0 } else { v.max(0.0) })
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

/// Ten equalizer band gains in dB, low frequency first, each already clamped to
/// +/- [`EQ_BAND_DB_LIMIT`].
///
/// The clamp lives in the constructor rather than in each caller, so the
/// invariant `Player` maintains by hand in two places today (`set_eq_band` and
/// `apply_eq_bands`) cannot be skipped by a third.
///
/// Band centres are the ones `equalizer-10bands` declares and the spike
/// measured: 29, 59, 119, 237, 474, 947, 1889, 3770, 7523, 15011 Hz. An adapter
/// may realise band 0 and band 9 as shelves, which is what GStreamer does and
/// what halved AVFoundation's error in the spike. It may not move a centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqCurve([f64; 10]);

impl EqCurve {
    pub const FLAT: EqCurve = EqCurve([0.0; 10]);

    /// Clamp each entry into range. Entries past the tenth are ignored; bands a
    /// short slice does not cover keep `self`'s value. Both are today's
    /// `apply_eq_bands` behaviour, moved into the type that owns the invariant.
    pub fn with_bands(self, bands: &[f64]) -> Self {
        let mut out = self.0;
        for (i, &gain) in bands.iter().take(10).enumerate() {
            out[i] = gain.clamp(-EQ_BAND_DB_LIMIT, EQ_BAND_DB_LIMIT);
        }
        EqCurve(out)
    }

    /// Out-of-range `band` is a no-op, matching today's `set_eq_band`.
    pub fn with_band(self, band: usize, gain_db: f64) -> Self {
        let mut out = self.0;
        if band < 10 {
            out[band] = gain_db.clamp(-EQ_BAND_DB_LIMIT, EQ_BAND_DB_LIMIT);
        }
        EqCurve(out)
    }

    /// `0.0` for an out-of-range band, matching today's `get_eq_band`.
    pub fn band(&self, band: usize) -> f64 {
        self.0.get(band).copied().unwrap_or(0.0)
    }

    pub fn as_db(&self) -> &[f64; 10] {
        &self.0
    }
}

impl Default for EqCurve {
    fn default() -> Self {
        Self::FLAT
    }
}

/// What the backend should be doing to the signal level, as a complete picture
/// rather than a delta.
///
/// `Player` resolves policy (which dB number applies to this track, whether the
/// user asked for album or track mode) and hands down the answer. The adapter
/// decides mechanism: whether that answer needs a new element in the graph, a
/// property write, or nothing at all.
///
/// Full state on every call, so a repeat is a no-op and a half-finished
/// previous call is corrected rather than compounded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Normalization {
    /// `false` means no normalization stage in the audio path at all.
    pub enabled: bool,
    /// Attenuate rather than clip when normalization pushes a loud track past
    /// full scale.
    pub clip_protection: bool,
    /// dB to apply when the stream carries no ReplayGain tags of its own.
    /// Already resolved by `Player` from the library's measured gain for this
    /// track, falling back to the user's configured value.
    pub fallback_db: f64,
    /// Prefer album gain over track gain when the stream carries both.
    pub album_mode: bool,
}

/// Whether a [`AudioBackend::set_normalization`] call changed what the listener
/// hears now, or only from the next [`AudioBackend::load`].
///
/// This exists because "can I reshape the audio path while it is running" is an
/// adapter fact, not a `Player` fact. GStreamer can only relink at
/// `State::Null`, so it answers `AtNextLoad` for a change that adds or removes
/// an element mid-track. An adapter whose normalization is a live gain node
/// answers `Now`, and the reload dance in
/// `src/ffi/settings.rs::ffi_apply_replaygain` stops firing there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Applied {
    Now,
    AtNextLoad,
}

/// What to play, parsed.
///
/// `Player::load` takes a `&str`, which is public API and does not change, and
/// parses it here at the one boundary where a string arrives from outside.
/// Below this point the device travels with the track it belongs to, which is
/// what lets the GStreamer adapter eventually drop the
/// `Arc<Mutex<Option<String>>>` that carries it out-of-band today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaSource {
    /// A URI the backend can open directly. The overwhelmingly common case,
    /// including a CD track reached as a file on a mounted disc, which is how
    /// macOS presents audio CDs.
    Uri(String),
    /// One track of a CD in a named drive. Separate from `Uri` because no URI
    /// scheme carries the device: `cdda://3?device=/dev/sr0` is Sparkamp's own
    /// invention, and stripping the suffix without somewhere to put it is what
    /// forced the out-of-band stash.
    ///
    /// An adapter that cannot open a raw CD-audio stream returns an error from
    /// `load`, which `Player` surfaces exactly as it surfaces a missing file.
    CdTrack {
        track: String,
        device: Option<String>,
    },
}

impl MediaSource {
    /// The one place a playback URI is parsed. Anything that is not a `cdda://`
    /// pseudo-URI is a `Uri`, verbatim.
    ///
    /// Delegates the pseudo-URI shape to [`crate::disc::parse_cdda_uri`] so the
    /// two do not drift.
    pub fn parse(uri: &str) -> Self {
        match crate::disc::parse_cdda_uri(uri) {
            Some((track, device)) => MediaSource::CdTrack {
                track: track.to_string(),
                device: device.map(str::to_string),
            },
            None => MediaSource::Uri(uri.to_string()),
        }
    }
}

/// Position and duration read from one instant.
///
/// Two `Option`s in one struct rather than two queries, because they must
/// describe the same moment. Today `sparkamp_tick` calls `duration()` three
/// times and `position()` twice per tick and quietly assumes they agree; under
/// AVFoundation both derive from the same `AVAudioTime.sampleTime` snapshot, so
/// making them one value makes the assumption true instead of lucky.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Timeline {
    /// `None` when nothing is loaded or the clock has not started.
    pub position: Option<Duration>,
    /// `None` until the format reports one. Never invented.
    pub duration: Option<Duration>,
}

/// What this backend can actually do. Fixed for the life of the backend:
/// `Player` reads it once and answers `has_eq`, `has_spectrum` and
/// `rg_available` from the copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// `false` means [`AudioBackend::set_eq`] is a silent no-op, per the house
    /// rule for missing plugins.
    pub eq: bool,
    /// `false` means no magnitudes will ever reach [`AnalysisTap`], so the bars
    /// visualizer stays flat.
    pub spectrum: bool,
    /// `false` means [`AudioBackend::set_normalization`] is a silent no-op.
    pub normalization: bool,
}

// ---------------------------------------------------------------------------
// Analysis capture: one root, a read half and a write half
// ---------------------------------------------------------------------------

/// The reader half of the visualizer buffers. `Player` owns exactly one.
///
/// The buffers are the existing `crate::model` types, unchanged. They already
/// do the two jobs that matter: `SpectrumData::update` resizes its smoothing
/// array to whatever band count it is handed, and `WaveformBuffer::push_samples`
/// evicts from a ring. That is why the AVFoundation tap returning 4410-frame
/// buffers when asked for 1024 costs this design nothing. No consumer, at any
/// level, states a capture size.
pub struct Analysis {
    spectrum: Arc<RwLock<SpectrumData>>,
    waveform: Arc<RwLock<WaveformBuffer>>,
}

impl Analysis {
    pub fn new(spectrum_bands: usize, waveform_capacity: usize) -> Self {
        Analysis {
            spectrum: Arc::new(RwLock::new(SpectrumData::new(spectrum_bands))),
            waveform: Arc::new(RwLock::new(WaveformBuffer::new(waveform_capacity))),
        }
    }

    /// The write handle to hand a backend. Cheap to clone; a clone is what
    /// travels into the GStreamer pad probe or the AVFoundation tap block.
    pub fn tap(&self) -> AnalysisTap {
        AnalysisTap {
            spectrum: Arc::clone(&self.spectrum),
            waveform: Arc::clone(&self.waveform),
        }
    }

    /// Smoothed magnitudes, 0..1, however many bands the backend last pushed.
    /// `Player` maps these onto display bars; nothing below the seam knows how
    /// many bars there are.
    pub fn magnitudes(&self) -> Vec<f64> {
        self.spectrum
            .read()
            .map(|s| s.smoothed().to_vec())
            .unwrap_or_default()
    }

    pub fn has_magnitudes(&self) -> bool {
        self.spectrum
            .read()
            .map(|s| s.has_received_data())
            .unwrap_or(false)
    }

    /// `count` PCM samples in `[-1.0, 1.0]`, resampled and smoothed from the
    /// ring. Zeros when too little audio has arrived.
    pub fn waveform(&self, count: usize) -> Vec<f64> {
        self.waveform
            .read()
            .map(|w| w.get_samples(count))
            .unwrap_or_else(|_| vec![0.0; count])
    }

    /// Collapse both visualizers to their resting state.
    pub fn clear(&self) {
        if let Ok(mut s) = self.spectrum.write() {
            s.clear();
        }
        self.reset_waveform();
    }

    /// Blank the scope without dropping the bars' history, which is what a
    /// track change wants.
    pub fn reset_waveform(&self) {
        if let Ok(mut w) = self.waveform.write() {
            w.reset();
        }
    }
}

/// The writer half, held by the backend and by whatever audio thread it hands a
/// clone to.
///
/// Write-only by construction: an adapter cannot read back what it pushed,
/// which is what keeps display policy from drifting into adapters.
///
/// ## Invariants
/// - Both methods accept any length. An adapter must not buffer up to a
///   "correct" size before pushing; there is no correct size.
/// - The two buffers are locked independently and no invariant spans them. A
///   reader may see a PCM frame newer than the magnitude frame, and nothing
///   depends on them agreeing.
/// - Both may be called from any thread, including a real-time audio callback.
#[derive(Clone)]
pub struct AnalysisTap {
    spectrum: Arc<RwLock<SpectrumData>>,
    waveform: Arc<RwLock<WaveformBuffer>>,
}

impl AnalysisTap {
    /// Push captured mono PCM in `[-1.0, 1.0]`.
    ///
    /// Mono because both consumers are mono: the waveform draws one trace and
    /// Granite's beat detector reads one channel. Downmixing is the adapter's
    /// job, which is where it already happens; today's pad probe takes the left
    /// channel.
    pub fn push_pcm(&self, mono: &[f64]) {
        if let Ok(mut w) = self.waveform.write() {
            w.push_samples(mono);
        }
    }

    /// Push one FFT magnitude frame in dB, any band count.
    ///
    /// dB rather than linear magnitude because that is the unit the
    /// normalization below is defined in, and the unit GStreamer's `spectrum`
    /// element already emits. An adapter computing its own FFT converts before
    /// pushing, and the two backends then produce bars that look the same
    /// rather than bars that look plausible.
    ///
    /// The normalization (rescale this frame's own min..max onto 0..1) happens
    /// here, once, rather than per adapter.
    pub fn push_magnitudes_db(&self, db: &[f64]) {
        if db.is_empty() {
            return;
        }
        let min = db.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = db.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let range = max - min;
        let normalized: Vec<f64> = if range > 0.0 {
            db.iter()
                .map(|&v| ((v - min) / range).clamp(0.0, 1.0))
                .collect()
        } else {
            vec![0.0; db.len()]
        };
        if let Ok(mut s) = self.spectrum.write() {
            s.update(normalized);
        }
    }
}

// ---------------------------------------------------------------------------
// The trait
// ---------------------------------------------------------------------------

/// Everything `Player` cannot do without knowing which audio stack it is
/// talking to. Ten methods, one of them the constructor.
///
/// Deliberately **not** object-safe (`Sized`, and `open` returns `Self`). The
/// backend is chosen when the binary is compiled and is never swapped while the
/// app runs, so there is nothing for a `dyn` to be. Requiring `Sized` states
/// that in the type system instead of in a comment, and closes the door on a
/// future `set_backend()`.
///
/// ## What is not here, and why
/// - No spectrum or waveform getter. Capture crosses at construction through
///   [`AnalysisTap`]; display shaping is `Player`'s.
/// - No `has_eq` / `has_spectrum` / `rg_available`. One [`Capabilities`] read.
/// - No `play` / `pause` / `stop`. One [`AudioBackend::set_state`].
/// - No per-band EQ setter, no album-mode setter, no fallback-gain setter.
///   Every write is full-state, so repeating one is free and interleaving two
///   cannot lose half of either.
///
/// ## Contract every adapter owes
/// - **Unity is unity.** [`Amplitude::UNITY`] means the same audible level on
///   every platform: 0.00 dB relative to the decoded stream, which is what
///   GStreamer's `volume` element passes at `1.0`. An output stage that imposes
///   an attenuation of its own must cancel it here. AVFoundation's
///   `mainMixerNode` applies a flat -3.01 dB (1/sqrt(2)) equal-power pan law on
///   a mono path; measured, not assumed. Uncompensated, macOS ships quieter
///   than Linux at the same user volume, and it reads as a broad filter error
///   rather than as a constant gain.
/// - **Degrade, never go silent.** A missing plugin, an EQ node that will not
///   build, a normalization stage that fails to link: the adapter drops that
///   stage and reports it through [`Capabilities`] or [`Applied`]. It does not
///   return an error that leaves no path for audio. This promotes the existing
///   house rule from prose to an invariant every adapter is held to.
/// - **Failure leaves a playable path.** If a reshape fails halfway, the
///   adapter restores a working chain before returning, so the next call starts
///   from a state that plays rather than from wreckage.
pub trait AudioBackend: Sized {
    /// Build the audio path and take the write half of the visualizer buffers.
    ///
    /// The tap is an argument rather than something the backend creates because
    /// `Player` owns the read half and there is only ever one pair. It is also
    /// why a test can choose its backend one test at a time: selection is a
    /// type argument to this call, not a `cfg` on the crate.
    ///
    /// Errors only when no audio is possible at all. A merely diminished
    /// backend (no EQ, no spectrum, no ReplayGain) opens successfully and says
    /// so in [`Self::capabilities`].
    fn open(tap: AnalysisTap) -> Result<Self>;

    /// Constant for the lifetime of this backend. `Player` reads it once.
    fn capabilities(&self) -> Capabilities;

    /// Point the audio path at `source` and return it to a stopped, unstarted
    /// state, discarding anything buffered from the previous track.
    ///
    /// Calling this twice in a row is loading twice: the second call discards
    /// the first's work. The FFI does exactly that on a ReplayGain-forced
    /// reload, so it must be cheap and safe rather than merely legal.
    ///
    /// An adapter that deferred a [`Self::set_normalization`] must apply it
    /// here, before the new source is opened. That is the whole meaning of
    /// [`Applied::AtNextLoad`], and the only ordering constraint on this trait.
    fn load(&mut self, source: &MediaSource) -> Result<()>;

    /// Drive the audio path to `state`.
    ///
    /// One setter rather than play/pause/stop because the states are mutually
    /// exclusive and the transition table belongs to `Player`, which already
    /// owns it. Idempotent: driving to the current state is a no-op, and a call
    /// that failed halfway is corrected by the next one rather than compounded.
    ///
    /// `Stopped` releases the output device. `Paused` preserves position.
    fn set_state(&mut self, state: PlayerState) -> Result<()>;

    /// Jump to an absolute position, flushing whatever is buffered.
    ///
    /// Shares a name with `Player::seek` but not a body: this is where the
    /// GStreamer `FLUSH | KEY_UNIT` flag choice lives, and where AVFoundation's
    /// stop / `scheduleSegment` / restart sequence will live.
    fn seek(&mut self, to: Duration) -> Result<()>;

    /// Position and duration as of now.
    ///
    /// Cheap enough to call every tick; both frontends do, at 10 to 33 Hz.
    fn timeline(&self) -> Timeline;

    /// Give the backend a slice of the caller's thread, and return the next
    /// transport-significant event, or `None`.
    ///
    /// Pull-shaped on purpose. GStreamer's bus is pull-only, and both tick
    /// loops (GTK 33 ms, macOS 100 ms) are already built around a pull. An
    /// adapter whose platform pushes, as AVFoundation does on its own queues,
    /// buffers events into a queue of its own and drains it here. Making
    /// `Player` push-shaped instead would mean callbacks from Rust into Swift,
    /// which the FFI's threading model does not have and does not want.
    ///
    /// ## Invariants
    /// - Events come back in occurrence order, each exactly once.
    /// - Returning `None` means the backend has caught up on everything
    ///   pending, including any analysis frames it services on this thread.
    ///   Callers drain in a `while let Some(_)` loop and must not be able to
    ///   starve it.
    /// - Non-blocking. Never waits on the audio path.
    fn poll_event(&mut self) -> Option<BusEvent>;

    /// Set the audible output level.
    ///
    /// One scalar, because that is genuinely all the backend needs: the
    /// composition `user_volume * user_preamp * fade_factor` is four lines of
    /// arithmetic that belong to `Player` and are identical on every platform.
    /// Infallible, and called from inside the fadeout ramp at tick rate.
    fn set_output_gain(&mut self, gain: Amplitude);

    /// Install all ten band gains.
    ///
    /// Full curve rather than one band, so the backend never holds partial
    /// state and never has to be asked what it currently has. `Player`'s shadow
    /// copy is the single source of truth; this mirrors it.
    ///
    /// A silent no-op when [`Capabilities::eq`] is false.
    fn set_eq(&mut self, curve: &EqCurve);

    /// Make the normalization stage match `want`, and say whether that took
    /// effect now or at the next [`Self::load`].
    ///
    /// The adapter diffs against what it installed; `Player` does not track it.
    /// That kills a shadow copy of pipeline shape that today lives in `Player`
    /// and can drift from the pipeline it shadows.
    ///
    /// A silent no-op returning [`Applied::Now`] when
    /// [`Capabilities::normalization`] is false, because nothing is pending
    /// when nothing will ever happen.
    fn set_normalization(&mut self, want: Normalization) -> Applied;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amplitude_clamps_negatives_and_nan_to_silence() {
        assert_eq!(Amplitude::linear(-1.0), Amplitude::SILENT);
        assert_eq!(Amplitude::linear(f64::NAN), Amplitude::SILENT);
        assert_eq!(Amplitude::linear(1.0), Amplitude::UNITY);
        assert_eq!(Amplitude::linear(0.5).get(), 0.5);
    }

    #[test]
    fn amplitude_allows_gain_above_unity() {
        // Pre-amp reaches 1.5x, so unity is not a ceiling.
        assert_eq!(Amplitude::linear(1.5).get(), 1.5);
    }

    #[test]
    fn eq_curve_clamps_each_band_to_the_limit() {
        let c = EqCurve::FLAT.with_band(3, 99.0);
        assert_eq!(c.band(3), EQ_BAND_DB_LIMIT);

        let c = c.with_band(3, -99.0);
        assert_eq!(c.band(3), -EQ_BAND_DB_LIMIT);

        let c = c.with_band(3, 4.5);
        assert_eq!(c.band(3), 4.5);
    }

    #[test]
    fn eq_curve_ignores_an_out_of_range_band() {
        let c = EqCurve::FLAT.with_band(10, 6.0).with_band(usize::MAX, 6.0);
        assert_eq!(c, EqCurve::FLAT);
    }

    #[test]
    fn eq_curve_reads_zero_past_the_tenth_band() {
        assert_eq!(EqCurve::FLAT.band(10), 0.0);
        assert_eq!(EqCurve::FLAT.band(usize::MAX), 0.0);
    }

    #[test]
    fn with_bands_clamps_and_leaves_uncovered_bands_alone() {
        let c = EqCurve::FLAT.with_band(9, 7.0).with_bands(&[99.0, -99.0, 2.0]);

        assert_eq!(c.band(0), EQ_BAND_DB_LIMIT);
        assert_eq!(c.band(1), -EQ_BAND_DB_LIMIT);
        assert_eq!(c.band(2), 2.0);
        for b in 3..9 {
            assert_eq!(c.band(b), 0.0);
        }
        assert_eq!(c.band(9), 7.0, "a band the slice did not cover is untouched");
    }

    #[test]
    fn with_bands_ignores_entries_past_the_tenth() {
        let c = EqCurve::FLAT.with_bands(&[1.0; 14]);
        assert_eq!(c.as_db(), &[1.0; 10]);
    }

    #[test]
    fn media_source_parses_a_plain_uri_verbatim() {
        assert_eq!(
            MediaSource::parse("file:///music/a.mp3"),
            MediaSource::Uri("file:///music/a.mp3".to_string())
        );
    }

    #[test]
    fn media_source_lifts_the_cdda_device_out_of_the_uri() {
        match MediaSource::parse("cdda://3?device=/dev/sr0") {
            MediaSource::CdTrack { track, device } => {
                assert_eq!(track, "3");
                assert_eq!(device.as_deref(), Some("/dev/sr0"));
            }
            other => panic!("expected a CdTrack, got {other:?}"),
        }
    }

    #[test]
    fn magnitudes_normalize_onto_zero_to_one() {
        let a = Analysis::new(10, 1024);
        let tap = a.tap();
        tap.push_magnitudes_db(&[-60.0, -30.0, 0.0]);

        let m = a.magnitudes();
        assert_eq!(m.len(), 3);
        assert!(m[0] < m[1] && m[1] < m[2], "order preserved: {m:?}");
        assert!(m.iter().all(|v| (0.0..=1.0).contains(v)), "in range: {m:?}");
    }

    #[test]
    fn a_flat_magnitude_frame_reads_as_silence() {
        let a = Analysis::new(10, 1024);
        a.tap().push_magnitudes_db(&[-40.0; 6]);
        assert!(a.magnitudes().iter().all(|&v| v == 0.0));
    }

    #[test]
    fn an_empty_magnitude_frame_is_ignored() {
        let a = Analysis::new(10, 1024);
        a.tap().push_magnitudes_db(&[]);
        assert!(!a.has_magnitudes());
    }

    #[test]
    fn the_tap_accepts_any_pcm_length() {
        // The AVFoundation tap returns whatever size it likes; the spike asked
        // for 1024 and got 4410. Neither length may be special.
        let a = Analysis::new(10, 8192);
        let tap = a.tap();
        tap.push_pcm(&[0.5; 1024]);
        tap.push_pcm(&[0.25; 4410]);
        assert_eq!(a.waveform(64).len(), 64);
    }
}
