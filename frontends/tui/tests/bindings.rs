//! Rebound keys and comma-separated add-file commits.

use super::*;
use crossterm::event::{KeyCode, KeyModifiers};

// New key binding tests (rebindings: ',' = move, '.' = remove, '/' = clear)
// -----------------------------------------------------------------------

/// ',' now enters MoveTrack mode (was 'm').
#[test]
fn comma_key_enters_move_track_mode_new_binding() {
    let mut app = make_app();
    app.handle_key(KeyCode::Char(','), KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::MoveTrack { from: None, .. }));
}

/// '.' now enters RemoveTrack mode (was ',').
#[test]
fn dot_key_enters_remove_track_mode_new_binding() {
    let mut app = make_app();
    app.handle_key(KeyCode::Char('.'), KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::RemoveTrack { .. }));
}

/// '/' clears all tracks and stops playback.
#[test]
fn slash_key_clears_all_tracks() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    assert_eq!(app.playlist.len(), 3);
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(app.playlist.is_empty(), "playlist should be empty after /");
    assert_eq!(app.playlist.current_index, 0);
    assert_eq!(app.playlist_cursor, 0);
    assert!(
        app.status_message
            .as_deref()
            .unwrap_or("")
            .contains("cleared"),
        "expected cleared message, got: {:?}",
        app.status_message
    );
}

/// 'i' enters Help mode with scroll at zero.
#[test]
fn i_key_enters_help_mode() {
    let mut app = make_app();
    app.handle_key(KeyCode::Char('i'), KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Help { scroll: 0 }));
}

/// Esc closes the help overlay.
#[test]
fn esc_in_help_mode_returns_to_normal() {
    let mut app = make_app();
    app.mode = Mode::Help { scroll: 0 };
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Normal));
}

/// ↑/↓ scroll the help overlay without closing it.
#[test]
fn arrow_keys_scroll_help_overlay() {
    let mut app = make_app();
    app.mode = Mode::Help { scroll: 5 };
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Help { scroll: 6 }));
    app.handle_key(KeyCode::Up, KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Help { scroll: 5 }));
}

/// z/x/c/v/b work in help mode (pass-through) and keep the overlay open.
#[test]
fn playback_keys_work_in_help_mode() {
    let mut app = make_app();
    app.mode = Mode::Help { scroll: 0 };
    // 'b' (next) should not close the overlay.
    app.handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
    assert!(
        matches!(app.mode, Mode::Help { .. }),
        "overlay should stay open after 'b'"
    );
    // 'v' (stop) should not close the overlay.
    app.handle_key(KeyCode::Char('v'), KeyModifiers::NONE);
    assert!(
        matches!(app.mode, Mode::Help { .. }),
        "overlay should stay open after 'v'"
    );
}

// -----------------------------------------------------------------------
// Comma-separated commit_add_file tests
// -----------------------------------------------------------------------

/// A comma-separated input with two invalid paths shows "No valid".
#[test]
fn commit_add_file_comma_separated_all_invalid_shows_no_valid() {
    let mut app = make_app();
    app.commit_add_file("/no/such/a.mp3, /no/such/b.mp3");
    let msg = app.status_message.as_deref().unwrap_or("");
    assert!(msg.contains("No valid"), "unexpected: {msg}");
}

/// An input that is only whitespace/commas produces "No valid".
#[test]
fn commit_add_file_empty_after_split_shows_no_valid() {
    let mut app = make_app();
    app.commit_add_file("  ,  ,  ");
    let msg = app.status_message.as_deref().unwrap_or("");
    assert!(msg.contains("No valid"), "unexpected: {msg}");
}

// -----------------------------------------------------------------------
