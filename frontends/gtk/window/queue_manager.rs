// Play-queue panel — the "Queue mode" content of the combined Jump/Queue
// window (player.rs). Lists the manual play queue in order with reorder /
// remove / clear / randomize controls and double-click "play now". Built once
// and embedded in the jump window; its rebuild closure is stashed in a
// thread_local so advance paths that drain the queue during playback can
// live-refresh it (see `refresh_queue_manager`).
// (include!d into window/mod.rs, so no module-level `//!` docs here.)

use std::cell::RefCell as StdRefCell;

thread_local! {
    /// The embedded queue panel's list-rebuild closure, so external queue
    /// drains (Next / b / EOS / MPRIS) can renumber it alongside the playlist
    /// badges. Set when the panel is built.
    static QUEUE_MANAGER_REFRESH: StdRefCell<Option<Rc<dyn Fn()>>> = const { StdRefCell::new(None) };
}

/// Rebuild the queue panel's list if one has been built. Called from the GTK
/// advance paths after a queued entry is consumed so an open Queue view stays
/// in sync with the playlist badges. Cheap (reads `state.queue`); a no-op if no
/// panel exists yet.
fn refresh_queue_manager() {
    let cb = QUEUE_MANAGER_REFRESH.with(|r| r.borrow().clone());
    if let Some(cb) = cb {
        cb();
    }
}

/// Build the queue-management panel: an ordered list of queued tracks plus
/// Up / Down / Remove / Clear / Randomize controls and double-click play-now.
///
/// - `refresh_main`: the playlist rebuild closure — called after any queue
///   mutation so the main playlist's `[n]` badges stay in sync.
/// - `play_and_update`: the shared "start playing current track" closure, used
///   by double-click "play now".
///
/// Returns the panel widget (to embed in the jump window) and its list-rebuild
/// closure (also stashed in `QUEUE_MANAGER_REFRESH`).
fn build_queue_panel(
    state: Rc<RefCell<AppState>>,
    refresh_main: Rc<dyn Fn()>,
    play_and_update: Rc<dyn Fn()>,
) -> (gtk4::Box, Rc<dyn Fn()>) {
    let root = GtkBox::new(Orientation::Vertical, 6);
    root.set_margin_top(4);
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
        .min_content_height(240)
        .child(&list)
        .build();
    root.append(&scroll);

    let status = Label::new(None);
    status.set_halign(Align::Start);
    status.add_css_class("status-label");
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
    QUEUE_MANAGER_REFRESH.with(|r| *r.borrow_mut() = Some(rebuild.clone()));

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
        b.add_css_class("pl-btn");
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

    (root, rebuild)
}
