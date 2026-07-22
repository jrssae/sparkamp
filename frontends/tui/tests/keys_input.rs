//! Key handling + the add/move/remove input modes.

use super::*;
use crossterm::event::{KeyCode, KeyModifiers};

// -----------------------------------------------------------------------
// Existing tests
// -----------------------------------------------------------------------

#[test]
fn esc_in_normal_mode_quits() {
    let mut app = make_app();
    assert!(!app.should_quit);
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.should_quit);
}

#[test]
fn esc_in_jump_mode_returns_to_normal_without_quitting() {
    let mut app = make_app();
    app.mode = Mode::Jump {
        query: String::new(),
        results: vec![],
        selected: 0,
        from_media_library: false,
    };
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.should_quit);
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn q_in_normal_mode_opens_queue_manager() {
    let mut app = make_app();
    app.handle_key(KeyCode::Char('q'), KeyModifiers::NONE);
    assert!(!app.should_quit);
    assert!(matches!(app.mode, Mode::Queue { .. }));
}

#[test]
fn ctrl_q_enqueues_highlighted_track() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.playlist_cursor = 1;
    app.handle_key(KeyCode::Char('q'), KeyModifiers::CONTROL);
    // B's entry id is now queued.
    let id_b = app.playlist.tracks[1].id;
    assert!(app.queue.contains(id_b));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn b_key_at_last_track_has_no_effect() {
    let mut app = app_with_tracks(&["A"]);
    app.playlist.current_index = 0;
    app.handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
    assert_eq!(
        app.playlist.current_index, 0,
        "pressing b on the last track must not advance current_index"
    );
}

// -----------------------------------------------------------------------
// Playlist visibility toggle (p key)
// -----------------------------------------------------------------------

#[test]
fn app_starts_with_playlist_visible() {
    let app = make_app();
    assert!(
        app.playlist_visible,
        "playlist should be visible by default"
    );
}

#[test]
fn p_key_toggles_playlist_visible_off() {
    let mut app = make_app();
    assert!(app.playlist_visible);
    app.handle_key(KeyCode::Char('p'), KeyModifiers::NONE);
    assert!(!app.playlist_visible);
}

#[test]
fn p_key_toggles_playlist_visible_back_on() {
    let mut app = make_app();
    app.handle_key(KeyCode::Char('p'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('p'), KeyModifiers::NONE);
    assert!(app.playlist_visible);
}

#[test]
fn capital_p_key_also_toggles_playlist_visible() {
    let mut app = make_app();
    app.handle_key(KeyCode::Char('P'), KeyModifiers::NONE);
    assert!(!app.playlist_visible);
}

// -----------------------------------------------------------------------
// Arrow key seeking
// -----------------------------------------------------------------------

#[test]
fn left_arrow_seek_without_active_track_does_not_panic() {
    // No track → position/duration both None → seek_delta_secs is a no-op.
    let mut app = make_app();
    app.handle_key(KeyCode::Left, KeyModifiers::NONE);
}

#[test]
fn right_arrow_seek_without_active_track_does_not_panic() {
    let mut app = make_app();
    app.handle_key(KeyCode::Right, KeyModifiers::NONE);
}

#[test]
fn seek_delta_secs_is_noop_when_no_duration() {
    // Directly exercises the method: no loaded track → no-op, no panic.
    let mut app = make_app();
    app.seek_delta_secs(5.0);
    app.seek_delta_secs(-5.0);
}

// -----------------------------------------------------------------------
// Add file (n key)
// -----------------------------------------------------------------------

#[test]
fn n_key_enters_add_file_mode() {
    let mut app = make_app();
    app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::AddFile { .. }));
}

#[test]
fn add_file_esc_returns_to_normal() {
    let mut app = make_app();
    app.mode = Mode::AddFile {
        input: "some/path".into(),
        scan_cancel: None,
        scan_added: 0,
    };
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn add_file_chars_accumulate_in_input() {
    let mut app = make_app();
    app.mode = Mode::AddFile {
        input: String::new(),
        scan_cancel: None,
        scan_added: 0,
    };
    for c in "/tmp/track.mp3".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    let Mode::AddFile { ref input, .. } = app.mode else {
        panic!("wrong mode")
    };
    assert_eq!(input, "/tmp/track.mp3");
}

#[test]
fn add_file_backspace_removes_last_char() {
    let mut app = make_app();
    app.mode = Mode::AddFile {
        input: "abc".into(),
        scan_cancel: None,
        scan_added: 0,
    };
    app.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
    let Mode::AddFile { ref input, .. } = app.mode else {
        panic!("wrong mode")
    };
    assert_eq!(input, "ab");
}

#[test]
fn add_file_enter_with_invalid_path_sets_error_and_returns_to_normal() {
    let mut app = make_app();
    app.mode = Mode::AddFile {
        input: "/nonexistent/file.mp3".into(),
        scan_cancel: None,
        scan_added: 0,
    };
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    // handle_key returns immediately; tick() drains the background scan results.
    app.tick();
    assert!(matches!(app.mode, Mode::Normal));
    assert!(
        app.status_message
            .as_deref()
            .unwrap_or("")
            .contains("No audio files"),
        "expected 'No audio files' message, got: {:?}",
        app.status_message
    );
}

#[test]
fn add_file_spaces_in_path_are_preserved() {
    let mut app = make_app();
    app.mode = Mode::AddFile {
        input: String::new(),
        scan_cancel: None,
        scan_added: 0,
    };
    for c in "/tmp/my music/track.mp3".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    let Mode::AddFile { ref input, .. } = app.mode else {
        panic!("wrong mode")
    };
    assert_eq!(input, "/tmp/my music/track.mp3");
}

// -----------------------------------------------------------------------
// commit_add_file with a directory path
// -----------------------------------------------------------------------

#[test]
fn commit_add_file_with_nonexistent_dir_shows_added_zero_message() {
    // A path that is_dir() returns false for (doesn't exist) falls through
    // to the file branch; Track::from_path fails → "No valid audio files found".
    let mut app = make_app();
    app.commit_add_file("/nonexistent_dir_xyz/");
    let msg = app.status_message.as_deref().unwrap_or("");
    assert!(
        msg.contains("No valid") || msg.contains("Added"),
        "unexpected message: {}",
        msg
    );
}

/// A tilde-prefixed path is expanded before scanning, not treated literally.
#[test]
fn commit_add_file_tilde_is_expanded() {
    // Use a controlled temp directory so the test doesn't scan the real home
    // directory (which may contain a large music library via a symlink).
    let dir = tempfile::tempdir().unwrap();
    let home_rel = dir.path().to_str().unwrap();

    // Build a "~/subdir" style input by replacing the home portion with ~.
    // Instead, directly verify that a bare "~" does not produce a
    // Track::from_path error on a path literally named "~".
    // We do this by pointing ~ at our empty temp dir via the
    // HOME env var so the scan is instantaneous.
    let original_home = std::env::var("HOME").unwrap_or_default();
    unsafe {
        std::env::set_var("HOME", home_rel);
    }
    let mut app = make_app();
    app.commit_add_file("~/");
    unsafe {
        std::env::set_var("HOME", &original_home);
    }

    // Empty dir → "No valid audio files found", not a panic or missing-message.
    assert_eq!(
        app.status_message.as_deref(),
        Some("No valid audio files found")
    );
}

// -----------------------------------------------------------------------
// Move track (m key)
// -----------------------------------------------------------------------

#[test]
fn comma_key_enters_move_track_mode() {
    // Move track is now bound to ',' (was 'm').
    let mut app = make_app();
    app.handle_key(KeyCode::Char(','), KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::MoveTrack { from: None, .. }));
}

#[test]
fn move_track_esc_returns_to_normal() {
    let mut app = make_app();
    app.mode = Mode::MoveTrack {
        input: String::new(),
        from: None,
    };
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn move_track_first_enter_stores_from_and_clears_input() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.mode = Mode::MoveTrack {
        input: "2".into(),
        from: None,
    };
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let Mode::MoveTrack { from, ref input } = app.mode else {
        panic!("wrong mode")
    };
    assert_eq!(from, Some(2));
    assert!(input.is_empty());
}

#[test]
fn move_track_invalid_from_shows_error_and_returns_to_normal() {
    let mut app = app_with_tracks(&["A", "B"]);
    app.mode = Mode::MoveTrack {
        input: "abc".into(),
        from: None,
    };
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.status_message.is_some());
}

#[test]
fn move_track_second_enter_reorders_playlist() {
    let mut app = app_with_tracks(&["A", "B", "C", "D"]);
    // move track 2 (B) to position 4 (D)
    app.mode = Mode::MoveTrack {
        input: "4".into(),
        from: Some(2),
    };
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Normal));
    let titles: Vec<_> = app
        .playlist
        .tracks
        .iter()
        .map(|t| t.title.as_str())
        .collect();
    assert_eq!(titles, ["A", "C", "D", "B"]);
}

#[test]
fn move_track_out_of_range_to_shows_error() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.mode = Mode::MoveTrack {
        input: "99".into(),
        from: Some(1),
    };
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.status_message.is_some());
}

#[test]
fn move_track_backspace_removes_last_char() {
    let mut app = make_app();
    app.mode = Mode::MoveTrack {
        input: "12".into(),
        from: None,
    };
    app.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
    let Mode::MoveTrack { ref input, .. } = app.mode else {
        panic!("wrong mode")
    };
    assert_eq!(input, "1");
}

// -----------------------------------------------------------------------
// Remove track (, key)
// -----------------------------------------------------------------------

#[test]
fn dot_key_enters_remove_track_mode() {
    // Remove track is now bound to '.' (was ',').
    let mut app = make_app();
    app.handle_key(KeyCode::Char('.'), KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::RemoveTrack { .. }));
}

#[test]
fn remove_track_esc_returns_to_normal() {
    let mut app = make_app();
    app.mode = Mode::RemoveTrack { input: "1".into() };
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn remove_track_enter_removes_correct_entry() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.mode = Mode::RemoveTrack { input: "2".into() };
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Normal));
    let titles: Vec<_> = app
        .playlist
        .tracks
        .iter()
        .map(|t| t.title.as_str())
        .collect();
    assert_eq!(titles, ["A", "C"]);
}

#[test]
fn remove_track_invalid_index_shows_error() {
    let mut app = app_with_tracks(&["A", "B"]);
    app.mode = Mode::RemoveTrack { input: "99".into() };
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.status_message.is_some());
    assert_eq!(app.playlist.len(), 2); // unchanged
}

#[test]
fn remove_track_non_numeric_input_shows_error() {
    let mut app = app_with_tracks(&["A"]);
    app.mode = Mode::RemoveTrack {
        input: "abc".into(),
    };
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.status_message.is_some());
}

#[test]
fn remove_track_backspace_removes_last_char() {
    let mut app = make_app();
    app.mode = Mode::RemoveTrack { input: "12".into() };
    app.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
    let Mode::RemoveTrack { ref input } = app.mode else {
        panic!("wrong mode")
    };
    assert_eq!(input, "1");
}

#[test]
fn remove_track_reduces_playlist_length() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    assert_eq!(app.playlist.len(), 3);
    app.mode = Mode::RemoveTrack { input: "1".into() };
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.playlist.len(), 2);
}

