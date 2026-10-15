//! The keyboard shortcuts window (`i`, F1, Ctrl+?, the ℹ button) and the
//! reference table it shows.
//!
//! Moved out of `player.rs`: the window is built once and only toggled
//! after that, and it needs just the main window, the key handler and the
//! ℹ button from `player::build`.

use super::*;

/// Keyboard-shortcut reference shown in the help window (`i`). This is the
/// single source of truth for GTK bindings (phase 6): the help builder renders
/// it and a test asserts every bound key appears here, so the dialog can never
/// silently drift from what the handlers actually do. Keep entries in sync with
/// the mac `KeyboardShortcutsView.swift` sections.
///
/// Alternates for one action are separated by " / " — `"i / F1 / Ctrl+? /
/// Ctrl+/"`. [`format_shortcut_keys`] breaks those onto separate lines. Keys
/// that are not alternates but a pair of opposites ("← →" seeks backward and
/// forward) are space-separated and stay on one line.
#[allow(clippy::type_complexity)]
pub(super) fn shortcut_sections()
-> &'static [(&'static str, &'static [(&'static str, &'static str)])] {
    &[
        (
            "Playback",
            &[
                ("z", "Previous track / restart"),
                ("x", "Play"),
                ("c", "Pause / resume"),
                ("v", "Stop"),
                ("Shift+V", "Stop with fadeout (length in Settings)"),
                ("b", "Next track"),
                ("t", "Stop after current track"),
                ("← →", "Seek −5 s / +5 s"),
                ("r", "Cycle repeat (off / song / playlist)"),
                ("s", "Toggle shuffle on/off"),
            ],
        ),
        (
            "Volume",
            &[
                ("-", "Volume down 5 %"),
                ("=", "Volume up 5 %"),
                ("↑ ↓", "Volume up / down (main window)"),
            ],
        ),
        (
            "Playlist",
            &[
                ("n", "Add file(s)"),
                ("Shift+N", "Add folder"),
                ("m", "Toggle Media Library window"),
                ("j / Ctrl+F", "Jump / search"),
                ("q", "Play queue (Jump/Queue window, Queue mode)"),
                ("Ctrl+Q", "Enqueue / dequeue selection (playlist or jump)"),
                ("↑ ↓", "Browse up / down (playlist window)"),
                ("Enter", "Play selected track"),
                ("Ctrl+S", "Save playlist"),
                ("Ctrl+I", "Invert selection"),
                ("Del", "Remove highlighted track"),
                ("p", "Toggle playlist window"),
            ],
        ),
        (
            "View & Tags",
            &[
                ("a", "Cycle visualizer mode (Bars / Waveform / Granite)"),
                ("e", "Random Granite effect (Granite mode)"),
                (
                    "f",
                    "Fullscreen visualizer (Waveform or Granite mode; Esc to exit)",
                ),
                ("g", "Toggle FPS / BPM overlay (fullscreen only)"),
                ("d", "View/Edit tags for current track"),
                (
                    "l",
                    "View/Search Lyrics (selected track, else the current one)",
                ),
                ("u", "Toggle equalizer window"),
                ("w", "Toggle now-playing panel (art, tags, links)"),
                ("k", "Open album-art window"),
                ("Ctrl+,", "Open settings"),
                ("Click logo", "Open settings"),
            ],
        ),
        (
            "Mouse",
            &[
                ("h / click time", "Switch elapsed / remaining"),
                ("Click viz", "Cycle visualizer mode"),
                (
                    "Dbl-click viz",
                    "Fullscreen visualizer (Waveform or Granite mode)",
                ),
            ],
        ),
        // Alt access keys for the two menu bars this branch added. Both sets
        // start at Alt+A (deconflicted only within their own menu bar — see
        // PLAYLIST_MENU_LABELS / SETTINGS_TAB_LABELS), so each description
        // names its window to disambiguate. A test below derives these
        // letters from those two constants so a relabelled mnemonic fails
        // the build instead of silently drifting from this list.
        (
            "Menus",
            &[
                ("Alt+A", "Playlist window: Add menu"),
                ("Alt+S", "Playlist window: Select menu"),
                ("Alt+O", "Playlist window: Sort menu"),
                ("Alt+L", "Playlist window: List menu"),
                ("Alt+A", "Settings window: Appearance tab"),
                ("Alt+B", "Settings window: Behavior tab"),
                ("Alt+V", "Settings window: Visualizer tab"),
                ("Alt+M", "Settings window: Media Library tab"),
                ("Alt+O", "Settings window: About tab"),
            ],
        ),
        (
            "Other",
            &[
                // Ctrl+/ is listed alongside Ctrl+? because both keyvals are
                // bound (see the CONTROL_MASK match arms in `build`): many
                // layouts report the unshifted keyval for Ctrl+Shift+/.
                ("i / F1 / Ctrl+? / Ctrl+/", "Toggle this help"),
                ("Esc", "Quit (main window) / close child window"),
            ],
        ),
    ]
}

/// Render one row's key column for the help window.
///
/// Four alternates run together on a single line ("i F1 Ctrl+? Ctrl+/") read as
/// one long key, so each alternate gets its own line. The slash stays at the
/// end of the line it follows, where it reads as the "or" it already is.
pub(super) fn format_shortcut_keys(keys: &str) -> String {
    keys.replace(" / ", " /\n")
}

/// Build the shortcuts window, hidden, transient for `parent`. Keys pressed
/// in it go to `handle_key`, so the transport shortcuts keep working while it
/// is open; Esc hides it. `btn_info` is highlighted while it is showing.
pub(super) fn build_window(
    parent: &ApplicationWindow,
    handle_key: Rc<dyn Fn(gdk::Key) -> glib::Propagation>,
    btn_info: &Button,
) -> gtk4::Window {
    let win = gtk4::Window::builder()
        .title("Keyboard Shortcuts")
        .modal(false)
        .default_width(420)
        .default_height(480)
        .build();
    win.set_transient_for(Some(parent.upcast_ref::<gtk4::Window>()));

    let sections = shortcut_sections();

    let grid = gtk4::Grid::builder()
        .column_spacing(16)
        .row_spacing(4)
        .halign(gtk4::Align::Fill)
        .valign(gtk4::Align::Start)
        .build();

    // Title row.
    let title = gtk4::Label::builder()
        .label("Sparkamp — Keyboard Shortcuts")
        .halign(gtk4::Align::Start)
        .css_classes(["info-title"])
        .build();
    grid.attach(&title, 0, 0, 2, 1);

    let mut row: i32 = 1;
    // Spacer below title.
    let spacer = gtk4::Label::new(Some(""));
    grid.attach(&spacer, 0, row, 2, 1);
    row += 1;

    for (section, entries) in sections.iter() {
        let header = gtk4::Label::builder()
            .label(*section)
            .halign(gtk4::Align::Start)
            .css_classes(["info-section"])
            .build();
        grid.attach(&header, 0, row, 2, 1);
        row += 1;

        for (key, desc) in entries.iter() {
            let key_lbl = gtk4::Label::builder()
                .label(format_shortcut_keys(key))
                .halign(gtk4::Align::Start)
                .valign(gtk4::Align::Start)
                .xalign(0.0) // keep stacked alternates flush left
                .css_classes(["info-key"])
                .build();
            let desc_lbl = gtk4::Label::builder()
                .label(*desc)
                .halign(gtk4::Align::Start)
                // Sits level with the first key line when the key wraps.
                .valign(gtk4::Align::Start)
                .css_classes(["info-desc"])
                .build();
            grid.attach(&key_lbl, 0, row, 1, 1);
            grid.attach(&desc_lbl, 1, row, 1, 1);
            row += 1;
        }

        // Section spacer.
        let spc = gtk4::Label::new(Some(""));
        grid.attach(&spc, 0, row, 2, 1);
        row += 1;
    }

    let body = GtkBox::new(Orientation::Vertical, 0);
    body.set_css_classes(&["info-text"]);
    body.append(&grid);

    let scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .child(&body)
        .build();
    let key_ctrl = gtk4::EventControllerKey::new();
    let handler = handle_key.clone();
    let win_wk = win.downgrade();
    key_ctrl.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            if let Some(w) = win_wk.upgrade() {
                w.set_visible(false);
            }
            return glib::Propagation::Stop;
        }
        handler(key)
    });
    win.add_controller(key_ctrl);
    win.set_child(Some(&scroll));
    win.set_hide_on_close(true);
    win.connect_visible_notify({
        let btn = btn_info.clone();
        move |w| {
            if w.is_visible() {
                btn.add_css_class("mode-btn-active");
            } else {
                btn.remove_css_class("mode-btn-active");
            }
        }
    });
    win
}

#[cfg(test)]
mod shortcut_dialog_tests {
    use super::{format_shortcut_keys, shortcut_sections};

    /// Alternates go one per line so four of them can be read at a glance;
    /// the slash stays at the end of the line it follows.
    #[test]
    fn alternates_are_split_one_per_line() {
        assert_eq!(
            format_shortcut_keys("i / F1 / Ctrl+? / Ctrl+/"),
            "i /\nF1 /\nCtrl+? /\nCtrl+/"
        );
    }

    /// "← →" is one row for two opposite actions, not two names for one — it
    /// carries no separator and must stay on a single line. The trailing
    /// "Ctrl+/" is the case a naive `replace("/", ...)` would break.
    #[test]
    fn non_alternates_and_slash_keys_are_left_alone() {
        assert_eq!(format_shortcut_keys("← →"), "← →");
        assert_eq!(format_shortcut_keys("Ctrl+/"), "Ctrl+/");
        assert_eq!(format_shortcut_keys("Ctrl+,"), "Ctrl+,");
    }

    /// The help window is the single source of truth for GTK bindings — every
    /// key the phase-6 handlers bind must appear in it, so the dialog can never
    /// silently drift from reality. Update this list deliberately when adding keys.
    #[test]
    fn shortcut_dialog_lists_every_phase6_key() {
        let keys: Vec<&str> = shortcut_sections()
            .iter()
            .flat_map(|(_, entries)| entries.iter().map(|(k, _)| *k))
            .collect();
        for k in [
            "m", "t", "Shift+N", "Ctrl+S", "Ctrl+,", "Ctrl+I", "n", "Enter", "↑ ↓",
        ] {
            assert!(keys.contains(&k), "shortcuts dialog is missing `{k}`");
        }
    }

    /// The shortcuts window is the only place a user discovers these, so a
    /// binding that exists in code but not in the dialog is invisible.
    ///
    /// Alternates share one row, separated by " / " (e.g. "i / F1 / Ctrl+? /
    /// Ctrl+/"), so this splits each row's key column on whitespace and checks
    /// exact token membership rather than a substring of the joined string — a
    /// substring check here previously let e.g. "Ctrl+F12" satisfy a
    /// `contains("F1")` probe (review Minor #10).
    #[test]
    fn shortcut_dialog_lists_the_standard_aliases() {
        let tokens: Vec<&str> = shortcut_sections()
            .iter()
            .flat_map(|(_, entries)| entries.iter().flat_map(|(k, _)| k.split_whitespace()))
            .collect();
        for expected in ["Ctrl+F", "F1", "Ctrl+?", "Ctrl+/", "Ctrl+,"] {
            assert!(
                tokens.contains(&expected),
                "{expected} missing from the shortcuts dialog: {tokens:?}"
            );
        }
    }

    /// PLAYLIST_MENU_LABELS and SETTINGS_TAB_LABELS are the source of truth
    /// for the Alt-key menu mnemonics this branch added; derive the letter
    /// from each label (the same extraction the two `_labels_are_deconflicted`
    /// tests use) instead of hardcoding it here, so a relabelled or newly
    /// added mnemonic fails this test rather than just being undocumented —
    /// which is exactly how the mnemonics went missing from this dialog the
    /// first time.
    #[test]
    fn shortcut_dialog_lists_every_menu_mnemonic() {
        use super::super::playlist_window::PLAYLIST_MENU_LABELS;
        use super::super::settings::SETTINGS_TAB_LABELS;

        fn mnemonic_of(label: &str) -> char {
            let us = label
                .find('_')
                .expect("label must contain a mnemonic underscore");
            label[us + 1..]
                .chars()
                .next()
                .expect("underscore must not be the label's last character")
                .to_ascii_uppercase()
        }

        let entries: Vec<(&str, &str)> = shortcut_sections()
            .iter()
            .flat_map(|(_, entries)| entries.iter().map(|(k, d)| (*k, *d)))
            .collect();

        for label in PLAYLIST_MENU_LABELS {
            let alt = format!("Alt+{}", mnemonic_of(label));
            assert!(
                entries
                    .iter()
                    .any(|(k, d)| *k == alt.as_str() && d.contains("Playlist window")),
                "playlist menu mnemonic {alt} (from {label:?}) is missing from the shortcuts dialog"
            );
        }
        for label in SETTINGS_TAB_LABELS {
            let alt = format!("Alt+{}", mnemonic_of(label));
            assert!(
                entries
                    .iter()
                    .any(|(k, d)| *k == alt.as_str() && d.contains("Settings window")),
                "settings tab mnemonic {alt} (from {label:?}) is missing from the shortcuts dialog"
            );
        }
    }

    /// Ctrl+. was replaced by Ctrl+, — the GNOME standard, and what the
    /// macOS frontend already uses. Leaving it listed would document a
    /// binding that no longer fires.
    #[test]
    fn shortcut_dialog_no_longer_lists_ctrl_period() {
        let keys: Vec<&str> = shortcut_sections()
            .iter()
            .flat_map(|(_, entries)| entries.iter().map(|(k, _)| *k))
            .collect();
        assert!(
            !keys.iter().any(|k| *k == "Ctrl+."),
            "Ctrl+. is no longer bound and must not be listed"
        );
    }
}
