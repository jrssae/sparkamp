//! The Settings window, one module per tab.
//!
//! `open_settings_window` was a single 2,775-line function holding all five
//! tabs inline. Each is its own file now, taking what it used to close over.
//! The window, the notebook and the close handling stay here.
use super::*;

/// Wrap a settings tab's content in a vertical scroller so a tab taller than
/// the window scrolls instead of being clipped. The scroller fills the tab
/// area (the window carries a fixed default height and is resizable), so short
/// tabs show empty space below rather than shrinking the window.
pub(super) fn settings_scroll_page(
    child: &impl gtk4::prelude::IsA<gtk4::Widget>,
) -> gtk4::ScrolledWindow {
    gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .child(child)
        .build()
}

/// Settings notebook tab labels with mnemonics, in order: Appearance, Behavior,
/// Visualizer, Media Library, About. Access keys are deconflicted within the
/// notebook: A, B, V, M, O (About uses O, not A, to avoid collision with
/// Appearance; B is used by Behavior, so About cannot use it).
pub(super) const SETTINGS_TAB_LABELS: [&str; 5] = [
    "_Appearance",
    "_Behavior",
    "_Visualizer",
    "_Media Library",
    "Ab_out",
];

pub(super) fn open_settings_window(
    parent: Option<&gtk4::Window>,
    state: Rc<RefCell<AppState>>,
    initial_tab: Option<u32>,
    css_provider: Rc<gtk4::CssProvider>,
    text_rgba: Rc<RefCell<gdk::RGBA>>,
    accent_rgba: Rc<RefCell<Option<gdk::RGBA>>>,
    rebuild_playlist: Rc<dyn Fn()>,
) {
    // Singleton: if a Settings window is already open, focus it instead of
    // opening a second one (matches ml_window / art_window). All callers pass
    // initial_tab = None today, so re-presenting keeps the existing tab.
    if let Some(existing) = state.borrow().settings_window.clone() {
        existing.present();
        return;
    }

    // Default height = twice the width (480 → 960) so the taller tabs (Behavior
    // has grown several sections) open with room to breathe, but never taller
    // than the monitor so it can't open off-screen. Resizable so the user can
    // grow or shrink from there; each tab is wrapped in a scroller below so a
    // tab taller than the window scrolls rather than being clipped.
    const SETTINGS_WIDTH: i32 = 480;
    let default_height = {
        let screen_h = gdk::Display::default()
            .and_then(|d| d.monitors().item(0))
            .and_downcast::<gdk::Monitor>()
            .map(|m| m.geometry().height())
            .unwrap_or(1000);
        (SETTINGS_WIDTH * 2).min(((screen_h as f64) * 0.9) as i32)
    };

    let win = gtk4::Window::new();
    win.set_title(Some("Settings — Sparkamp"));
    win.set_default_size(SETTINGS_WIDTH, default_height);
    win.set_resizable(true);
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }

    let notebook = Notebook::new();
    notebook.set_margin_top(8);
    notebook.set_margin_bottom(8);
    notebook.set_margin_start(8);
    notebook.set_margin_end(8);

    // ── Tab 0: Appearance ─────────────────────────────────────────────────
    appearance::build(&notebook, &state, &css_provider, &text_rgba, &accent_rgba, &rebuild_playlist, &win);
    behavior::build(&notebook, &state, &win);
    visualizer::build(&notebook, &state);
    media_library::build(&notebook, &state, &win);
    about::build(&notebook);

    // About tab is index 0 — the default landing tab when no specific tab was
    // requested by the caller, and the leftmost one, matching macOS. The rest
    // follow it: Appearance(1), Behavior(2), Visualizer(3), Media Library(4).
    // (Filetypes is gone; its one dropdown moved into Behavior.)
    notebook.set_current_page(Some(initial_tab.unwrap_or(0)));

    // ── Close button ───────────────────────────────────────────────────────
    // Changes are applied immediately; this button just closes the window.
    let close_btn = Button::with_label("Close");
    close_btn.set_margin_top(4);
    close_btn.set_margin_bottom(8);
    close_btn.set_margin_start(8);
    close_btn.set_margin_end(8);
    close_btn.set_halign(Align::End);
    {
        let win_wk = win.downgrade();
        close_btn.connect_clicked(move |_| {
            if let Some(w) = win_wk.upgrade() {
                w.close();
            }
        });
    }

    // Save when the window is closed via the window-manager button.
    {
        let state_rc = state.clone();
        win.connect_close_request(move |_| {
            let mut s = state_rc.borrow_mut();
            let _ = s.config.save();
            // Clear the singleton handle so the next open builds a fresh one.
            s.settings_window = None;
            glib::Propagation::Proceed
        });
    }

    let vbox = GtkBox::new(Orientation::Vertical, 0);
    vbox.append(&notebook);
    vbox.append(&close_btn);
    // Every toast in this window lands here. Wrapping the root once means
    // call sites only need the window, not a threaded-through overlay.
    let toaster = adw::ToastOverlay::new();
    toaster.set_child(Some(&vbox));
    win.set_child(Some(&toaster));
    win.present();
    state.borrow_mut().settings_window = Some(win);
}

#[cfg(test)]
mod settings_tab_mnemonic_tests {
    use super::*;

    /// Verify that SETTINGS_TAB_LABELS defines all five mnemonics correctly.
    /// All labels must contain exactly one underscore, and the mnemonic
    /// characters (the ones following the underscores) must be distinct when
    /// lowercased to avoid collisions in the notebook.
    #[test]
    fn settings_tab_labels_are_deconflicted() {
        assert_eq!(
            SETTINGS_TAB_LABELS.len(),
            5,
            "SETTINGS_TAB_LABELS must have exactly 5 entries"
        );

        let mut mnemonic_chars = Vec::new();

        for (idx, label) in SETTINGS_TAB_LABELS.iter().enumerate() {
            let underscores: Vec<usize> = label
                .chars()
                .enumerate()
                .filter(|(_, c)| *c == '_')
                .map(|(i, _)| i)
                .collect();

            assert_eq!(
                underscores.len(),
                1,
                "SETTINGS_TAB_LABELS[{}] = {:?} must have exactly one underscore",
                idx,
                label
            );

            // The character immediately after the underscore is the mnemonic key.
            let underscore_idx = underscores[0];
            let mnemonic_char = label
                .chars()
                .nth(underscore_idx + 1)
                .expect("underscore must not be the last character");
            let mnemonic_lowercase = mnemonic_char.to_lowercase().to_string();

            // Check for duplicates.
            assert!(
                !mnemonic_chars.contains(&mnemonic_lowercase),
                "SETTINGS_TAB_LABELS[{}] = {:?} has mnemonic '{}' which \
                 collides with an earlier tab (GTK lowercases all mnemonics)",
                idx,
                label,
                mnemonic_lowercase
            );
            mnemonic_chars.push(mnemonic_lowercase);
        }
    }
}

// ---------------------------------------------------------------------------
// Equalizer window
// ---------------------------------------------------------------------------


mod appearance;
mod behavior;
mod visualizer;
mod media_library;
mod about;
