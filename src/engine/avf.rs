//! The AVFoundation adapter.
//!
//! Everything below the seam that knows what an `AVAudioNode` is. The audio
//! path it builds:
//!
//! ```text
//! AVAudioPlayerNode → AVAudioUnitEQ → mainMixerNode → output
//!                   ↑
//!                   tap (PCM + FFT)
//! ```
//!
//! Three shapes here are not obvious and each comes from a measurement in
//! `docs/superpowers/plans/2026-08-31-macos-audio-backend-spike.md`:
//!
//! 1. **Output gain rides the EQ's `globalGain`, in dB, not the mixer's
//!    `outputVolume`.** `AVAudioMixerNode.outputVolume` is documented 0.0 to
//!    1.0, and this adapter needs to go above unity twice over: Sparkamp's
//!    pre-amp reaches 1.5x, and the pan-law compensation below is another
//!    +3.01 dB. `globalGain` runs -96 to +24 dB and has the headroom.
//! 2. **The pan-law compensation.** `mainMixerNode` applies an equal-power pan
//!    law to a mono input, which costs a flat 1/sqrt(2). GStreamer's `volume`
//!    passes 1.0 unattenuated, and the trait's contract says
//!    [`Amplitude::UNITY`] is the same audible level on both, so this adapter
//!    cancels it.
//! 3. **The tap sits on the player node, not the mixer.** GStreamer's probe
//!    hangs off `audioconvert`, upstream of both `volume` and
//!    `equalizer-10bands`, so the visualizer there does not react to the volume
//!    slider or the EQ. The player node's output bus is the same point in this
//!    graph, and tapping the mixer instead would make the two platforms' bars
//!    move differently for the same audio.
//!
//! The output stage is a construction parameter rather than always the audio
//! device, for the same reason [`crate::engine::gst::GstBackend`] takes its
//! sink: the pan-law compensation is only worth having if a test can measure
//! it, and measuring it means rendering without hardware.
//!
//! ## Not the default backend
//!
//! `DefaultBackend` is still `gst::GstBackend`. This adapter compiles and is
//! tested but nothing in the app constructs it yet; the switch happens behind
//! measured parity. That is why the module carries an `allow(dead_code)`: in
//! the binary target, where `mod engine` is private, every item here is
//! unreachable until that switch flips.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use block2::RcBlock;
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_avf_audio::{
    AVAudioEngine, AVAudioFile, AVAudioFormat, AVAudioMixerNode, AVAudioPCMBuffer,
    AVAudioPlayerNode, AVAudioPlayerNodeCompletionCallbackType, AVAudioTime, AVAudioUnitEQ,
    AVAudioUnitEQFilterType,
};
use objc2_foundation::{NSString, NSURL};
use rustfft::{FftPlanner, num_complex::Complex};

use crate::engine::backend::{
    Amplitude, AnalysisTap, Applied, AudioBackend, Capabilities, EqCurve, MediaSource,
    Normalization, Timeline,
};
use crate::engine::{BusEvent, PlayerState};

// ---------------------------------------------------------------------------
// Constants the spike fixed
// ---------------------------------------------------------------------------

/// The band centres `equalizer-10bands` declares, which the spike confirmed by
/// measurement for bands 1 through 8. An adapter may realise the edges as
/// shelves; it may not move a centre.
const BAND_FREQUENCIES_HZ: [f32; 10] = [
    29.0, 59.0, 119.0, 237.0, 474.0, 947.0, 1889.0, 3770.0, 7523.0, 15011.0,
];

/// One octave, because the bands are one octave apart. With this and shelves on
/// the edges the spike measured 0.13 dB RMS against GStreamer on mid bands;
/// widening or narrowing it is what made that number worse.
///
/// Applies to the eight parametric bands only. A plain `.lowShelf` or
/// `.highShelf` has no bandwidth parameter, which the spike's "bandwidth 1.0
/// octave" did not distinguish; see [`AvBackend::configure_eq_shape`].
const BAND_BANDWIDTH_OCTAVES: f32 = 1.0;

/// What `mainMixerNode` costs a mono input: 1/sqrt(2), or -3.01 dB, measured
/// flat across the whole spectrum. Expressed in dB because that is the unit
/// `globalGain` takes.
///
/// A stereo input is not panned, so it pays nothing. [`AvBackend::reconnect`]
/// picks between the two from the connection format's channel count, and
/// `unity_in_is_unity_out_on_both_a_mono_and_a_stereo_path` measures both
/// rather than reading this number back.
const MONO_PAN_LAW_DB: f64 = 3.010_299_956_639_812;

/// Bands pushed per spectrum frame. `spectrum bands=256` is what
/// [`crate::engine::gst`] configures, and matching it is what makes the two
/// backends' bars the same rather than merely both plausible.
const SPECTRUM_BANDS: usize = 256;

/// GStreamer's `spectrum` runs an FFT of twice its band count, so this one
/// does too: at 44.1 kHz both give 86 Hz per band.
const SPECTRUM_FFT_SIZE: usize = SPECTRUM_BANDS * 2;

/// `spectrum`'s default `threshold`. Without a floor a near-silent frame
/// rescales its own noise onto the full 0..1 range inside the tap and the bars
/// thrash.
const SPECTRUM_FLOOR_DB: f64 = -60.0;

/// Handed to `installTapOnBus:`, and advisory: the spike asked for 1024 and got
/// 4410 every time. Nothing downstream states a size.
const TAP_BUFFER_SIZE: u32 = 1024;

/// The format the graph is connected with before any file is loaded, so that
/// `set_eq` and `set_output_gain` have somewhere to write from the moment
/// [`AudioBackend::open`] returns. Replaced by the file's own processing format
/// at the first [`AudioBackend::load`].
const DEFAULT_SAMPLE_RATE: f64 = 44_100.0;

// ---------------------------------------------------------------------------
// Where the engine renders
// ---------------------------------------------------------------------------

/// The engine's output stage.
///
/// `Offline` exists because the pan-law compensation is a claim about a
/// measured level, and a claim about a level that no test can measure is a
/// comment. `renderOffline:toBuffer:error:` is not generated in
/// `objc2-avf-audio` 0.3.2, so the render itself goes through
/// `manualRenderingBlock`.
pub enum Output {
    /// The system's current output device.
    Device,
    /// No device at all: the caller pulls frames with
    /// [`AvBackend::render_offline`].
    Offline {
        sample_rate: f64,
        channels: u32,
        max_frames: u32,
    },
}

/// The file currently loaded, and the two numbers `timeline()` needs from it.
struct LoadedFile {
    file: Retained<AVAudioFile>,
    frames: i64,
    sample_rate: f64,
}

// ---------------------------------------------------------------------------
// The backend
// ---------------------------------------------------------------------------

/// The AVFoundation audio path and the state that only means something to it.
///
/// Not `Send`: `Retained` is not, and neither is `Player`.
pub struct AvBackend {
    engine: Retained<AVAudioEngine>,
    player: Retained<AVAudioPlayerNode>,
    eq: Retained<AVAudioUnitEQ>,
    mixer: Retained<AVAudioMixerNode>,

    file: Option<LoadedFile>,
    state: PlayerState,

    /// The frame the current `scheduleSegment` started at. The player's own
    /// sample clock restarts at zero on every `stop()`, so a seek's offset has
    /// to be carried here or position jumps back to the seek point's start.
    segment_start_frame: i64,

    /// The last position `timeline()` could read, in frames.
    ///
    /// `playerTimeForNodeTime:` returns nil whenever the player is not playing,
    /// so without this the position would drop to nothing the instant a track
    /// is paused. Behind a `Mutex` only because `timeline()` takes `&self`.
    last_position_frames: Mutex<i64>,

    /// What `Player` last asked for, kept so that a normalization change can
    /// recompute the total without `Player` pushing the gain again.
    gain: Amplitude,
    normalization: Normalization,
    /// dB to add for the mixer's pan law: [`MONO_PAN_LAW_DB`] on a mono
    /// connection, nothing on a stereo one.
    pan_law_db: f64,

    /// Filled by the completion handler, which fires on one of AVFoundation's
    /// own queues, and drained by `poll_event` on the caller's thread.
    events: Arc<Mutex<VecDeque<BusEvent>>>,
    /// Bumped by anything that invalidates what is scheduled. A completion
    /// handler whose captured generation no longer matches was cancelled rather
    /// than finished, and must not post an EOS.
    generation: Arc<Mutex<u64>>,

    /// The tap block, kept alive for as long as the tap is installed.
    tap_block: Option<RcBlock<dyn Fn(NonNull<AVAudioPCMBuffer>, NonNull<AVAudioTime>)>>,

    caps: Capabilities,
}

impl AvBackend {
    /// Build the audio path with `output` as its output stage.
    pub fn open_with_output(tap: AnalysisTap, output: Output) -> Result<Self> {
        // SAFETY: every call below is an ordinary Objective-C message to an
        // object this function owns. The unsafe is objc2's blanket marking of
        // generated methods, not a claim about aliasing.
        unsafe {
            let engine = AVAudioEngine::new();

            // Manual rendering has to be enabled before mainMixerNode is
            // touched: reading that property is what configures the hardware
            // for device rendering, and doing it first makes the later
            // enableManualRenderingMode fail.
            if let Output::Offline {
                sample_rate,
                channels,
                max_frames,
            } = output
            {
                let format = AVAudioFormat::initStandardFormatWithSampleRate_channels(
                    AVAudioFormat::alloc(),
                    sample_rate,
                    channels,
                )
                .ok_or_else(|| anyhow!("AVAudioFormat rejected {channels} channels"))?;
                engine
                    .enableManualRenderingMode_format_maximumFrameCount_error(
                        objc2_avf_audio::AVAudioEngineManualRenderingMode::Offline,
                        &format,
                        max_frames,
                    )
                    .map_err(|e| anyhow!("could not enable manual rendering: {e:?}"))?;
            }

            let player = AVAudioPlayerNode::new();
            let eq = AVAudioUnitEQ::initWithNumberOfBands(AVAudioUnitEQ::alloc(), 10);
            let mixer = engine.mainMixerNode();

            engine.attachNode(&player);
            engine.attachNode(&eq);

            let mut backend = AvBackend {
                engine,
                player,
                eq,
                mixer,
                file: None,
                state: PlayerState::Stopped,
                segment_start_frame: 0,
                last_position_frames: Mutex::new(0),
                gain: Amplitude::UNITY,
                normalization: Normalization {
                    enabled: false,
                    clip_protection: false,
                    fallback_db: 0.0,
                    album_mode: false,
                },
                pan_law_db: 0.0,
                events: Arc::new(Mutex::new(VecDeque::new())),
                generation: Arc::new(Mutex::new(0)),
                tap_block: None,
                caps: Capabilities {
                    eq: true,
                    spectrum: true,
                    // A plain gain, honestly bounded: see set_normalization.
                    normalization: true,
                },
            };

            backend.configure_eq_shape();
            // Stereo until a file says otherwise, which is also the shape that
            // pays no pan law.
            let format = AVAudioFormat::initStandardFormatWithSampleRate_channels(
                AVAudioFormat::alloc(),
                DEFAULT_SAMPLE_RATE,
                2,
            )
            .context("AVAudioFormat rejected 44.1 kHz stereo")?;
            backend.reconnect(&format);
            backend.install_tap(tap);
            backend.set_output_gain(Amplitude::UNITY);

            Ok(backend)
        }
    }

    /// Set every band's filter type, centre and bandwidth. These never change
    /// again; only the gains do, from [`AudioBackend::set_eq`].
    ///
    /// Shelves on the edges because that is what GStreamer's bands 0 and 9
    /// measure as, and because it halved band 9's error in the spike (1.86 dB
    /// RMS to 0.55). Band 0 deliberately does **not** chase GStreamer, whose
    /// Direct Form biquad at a centre 1520x below the sample rate cuts 2.8 dB
    /// at 50 Hz when asked to boost at 29 Hz. That is a defect, and this ships
    /// the correct shelf instead.
    fn configure_eq_shape(&mut self) {
        unsafe {
            let bands = self.eq.bands();
            for i in 0..10 {
                let band = bands.objectAtIndex(i);
                let is_shelf = i == 0 || i == 9;
                band.setFilterType(if is_shelf {
                    if i == 0 {
                        AVAudioUnitEQFilterType::LowShelf
                    } else {
                        AVAudioUnitEQFilterType::HighShelf
                    }
                } else {
                    AVAudioUnitEQFilterType::Parametric
                });
                band.setFrequency(BAND_FREQUENCIES_HZ[i]);
                if !is_shelf {
                    // Bandwidth is a parameter of a parametric filter and not
                    // of a plain shelf. Written to a shelf it does not stick:
                    // across 40 fresh engines the two shelf bands read back
                    // something other than what was written 41 times out of 80,
                    // while the eight parametric bands were exact 320 times out
                    // of 320. So it is written where it means something and
                    // nowhere else.
                    band.setBandwidth(BAND_BANDWIDTH_OCTAVES);
                }
                band.setGain(0.0);
                band.setBypass(false);
            }
        }
    }

    /// Relink player → EQ → mixer at `format`, and recompute what the mixer's
    /// pan law will cost at that channel count.
    ///
    /// Called again on every load because a file's processing format is the
    /// format the player node has to be connected with.
    fn reconnect(&mut self, format: &AVAudioFormat) {
        unsafe {
            self.engine
                .connect_to_format(&self.player, &self.eq, Some(format));
            self.engine
                .connect_to_format(&self.eq, &self.mixer, Some(format));
            self.pan_law_db = if format.channelCount() == 1 {
                MONO_PAN_LAW_DB
            } else {
                0.0
            };
        }
        // The compensation just changed; push the level that accounts for it.
        self.write_gain();
    }

    /// Install the PCM/FFT tap on the player node's output bus.
    fn install_tap(&mut self, tap: AnalysisTap) {
        // Behind a Mutex because the tap fires on one of AVFoundation's own
        // threads and an Objective-C block must be `Fn`, not `FnMut`. It is
        // uncontended: only this block ever locks it.
        let analyzer = Mutex::new(SpectrumAnalyzer::new());
        let block = RcBlock::new(
            move |buffer: NonNull<AVAudioPCMBuffer>, _when: NonNull<AVAudioTime>| {
                // SAFETY: AVFoundation hands the tap a live buffer that
                // outlives this call, and `mono_samples` only reads
                // `frameLength` frames of channel 0.
                let mono = unsafe { mono_samples(buffer.as_ref()) };
                if mono.is_empty() {
                    return;
                }
                // Whatever length arrived, pushed as it arrived: the tap
                // absorbs any size and there is no correct one.
                tap.push_pcm(&mono);
                let frame = analyzer.lock().ok().and_then(|mut a| a.feed(&mono));
                if let Some(db) = frame {
                    tap.push_magnitudes_db(&db);
                }
            },
        );
        unsafe {
            // A None format means the bus's own, which is what this wants: the
            // connection format is already the file's.
            self.player.installTapOnBus_bufferSize_format_block(
                0,
                TAP_BUFFER_SIZE,
                None,
                RcBlock::as_ptr(&block),
            );
        }
        self.tap_block = Some(block);
    }

    /// Push `gain`, the normalization offset and the pan-law compensation as
    /// one dB figure onto the EQ's global gain.
    ///
    /// One writer for all three, so the composition cannot end up applied twice
    /// or half-applied.
    fn write_gain(&mut self) {
        let normalization_db = if self.normalization.enabled {
            self.normalization.fallback_db
        } else {
            0.0
        };
        let linear = self.gain.get();
        // Amplitude clamps at zero, and log10(0) is -inf. -96 dB is
        // globalGain's floor and is inaudible.
        let gain_db = if linear > 0.0 {
            20.0 * linear.log10()
        } else {
            -96.0
        };
        let total = (gain_db + normalization_db + self.pan_law_db).clamp(-96.0, 24.0);
        unsafe { self.eq.setGlobalGain(total as f32) };
    }

    /// Point the player at `from` and schedule to the end of the file.
    ///
    /// Every caller has already invalidated the previous schedule, so the
    /// generation captured here is the one a completion handler must still see
    /// for its EOS to be real.
    fn schedule_from(&mut self, from: i64) -> Result<()> {
        let Some(loaded) = self.file.as_ref() else {
            bail!("nothing loaded to schedule");
        };
        let remaining = loaded.frames.saturating_sub(from);
        if remaining <= 0 {
            // Seeking to or past the end: nothing to schedule, and the track is
            // over. Report it the way a finished track is reported.
            self.push_event(BusEvent::Eos);
            return Ok(());
        }

        let frames_to_play = remaining.min(u32::MAX as i64) as u32;
        let generation = self.generation.lock().map(|g| *g).unwrap_or(0);
        let events = Arc::clone(&self.events);
        let generation_at_schedule = Arc::clone(&self.generation);
        let completion = RcBlock::new(move |_kind: AVAudioPlayerNodeCompletionCallbackType| {
            // A stop() or a seek fires this handler too. Only the schedule that
            // is still current can have reached its end.
            let current = generation_at_schedule
                .lock()
                .map(|g| *g)
                .unwrap_or(generation);
            if current != generation {
                return;
            }
            if let Ok(mut q) = events.lock() {
                q.push_back(BusEvent::Eos);
            }
        });

        unsafe {
            self.player
                .scheduleSegment_startingFrame_frameCount_atTime_completionCallbackType_completionHandler(
                    &loaded.file,
                    from,
                    frames_to_play,
                    None,
                    // DataPlayedBack is documented as device-rendering only,
                    // which would leave offline renders with no EOS at all.
                    // DataRendered fires as soon as the player has produced the
                    // frames, which is the same instant on both paths bar the
                    // device's own buffer.
                    AVAudioPlayerNodeCompletionCallbackType::DataRendered,
                    RcBlock::as_ptr(&completion),
                );
        }
        self.segment_start_frame = from;
        self.set_last_position(from);
        Ok(())
    }

    /// Invalidate whatever is scheduled, so its completion handler stops
    /// counting as an end-of-track.
    fn invalidate_schedule(&mut self) {
        if let Ok(mut g) = self.generation.lock() {
            *g = g.wrapping_add(1);
        }
    }

    fn push_event(&self, event: BusEvent) {
        if let Ok(mut q) = self.events.lock() {
            q.push_back(event);
        }
    }

    fn set_last_position(&self, frames: i64) {
        if let Ok(mut p) = self.last_position_frames.lock() {
            *p = frames;
        }
    }

    /// Start the engine if it is not already running.
    fn start_engine(&mut self) -> Result<()> {
        unsafe {
            if self.engine.isRunning() {
                return Ok(());
            }
            self.engine
                .startAndReturnError()
                .map_err(|e| anyhow!("AVAudioEngine would not start: {e:?}"))
        }
    }

    /// Pull `frames` frames through the graph without an audio device, and
    /// return channel 0.
    ///
    /// The engine must have been opened with [`Output::Offline`] and started.
    /// Returns fewer frames than asked for only at the end of the source.
    pub fn render_offline(&self, frames: u32) -> Result<Vec<f32>> {
        unsafe {
            let format = self.engine.manualRenderingFormat();
            let buffer = AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
                AVAudioPCMBuffer::alloc(),
                &format,
                frames,
            )
            .context("could not allocate the render buffer")?;

            // `mutableAudioBufferList`'s documentation says its mDataByteSize
            // fields express frameCapacity. They do not: on a fresh buffer they
            // are zero, because they track frameLength, and the engine rejects
            // a zero-sized destination with paramErr (-50). Measured, not
            // guessed. Declaring the length first is what makes the render
            // legal.
            buffer.setFrameLength(frames);
            let list = buffer.mutableAudioBufferList();

            let block = self.engine.manualRenderingBlock();
            if block.is_null() {
                bail!("the engine is not in manual rendering mode");
            }
            let mut os_status: i32 = 0;
            let status = (*block).call((frames, list, &mut os_status as *mut i32));
            if status != objc2_avf_audio::AVAudioEngineManualRenderingStatus::Success {
                bail!("manual render returned {status:?}, OSStatus {os_status}");
            }

            // The render block writes the buffer list, not the AVAudioPCMBuffer
            // wrapper around it, so the count of frames it actually produced
            // has to be read back off the list.
            let bytes = list.as_ref().mBuffers[0].mDataByteSize as usize;
            let rendered = bytes / size_of::<f32>();
            let channels = buffer.floatChannelData();
            if channels.is_null() || rendered == 0 {
                return Ok(Vec::new());
            }
            let stride = buffer.stride() as usize;
            let left = (*channels).as_ptr();
            Ok((0..rendered).map(|i| *left.add(i * stride)).collect())
        }
    }

    /// The gain the EQ is actually carrying, in dB — everything
    /// [`Self::write_gain`] composed, read back off the audio unit.
    pub fn global_gain_db(&self) -> f32 {
        unsafe { self.eq.globalGain() }
    }

    /// The EQ node, so a test can read back the band configuration this adapter
    /// installed rather than the copy it believes it installed.
    pub fn eq_unit(&self) -> &AVAudioUnitEQ {
        &self.eq
    }
}

impl AudioBackend for AvBackend {
    fn open(tap: AnalysisTap) -> Result<Self> {
        Self::open_with_output(tap, Output::Device)
    }

    fn capabilities(&self) -> Capabilities {
        self.caps
    }

    fn load(&mut self, source: &MediaSource) -> Result<()> {
        let uri = match source {
            MediaSource::CdTrack { .. } => {
                // macOS mounts an audio CD as one AIFF per track, so a disc
                // reaches this adapter as a plain file URI and this arm should
                // never fire. AVFoundation has no raw CD-audio reader to fall
                // back on if it does.
                bail!(
                    "AVFoundation cannot read raw CD audio; macOS mounts audio CDs as files, \
                     so a disc track should arrive as a file URI"
                );
            }
            MediaSource::Uri(uri) => uri,
        };

        // Invalidate before stopping: stop() fires the outgoing completion
        // handler, and while the generation still matches that handler would
        // post an EOS for a track that was cancelled rather than finished.
        self.invalidate_schedule();
        unsafe {
            self.player.stop();
            self.engine.stop();
        }
        if let Ok(mut q) = self.events.lock() {
            q.clear();
        }
        self.file = None;
        self.state = PlayerState::Stopped;
        self.segment_start_frame = 0;
        self.set_last_position(0);

        let url = NSURL::URLWithString(&NSString::from_str(uri))
            .ok_or_else(|| anyhow!("not a URL: {uri}"))?;
        let (file, frames, sample_rate, format) = unsafe {
            let file = AVAudioFile::initForReading_error(AVAudioFile::alloc(), &url)
                .map_err(|e| anyhow!("could not open {uri}: {e:?}"))?;
            let format = file.processingFormat();
            (file.clone(), file.length(), format.sampleRate(), format)
        };
        if sample_rate <= 0.0 {
            bail!("{uri} reports a sample rate of {sample_rate}");
        }

        // The player node must be connected at the file's own processing
        // format, so this is also where the pan-law compensation is recomputed.
        self.reconnect(&format);
        self.file = Some(LoadedFile {
            file,
            frames,
            sample_rate,
        });
        self.schedule_from(0)
    }

    fn set_state(&mut self, state: PlayerState) -> Result<()> {
        if self.state == state {
            return Ok(());
        }
        match state {
            PlayerState::Playing => {
                if self.file.is_none() {
                    bail!("nothing loaded to play");
                }
                self.start_engine()?;
                unsafe { self.player.play() };
            }
            PlayerState::Paused => unsafe {
                self.player.pause();
                self.engine.pause();
            },
            PlayerState::Stopped => {
                // Releases the output device, and resets the player's sample
                // clock, so a later play() starts the track again from the top.
                self.invalidate_schedule();
                unsafe {
                    self.player.stop();
                    self.engine.stop();
                }
                if self.file.is_some() {
                    self.state = state;
                    // Leave the graph loaded and ready rather than empty, so
                    // play() after stop() plays the same track again. This is
                    // also what rewinds the position, since scheduling from
                    // frame 0 is what "back to the start" means here.
                    self.schedule_from(0)?;
                    return Ok(());
                }
            }
        }
        self.state = state;
        Ok(())
    }

    fn seek(&mut self, to: Duration) -> Result<()> {
        let Some(sample_rate) = self.file.as_ref().map(|f| f.sample_rate) else {
            bail!("nothing loaded to seek");
        };
        let frame = (to.as_secs_f64() * sample_rate).round().max(0.0) as i64;
        let was_playing = self.state == PlayerState::Playing;

        // stop() clears the scheduled segment and resets the player's sample
        // clock to zero, which is why segment_start_frame exists.
        self.invalidate_schedule();
        unsafe { self.player.stop() };
        self.schedule_from(frame)?;
        if was_playing {
            self.start_engine()?;
            unsafe { self.player.play() };
        }
        Ok(())
    }

    fn timeline(&self) -> Timeline {
        let Some(loaded) = self.file.as_ref() else {
            return Timeline::default();
        };
        let duration = Some(Duration::from_secs_f64(
            loaded.frames as f64 / loaded.sample_rate,
        ));

        // One snapshot. `sampleTime` counts frames the player has rendered
        // since the current schedule started, so the position is that plus the
        // frame the schedule started at, and nothing else is read.
        let played = unsafe {
            self.player
                .lastRenderTime()
                .and_then(|node_time| self.player.playerTimeForNodeTime(&node_time))
                .map(|player_time| player_time.sampleTime())
        };
        let frames = match played {
            Some(sample_time) => {
                let frames = (self.segment_start_frame + sample_time).clamp(0, loaded.frames);
                self.set_last_position(frames);
                frames
            }
            // Not playing: playerTimeForNodeTime is nil, and the last position
            // read is what the transport is still sitting at.
            None => self
                .last_position_frames
                .lock()
                .map(|p| *p)
                .unwrap_or(self.segment_start_frame),
        };

        Timeline {
            position: Some(Duration::from_secs_f64(frames as f64 / loaded.sample_rate)),
            duration,
        }
    }

    fn poll_event(&mut self) -> Option<BusEvent> {
        // Analysis is serviced on the tap's own thread, so there is nothing for
        // this call to catch up on beyond the queue itself.
        self.events.lock().ok()?.pop_front()
    }

    fn set_output_gain(&mut self, gain: Amplitude) {
        self.gain = gain;
        self.write_gain();
    }

    fn set_eq(&mut self, curve: &EqCurve) {
        unsafe {
            let bands = self.eq.bands();
            for (i, gain_db) in curve.as_db().iter().enumerate() {
                bands.objectAtIndex(i).setGain(*gain_db as f32);
            }
        }
    }

    fn set_normalization(&mut self, want: Normalization) -> Applied {
        // A plain gain, and only that. `fallback_db` is what `Player` resolved
        // for this track from the library's own measured gain, which is the
        // path Sparkamp actually uses, and folding it into the live gain stage
        // takes effect immediately.
        //
        // Two flags are honestly inert here and this is where that is recorded:
        // `clip_protection` has no limiter behind it, and `album_mode` has
        // nothing to choose between, because AVAudioFile does not surface a
        // stream's own REPLAYGAIN tags the way `rgvolume` reads them. Both are
        // stored so a later implementation diffs against the truth.
        self.normalization = want;
        self.write_gain();
        Applied::Now
    }
}

impl Drop for AvBackend {
    fn drop(&mut self) {
        unsafe {
            self.player.removeTapOnBus(0);
            self.player.stop();
            self.engine.stop();
        }
    }
}

// ---------------------------------------------------------------------------
// Capture
// ---------------------------------------------------------------------------

/// Channel 0 of `buffer` as f64, which is the mono trace both visualizer
/// consumers draw. The GStreamer probe takes the left channel the same way.
///
/// # Safety
/// `buffer` must be a live `AVAudioPCMBuffer` in a 32-bit float format.
unsafe fn mono_samples(buffer: &AVAudioPCMBuffer) -> Vec<f64> {
    unsafe {
        let frames = buffer.frameLength() as usize;
        let channels = buffer.floatChannelData();
        if frames == 0 || channels.is_null() {
            return Vec::new();
        }
        let stride = buffer.stride() as usize;
        let left = (*channels).as_ptr();
        (0..frames).map(|i| *left.add(i * stride) as f64).collect()
    }
}

/// One FFT frame's worth of the most recent audio, and the transform over it.
///
/// The window is a fixed 512 samples because that is what the transform needs,
/// which is a different thing from buffering PCM up to a "correct" size: the
/// samples reach [`AnalysisTap::push_pcm`] at whatever length they arrived in,
/// before this sees them at all.
struct SpectrumAnalyzer {
    fft: Arc<dyn rustfft::Fft<f32>>,
    /// Hamming, matching `spectrum`'s default `window-type`.
    window: Vec<f32>,
    /// The last [`SPECTRUM_FFT_SIZE`] samples seen, oldest first.
    recent: VecDeque<f32>,
    scratch: Vec<Complex<f32>>,
}

impl SpectrumAnalyzer {
    fn new() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(SPECTRUM_FFT_SIZE);
        let window = (0..SPECTRUM_FFT_SIZE)
            .map(|i| {
                let phase =
                    2.0 * std::f32::consts::PI * i as f32 / (SPECTRUM_FFT_SIZE as f32 - 1.0);
                0.54 - 0.46 * phase.cos()
            })
            .collect();
        SpectrumAnalyzer {
            fft,
            window,
            recent: VecDeque::with_capacity(SPECTRUM_FFT_SIZE),
            scratch: vec![Complex::new(0.0, 0.0); SPECTRUM_FFT_SIZE],
        }
    }

    /// Absorb a tap buffer of any length and return a magnitude frame in dB
    /// once there is a full window, or `None` while there is not.
    fn feed(&mut self, samples: &[f64]) -> Option<Vec<f64>> {
        for &s in samples {
            if self.recent.len() == SPECTRUM_FFT_SIZE {
                self.recent.pop_front();
            }
            self.recent.push_back(s as f32);
        }
        if self.recent.len() < SPECTRUM_FFT_SIZE {
            return None;
        }

        for (slot, (sample, window)) in self
            .scratch
            .iter_mut()
            .zip(self.recent.iter().zip(self.window.iter()))
        {
            *slot = Complex::new(sample * window, 0.0);
        }
        self.fft.process(&mut self.scratch);

        // Bins 1..=SPECTRUM_BANDS: bin 0 is DC, which no bar should show.
        // dB rather than linear magnitude because that is the unit the tap's
        // 0..1 rescale is defined in and the unit `spectrum` emits.
        Some(
            self.scratch[1..=SPECTRUM_BANDS]
                .iter()
                .map(|c| {
                    let magnitude = c.norm() as f64 / SPECTRUM_FFT_SIZE as f64;
                    if magnitude > 0.0 {
                        (20.0 * magnitude.log10()).max(SPECTRUM_FLOOR_DB)
                    } else {
                        SPECTRUM_FLOOR_DB
                    }
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EQ_BAND_DB_LIMIT;
    use crate::engine::backend::Analysis;
    use objc2_foundation::NSString;

    const RATE: f64 = 44_100.0;
    const TONE_HZ: f64 = 1_000.0;
    const TONE_AMPLITUDE: f64 = 0.5;
    /// One manual-render call's worth. Also the engine's maximumFrameCount.
    const CHUNK: u32 = 4_096;

    /// A one-second 1 kHz sine at [`TONE_AMPLITUDE`], 16-bit PCM, written at
    /// test time so no binary fixture is committed.
    ///
    /// A tone rather than a constant because this is measured as RMS through a
    /// filter chain, and 1 kHz sits where every band is flat.
    fn tone_fixture(channels: u16) -> tempfile::NamedTempFile {
        const BITS: u16 = 16;
        let frames = RATE as u32;
        let data_len = frames * channels as u32 * (BITS / 8) as u32;

        let mut wav = Vec::with_capacity(44 + data_len as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&(RATE as u32).to_le_bytes());
        wav.extend_from_slice(&(RATE as u32 * channels as u32 * (BITS / 8) as u32).to_le_bytes());
        wav.extend_from_slice(&(channels * (BITS / 8)).to_le_bytes());
        wav.extend_from_slice(&BITS.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for frame in 0..frames {
            let t = frame as f64 / RATE;
            let sample = ((TONE_AMPLITUDE * (2.0 * std::f64::consts::PI * TONE_HZ * t).sin())
                * 32767.0)
                .round() as i16;
            for _ in 0..channels {
                wav.extend_from_slice(&sample.to_le_bytes());
            }
        }

        let file = tempfile::NamedTempFile::with_suffix(".wav").unwrap();
        std::fs::write(file.path(), &wav).unwrap();
        file
    }

    fn file_source(fixture: &tempfile::NamedTempFile) -> MediaSource {
        let path = NSString::from_str(fixture.path().to_str().unwrap());
        let url = NSURL::fileURLWithPath(&path);
        MediaSource::Uri(url.absoluteString().unwrap().to_string())
    }

    /// A backend rendering to nothing at all, which is what makes these tests
    /// runnable without an audio device — the same trade `GstBackend`'s
    /// `fakesink` makes.
    fn offline_backend() -> (AvBackend, Analysis) {
        let analysis = Analysis::new(64, 65_536);
        let backend = AvBackend::open_with_output(
            analysis.tap(),
            Output::Offline {
                sample_rate: RATE,
                channels: 2,
                max_frames: CHUNK,
            },
        )
        .expect("an offline engine needs no hardware");
        (backend, analysis)
    }

    /// Poll `ready` until it holds, up to ten seconds.
    ///
    /// The tap block and the schedule's completion handler both run on
    /// AVFoundation's own queues, so `render_offline` returning says nothing
    /// about whether their effects have arrived. Asserting straight after the
    /// render failed four runs in ten under the suite's parallelism.
    fn wait_for(mut ready: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if ready() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    fn rms(samples: &[f32]) -> f64 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
        (sum / samples.len() as f64).sqrt()
    }

    /// Play `fixture` through the graph offline and return the RMS of the left
    /// output channel, skipping the first chunk so nothing is measured while
    /// the graph is still filling.
    fn measure_output_rms(backend: &mut AvBackend, fixture: &tempfile::NamedTempFile) -> f64 {
        backend.load(&file_source(fixture)).unwrap();
        backend.set_state(PlayerState::Playing).unwrap();

        let mut measured = Vec::new();
        for chunk in 0..8 {
            let rendered = backend.render_offline(CHUNK).unwrap();
            if chunk > 0 {
                measured.extend_from_slice(&rendered);
            }
        }
        rms(&measured)
    }

    /// The measurement that catches a real regression.
    ///
    /// `mainMixerNode` applies an equal-power pan law to a mono input and
    /// GStreamer's `volume` does not, so without compensation macOS ships a
    /// flat 3.01 dB quieter at the same user volume. This renders real audio
    /// through the real graph and compares levels; it never reads the
    /// compensation constant back.
    ///
    /// Both halves matter. Mono fails low if the compensation is dropped and
    /// high if the mixer stops applying the pan law. Stereo fails high if the
    /// compensation is applied unconditionally, which is the other way to get
    /// this wrong.
    #[test]
    fn unity_in_is_unity_out_on_both_a_mono_and_a_stereo_path() {
        // The RMS of the tone as written to the file, which is what
        // Amplitude::UNITY must preserve.
        let expected = TONE_AMPLITUDE / std::f64::consts::SQRT_2;

        for channels in [1u16, 2] {
            let (mut backend, _analysis) = offline_backend();
            backend.set_output_gain(Amplitude::UNITY);
            let fixture = tone_fixture(channels);
            let measured = measure_output_rms(&mut backend, &fixture);

            let error_db = 20.0 * (measured / expected).log10();
            assert!(
                error_db.abs() < 0.2,
                "{channels}-channel source at unity came out {error_db:+.2} dB off \
                 (measured RMS {measured:.4}, expected {expected:.4})"
            );
        }
    }

    /// The pan law is real, and it is mono-only.
    ///
    /// Without this the test above could pass on a build where AVFoundation
    /// applied no pan law and this adapter added no compensation. Measuring the
    /// mixer's own behaviour, with the compensation cancelled by an equal and
    /// opposite request, pins which of the two is happening.
    #[test]
    fn the_mixer_costs_a_mono_path_exactly_three_decibels() {
        let expected = TONE_AMPLITUDE / std::f64::consts::SQRT_2;

        // Asking for -3.01 dB cancels the +3.01 dB compensation, so what
        // reaches the mixer is the uncompensated signal.
        let (mut backend, _a) = offline_backend();
        backend.set_output_gain(Amplitude::linear(1.0 / std::f64::consts::SQRT_2));
        let mono = measure_output_rms(&mut backend, &tone_fixture(1));

        let uncompensated_db = 20.0 * (mono / expected).log10();
        assert!(
            (uncompensated_db + 3.01).abs() < 0.2,
            "an uncompensated mono path should measure -3.01 dB, \
             measured {uncompensated_db:+.2} dB"
        );
    }

    /// The band shape the spike settled on, read off the audio unit rather than
    /// off this adapter's own copy of it.
    #[test]
    fn ten_bands_sit_at_the_declared_centres_with_shelves_on_the_edges() {
        // Written out again rather than read back from this module's own
        // table: comparing a constant against itself passes whatever the
        // constant happens to say, and these ten numbers are the contract.
        const DECLARED_HZ: [f32; 10] = [
            29.0, 59.0, 119.0, 237.0, 474.0, 947.0, 1889.0, 3770.0, 7523.0, 15011.0,
        ];

        let (backend, _analysis) = offline_backend();
        let bands = unsafe { backend.eq_unit().bands() };

        assert_eq!(bands.count(), 10, "equalizer-10bands has ten bands");
        for i in 0..10 {
            let band = bands.objectAtIndex(i);
            unsafe {
                assert_eq!(
                    band.frequency(),
                    DECLARED_HZ[i],
                    "band {i} must sit where equalizer-10bands declares it"
                );
                assert_eq!(
                    band.filterType(),
                    match i {
                        0 => AVAudioUnitEQFilterType::LowShelf,
                        9 => AVAudioUnitEQFilterType::HighShelf,
                        _ => AVAudioUnitEQFilterType::Parametric,
                    },
                    "band {i}'s filter type"
                );
                if i != 0 && i != 9 {
                    // Only the parametric bands. A plain shelf has no
                    // bandwidth parameter, and reading one back off a shelf
                    // returns what was written only about half the time.
                    assert_eq!(
                        band.bandwidth(),
                        1.0,
                        "band {i} is one octave wide, because the bands are one octave apart"
                    );
                }
                assert!(!band.bypass(), "band {i} must be in circuit");
            }
        }
    }

    /// Gains cross the seam already clamped by [`EqCurve`], and this is where
    /// that clamp is confirmed to survive all the way into the audio unit.
    #[test]
    fn band_gains_reach_the_audio_unit_clamped_to_the_limit() {
        let (mut backend, _analysis) = offline_backend();
        backend.set_eq(
            &EqCurve::FLAT
                .with_band(0, 99.0)
                .with_band(9, -99.0)
                .with_band(4, 4.5),
        );

        let bands = unsafe { backend.eq_unit().bands() };
        unsafe {
            assert_eq!(bands.objectAtIndex(0).gain(), EQ_BAND_DB_LIMIT as f32);
            assert_eq!(bands.objectAtIndex(9).gain(), -EQ_BAND_DB_LIMIT as f32);
            assert_eq!(bands.objectAtIndex(4).gain(), 4.5);
            assert_eq!(
                bands.objectAtIndex(7).gain(),
                0.0,
                "untouched bands stay flat"
            );
        }
    }

    /// macOS mounts an audio CD as one file per track, so this arm should never
    /// fire; when it does, the error has to say why rather than fail silently.
    #[test]
    fn a_cd_track_is_refused_with_a_reason() {
        let (mut backend, _analysis) = offline_backend();
        let err = backend
            .load(&MediaSource::CdTrack {
                track: "3".to_string(),
                device: Some("/dev/sr0".to_string()),
            })
            .expect_err("AVFoundation has no raw CD-audio reader");
        let message = err.to_string();
        assert!(
            message.contains("CD"),
            "the error must name what it cannot do: {message}"
        );
    }

    /// Audio flowing through the graph must reach both visualizer buffers,
    /// whatever length AVFoundation chose to deliver it in.
    ///
    /// Loading twice, at two channel counts, on purpose. Every load relinks the
    /// player node at the new file's processing format, and the tap is
    /// installed once at `open` and never reinstalled — so this is where a
    /// reconnect silently detaching it would show up.
    #[test]
    fn the_tap_feeds_both_visualizers_across_a_format_change() {
        let (mut backend, analysis) = offline_backend();

        for channels in [1u16, 2] {
            analysis.clear();
            let fixture = tone_fixture(channels);
            backend.load(&file_source(&fixture)).unwrap();
            backend.set_state(PlayerState::Playing).unwrap();
            for _ in 0..8 {
                backend.render_offline(CHUNK).unwrap();
            }

            assert!(
                wait_for(|| analysis.waveform(256).iter().any(|s| s.abs() > 0.1)),
                "the tap fed the ring the {channels}-channel tone that was rendered"
            );
            assert!(
                wait_for(|| analysis.has_magnitudes()),
                "the adapter's own FFT reached the spectrum buffer at {channels} channels"
            );
            backend.set_state(PlayerState::Stopped).unwrap();
        }
    }

    /// The requested tap size is advisory — the spike asked 1024 and got 4410
    /// every time — so no length may be special, including one shorter than the
    /// FFT window.
    #[test]
    fn the_analyzer_absorbs_buffers_shorter_and_longer_than_its_window() {
        let mut analyzer = SpectrumAnalyzer::new();
        let tone = |n: usize| -> Vec<f64> {
            (0..n)
                .map(|i| (2.0 * std::f64::consts::PI * i as f64 / 32.0).sin() * 0.5)
                .collect()
        };

        assert!(
            analyzer.feed(&tone(100)).is_none(),
            "a short buffer contributes and waits rather than padding itself out"
        );
        assert!(
            analyzer.feed(&tone(SPECTRUM_FFT_SIZE)).is_some(),
            "the window fills from whatever arrived, however it was chopped up"
        );
        for length in [1, 100, 512, 1024, 4410] {
            let frame = analyzer
                .feed(&tone(length))
                .unwrap_or_else(|| panic!("a {length}-frame buffer produced no magnitudes"));
            assert_eq!(frame.len(), 256, "the band count `spectrum bands=256` uses");
            assert!(
                frame
                    .iter()
                    .all(|v| v.is_finite() && *v >= SPECTRUM_FLOOR_DB),
                "magnitudes are finite dB above the floor"
            );
        }
    }

    /// The transform resolves frequency where GStreamer's `spectrum` does.
    ///
    /// Every number here is a literal on purpose. Asserting the band count
    /// against this module's own constant would pass whatever that constant
    /// said, and the point is that a 44.1 kHz stream lands a 1 kHz tone in the
    /// same bar on both backends.
    #[test]
    fn a_one_kilohertz_tone_peaks_where_a_512_point_transform_puts_it() {
        let mut analyzer = SpectrumAnalyzer::new();
        let tone: Vec<f64> = (0..4096)
            .map(|i| (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 44_100.0).sin() * 0.5)
            .collect();
        let frame = analyzer.feed(&tone).expect("4096 frames fill the window");

        let peak = frame
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();

        // 1000 Hz / (44100 / 512) = bin 11.6, and the frame starts at bin 1
        // because bin 0 is DC, so the peak is at index 10 or 11.
        assert!(
            (10..=11).contains(&peak),
            "a 1 kHz tone should peak at index 10 or 11, peaked at {peak}"
        );
        // A windowed transform confines a pure tone to its own neighbourhood.
        // Twenty bins away is 1.7 kHz from the tone and must be silent; drop
        // the Hamming window and leakage holds that bin 14 dB above the floor.
        assert!(
            frame[peak + 20] <= -60.0 + 1e-3,
            "leakage 20 bins from a pure tone should be at the floor, is {:.2} dB",
            frame[peak + 20]
        );
    }

    /// Normalization is a live gain here, so it lands now and it moves the
    /// level that is actually being written to the audio unit.
    #[test]
    fn normalization_is_a_live_gain_that_applies_now() {
        let (mut backend, _analysis) = offline_backend();
        backend.set_output_gain(Amplitude::UNITY);
        let before = backend.global_gain_db();

        let applied = backend.set_normalization(Normalization {
            enabled: true,
            clip_protection: false,
            fallback_db: -6.0,
            album_mode: false,
        });

        assert_eq!(applied, Applied::Now, "a gain node needs no reload");
        assert!(
            (backend.global_gain_db() - (before - 6.0)).abs() < 1e-3,
            "the -6 dB reached the gain stage: {} then {}",
            before,
            backend.global_gain_db()
        );

        let applied = backend.set_normalization(Normalization {
            enabled: false,
            clip_protection: false,
            fallback_db: -6.0,
            album_mode: false,
        });
        assert_eq!(applied, Applied::Now);
        assert!(
            (backend.global_gain_db() - before).abs() < 1e-3,
            "disabling puts the level back"
        );
    }

    /// Position and duration describe the same instant, and a seek's offset
    /// survives the player's sample clock restarting at zero.
    #[test]
    fn timeline_reports_the_seek_offset_plus_what_has_been_rendered() {
        let (mut backend, _analysis) = offline_backend();
        let fixture = tone_fixture(2);
        backend.load(&file_source(&fixture)).unwrap();

        let before = backend.timeline();
        let duration = before.duration.expect("a loaded file has a duration");
        assert!(
            (duration.as_secs_f64() - 1.0).abs() < 0.01,
            "one second of audio: {duration:?}"
        );

        backend.seek(Duration::from_millis(500)).unwrap();
        assert_eq!(
            backend.timeline().position,
            Some(Duration::from_millis(500)),
            "a seek moves the reported position before anything renders"
        );

        backend.set_state(PlayerState::Playing).unwrap();
        backend.render_offline(CHUNK).unwrap();
        let after = backend.timeline().position.unwrap().as_secs_f64();
        let expected = 0.5 + CHUNK as f64 / RATE;
        assert!(
            (after - expected).abs() < 0.05,
            "position is the seek offset plus what was rendered: {after:.3} vs {expected:.3}"
        );
        assert_eq!(
            backend.timeline().duration,
            before.duration,
            "duration does not move under the position"
        );
    }

    /// The requested tap size is a request, not a promise.
    ///
    /// The spike asked `installTapOnBus:` for 1024 frames and got 4410 every
    /// time, so the adapter reads `frameLength` and never the size it asked
    /// for. This builds a buffer at the size the spike measured and checks that
    /// every frame of it comes back — reading only [`TAP_BUFFER_SIZE`] would
    /// silently drop three quarters of the audio and still look like it worked.
    #[test]
    fn capture_reads_the_delivered_length_not_the_requested_one() {
        const DELIVERED: u32 = 4410;
        assert_ne!(
            DELIVERED, TAP_BUFFER_SIZE,
            "the point of this test is that the two differ"
        );

        unsafe {
            let format = AVAudioFormat::initStandardFormatWithSampleRate_channels(
                AVAudioFormat::alloc(),
                RATE,
                2,
            )
            .unwrap();
            let buffer = AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
                AVAudioPCMBuffer::alloc(),
                &format,
                DELIVERED,
            )
            .unwrap();
            buffer.setFrameLength(DELIVERED);

            // Left counts up, right is a constant nothing should ever return:
            // the mono trace is the left channel, as GStreamer's probe takes it.
            let channels = buffer.floatChannelData();
            let left = (*channels).as_ptr();
            let right = (*channels.add(1)).as_ptr();
            for i in 0..DELIVERED as usize {
                *left.add(i) = i as f32 / DELIVERED as f32;
                *right.add(i) = -1.0;
            }

            let mono = mono_samples(&buffer);
            assert_eq!(
                mono.len(),
                DELIVERED as usize,
                "every delivered frame is captured"
            );
            assert_eq!(mono[0], 0.0);
            assert!(
                (mono[DELIVERED as usize - 1] - (DELIVERED - 1) as f64 / DELIVERED as f64).abs()
                    < 1e-6,
                "the last frame is the last one delivered, not the 1024th"
            );
            assert!(
                mono.iter().all(|&s| s >= 0.0),
                "the left channel, not the right"
            );
        }
    }

    /// The production path, which every other test here deliberately avoids.
    ///
    /// Everything above renders offline so the suite needs no hardware, which
    /// means nothing else exercises `AudioBackend::open` — the device output
    /// stage, the tap on a real render thread, and the engine's own clock. This
    /// does, at silence, and is ignored by default like the other `live_*`
    /// tests. Run: `cargo test --lib live_avf_device_playback -- --ignored`.
    #[test]
    #[ignore = "opens the real audio output device; human-run"]
    fn live_avf_device_playback() {
        let analysis = Analysis::new(64, 65_536);
        let mut backend = AvBackend::open(analysis.tap()).expect("the default output device");
        // Inaudible: -96 dB is globalGain's floor.
        backend.set_output_gain(Amplitude::SILENT);

        let fixture = tone_fixture(2);
        backend.load(&file_source(&fixture)).unwrap();
        backend.set_state(PlayerState::Playing).unwrap();

        assert!(
            wait_for(|| analysis.waveform(256).iter().any(|s| s.abs() > 0.1)),
            "the tap fed the ring from a device render thread"
        );
        assert!(
            wait_for(|| backend.timeline().position.unwrap_or_default() > Duration::ZERO),
            "the device clock advanced the position"
        );
        assert!(
            wait_for(|| backend.poll_event() == Some(BusEvent::Eos)),
            "a one-second file plays to its end"
        );
        backend.set_state(PlayerState::Stopped).unwrap();
    }

    /// Seeking past the end has to land somewhere. There is nothing left to
    /// schedule, so it is reported as the track being over rather than leaving
    /// the transport waiting on frames that will never arrive.
    #[test]
    fn a_seek_past_the_end_reports_the_track_as_finished() {
        let (mut backend, _analysis) = offline_backend();
        let fixture = tone_fixture(2);
        backend.load(&file_source(&fixture)).unwrap();
        assert_eq!(
            backend.poll_event(),
            None,
            "a fresh load has nothing pending"
        );

        // The fixture is one second long.
        backend.seek(Duration::from_secs(5)).unwrap();
        assert_eq!(backend.poll_event(), Some(BusEvent::Eos));
    }

    /// "`Stopped` releases the output device. `Paused` preserves position." —
    /// the trait's own words, and the half that is easy to get wrong is the
    /// pause, because the player's clock reads nil the moment it stops.
    #[test]
    fn pausing_preserves_the_position_and_stopping_returns_to_the_start() {
        let (mut backend, _analysis) = offline_backend();
        let fixture = tone_fixture(2);
        backend.load(&file_source(&fixture)).unwrap();
        backend.set_state(PlayerState::Playing).unwrap();
        for _ in 0..3 {
            backend.render_offline(CHUNK).unwrap();
        }

        let playing = backend.timeline().position.unwrap();
        assert!(playing > Duration::ZERO, "something rendered");

        backend.set_state(PlayerState::Paused).unwrap();
        assert_eq!(
            backend.timeline().position,
            Some(playing),
            "a pause holds the position it stopped at"
        );

        backend.set_state(PlayerState::Playing).unwrap();
        backend.render_offline(CHUNK).unwrap();
        assert!(
            backend.timeline().position.unwrap() > playing,
            "resuming carries on from there rather than starting over"
        );

        backend.set_state(PlayerState::Stopped).unwrap();
        assert_eq!(
            backend.timeline().position,
            Some(Duration::ZERO),
            "a stop rewinds to the start of the track"
        );
    }

    /// Playing to the end posts exactly one EOS, and a stop on the way does
    /// not post one at all.
    #[test]
    fn end_of_track_posts_one_eos_and_a_stop_posts_none() {
        let (mut backend, _analysis) = offline_backend();
        let fixture = tone_fixture(2);

        backend.load(&file_source(&fixture)).unwrap();
        backend.set_state(PlayerState::Playing).unwrap();
        // One second of audio, rendered a chunk at a time with room to spare.
        for _ in 0..14 {
            backend.render_offline(CHUNK).unwrap();
        }
        assert!(
            wait_for(|| backend.poll_event() == Some(BusEvent::Eos)),
            "playing past the end of the file posts an EOS"
        );
        assert_eq!(backend.poll_event(), None, "exactly once");

        backend.load(&file_source(&fixture)).unwrap();
        backend.set_state(PlayerState::Playing).unwrap();
        backend.render_offline(CHUNK).unwrap();
        backend.set_state(PlayerState::Stopped).unwrap();
        // A negative cannot be waited for, only settled for: stop() fires the
        // outgoing completion handler on a queue, and this is long enough for
        // it to have arrived and been discarded.
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(
            backend.poll_event(),
            None,
            "a cancelled schedule is not a finished track"
        );
    }

    /// Parity harness: render a WAV through this adapter's EQ and write the
    /// result, so its magnitude response can be compared against GStreamer's
    /// `equalizer-10bands` over the same input.
    ///
    /// Ignored because it is a measurement tool, not an assertion. The gate it
    /// feeds is EQ parity before `DefaultBackend` flips to AVFoundation.
    ///
    /// ```text
    /// PARITY_IN=noise.wav PARITY_OUT=avf.wav PARITY_GAINS=0,0,0,0,0,12,0,0,0,0 \
    ///   cargo test --lib parity_render_through_the_eq -- --ignored --nocapture
    /// ```
    ///
    /// Writes 32-bit float mono, taking the left channel: the offline graph is
    /// stereo, and a mono source reaches it through the pan law the adapter
    /// compensates, so left is directly comparable to GStreamer's mono output.
    #[test]
    #[ignore = "measurement harness; needs PARITY_IN and PARITY_OUT"]
    fn parity_render_through_the_eq() {
        let input = std::env::var("PARITY_IN").expect("PARITY_IN");
        let output = std::env::var("PARITY_OUT").expect("PARITY_OUT");
        let gains: Vec<f64> = std::env::var("PARITY_GAINS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.trim().parse().expect("PARITY_GAINS must be numbers"))
            .collect();

        let (mut backend, _analysis) = offline_backend();
        backend.set_output_gain(Amplitude::UNITY);
        backend.set_eq(&EqCurve::FLAT.with_bands(&gains));

        let path = NSString::from_str(&input);
        let url = NSURL::fileURLWithPath(&path);
        backend
            .load(&MediaSource::Uri(url.absoluteString().unwrap().to_string()))
            .expect("load the parity input");
        backend.set_state(PlayerState::Playing).unwrap();

        let want_frames: u64 = std::env::var("PARITY_FRAMES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| std::fs::metadata(&input).unwrap().len() / 4);
        let mut left: Vec<f32> = Vec::new();
        while (left.len() as u64) < want_frames {
            let rendered = backend.render_offline(CHUNK).unwrap();
            if rendered.is_empty() {
                break;
            }
            left.extend_from_slice(&rendered);
        }

        let data_len = (left.len() * 4) as u32;
        let mut wav = Vec::with_capacity(44 + data_len as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&3u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&(RATE as u32).to_le_bytes());
        wav.extend_from_slice(&(RATE as u32 * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&32u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for s in &left {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(&output, &wav).unwrap();
        println!("wrote {} frames to {output}", left.len());
    }
}
