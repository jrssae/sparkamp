//! Whether the burn panel should be on screen at all.
//!
//! Two independent questions decide this, and conflating them is easy:
//!
//! - **The drive**: can this hardware write? A DVD-ROM never can, so its burn
//!   panel is dead weight no disc will ever revive.
//! - **The disc**: can the medium currently loaded be written? That answer
//!   changes every time a disc is swapped, and it is
//!   [`crate::disc::burn::erase_decision`]'s to give.
//!
//! Sensitivity of the individual burn buttons stays where it already lives, in
//! the panel itself. This module only decides whether the panel is shown.

/// What to do with the burn panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurnPanel {
    /// Show it, ready to use.
    Visible,
    /// Show it, but with the "couldn't identify this disc" hint up: the media
    /// probe failed, so refusing to show the panel would be acting on a guess.
    VisibleWithHint,
    /// Keep it off screen entirely.
    Hidden,
}

/// The inputs to the decision, named so no call site can transpose them.
#[derive(Debug, Clone, Copy)]
pub struct BurnContext {
    /// The drive's own capability, independent of any disc in it.
    pub supports_writing: bool,
    /// Whether the disc's mount can be read. A drive we cannot reach cannot
    /// be burned to either.
    pub mount_readable: bool,
    /// Whether any medium is loaded.
    pub media_present: bool,
    /// Whether the loaded medium can be written — `erase_decision`'s verdict.
    pub media_writable: bool,
    /// Whether the media probe failed, making `media_writable` a default
    /// rather than a reading.
    pub typing_unknown: bool,
}

/// Decide whether the burn panel belongs on screen.
pub fn burn_panel_state(ctx: BurnContext) -> BurnPanel {
    // Hardware first: no disc, and no uncertainty about a disc, can make a
    // reader into a writer.
    if !ctx.supports_writing || !ctx.mount_readable {
        return BurnPanel::Hidden;
    }
    // No medium: hidden, which is what the page has always done. Staging a
    // queue against an empty tray would be a nicer workflow, but it is not
    // what was asked for here.
    if !ctx.media_present {
        return BurnPanel::Hidden;
    }
    // A probe that could not run leaves `media_writable` at its default, which
    // is not a reading. Show the panel and let the hint say so.
    if ctx.typing_unknown {
        return BurnPanel::VisibleWithHint;
    }
    if ctx.media_writable {
        BurnPanel::Visible
    } else {
        BurnPanel::Hidden
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writable_drive() -> BurnContext {
        BurnContext {
            supports_writing: true,
            mount_readable: true,
            media_present: true,
            media_writable: true,
            typing_unknown: false,
        }
    }

    #[test]
    fn a_burner_with_writable_media_shows_the_panel() {
        assert_eq!(burn_panel_state(writable_drive()), BurnPanel::Visible);
    }

    #[test]
    fn a_drive_that_cannot_write_never_shows_the_panel() {
        let ctx = BurnContext { supports_writing: false, ..writable_drive() };
        assert_eq!(burn_panel_state(ctx), BurnPanel::Hidden);
    }

    #[test]
    fn an_unreadable_mount_hides_the_panel() {
        let ctx = BurnContext { mount_readable: false, ..writable_drive() };
        assert_eq!(burn_panel_state(ctx), BurnPanel::Hidden);
    }

    #[test]
    fn a_pressed_disc_hides_the_panel() {
        let ctx = BurnContext { media_writable: false, ..writable_drive() };
        assert_eq!(burn_panel_state(ctx), BurnPanel::Hidden);
    }

    #[test]
    fn an_empty_tray_hides_the_panel_as_it_always_has() {
        // Pre-existing behaviour, preserved deliberately: `disc_page` has
        // hidden the panel on an empty tray since before this module existed.
        // Showing it here so a queue could be staged before a blank goes in
        // would be a change nobody asked for, and belongs in its own branch.
        let ctx = BurnContext {
            media_present: false,
            media_writable: false,
            ..writable_drive()
        };
        assert_eq!(burn_panel_state(ctx), BurnPanel::Hidden);
    }

    #[test]
    fn a_disc_that_could_not_be_typed_keeps_the_panel_and_explains_itself() {
        // `media_writable` is false here only because the probe could not run
        // — common in the Flatpak, where opening the device fails. Hiding on
        // that guess takes burning away from discs that are in fact writable.
        let ctx = BurnContext {
            media_writable: false,
            typing_unknown: true,
            ..writable_drive()
        };
        assert_eq!(burn_panel_state(ctx), BurnPanel::VisibleWithHint);
    }

    #[test]
    fn a_failed_probe_does_not_rescue_a_drive_that_cannot_write() {
        // Drive capability outranks any uncertainty about the disc.
        let ctx = BurnContext {
            supports_writing: false,
            typing_unknown: true,
            ..writable_drive()
        };
        assert_eq!(burn_panel_state(ctx), BurnPanel::Hidden);
    }

    #[test]
    fn a_failed_probe_does_not_rescue_an_unreadable_mount() {
        let ctx = BurnContext {
            mount_readable: false,
            typing_unknown: true,
            ..writable_drive()
        };
        assert_eq!(burn_panel_state(ctx), BurnPanel::Hidden);
    }
}
