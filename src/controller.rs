//! Shared playback controller logic.
//!
//! This module contains the navigation and playback decision logic that would
//! otherwise be duplicated across all UI frontends (TUI and GTK4).  Each
//! frontend holds all the necessary state fields directly and obtains a
//! [`Controller`] borrowed view via its own `ctrl()` helper method.
//!
//! ## Design rationale
//!
//! Both frontends need UI-specific fields alongside the shared playback state,
//! so embedding an owned sub-struct would require renaming every field access
//! (e.g. `self.player` → `self.ctrl.player`) throughout both large files.  A
//! borrowed view avoids that churn while still centralising the shared logic
//! here, satisfying the rule that core logic must not live in the UI layer.
//!
//! ## Usage
//!
//! ```ignore
//! // Inside a frontend method:
//! match self.ctrl().nav_next() {
//!     NavResult::Target { was_playing: true } => self.play_current(),
//!     NavResult::Target { was_playing: false } => { /* update UI cursor */ }
//!     NavResult::NoTarget => {}
//! }
//! ```

use std::time::Duration;

use crate::{
    config::{Config, VisualizerMode},
    engine::{Player, PlayerState},
    model::Playlist,
    plugin_manager::PluginManager,
    shuffle::{RepeatMode, ShuffleState},
};

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Outcome of a load-and-play operation.
#[derive(Debug)]
#[allow(dead_code)]
pub enum PlayResult {
    /// Track loaded and playback started successfully.
    Started { display_name: String },
    /// The playlist is empty or the current index is invalid.
    NoTrack,
    /// GStreamer could not load or start the track.  The track has been
    /// marked broken in the playlist so it is skipped on future advances.
    Error(String),
}

/// Outcome of a manual navigation call ([`Controller::nav_next`] /
/// [`Controller::nav_prev`]).
#[derive(Debug)]
pub enum NavResult {
    /// Navigation succeeded.  `was_playing` tells the caller whether to start
    /// playback on the (already-updated) current track.
    Target { was_playing: bool },
    /// No navigation target exists (e.g. at the first track with repeat off,
    /// or at the last track with no wrap).
    NoTarget,
}

/// Outcome of an EOS auto-advance ([`Controller::advance_to_next_playable`]).
#[derive(Debug)]
pub enum AdvanceResult {
    /// A non-broken track was found, loaded, and is now playing.  `new_index`
    /// is the playlist index of that track.
    Playing { new_index: usize },
    /// No playable track could be found; the player has been stopped.
    Stopped,
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

/// A borrowed view over the shared playback state owned by a frontend struct.
///
/// Construct one via the frontend's `ctrl()` helper:
///
/// ```ignore
/// let result = self.ctrl().play_current_no_record();
/// ```
///
/// The view borrows the relevant fields mutably for its lifetime.  The borrows
/// are released as soon as the expression completes, so the caller can access
/// other fields (like TUI-specific `status_message` or GTK-specific
/// `pending_seek`) before and after the call without lifetime conflicts.
pub struct Controller<'a> {
    pub player: &'a mut Player,
    pub playlist: &'a mut Playlist,
    pub config: &'a mut Config,
    pub shuffle_state: &'a mut ShuffleState,
    pub plugin_manager: &'a mut PluginManager,
}

impl Controller<'_> {
    // -----------------------------------------------------------------------
    // Playback
    // -----------------------------------------------------------------------

    /// Load and begin playing the track at `playlist.current_index`.
    ///
    /// Does NOT record the track in the shuffle history.  Use this when
    /// replaying a track that is already in the history (restart or backward
    /// step) so the history cursor is not truncated.
    ///
    /// On load or play failure the track is marked `broken` in the playlist
    /// and `PlayResult::Error` is returned; errors also surface on the next
    /// `poll_bus()` call in the tick loop.
    pub fn play_current_no_record(&mut self) -> PlayResult {
        let Some(track) = self.playlist.current() else {
            return PlayResult::NoTrack;
        };
        let display = track.display_name();
        let uri = track.uri();
        let idx = self.playlist.current_index;
        if let Err(e) = self.player.load(&uri) {
            self.playlist.tracks[idx].broken = true;
            return PlayResult::Error(format!("Load error: {e}"));
        }
        if let Err(e) = self.player.play() {
            self.playlist.tracks[idx].broken = true;
            return PlayResult::Error(format!("Play error: {e}"));
        }
        PlayResult::Started {
            display_name: display,
        }
    }

    /// Record the current track in the shuffle history, then load and play it.
    ///
    /// Use for explicit user-initiated playback (pressing Play, selecting a
    /// track, pressing Next).  For back navigation and restarts use
    /// [`play_current_no_record`][Self::play_current_no_record] instead.
    pub fn play_current(&mut self) -> PlayResult {
        let idx = self.playlist.current_index;
        self.shuffle_state.record_played(idx);
        self.play_current_no_record()
    }

    // -----------------------------------------------------------------------
    // Navigation
    // -----------------------------------------------------------------------

    /// Compute the next target index for manual "next" navigation, jump the
    /// playlist to it, and return whether playback should start.
    ///
    /// `RepeatMode::Song` is treated as `Off` here — it only governs
    /// automatic end-of-stream advance, not manual navigation.
    ///
    /// The caller is responsible for invoking its own play wrapper when
    /// `NavResult::Target { was_playing: true }` is returned.
    pub fn nav_next(&mut self) -> NavResult {
        let was_playing = matches!(
            *self.player.state(),
            PlayerState::Playing | PlayerState::Paused
        );
        let total = self.playlist.len();
        let current = self.playlist.current_index;

        let idx = if self.shuffle_state.enabled {
            // RepeatMode::Song must not lock shuffle-next on the same track.
            let eff = match self.config.playback.repeat_mode {
                RepeatMode::Song => RepeatMode::Off,
                r => r,
            };
            match self.shuffle_state.next_index(current, total, eff) {
                Some(i) => i,
                None => return NavResult::NoTarget,
            }
        } else {
            let next = current + 1;
            if next < total {
                next
            } else if self.config.playback.repeat_mode == RepeatMode::Playlist {
                0
            } else {
                return NavResult::NoTarget;
            }
        };

        self.playlist.jump_to(idx);
        NavResult::Target { was_playing }
    }

    /// Compute the previous target index (or restart position) for manual
    /// "back" navigation, jump the playlist to it, and return whether
    /// playback should start.
    ///
    /// - **≥ 2 s elapsed:** restart semantics — `current_index` is unchanged
    ///   but `was_playing` is propagated so the caller restarts the track.
    /// - **>= 5 s:** restart the current track from the beginning.
    /// - **< 5 s, shuffle on:** step back through the session history.
    /// - **< 5 s, shuffle off:** go to `current − 1`; wraps to the last track
    ///   only under `RepeatMode::Playlist`.
    /// - **At the first track with shuffle off and no wrap:** returns `NavResult::NoTarget`.
    ///
    /// `RepeatMode::Song` does not affect manual back navigation.
    pub fn nav_prev(&mut self) -> NavResult {
        let was_playing = matches!(
            *self.player.state(),
            PlayerState::Playing | PlayerState::Paused
        );
        let pos = self.player.position().unwrap_or(Duration::ZERO);

        if pos.as_secs() >= 5 {
            // Restart current track — index unchanged.
            return NavResult::Target { was_playing };
        }

        let idx = if self.shuffle_state.enabled {
            match self.shuffle_state.prev_from_history() {
                Some(i) => i,
                None => return NavResult::NoTarget,
            }
        } else {
            let current = self.playlist.current_index;
            if current == 0 {
                if self.config.playback.repeat_mode == RepeatMode::Playlist {
                    self.playlist.len().saturating_sub(1)
                } else {
                    return NavResult::NoTarget;
                }
            } else {
                current - 1
            }
        };

        self.playlist.jump_to(idx);
        NavResult::Target { was_playing }
    }

    // -----------------------------------------------------------------------
    // EOS auto-advance
    // -----------------------------------------------------------------------

    /// Advance past the current track after end-of-stream, respecting repeat
    /// and shuffle modes.
    ///
    /// Skips any track already flagged `broken` and also marks as broken any
    /// track whose load or play call fails.  The search is bounded to `total`
    /// iterations to prevent an infinite loop when most tracks are broken.
    ///
    /// Returns `Playing { new_index }` when a track was found and started, or
    /// `Stopped` when there is nothing left to play (the player is also
    /// explicitly stopped in that case).
    pub fn advance_to_next_playable(&mut self) -> AdvanceResult {
        let total = self.playlist.len();
        let current = self.playlist.current_index;
        let repeat = self.config.playback.repeat_mode;

        let Some(mut idx) = self.shuffle_state.next_index(current, total, repeat) else {
            let _ = self.player.stop();
            return AdvanceResult::Stopped;
        };

        for _ in 0..total {
            if self
                .playlist
                .tracks
                .get(idx)
                .map(|t| t.broken)
                .unwrap_or(false)
            {
                // Already marked broken — skip without trying to play.
                self.shuffle_state.record_played(idx);
                match self.shuffle_state.next_index(idx, total, repeat) {
                    Some(i) => {
                        idx = i;
                        continue;
                    }
                    None => {
                        let _ = self.player.stop();
                        return AdvanceResult::Stopped;
                    }
                }
            }

            self.playlist.jump_to(idx);
            let uri = self.playlist.current().map(|t| t.uri()).unwrap_or_default();
            let ok = self.player.load(&uri).is_ok() && self.player.play().is_ok();
            if ok {
                self.shuffle_state.record_played(idx);
                return AdvanceResult::Playing { new_index: idx };
            }
            // Load or play failed — mark broken and try the next candidate.
            self.playlist.tracks[idx].broken = true;
            match self.shuffle_state.next_index(idx, total, repeat) {
                Some(i) => idx = i,
                None => break,
            }
        }

        let _ = self.player.stop();
        AdvanceResult::Stopped
    }

    // -----------------------------------------------------------------------
    // Volume
    // -----------------------------------------------------------------------

    /// Adjust playback volume by `delta`, clamping to `[0.0, 1.0]`.
    ///
    /// Applies the new volume to the player immediately and returns it so the
    /// caller can update any volume slider or label without re-reading state.
    pub fn adjust_volume(&mut self, delta: f64) -> f64 {
        let vol = self.config.playback.adjust_volume(delta);
        self.player.set_volume(vol);
        vol
    }

    // -----------------------------------------------------------------------
    // Equalizer
    // -----------------------------------------------------------------------

    /// Set EQ band `index` to `gain` dB, clamped to `[-12, +12]`.
    ///
    /// Stores the new gain in config and — only when EQ is currently enabled —
    /// applies it to the GStreamer pipeline immediately.  Returns the clamped
    /// value so the caller can update any gain label without re-reading state.
    pub fn set_eq_band(&mut self, index: usize, gain: f64) -> f64 {
        let clamped = self.config.equalizer.set_band_gain(index, gain);
        if self.config.equalizer.enabled {
            self.player.set_eq_band(index, clamped);
        }
        clamped
    }

    /// Set the pre-amp multiplier, clamped to `[0.5, 1.5]`.
    ///
    /// Stores the new value in config and — only when EQ is currently enabled —
    /// applies it to the GStreamer pipeline immediately.  Returns the clamped
    /// value so the caller can update any label without re-reading state.
    pub fn set_preamp(&mut self, mult: f64) -> f64 {
        let clamped = mult.clamp(0.5, 1.5);
        self.config.equalizer.preamp = clamped;
        if self.config.equalizer.enabled {
            self.player.set_preamp(clamped);
        }
        clamped
    }

    /// Set EQ enabled/disabled state and immediately push the effective
    /// pipeline configuration to GStreamer.
    ///
    /// When disabling, sends flat bands and unity pre-amp to the engine.
    /// When re-enabling, restores the stored values.
    pub fn set_eq_enabled(&mut self, enabled: bool) {
        self.config.equalizer.enabled = enabled;
        self.player
            .apply_eq_bands(&self.config.equalizer.effective_bands());
        self.player
            .set_preamp(self.config.equalizer.effective_preamp());
    }

    /// Advance to the next EQ preset (cycling) and apply it to the player
    /// when EQ is currently enabled.
    pub fn cycle_eq_preset(&mut self) {
        self.config.equalizer.cycle_preset();
        if self.config.equalizer.enabled {
            let bands = self.config.equalizer.bands.clone();
            self.player.apply_eq_bands(&bands);
        }
    }

    /// Reset all EQ bands to 0 dB (the "Flat" preset) and apply to the player
    /// unconditionally — the user explicitly requested a reset.
    pub fn reset_eq_to_flat(&mut self) {
        let flat = [0.0f64; 10];
        self.config.equalizer.preset = "Flat".to_string();
        self.config.equalizer.bands = flat.to_vec();
        self.player.apply_eq_bands(&flat);
    }

    // -----------------------------------------------------------------------
    // Seek
    // -----------------------------------------------------------------------

    /// Seek forward (`secs` > 0) or backward (`secs` < 0) within the current
    /// track.  The new position is clamped to `[0, duration]`.  No-op when
    /// position or duration is unavailable (pipeline not loaded).
    pub fn seek_delta_secs(&mut self, secs: f64) {
        if let (Some(pos), Some(dur)) = (self.player.position(), self.player.duration()) {
            let new_secs = (pos.as_secs_f64() + secs).clamp(0.0, dur.as_secs_f64());
            let _ = self.player.seek(Duration::from_secs_f64(new_secs));
        }
    }

    // -----------------------------------------------------------------------
    // Visualizer
    // -----------------------------------------------------------------------

    /// Cycle the visualizer to the next available mode.
    ///
    /// Cycle order: Bars → Oscilloscope → plugin 0 → plugin 1 → … → Bars.
    /// When no plugins are loaded the cycle is simply Bars ↔ Oscilloscope.
    pub fn toggle_visualizer_mode(&mut self) {
        let viz_count = self.plugin_manager.viz_plugins().count();
        match self.plugin_manager.active_viz_index() {
            None => match self.config.visualizer.mode {
                VisualizerMode::Bars => {
                    self.config.visualizer.mode = VisualizerMode::Oscilloscope;
                }
                VisualizerMode::Oscilloscope => {
                    if viz_count > 0 {
                        self.plugin_manager.set_active_viz_index(Some(0));
                    } else {
                        self.config.visualizer.mode = VisualizerMode::Bars;
                    }
                }
            },
            Some(idx) => {
                if idx + 1 < viz_count {
                    self.plugin_manager.set_active_viz_index(Some(idx + 1));
                } else {
                    self.plugin_manager.set_active_viz_index(None);
                    self.config.visualizer.mode = VisualizerMode::Bars;
                }
            }
        }
    }
}
