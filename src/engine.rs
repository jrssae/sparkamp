//! GStreamer-backed audio playback engine.
//!
//! This module wraps GStreamer's high-level `playbin` element behind a simple,
//! synchronous-looking API.  All heavy lifting (decoding, audio output, buffer
//! management) is handled by GStreamer internally.  Callers interact only with
//! the small surface exposed here: load a URI, control transport, and poll for
//! end-of-stream or errors.

use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use std::time::Duration;

// ---------------------------------------------------------------------------
// BusEvent
// ---------------------------------------------------------------------------

/// The two events the GStreamer bus can signal that the UI cares about.
///
/// Returned by [`Player::poll_bus`].  `None` from that method means no event
/// is pending; `Some(BusEvent)` means something happened and the caller
/// should react (advance the playlist, mark a track broken, etc.).
#[derive(Debug, Clone, PartialEq)]
pub enum BusEvent {
    /// The current track finished playing normally (end-of-stream).
    Eos,
    /// GStreamer reported a fatal error (e.g. file not found, codec missing).
    Error,
}

// ---------------------------------------------------------------------------
// PlayerState
// ---------------------------------------------------------------------------

/// The three mutually-exclusive transport states of the player.
///
/// This mirrors the subset of GStreamer pipeline states that the rest of the
/// application cares about.  It is kept in sync with the pipeline inside each
/// `Player` method that changes state.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerState {
    /// No track loaded, or playback has been explicitly stopped.
    Stopped,
    /// A track is loaded and audio is actively being decoded and output.
    Playing,
    /// A track is loaded but decoding is frozen; position is preserved.
    Paused,
}

// ---------------------------------------------------------------------------
// Player
// ---------------------------------------------------------------------------

/// A thin wrapper around GStreamer's `playbin` element.
///
/// `Player` owns a single `playbin` pipeline and exposes a state-machine-style
/// API that matches the transport controls visible to the user.  One instance
/// is shared for the lifetime of the application; tracks are loaded by calling
/// `load()` before `play()`.
///
/// When the `equalizer-10bands` GStreamer element is available it is
/// automatically inserted into the audio processing chain via `playbin`'s
/// `audio-filter` property, chained behind a pre-amp `volume` element:
///
/// ```text
/// playbin → [GstBin: volume (pre-amp) → equalizer-10bands] → audio sink
/// ```
///
/// EQ band gains and the pre-amp multiplier can be adjusted at any time
/// (even during playback) via the respective methods.
///
/// ## Thread safety
/// GStreamer itself is thread-safe, but `Player` is not `Send`.  It must be
/// used on the thread where `gstreamer::init()` was called (typically the
/// main thread).
pub struct Player {
    /// The GStreamer `playbin` pipeline element.  `playbin` handles format
    /// detection, decoding, audio sink selection, and volume control.
    pipeline: gst::Element,
    /// Our local view of the pipeline state, updated synchronously on every
    /// transport method call.
    state: PlayerState,
    /// The GStreamer `equalizer-10bands` element injected via `audio-filter`,
    /// or `None` if the element plugin is not installed.
    eq: Option<gst::Element>,
    /// A GStreamer `volume` element inserted before the EQ for pre-amplification.
    /// Stored so that `set_preamp` can update it live during playback.
    /// `None` when the EQ is unavailable.
    preamp_elem: Option<gst::Element>,
    /// Shadow copy of the current band gains, used to compute auto-compensation.
    eq_bands: [f64; 10],
    /// User-requested pre-amp multiplier (0.5–1.5).
    /// The value actually sent to the hardware is `user_preamp * compensation`
    /// where compensation counters any positive-dB band boost to prevent clipping.
    user_preamp: f64,
}

impl Player {
    /// Create a new `Player` and verify that the `playbin` GStreamer element
    /// is available.
    ///
    /// Returns an error if `playbin` is not registered in the GStreamer
    /// registry (e.g., `gstreamer-plugins-base` is not installed).
    ///
    /// `gstreamer::init()` must have been called before this.
    pub fn new() -> Result<Self> {
        let pipeline = gst::ElementFactory::make("playbin")
            .name("player")
            .build()
            .context("Failed to create GStreamer playbin. Ensure GStreamer and MP3 plugins are installed.")?;

        // Try to insert a pre-amp volume element and a 10-band equalizer into
        // the playbin audio chain via the `audio-filter` property.  Both
        // elements are wrapped in a GstBin so playbin sees them as one unit.
        //
        // Chain: [GstBin: volume (pre-amp) → equalizer-10bands]
        //
        // Skipped in test builds: the GLib type system for `GstIirEqualizerBand`
        // is not safe to register from multiple threads simultaneously, which
        // happens when cargo runs tests in parallel.  Tests verify config/state
        // logic; the GStreamer elements are exercised by running the actual app.
        #[cfg(not(test))]
        let (eq, preamp_elem) = {
            match (
                gst::ElementFactory::make("volume").build(),
                gst::ElementFactory::make("equalizer-10bands").build(),
            ) {
                (Ok(vol), Ok(eq_elem)) => {
                    // Wrap both in a bin so playbin treats them as a single
                    // audio-filter element.
                    let bin = gst::Bin::new();
                    let _ = bin.add_many([&vol, &eq_elem]);
                    let _ = vol.link(&eq_elem);
                    // Expose the bin's sink (from vol) and src (from eq) as
                    // ghost pads so playbin can connect to them.
                    if let Some(sink_pad) = vol.static_pad("sink") {
                        if let Ok(ghost) = gst::GhostPad::with_target(&sink_pad) {
                            let _ = bin.add_pad(&ghost);
                        }
                    }
                    if let Some(src_pad) = eq_elem.static_pad("src") {
                        if let Ok(ghost) = gst::GhostPad::with_target(&src_pad) {
                            let _ = bin.add_pad(&ghost);
                        }
                    }
                    // Set the bin as the audio-filter for playbin.
                    pipeline.set_property("audio-filter", &bin);
                    (Some(eq_elem), Some(vol))
                }
                // If either element is unavailable (missing GStreamer plugin),
                // skip both silently — the player works without EQ/pre-amp.
                _ => (None, None),
            }
        };
        #[cfg(test)]
        let (eq, preamp_elem): (Option<gst::Element>, Option<gst::Element>) = (None, None);

        Ok(Player {
            pipeline,
            state: PlayerState::Stopped,
            eq,
            preamp_elem,
            eq_bands:    [0.0; 10],
            user_preamp: 1.0,
        })
    }

    /// Load a URI (e.g. `"file:///path/to/track.mp3"`) and reset to the
    /// stopped state.
    ///
    /// This must be called before `play()` when switching to a new track.
    /// It sets the pipeline state to `Null` first, which flushes any buffered
    /// data from the previous track, then assigns the new URI.
    pub fn load(&mut self, uri: &str) -> Result<()> {
        // Setting state to Null tears down the current pipeline (flushes
        // buffers, releases the audio device, etc.) so the new URI starts
        // clean.
        self.pipeline.set_state(gst::State::Null)?;
        self.pipeline.set_property("uri", uri);
        self.state = PlayerState::Stopped;
        Ok(())
    }

    /// Begin or resume playback of the currently loaded URI.
    ///
    /// GStreamer transitions the pipeline to `Playing` asynchronously in the
    /// background.  The method returns as soon as the state-change request is
    /// posted, before audio actually starts.
    pub fn play(&mut self) -> Result<()> {
        self.pipeline.set_state(gst::State::Playing)?;
        self.state = PlayerState::Playing;
        Ok(())
    }

    /// Toggle between `Playing` and `Paused`.
    ///
    /// - If currently `Playing`, pauses (freezes decode, retains position).
    /// - If currently `Paused`, resumes from the frozen position.
    /// - If `Stopped`, does nothing (nothing to pause or resume).
    pub fn toggle_pause(&mut self) -> Result<()> {
        match self.state {
            PlayerState::Playing => {
                self.pipeline.set_state(gst::State::Paused)?;
                self.state = PlayerState::Paused;
            }
            PlayerState::Paused => {
                self.pipeline.set_state(gst::State::Playing)?;
                self.state = PlayerState::Playing;
            }
            PlayerState::Stopped => {}
        }
        Ok(())
    }

/// Stop playback and release the audio device.
    ///
    /// Sets the pipeline state to `Null`.  A subsequent `play()` call will
    /// restart from the beginning of the last loaded URI.
    pub fn stop(&mut self) -> Result<()> {
        self.pipeline.set_state(gst::State::Null)?;
        self.state = PlayerState::Stopped;
        Ok(())
    }

    /// Return the current [`PlayerState`] without changing it.
    pub fn state(&self) -> &PlayerState {
        &self.state
    }

    /// Force the player into a specific state without touching GStreamer.
    /// Only available in tests — used to simulate paused/playing conditions
    /// without needing a real audio pipeline.
    #[cfg(test)]
    pub fn set_state_for_test(&mut self, s: PlayerState) {
        self.state = s;
    }

    /// Return the current playback position, or `None` if no track is loaded.
    ///
    /// The position is queried directly from the GStreamer pipeline clock and
    /// is accurate to nanoseconds, though the system timer resolution may be
    /// coarser in practice.
    pub fn position(&self) -> Option<Duration> {
        self.pipeline
            .query_position::<gst::ClockTime>()
            .map(|t| Duration::from_nanos(t.nseconds()))
    }

    /// Return the total duration of the loaded track, or `None` if the
    /// duration is not yet known (e.g., the pipeline is still starting up or
    /// the format does not advertise a duration).
    pub fn duration(&self) -> Option<Duration> {
        self.pipeline
            .query_duration::<gst::ClockTime>()
            .map(|t| Duration::from_nanos(t.nseconds()))
    }

    /// Seek to an absolute position within the current track.
    ///
    /// Uses `FLUSH | KEY_UNIT` flags so GStreamer discards buffered data and
    /// snaps to the nearest keyframe, which prevents audible glitches.
    pub fn seek(&mut self, pos: Duration) -> Result<()> {
        let time = gst::ClockTime::from_nseconds(pos.as_nanos() as u64);
        self.pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            time,
        )?;
        Ok(())
    }

    /// Set the playback volume.
    ///
    /// `vol` is clamped to `[0.0, 1.0]` before being applied.  GStreamer's
    /// `playbin` accepts `0.0` (silence) through `10.0` (10× amplification);
    /// we restrict to `1.0` to prevent accidental over-amplification.
    pub fn set_volume(&mut self, vol: f64) {
        self.pipeline.set_property("volume", vol.clamp(0.0, 1.0));
    }

    /// Returns `true` if the `equalizer-10bands` element was successfully
    /// created at startup.  The EQ methods are no-ops when this returns `false`.
    #[allow(dead_code)]
    pub fn has_eq(&self) -> bool {
        self.eq.is_some()
    }

    /// Set the gain for a single EQ band.
    ///
    /// `band` must be in `0..10`; values outside that range are silently
    /// ignored.  `gain_db` is clamped to `[-12.0, +12.0]` dB — a symmetric
    /// range that fits within GStreamer's `equalizer-10bands` hardware limit.
    ///
    /// After setting the band, the pre-amp volume is automatically adjusted
    /// downward to compensate for any positive boost, preventing clipping.
    ///
    /// The change takes effect immediately, even during playback.
    pub fn set_eq_band(&mut self, band: usize, gain_db: f64) {
        if band < 10 {
            let clamped = gain_db.clamp(-12.0, 12.0);
            if let Some(eq) = &self.eq {
                let prop = format!("band{band}");
                eq.set_property(&prop, clamped);
            }
            self.eq_bands[band] = clamped;
            self.apply_preamp_compensation();
        }
    }

    /// Read back the current gain for a single EQ band from the shadow copy.
    ///
    /// Returns `0.0` if `band` is out of range.
    #[allow(dead_code)]
    pub fn get_eq_band(&self, band: usize) -> f64 {
        if band < 10 { self.eq_bands[band] } else { 0.0 }
    }

    /// Apply all 10 band gains from a slice in one call.
    ///
    /// Convenient for bulk-applying a preset or a restored config.  Silently
    /// ignores extra elements if `bands` has more than 10 entries; bands not
    /// covered by a short slice are left unchanged.  Pre-amp compensation is
    /// recalculated once after all bands are applied.
    pub fn apply_eq_bands(&mut self, bands: &[f64]) {
        for (i, &gain) in bands.iter().take(10).enumerate() {
            let clamped = gain.clamp(-12.0, 12.0);
            if let Some(eq) = &self.eq {
                let prop = format!("band{i}");
                eq.set_property(&prop, clamped);
            }
            self.eq_bands[i] = clamped;
        }
        self.apply_preamp_compensation();
    }

    /// Set the user-requested pre-amplifier gain applied before the EQ bands.
    ///
    /// `multiplier` is a linear scale factor in `[0.5, 1.5]` (50 %–150 %).
    /// Pass `1.0` for unity gain.  The value actually written to the hardware
    /// is reduced automatically when any band has a positive boost, so the
    /// combined output never clips.  This is a no-op when the EQ plugin is
    /// unavailable.
    pub fn set_preamp(&mut self, multiplier: f64) {
        self.user_preamp = multiplier.clamp(0.5, 1.5);
        self.apply_preamp_compensation();
    }

    /// Apply the user pre-amp multiplier directly to the volume element.
    ///
    /// EQ band boosts are left to the equalizer element — no auto-compensation
    /// is applied here, as that was silently dropping volume whenever any band
    /// was boosted, which is the wrong behaviour for an EQ.
    fn apply_preamp_compensation(&self) {
        if let Some(vol) = &self.preamp_elem {
            vol.set_property("volume", self.user_preamp);
        }
    }

    /// Non-blocking bus poll.  Returns `Some(BusEvent)` when the current track
    /// has ended (EOS) or hit a fatal error, or `None` when nothing noteworthy
    /// is pending.  The caller should advance the playlist on any `Some` result,
    /// and additionally mark the current track broken on `BusEvent::Error`.
    ///
    /// Only processes messages already in the bus queue (zero-timeout), so it
    /// never blocks the calling thread.  Should be called regularly (e.g.
    /// every 100 ms) from the UI tick loop.
    ///
    /// Errors are NOT written to stderr; callers surface them through the UI.
    pub fn poll_bus(&mut self) -> Option<BusEvent> {
        let bus = self.pipeline.bus()?;

        // Drain every pending message in one call so we don't leave stale
        // messages in the queue between ticks.
        while let Some(msg) = bus.timed_pop(gst::ClockTime::ZERO) {
            use gst::MessageView;
            match msg.view() {
                MessageView::Eos(..) => {
                    self.state = PlayerState::Stopped;
                    return Some(BusEvent::Eos);
                }
                MessageView::Error(_) => {
                    self.state = PlayerState::Stopped;
                    return Some(BusEvent::Error);
                }
                _ => {}
            }
        }
        None
    }

}

impl Drop for Player {
    /// Shut down the GStreamer pipeline when the `Player` is dropped.
    ///
    /// Setting the state to `Null` releases the audio device and all
    /// associated resources, preventing resource leaks on exit.
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
