//! The playback time counter's elapsed / remaining mode.

use super::*;
use crossterm::event::{KeyCode, KeyModifiers};

/// `h` flips the counter between time played and time left, and the choice is
/// written to config so the next launch starts where the last one ended.
///
/// GTK and macOS toggle this by clicking the counter. A terminal has no
/// counter to click, so it gets a key.
#[test]
fn h_toggles_the_time_counter_and_remembers_it() {
    let mut app = make_app();
    assert!(!app.config.display.show_remaining(), "elapsed by default");

    app.handle_key(KeyCode::Char('h'), KeyModifiers::NONE);
    assert!(app.config.display.show_remaining(), "h switches to remaining");

    app.handle_key(KeyCode::Char('H'), KeyModifiers::NONE);
    assert!(!app.config.display.show_remaining(), "and back again");
}

/// The counter itself reads the config rather than a separate flag, so the two
/// cannot disagree about which mode is showing.
#[test]
fn the_counter_label_follows_the_configured_mode() {
    use std::time::Duration;
    let pos = Duration::from_secs(30);
    let dur = Duration::from_secs(200);
    assert_eq!(crate::tui::ui::progress_label(pos, dur, false), "0:30  /  3:20");
    assert_eq!(crate::tui::ui::progress_label(pos, dur, true), "-2:50  /  3:20");
}
