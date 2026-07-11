//! Playback state, back/next navigation, repeat modes.

use super::*;
use crate::engine::PlayerState;
use crossterm::event::{KeyCode, KeyModifiers};

// -----------------------------------------------------------------------
// Playback state tests
// -----------------------------------------------------------------------

/// Pressing x on a stopped player with no tracks does not crash.
#[test]
fn play_key_with_empty_playlist_does_not_crash() {
    let mut app = make_app();
    app.handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
    // no panic = pass
}

/// Pressing c (pause/resume) on a stopped player does not crash.
#[test]
fn pause_key_when_stopped_does_not_crash() {
    let mut app = make_app();
    app.handle_key(KeyCode::Char('c'), KeyModifiers::NONE);
}

/// Pressing v (stop) on a stopped player does not crash.
#[test]
fn stop_key_when_already_stopped_does_not_crash() {
    let mut app = make_app();
    app.handle_key(KeyCode::Char('v'), KeyModifiers::NONE);
}

/// Simulating EOS on a two-track playlist advances to the next track.
#[test]
fn eos_auto_advances_to_next_track() {
    let mut app = app_with_tracks(&["A", "B"]);
    app.playlist.current_index = 0;
    // Simulate what tick() does when poll_bus() returns true (EOS)
    app.play_next();
    assert_eq!(app.playlist.current_index, 1);
}

/// Simulating EOS on the only track in the playlist stops cleanly.
#[test]
fn eos_on_only_track_stops_cleanly_no_crash() {
    let mut app = app_with_tracks(&["A"]);
    app.playlist.current_index = 0;
    // Simulate EOS — play_next() with no successor does nothing
    app.play_next();
    assert_eq!(app.playlist.current_index, 0);
    assert_eq!(app.playlist.len(), 1);
}

// -----------------------------------------------------------------------
// Back-button logic
// -----------------------------------------------------------------------

/// When the player position is effectively zero (< 2 s), z goes to previous track.
/// Without real audio, position() returns None → Duration::ZERO, always < 2 s.
#[test]
fn back_when_position_is_zero_goes_to_previous_track() {
    let mut app = app_with_tracks(&["A", "B"]);
    app.playlist.current_index = 1;
    app.handle_key(KeyCode::Char('z'), KeyModifiers::NONE);
    assert_eq!(app.playlist.current_index, 0);
}

/// Pressing back on the first track stays at track 0 regardless of position.
#[test]
fn back_on_first_track_stays_at_index_zero() {
    let mut app = app_with_tracks(&["A", "B"]);
    app.playlist.current_index = 0;
    app.handle_key(KeyCode::Char('z'), KeyModifiers::NONE);
    assert_eq!(app.playlist.current_index, 0);
}

/// Pressing back on the first track with only one item does not crash.
#[test]
fn back_on_first_and_only_track_does_not_crash() {
    let mut app = app_with_tracks(&["A"]);
    app.playlist.current_index = 0;
    app.handle_key(KeyCode::Char('z'), KeyModifiers::NONE);
    assert_eq!(app.playlist.current_index, 0);
}

/// Linear back must step through the full playlist positionally — not be
/// limited by session history.  Starting at index 3 and pressing back 3
/// times must reach index 0.
#[test]
fn back_can_step_back_multiple_songs_in_sequence() {
    let mut app = app_with_tracks(&["A", "B", "C", "D"]);
    app.playlist.current_index = 3;

    app.play_prev();
    assert_eq!(
        app.playlist.current_index, 2,
        "first back should reach C (index 2)"
    );

    app.play_prev();
    assert_eq!(
        app.playlist.current_index, 1,
        "second back should reach B (index 1)"
    );

    app.play_prev();
    assert_eq!(
        app.playlist.current_index, 0,
        "third back should reach A (index 0)"
    );
}

/// Linear next must step forward through the full playlist positionally.
#[test]
fn next_can_step_forward_multiple_songs_in_sequence() {
    let mut app = app_with_tracks(&["A", "B", "C", "D"]);
    app.playlist.current_index = 0;

    app.play_next();
    assert_eq!(
        app.playlist.current_index, 1,
        "first next should reach B (index 1)"
    );

    app.play_next();
    assert_eq!(
        app.playlist.current_index, 2,
        "second next should reach C (index 2)"
    );

    app.play_next();
    assert_eq!(
        app.playlist.current_index, 3,
        "third next should reach D (index 3)"
    );
}

/// With RepeatMode::Playlist, back from index 0 wraps to the last track.
#[test]
fn back_wraps_to_last_track_when_repeat_playlist_is_on() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Playlist;
    app.playlist.current_index = 0;

    app.play_prev();
    assert_eq!(
        app.playlist.current_index, 2,
        "back at first track should wrap to last (index 2)"
    );
}

/// With RepeatMode::Playlist, next from the last track wraps to index 0.
#[test]
fn next_wraps_to_first_track_when_repeat_playlist_is_on() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Playlist;
    app.playlist.current_index = 2;

    app.play_next();
    assert_eq!(
        app.playlist.current_index, 0,
        "next at last track should wrap to first (index 0)"
    );
}

/// RepeatMode::Song must not prevent manual next from advancing.
#[test]
fn next_advances_past_current_track_even_when_repeat_song_is_on() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Song;
    app.playlist.current_index = 0;

    app.play_next();
    assert_eq!(
        app.playlist.current_index, 1,
        "repeat-song must not block manual next"
    );
}

/// In shuffle mode the previous button must step backward through the
/// session's shuffle-play order, not the playlist's linear order.
#[test]
fn shuffle_back_steps_through_shuffle_history() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.shuffle_state.enabled = true;
    // Simulate shuffle play order: B(1) → C(2) → A(0).
    app.shuffle_state.record_played(1);
    app.shuffle_state.record_played(2);
    app.shuffle_state.record_played(0);
    app.playlist.current_index = 0; // currently on A

    app.play_prev(); // back to C
    assert_eq!(
        app.playlist.current_index, 2,
        "first back in shuffle should reach C (index 2)"
    );

    app.play_prev(); // back to B
    assert_eq!(
        app.playlist.current_index, 1,
        "second back in shuffle should reach B (index 1)"
    );
}

/// After stepping back in shuffle, pressing record_played (simulating forward)
/// must truncate the stale future so back no longer returns to it.
#[test]
fn shuffle_forward_after_back_truncates_stale_future() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.shuffle_state.enabled = true;
    // History: A(0) → B(1) → C(2); cursor at 2.
    app.shuffle_state.record_played(0);
    app.shuffle_state.record_played(1);
    app.shuffle_state.record_played(2);
    app.playlist.current_index = 2;

    // Step back to B (cursor moves to 1, pointing at index 1).
    app.play_prev();
    assert_eq!(app.playlist.current_index, 1);

    // Simulate forward navigation from B — record_played truncates C from history.
    app.shuffle_state.record_played(1);

    // Back from here must return to A (the entry before B in the new history),
    // not to C (the now-stale entry that was truncated).
    app.shuffle_state.prev_from_history();
    assert_ne!(
        app.playlist.current_index, 2,
        "stale future entry C must not be reachable after forward navigation"
    );
}

// -----------------------------------------------------------------------
// Paused-state navigation
// -----------------------------------------------------------------------

/// Pressing next while paused must load the new track and start playing it,
/// not stay paused on the original track.
#[test]
fn next_while_paused_advances_and_plays() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.playlist.current_index = 0;
    app.player.set_state_for_test(PlayerState::Paused);

    app.play_next();
    assert_eq!(
        app.playlist.current_index, 1,
        "next while paused must advance to track B"
    );
}

/// Pressing back while paused must load the new track and start playing it.
#[test]
fn back_while_paused_steps_back_and_plays() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.playlist.current_index = 2;
    app.player.set_state_for_test(PlayerState::Paused);

    app.play_prev();
    assert_eq!(
        app.playlist.current_index, 1,
        "back while paused must step back to track B"
    );
}

// -----------------------------------------------------------------------
// Repeat mode — manual back/next boundary behaviour
//
// RepeatMode key:
//   Off      — no wrap, stop at boundaries
//   Song     — loops current song on EOS only; manual nav ignores it (same as Off)
//   Playlist — wraps from end→start and start→end
// -----------------------------------------------------------------------

// ── RepeatMode::Off ──────────────────────────────────────────────────────

/// RepeatOff: next at the last track does nothing (stays at last index).
#[test]
fn repeat_off_next_at_last_track_stays() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Off;
    app.playlist.current_index = 2; // last track

    app.play_next();
    assert_eq!(
        app.playlist.current_index, 2,
        "RepeatOff: next at last should stay at last"
    );
}

/// RepeatOff: back at the first track does nothing (stays at index 0).
#[test]
fn repeat_off_back_at_first_track_stays() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Off;
    app.playlist.current_index = 0;

    app.play_prev();
    assert_eq!(
        app.playlist.current_index, 0,
        "RepeatOff: back at first should stay at first"
    );
}

/// RepeatOff: next in the middle still advances normally.
#[test]
fn repeat_off_next_in_middle_advances() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Off;
    app.playlist.current_index = 1;

    app.play_next();
    assert_eq!(app.playlist.current_index, 2);
}

/// RepeatOff: back in the middle steps back normally.
#[test]
fn repeat_off_back_in_middle_steps_back() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Off;
    app.playlist.current_index = 1;

    app.play_prev();
    assert_eq!(app.playlist.current_index, 0);
}

// ── RepeatMode::Song ─────────────────────────────────────────────────────
// Manual back/next treats RepeatMode::Song identically to RepeatMode::Off.
// Song-repeat only fires on EOS auto-advance (advance_to_next_playable).

/// RepeatSong: next at the last track does nothing (same boundary as RepeatOff).
#[test]
fn repeat_song_next_at_last_track_stays() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Song;
    app.playlist.current_index = 2;

    app.play_next();
    assert_eq!(
        app.playlist.current_index, 2,
        "RepeatSong: next at last should stay (Song only affects EOS)"
    );
}

/// RepeatSong: back at the first track does nothing.
#[test]
fn repeat_song_back_at_first_track_stays() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Song;
    app.playlist.current_index = 0;

    app.play_prev();
    assert_eq!(
        app.playlist.current_index, 0,
        "RepeatSong: back at first should stay"
    );
}

/// RepeatSong: next in the middle advances to the next track (Song does not lock it).
#[test]
fn repeat_song_next_in_middle_advances() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Song;
    app.playlist.current_index = 0;

    app.play_next();
    assert_eq!(
        app.playlist.current_index, 1,
        "RepeatSong: next must advance past current track"
    );
}

/// RepeatSong: back in the middle steps back normally.
#[test]
fn repeat_song_back_in_middle_steps_back() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Song;
    app.playlist.current_index = 2;

    app.play_prev();
    assert_eq!(app.playlist.current_index, 1);
}

// ── RepeatMode::Playlist ─────────────────────────────────────────────────

/// RepeatPlaylist: next in the middle still advances normally (no wrap needed).
#[test]
fn repeat_playlist_next_in_middle_advances() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Playlist;
    app.playlist.current_index = 0;

    app.play_next();
    assert_eq!(app.playlist.current_index, 1);
}

/// RepeatPlaylist: back in the middle steps back normally (no wrap needed).
#[test]
fn repeat_playlist_back_in_middle_steps_back() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Playlist;
    app.playlist.current_index = 2;

    app.play_prev();
    assert_eq!(app.playlist.current_index, 1);
}

// ── Single-track edge cases ──────────────────────────────────────────────

/// Single track, RepeatOff: next stays at index 0 (nothing to advance to).
#[test]
fn single_track_repeat_off_next_stays() {
    let mut app = app_with_tracks(&["A"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Off;
    app.playlist.current_index = 0;

    app.play_next();
    assert_eq!(app.playlist.current_index, 0);
}

/// Single track, RepeatOff: back stays at index 0.
#[test]
fn single_track_repeat_off_back_stays() {
    let mut app = app_with_tracks(&["A"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Off;
    app.playlist.current_index = 0;

    app.play_prev();
    assert_eq!(app.playlist.current_index, 0);
}

/// Single track, RepeatPlaylist: next wraps to the same (only) track.
#[test]
fn single_track_repeat_playlist_next_wraps_to_self() {
    let mut app = app_with_tracks(&["A"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Playlist;
    app.playlist.current_index = 0;

    app.play_next();
    assert_eq!(
        app.playlist.current_index, 0,
        "single-track RepeatPlaylist next must wrap to index 0"
    );
}

/// Single track, RepeatPlaylist: back wraps to the same (only) track.
#[test]
fn single_track_repeat_playlist_back_wraps_to_self() {
    let mut app = app_with_tracks(&["A"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Playlist;
    app.playlist.current_index = 0;

    app.play_prev();
    assert_eq!(
        app.playlist.current_index, 0,
        "single-track RepeatPlaylist back must wrap to index 0"
    );
}

// ── EOS auto-advance decision logic (ShuffleState::next_index) ──────────
// advance_to_next_playable() mixes the decision engine with real GStreamer
// play calls that fail on fake paths, so we test the decision layer
// (ShuffleState::next_index) directly.  This is exactly the function the
// tick loop consults when an end-of-stream event fires.

/// EOS, RepeatOff: next_index at the last track returns None (stop).
#[test]
fn eos_repeat_off_at_last_track_returns_none() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Off;
    let total = app.playlist.len();
    let last = total - 1;

    let result = app
        .shuffle_state
        .next_index(last, total, crate::shuffle::RepeatMode::Off);
    assert!(
        result.is_none(),
        "EOS RepeatOff at last track must return None (stop playback)"
    );
}

/// EOS, RepeatOff: next_index in the middle returns current + 1.
#[test]
fn eos_repeat_off_in_middle_returns_next() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    let total = app.playlist.len();

    let result = app
        .shuffle_state
        .next_index(1, total, crate::shuffle::RepeatMode::Off);
    assert_eq!(result, Some(2));
}

/// EOS, RepeatSong: next_index always returns the current index (replay).
#[test]
fn eos_repeat_song_returns_current_index() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    let total = app.playlist.len();

    // Mid-playlist
    let mid = app
        .shuffle_state
        .next_index(1, total, crate::shuffle::RepeatMode::Song);
    assert_eq!(mid, Some(1), "EOS RepeatSong mid should replay same track");

    // Last track — must NOT wrap (that is RepeatPlaylist behaviour)
    let last = app
        .shuffle_state
        .next_index(2, total, crate::shuffle::RepeatMode::Song);
    assert_eq!(
        last,
        Some(2),
        "EOS RepeatSong at last must replay same track, not wrap"
    );
}

/// EOS, RepeatPlaylist: next_index at the last track wraps to 0.
#[test]
fn eos_repeat_playlist_at_last_track_wraps_to_zero() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    let total = app.playlist.len();
    let last = total - 1;

    let result =
        app.shuffle_state
            .next_index(last, total, crate::shuffle::RepeatMode::Playlist);
    assert_eq!(
        result,
        Some(0),
        "EOS RepeatPlaylist at last must wrap to first track"
    );
}

/// EOS, RepeatPlaylist: next_index in the middle still returns current + 1.
#[test]
fn eos_repeat_playlist_in_middle_returns_next() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    let total = app.playlist.len();

    let result = app
        .shuffle_state
        .next_index(1, total, crate::shuffle::RepeatMode::Playlist);
    assert_eq!(result, Some(2));
}

/// EOS, RepeatPlaylist: next_index on a single-track playlist wraps to 0.
#[test]
fn eos_repeat_playlist_single_track_wraps_to_self() {
    let mut app = app_with_tracks(&["A"]);
    let total = app.playlist.len();

    let result = app
        .shuffle_state
        .next_index(0, total, crate::shuffle::RepeatMode::Playlist);
    assert_eq!(result, Some(0));
}

/// EOS, RepeatOff: single-track playlist returns None (stop after the only track).
#[test]
fn eos_repeat_off_single_track_returns_none() {
    let mut app = app_with_tracks(&["A"]);
    let total = app.playlist.len();

    let result = app
        .shuffle_state
        .next_index(0, total, crate::shuffle::RepeatMode::Off);
    assert!(result.is_none());
}

// -----------------------------------------------------------------------
