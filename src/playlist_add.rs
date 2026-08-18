//! The one rule for whether adding tracks to the active playlist replaces
//! what is there or appends to it.
//!
//! Lives in core because every frontend needs the same answer and each had
//! been deciding for itself: five GTK sites, one Swift copy in
//! `SparkampModel+Transport.swift`, and the drag-and-drop drop handler which
//! did not consult the setting at all.

use crate::config::PlaylistAddBehavior;

/// Why tracks are being added, which decides whether the configured
/// preference applies at all.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AddMode {
    /// Honour the user's `playlist_add_behavior` setting. Drag-and-drop,
    /// double-click, and the plain "Add" buttons all use this.
    Behavior,
    /// Always append, whatever the setting says — an explicit "Enqueue"
    /// action has already told us what the user wants.
    Enqueue,
    /// Always replace — an explicit "Play now" action.
    Replace,
}

/// Whether this add should clear the playlist before adding.
///
/// A Replace discards any drop position: the playlist is cleared and the new
/// tracks become the whole of it. That is the decision recorded on
/// 2026-08-18 — "replace clears first then adds".
pub fn should_replace(behavior: &PlaylistAddBehavior, mode: AddMode) -> bool {
    match mode {
        AddMode::Replace => true,
        AddMode::Enqueue => false,
        AddMode::Behavior => *behavior == PlaylistAddBehavior::Replace,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_enqueue_appends_even_when_the_setting_says_replace() {
        assert!(!should_replace(&PlaylistAddBehavior::Replace, AddMode::Enqueue));
    }

    #[test]
    fn an_explicit_play_now_replaces_even_when_the_setting_says_append() {
        assert!(should_replace(&PlaylistAddBehavior::Append, AddMode::Replace));
    }

    #[test]
    fn the_default_mode_follows_the_setting() {
        assert!(should_replace(&PlaylistAddBehavior::Replace, AddMode::Behavior));
        assert!(!should_replace(&PlaylistAddBehavior::Append, AddMode::Behavior));
    }
}
