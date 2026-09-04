use super::*;

// Phase 12 F15 (2026-08-01 revision) — the lyrics WINDOW and the single entry
// point every GTK track-row surface calls. The window ALWAYS opens (no saved
// lyrics shows "No lyrics available"); Search is an in-window button, not an
// alternate code path. The title, search URL, and body all come from core
// (`sparkamp::lyrics::lyrics_view`) so no surface re-implements the decision.

/// Whether the lyrics window tracks a fixed song (opened from a playlist/ML
/// row) or the currently-playing song (opened from the player's A1 affordance).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LyricsMode {
    /// Static: the window keeps showing the song it was opened for.
    Specific,
    /// Live: title + body follow the currently-playing track.
    Current,
}

/// Open (or replace) the singleton lyrics window for one track.
///
/// `mode` seeds the Specific/Current radio: playlist/ML surfaces pass
/// `Specific`; the player's now-playing affordance passes `Current`.
/// `rebuild_cb` is threaded to the "Edit in tag editor" link so a save from the
/// editor refreshes the surface the user came from.
pub(super) fn view_or_search_lyrics(
    state: &Rc<RefCell<AppState>>,
    path: &std::path::Path,
    artist: &str,
    title: &str,
    album_artist: &str,
    rebuild_cb: Rc<dyn Fn()>,
    mode: LyricsMode,
) {
    // Toggle: pressing `l` on the track the window is already showing closes
    // it; pressing `l` on a different track falls through and retargets the
    // window (show_lyrics_window replaces the singleton). "Same track" is by
    // path, which is what the shown-path cell records on open and on every
    // Current-mode refresh.
    let showing_same = {
        let s = state.borrow();
        s.lyrics_window.is_some() && s.lyrics_shown_path.borrow().as_deref() == Some(path)
    };
    if showing_same {
        if let Some(win) = state.borrow_mut().lyrics_window.take() {
            win.close();
        }
        return;
    }
    show_lyrics_window(state, path, artist, title, album_artist, rebuild_cb, mode);
}

/// Open or replace the singleton lyrics window. Mirrors
/// `open_id3_editor_window`'s take-then-close singleton discipline so the
/// borrow is released before `close()` fires its synchronous handler.
pub(super) fn show_lyrics_window(
    state: &Rc<RefCell<AppState>>,
    path: &std::path::Path,
    artist: &str,
    title: &str,
    album_artist: &str,
    rebuild_cb: Rc<dyn Fn()>,
    mode: LyricsMode,
) {
    use gtk4::prelude::*;

    let existing = state.borrow_mut().lyrics_window.take();
    if let Some(win) = existing {
        win.close();
    }

    let view = sparkamp::lyrics::lyrics_view(path, artist, title, album_artist);

    // Shared, refresh-updated state so Current mode can retarget the window on
    // every track change without rebuilding it.
    let active_path = Rc::new(RefCell::new(path.to_path_buf()));
    let search_url = Rc::new(RefCell::new(view.search_url.clone()));

    // Mirror the shown track into AppState so the `l`-key toggle can tell
    // "same track → close" from "different track → retarget". Updated again in
    // the Current-mode refresh closure and cleared on close.
    let shown_path = state.borrow().lyrics_shown_path.clone();
    *shown_path.borrow_mut() = Some(path.to_path_buf());

    let win = gtk4::Window::builder()
        .title(format!("Lyrics — {}", gtk_safe(&view.title)))
        .default_width(420)
        .default_height(520)
        .build();

    let vbox = GtkBox::new(gtk4::Orientation::Vertical, 6);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);
    vbox.set_margin_start(8);
    vbox.set_margin_end(8);

    let text_view = gtk4::TextView::new();
    text_view.set_editable(false);
    text_view.set_cursor_visible(false);
    text_view.set_wrap_mode(gtk4::WrapMode::WordChar);
    text_view.add_css_class("lyrics-view");
    text_view
        .buffer()
        .set_text(&gtk_safe(view.body.as_deref().unwrap_or("No lyrics available")));

    let scroller = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .vexpand(true)
        .child(&text_view)
        .build();
    vbox.append(&scroller);

    // ── Bottom control row: mode radio · Search · Edit ──────────────────────
    let controls = GtkBox::new(gtk4::Orientation::Horizontal, 8);
    controls.set_halign(gtk4::Align::Start);

    // Specific/Current radio (F15 revision, point 4).
    let specific_btn = gtk4::CheckButton::with_label("This song");
    let current_btn = gtk4::CheckButton::with_label("Now playing");
    current_btn.set_group(Some(&specific_btn));
    match mode {
        LyricsMode::Specific => specific_btn.set_active(true),
        LyricsMode::Current => current_btn.set_active(true),
    }
    controls.append(&specific_btn);
    controls.append(&current_btn);

    // Search button (F15 revision, point 6) — DuckDuckGo for the ACTIVE track.
    let search_btn = gtk4::Button::with_label("Search");
    search_btn.add_css_class("pl-btn");
    {
        let search_url = search_url.clone();
        search_btn.connect_clicked(move |_| {
            let url = search_url.borrow().clone();
            // Use the portal-backed UriLauncher (same path the working Wikipedia
            // LinkButtons use). `gio::AppInfo::launch_default_for_uri` silently
            // no-ops for http(s) in a sandboxed / portal-only runtime.
            gtk4::UriLauncher::new(&url).launch(
                None::<&gtk4::Window>,
                gio::Cancellable::NONE,
                |_| {},
            );
        });
    }
    controls.append(&search_btn);

    // "Edit in tag editor" — jumps to the ID3 editor for the ACTIVE track.
    let edit_btn = gtk4::Button::with_label("Edit in tag editor");
    edit_btn.add_css_class("pl-btn");
    {
        let state_edit = state.clone();
        let active_path = active_path.clone();
        let win_weak = win.downgrade();
        edit_btn.connect_clicked(move |_| {
            let parent = win_weak.upgrade();
            let path_now = active_path.borrow().clone();
            open_id3_editor_window(
                parent.as_ref(),
                path_now,
                state_edit.clone(),
                rebuild_cb.clone(),
                None,
                // Force the Lyric field visible in the editor even if the user's
                // column config hides it (F15 revision, point 2).
                Some("lyric".to_string()),
            );
            if let Some(w) = win_weak.upgrade() {
                w.close();
            }
        });
    }
    controls.append(&edit_btn);
    vbox.append(&controls);

    win.set_child(Some(&vbox));

    // Publish the mode + a refresh closure so the now-playing subscriber
    // (registered once in player.rs) can retarget this window in Current mode.
    state.borrow().lyrics_mode.set(mode);
    let refresh: Rc<dyn Fn()> = {
        let state = state.clone();
        let win_weak = win.downgrade();
        let text_view = text_view.clone();
        let active_path = active_path.clone();
        let search_url = search_url.clone();
        let shown_path = shown_path.clone();
        Rc::new(move || {
            // Read the current track under a short borrow, then drop it before
            // touching any widget (subscribers must never re-enter AppState).
            let cur = state.borrow().playlist.current().map(|t| {
                (
                    t.path.clone(),
                    t.artist.clone(),
                    t.title.clone(),
                    t.album_artist.clone(),
                )
            });
            let Some((p, a, t, aa)) = cur else { return };
            let Some(w) = win_weak.upgrade() else { return };
            let v = sparkamp::lyrics::lyrics_view(&p, &a, &t, &aa);
            w.set_title(Some(&format!("Lyrics — {}", gtk_safe(&v.title))));
            text_view
                .buffer()
                .set_text(&gtk_safe(v.body.as_deref().unwrap_or("No lyrics available")));
            *shown_path.borrow_mut() = Some(p.clone());
            *active_path.borrow_mut() = p;
            *search_url.borrow_mut() = v.search_url;
        })
    };
    state.borrow_mut().lyrics_refresh = Some(refresh.clone());

    // Radio wiring: Current re-targets immediately; Specific freezes on the
    // song currently shown.
    {
        let state_mode = state.clone();
        let refresh = refresh.clone();
        current_btn.connect_toggled(move |b| {
            if b.is_active() {
                state_mode.borrow().lyrics_mode.set(LyricsMode::Current);
                refresh();
            }
        });
    }
    {
        let state_mode = state.clone();
        specific_btn.connect_toggled(move |b| {
            if b.is_active() {
                state_mode.borrow().lyrics_mode.set(LyricsMode::Specific);
            }
        });
    }

    // Keys: Esc closes; the Winamp transport keys (z/x/c/v/b/j/r/s) forward to
    // the main window's handler so playback control still works while the
    // lyrics window is focused (F15 revision, point 5).
    let key = gtk4::EventControllerKey::new();
    let win_esc = win.downgrade();
    let state_keys = state.clone();
    key.connect_key_pressed(move |_, keyval, _, _| {
        if keyval == gdk::Key::Escape {
            if let Some(w) = win_esc.upgrade() {
                w.close();
            }
            return glib::Propagation::Stop;
        }
        let is_transport = matches!(
            keyval,
            gdk::Key::z
                | gdk::Key::Z
                | gdk::Key::x
                | gdk::Key::X
                | gdk::Key::c
                | gdk::Key::C
                | gdk::Key::v
                | gdk::Key::V
                | gdk::Key::b
                | gdk::Key::B
                | gdk::Key::j
                | gdk::Key::J
                | gdk::Key::r
                | gdk::Key::R
                | gdk::Key::s
                | gdk::Key::S
        );
        if is_transport {
            // Clone the handler out and drop the borrow before invoking it —
            // the handler itself borrows AppState.
            let handler = state_keys.borrow().transport_key_handler();
            if let Some(h) = handler {
                return h(keyval);
            }
        }
        glib::Propagation::Proceed
    });
    win.add_controller(key);

    let state_close = state.clone();
    win.connect_close_request(move |w| {
        let mut s = state_close.borrow_mut();
        if s.lyrics_window.as_ref() == Some(w) {
            s.lyrics_window = None;
            // Break the refresh cycle (refresh holds an Rc<AppState> clone).
            s.lyrics_refresh = None;
            // Forget the shown track so the next `l` opens rather than toggles.
            *s.lyrics_shown_path.borrow_mut() = None;
        }
        glib::Propagation::Proceed
    });

    state.borrow_mut().lyrics_window = Some(win.clone());
    win.present();
}
