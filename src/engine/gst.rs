//! The GStreamer adapter.
//!
//! Everything below the seam that knows what a `GstElement` is. The audio path
//! it builds:
//!
//! ```text
//! uridecodebin → audioconvert → [spectrum] → [rgvolume → rglimiter] → volume → [equalizer-10bands] → sink
//! ```
//!
//! Bracketed stages are optional: a missing plugin drops that stage and is
//! reported through [`Capabilities`], never as an error that leaves no audio
//! path.
//!
//! The sink is a construction parameter rather than a hardcoded
//! `autoaudiosink` because a test needs a pipeline that reaches PLAYING with no
//! audio device: [`GstBackend::open_with_sink`] takes `fakesink sync=false` and
//! the ReplayGain deferral test then asserts against real pipeline state
//! instead of a forced one.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_sys;

use crate::engine::backend::{
    Amplitude, AnalysisTap, Applied, AudioBackend, Capabilities, EqCurve, MediaSource,
    Normalization, Timeline,
};
use crate::engine::{BusEvent, PlayerState};

/// The GStreamer audio path and the state that only means something to it.
///
/// Not `Send`: it must be used on the thread where `gstreamer::init()` was
/// called.
pub struct GstBackend {
    pipeline: gst::Pipeline,
    decodebin: gst::Element,
    /// The head of the ReplayGain segment when there is no spectrum element,
    /// and the pad the waveform probe sits on.
    audioconvert: gst::Element,
    spectrum_elem: Option<gst::Element>,
    eq: Option<gst::Element>,
    volume_elem: gst::Element,
    /// ReplayGain in-chain elements, present only while active.
    rg_volume: Option<gst::Element>,
    rg_limiter: Option<gst::Element>,
    /// What the pipeline is actually shaped and set to right now. Read back
    /// from the elements' own presence rather than promised by a caller, so
    /// there is nothing here for `Player` to shadow.
    installed: Normalization,
    /// A normalization requested while the pipeline was running, applied at the
    /// next [`AudioBackend::load`]. Relinking is only legal at `State::Null`.
    pending: Option<Normalization>,
    caps: Capabilities,
    tap: AnalysisTap,
    /// Device node for the next `cdda://` load (e.g. `/dev/sr0`), consumed by
    /// the `source-setup` handler. Carried out-of-band because the GStreamer
    /// cdda URI has no device syntax.
    cdda_device: Arc<Mutex<Option<String>>>,
    /// Set while this backend holds one count of the exclusive-read guard, for
    /// either kind of disc read: a `cdda://` stream or a file on a mounted
    /// optical volume. One flag because it is one count.
    holds_disc_guard: bool,
}

impl GstBackend {
    /// Build the audio path with `sink` as its output stage.
    ///
    /// `gstreamer::init()` must have been called before this.
    pub fn open_with_sink(tap: AnalysisTap, sink: gst::Element) -> Result<Self> {
        let pipeline = gst::Pipeline::new();

        let decodebin = gst::ElementFactory::make("uridecodebin")
            .name("decode")
            .build()
            .context(
                "Failed to create uridecodebin. Ensure GStreamer base plugins are installed.",
            )?;

        let audioconvert = gst::ElementFactory::make("audioconvert")
            .name("convert")
            .build()
            .context("Failed to create audioconvert element.")?;

        let spectrum_elem: Option<gst::Element> = gst::ElementFactory::make("spectrum")
            .name("spectrum")
            .build()
            .ok();

        if let Some(ref spec) = spectrum_elem {
            spec.set_property("bands", 256u32);
            spec.set_property("interval", 50u64 * gst::ClockTime::MSECOND);
            spec.set_property("post-messages", true);
        }

        let volume_elem = gst::ElementFactory::make("volume")
            .name("volume")
            .build()
            .context("Failed to create volume element.")?;

        // Widen the sink's ring buffer from its 200 ms default.
        //
        // Nothing between the source and the sink buffers anything: read,
        // parse, convert, spectrum, EQ and volume all run on one streaming
        // thread that pushes straight into the sink, so the ring buffer is the
        // only slack in the pipeline. 200 ms of it is not much. Measured on
        // macOS with a CD playing (`GST_DEBUG=audiobasesink:6`), the sink is
        // normally called every 42 ms with 40 ms of audio; adding a track to
        // the playlist stalled the streaming thread long enough that four
        // buffers arrived over 640 ms — 160 ms of audio where 640 ms was
        // needed. Every buffer was contiguous and complete, so nothing failed:
        // the samples simply arrived after the speaker had run out, which is
        // audible as a skip.
        //
        // Half a second absorbs that with room to spare, and covers the other
        // sources of jitter on this path — an optical drive seeking, or any
        // main-loop hitch. The cost is latency on seek and on EQ changes, both
        // of which flush the pipeline, so it is bounded by this figure.
        //
        // `autoaudiosink` is a bin that picks the real sink at state change,
        // so the property has to be set on the child when it appears rather
        // than here; `buffer-time` is microseconds, and the guard keeps this a
        // no-op for any sink that does not have it.
        if let Some(bin) = sink.downcast_ref::<gst::Bin>() {
            bin.connect_element_added(|_, element| {
                if element.find_property("buffer-time").is_some() {
                    element.set_property("buffer-time", 500_000i64);
                }
            });
        }

        // `equalizer-10bands` is not safe to instantiate for the FIRST time
        // from two threads at once: both enter the plugin's class init and one
        // faults on a half-built class (EXC_BAD_ACCESS at 0x70, inside
        // libgstequalizer). Measured here: eight threads building it cold
        // crashed 5 runs in 10, and 0 in 10 when one instance was built first.
        // Every instantiation after the first is fine.
        //
        // Production opens one backend on one thread and never sees this. The
        // test suite opens many on libtest's threads and does — which is what
        // the old `#[cfg(test)] let eq = None` stub was really hiding, at the
        // cost of making the equalizer untestable. One serialized build is the
        // whole fix.
        static EQUALIZER_CLASS_INIT: std::sync::Once = std::sync::Once::new();
        EQUALIZER_CLASS_INIT.call_once(|| {
            let _ = gst::ElementFactory::make("equalizer-10bands").build();
        });

        let eq: Option<gst::Element> = gst::ElementFactory::make("equalizer-10bands")
            .name("equalizer")
            .build()
            .ok();

        pipeline.add(&decodebin)?;
        pipeline.add(&audioconvert)?;
        if let Some(ref spec) = spectrum_elem {
            pipeline.add(spec)?;
        }
        pipeline.add(&volume_elem)?;
        if let Some(ref eq_elem) = eq {
            pipeline.add(eq_elem)?;
        }
        pipeline.add(&sink)?;

        if let Some(ref spec) = spectrum_elem {
            audioconvert.link(spec)?;
            spec.link(&volume_elem)?;
        } else {
            audioconvert.link(&volume_elem)?;
        }

        if let Some(ref eq_elem) = eq {
            volume_elem.link(eq_elem)?;
            eq_elem.link(&sink)?;
        } else {
            volume_elem.link(&sink)?;
        }

        // uridecodebin creates its pads dynamically, so the decode → convert
        // link can only be made once a pad shows up.
        let audioconvert_clone = audioconvert.clone();
        decodebin.connect_pad_added(move |_dbin, src_pad| {
            let Some(sink_pad) = audioconvert_clone.static_pad("sink") else {
                return;
            };
            if sink_pad.is_linked() {
                return;
            }
            let Some(caps) = src_pad.current_caps() else {
                // Caps not negotiated yet; linking is still the best guess.
                let _ = src_pad.link(&sink_pad);
                return;
            };
            if caps.to_string().contains("audio") {
                let _ = src_pad.link(&sink_pad);
            }
        });

        Self::attach_waveform_probe(&audioconvert, tap.clone());

        // Route the target drive to CD-audio sources. The cdda URI carries no
        // device, so `load()` stashes it here and this handler applies it to
        // the source uridecodebin creates (cdparanoiasrc on Linux — anything
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

        let caps = Capabilities {
            eq: eq.is_some(),
            spectrum: spectrum_elem.is_some(),
            normalization: gst::ElementFactory::find("rgvolume").is_some(),
        };

        let mut backend = GstBackend {
            pipeline,
            decodebin,
            audioconvert,
            spectrum_elem,
            eq,
            volume_elem,
            rg_volume: None,
            rg_limiter: None,
            // The chain as built above: no normalization stage at all.
            installed: Normalization {
                enabled: false,
                clip_protection: false,
                fallback_db: 0.0,
                album_mode: false,
            },
            pending: None,
            caps,
            tap,
            cdda_device,
            holds_disc_guard: false,
        };
        backend.set_output_gain(Amplitude::UNITY);
        Ok(backend)
    }

    /// Capture raw PCM off audioconvert's src pad for the waveform visualizer.
    /// The probe runs on the streaming thread and writes through its own tap
    /// clone.
    fn attach_waveform_probe(audioconvert: &gst::Element, tap: AnalysisTap) {
        let Some(src_pad) = audioconvert.static_pad("src") else {
            return;
        };
        src_pad.add_probe(gst::PadProbeType::BUFFER, move |pad, probe_info| {
            // Caps are negotiated before the first buffer arrives; bail if not
            // yet set.
            let Some(caps) = pad.current_caps() else {
                return gst::PadProbeReturn::Ok;
            };
            let Some(structure) = caps.structure(0) else {
                return gst::PadProbeReturn::Ok;
            };

            let format = structure.get::<String>("format").unwrap_or_default();
            let channels = structure.get::<i32>("channels").unwrap_or(1).max(1) as usize;

            if let Some(gst::PadProbeData::Buffer(ref buffer)) = probe_info.data {
                if let Ok(map) = buffer.map_readable() {
                    let data = map.as_slice();
                    // The left channel stands in for the mono trace the two
                    // consumers draw.
                    let samples: Vec<f64> = match format.as_str() {
                        "F32LE" => {
                            let frame = 4 * channels;
                            data.chunks_exact(frame)
                                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64)
                                .collect()
                        }
                        "F64LE" => {
                            let frame = 8 * channels;
                            data.chunks_exact(frame)
                                .map(|c| {
                                    f64::from_le_bytes([
                                        c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7],
                                    ])
                                })
                                .collect()
                        }
                        "S16LE" => {
                            let frame = 2 * channels;
                            data.chunks_exact(frame)
                                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f64 / 32768.0)
                                .collect()
                        }
                        _ => vec![],
                    };

                    if !samples.is_empty() {
                        tap.push_pcm(&samples);
                    }
                }
            }
            gst::PadProbeReturn::Ok
        });
    }

    /// The element the ReplayGain segment hangs off: spectrum when present,
    /// else audioconvert (mirrors the link order in `open_with_sink`).
    fn rg_upstream(&self) -> &gst::Element {
        self.spectrum_elem.as_ref().unwrap_or(&self.audioconvert)
    }

    /// Whether `want` needs elements added to or removed from the graph, as
    /// opposed to a property write on what is already linked.
    fn needs_relink(&self, want: &Normalization) -> bool {
        want.enabled != self.installed.enabled
            || (want.enabled && want.clip_protection != self.installed.clip_protection)
    }

    /// Rebuild the ReplayGain segment.
    ///
    /// CALLER CONTRACT: the pipeline is at `State::Null`. Never call this while
    /// Playing or Paused — that is what `pending` is for.
    ///
    /// rgvolume runs BEFORE Sparkamp's own volume/preamp so user volume stacks
    /// on top of normalization; rgvolume's own `pre-amp` stays at 0.
    fn apply_chain(&mut self, want: Normalization) -> Result<()> {
        let upstream = self.rg_upstream().clone();
        if let Some(rgv) = self.rg_volume.take() {
            upstream.unlink(&rgv);
            if let Some(rgl) = self.rg_limiter.take() {
                rgv.unlink(&rgl);
                rgl.unlink(&self.volume_elem);
                self.pipeline.remove(&rgl)?;
            } else {
                rgv.unlink(&self.volume_elem);
            }
            self.pipeline.remove(&rgv)?;
        } else {
            upstream.unlink(&self.volume_elem);
        }

        if want.enabled {
            if let Ok(rgv) = gst::ElementFactory::make("rgvolume").name("rgvol").build() {
                rgv.set_property("fallback-gain", want.fallback_db);
                rgv.set_property("album-mode", want.album_mode);
                self.pipeline.add(&rgv)?;
                upstream.link(&rgv)?;

                let tail = if want.clip_protection {
                    match gst::ElementFactory::make("rglimiter").name("rglim").build() {
                        Ok(rgl) => {
                            self.pipeline.add(&rgl)?;
                            rgv.link(&rgl)?;
                            self.rg_limiter = Some(rgl.clone());
                            rgl
                        }
                        // Limiter missing but rgvolume present: degrade to
                        // gain-without-limiting rather than no RG at all.
                        Err(_) => rgv.clone(),
                    }
                } else {
                    rgv.clone()
                };
                tail.link(&self.volume_elem)?;
                self.rg_volume = Some(rgv);
                self.installed = Normalization {
                    clip_protection: self.rg_limiter.is_some(),
                    ..want
                };
                return Ok(());
            }
            // rgvolume missing entirely → fall through to the direct link
            // (house rule: silent no-op when plugins are absent).
        }

        upstream.link(&self.volume_elem)?;
        self.installed = Normalization {
            enabled: false,
            clip_protection: false,
            ..want
        };
        Ok(())
    }

    /// Push the two properties that can change without touching the graph.
    fn write_live_properties(&mut self, want: Normalization) {
        if let Some(ref rgv) = self.rg_volume {
            rgv.set_property("fallback-gain", want.fallback_db);
            rgv.set_property("album-mode", want.album_mode);
        }
        self.installed = Normalization {
            enabled: self.rg_volume.is_some(),
            clip_protection: self.rg_limiter.is_some(),
            ..want
        };
    }

    /// Leave the current disc session, if there is one, and release the
    /// exclusive-read guard it took.
    ///
    /// One place rather than three, because there are three ways out of a disc
    /// session — stopping, loading something that is not on a disc, and
    /// dropping the backend — and the third one is the one that leaked: the
    /// count stayed up for the rest of the process, so disc detection never
    /// polled again, with no error anywhere to say why.
    fn release_disc_guard(&mut self) {
        if let Ok(mut slot) = self.cdda_device.lock() {
            *slot = None;
        }
        if std::mem::replace(&mut self.holds_disc_guard, false) {
            crate::disc::detect::end_exclusive_read();
        }
    }

    fn handle_spectrum_message(&self, structure: &gst::StructureRef) {
        let Some(data) = self.extract_magnitude_as_vec(structure) else {
            return;
        };
        // The min/max→0..1 rescale lives in the tap, not here, so a second
        // backend computing its own FFT normalizes identically instead of
        // plausibly.
        self.tap.push_magnitudes_db(&data);
    }

    /// The spectrum element sends magnitude as a `GST_TYPE_LIST` of
    /// `G_TYPE_FLOAT`, which the safe bindings cannot walk.
    fn extract_magnitude_as_vec(&self, structure: &gst::StructureRef) -> Option<Vec<f64>> {
        use gst::glib::translate::ToGlibPtr;

        unsafe {
            let field_value = structure.value("magnitude").map_err(|_| ()).ok()?;
            let list_gvalue_ptr = field_value.to_glib_none().0;

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
                let float_val = gst::glib::gobject_ffi::g_value_get_float(value_ptr);
                result.push(float_val as f64);
            }

            if result.is_empty() {
                return None;
            }
            Some(result)
        }
    }

    /// The audible level as the volume element actually holds it.
    #[cfg(test)]
    pub(crate) fn output_volume(&self) -> f64 {
        self.volume_elem.property::<f64>("volume")
    }
}

impl AudioBackend for GstBackend {
    fn open(tap: AnalysisTap) -> Result<Self> {
        let sink = gst::ElementFactory::make("autoaudiosink")
            .name("sink")
            .build()
            .context(
                "Failed to create audio sink. Ensure GStreamer audio output plugins are installed.",
            )?;
        Self::open_with_sink(tap, sink)
    }

    fn capabilities(&self) -> Capabilities {
        self.caps
    }

    fn load(&mut self, source: &MediaSource) -> Result<()> {
        // Null tears down the current pipeline (flushes buffers, releases the
        // audio device) so the new source starts clean, and is the only safe
        // moment to reshape the ReplayGain segment.
        self.pipeline.set_state(gst::State::Null)?;
        if let Some(want) = self.pending.take() {
            let _ = self.apply_chain(want);
        }

        let uri = match source {
            MediaSource::CdTrack { track, device } => {
                if let Ok(mut slot) = self.cdda_device.lock() {
                    *slot = device.clone();
                }
                // From here until stop() (or the next non-disc load) the drive
                // belongs to the pipeline's streaming read — silence every
                // detection poll BEFORE the source opens the device (a status
                // ioctl mid-stream faults flaky drives and wedges the open in
                // endless retries). The guard is refcounted, so back-to-back
                // disc loads with no `stop()` between them (advancing tracks
                // on the same disc) must not `begin` again — that would leave
                // the count one too high after the eventual single `end`,
                // wedging polling off even once playback actually stops.
                if !self.holds_disc_guard {
                    crate::disc::detect::begin_exclusive_read();
                    self.holds_disc_guard = true;
                }
                format!("cdda://{track}")
            }
            MediaSource::Uri(uri) => {
                // Release whatever the previous track held before deciding
                // about this one, so the count is balanced either way.
                self.release_disc_guard();
                // A track that lives on a mounted optical volume is every bit
                // as much a streaming read off the drive as a `cdda://` URI —
                // it is just reached through the filesystem. macOS plays audio
                // CDs this way (the mounted `.aiff` per track), so without this
                // the guard stayed DOWN for the whole of CD playback there, and
                // the disc poll went on issuing `drutil status` into the middle
                // of the read every ten seconds.
                // Through glib rather than trimming the scheme by hand: a mount
                // path like `/Volumes/Audio CD 1` arrives percent-encoded, and
                // a literal prefix test against the raw URI would never match.
                if let Ok((path, _)) = gst::glib::filename_from_uri(uri) {
                    if crate::disc::detect::path_is_on_optical_media(&path) {
                        crate::disc::detect::begin_exclusive_read();
                        self.holds_disc_guard = true;
                    }
                }
                uri.clone()
            }
        };

        self.decodebin.set_property("uri", &uri);
        Ok(())
    }

    fn set_state(&mut self, state: PlayerState) -> Result<()> {
        match state {
            PlayerState::Playing => {
                self.pipeline.set_state(gst::State::Playing)?;
            }
            PlayerState::Paused => {
                self.pipeline.set_state(gst::State::Paused)?;
            }
            PlayerState::Stopped => {
                self.pipeline.set_state(gst::State::Null)?;
                // Null released the device — detection polling may resume.
                self.release_disc_guard();
            }
        }
        Ok(())
    }

    fn seek(&mut self, to: Duration) -> Result<()> {
        let time = gst::ClockTime::from_nseconds(to.as_nanos() as u64);
        self.pipeline
            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, time)?;
        Ok(())
    }

    fn timeline(&self) -> Timeline {
        Timeline {
            position: self
                .pipeline
                .query_position::<gst::ClockTime>()
                .map(|t| Duration::from_nanos(t.nseconds())),
            duration: self
                .pipeline
                .query_duration::<gst::ClockTime>()
                .map(|t| Duration::from_nanos(t.nseconds())),
        }
    }

    fn poll_event(&mut self) -> Option<BusEvent> {
        use gst::MessageView;
        let bus = self.pipeline.bus()?;

        // Drain everything pending so no stale message survives between ticks,
        // stopping at the first transport event — the rest keep their place in
        // the queue for the next call.
        while let Some(msg) = bus.timed_pop(gst::ClockTime::ZERO) {
            match msg.view() {
                MessageView::Eos(..) => return Some(BusEvent::Eos),
                MessageView::Error(_) => return Some(BusEvent::Error),
                MessageView::Element(elem) => {
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

    fn set_output_gain(&mut self, gain: Amplitude) {
        // GStreamer's volume element passes 1.0 through unattenuated, so unity
        // is unity with nothing to cancel.
        self.volume_elem.set_property("volume", gain.get());
    }

    fn set_eq(&mut self, curve: &EqCurve) {
        let Some(eq) = self.eq.as_ref() else {
            return;
        };
        for (band, gain_db) in curve.as_db().iter().enumerate() {
            eq.set_property(&format!("band{band}"), *gain_db);
        }
    }

    fn set_normalization(&mut self, want: Normalization) -> Applied {
        if !self.caps.normalization {
            return Applied::Now;
        }
        if !self.needs_relink(&want) {
            self.pending = None;
            self.write_live_properties(want);
            return Applied::Now;
        }
        if self.pipeline.current_state() == gst::State::Null {
            // Already Null in every path that reaches here; the set_state is
            // belt-and-suspenders against a pipeline mid-teardown.
            let _ = self.pipeline.set_state(gst::State::Null);
            let _ = self.apply_chain(want);
            self.pending = None;
            Applied::Now
        } else {
            self.pending = Some(want);
            Applied::AtNextLoad
        }
    }
}

impl Drop for GstBackend {
    /// Dropping mid-disc is the third way out of a disc session, alongside
    /// stopping and loading something else, and it used to be the one that
    /// leaked.
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        self.release_disc_guard();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::backend::Analysis;

    /// Init GStreamer, then build a backend — but only when the ReplayGain
    /// plugin is present (returns None to skip in plugin-less environments).
    /// The capability probe needs gst initialized, so init MUST come first.
    fn rg_backend() -> Option<GstBackend> {
        gst::init().unwrap();
        let analysis = Analysis::new(64, 8192);
        let backend = GstBackend::open(analysis.tap()).unwrap();
        if !backend.capabilities().normalization {
            return None;
        }
        Some(backend)
    }

    /// Peer-check helper: element A's src pad must feed element B's sink.
    fn feeds(a: &gst::Element, b: &gst::Element) -> bool {
        a.static_pad("src")
            .and_then(|p| p.peer())
            .map(|peer| peer.parent_element().as_ref() == Some(b))
            .unwrap_or(false)
    }

    fn want(enabled: bool, clip_protection: bool, fallback_db: f64) -> Normalization {
        Normalization {
            enabled,
            clip_protection,
            fallback_db,
            album_mode: false,
        }
    }

    #[test]
    fn rg_chain_full_shape() {
        let Some(mut b) = rg_backend() else {
            return;
        };
        assert_eq!(b.set_normalization(want(true, true, -6.0)), Applied::Now);
        let rgv = b.pipeline.by_name("rgvol").expect("rgvolume inserted");
        let rgl = b.pipeline.by_name("rglim").expect("rglimiter inserted");
        assert!(feeds(&rgv, &rgl));
        assert!(feeds(&rgl, &b.volume_elem));
        assert_eq!(rgv.property::<f64>("fallback-gain"), -6.0);
    }

    #[test]
    fn rg_chain_no_limiter_shape() {
        let Some(mut b) = rg_backend() else {
            return;
        };
        assert_eq!(b.set_normalization(want(true, false, 0.0)), Applied::Now);
        let rgv = b.pipeline.by_name("rgvol").expect("rgvolume inserted");
        assert!(b.pipeline.by_name("rglim").is_none());
        assert!(feeds(&rgv, &b.volume_elem));
    }

    #[test]
    fn rg_disable_restores_direct_link() {
        let Some(mut b) = rg_backend() else {
            return;
        };
        assert_eq!(b.set_normalization(want(true, true, -6.0)), Applied::Now);
        assert_eq!(b.set_normalization(want(false, false, -6.0)), Applied::Now);
        assert!(b.pipeline.by_name("rgvol").is_none());
        assert!(b.pipeline.by_name("rglim").is_none());
        let up = b.rg_upstream().clone();
        assert!(feeds(&up, &b.volume_elem));
    }

    #[test]
    fn rg_album_mode_is_live_no_rebuild() {
        let Some(mut b) = rg_backend() else {
            return;
        };
        assert_eq!(b.set_normalization(want(true, false, 0.0)), Applied::Now);
        let rgv = b.pipeline.by_name("rgvol").unwrap();

        let album = Normalization {
            album_mode: true,
            ..want(true, false, 0.0)
        };
        assert_eq!(b.set_normalization(album), Applied::Now);
        assert!(rgv.property::<bool>("album-mode"));
        assert_eq!(
            b.pipeline.by_name("rgvol").as_ref(),
            Some(&rgv),
            "the same element, not a rebuilt one"
        );

        assert_eq!(b.set_normalization(want(true, false, 0.0)), Applied::Now);
        assert!(!rgv.property::<bool>("album-mode"));
    }

    /// The mechanism half of the deferral rule, against a pipeline that is
    /// really PLAYING.
    ///
    /// `fakesink sync=false` is what makes that affordable: no audio device is
    /// involved, so this runs anywhere the plugins are installed. The state the
    /// deferral turns on is read off the pipeline, not off a field a test set.
    #[test]
    fn rg_relink_defers_until_a_playing_pipeline_reloads() {
        gst::init().unwrap();
        if gst::ElementFactory::find("rgvolume").is_none() {
            return;
        }
        let Some((mut b, _analysis)) = fakesink_backend() else {
            return;
        };

        let fixture = wav_fixture(0);
        let source = file_source(&fixture);

        b.load(&source).unwrap();
        b.set_state(PlayerState::Playing).unwrap();
        let (change, current, _) = b.pipeline.state(gst::ClockTime::from_seconds(10));
        assert!(change.is_ok(), "the fixture pipeline must reach PLAYING");
        assert_eq!(current, gst::State::Playing);

        assert_eq!(
            b.set_normalization(want(true, true, -6.0)),
            Applied::AtNextLoad
        );
        assert!(
            b.pipeline.by_name("rgvol").is_none(),
            "must not relink a running pipeline"
        );

        b.load(&source).unwrap();
        assert!(
            b.pipeline.by_name("rgvol").is_some(),
            "the next load applies what was deferred"
        );
        b.set_state(PlayerState::Stopped).unwrap();
    }

    /// Half a second of 16-bit mono PCM at a constant `sample`, written at test
    /// time so no binary fixture is committed.
    ///
    /// Constant rather than a tone: `WaveformBuffer::get_samples` resamples and
    /// then runs a 5-tap moving average, and a constant is the one signal that
    /// survives both unchanged, so a capture assertion cannot be washed out by
    /// smoothing.
    fn wav_fixture(sample: i16) -> tempfile::NamedTempFile {
        const SAMPLE_RATE: u32 = 44_100;
        const CHANNELS: u16 = 1;
        const BITS: u16 = 16;
        let data_len = SAMPLE_RATE / 2 * CHANNELS as u32 * (BITS / 8) as u32;

        let mut wav = Vec::with_capacity(44 + data_len as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&CHANNELS.to_le_bytes());
        wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        wav.extend_from_slice(&(SAMPLE_RATE * CHANNELS as u32 * (BITS / 8) as u32).to_le_bytes());
        wav.extend_from_slice(&(CHANNELS * (BITS / 8)).to_le_bytes());
        wav.extend_from_slice(&BITS.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for _ in 0..data_len / 2 {
            wav.extend_from_slice(&sample.to_le_bytes());
        }

        let file = tempfile::NamedTempFile::with_suffix(".wav").unwrap();
        std::fs::write(file.path(), &wav).unwrap();
        file
    }

    /// A backend over `fakesink sync=false`, which reaches PLAYING with no
    /// audio device, plus the reader half of its visualizer buffers.
    fn fakesink_backend() -> Option<(GstBackend, Analysis)> {
        gst::init().unwrap();
        let sink = gst::ElementFactory::make("fakesink")
            .name("sink")
            .property("sync", false)
            .build()
            .ok()?;
        let analysis = Analysis::new(64, 8192);
        let backend = GstBackend::open_with_sink(analysis.tap(), sink).ok()?;
        Some((backend, analysis))
    }

    fn file_source(fixture: &tempfile::NamedTempFile) -> MediaSource {
        MediaSource::Uri(
            gst::glib::filename_to_uri(fixture.path(), None)
                .unwrap()
                .to_string(),
        )
    }

    /// The band gains must reach a real `equalizer-10bands`.
    ///
    /// Untestable until now: the EQ element was forced to `None` under
    /// `cfg(test)`, so every test in the tree ran against a player whose
    /// equalizer did not exist and only the shadow copy was ever observable.
    #[test]
    fn eq_bands_reach_a_real_equalizer_element() {
        gst::init().unwrap();
        let mut b = GstBackend::open(Analysis::new(64, 8192).tap()).unwrap();
        if !b.capabilities().eq {
            return;
        }

        b.set_eq(&EqCurve::FLAT.with_band(0, 6.0).with_band(9, -6.0));

        let eq = b.pipeline.by_name("equalizer").expect("equalizer inserted");
        assert!((eq.property::<f64>("band0") - 6.0).abs() < 1e-6);
        assert!((eq.property::<f64>("band9") + 6.0).abs() < 1e-6);
        assert_eq!(eq.property::<f64>("band4"), 0.0, "untouched bands stay flat");
    }

    /// Audio that flows through the pipeline must reach the waveform ring.
    ///
    /// The pad probe that does this was compiled out under `cfg(test)`, which
    /// left `WaveformBuffer::push_samples` with no caller in a test build and
    /// forced a dead-code allow on it. This is the assertion that was missing.
    #[test]
    fn the_waveform_probe_captures_pcm_from_a_playing_pipeline() {
        let Some((mut b, analysis)) = fakesink_backend() else {
            return;
        };

        let fixture = wav_fixture(8_000);
        b.load(&file_source(&fixture)).unwrap();
        b.set_state(PlayerState::Playing).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut reached_eos = false;
        while std::time::Instant::now() < deadline {
            if b.poll_event() == Some(BusEvent::Eos) {
                reached_eos = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(reached_eos, "the fixture should play to its end");
        b.set_state(PlayerState::Stopped).unwrap();

        let captured = analysis.waveform(64);
        let expected = 8_000.0 / 32_768.0;
        assert!(
            captured.iter().all(|s| (s - expected).abs() < 0.01),
            "the probe fed the ring the samples that were decoded: {captured:?}"
        );
    }
}
