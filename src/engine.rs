//! Audio playback engine.
//!
//! [`Player`] owns everything about playback that does not depend on which
//! audio stack is underneath: the transport state machine, the fadeout ramp,
//! stop-after-current, the output composition rule
//! `user_volume * user_preamp * fade_factor`, the display-band frequency table,
//! the Granite visualizer, and ReplayGain *policy* — which dB number applies to
//! this track.
//!
//! Everything that needs to know what a `GstElement` is lives below the
//! [`backend::AudioBackend`] seam, in [`gst::GstBackend`]. Which adapter a
//! `Player` uses is a type argument with a `cfg`-selected default, so a bare
//! `Player` and `Player::new()` still mean "the real audio stack" everywhere,
//! while a test can ask for [`null::NullBackend`] one test at a time.
//!
//! Design of record: `docs/superpowers/plans/2026-08-31-audio-backend-seam-design.md`.

use anyhow::Result;
use std::time::{Duration, Instant};

/// The AVFoundation adapter, and macOS's [`DefaultBackend`].
#[cfg(target_os = "macos")]
pub mod avf;
pub mod backend;
/// The GStreamer adapter. Not compiled on macOS: nothing there uses it, and
/// the App Store build ships no GStreamer to link against.
#[cfg(not(target_os = "macos"))]
pub mod gst;
#[cfg(test)]
pub mod null;

use crate::config::{PREAMP_MAX, PREAMP_MIN};
use crate::engine::backend::{
    Amplitude, Analysis, Applied, AudioBackend, Capabilities, EqCurve, MediaSource, Normalization,
};

// ---------------------------------------------------------------------------
// BusEvent
// ---------------------------------------------------------------------------

/// The two events a backend can signal that the UI cares about.
///
/// Returned by [`Player::poll_bus`].  `None` from that method means no event
/// is pending; `Some(BusEvent)` means something happened and the caller
/// should react (advance the playlist, mark a track broken, etc.).
#[derive(Debug, Clone, PartialEq)]
pub enum BusEvent {
    /// The current track finished playing normally (end-of-stream).
    Eos,
    /// The backend reported a fatal error (e.g. file not found, codec missing).
    Error,
}

// ---------------------------------------------------------------------------
// PlayerState
// ---------------------------------------------------------------------------

/// The three mutually-exclusive transport states of the player.
///
/// `Player` owns the transition table; a backend is only ever told which of the
/// three to be in.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerState {
    /// No track loaded, or playback has been explicitly stopped.
    Stopped,
    /// A track is loaded and audio is actively being decoded and output.
    Playing,
    /// A track is loaded but decoding is frozen; position is preserved.
    Paused,
}

/// An in-flight stop-with-fadeout ramp.
///
/// Wall-clock based rather than step-counted: `poll_fadeout` is driven by each
/// frontend's tick loop, and those run at different rates (GTK 33 ms, mac
/// 100 ms), so a step count would make the same fade take different lengths on
/// different frontends.
struct Fadeout {
    started: Instant,
    duration: Duration,
}

/// The chain-shape subset of the ReplayGain config: the two flags that decide
/// WHICH elements sit in the pipeline plus the fallback gain applied at build.
/// `album-mode` and live fallback-gain changes are set as element properties,
/// deliberately NOT part of this struct — changing them must not trigger a
/// pipeline rebuild.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RgChain {
    pub enabled: bool,
    pub clip_protection: bool,
    pub fallback_db: f64,
}

/// The backend a bare `Player` gets.
///
/// `#[cfg]` picks the default; the constructor picks the actual. The seam is
/// what makes this one line the whole switch — nothing above it knows which
/// audio stack it is talking to, and a test can still ask for a named backend
/// one test at a time, which a crate-wide `cfg` could never allow.
///
/// **macOS is AVFoundation.** A bundled GStreamer invites App Store rejection
/// — plugins `dlopen`'d through a shell-script launcher, liborc JIT-compiling
/// into RWX pages, ~40 MB of dylibs needing signature and licence audit — and
/// the switch is not a compromise on either side of that:
///
/// - **EQ**, measured against the GStreamer chain: 0.13 dB RMS across the
///   mid-bands.
/// - **Formats**, measured by decoding one file of each through the adapter:
///   AVFoundation plays mp3, flac, ogg, opus, wav, **aac, m4a and aiff**,
///   while the shipped bundle's plugin allowlist carries decoders for only the
///   first five. The switch is a strict gain.
///
/// The rest of `AUDIO_EXTENSIONS`, measured on 4 September 2026 by asking each
/// build for the element rather than by reading a plugin list:
///
/// - `wma`, `ape` and `wv` play on Linux. The GNOME 50 runtime the Flatpak
///   uses carries `avdec_wmav2`, `avdec_ape` and `wavpackdec`. None of the
///   three plays on macOS, where CoreAudio decodes none of them.
/// - `tta` and `mpc` play on neither. `ttadec` and `musepackdec` are absent
///   from that runtime, though a full Linux install with gst-plugins-bad has
///   both, so the list is not wrong for everyone and removing them would cost
///   those users their library rows.
///
/// An earlier version of this note said all five were playable on neither.
/// That was true of macOS and wrong about Linux.
///
/// What it costs is recorded on `avf::AvBackend::set_normalization`:
/// ReplayGain applies as a plain gain, with no limiter behind
/// `clip_protection` and nothing for `album_mode` to choose between.
#[cfg(target_os = "macos")]
pub type DefaultBackend = avf::AvBackend;
#[cfg(not(target_os = "macos"))]
pub type DefaultBackend = gst::GstBackend;

// ---------------------------------------------------------------------------
// Player
// ---------------------------------------------------------------------------

/// The transport, the visualizer and the volume rules, over whichever audio
/// stack `B` is.
///
/// One instance is shared for the lifetime of the application; tracks are
/// loaded by calling `load()` before `play()`.
///
/// ## Thread safety
/// `Player` is not `Send`. It must be used on the thread where the backend was
/// opened (for GStreamer, the thread that called `gstreamer::init()` —
/// typically the main thread).
pub struct Player<B: AudioBackend = DefaultBackend> {
    /// The audio stack. Everything that knows what an audio element is lives
    /// here and nowhere above it.
    backend: B,
    /// What the backend can do, read once at construction because it cannot
    /// change afterwards.
    caps: Capabilities,
    /// Our local view of the transport state, updated synchronously on every
    /// transport method call.
    state: PlayerState,
    /// Transient "stop after the current track ends" flag (phase 6, key `t`).
    /// Not persisted: it governs a single automatic EOS advance, then clears.
    /// Manual transport (next/prev/play/stop) also clears it — see the
    /// accessors below and the advance seams that consult it.
    stop_after_current: bool,
    /// In-flight stop-with-fadeout ramp (Shift+V), or `None` when not fading.
    fadeout: Option<Fadeout>,
    /// Output attenuation currently imposed by a fadeout, 1.0 when none.
    /// Kept apart from `user_volume` so the ramp can move the audible level
    /// without rewriting the volume the user chose — restoring is just
    /// setting this back to 1.0.
    fade_factor: f64,
    /// The band gains as the user set them, mirrored to the backend on every
    /// change. The single source of truth; the backend never holds partial
    /// state and is never asked what it currently has.
    eq_curve: EqCurve,
    /// User-requested pre-amp multiplier (0.5–1.5).
    user_preamp: f64,
    /// User-requested playback volume (0.0–1.0).
    user_volume: f64,
    /// Read half of the visualizer buffers. The write half went to the backend
    /// at construction, which is the only time capture crosses the seam.
    analysis: Analysis,
    /// Granite plasma renderer state (lazy-allocated on first use).
    granite: Option<crate::granite::Granite>,
    /// The ReplayGain shape the *user* asked for. Not a shadow of what the
    /// audio path is currently shaped like — the backend owns that and is the
    /// one that answers whether a change took effect.
    rg_config: RgChain,
    /// Last-set album-mode, part of every normalization pushed down.
    rg_album_mode: bool,
    /// DB-sourced gain for the next `load()` — see `set_rg_db_gain`.
    rg_db_gain: Option<f64>,
    /// The gain that won for the track currently loaded, so that a later
    /// settings push does not silently replace it with the configured
    /// fallback.
    rg_track_fallback_db: Option<f64>,
    /// Whether the backend answered [`Applied::AtNextLoad`] to the last
    /// normalization pushed, i.e. whether a reload is needed to make the
    /// change audible now.
    rg_reload_pending: bool,
    /// Fake position for testing (overrides real position when set).
    #[cfg(test)]
    fake_position: Option<Duration>,
}

impl Player<DefaultBackend> {
    /// Create a new `Player` on the platform's audio stack.
    ///
    /// Returns an error if no audio path can be built at all; a merely
    /// diminished backend (no EQ, no spectrum, no ReplayGain) opens fine and
    /// says so through `has_eq` and friends.
    ///
    /// Defined here rather than on the generic impl on purpose: a `new()` that
    /// could be any backend would make every bare `Player::new()` ambiguous.
    ///
    /// `gstreamer::init()` must have been called before this.
    pub fn new() -> Result<Self> {
        Self::open()
    }
}

impl<B: AudioBackend> Player<B> {
    /// Create a `Player` on a named backend. `Player::new()` is the same thing
    /// with the platform default filled in, and is what production calls.
    pub fn open() -> Result<Self> {
        // 64 spectrum bands; waveform ring of 8192 samples ≈ 185 ms at 44.1 kHz.
        let analysis = Analysis::new(64, 8192);
        let backend = B::open(analysis.tap())?;
        let caps = backend.capabilities();

        Ok(Player {
            backend,
            caps,
            state: PlayerState::Stopped,
            stop_after_current: false,
            fadeout: None,
            fade_factor: 1.0,
            eq_curve: EqCurve::FLAT,
            user_preamp: 1.0,
            user_volume: 1.0,
            analysis,
            granite: None,
            // ReplayGain starts inactive — the audio path is exactly as the
            // backend built it. The first real shape is applied via
            // `set_replaygain` (config load) before the first play.
            rg_config: RgChain {
                enabled: false,
                clip_protection: false,
                fallback_db: 0.0,
            },
            rg_album_mode: false,
            rg_db_gain: None,
            rg_track_fallback_db: None,
            rg_reload_pending: false,
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
    /// `dt` is the elapsed time since the previous frame in 30 fps frame
    /// units (1.0 = 33 ms) — pass the measured frame delta so the plasma
    /// moves at the same speed at any refresh rate (see `Granite::render`).
    pub fn render_granite(
        &mut self,
        dst: &mut [u8],
        w: u32,
        h: u32,
        cfg: &crate::granite::GraniteConfig,
        dt: f32,
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
        g.render(dst, w, h, t_seconds, is_active, &pcm, cfg, dt);
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
    /// The backend discards anything buffered from the previous track and
    /// applies whatever normalization change it had to defer.
    pub fn load(&mut self, uri: &str) -> Result<()> {
        // Taken rather than peeked so a caller that forgets to prime the next
        // track can only under-apply, never mis-apply the previous track's
        // gain.
        self.rg_track_fallback_db = self.rg_db_gain.take();
        self.push_normalization();

        self.backend.load(&MediaSource::parse(uri))?;
        // Whatever was deferred has just been applied, by the one ordering
        // constraint the trait puts on `load`.
        self.rg_reload_pending = false;

        // A new track must not inherit the previous one's fade attenuation.
        // Restored only now that the outgoing track is silent: handing the
        // level back while it was still audible would blip.
        self.cancel_fadeout();

        // Clear stale waveform samples from the previous track so the new
        // track starts with a blank canvas rather than a ghost of old audio.
        // The bars keep their history; only the scope blanks.
        self.analysis.reset_waveform();

        self.state = PlayerState::Stopped;
        Ok(())
    }

    /// Begin or resume playback of the currently loaded URI.
    ///
    /// Returns as soon as the state-change request is posted, before audio
    /// actually starts.
    pub fn play(&mut self) -> Result<()> {
        // Any deliberate transport overrides a fade in progress; without this
        // the track would come back attenuated and then stop anyway.
        self.cancel_fadeout();
        self.backend.set_state(PlayerState::Playing)?;
        self.state = PlayerState::Playing;
        Ok(())
    }

    /// Toggle between `Playing` and `Paused`.
    ///
    /// - If currently `Playing`, pauses (freezes decode, retains position).
    /// - If currently `Paused`, resumes from the frozen position.
    /// - If `Stopped`, does nothing (nothing to pause or resume).
    pub fn toggle_pause(&mut self) -> Result<()> {
        // Pausing mid-fade cancels it: the ramp is wall-clock, so resuming
        // later would find it already expired and stop instantly.
        self.cancel_fadeout();
        let next = match self.state {
            PlayerState::Playing => PlayerState::Paused,
            PlayerState::Paused => PlayerState::Playing,
            PlayerState::Stopped => return Ok(()),
        };
        self.backend.set_state(next.clone())?;
        self.state = next;
        Ok(())
    }

    /// Stop playback and release the audio device.
    ///
    /// A subsequent `play()` call restarts from the beginning of the last
    /// loaded URI.
    ///
    /// Also clears the spectrum and waveform buffers so the visualizer
    /// collapses to its starting state (no bars / flat line) instead of
    /// freezing on the last received frame.  Pause deliberately leaves
    /// the buffers intact — the user expects pause to hold the picture.
    pub fn stop(&mut self) -> Result<()> {
        self.backend.set_state(PlayerState::Stopped)?;
        // Restore the level only after the audio path is down. Doing it first
        // would jump a fading track back to full volume for the instant before
        // the stop takes effect — an audible blip at the end of every fade.
        // Idempotent, so an ordinary stop that cut a fade short lands here too.
        self.cancel_fadeout();
        self.state = PlayerState::Stopped;
        self.analysis.clear();
        Ok(())
    }

    /// Return the current [`PlayerState`] without changing it.
    pub fn state(&self) -> &PlayerState {
        &self.state
    }

    /// Whether playback should stop when the current track reaches EOS.
    pub fn stop_after_current(&self) -> bool {
        self.stop_after_current
    }

    /// Arm/disarm the stop-after-current flag (key `t` toggles it).
    pub fn set_stop_after_current(&mut self, v: bool) {
        self.stop_after_current = v;
    }

    /// Read the flag and clear it in one step — the advance seam calls this
    /// so a single EOS consumes the arming and the next track auto-advances.
    pub fn take_stop_after_current(&mut self) -> bool {
        std::mem::replace(&mut self.stop_after_current, false)
    }

    // -----------------------------------------------------------------------
    // Stop with fadeout (Shift+V)
    // -----------------------------------------------------------------------

    /// Start ramping the output down to silence; `poll_fadeout` stops playback
    /// once the ramp reaches the end.
    ///
    /// Only meaningful while playing — fading a paused or stopped player would
    /// just leave the volume turned down with nothing to hear it happen. Asking
    /// for a fade that is already running restarts it from the current level,
    /// so a double press shortens rather than lengthens the stop.
    pub fn begin_fadeout(&mut self, duration: Duration) {
        if self.state != PlayerState::Playing {
            return;
        }
        self.fadeout = Some(Fadeout {
            started: Instant::now(),
            duration,
        });
    }

    /// Advance an in-flight fadeout. Returns `true` on the tick that finishes
    /// it — by then the player is stopped and the volume is back to normal, so
    /// a caller can use the return purely to update its status line.
    ///
    /// Call this from the frontend tick loop alongside `poll_bus`.
    pub fn poll_fadeout(&mut self) -> bool {
        let Some(fade) = self.fadeout.as_ref() else {
            return false;
        };

        // The track can end on its own mid-fade. Nothing is left to fade out,
        // so drop the ramp and hand the level back rather than stopping a
        // player that already stopped.
        if self.state != PlayerState::Playing {
            self.cancel_fadeout();
            return false;
        }

        let elapsed = fade.started.elapsed();
        if elapsed >= fade.duration {
            let _ = self.stop();
            self.cancel_fadeout();
            return true;
        }

        // Linear in amplitude. A perceptual (dB) curve sounds better in theory,
        // but over ~1.5 s the difference is slight and linear cannot produce
        // the "silent long before the end" effect a steep curve can.
        self.fade_factor = 1.0 - elapsed.as_secs_f64() / fade.duration.as_secs_f64();
        self.apply_output_volume();
        false
    }

    /// Abandon any in-flight fadeout and restore full output.
    pub fn cancel_fadeout(&mut self) {
        if self.fadeout.take().is_some() || self.fade_factor != 1.0 {
            self.fade_factor = 1.0;
            self.apply_output_volume();
        }
    }

    /// Whether a stop-with-fadeout ramp is currently running.
    pub fn is_fading_out(&self) -> bool {
        self.fadeout.is_some()
    }

    /// Force the player's own view of the transport without touching the
    /// backend, so a frontend test can simulate paused or playing without
    /// starting real audio.
    ///
    /// Not `#[cfg(test)]`, though it reads like it should be. The frontends
    /// that use it live in the binary crate, and a library compiled as that
    /// binary's dependency is built with `cfg(test)` off, so gating this made
    /// the TUI's own tests stop compiling the moment the binary stopped
    /// re-declaring the module tree. It is a two-line setter on a field the
    /// player already owns, so carrying it in release builds costs nothing
    /// worth a feature flag.
    #[doc(hidden)]
    pub fn set_state_for_test(&mut self, s: PlayerState) {
        self.state = s;
    }

    /// Only available in tests — sets a fake position for testing back button behavior.
    /// Its callers live in the GTK window tests, so on non-Linux test builds
    /// (where the GTK frontend isn't compiled) it would warn as dead code.
    #[cfg(test)]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn set_position_for_test(&mut self, pos: Duration) {
        self.fake_position = Some(pos);
    }

    /// The backend, for tests that want to read back what crossed the seam.
    #[cfg(test)]
    pub(crate) fn backend(&self) -> &B {
        &self.backend
    }

    /// The backend, for tests that need to drive it into a state `Player`
    /// cannot reach on its own.
    #[cfg(test)]
    pub(crate) fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Return the current playback position, or `None` if no track is loaded.
    ///
    /// In tests, returns the fake position if set via `set_position_for_test`.
    pub fn position(&self) -> Option<Duration> {
        #[cfg(test)]
        if let Some(pos) = self.fake_position {
            return Some(pos);
        }
        self.backend.timeline().position
    }

    /// Return the total duration of the loaded track, or `None` if the
    /// duration is not yet known (e.g., playback is still starting up or the
    /// format does not advertise a duration).
    pub fn duration(&self) -> Option<Duration> {
        self.backend.timeline().duration
    }

    /// Current playback position in microseconds (0 when unknown). Convenience
    /// for MPRIS / MPNowPlayingInfoCenter, whose Position is `x` (µs) / elapsed
    /// seconds — avoids each consumer re-deriving it from `position()`.
    /// `dead_code` until the phase-3 MPRIS layer consumes it.
    #[allow(dead_code)]
    pub fn position_usecs(&self) -> i64 {
        self.position().map(|d| d.as_micros() as i64).unwrap_or(0)
    }

    /// Total track length in microseconds (0 when unknown). MPRIS `mpris:length`.
    #[allow(dead_code)]
    pub fn length_usecs(&self) -> i64 {
        self.duration().map(|d| d.as_micros() as i64).unwrap_or(0)
    }

    /// Seek to an absolute position within the current track.
    pub fn seek(&mut self, pos: Duration) -> Result<()> {
        self.backend.seek(pos)
    }

    /// Set the playback volume.
    ///
    /// `vol` is clamped to `[0.0, 1.0]`.  What reaches the backend is
    /// `vol × user_preamp × fade_factor`, so no factor overwrites another.
    pub fn set_volume(&mut self, vol: f64) {
        self.user_volume = vol.clamp(0.0, 1.0);
        self.apply_output_volume();
    }

    // -----------------------------------------------------------------------
    // ReplayGain
    //
    // The split is when-versus-how. `Player` resolves which dB number applies
    // to this track and pushes the complete desired shape; the backend answers
    // whether that took effect now or only from the next `load()`. GStreamer's
    // "relink only at Null" rule lives in the adapter, not here.
    // -----------------------------------------------------------------------

    /// True when the backend can normalize at all. The feature silently no-ops
    /// when it cannot.
    #[allow(dead_code)]
    pub fn rg_available(&self) -> bool {
        self.caps.normalization
    }

    /// Request a ReplayGain chain shape. Takes effect immediately when the
    /// backend can reshape its audio path right now; otherwise at the next
    /// `load()` — mid-track toggles take effect on the next track by design.
    #[allow(dead_code)]
    pub fn set_replaygain(&mut self, cfg: RgChain) {
        if cfg.fallback_db != self.rg_config.fallback_db {
            // A newly configured fallback replaces the gain primed for
            // whatever is loaded now; the next load primes the next one.
            self.rg_track_fallback_db = None;
        }
        self.rg_config = cfg;
        self.push_normalization();
    }

    /// True when a ReplayGain change is waiting for the next `load()`. The
    /// controller uses this to decide whether it must reload the current track
    /// to apply the change live, vs. an album-mode/fallback tweak that needs no
    /// reload.
    #[allow(dead_code)]
    pub fn rg_reload_pending(&self) -> bool {
        self.rg_reload_pending
    }

    /// Live album/track-mode switch (Automatic source sets this at each track
    /// start from the shuffle state).
    #[allow(dead_code)]
    pub fn set_rg_album_mode(&mut self, album: bool) {
        self.rg_album_mode = album;
        self.push_normalization();
    }

    /// Live fallback-gain change (dB applied to untagged files).
    #[allow(dead_code)]
    pub fn set_rg_fallback_db(&mut self, db: f64) {
        self.rg_config.fallback_db = db;
        // The user changing the configured fallback is a decision about what
        // is playing now, not only about the next track.
        self.rg_track_fallback_db = None;
        self.push_normalization();
    }

    /// Gain (dB) to use for the NEXT `load()` when the file itself carries no
    /// ReplayGain tags — i.e. the value Sparkamp measured and stored in the
    /// library DB. Pass `None` for "nothing analyzed", which leaves the user's
    /// configured fallback in charge.
    ///
    /// This is how DB-stored gains reach playback at all. `rgvolume` only ever
    /// reads tags off the decoded stream, so an analyzed-but-untagged file
    /// (analysis with write-tags off, or a container Sparkamp cannot tag)
    /// would otherwise play completely unnormalized despite having a
    /// perfectly good measured gain sitting in the library. Feeding it in as
    /// the fallback slots into rgvolume's own precedence: real tags still win,
    /// and after a scan harvest those tags hold the same number anyway.
    ///
    /// `load()` consumes and clears this, so a missed call degrades to the
    /// configured fallback rather than silently applying the previous track's
    /// gain to a different song.
    #[allow(dead_code)]
    pub fn set_rg_db_gain(&mut self, db: Option<f64>) {
        self.rg_db_gain = db;
    }

    /// Hand the backend the complete normalization picture and record what it
    /// answered. Full state every time, so a repeat is a no-op and two
    /// interleaved changes cannot lose half of either.
    fn push_normalization(&mut self) {
        let want = Normalization {
            enabled: self.rg_config.enabled,
            clip_protection: self.rg_config.clip_protection,
            fallback_db: self
                .rg_track_fallback_db
                .unwrap_or(self.rg_config.fallback_db),
            album_mode: self.rg_album_mode,
        };
        self.rg_reload_pending = self.backend.set_normalization(want) == Applied::AtNextLoad;
    }

    /// Returns `true` if the backend has a working equalizer.  The EQ methods
    /// keep the shadow copy either way; only the audible effect is missing.
    #[allow(dead_code)]
    pub fn has_eq(&self) -> bool {
        self.caps.eq
    }

    /// Returns `true` if the backend can produce spectrum data.
    #[allow(dead_code)]
    pub fn has_spectrum(&self) -> bool {
        self.caps.spectrum
    }

    /// Set the gain for a single EQ band.
    ///
    /// `band` must be in `0..10`; values outside that range are silently
    /// ignored.  `gain_db` is clamped to `[-12.0, +12.0]` dB — a symmetric
    /// range that fits within GStreamer's `equalizer-10bands` hardware limit.
    ///
    /// The change takes effect immediately, even during playback.
    pub fn set_eq_band(&mut self, band: usize, gain_db: f64) {
        self.eq_curve = self.eq_curve.with_band(band, gain_db);
        self.backend.set_eq(&self.eq_curve);
        self.apply_output_volume();
    }

    /// Read back the current gain for a single EQ band from the shadow copy.
    ///
    /// Returns `0.0` if `band` is out of range.
    #[allow(dead_code)]
    pub fn get_eq_band(&self, band: usize) -> f64 {
        self.eq_curve.band(band)
    }

    /// Apply all 10 band gains from a slice in one call.
    ///
    /// Convenient for bulk-applying a preset or a restored config.  Silently
    /// ignores extra elements if `bands` has more than 10 entries; bands not
    /// covered by a short slice are left unchanged.
    pub fn apply_eq_bands(&mut self, bands: &[f64]) {
        self.eq_curve = self.eq_curve.with_bands(bands);
        self.backend.set_eq(&self.eq_curve);
        self.apply_output_volume();
    }

    /// Set the user-requested pre-amplifier gain applied before the EQ bands.
    ///
    /// `multiplier` is a linear scale factor in `[0.5, 1.5]` (50 %–150 %).
    /// Pass `1.0` for unity gain.
    pub fn set_preamp(&mut self, multiplier: f64) {
        self.user_preamp = multiplier.clamp(PREAMP_MIN, PREAMP_MAX);
        self.apply_output_volume();
    }

    /// Push the audible level — user volume, pre-amp, and any fadeout
    /// attenuation — to the backend. Every writer goes through here so that
    /// changing one factor mid-fade cannot silently discard the others.
    fn apply_output_volume(&mut self) {
        let gain = Amplitude::linear(self.user_volume * self.user_preamp * self.fade_factor);
        self.backend.set_output_gain(gain);
    }

    /// Non-blocking event poll.  Returns `Some(BusEvent)` when the current
    /// track has ended (EOS) or hit a fatal error, or `None` when nothing
    /// noteworthy is pending.  The caller should advance the playlist on any
    /// `Some` result, and additionally mark the current track broken on
    /// `BusEvent::Error`.
    ///
    /// Never blocks; should be called regularly (e.g. every 100 ms) from the
    /// UI tick loop, which is also what services the backend's analysis
    /// capture.
    ///
    /// Errors are NOT written to stderr; callers surface them through the UI.
    pub fn poll_bus(&mut self) -> Option<BusEvent> {
        let event = self.backend.poll_event()?;
        // Both events end playback.
        self.state = PlayerState::Stopped;
        Some(event)
    }

    /// Return spectrum data mapped to display bars using logarithmic frequency scale.
    ///
    /// Maps the raw spectrum bands (0-22050 Hz) to `num_bands` display bars
    /// using a logarithmic scale that matches the equalizer frequency range.
    ///
    /// Uses smoothed band values for smooth bar animation.
    pub fn get_spectrum_display_bands(&self, num_bands: u32) -> Vec<f64> {
        let spectrum = self.analysis.magnitudes();
        if !self.analysis.has_magnitudes() || spectrum.is_empty() {
            return vec![0.0; num_bands as usize];
        }

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
        self.analysis.waveform(count)
    }

    /// Check if spectrum data has actually arrived from the backend.
    #[allow(dead_code)] // GTK-only; out of bin reach on macOS where GTK is gated.
    pub fn has_spectrum_data(&self) -> bool {
        self.caps.spectrum && self.analysis.has_magnitudes()
    }
}

#[cfg(test)]
mod live_cdda_tests {
    use super::*;

    /// Live diagnosis: play a real CD track through the full Player pipeline and
    /// log the bus events + position each 250 ms. Run:
    /// `cargo test --lib live_play_cdda -- --ignored --nocapture`
    #[test]
    fn position_usecs_converts_and_defaults() {
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().unwrap();
        let mut p = Player::new().unwrap();
        // No pipeline position yet → 0 (not a panic).
        assert_eq!(p.position_usecs(), 0);
        // Fake position flows through the µs conversion.
        p.set_position_for_test(Duration::from_millis(1500));
        assert_eq!(p.position_usecs(), 1_500_000);
    }

    #[test]
    fn stop_after_current_flag_arms_and_takes_once() {
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().unwrap();
        let mut p = Player::new().unwrap();
        assert!(!p.stop_after_current());
        p.set_stop_after_current(true);
        assert!(p.stop_after_current());
        assert!(p.take_stop_after_current()); // fires
        assert!(!p.stop_after_current()); // cleared by take
        assert!(!p.take_stop_after_current()); // already clear
    }

    /// The audible level while fading, read straight off the volume element.
    fn output_volume(p: &Player) -> f64 {
        p.backend().output_volume()
    }

    /// GStreamer stores the volume property as a float internally, so a value
    /// written as an `f64` reads back a few ULPs away (0.7 → 0.699999988…).
    fn assert_volume(p: &Player, expected: f64) {
        let actual = output_volume(p);
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected volume {expected}, got {actual}"
        );
    }

    /// The three ways out of a cdda session must each release the guard, and
    /// exactly once.
    ///
    /// Dropping the player was the one that did not: the count stayed up for
    /// the rest of the process and disc detection silently never polled again.
    /// No hardware needed — `load` only parses the URI and sets a property;
    /// the device is not opened until playback starts.
    #[test]
    /// Named `GstBackend` rather than left on the default: `cdda://` is the
    /// Linux disc path, and macOS's default backend refuses it outright
    /// because a Mac reaches an audio CD through the filesystem instead. The
    /// macOS shape of this same guard is the test below.
     // GStreamer-path test: `cdda://` is the Linux disc source, and the
    // adapter it names is not compiled on macOS.
    #[cfg(not(target_os = "macos"))]
   fn every_exit_from_a_cdda_session_releases_the_guard() {
        let _lock = crate::disc::detect::exclusive_read_test_guard();
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().unwrap();
        use crate::disc::detect::{exclusive_read, exclusive_read_depth};
        assert_eq!(exclusive_read_depth(), 0, "must start clear");

        // 1. stop()
        let mut p = Player::<crate::engine::gst::GstBackend>::open().unwrap();
        p.load("cdda://1?device=/dev/sr0").unwrap();
        assert!(exclusive_read(), "a cdda load takes the guard");
        p.stop().unwrap();
        assert_eq!(exclusive_read_depth(), 0, "stop released it");

        // 2. Loading something that is not a disc track.
        p.load("cdda://2?device=/dev/sr0").unwrap();
        assert!(exclusive_read());
        p.load("file:///nonexistent.mp3").unwrap();
        assert_eq!(exclusive_read_depth(), 0, "a non-cdda load released it");

        // Back-to-back cdda loads are one session, not two — otherwise
        // advancing tracks on the same disc would leave the count too high.
        p.load("cdda://3?device=/dev/sr0").unwrap();
        p.load("cdda://4?device=/dev/sr0").unwrap();
        assert_eq!(exclusive_read_depth(), 1, "still one session, not two");

        // 3. Drop, with no stop() first.
        drop(p);
        assert_eq!(exclusive_read_depth(), 0, "drop released it");
    }

    /// Playing a file that lives on a mounted disc must raise the same guard a
    /// `cdda://` stream does, and drop it again on the way out.
    ///
    /// macOS reaches an audio CD this way — one mounted `.aiff` per track — so
    /// without this the guard stayed down for the whole of CD playback there
    /// and the ten-second drive poll kept issuing `drutil status` into the
    /// middle of the read. GTK never showed it because the Linux poll checks
    /// the guard and answers from a cheap status ioctl instead.
    ///
    /// No hardware: `load` only parses the URI and sets a property, and the
    /// mount list is seeded directly.
    #[test]
     // GStreamer-path test: `cdda://` is the Linux disc source, and the
    // adapter it names is not compiled on macOS.
    #[cfg(not(target_os = "macos"))]
   fn playing_a_file_on_a_disc_raises_and_releases_the_guard() {
        let _lock = crate::disc::detect::exclusive_read_test_guard();
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().unwrap();
        use crate::disc::detect::{exclusive_read_depth, set_optical_mounts_for_test};
        assert_eq!(exclusive_read_depth(), 0, "must start clear");

        set_optical_mounts_for_test(vec![std::path::PathBuf::from("/Volumes/Audio CD 1")]);
        // `GstBackend` by name, and the paths deliberately do not exist.
        //
        // On macOS `path_is_on_optical_media` answers from `statfs` for any
        // path that can be stat'd, and consults the seeded mount list only for
        // one that cannot — so a real temp directory reports `apfs` and can
        // never stand in for an optical mount there. That leaves the seeded
        // list reachable only through paths that do not exist, which in turn
        // needs a backend whose `load` does not open the file. GStreamer's
        // sets a URI property and defers; AVFoundation opens immediately.
        //
        // What this covers is the decision and the balancing: the percent
        // decode, one session across tracks, and the three ways out. The same
        // guard on the macOS backend against a real disc is
        // `live_avf_disc_playback_holds_the_guard`.
        let mut p = Player::<crate::engine::gst::GstBackend>::open().unwrap();

        // A file somewhere else is not a disc read and must not take it.
        p.load("file:///Users/me/Music/a.mp3").unwrap();
        assert_eq!(exclusive_read_depth(), 0, "an ordinary file takes nothing");

        // The space in the volume name arrives percent-encoded; the prefix
        // test only works because the URI is decoded back to a path first.
        p.load("file:///Volumes/Audio%20CD%201/1%20Track%201.aiff").unwrap();
        assert_eq!(exclusive_read_depth(), 1, "a file on the disc takes the guard");

        // Another track on the same disc is one session, not two.
        p.load("file:///Volumes/Audio%20CD%201/2%20Track%202.aiff").unwrap();
        assert_eq!(exclusive_read_depth(), 1, "still one session");

        // Leaving the disc releases it.
        p.load("file:///Users/me/Music/a.mp3").unwrap();
        assert_eq!(exclusive_read_depth(), 0, "leaving the disc released it");

        // And so does dropping mid-track, the leak `release_disc_guard` exists for.
        p.load("file:///Volumes/Audio%20CD%201/3%20Track%203.aiff").unwrap();
        assert_eq!(exclusive_read_depth(), 1);
        drop(p);
        assert_eq!(exclusive_read_depth(), 0, "drop released it");

        set_optical_mounts_for_test(Vec::new());
    }

    /// LIVE: the macOS backend must take and release the same guard while
    /// playing a track off a real audio CD.
    /// `cargo test --lib live_avf_disc_playback_holds_the_guard -- --ignored --nocapture`
    ///
    /// The unit test above cannot reach this on macOS: `statfs` answers for
    /// any path that exists, so only a real optical mount reports `cddafs`.
    /// Without the guard the ten-second drive poll reads the medium underneath
    /// playback, which is the hazard it exists for — and the AVFoundation
    /// backend shipped without it until the default was switched.
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn live_avf_disc_playback_holds_the_guard() {
        let _lock = crate::disc::detect::exclusive_read_test_guard();
        use crate::disc::detect::exclusive_read_depth;
        let drives = crate::disc::detect::list_drives();
        let Some(track) = drives
            .iter()
            .filter(|d| d.media.is_audio_cd)
            .filter_map(|d| d.mount_path.clone())
            .find_map(|mount| {
                std::fs::read_dir(mount)
                    .ok()?
                    .flatten()
                    .map(|e| e.path())
                    .find(|p| crate::model::is_audio_file(p))
            })
        else {
            println!("no audio CD mounted — skipping");
            return;
        };
        println!("playing {}", track.display());
        assert_eq!(exclusive_read_depth(), 0, "must start clear");

        let uri = format!("file://{}", track.display()).replace(' ', "%20");
        let mut p = Player::new().unwrap();
        p.load(&uri).unwrap();
        assert_eq!(
            exclusive_read_depth(),
            1,
            "a track on a mounted audio CD must take the guard"
        );
        p.stop().unwrap();
        assert_eq!(exclusive_read_depth(), 0, "stop released it");

        p.load(&uri).unwrap();
        assert_eq!(exclusive_read_depth(), 1);
        drop(p);
        assert_eq!(exclusive_read_depth(), 0, "drop released it");
    }

    /// The whole fadeout contract in one pass: it attenuates while running,
    /// stops the player when the ramp expires, and hands the user's volume
    /// back afterwards so the next track is not silent.
    #[test]
    fn fadeout_ramps_down_then_stops_and_restores_the_volume() {
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().unwrap();
        let mut p = Player::new().unwrap();
        p.set_volume(0.8);
        p.set_state_for_test(PlayerState::Playing);

        p.begin_fadeout(Duration::from_millis(60));
        assert!(p.is_fading_out());

        std::thread::sleep(Duration::from_millis(20));
        assert!(!p.poll_fadeout(), "still mid-ramp");
        let mid = output_volume(&p);
        assert!(mid < 0.8 && mid > 0.0, "attenuated but not silent: {mid}");

        std::thread::sleep(Duration::from_millis(60));
        assert!(p.poll_fadeout(), "ramp expired — reports completion once");
        assert_eq!(*p.state(), PlayerState::Stopped);
        assert!(!p.is_fading_out());
        assert_volume(&p, 0.8); // restored
        assert!(!p.poll_fadeout(), "completion is reported only once");
    }

    /// Fading a player that is not playing would just turn the volume down
    /// with nothing audible to fade, and would then stop an already-stopped
    /// player.
    #[test]
    fn fadeout_is_a_no_op_unless_playing() {
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().unwrap();
        let mut p = Player::new().unwrap();
        p.set_volume(0.7);

        p.begin_fadeout(Duration::from_millis(10));
        assert!(!p.is_fading_out(), "stopped player does not fade");

        p.set_state_for_test(PlayerState::Paused);
        p.begin_fadeout(Duration::from_millis(10));
        assert!(!p.is_fading_out(), "paused player does not fade");
        assert_volume(&p, 0.7);
    }

    /// Deliberate transport beats a fade in progress — otherwise the track
    /// would resume attenuated and then stop out from under the user.
    #[test]
    fn transport_cancels_a_fadeout_and_restores_the_volume() {
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().unwrap();
        let mut p = Player::new().unwrap();
        p.set_volume(0.6);
        p.set_state_for_test(PlayerState::Playing);

        p.begin_fadeout(Duration::from_millis(500));
        std::thread::sleep(Duration::from_millis(30));
        p.poll_fadeout();
        assert!(output_volume(&p) < 0.6, "ramp took hold");

        // The state change itself fails here — an empty pipeline with no URI
        // cannot reach Playing — but the cancel runs before it, which is the
        // ordering this test exists to hold in place.
        let _ = p.play();
        assert!(!p.is_fading_out());
        assert_volume(&p, 0.6); // restored
    }

    /// A track can reach its own end mid-fade. There is nothing left to fade,
    /// so the ramp is abandoned rather than firing a stop at a player that
    /// already stopped — and the volume must not stay ducked.
    #[test]
    fn a_track_ending_mid_fade_abandons_the_ramp() {
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().unwrap();
        let mut p = Player::new().unwrap();
        p.set_volume(0.9);
        p.set_state_for_test(PlayerState::Playing);

        p.begin_fadeout(Duration::from_millis(500));
        std::thread::sleep(Duration::from_millis(30));
        p.poll_fadeout();

        // What poll_bus does on EOS.
        p.set_state_for_test(PlayerState::Stopped);
        assert!(!p.poll_fadeout(), "not reported as a fade completion");
        assert!(!p.is_fading_out());
        assert_volume(&p, 0.9); // restored
    }

    /// The end-to-end form of the guard bug, against a real drive: play a CD
    /// track, drop the player without stopping, and check that detection
    /// actually polls the device again.
    ///
    /// The unit test above proves the counter returns to zero. This proves what
    /// that was for — before the fix, disc detection went silently dead for the
    /// rest of the process, and nothing surfaced an error to say so.
    ///
    /// `cargo test --lib live_drop_mid_cdda -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_drop_mid_cdda_lets_detection_resume() {
        let _lock = crate::disc::detect::exclusive_read_test_guard();
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().unwrap();
        use crate::disc::detect::{exclusive_read, exclusive_read_depth, list_drives};

        let before = list_drives();
        let Some(disc) = before.iter().find(|d| d.media.is_audio_cd) else {
            panic!("this test needs an audio CD in the drive");
        };
        // The device comes from detection, not from a literal. A drive id is
        // "/dev/sr0" on Linux and "drive-<hash>" on macOS, so a hardcoded node
        // is a Linux-only test wearing platform-neutral clothes.
        let uri = format!("cdda://1?device={}", disc.id);
        assert_eq!(exclusive_read_depth(), 0, "must start clear");

        let mut p = Player::new().unwrap();
        p.load(&uri).unwrap();
        p.play().unwrap();
        // Wait for the drive rather than guessing at it: spin-up varies by
        // drive and by disc, and a fixed sleep long enough for one is a
        // coin-flip for the next.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut playing = false;
        while std::time::Instant::now() < deadline {
            let _ = p.poll_bus();
            if p.position().is_some_and(|pos| pos > Duration::ZERO) {
                playing = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(playing, "the disc should actually be playing");
        assert!(exclusive_read(), "playback owns the drive");

        // No stop() — this is the path that leaked.
        drop(p);
        assert_eq!(exclusive_read_depth(), 0, "dropping released the guard");

        // And detection is alive again: a fresh probe still sees the disc.
        let after = list_drives();
        assert!(
            after.iter().any(|d| d.media.is_audio_cd),
            "detection must poll the device again once the guard is released"
        );
    }

    #[test]
    #[ignore]
    fn live_play_cdda() {
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().unwrap();
        let drives = crate::disc::detect::list_drives();
        let Some(disc) = drives.iter().find(|d| d.media.is_audio_cd) else {
            println!("no audio CD in any drive, skipping");
            return;
        };
        let mut p = Player::new().unwrap();
        p.load(&format!("cdda://1?device={}", disc.id)).unwrap();
        p.play().unwrap();
        for i in 0..24 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let ev = p.poll_bus();
            eprintln!(
                "t={i:2} ev={:?} pos={:?} dur={:?} state={:?}",
                ev,
                p.position(),
                p.duration(),
                p.state()
            );
        }
    }
}

/// `Player`'s own policy, with no audio stack under it.
///
/// These are the rules that used to be untestable: every test built a real
/// GStreamer pipeline, so the ReplayGain deferral tests all silently `return`ed
/// where the plugin was absent, and the one that ran asserted against a state
/// field a test had written rather than anything the audio path believed.
#[cfg(test)]
mod player_over_null {
    use super::null::NullBackend;
    use super::*;

    fn player() -> Player<NullBackend> {
        Player::<NullBackend>::open().unwrap()
    }

    fn chain(enabled: bool, clip_protection: bool, fallback_db: f64) -> RgChain {
        RgChain {
            enabled,
            clip_protection,
            fallback_db,
        }
    }

    /// The policy half of the deferral rule: a reshape the backend cannot do
    /// while running is remembered, reported as needing a reload, and applied
    /// by the next `load()`.
    #[test]
    fn a_reshape_refused_mid_track_is_applied_by_the_next_load() {
        let mut p = player();
        p.backend_mut().defer_reshape(true);
        p.backend_mut().force_state(PlayerState::Playing);

        p.set_replaygain(chain(true, true, -6.0));
        assert!(p.rg_reload_pending(), "the backend asked for a reload");
        assert!(
            !p.backend().normalization().enabled,
            "nothing was reshaped while running"
        );

        p.load("file:///nonexistent.mp3").unwrap();
        assert!(!p.rg_reload_pending(), "the load consumed the deferral");
        let applied = p.backend().normalization();
        assert!(applied.enabled);
        assert!(applied.clip_protection);
        assert_eq!(applied.fallback_db, -6.0);
    }

    /// A backend that can reshape while running never asks for a reload, which
    /// is what stops the reload dance in `ffi_apply_replaygain` from firing on
    /// a platform that does not need it.
    #[test]
    fn a_backend_that_applies_now_never_asks_for_a_reload() {
        let mut p = player();
        p.backend_mut().force_state(PlayerState::Playing);

        p.set_replaygain(chain(true, true, -6.0));
        assert!(!p.rg_reload_pending());
        assert!(p.backend().normalization().enabled);
    }

    /// The DB-measured gain for the track being loaded outranks the configured
    /// fallback, and is consumed by that one load.
    #[test]
    fn a_primed_track_gain_becomes_this_track_s_fallback() {
        let mut p = player();
        p.set_replaygain(chain(true, false, -3.0));

        p.set_rg_db_gain(Some(-11.5));
        p.load("file:///a.mp3").unwrap();
        assert_eq!(p.backend().normalization().fallback_db, -11.5);

        // Nothing primed for the next track: the configured fallback is back
        // in charge rather than the previous track's gain.
        p.load("file:///b.mp3").unwrap();
        assert_eq!(p.backend().normalization().fallback_db, -3.0);
    }

    /// Album mode reaches the backend without disturbing the chain shape.
    #[test]
    fn album_mode_crosses_the_seam_as_part_of_the_whole_picture() {
        let mut p = player();
        p.set_replaygain(chain(true, false, 0.0));

        p.set_rg_album_mode(true);
        assert!(p.backend().normalization().album_mode);
        assert!(p.backend().normalization().enabled, "shape untouched");

        p.set_rg_album_mode(false);
        assert!(!p.backend().normalization().album_mode);
    }

    /// An end-of-stream from the backend is what stops the player; the
    /// frontends read the state right after polling.
    #[test]
    fn an_eos_from_the_backend_stops_the_player() {
        let mut p = player();
        p.play().unwrap();
        assert_eq!(*p.state(), PlayerState::Playing);

        p.backend_mut().post_event(BusEvent::Eos);
        assert_eq!(p.poll_bus(), Some(BusEvent::Eos));
        assert_eq!(*p.state(), PlayerState::Stopped);
        assert_eq!(p.poll_bus(), None, "the event is delivered exactly once");
    }
}

/// Characterization pin for the EQ shadow state and the output-volume
/// composition, written before the `AudioBackend` seam extraction.
///
/// The suite covered the ReplayGain chain and the fadeout ramp but never
/// asserted that a band gain or a preamp change survives clamping and reaches
/// the output, which is the exact path extraction moves. It now also asserts
/// that both actually crossed the seam, which before the seam was unobservable:
/// the EQ element was forced to `None` in every test.
#[cfg(test)]
mod eq_volume_pin {
    use super::null::NullBackend;
    use super::*;
    use crate::config::EQ_BAND_DB_LIMIT;

    fn player() -> Player<NullBackend> {
        Player::<NullBackend>::open().unwrap()
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    fn output_volume(p: &Player<NullBackend>) -> f64 {
        p.backend().output_gain().get()
    }

    /// What the backend was actually told, as opposed to what `Player`
    /// remembers telling it.
    fn installed_band(p: &Player<NullBackend>, band: usize) -> f64 {
        p.backend().eq().band(band)
    }

    #[test]
    fn set_eq_band_clamps_to_plus_minus_limit() {
        let mut p = player();

        p.set_eq_band(3, 99.0);
        assert_close(p.get_eq_band(3), EQ_BAND_DB_LIMIT);
        assert_close(installed_band(&p, 3), EQ_BAND_DB_LIMIT);

        p.set_eq_band(3, -99.0);
        assert_close(p.get_eq_band(3), -EQ_BAND_DB_LIMIT);
        assert_close(installed_band(&p, 3), -EQ_BAND_DB_LIMIT);

        p.set_eq_band(3, 4.5);
        assert_close(p.get_eq_band(3), 4.5);
        assert_close(installed_band(&p, 3), 4.5);
    }

    #[test]
    fn set_eq_band_out_of_range_is_a_no_op() {
        let mut p = player();
        p.set_eq_band(10, 6.0);
        p.set_eq_band(usize::MAX, 6.0);
        for b in 0..10 {
            assert_close(p.get_eq_band(b), 0.0);
            assert_close(installed_band(&p, b), 0.0);
        }
    }

    #[test]
    fn get_eq_band_out_of_range_reads_zero() {
        let p = player();
        assert_close(p.get_eq_band(10), 0.0);
        assert_close(p.get_eq_band(usize::MAX), 0.0);
    }

    #[test]
    fn apply_eq_bands_clamps_and_leaves_uncovered_bands_alone() {
        let mut p = player();
        p.set_eq_band(9, 7.0);

        p.apply_eq_bands(&[99.0, -99.0, 2.0]);

        assert_close(p.get_eq_band(0), EQ_BAND_DB_LIMIT);
        assert_close(p.get_eq_band(1), -EQ_BAND_DB_LIMIT);
        assert_close(p.get_eq_band(2), 2.0);
        for b in 3..9 {
            assert_close(p.get_eq_band(b), 0.0);
        }
        assert_close(p.get_eq_band(9), 7.0);
        assert_eq!(
            p.backend().eq().as_db(),
            &[
                EQ_BAND_DB_LIMIT,
                -EQ_BAND_DB_LIMIT,
                2.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                7.0
            ],
            "the whole curve reaches the backend, not just the changed bands"
        );
    }

    #[test]
    fn apply_eq_bands_ignores_entries_past_the_tenth() {
        let mut p = player();
        p.apply_eq_bands(&[1.0; 14]);
        for b in 0..10 {
            assert_close(p.get_eq_band(b), 1.0);
            assert_close(installed_band(&p, b), 1.0);
        }
    }

    #[test]
    fn volume_and_preamp_compose_multiplicatively_on_the_output() {
        let mut p = player();
        p.set_volume(0.5);
        p.set_preamp(1.4);
        assert_close(output_volume(&p), 0.5 * 1.4);

        p.set_volume(0.25);
        assert_close(output_volume(&p), 0.25 * 1.4);
    }

    #[test]
    fn set_preamp_clamps_to_the_configured_range() {
        let mut p = player();
        p.set_volume(1.0);

        p.set_preamp(99.0);
        assert_close(output_volume(&p), PREAMP_MAX);

        p.set_preamp(0.0);
        assert_close(output_volume(&p), PREAMP_MIN);
    }
}
