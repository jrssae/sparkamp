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

// -----------------------------------------------------------------------
// Media Library: Albums-tab drill-down vs. add-path input precedence
// -----------------------------------------------------------------------

/// Esc must close the add-path input FIRST when both the Albums-tab drill
/// and the add-path prompt are open at once (reachable via 'a' while
/// drilled into an album). Only once the input is closed should a
/// subsequent Esc pop the drill back to the album list — same precedence
/// every other tab already has between its modal inputs and the plain
/// "Esc closes this view" behavior.
#[test]
fn albums_esc_closes_add_path_before_popping_drill() {
    let mut app = make_app();
    app.open_media_library();
    if let Mode::MediaLibrary(s) = &mut app.mode {
        s.tab = MediaLibraryTab::Albums;
        s.album_drill = Some(("Dark Side of the Moon".to_string(), "Pink Floyd".to_string()));
        s.add_input = Some(String::new());
    } else {
        panic!("expected MediaLibrary mode after open_media_library()");
    }

    // First Esc: closes the add-path input; the drill must survive.
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    if let Mode::MediaLibrary(s) = &app.mode {
        assert!(
            s.add_input.is_none(),
            "first Esc should close the add-path input"
        );
        assert!(
            s.album_drill.is_some(),
            "drill must still be active after the add-path input closes"
        );
    } else {
        panic!("expected MediaLibrary mode to remain open after first Esc");
    }

    // Second Esc: no modal input left open, so this one pops the drill.
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    if let Mode::MediaLibrary(s) = &app.mode {
        assert!(
            s.album_drill.is_none(),
            "second Esc should pop the drill back to the album list"
        );
    } else {
        panic!("expected MediaLibrary mode to remain open after second Esc");
    }
}

// -----------------------------------------------------------------------
// '/' opens search instead of clearing the playlist (item 14)
// -----------------------------------------------------------------------

/// `/` used to clear the entire playlist while meaning "search" in the
/// Media Library. It is the key every terminal user presses to search,
/// so the destructive binding was a data-loss trap.
#[test]
fn slash_opens_search_and_does_not_clear_the_playlist() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    assert_eq!(app.playlist.len(), 3, "/ must not clear the playlist");
    assert!(matches!(app.mode, Mode::Jump { .. }), "/ must open search");
}

/// Ctrl+F is the same action, matching what the Media Library already
/// accepts.
#[test]
fn ctrl_f_opens_search() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.handle_key(KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(matches!(app.mode, Mode::Jump { .. }));
}

/// Clearing the playlist is still reachable, but from the ops popup where
/// the other whole-playlist operations live — the same place GTK puts it,
/// as List ▾ ▸ Remove All.
#[test]
fn remove_all_is_reachable_from_the_ops_popup() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.handle_key(KeyCode::Char('o'), KeyModifiers::NONE);
    let idx = App::PLAYLIST_OPS_LABELS
        .iter()
        .position(|l| *l == "Remove All")
        .expect("ops popup must offer Remove All");
    for _ in 0..idx {
        app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    }
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.playlist.len(), 0);
}

/// F1 joins `i` for help. No F-keys were bound before, so there is no
/// conflict; `i` still works if a multiplexer intercepts F1.
#[test]
fn f1_opens_help() {
    let mut app = app_with_tracks(&["A"]);
    app.handle_key(KeyCode::F(1), KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Help { .. }));
}

