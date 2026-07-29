// Phase 12 F15 — read-only lyrics viewer + the single View/Search decision
// entry point every GTK track-row surface calls. The Show-vs-Search branch and
// the search-URL live in core (`crate::lyrics`); this file is only the GTK
// window + browser hand-off, so no surface re-implements the decision.

/// Resolve lyrics for one track: saved USLT opens (or replaces) the singleton
/// viewer; no lyrics hands a DuckDuckGo search to the default browser.
///
/// `rebuild_cb` is threaded to the "Edit in tag editor" link so a save from the
/// editor refreshes the surface the user came from, exactly like the row's own
/// "View/Edit ID3" action.
fn view_or_search_lyrics(
    state: &Rc<RefCell<AppState>>,
    path: &std::path::Path,
    artist: &str,
    title: &str,
    album_artist: &str,
    rebuild_cb: Rc<dyn Fn()>,
) {
    match crate::lyrics::lyrics_action(path, artist, title, album_artist) {
        crate::lyrics::LyricsAction::Search(url) => {
            // Same browser hand-off the Wikipedia links use (dedupe.rs).
            let _ = gio::AppInfo::launch_default_for_uri(&url, None::<&gio::AppLaunchContext>);
        }
        crate::lyrics::LyricsAction::Show(text) => {
            show_lyrics_window(state, path, title, &text, rebuild_cb);
        }
    }
}

/// Open or replace the singleton lyrics window with `text`. Mirrors
/// `open_id3_editor_window`'s take-then-close singleton discipline so the
/// borrow is released before `close()` fires its synchronous handler.
fn show_lyrics_window(
    state: &Rc<RefCell<AppState>>,
    path: &std::path::Path,
    title: &str,
    text: &str,
    rebuild_cb: Rc<dyn Fn()>,
) {
    use gtk4::prelude::*;

    let existing = state.borrow_mut().lyrics_window.take();
    if let Some(win) = existing {
        win.close();
    }

    let disp_title = gtk_safe(if title.trim().is_empty() {
        path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
    } else {
        title
    });

    let win = gtk4::Window::builder()
        .title(format!("Lyrics — {disp_title}"))
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
    text_view.buffer().set_text(&gtk_safe(text));

    let scroller = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .vexpand(true)
        .child(&text_view)
        .build();
    vbox.append(&scroller);

    // "Edit in tag editor" — closes the phase-0 single-line-Entry gap by
    // jumping straight to the ID3 editor for this track (user decision).
    let edit_btn = gtk4::Button::with_label("Edit in tag editor");
    edit_btn.add_css_class("pl-btn");
    edit_btn.set_halign(gtk4::Align::Start);
    {
        let state_edit = state.clone();
        let path_buf = path.to_path_buf();
        let win_weak = win.downgrade();
        edit_btn.connect_clicked(move |_| {
            let parent = win_weak.upgrade();
            open_id3_editor_window(
                parent.as_ref(),
                path_buf.clone(),
                state_edit.clone(),
                rebuild_cb.clone(),
                None,
            );
            if let Some(w) = win_weak.upgrade() {
                w.close();
            }
        });
    }
    vbox.append(&edit_btn);

    win.set_child(Some(&vbox));

    // Esc closes the viewer (read-only, nothing to lose).
    let key = gtk4::EventControllerKey::new();
    let win_esc = win.downgrade();
    key.connect_key_pressed(move |_, keyval, _, _| {
        if keyval == gdk::Key::Escape {
            if let Some(w) = win_esc.upgrade() {
                w.close();
            }
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    win.add_controller(key);

    let state_close = state.clone();
    win.connect_close_request(move |w| {
        let mut s = state_close.borrow_mut();
        if s.lyrics_window.as_ref() == Some(w) {
            s.lyrics_window = None;
        }
        glib::Propagation::Proceed
    });

    state.borrow_mut().lyrics_window = Some(win.clone());
    win.present();
}
