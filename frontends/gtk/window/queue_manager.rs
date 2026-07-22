// Play Queue Manager — a singleton window listing the manual play queue in
// order with reorder / remove / clear / randomize controls and double-click
// "play now". Mirrors the `art_window::open_or_focus` singleton idiom, but
// keeps its one window in a `thread_local` (the queue has no `AppState`
// subscription to register, so it needs no field there).
// (include!d into window/mod.rs, so no module-level `//!` docs here.)

use std::cell::RefCell as StdRefCell;

thread_local! {
    /// The single Queue Manager window, kept alive (hidden, not destroyed) for
    /// the app's lifetime so repeated open/close cycles reuse it.
    static QUEUE_MANAGER_WIN: StdRefCell<Option<gtk4::Window>> = const { StdRefCell::new(None) };
}

/// Open the Queue Manager (or present it if already open).
///
/// - `refresh_main`: the playlist rebuild closure — called after any queue
///   mutation so the main playlist's `[n]` badges stay in sync.
/// - `play_and_update`: the shared "start playing current track" closure, used
///   by double-click "play now".
/// - `handle_key`: shared shortcut handler so transport keys keep working while
///   this window has focus (Esc hides locally).
fn open_or_focus_queue_manager(
    state: Rc<RefCell<AppState>>,
    refresh_main: Rc<dyn Fn()>,
    play_and_update: Rc<dyn Fn()>,
    handle_key: Rc<dyn Fn(gdk::Key) -> glib::Propagation>,
    parent: Option<&gtk4::Window>,
) {
    // Singleton fast path.
    if QUEUE_MANAGER_WIN.with(|w| {
        if let Some(win) = w.borrow().as_ref() {
            win.present();
            true
        } else {
            false
        }
    }) {
        return;
    }

    let win = gtk4::Window::builder()
        .title("Play Queue — Sparkamp")
        .default_width(360)
        .default_height(420)
        .resizable(true)
        .build();
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }
    win.set_hide_on_close(true);

    let root = GtkBox::new(Orientation::Vertical, 6);
    root.set_margin_top(8);
    root.set_margin_bottom(8);
    root.set_margin_start(8);
    root.set_margin_end(8);

    let list = ListBox::new();
    list.set_selection_mode(gtk4::SelectionMode::Single);
    // Skin selection/hover colours (house rule for hand-built list boxes).
    list.add_css_class("ml-col-view");
    let scroll = ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .vexpand(true)
        .child(&list)
        .build();
    root.append(&scroll);

    let status = Label::new(None);
    status.set_halign(Align::Start);
    status.add_css_class("dim-label");
    root.append(&status);

    // Rebuild the list from the current queue order.
    let rebuild: Rc<dyn Fn()> = {
        let state = state.clone();
        let list = list.clone();
        let status = status.clone();
        Rc::new(move || {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let s = state.borrow();
            let ids = s.queue.ids().to_vec();
            for (pos, id) in ids.iter().enumerate() {
                let name = s
                    .playlist
                    .tracks
                    .iter()
                    .find(|t| t.id == *id)
                    .map(|t| t.display_name())
                    .unwrap_or_else(|| "(missing)".to_string());
                let label = Label::builder()
                    .label(gtk_safe(&format!("{}. {}", pos + 1, name)))
                    .halign(Align::Start)
                    .ellipsize(gtk4::pango::EllipsizeMode::End)
                    .build();
                label.set_margin_start(6);
                label.set_margin_end(6);
                label.set_margin_top(3);
                label.set_margin_bottom(3);
                let row = gtk4::ListBoxRow::new();
                row.set_child(Some(&label));
                list.append(&row);
            }
            let n = ids.len();
            status.set_text(&format!("{n} queued track{}", if n == 1 { "" } else { "s" }));
        })
    };
    rebuild();

    // Selected queue position (0-based), or None.
    let selected_pos = {
        let list = list.clone();
        move || list.selected_row().map(|r| r.index() as usize)
    };

    // Button row: Up / Down / Remove / Clear / Randomize.
    let btn_row = GtkBox::new(Orientation::Horizontal, 4);
    let btn_up = Button::with_label("↑ Up");
    let btn_down = Button::with_label("↓ Down");
    let btn_remove = Button::with_label("✕ Remove");
    let btn_clear = Button::with_label("Clear");
    let btn_random = Button::with_label("Randomize");
    for b in [&btn_up, &btn_down, &btn_remove, &btn_clear, &btn_random] {
        btn_row.append(b);
    }
    root.append(&btn_row);

    // Each op mutates the queue, rebuilds this list, and refreshes the main
    // playlist badges.
    let after_op: Rc<dyn Fn()> = {
        let rebuild = rebuild.clone();
        let refresh_main = refresh_main.clone();
        Rc::new(move || {
            rebuild();
            refresh_main();
        })
    };

    {
        let state = state.clone();
        let sel = selected_pos.clone();
        let list = list.clone();
        let after = after_op.clone();
        btn_up.connect_clicked(move |_| {
            if let Some(pos) = sel() {
                state.borrow_mut().queue.move_up(pos);
                after();
                if pos > 0 {
                    if let Some(row) = list.row_at_index((pos - 1) as i32) {
                        list.select_row(Some(&row));
                    }
                }
            }
        });
    }
    {
        let state = state.clone();
        let sel = selected_pos.clone();
        let list = list.clone();
        let after = after_op.clone();
        btn_down.connect_clicked(move |_| {
            if let Some(pos) = sel() {
                state.borrow_mut().queue.move_down(pos);
                after();
                if let Some(row) = list.row_at_index((pos + 1) as i32) {
                    list.select_row(Some(&row));
                }
            }
        });
    }
    {
        let state = state.clone();
        let sel = selected_pos.clone();
        let after = after_op.clone();
        btn_remove.connect_clicked(move |_| {
            if let Some(pos) = sel() {
                let id = state.borrow().queue.ids().get(pos).copied();
                if let Some(id) = id {
                    state.borrow_mut().queue.dequeue(id);
                    after();
                }
            }
        });
    }
    {
        let state = state.clone();
        let after = after_op.clone();
        btn_clear.connect_clicked(move |_| {
            state.borrow_mut().queue.clear();
            after();
        });
    }
    {
        let state = state.clone();
        let after = after_op.clone();
        btn_random.connect_clicked(move |_| {
            state.borrow_mut().queue.shuffle();
            after();
        });
    }

    // Double-click a queued row → play it now (jump to its playlist position,
    // remove it from the queue, start playback).
    {
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        let after = after_op.clone();
        list.connect_row_activated(move |_, row| {
            let pos = row.index() as usize;
            let id = state.borrow().queue.ids().get(pos).copied();
            if let Some(id) = id {
                let track_idx = {
                    let mut s = state.borrow_mut();
                    s.queue.dequeue(id);
                    s.playlist.tracks.iter().position(|t| t.id == id)
                };
                if let Some(idx) = track_idx {
                    state.borrow_mut().playlist.jump_to(idx);
                    play_and_update();
                }
                after();
            }
        });
    }

    // Esc hides; every other key delegates to the shared handler.
    {
        let key_ctrl = EventControllerKey::new();
        let handler = handle_key.clone();
        let win_wk = win.downgrade();
        key_ctrl.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(w) = win_wk.upgrade() {
                    w.hide();
                }
                return glib::Propagation::Stop;
            }
            handler(key)
        });
        win.add_controller(key_ctrl);
    }

    win.set_child(Some(&root));
    QUEUE_MANAGER_WIN.with(|w| *w.borrow_mut() = Some(win.clone()));
    win.present();
}
