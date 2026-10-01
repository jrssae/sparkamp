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
//!     NavResult::Target { was_playing: true } => self.play_current_no_record(),
//!     NavResult::Target { was_playing: false } => { /* update UI cursor */ }
//!     NavResult::NoTarget => {}
//! }
//! ```

use std::time::Duration;

use crate::servers::playback::SongNotReady;
use crate::{
    config::{Config, VisualizerMode},
    engine::{Player, PlayerState},
    model::Playlist,
    shuffle::{RepeatMode, ShuffleState},
};

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Outcome of a load-and-play operation.
#[derive(Debug)]
pub enum PlayResult {
    /// Track loaded and playback started successfully.
    Started { display_name: String },
    /// The playlist is empty or the current index is invalid.
    NoTrack,
    /// GStreamer could not load or start the track.  The track has been
    /// marked broken in the playlist so it is skipped on future advances.
    Error(String),
    /// A server song is still downloading. Nothing is marked; play it again
    /// shortly (the frontend retries on its tick).
    Downloading { display_name: String },
    /// No server holding this song can be reached. The entry is marked
    /// unavailable, not broken, and is skipped until the server is back.
    Unavailable(String),
}

/// Outcome of a manual navigation call ([`Controller::nav_next`] /
/// [`Controller::nav_prev`]).
///
/// Recording into shuffle history is owned by `nav_next`/`nav_prev` —
/// callers must trigger playback with
/// [`play_current_no_record`][Controller::play_current_no_record] to avoid
/// double-recording fresh picks.
#[derive(Debug)]
pub enum NavResult {
    /// Navigation succeeded.  `was_playing` tells the caller whether to
    /// start playback on the (already-updated) current track.
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
    /// The next track is a server song still downloading. The playlist is on
    /// it; the frontend retries `play_current_no_record` on a later tick.
    Downloading { new_index: usize },
}

/// Server songs fetched ahead of the play order. Jumping or skipping past
/// them means waiting for a download.
pub const PREFETCH_AHEAD: usize = 2;

/// What one automatic load attempt came to.
enum Attempt {
    Playing,
    Downloading,
    Skip,
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
    /// Manual play queue — drained ahead of shuffle/linear advance. Session
    /// state owned by the frontend; borrowed here for its lifetime.
    pub queue: &'a mut crate::queue::Queue,
    /// Library DB, when open — read only, to look up the track's stored
    /// ReplayGain before each load. `None` (library closed / not yet opened)
    /// simply means playback uses the configured fallback gain.
    pub media_library: Option<&'a crate::media_library::MediaLibrary>,
}

impl Controller<'_> {
    // -----------------------------------------------------------------------
    // Playback
    // -----------------------------------------------------------------------

    /// Feed the current track's stored ReplayGain to the pipeline.
    ///
    /// Must run immediately before `player.load`: `rgvolume` only reads
    /// REPLAYGAIN tags off the decoded stream, so a gain that lives only in the
    /// library reaches playback through the fallback that the load consumes.
    /// Every path that loads a track has to call this — one that forgets plays
    /// the track at its raw level with no visible sign anything was skipped.
    fn prime_gain_for_current(&mut self) {
        let Some(path) = self
            .playlist
            .current()
            .map(|t| t.path.to_string_lossy().into_owned())
        else {
            return;
        };
        let album_mode = crate::config::rg_album_mode(
            self.config.playback.replaygain.source,
            self.config.playback.shuffle_enabled,
        );
        crate::replaygain::prime_player_gain(self.player, self.media_library, &path, album_mode);
    }

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
        self.prime_gain_for_current();
        if let Err(e) = self.player.load(&uri) {
            return match e.downcast_ref::<SongNotReady>() {
                Some(SongNotReady::Downloading) => {
                    // Play it the moment it can: the player keeps the wish
                    // while it waits on the download.
                    let _ = self.player.play();
                    PlayResult::Downloading { display_name: display }
                }
                Some(SongNotReady::Unavailable(why)) => {
                    self.playlist.mark_unavailable(idx);
                    PlayResult::Unavailable(format!("{display}: {why}"))
                }
                None => {
                    self.playlist.tracks[idx].broken = true;
                    PlayResult::Error(format!("Load error: {e}"))
                }
            };
        }
        if let Err(e) = self.player.play() {
            self.playlist.tracks[idx].broken = true;
            return PlayResult::Error(format!("Play error: {e}"));
        }
        self.note_play_context();
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
        // Explicit user-initiated playback cancels a pending stop-after-current
        // (phase 6). Auto-advance uses play_current_no_record, so it is unaffected.
        self.player.set_stop_after_current(false);
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
    /// `NavResult::Target { was_playing: true, .. }` is returned.  Use
    /// [`play_current_no_record`][Self::play_current_no_record] — recording
    /// is owned by this method.
    ///
    /// In shuffle mode, navigation walks the session history first (so
    /// pressing Forward after Back replays the same track), then falls back
    /// to a fresh shuffle pick when at the head of history.  Fresh picks
    /// are recorded into `ShuffleState` here; history walks are not.
    /// Pop the next still-present queued entry and return its current playlist
    /// index, or `None` when the queue is empty (or only holds ids no longer
    /// in the playlist — those are popped and skipped). On a hit the queue has
    /// already been drained of that id. The manual queue takes precedence over
    /// shuffle/linear advance.
    // phase 6: stop-after-current guards ABOVE the callers of this.
    fn queue_next_index(&mut self) -> Option<usize> {
        while let Some(id) = self.queue.pop_next() {
            if let Some(idx) = self.playlist.tracks.iter().position(|t| t.id == id) {
                return Some(idx);
            }
        }
        None
    }

    /// Drop any queued ids whose entries no longer exist in the playlist.
    /// Call after any playlist removal / clear (reorder needs no call — ids are
    /// stable across reorder).
    // Consumed by the frontend playlist remove/clear seams (phase-5 tasks 5/7/8).
    pub fn sync_queue_to_playlist(&mut self) {
        let live: std::collections::HashSet<u64> =
            self.playlist.tracks.iter().map(|t| t.id).collect();
        self.queue.retain_ids(&live);
    }

    pub fn nav_next(&mut self) -> NavResult {
        let was_playing = matches!(
            *self.player.state(),
            PlayerState::Playing | PlayerState::Paused
        );
        let total = self.playlist.len();
        let current = self.playlist.current_index;

        // Manual queue wins over shuffle/linear. A queued hit sets the resume
        // point to that entry's position (jump_to) and is NOT recorded into
        // shuffle history — queue playback is manual, not a shuffle pick.
        if let Some(idx) = self.queue_next_index() {
            self.playlist.jump_to(idx);
            return NavResult::Target { was_playing };
        }

        // In shuffle mode, try walking forward through existing history
        // first.  This is what makes Back-then-Forward replay the same
        // tracks instead of generating new random picks.  Seed history
        // with the current track first so even a fresh stopped-state
        // session leaves something for a subsequent Back to step into.
        if self.shuffle_state.enabled {
            self.shuffle_state.ensure_seeded(current);
            if let Some(idx) = self.shuffle_state.next_from_history() {
                self.playlist.jump_to(idx);
                return NavResult::Target { was_playing };
            }
        }

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
        // Record fresh picks here (not in the FFI layer) so back-navigation
        // works even when the UI only pre-loaded the track without playing.
        self.shuffle_state.record_played(idx);
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

        let current = self.playlist.current_index;
        if self.shuffle_state.enabled {
            self.shuffle_state.ensure_seeded(current);
        }

        let idx = if self.shuffle_state.enabled {
            match self.shuffle_state.prev_from_history() {
                Some(i) => i,
                None => {
                    // Empty history — fall back to a linear step-back without
                    // wrap.  Picking random here would surprise the user
                    // (Back after a shuffled Next must not roll a new track).
                    if current == 0 {
                        return NavResult::NoTarget;
                    }
                    current - 1
                }
            }
        } else if current == 0 {
            if self.config.playback.repeat_mode == RepeatMode::Playlist {
                self.playlist.len().saturating_sub(1)
            } else {
                return NavResult::NoTarget;
            }
        } else {
            current - 1
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

        // Stop-after-current (phase 6, key `t`) wins over queue/shuffle/linear
        // on automatic EOS advance only. `take_` clears the arming so the very
        // next EOS advances normally. Manual next/prev never reach this method.
        if self.player.take_stop_after_current() {
            let _ = self.player.stop();
            return AdvanceResult::Stopped;
        }

        // Manual queue wins over shuffle/linear on auto-advance too. Play the
        // queued entry directly; on load/play failure mark it broken and fall
        // through to the normal advance. Not recorded into shuffle history.
        if let Some(idx) = self.queue_next_index() {
            self.playlist.jump_to(idx);
            match self.try_load_current(idx) {
                Attempt::Playing => {
                    self.note_play_context();
                    return AdvanceResult::Playing { new_index: idx };
                }
                Attempt::Downloading => return AdvanceResult::Downloading { new_index: idx },
                Attempt::Skip => {} // fall through to shuffle/linear advance below
            }
        }

        let Some(mut idx) = self.shuffle_state.next_index(current, total, repeat) else {
            let _ = self.player.stop();
            return AdvanceResult::Stopped;
        };

        for _ in 0..total {
            if self.playlist.tracks.get(idx).map(|t| t.broken).unwrap_or(false)
                || self.playlist.is_unavailable(idx)
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
            match self.try_load_current(idx) {
                Attempt::Playing => {
                    self.shuffle_state.record_played(idx);
                    self.note_play_context();
                    return AdvanceResult::Playing { new_index: idx };
                }
                Attempt::Downloading => return AdvanceResult::Downloading { new_index: idx },
                // Marked broken or unavailable — try the next candidate.
                Attempt::Skip => {}
            }
            match self.shuffle_state.next_index(idx, total, repeat) {
                Some(i) => idx = i,
                None => break,
            }
        }

        let _ = self.player.stop();
        AdvanceResult::Stopped
    }

    /// Server songs to keep in the playback cache and to fetch ahead, as
    /// `(keep, ahead)` song URIs. Keep: the song that played before this one
    /// (for "previous") and this one. Ahead: queued songs first, then the
    /// play order, [`PREFETCH_AHEAD`] songs; in shuffle only the next pick,
    /// chosen now so it is the one that plays. Local files need neither.
    pub fn play_context(&mut self) -> (Vec<String>, Vec<String>) {
        let total = self.playlist.len();
        let current = self.playlist.current_index;
        let repeat = self.config.playback.repeat_mode;

        let mut keep_idx: Vec<usize> = self.shuffle_state.previous_started().into_iter().collect();
        keep_idx.push(current);

        let mut ahead_idx: Vec<usize> = self
            .queue
            .ids()
            .iter()
            .filter_map(|id| self.playlist.tracks.iter().position(|t| t.id == *id))
            .take(PREFETCH_AHEAD)
            .collect();
        if ahead_idx.len() < PREFETCH_AHEAD {
            if self.shuffle_state.enabled {
                ahead_idx.extend(self.shuffle_state.peek_next(current, total, repeat));
            } else {
                let mut at = current;
                while ahead_idx.len() < PREFETCH_AHEAD {
                    match self.shuffle_state.peek_next(at, total, repeat) {
                        Some(next) if next != current && !ahead_idx.contains(&next) => {
                            ahead_idx.push(next);
                            at = next;
                        }
                        _ => break,
                    }
                }
            }
        }

        let uris = |idx: &[usize]| -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            for i in idx {
                if let Some(t) = self.playlist.tracks.get(*i) {
                    let p = t.path.to_string_lossy().into_owned();
                    if crate::model::is_song_uri(&t.path) && !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
            out
        };
        let keep = uris(&keep_idx);
        let ahead: Vec<String> = uris(&ahead_idx).into_iter().filter(|u| !keep.contains(u)).collect();
        (keep, ahead)
    }

    /// Tell the song source what is playing and what comes next, if that
    /// changed. Frontends call this on every tick, so adding, removing or
    /// reordering songs, the play queue, shuffle and repeat all reach the
    /// prefetch without waiting for the next song to start. Nothing is
    /// fetched ahead while stopped.
    pub fn sync_play_context(&mut self) {
        if *self.player.state() == PlayerState::Stopped {
            return;
        }
        self.note_play_context();
    }

    /// Tell the song source what is playing and what comes next, after a
    /// song started.
    fn note_play_context(&mut self) {
        let (keep, ahead) = self.play_context();
        crate::servers::playback::note_play_context(&keep, &ahead);
    }

    /// Load and play the current entry (`idx`) for an automatic advance. A
    /// failure marks the entry broken, or unavailable when its server could
    /// not be reached.
    fn try_load_current(&mut self, idx: usize) -> Attempt {
        let uri = self.playlist.current().map(|t| t.uri()).unwrap_or_default();
        self.prime_gain_for_current();
        match self.player.load(&uri) {
            Ok(()) if self.player.play().is_ok() => return Attempt::Playing,
            Ok(()) => {}
            Err(e) => match e.downcast_ref::<SongNotReady>() {
                Some(SongNotReady::Downloading) => {
                    let _ = self.player.play();
                    return Attempt::Downloading;
                }
                Some(SongNotReady::Unavailable(_)) => {
                    self.playlist.mark_unavailable(idx);
                    return Attempt::Skip;
                }
                None => {}
            },
        }
        self.playlist.tracks[idx].broken = true;
        Attempt::Skip
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

    /// Cycle the visualizer to the next built-in mode.
    ///
    /// Cycle order: Bars → Waveform → Granite → Bars.
    pub fn toggle_visualizer_mode(&mut self) {
        self.config.visualizer.mode = match self.config.visualizer.mode {
            VisualizerMode::Bars => VisualizerMode::Waveform,
            VisualizerMode::Waveform => VisualizerMode::Granite,
            VisualizerMode::Granite => VisualizerMode::Bars,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;
    use std::path::PathBuf;

    /// Everything a `Controller` borrows, owned by the test. The Player needs
    /// GStreamer initialized (element creation only — nothing plays).
    struct Fixture {
        player: Player,
        playlist: Playlist,
        config: Config,
        shuffle: ShuffleState,
        queue: crate::queue::Queue,
    }

    impl Fixture {
        fn new(tracks: usize) -> Self {
            // Linux plays through GStreamer, so its tests need it up. macOS
            // plays through AVFoundation and does not link GStreamer at all.
            #[cfg(not(target_os = "macos"))]
            #[cfg(not(target_os = "macos"))]
            gstreamer::init().expect("GStreamer must be available for tests");
            let mut playlist = Playlist::new();
            for i in 0..tracks {
                playlist.add(Track {
                    path: PathBuf::from(format!("/fake/{i}.mp3")),
                    title: format!("T{i}"),
                    artist: String::new(),
                    album_artist: String::new(),
                    album: String::new(),
                    duration: None,
                    broken: false,
                    read_only: false,
                    id: 0,
                });
            }
            Fixture {
                player: Player::new().expect("Player::new"),
                playlist,
                config: Config::default(),
                shuffle: ShuffleState::new(),
                queue: crate::queue::Queue::new(),
            }
        }

        fn ctrl(&mut self) -> Controller<'_> {
            Controller {
                player: &mut self.player,
                playlist: &mut self.playlist,
                config: &mut self.config,
                shuffle_state: &mut self.shuffle,
                queue: &mut self.queue,
                media_library: None,
            }
        }
    }

    fn server_track(path: &str) -> Track {
        Track {
            path: PathBuf::from(path),
            title: "server".into(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            duration: None,
            broken: false,
            read_only: false,
            id: 0,
        }
    }

    fn song(i: usize) -> String {
        format!("subsonic://oscar//music/{i}.mp3")
    }

    fn with_server_songs(n: usize) -> Fixture {
        let mut f = Fixture::new(0);
        for i in 0..n {
            f.playlist.add(server_track(&song(i)));
        }
        f
    }

    #[test]
    fn playing_in_order_keeps_the_song_before_and_fetches_the_next_two() {
        let mut f = with_server_songs(5);
        f.shuffle.record_played(0);
        f.shuffle.record_played(1);
        f.playlist.jump_to(1);
        let (keep, ahead) = f.ctrl().play_context();
        assert_eq!(keep, vec![song(0), song(1)]);
        assert_eq!(ahead, vec![song(2), song(3)]);
    }

    #[test]
    fn queued_songs_are_fetched_first() {
        let mut f = with_server_songs(5);
        f.playlist.jump_to(1);
        let id = f.playlist.tracks[4].id;
        f.queue.enqueue(id);
        let (_, ahead) = f.ctrl().play_context();
        assert_eq!(ahead, vec![song(4), song(2)]);
    }

    #[test]
    fn in_shuffle_the_song_fetched_ahead_is_the_one_that_plays_next() {
        let mut f = with_server_songs(10);
        f.shuffle.enabled = true;
        f.shuffle.record_played(0);
        f.playlist.jump_to(0);
        let (_, ahead) = f.ctrl().play_context();
        assert_eq!(ahead.len(), 1);
        f.ctrl().nav_next();
        let now = f.playlist.tracks[f.playlist.current_index].path.to_string_lossy().into_owned();
        assert_eq!(now, ahead[0]);
    }

    #[test]
    fn local_files_are_neither_kept_nor_fetched() {
        let mut f = Fixture::new(4);
        f.shuffle.record_played(0);
        f.shuffle.record_played(1);
        f.playlist.jump_to(1);
        assert_eq!(f.ctrl().play_context(), (vec![], vec![]));
    }

    /// Every context the shared test source was told about, newest last.
    fn told() -> Vec<(Vec<String>, Vec<String>)> {
        crate::servers::playback::recorded_play_contexts()
    }

    /// A playlist whose first entry is a real, silent WAV the engine can
    /// open, so the fixture can be put into the playing state.
    fn playing_a_local_file() -> (Fixture, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("silence.wav");
        let frames: u32 = 44_100 / 2;
        let data = frames * 4;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&2u16.to_le_bytes()); // stereo
        wav.extend_from_slice(&44_100u32.to_le_bytes());
        wav.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data.to_le_bytes());
        wav.resize(wav.len() + data as usize, 0);
        std::fs::write(&path, wav).unwrap();
        let mut f = Fixture::new(0);
        let mut t = server_track(&path.to_string_lossy());
        t.title = "silence".into();
        f.playlist.add(t);
        f.config.playback.volume = 0.0;
        (f, dir)
    }

    #[test]
    fn adding_a_server_song_after_the_playing_one_fetches_it_without_waiting_for_a_track_change() {
        crate::servers::playback::install_test_answers();
        let (mut f, _dir) = playing_a_local_file();
        let r = f.ctrl().play_current();
        assert!(matches!(r, PlayResult::Started { .. }), "{r:?}");
        let added = "subsonic://oscar//music/sync-added-after-playing.mp3";
        f.playlist.add(server_track(added));
        f.ctrl().sync_play_context();
        assert!(
            told().iter().any(|(_, ahead)| ahead.iter().any(|u| u == added)),
            "the added song is fetched ahead"
        );
    }

    #[test]
    fn nothing_is_fetched_ahead_while_stopped() {
        crate::servers::playback::install_test_answers();
        let (mut f, _dir) = playing_a_local_file();
        let added = "subsonic://oscar//music/sync-added-while-stopped.mp3";
        f.playlist.add(server_track(added));
        f.ctrl().sync_play_context();
        assert!(!told().iter().any(|(_, ahead)| ahead.iter().any(|u| u == added)));
    }

    #[test]
    fn removing_the_next_server_song_stops_fetching_it() {
        crate::servers::playback::install_test_answers();
        let (mut f, _dir) = playing_a_local_file();
        let gone = "subsonic://oscar//music/sync-removed.mp3";
        let after = "subsonic://oscar//music/sync-after-the-removed-one.mp3";
        f.playlist.add(server_track(gone));
        f.playlist.add(server_track(after));
        let r = f.ctrl().play_current();
        assert!(matches!(r, PlayResult::Started { .. }), "{r:?}");
        f.ctrl().sync_play_context();
        f.playlist.remove(1);
        f.ctrl().sync_play_context();
        let log = told();
        let with = log.iter().position(|(_, a)| a == &vec![gone.to_string(), after.to_string()]);
        let without = log.iter().rposition(|(_, a)| a == &vec![after.to_string()]);
        assert!(with.is_some() && without > with, "{log:?}");
    }

    #[test]
    fn a_server_song_still_downloading_is_not_broken() {
        crate::servers::playback::install_test_answers();
        let mut f = Fixture::new(0);
        f.playlist.add(server_track("subsonic://oscar//music/wait.mp3"));
        let r = f.ctrl().play_current();
        assert!(matches!(r, PlayResult::Downloading { .. }), "{r:?}");
        assert!(!f.playlist.tracks[0].broken);
        assert!(!f.playlist.is_unavailable(0));
    }

    #[test]
    fn an_unreachable_server_song_is_unavailable_not_broken() {
        crate::servers::playback::install_test_answers();
        let mut f = Fixture::new(0);
        f.playlist.add(server_track("subsonic://oscar//music/gone.mp3"));
        let r = f.ctrl().play_current();
        assert!(matches!(r, PlayResult::Unavailable(_)), "{r:?}");
        assert!(!f.playlist.tracks[0].broken);
        assert!(f.playlist.is_unavailable(0));
        f.playlist.clear_unavailable();
        assert!(!f.playlist.is_unavailable(0), "the server came back");
    }

    #[test]
    fn advancing_skips_unreachable_server_songs_without_breaking_them() {
        crate::servers::playback::install_test_answers();
        let mut f = Fixture::new(1);
        f.playlist.add(server_track("subsonic://oscar//music/gone.mp3"));
        f.playlist.add(server_track("subsonic://oscar//music/also-gone.mp3"));
        f.playlist.jump_to(0);
        f.ctrl().advance_to_next_playable();
        assert!(f.playlist.is_unavailable(1) && f.playlist.is_unavailable(2));
        assert!(!f.playlist.tracks[1].broken && !f.playlist.tracks[2].broken);
    }

    #[test]
    fn advancing_onto_a_song_still_downloading_waits_on_it() {
        crate::servers::playback::install_test_answers();
        let mut f = Fixture::new(1);
        f.playlist.add(server_track("subsonic://oscar//music/wait.mp3"));
        f.playlist.jump_to(0);
        let r = f.ctrl().advance_to_next_playable();
        assert!(matches!(r, AdvanceResult::Downloading { new_index: 1 }), "{r:?}");
        assert_eq!(f.playlist.current_index, 1);
        assert!(!f.playlist.tracks[1].broken);
    }

    #[test]
    fn queued_entries_play_before_linear_then_resume_from_position() {
        // playlist [T0,T1,T2,T3]; queue T2 then T0.
        let mut f = Fixture::new(4);
        let id_t0 = f.playlist.tracks[0].id;
        let id_t2 = f.playlist.tracks[2].id;
        f.queue.enqueue(id_t2);
        f.queue.enqueue(id_t0);

        // Drain T2.
        assert!(matches!(f.ctrl().nav_next(), NavResult::Target { .. }));
        assert_eq!(f.playlist.current_index, 2, "queued T2 plays first");
        // Drain T0.
        assert!(matches!(f.ctrl().nav_next(), NavResult::Target { .. }));
        assert_eq!(f.playlist.current_index, 0, "queued T0 plays next");
        assert!(f.queue.is_empty(), "queue drained");
        // Queue empty → linear resumes from T0's position → T1.
        assert!(matches!(f.ctrl().nav_next(), NavResult::Target { .. }));
        assert_eq!(f.playlist.current_index, 1, "linear resumes from last-queued position");
    }

    #[test]
    fn stop_after_current_halts_eos_advance_before_queue() {
        // playlist [T0,T1,T2]; queue T2, arm stop-after-current.
        let mut f = Fixture::new(3);
        let id_t2 = f.playlist.tracks[2].id;
        f.queue.enqueue(id_t2);
        f.player.set_stop_after_current(true);

        let result = f.ctrl().advance_to_next_playable();
        assert!(matches!(result, AdvanceResult::Stopped), "armed EOS stops");
        assert!(!f.player.stop_after_current(), "flag cleared after firing");
        // Queue NOT consumed by the halt — the guard returns before
        // queue_next_index, so the queued entry is still pending for next play.
        assert_eq!(f.queue.len(), 1, "stop-after-current wins over the queue");
        assert!(f.queue.contains(id_t2), "queued track still present");
    }

    #[test]
    fn play_current_clears_stop_after_current() {
        let mut f = Fixture::new(2);
        f.player.set_stop_after_current(true);
        let _ = f.ctrl().play_current();
        assert!(!f.player.stop_after_current(), "manual play cancels the arming");
    }

    #[test]
    fn reorder_leaves_the_queue_intact() {
        let mut f = Fixture::new(4);
        let id2 = f.playlist.tracks[2].id;
        f.queue.enqueue(id2);
        f.playlist.reverse();
        // Queue still holds id2; it now resolves to a different index but the
        // same track.
        assert!(f.queue.contains(id2));
        assert!(f.playlist.tracks.iter().any(|t| t.id == id2));
    }

    #[test]
    fn queue_wins_over_shuffle() {
        let mut f = Fixture::new(4);
        f.shuffle.enabled = true;
        let id_t3 = f.playlist.tracks[3].id;
        f.queue.enqueue(id_t3);
        // Even with shuffle on, the queued entry is what plays next.
        assert!(matches!(f.ctrl().nav_next(), NavResult::Target { .. }));
        assert_eq!(f.playlist.current_index, 3, "queue beats shuffle");
        assert!(f.queue.is_empty());
    }

    #[test]
    fn removing_a_queued_track_dequeues_it() {
        let mut f = Fixture::new(3);
        let id_t1 = f.playlist.tracks[1].id;
        f.queue.enqueue(id_t1);
        // Remove T1 from the playlist, then sync.
        f.playlist.tracks.remove(1);
        f.ctrl().sync_queue_to_playlist();
        assert!(!f.queue.contains(id_t1), "removed track leaves the queue");
        assert!(f.queue.is_empty());
    }

    #[test]
    fn queue_next_index_skips_ids_no_longer_present() {
        let mut f = Fixture::new(3);
        let id_gone = f.playlist.tracks[1].id;
        let id_t2 = f.playlist.tracks[2].id;
        f.queue.enqueue(id_gone);
        f.queue.enqueue(id_t2);
        // Remove the first-queued track from the playlist.
        f.playlist.tracks.remove(1);
        // nav_next pops the missing id, skips it, lands on T2 (now index 1).
        assert!(matches!(f.ctrl().nav_next(), NavResult::Target { .. }));
        assert_eq!(f.playlist.tracks[f.playlist.current_index].id, id_t2);
        assert!(f.queue.is_empty());
    }

    #[test]
    fn adjust_volume_clamps_both_ends_and_returns_the_result() {
        let mut f = Fixture::new(0);
        assert_eq!(f.ctrl().adjust_volume(10.0), 1.0);
        assert_eq!(f.ctrl().adjust_volume(-10.0), 0.0);
        let v = f.ctrl().adjust_volume(0.25);
        assert!((v - 0.25).abs() < 1e-9);
        assert!((f.config.playback.volume - 0.25).abs() < 1e-9);
    }

    #[test]
    fn set_eq_band_clamps_to_plus_minus_12_and_stores() {
        let mut f = Fixture::new(0);
        assert_eq!(f.ctrl().set_eq_band(0, 40.0), 12.0);
        assert_eq!(f.ctrl().set_eq_band(0, -40.0), -12.0);
        assert_eq!(f.ctrl().set_eq_band(3, 5.5), 5.5);
        assert_eq!(f.config.equalizer.bands[3], 5.5);
    }

    #[test]
    fn set_preamp_clamps_to_half_through_one_and_a_half() {
        let mut f = Fixture::new(0);
        assert_eq!(f.ctrl().set_preamp(9.0), 1.5);
        assert_eq!(f.ctrl().set_preamp(0.0), 0.5);
        assert_eq!(f.ctrl().set_preamp(1.2), 1.2);
        assert_eq!(f.config.equalizer.preamp, 1.2);
    }

    #[test]
    fn reset_eq_to_flat_zeroes_every_band_and_names_the_preset() {
        let mut f = Fixture::new(0);
        f.ctrl().set_eq_band(2, 8.0);
        f.ctrl().reset_eq_to_flat();
        assert!(f.config.equalizer.bands.iter().all(|b| *b == 0.0));
        assert_eq!(f.config.equalizer.preset, "Flat");
    }

    #[test]
    fn cycle_eq_preset_changes_the_stored_preset() {
        let mut f = Fixture::new(0);
        let before = f.config.equalizer.preset.clone();
        f.ctrl().cycle_eq_preset();
        assert_ne!(f.config.equalizer.preset, before);
    }

    #[test]
    fn seek_delta_without_a_loaded_pipeline_is_a_noop() {
        // Stopped player has no position/duration — must not panic or seek.
        let mut f = Fixture::new(1);
        f.ctrl().seek_delta_secs(30.0);
        f.ctrl().seek_delta_secs(-30.0);
    }

    #[test]
    fn play_current_with_empty_playlist_reports_no_track() {
        let mut f = Fixture::new(0);
        assert!(matches!(f.ctrl().play_current(), PlayResult::NoTrack));
    }
}
