//! Shuffle and repeat state for playlist playback.
//!
//! This module provides two things:
//! 1. [`RepeatMode`] — a serialisable enum that controls what happens when the
//!    current track ends (persisted in config).
//! 2. [`ShuffleState`] — session-only state that randomises the playback order
//!    while keeping a history so the "previous" button always works.
//!
//! ## Shuffle algorithm
//!
//! The shuffle is a "no-repeat until all played" draw:
//! - A *played* set tracks which track indices have been heard this pass.
//! - When `next_index` is called, it picks a random track **not** in `played`.
//! - When every track has been played once:
//!   - `RepeatMode::Playlist` → the played set resets and another pass begins.
//!   - `RepeatMode::Off` → returns `None` (playback stops).
//! - Adding or removing a track from the playlist calls `reset()`, which wipes
//!   the played set and history so the new playlist is treated as fresh.
//!
//! ## History (for "previous")
//!
//! Every track that starts playing is appended to `history`.  A `cursor` into
//! `history` tracks which entry the user is logically "at".  Pressing previous
//! decrements the cursor; pressing next or having a track auto-advance appends
//! the new choice and advances the cursor.  This means the user can always
//! step back through the session in shuffle mode exactly as they would in
//! linear mode.

use rand::seq::SliceRandom;
use rand::thread_rng;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// RepeatMode
// ---------------------------------------------------------------------------

/// Controls what happens when the last (or only) track finishes playing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    /// Playback stops when the last track ends.  This is the default.
    #[default]
    Off,
    /// The current track restarts automatically when it ends.
    Song,
    /// The playlist wraps: after the last track, the first (or a new random
    /// track in shuffle mode) starts playing.
    Playlist,
}

impl RepeatMode {
    /// Cycle through Off → Song → Playlist → Off.
    ///
    /// Useful for a single "repeat" button that steps through all modes.
    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::Song,
            Self::Song => Self::Playlist,
            Self::Playlist => Self::Off,
        }
    }

    /// Short human-readable label for UI display (e.g. status bars, buttons).
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Repeat: Off",
            Self::Song => "Repeat: Song",
            Self::Playlist => "Repeat: Playlist",
        }
    }

}

// ---------------------------------------------------------------------------
// ShuffleState
// ---------------------------------------------------------------------------

/// Session-only shuffle bookkeeping — not serialised.
///
/// Because shuffle history is meaningless across restarts, this struct is
/// created fresh on each launch and is never written to disk.
pub struct ShuffleState {
    /// Whether shuffle is currently active.
    pub enabled: bool,
    /// Set of track indices that have been played in the current pass.
    /// Reset when all tracks have been heard (for repeat-playlist) or on
    /// playlist mutation.
    played: HashSet<usize>,
    /// Ordered sequence of all track indices played so far this session.
    /// Acts as an undo/redo stack for the "previous" button.
    history: Vec<usize>,
    /// Where in `history` the user currently is.  Normally points to the last
    /// element; stepping back decrements it; stepping forward appends.
    history_cursor: usize,
    /// The shuffle pick chosen ahead of time by [`Self::peek_next`], so a
    /// server song can be fetched before it plays. The next advance plays
    /// exactly this one while it is still a valid pick.
    pending: Option<usize>,
    /// The two most recently started indices, in both modes: the one before
    /// the current song is kept in the playback cache for "previous".
    last_started: Option<usize>,
    before_last: Option<usize>,
}

impl ShuffleState {
    /// Create a new, disabled shuffle state with no history.
    pub fn new() -> Self {
        ShuffleState {
            enabled: false,
            played: HashSet::new(),
            history: Vec::new(),
            history_cursor: 0,
            pending: None,
            last_started: None,
            before_last: None,
        }
    }

    /// Toggle shuffle on or off.
    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
    }

    /// Reset all shuffle history and the played set.
    ///
    /// Must be called whenever tracks are added to or removed from the playlist
    /// so that the shuffle state reflects the new contents.
    pub fn reset(&mut self) {
        self.played.clear();
        self.history.clear();
        self.history_cursor = 0;
        self.pending = None;
        self.last_started = None;
        self.before_last = None;
    }

    /// The index that started playing before the current one, if any.
    pub fn previous_started(&self) -> Option<usize> {
        self.before_last
    }

    /// The index an automatic advance from `current` would play, without
    /// advancing. In shuffle mode the pick is made now and kept, so the next
    /// advance plays exactly this song (and it can be fetched ahead).
    pub fn peek_next(&mut self, current: usize, total: usize, repeat: RepeatMode) -> Option<usize> {
        if total == 0 {
            return None;
        }
        if repeat == RepeatMode::Song {
            return Some(current);
        }
        if !self.enabled {
            return self.next_linear(current, total, repeat);
        }
        if let Some(p) = self.pending {
            if self.pending_is_valid(p, current, total, repeat) {
                return Some(p);
            }
        }
        let mut available: Vec<usize> =
            (0..total).filter(|i| !self.played.contains(i) && *i != current).collect();
        if available.is_empty() && repeat == RepeatMode::Playlist {
            available = (0..total).filter(|i| *i != current || total == 1).collect();
        }
        available.shuffle(&mut thread_rng());
        self.pending = available.first().copied();
        self.pending
    }

    /// Whether a pick made ahead of time is still a fair next song: not the
    /// one playing, and not heard this pass (unless the pass is over and
    /// repeat starts a new one).
    fn pending_is_valid(&self, p: usize, current: usize, total: usize, repeat: RepeatMode) -> bool {
        let pass_over = (0..total).all(|i| self.played.contains(&i));
        p < total
            && (p != current || total == 1)
            && (!self.played.contains(&p) || (pass_over && repeat == RepeatMode::Playlist))
    }

    /// Record that `index` has started playing.
    ///
    /// Only populates history when shuffle is enabled.
    /// When shuffle is off, history is not used and this method only marks
    /// the track as played.
    pub fn record_played(&mut self, index: usize) {
        if self.last_started != Some(index) {
            self.before_last = self.last_started;
            self.last_started = Some(index);
        }
        if !self.enabled {
            // When shuffle is off, history is not used - just mark as played
            self.played.insert(index);
            return;
        }

        // Truncate any forward history (user stepped back, then a new track
        // started — the old "future" is now stale).
        if !self.history.is_empty() {
            self.history.truncate(self.history_cursor + 1);
        }
        self.history.push(index);
        self.history_cursor = self.history.len() - 1;
        self.played.insert(index);
    }

    /// Determine the next track index to play, given the current position and
    /// the total number of tracks in the playlist.
    ///
    /// Returns:
    /// - `Some(index)` — the index of the next track to play.
    /// - `None` — playback should stop (no more tracks and repeat is off).
    ///
    /// ## Linear mode (shuffle disabled)
    /// - `RepeatMode::Song` → returns `current` unchanged.
    /// - `RepeatMode::Playlist` → wraps to 0 when at the last track.
    /// - `RepeatMode::Off` → returns `None` when at the last track.
    ///
    /// ## Shuffle mode (shuffle enabled)
    /// - Picks a random index not yet in the played set.
    /// - When the played set is full and `RepeatMode::Playlist`, resets the
    ///   set and picks again for a new pass.
    /// - When the played set is full and `RepeatMode::Off`, returns `None`.
    /// - `RepeatMode::Song` is respected the same way as in linear mode.
    pub fn next_index(
        &mut self,
        current: usize,
        total: usize,
        repeat: RepeatMode,
    ) -> Option<usize> {
        if total == 0 {
            return None;
        }

        // Repeat-song is the same regardless of shuffle: replay the same track.
        if repeat == RepeatMode::Song {
            return Some(current);
        }

        if self.enabled {
            self.next_shuffle(current, total, repeat)
        } else {
            self.next_linear(current, total, repeat)
        }
    }

    /// Linear (non-shuffle) next-track logic.
    fn next_linear(&self, current: usize, total: usize, repeat: RepeatMode) -> Option<usize> {
        let next = current + 1;
        if next < total {
            Some(next)
        } else {
            // We are at the last track.
            match repeat {
                RepeatMode::Playlist => Some(0), // wrap to beginning
                RepeatMode::Off | RepeatMode::Song => None,
            }
        }
    }

    /// Shuffle next-track logic: pick a random unplayed track.
    fn next_shuffle(&mut self, current: usize, total: usize, repeat: RepeatMode) -> Option<usize> {
        // A pick made ahead of time (and possibly already fetched) wins.
        if let Some(p) = self.pending.take() {
            if self.pending_is_valid(p, current, total, repeat) {
                if (0..total).all(|i| self.played.contains(&i)) {
                    self.played.clear();
                }
                return Some(p);
            }
        }
        let all_indices: Vec<usize> = (0..total).collect();

        // Collect indices not yet played this pass.
        let mut available: Vec<usize> = all_indices
            .iter()
            .copied()
            .filter(|i| !self.played.contains(i))
            .collect();

        if available.is_empty() {
            // Every track has been played once this pass.
            match repeat {
                RepeatMode::Playlist => {
                    // Start a new pass: clear played set and pick from the full list,
                    // but exclude the track that just finished so we never immediately
                    // repeat it.  A duplicate at the pass boundary creates a fake entry
                    // in the shuffle history, which makes the back-button appear broken.
                    self.played.clear();
                    let without_current: Vec<usize> = all_indices
                        .iter()
                        .copied()
                        .filter(|&i| i != current)
                        .collect();
                    // Fall back to the full list only for a single-track playlist.
                    available = if without_current.is_empty() {
                        all_indices
                    } else {
                        without_current
                    };
                }
                RepeatMode::Off | RepeatMode::Song => {
                    // No more tracks to play.
                    return None;
                }
            }
        }

        // Pick a random track from the available pool.
        available.shuffle(&mut thread_rng());
        available.first().copied()
    }

    /// Determine the previous track index using session history.
    ///
    /// In shuffle mode, "previous" steps back through the history rather than
    /// decrementing the playlist index, so the user hears the exact track they
    /// heard before.  In linear mode the same history is used, which matches
    /// standard (non-shuffle) previous behaviour.
    ///
    /// Returns `Some(index)` if there is history to step back into, or `None`
    /// if we are at the beginning of the session.
    pub fn prev_from_history(&mut self) -> Option<usize> {
        if self.history_cursor > 0 {
            self.history_cursor -= 1;
            self.history.get(self.history_cursor).copied()
        } else {
            // No history available; let the caller fall back to linear prev.
            None
        }
    }

    /// Whether we have any history to step back into.
    pub fn has_history(&self) -> bool {
        self.history_cursor > 0
    }

    /// Walk the history cursor forward without appending.
    ///
    /// Mirrors [`prev_from_history`][Self::prev_from_history] in reverse:
    /// returns `Some(index)` when the user has previously stepped back and
    /// there is a "future" entry to replay, or `None` when the cursor is
    /// already at the head of history (and a fresh shuffle pick is needed).
    ///
    /// Used by manual "next" navigation so that pressing Forward after Back
    /// replays the same track instead of generating a fresh random pick.
    pub fn next_from_history(&mut self) -> Option<usize> {
        if self.history_cursor + 1 < self.history.len() {
            self.history_cursor += 1;
            self.history.get(self.history_cursor).copied()
        } else {
            None
        }
    }

    /// Seed history with the current track if it is empty.
    ///
    /// No-op when shuffle is disabled or history already has entries.  Used
    /// at the top of manual nav so that the first Next while stopped (which
    /// only pre-loads, never recording) leaves something for a subsequent
    /// Back to step into.
    pub fn ensure_seeded(&mut self, idx: usize) {
        if self.enabled && self.history.is_empty() {
            self.record_played(idx);
        }
    }
}

impl Default for ShuffleState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> ShuffleState {
        ShuffleState::new()
    }

    // -----------------------------------------------------------------------
    // Looking ahead (server songs are fetched before they play)
    // -----------------------------------------------------------------------

    #[test]
    fn the_peeked_shuffle_pick_is_the_one_played_next() {
        let mut s = fresh();
        s.enabled = true;
        s.record_played(3);
        let peeked = s.peek_next(3, 20, RepeatMode::Off).unwrap();
        assert_eq!(s.peek_next(3, 20, RepeatMode::Off), Some(peeked), "stable until used");
        assert_eq!(s.next_index(3, 20, RepeatMode::Off), Some(peeked));
    }

    #[test]
    fn a_peeked_pick_that_got_played_another_way_is_replaced() {
        let mut s = fresh();
        s.enabled = true;
        let peeked = s.peek_next(0, 3, RepeatMode::Off).unwrap();
        // The user jumped to exactly that song.
        s.record_played(peeked);
        let next = s.next_index(peeked, 3, RepeatMode::Off).unwrap();
        assert_ne!(next, peeked);
    }

    #[test]
    fn linear_order_peeks_the_next_index() {
        let mut s = fresh();
        assert_eq!(s.peek_next(4, 10, RepeatMode::Off), Some(5));
        assert_eq!(s.peek_next(9, 10, RepeatMode::Playlist), Some(0));
        assert_eq!(s.peek_next(9, 10, RepeatMode::Off), None);
    }

    #[test]
    fn the_song_started_before_this_one_is_remembered_in_both_modes() {
        let mut s = fresh();
        assert_eq!(s.previous_started(), None);
        s.record_played(2);
        s.record_played(5);
        assert_eq!(s.previous_started(), Some(2));
        s.enabled = true;
        s.record_played(7);
        assert_eq!(s.previous_started(), Some(5));
    }

    // -----------------------------------------------------------------------
    // RepeatMode::cycle
    // -----------------------------------------------------------------------

    #[test]
    fn repeat_cycle_off_to_song() {
        assert_eq!(RepeatMode::Off.cycle(), RepeatMode::Song);
    }

    #[test]
    fn repeat_cycle_song_to_playlist() {
        assert_eq!(RepeatMode::Song.cycle(), RepeatMode::Playlist);
    }

    #[test]
    fn repeat_cycle_playlist_to_off() {
        assert_eq!(RepeatMode::Playlist.cycle(), RepeatMode::Off);
    }

    // -----------------------------------------------------------------------
    // Linear next_index
    // -----------------------------------------------------------------------

    #[test]
    fn linear_next_advances() {
        let mut s = fresh();
        assert_eq!(s.next_index(0, 3, RepeatMode::Off), Some(1));
        assert_eq!(s.next_index(1, 3, RepeatMode::Off), Some(2));
    }

    #[test]
    fn linear_next_at_end_off_returns_none() {
        let mut s = fresh();
        assert_eq!(s.next_index(2, 3, RepeatMode::Off), None);
    }

    #[test]
    fn linear_next_at_end_playlist_wraps() {
        let mut s = fresh();
        assert_eq!(s.next_index(2, 3, RepeatMode::Playlist), Some(0));
    }

    #[test]
    fn linear_repeat_song_stays_on_current() {
        let mut s = fresh();
        assert_eq!(s.next_index(1, 3, RepeatMode::Song), Some(1));
    }

    // -----------------------------------------------------------------------
    // Shuffle next_index
    // -----------------------------------------------------------------------

    #[test]
    fn shuffle_picks_within_bounds() {
        let mut s = fresh();
        s.toggle(); // enable shuffle
        for _ in 0..20 {
            let idx = s.next_index(0, 5, RepeatMode::Playlist).unwrap();
            assert!(idx < 5);
            s.record_played(idx);
        }
    }

    #[test]
    fn shuffle_off_returns_none_when_all_played() {
        let mut s = fresh();
        s.toggle();
        // Simulate playing all 3 tracks.
        for i in 0..3 {
            s.played.insert(i);
        }
        assert_eq!(s.next_index(2, 3, RepeatMode::Off), None);
    }

    #[test]
    fn shuffle_playlist_repeats_after_all_played() {
        let mut s = fresh();
        s.toggle();
        // Mark all tracks as played.
        for i in 0..3 {
            s.played.insert(i);
        }
        // With Playlist repeat, should pick something (new pass starts).
        let result = s.next_index(2, 3, RepeatMode::Playlist);
        assert!(result.is_some());
        // played set should have been reset and then one entry added.
        assert!(s.played.is_empty() || s.played.len() <= 1); // reset + possibly one
    }

    #[test]
    fn shuffle_no_duplicate_until_all_played() {
        let mut s = fresh();
        s.toggle();
        let total = 5;
        let mut seen = HashSet::new();
        // Drain all 5 tracks; each should be unique in one pass.
        for _ in 0..total {
            let idx = s.next_index(0, total, RepeatMode::Off).unwrap();
            assert!(
                !seen.contains(&idx),
                "Duplicate index {} in shuffle pass",
                idx
            );
            seen.insert(idx);
            s.record_played(idx);
        }
        assert_eq!(seen.len(), total);
    }

    // -----------------------------------------------------------------------
    // History / previous
    // -----------------------------------------------------------------------

    #[test]
    fn history_prev_steps_back() {
        let mut s = fresh();
        s.toggle(); // Enable shuffle - history only populates when shuffle is ON
        s.record_played(0);
        s.record_played(1);
        s.record_played(2);
        assert_eq!(s.prev_from_history(), Some(1)); // step back to 1
        assert_eq!(s.prev_from_history(), Some(0)); // step back to 0
        assert_eq!(s.prev_from_history(), None); // at beginning
    }

    #[test]
    fn history_no_history_returns_none() {
        let mut s = fresh();
        assert_eq!(s.prev_from_history(), None);
    }

    #[test]
    fn history_reset_clears_everything() {
        let mut s = fresh();
        s.record_played(0);
        s.record_played(1);
        s.reset();
        assert!(!s.has_history());
        assert_eq!(s.prev_from_history(), None);
    }

    #[test]
    fn shuffle_toggle_enables_and_disables() {
        let mut s = fresh();
        assert!(!s.enabled);
        s.toggle();
        assert!(s.enabled);
        s.toggle();
        assert!(!s.enabled);
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn next_index_empty_playlist_returns_none() {
        let mut s = fresh();
        assert_eq!(s.next_index(0, 0, RepeatMode::Playlist), None);
    }

    #[test]
    fn next_index_single_track_song_repeat() {
        let mut s = fresh();
        assert_eq!(s.next_index(0, 1, RepeatMode::Song), Some(0));
    }

    #[test]
    fn next_index_single_track_playlist_repeat_wraps() {
        let mut s = fresh();
        assert_eq!(s.next_index(0, 1, RepeatMode::Playlist), Some(0));
    }

    #[test]
    fn next_index_single_track_off_returns_none() {
        let mut s = fresh();
        assert_eq!(s.next_index(0, 1, RepeatMode::Off), None);
    }

    // -----------------------------------------------------------------------
    // next_from_history / ensure_seeded
    // -----------------------------------------------------------------------

    #[test]
    fn next_from_history_walks_forward_after_prev() {
        let mut s = fresh();
        s.toggle(); // enable shuffle so record_played populates history
        s.record_played(10);
        s.record_played(20);
        s.record_played(30);
        // history = [10, 20, 30], cursor = 2
        assert_eq!(s.prev_from_history(), Some(20));
        assert_eq!(s.prev_from_history(), Some(10));
        // cursor = 0 — now walk forward
        assert_eq!(s.next_from_history(), Some(20));
        assert_eq!(s.next_from_history(), Some(30));
        // cursor = 2 (head) — no future left
        assert_eq!(s.next_from_history(), None);
    }

    #[test]
    fn next_from_history_returns_none_at_head() {
        let mut s = fresh();
        s.toggle();
        s.record_played(5);
        // cursor at last entry — nothing forward
        assert_eq!(s.next_from_history(), None);
    }

    #[test]
    fn next_from_history_returns_none_when_empty() {
        let mut s = fresh();
        s.toggle();
        assert_eq!(s.next_from_history(), None);
    }

    #[test]
    fn record_after_back_truncates_future() {
        // Documented intentional semantic: making a fresh pick after walking
        // back-then-not-all-the-way-forward truncates remaining future.
        let mut s = fresh();
        s.toggle();
        s.record_played(1);
        s.record_played(2);
        s.record_played(3);
        // step back twice: cursor = 0
        s.prev_from_history();
        s.prev_from_history();
        // now record a fresh pick
        s.record_played(99);
        // history should be [1, 99] — old future (2, 3) discarded
        assert_eq!(s.prev_from_history(), Some(1));
        assert_eq!(s.next_from_history(), Some(99));
        assert_eq!(s.next_from_history(), None);
    }

    #[test]
    fn ensure_seeded_no_op_when_disabled() {
        let mut s = fresh();
        // shuffle disabled
        s.ensure_seeded(7);
        assert_eq!(s.next_from_history(), None);
        assert_eq!(s.prev_from_history(), None);
    }

    #[test]
    fn ensure_seeded_no_op_when_already_populated() {
        let mut s = fresh();
        s.toggle();
        s.record_played(1);
        s.ensure_seeded(99); // should NOT add 99
        assert_eq!(s.prev_from_history(), None); // only 1 in history, cursor at 0
    }

    #[test]
    fn ensure_seeded_seeds_when_empty_and_enabled() {
        let mut s = fresh();
        s.toggle();
        s.ensure_seeded(42);
        // history now contains 42 at cursor 0; record another so we can step back
        s.record_played(43);
        assert_eq!(s.prev_from_history(), Some(42));
    }
}
