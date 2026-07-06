//! GStreamer-backed audio playback engine.
//!
//! This module wraps GStreamer's high-level `playbin` element behind a simple,
//! synchronous-looking API.  All heavy lifting (decoding, audio output, buffer
//! management) is handled by GStreamer internally.  Callers interact only with
//! the small surface exposed here: load a URI, control transport, and poll for
//! end-of-stream or errors.
//!
//! When the `equalizer-10bands` GStreamer element is available it is
//! automatically inserted into the audio processing chain:
//!
//! ```text
//! uridecodebin → audioconvert → spectrum → volume → [equalizer-10bands] → autoaudiosink
//! ```
//!
//! The spectrum element performs FFT analysis on the audio signal and sends
//! spectrum data via GStreamer messages, which are processed by poll_bus()
//! and stored in spectrum_data for the visualizer.

use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_sys;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use crate::config::{EQ_BAND_DB_LIMIT, PREAMP_MAX, PREAMP_MIN};
use crate::model::{SpectrumData, WaveformBuffer};

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

/// A wrapper around GStreamer elements for audio playback.
///
/// `Player` owns a custom pipeline and exposes a state-machine-style API that
/// matches the transport controls visible to the user.  One instance is shared
/// for the lifetime of the application; tracks are loaded by calling `load()`
/// before `play()`.
///
/// The pipeline includes:
/// - `uridecodebin`: decodes any audio format
/// - `audioconvert`: handles format conversion
/// - `spectrum`: performs FFT analysis for the visualizer (placed BEFORE EQ)
/// - `volume`: pre-amp control
/// - `equalizer-10bands`: 10-band EQ (when available)
/// - `autoaudiosink`: audio output
///
/// ## Thread safety
/// GStreamer itself is thread-safe, but `Player` is not `Send`.  It must be
/// used on the thread where `gstreamer::init()` was called (typically the
/// main thread).
pub struct Player {
    /// The GStreamer pipeline.
    pipeline: gst::Pipeline,
    /// The GStreamer `uridecodebin` element for decoding audio.
    decodebin: gst::Element,
    /// The GStreamer `audioconvert` element for format conversion.
    /// Kept alive here — dropping it would disconnect the pipeline.
    #[allow(dead_code)]
    audioconvert: gst::Element,
    /// The GStreamer `spectrum` element for visualizer FFT analysis.
    /// Kept alive here — dropping it would remove it from the pipeline.
    #[allow(dead_code)]
    spectrum_elem: Option<gst::Element>,
    /// Our local view of the pipeline state, updated synchronously on every
    /// transport method call.
    state: PlayerState,
    /// The GStreamer `equalizer-10bands` element, or `None` if unavailable.
    eq: Option<gst::Element>,
    /// A GStreamer `volume` element for pre-amplification.
    /// Stored so that `set_volume` and `set_preamp` can update it.
    volume_elem: gst::Element,
    /// Shadow copy of the current band gains, used to compute auto-compensation.
    eq_bands: [f64; 10],
    /// User-requested pre-amp multiplier (0.5–1.5).
    user_preamp: f64,
    /// User-requested playback volume (0.0–1.0).
    /// Kept separately so that `apply_preamp_compensation` does not overwrite it.
    user_volume: f64,
    /// Shared spectrum data updated from GStreamer bus messages.
    /// Protected by RwLock for thread-safe access.
    spectrum_data: Arc<RwLock<SpectrumData>>,
    /// Ring buffer of recent raw PCM samples for the waveform visualizer.
    /// Written from the GStreamer streaming thread via a pad probe.
    waveform_data: Arc<RwLock<WaveformBuffer>>,
    /// Flag indicating if spectrum element is available.
    has_spectrum: bool,
    /// Granite plasma renderer state (lazy-allocated on first use).
    granite: Option<crate::granite::Granite>,
    /// Device node for the next `cdda://` load (e.g. `/dev/sr0`), consumed by
    /// the `source-setup` handler. Carried out-of-band because the GStreamer
    /// cdda URI has no device syntax: `load()` strips the `?device=` suffix
    /// off the pseudo-URI and stashes it here.
    cdda_device: Arc<Mutex<Option<String>>>,
    /// Fake position for testing (overrides real position when set).
    #[cfg(test)]
    fake_position: Option<Duration>,
}

impl Player {
    /// Create a new `Player` and set up the GStreamer pipeline.
    ///
    /// Returns an error if required GStreamer elements are not available.
    ///
    /// `gstreamer::init()` must have been called before this.
    pub fn new() -> Result<Self> {
        let pipeline = gst::Pipeline::new();

        // Create uridecodebin for decoding audio from any URI
        let decodebin = gst::ElementFactory::make("uridecodebin")
            .name("decode")
            .build()
            .context(
                "Failed to create uridecodebin. Ensure GStreamer base plugins are installed.",
            )?;

        // Create audioconvert for format conversion
        let audioconvert = gst::ElementFactory::make("audioconvert")
            .name("convert")
            .build()
            .context("Failed to create audioconvert element.")?;

        // Create spectrum element for visualizer FFT analysis
        // This is optional - visualizer will be disabled if unavailable
        let spectrum_elem: Option<gst::Element> = gst::ElementFactory::make("spectrum")
            .name("spectrum")
            .build()
            .ok();

        let has_spectrum = spectrum_elem.is_some();

        // Configure spectrum if available
        if let Some(ref spec) = spectrum_elem {
            spec.set_property("bands", 256u32);
            spec.set_property("interval", 50u64 * gst::ClockTime::MSECOND);
            spec.set_property("post-messages", true);
        }

        // Create volume element for pre-amp
        let volume_elem = gst::ElementFactory::make("volume")
            .name("volume")
            .build()
            .context("Failed to create volume element.")?;

        // Create audio sink
        let audiosink = gst::ElementFactory::make("autoaudiosink")
            .name("sink")
            .build()
            .context(
                "Failed to create audio sink. Ensure GStreamer audio output plugins are installed.",
            )?;

        // Try to create equalizer element
        #[cfg(not(test))]
        let eq: Option<gst::Element> = gst::ElementFactory::make("equalizer-10bands")
            .name("equalizer")
            .build()
            .ok();
        #[cfg(test)]
        let eq: Option<gst::Element> = None;

        // Add all elements to pipeline
        pipeline.add(&decodebin)?;
        pipeline.add(&audioconvert)?;
        if let Some(ref spec) = spectrum_elem {
            pipeline.add(spec)?;
        }
        pipeline.add(&volume_elem)?;
        if let Some(ref eq_elem) = eq {
            pipeline.add(eq_elem)?;
        }
        pipeline.add(&audiosink)?;

        // Link elements in order:
        // decodebin → audioconvert → [spectrum] → volume → [equalizer] → audiosink
        // Note: spectrum is linked only if available; otherwise audioconvert → volume directly

        // First, handle the decodebin → audioconvert link (needs pad-added callback)
        // We'll do this asynchronously via the pad-added signal

        // Link audioconvert → [spectrum] → volume
        if let Some(ref spec) = spectrum_elem {
            audioconvert.link(spec)?;
            spec.link(&volume_elem)?;
        } else {
            audioconvert.link(&volume_elem)?;
        }

        // Link volume → [equalizer] → audiosink
        if let Some(ref eq_elem) = eq {
            volume_elem.link(eq_elem)?;
            eq_elem.link(&audiosink)?;
        } else {
            volume_elem.link(&audiosink)?;
        }

        // Connect decodebin pad-added signal to link the decoded audio to audioconvert
        // This is asynchronous because uridecodebin creates pads dynamically
        let audioconvert_clone = audioconvert.clone();
        decodebin.connect_pad_added(move |_dbin, src_pad| {
            // Get the sink pad from audioconvert
            let Some(sink_pad) = audioconvert_clone.static_pad("sink") else {
                return;
            };

            // Only link if not already linked
            if sink_pad.is_linked() {
                return;
            }

            // Check if the pad has audio capability
            let Some(caps) = src_pad.current_caps() else {
                // Caps not yet available, try to link anyway
                let _ = src_pad.link(&sink_pad);
                return;
            };

            let caps_str = caps.to_string();
            let has_audio = caps_str.contains("audio");

            if has_audio || caps_str.contains("audio") {
                let _ = src_pad.link(&sink_pad);
            }
        });

        // Initialize spectrum data
        let spectrum_data = Arc::new(RwLock::new(SpectrumData::new(64)));

        // Waveform ring buffer — 8192 samples ≈ 185 ms at 44.1 kHz.
        let waveform_data = Arc::new(RwLock::new(WaveformBuffer::new(8192)));

        // Add a pad probe to audioconvert's src pad to capture raw PCM samples
        // for the waveform visualizer.  The probe runs on the GStreamer streaming
        // thread; it writes into the RwLock-protected ring buffer.
        #[cfg(not(test))]
        {
            let wd = Arc::clone(&waveform_data);
            if let Some(src_pad) = audioconvert.static_pad("src") {
                src_pad.add_probe(
                    gst::PadProbeType::BUFFER,
                    move |pad, probe_info| {
                        // Caps are negotiated before first buffer arrives; bail if not yet set.
                        let caps = match pad.current_caps() {
                            Some(c) => c,
                            None => return gst::PadProbeReturn::Ok,
                        };
                        let structure = match caps.structure(0) {
                            Some(s) => s,
                            None => return gst::PadProbeReturn::Ok,
                        };

                        let format = structure
                            .get::<String>("format")
                            .unwrap_or_default();
                        let channels = structure
                            .get::<i32>("channels")
                            .unwrap_or(1)
                            .max(1) as usize;

                        if let Some(gst::PadProbeData::Buffer(ref buffer)) = probe_info.data {
                            if let Ok(map) = buffer.map_readable() {
                                let data = map.as_slice();
                                // Extract mono samples (left channel) from the buffer.
                                // Supported formats: F32LE (most common with spectrum),
                                // S16LE (fallback).
                                let samples: Vec<f64> = match format.as_str() {
                                    "F32LE" => {
                                        let frame = 4 * channels; // bytes per frame
                                        data.chunks_exact(frame)
                                            .map(|c| {
                                                f32::from_le_bytes([c[0], c[1], c[2], c[3]])
                                                    as f64
                                            })
                                            .collect()
                                    }
                                    "F64LE" => {
                                        let frame = 8 * channels;
                                        data.chunks_exact(frame)
                                            .map(|c| {
                                                f64::from_le_bytes([
                                                    c[0], c[1], c[2], c[3], c[4], c[5], c[6],
                                                    c[7],
                                                ])
                                            })
                                            .collect()
                                    }
                                    "S16LE" => {
                                        let frame = 2 * channels;
                                        data.chunks_exact(frame)
                                            .map(|c| {
                                                i16::from_le_bytes([c[0], c[1]]) as f64
                                                    / 32768.0
                                            })
                                            .collect()
                                    }
                                    _ => vec![],
                                };

                                if !samples.is_empty() {
                                    if let Ok(mut wb) = wd.write() {
                                        wb.push_samples(&samples);
                                    }
                                }
                            }
                        }
                        gst::PadProbeReturn::Ok
                    },
                );
            }
        }

        // Route the target drive to CD-audio sources. The cdda URI carries no
        // device, so `load()` stashes it here and this handler applies it to
        // the source uridecodebin creates (cdiocddasrc on Linux — anything
        // exposing a "device" property).
        let cdda_device: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        {
            let cdda_device = cdda_device.clone();
            decodebin.connect("source-setup", false, move |values| {
                let Some(dev) = cdda_device.lock().ok().and_then(|d| d.clone()) else {
                    return None;
                };
                if let Ok(source) = values[1].get::<gst::Element>() {
                    if source.find_property("device").is_some() {
                        source.set_property("device", &dev);
                    }
                }
                None
            });
        }

        Ok(Player {
            pipeline,
            decodebin,
            audioconvert,
            spectrum_elem,
            state: PlayerState::Stopped,
            eq,
            volume_elem,
            eq_bands: [0.0; 10],
            user_preamp: 1.0,
            user_volume: 1.0,
            spectrum_data,
            waveform_data,
            has_spectrum,
            granite: None,
            cdda_device,
            #[cfg(test)]
            fake_position: None,
        })
    }

    // -----------------------------------------------------------------------
    // Granite plasma renderer
    // -----------------------------------------------------------------------

    /// Render one frame of the Granite plasma into a caller-owned RGBA8 buffer.
    ///
    /// `dst.len()` must equal `(w * h * 4) as usize`. The renderer's previous-
    /// frame buffer is allocated lazily and persists across calls, so the
    /// feedback effect builds up the same way the plugin's did.
    // Called by the GTK frontend (Linux bin) and the C FFI (lib); dead in
    // the macOS bin where neither is compiled.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn render_granite(
        &mut self,
        dst: &mut [u8],
        w: u32,
        h: u32,
        cfg: &crate::granite::GraniteConfig,
    ) {
        let t_seconds = self
            .position()
            .map(|d| d.as_secs_f64() as f32)
            .unwrap_or(0.0);
        let is_active = self.state == PlayerState::Playing;
        // PCM samples drive the scope shape that's drawn on top of each
        // frame and dissolved by the next frame's warp (Geiss flow).
        let pcm_f64 = self.get_waveform_samples(1024);
        let pcm: Vec<f32> = pcm_f64.iter().map(|&v| v as f32).collect();
        let g = self
            .granite
            .get_or_insert_with(|| crate::granite::Granite::new(w, h));
        g.render(dst, w, h, t_seconds, is_active, &pcm, cfg);
    }

    /// Live effect the scheduler is showing this frame. `None` if the
    /// renderer hasn't been initialised yet (no Granite frame rendered).
    #[allow(dead_code)] // used by macOS FFI only; GTK reads config.effect instead.
    pub fn granite_active_effect(&self) -> Option<crate::granite::GraniteEffect> {
        self.granite.as_ref().map(|g| g.active_effect())
    }

    /// Pin a specific Granite effect (used when the user picks one from
    /// Settings). Skips the scheduler for ~20 s so the choice sticks.
    // Called by the GTK frontend (Linux bin) and the C FFI (lib); dead in
    // the macOS bin where neither is compiled.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn granite_set_effect(&mut self, effect: crate::granite::GraniteEffect) {
        if let Some(g) = self.granite.as_mut() {
            g.set_effect(effect);
        }
    }

    /// Force an immediate switch to a random other Granite effect (keyboard
    /// shortcut). Returns the newly-chosen effect, or `None` when the
    /// renderer hasn't drawn a frame yet.
    // Called by the GTK frontend (Linux bin) and the C FFI (lib); dead in
    // the macOS bin where neither is compiled.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn granite_random_effect(&mut self) -> Option<crate::granite::GraniteEffect> {
        self.granite.as_mut().map(|g| g.random_switch())
    }

    /// Apply a user-picked Granite palette immediately (Settings). Holds
    /// the choice ~20 s before auto palette rolling resumes.
    // Called by the GTK frontend (Linux bin) and the C FFI (lib); dead in
    // the macOS bin where neither is compiled.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn granite_set_palette(&mut self, palette: crate::granite::GranitePalette) {
        if let Some(g) = self.granite.as_mut() {
            g.set_palette(palette);
        }
    }

    /// Estimated tempo from the Granite beat detector; 0.0 when unknown.
    // Called by the GTK frontend (Linux bin) and the C FFI (lib); dead in
    // the macOS bin where neither is compiled.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn granite_bpm(&self) -> f32 {
        self.granite.as_ref().map(|g| g.bpm()).unwrap_or(0.0)
    }

    /// Estimated beats-per-measure from the Granite beat detector (3 or 4);
    /// 0 while unknown.
    // Called by the GTK frontend (Linux bin) and the C FFI (lib); dead in
    // the macOS bin where neither is compiled.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn granite_meter(&self) -> u8 {
        self.granite.as_ref().map(|g| g.meter()).unwrap_or(0)
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

        // CD-audio pseudo-URIs carry the target drive as a query suffix
        // (`cdda://3?device=/dev/sr0`) because the GStreamer cdda scheme has
        // no device syntax. Strip it and hand the device to the source-setup
        // handler; plain URIs clear any stale device.
        let uri = if let Some(rest) = uri.strip_prefix("cdda://") {
            let (track, device) = match rest.split_once("?device=") {
                Some((t, d)) => (t, Some(d.to_string())),
                None => (rest, None),
            };
            if let Ok(mut slot) = self.cdda_device.lock() {
                *slot = device;
            }
            format!("cdda://{track}")
        } else {
            if let Ok(mut slot) = self.cdda_device.lock() {
                *slot = None;
            }
            uri.to_string()
        };

        // Set the URI on the decodebin element
        self.decodebin.set_property("uri", &uri);

        // Clear stale waveform samples from the previous track so the new
        // track starts with a blank canvas rather than a ghost of old audio.
        if let Ok(mut wb) = self.waveform_data.write() {
            wb.reset();
        }

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
    ///
    /// Also clears the spectrum and waveform buffers so the visualizer
    /// collapses to its starting state (no bars / flat line) instead of
    /// freezing on the last received frame.  Pause deliberately leaves
    /// the buffers intact — the user expects pause to hold the picture.
    pub fn stop(&mut self) -> Result<()> {
        self.pipeline.set_state(gst::State::Null)?;
        self.state = PlayerState::Stopped;
        if let Ok(mut spec) = self.spectrum_data.write() {
            spec.clear();
        }
        if let Ok(mut wb) = self.waveform_data.write() {
            wb.clear();
        }
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

    /// Only available in tests — sets a fake position for testing back button behavior.
    #[cfg(test)]
    pub fn set_position_for_test(&mut self, pos: Duration) {
        self.fake_position = Some(pos);
    }

    /// Return the current playback position, or `None` if no track is loaded.
    ///
    /// The position is queried directly from the GStreamer pipeline clock and
    /// is accurate to nanoseconds, though the system timer resolution may be
    /// coarser in practice.
    ///
    /// In tests, returns the fake position if set via `set_position_for_test`.
    pub fn position(&self) -> Option<Duration> {
        #[cfg(test)]
        if let Some(pos) = self.fake_position {
            return Some(pos);
        }
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
        self.pipeline
            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, time)?;
        Ok(())
    }

    /// Set the playback volume.
    ///
    /// `vol` is clamped to `[0.0, 1.0]` before being applied.  The value
    /// written to GStreamer is `vol × user_preamp` so that subsequent
    /// `apply_preamp_compensation` calls do not reset the user's chosen level.
    pub fn set_volume(&mut self, vol: f64) {
        self.user_volume = vol.clamp(0.0, 1.0);
        self.volume_elem
            .set_property("volume", self.user_volume * self.user_preamp);
    }

    /// Returns `true` if the `equalizer-10bands` element was successfully
    /// created at startup.  The EQ methods are no-ops when this returns `false`.
    #[allow(dead_code)]
    pub fn has_eq(&self) -> bool {
        self.eq.is_some()
    }

    /// Returns `true` if the spectrum element is available.
    #[allow(dead_code)]
    pub fn has_spectrum(&self) -> bool {
        self.has_spectrum
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
            let clamped = gain_db.clamp(-EQ_BAND_DB_LIMIT, EQ_BAND_DB_LIMIT);
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
        if band < 10 {
            self.eq_bands[band]
        } else {
            0.0
        }
    }

    /// Apply all 10 band gains from a slice in one call.
    ///
    /// Convenient for bulk-applying a preset or a restored config.  Silently
    /// ignores extra elements if `bands` has more than 10 entries; bands not
    /// covered by a short slice are left unchanged.  Pre-amp compensation is
    /// recalculated once after all bands are applied.
    pub fn apply_eq_bands(&mut self, bands: &[f64]) {
        for (i, &gain) in bands.iter().take(10).enumerate() {
            let clamped = gain.clamp(-EQ_BAND_DB_LIMIT, EQ_BAND_DB_LIMIT);
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
        self.user_preamp = multiplier.clamp(PREAMP_MIN, PREAMP_MAX);
        self.apply_preamp_compensation();
    }

    /// Write the combined `user_volume × user_preamp` value to the GStreamer
    /// volume element.  Called by both `set_volume` and `set_preamp` so that
    /// neither overwrites the other's contribution.
    fn apply_preamp_compensation(&self) {
        self.volume_elem
            .set_property("volume", self.user_volume * self.user_preamp);
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
    /// This method also updates the shared spectrum data from GStreamer messages.
    ///
    /// Errors are NOT written to stderr; callers surface them through the UI.
    pub fn poll_bus(&mut self) -> Option<BusEvent> {
        use gst::MessageView;
        let bus = self.pipeline.bus()?;

        // Drain every pending message in one call so we don't leave stale
        // messages in the queue between ticks.
        while let Some(msg) = bus.timed_pop(gst::ClockTime::ZERO) {
            match msg.view() {
                MessageView::Eos(..) => {
                    self.state = PlayerState::Stopped;
                    return Some(BusEvent::Eos);
                }
                MessageView::Error(_) => {
                    self.state = PlayerState::Stopped;
                    return Some(BusEvent::Error);
                }
                MessageView::Element(elem) => {
                    // Handle spectrum messages
                    if let Some(structure) = elem.structure() {
                        if structure.has_name("spectrum") {
                            self.handle_spectrum_message(&structure);
                        }
                    }
                }
                _ => {}
            }
        }

        None
    }

    /// Handle a spectrum message from GStreamer and update shared spectrum data.
    fn handle_spectrum_message(&self, structure: &gst::StructureRef) {
        let data = match self.extract_magnitude_as_vec(structure) {
            Some(d) => d,
            None => return,
        };

        if !data.is_empty() {
            // Find min and max dB values for dynamic normalization
            let min_val = data.iter().cloned().fold(f64::INFINITY, f64::min);
            let max_val = data.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

            let range = max_val - min_val;
            let normalized: Vec<f64> = if range > 0.0 {
                data.iter()
                    .map(|&db| ((db - min_val) / range).clamp(0.0, 1.0))
                    .collect()
            } else {
                // All values are the same, treat as silence
                vec![0.0; data.len()]
            };

            if let Ok(mut spec_data) = self.spectrum_data.write() {
                spec_data.update(normalized);
            }
        }
    }

    /// Extract magnitude data from the spectrum structure using FFI.
    /// The spectrum element sends magnitude as GST_TYPE_LIST containing G_TYPE_FLOAT values.
    fn extract_magnitude_as_vec(&self, structure: &gst::StructureRef) -> Option<Vec<f64>> {
        use gst::glib::translate::ToGlibPtr;

        unsafe {
            let field_value = structure.value("magnitude").map_err(|_| ()).ok()?;
            let list_gvalue_ptr = field_value.to_glib_none().0;

            // Get the number of values in the list
            let num_values = gstreamer_sys::gst_value_list_get_size(list_gvalue_ptr);
            if num_values == 0 {
                return None;
            }

            let mut result = Vec::with_capacity(num_values as usize);

            for i in 0..num_values {
                let value_ptr = gstreamer_sys::gst_value_list_get_value(list_gvalue_ptr, i);
                if value_ptr.is_null() {
                    break;
                }

                // Extract the float value from the GValue
                let float_val = gst::glib::gobject_ffi::g_value_get_float(value_ptr);
                result.push(float_val as f64);
            }

            if result.is_empty() {
                return None;
            }

            Some(result)
        }
    }

    /// Return spectrum data mapped to display bars using logarithmic frequency scale.
    ///
    /// Maps the raw spectrum bands (64 bands, 0-22050 Hz) to `num_bands` display bars
    /// using a logarithmic scale that matches the equalizer frequency range (30-15000 Hz).
    ///
    /// Uses smoothed band values for smooth bar animation.
    pub fn get_spectrum_display_bands(&self, num_bands: u32) -> Vec<f64> {
        let spectrum = match self.spectrum_data.read() {
            Ok(data) if data.has_received_data() && !data.bands.is_empty() => {
                data.smoothed().to_vec()
            }
            _ => return vec![0.0; num_bands as usize],
        };

        let spectrum_len = spectrum.len() as f64;
        let nyquist = 22050.0_f64;

        // Plateau distribution with 256 FFT bands for better frequency resolution
        // Each frequency maps to a distinct FFT band to minimize spectral leakage overlap
        // Range: 100 Hz to 3800 Hz
        let target_freqs: [f64; 16] = [
            86.0,   // Bar 0: FFT band 1 (86-172 Hz)
            172.0,  // Bar 1: FFT band 2 (172-258 Hz)
            344.0,  // Bar 2: FFT band 4 (344-430 Hz)
            430.0,  // Bar 3: FFT band 5 (430-516 Hz)
            602.0,  // Bar 4: FFT band 7 (602-688 Hz)
            775.0,  // Bar 5: FFT band 9 (775-861 Hz)
            947.0,  // Bar 6: FFT band 11 (947-1033 Hz)
            1119.0, // Bar 7: FFT band 13 (1119-1205 Hz)
            1292.0, // Bar 8: FFT band 15 (1292-1378 Hz)
            1464.0, // Bar 9: FFT band 17 (1464-1550 Hz)
            1722.0, // Bar 10: FFT band 20 (1722-1808 Hz)
            1981.0, // Bar 11: FFT band 23 (1981-2067 Hz)
            2239.0, // Bar 12: FFT band 26 (2239-2325 Hz)
            2670.0, // Bar 13: FFT band 31 (2670-2756 Hz)
            3272.0, // Bar 14: FFT band 38 (3272-3358 Hz)
            3790.0, // Bar 15: FFT band 44 (3790-3876 Hz)
        ];

        (0..num_bands)
            .map(|i| {
                let i = i as usize;
                let target_freq = if i < target_freqs.len() {
                    target_freqs[i]
                } else {
                    // Fallback for num_bands > 16
                    let t = i as f64 / num_bands as f64;
                    100.0 * (38.0_f64).powf(t)
                };
                let band_idx =
                    ((target_freq / nyquist) * spectrum_len).min(spectrum_len - 1.0) as usize;
                spectrum.get(band_idx).copied().unwrap_or(0.0)
            })
            .collect()
    }

    /// Return `count` waveform PCM samples for the visualizer.
    ///
    /// Samples are in `[-1.0, 1.0]` (bipolar, centre = silence).  Returns
    /// all zeros when not enough audio has been buffered yet.
    pub fn get_waveform_samples(&self, count: usize) -> Vec<f64> {
        self.waveform_data
            .read()
            .map(|wb| wb.get_samples(count))
            .unwrap_or_else(|_| vec![0.0; count])
    }

    /// Check if spectrum data has been received from GStreamer.
    ///
    /// Returns true if the spectrum element is available and has sent at least
    /// one message with valid data.
    #[allow(dead_code)] // GTK-only; out of bin reach on macOS where GTK is gated.
    pub fn has_spectrum_data(&self) -> bool {
        self.has_spectrum
            && self
                .spectrum_data
                .read()
                .map(|data| data.has_received_data())
                .unwrap_or(false)
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
