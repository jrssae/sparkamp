use super::*;

/// The Jump window — the search / jump-to interface opened with `j`, which
/// also hosts Queue mode (the former standalone Queue Manager, folded in).
///
/// Handed back by [`build`] and read by [`connect`], by the key dispatcher,
/// and by the fullscreen visualiser's Esc handling.
pub(super) struct JumpWin {
    pub(super) jump_win: gtk4::Window,
    pub(super) jump_entry: gtk4::SearchEntry,
    pub(super) jump_box: ListBox,
    pub(super) jump_indices: Rc<RefCell<Vec<usize>>>,
    pub(super) jump_queue_mode: Rc<Cell<bool>>,
    pub(super) rebuild_jump: Rc<dyn Fn()>,
    pub(super) open_jump_mode: Rc<dyn Fn(bool)>,
}

/// Build the Jump window: its entry, results list, status line, mode
/// selector and the two panes behind it.
///
/// Split out of `player::build` (breakup step 9b). Four bindings flow in and
/// seven flow out, which is why this is a bundle rather than an `install`.
pub(super) fn build(ctx: &PlayerCtx, btn_jump_vol: &Button) -> JumpWin {
    // Aliased under their original names so the moved body is unchanged.
    let state = ctx.state.clone();
    let window = ctx.window.clone();
    let btn_jump_vol = btn_jump_vol.clone();
    let rebuild_playlist = ctx.rebuild_playlist.clone();
    let play_and_update = ctx.play_and_update.clone();

    // Jump window — dedicated search/jump interface (opened with 'j').
    // Lives in its own window separate from the playlist so the two don't
    // overlap.  Populated fresh every time it opens.
    // ══════════════════════════════════════════════════════════════════════════
    let jump_entry = gtk4::SearchEntry::new();
    jump_entry.set_placeholder_text(Some("Search… (↑↓ navigate, Enter play, Esc close)"));
    jump_entry.set_margin_top(8);
    jump_entry.set_margin_bottom(4);
    jump_entry.set_margin_start(8);
    jump_entry.set_hexpand(true);

    let jump_clear_btn = Button::with_label("✕");
    jump_clear_btn.add_css_class("pl-btn");
    // The label text is a bare glyph — a screen reader needs a real word.
    jump_clear_btn.update_property(&[gtk4::accessible::Property::Label("Clear search")]);
    jump_clear_btn.set_margin_top(8);
    jump_clear_btn.set_margin_bottom(4);
    jump_clear_btn.set_margin_end(8);

    let jump_search_row = GtkBox::new(Orientation::Horizontal, 4);
    jump_search_row.append(&jump_entry);
    jump_search_row.append(&jump_clear_btn);

    let jump_box = ListBox::new();
    jump_box.add_css_class("playlist");
    jump_box.set_selection_mode(gtk4::SelectionMode::Single);

    let jump_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .min_content_height(280)
        .child(&jump_box)
        .build();

    // Status line below the results box: shows match count or a hint.
    let jump_status = gtk4::Label::builder()
        .halign(Align::Start)
        .margin_start(8)
        .margin_end(8)
        .margin_top(2)
        .margin_bottom(4)
        .build();
    jump_status.add_css_class("status-label");

    // Mode selector (radio): Jump (search + jump-to) vs Queue (manage the play
    // queue). The window hosts both panes and shows one at a time.
    let radio_jump = gtk4::CheckButton::with_label("Jump");
    let radio_queue = gtk4::CheckButton::with_label("Queue");
    radio_queue.set_group(Some(&radio_jump));
    radio_jump.set_active(true);
    let jump_mode_row = GtkBox::new(Orientation::Horizontal, 8);
    jump_mode_row.set_margin_top(6);
    jump_mode_row.set_margin_start(8);
    jump_mode_row.set_margin_end(8);
    jump_mode_row.append(&radio_jump);
    jump_mode_row.append(&radio_queue);

    // Jump-mode content, wrapped so it can be shown/hidden as one unit.
    let jump_pane = GtkBox::new(Orientation::Vertical, 0);
    jump_pane.set_vexpand(true);
    jump_pane.append(&jump_search_row);
    jump_pane.append(&jump_scroll);
    jump_pane.append(&jump_status);

    // Queue-mode content (the former standalone Queue Manager, folded in here).
    let (queue_pane, queue_rebuild) = build_queue_panel(
        state.clone(),
        rebuild_playlist.clone(),
        play_and_update.clone(),
    );
    queue_pane.set_visible(false);

    // Shared "am I in queue mode?" flag so the jump-window key controller knows
    // whether arrow/Enter/Ctrl+Q keys belong to the search list or the queue.
    let jump_queue_mode = Rc::new(Cell::new(false));

    let jump_root = gtk4::Box::new(Orientation::Vertical, 0);
    jump_root.append(&jump_mode_row);
    jump_root.append(&jump_pane);
    jump_root.append(&queue_pane);

    // Switch panes. `queue_mode = true` shows the queue; false shows search.
    let apply_jump_mode: Rc<dyn Fn(bool)> = {
        let jump_pane = jump_pane.clone();
        let queue_pane = queue_pane.clone();
        let jump_entry = jump_entry.clone();
        let queue_rebuild = queue_rebuild.clone();
        let flag = jump_queue_mode.clone();
        Rc::new(move |queue_mode: bool| {
            flag.set(queue_mode);
            jump_pane.set_visible(!queue_mode);
            queue_pane.set_visible(queue_mode);
            if queue_mode {
                queue_rebuild();
            } else {
                jump_entry.grab_focus();
            }
        })
    };
    {
        let apply = apply_jump_mode.clone();
        radio_queue.connect_toggled(move |b| apply(b.is_active()));
    }

    let jump_win = gtk4::Window::builder()
        .title("Jump / Queue")
        .default_width(380)
        .default_height(360)
        .modal(false)
        .build();
    jump_win.set_transient_for(Some(&window));
    jump_win.set_child(Some(&jump_root));
    // Hide instead of destroy when the user closes the window so it can be
    // reopened later.  Without this, the underlying GObject may be freed after
    // the first close, making subsequent `present()` calls a no-op.
    jump_win.set_hide_on_close(true);
    jump_win.connect_visible_notify({
        let btn = btn_jump_vol.clone();
        move |w| {
            if w.is_visible() {
                btn.add_css_class("mode-btn-active");
            } else {
                btn.remove_css_class("mode-btn-active");
            }
        }
    });

    // Open the window in a specific mode: `j` / find-button → Jump,
    // `q` → Queue. Present() also (re)runs the visible-notify + entry-change
    // seams below that refresh the active pane.
    let open_jump_mode: Rc<dyn Fn(bool)> = {
        let jump_win = jump_win.clone();
        let radio_jump = radio_jump.clone();
        let radio_queue = radio_queue.clone();
        let apply = apply_jump_mode.clone();
        Rc::new(move |queue_mode: bool| {
            if queue_mode {
                radio_queue.set_active(true);
            } else {
                radio_jump.set_active(true);
            }
            // Apply explicitly too: set_active is a no-op (no toggle signal) if
            // the radio was already in that state.
            apply(queue_mode);
            jump_win.present();
        })
    };

    // Maps each visible row in jump_box → the original track index in the playlist.
    let jump_indices: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    // Maximum rows shown in the jump list.  Caps widget creation so the window
    // stays responsive on playlists with tens of thousands of tracks.
    const MAX_JUMP_RESULTS: usize = 500;

    // Closure: clear and repopulate jump_box based on the current query.
    let rebuild_jump: Rc<dyn Fn()> = {
        let state = state.clone();
        let jump_entry = jump_entry.clone();
        let jump_box = jump_box.clone();
        let jump_indices = jump_indices.clone();
        let jump_status = jump_status.clone();
        Rc::new(move || {
            // Stamp any unstamped entries so queue badges resolve (idempotent).
            state.borrow_mut().playlist.ensure_ids();
            // remove_all() is a single GTK call instead of O(n) individual removes.
            jump_box.remove_all();
            let mut indices = jump_indices.borrow_mut();
            indices.clear();

            let q = jump_entry.text();
            // Empty query: show a hint and leave the list empty.
            // Without this guard, an empty query would match every track and
            // create tens of thousands of widgets, freezing the UI.
            if q.trim().is_empty() {
                let total = state.borrow().playlist.len();
                jump_status.set_text(&format!("{total} tracks — type to search"));
                return;
            }

            let all_matches = {
                let s = state.borrow();
                s.playlist.search_indices(&q)
            };
            let total_matches = all_matches.len();
            let capped = total_matches > MAX_JUMP_RESULTS;
            let s = state.borrow();
            for &idx in all_matches.iter().take(MAX_JUMP_RESULTS) {
                let track = &s.playlist.tracks[idx];
                // Manual-queue position badge (prefix), mirroring the playlist.
                let badge = s.queue.badge(track.id);
                let label_text = if track.artist.is_empty() {
                    format!("{}{:2}. {}", badge, idx + 1, track.title)
                } else {
                    format!("{}{:2}. {} — {}", badge, idx + 1, track.artist, track.title)
                };
                let row_label = gtk4::Label::builder()
                    .label(&label_text)
                    .halign(Align::Start)
                    .ellipsize(gtk4::pango::EllipsizeMode::End)
                    .build();
                row_label.set_margin_start(6);
                row_label.set_margin_end(6);
                row_label.set_margin_top(3);
                row_label.set_margin_bottom(3);
                let row = gtk4::ListBoxRow::new();
                row.set_child(Some(&row_label));
                jump_box.append(&row);
                indices.push(idx);
            }
            drop(s);

            // Status line.
            if total_matches == 0 {
                jump_status.set_text("No matches");
            } else if capped {
                jump_status.set_text(&format!(
                    "Showing {} of {} matches. Type more to narrow",
                    MAX_JUMP_RESULTS, total_matches
                ));
            } else {
                jump_status.set_text(&format!("{total_matches} match{}", if total_matches == 1 { "" } else { "es" }));
            }

            // Auto-select the first row so Enter immediately plays.
            if let Some(row) = jump_box.row_at_index(0) {
                jump_box.select_row(Some(&row));
            }
        })
    };
    // Let queue changes renumber the Jump-mode search-list badges too.
    set_jump_refresh(rebuild_jump.clone());

    // Wire up the jump-window clear button now that rebuild_jump is in scope.
    {
        let e = jump_entry.clone();
        let rj = rebuild_jump.clone();
        jump_clear_btn.connect_clicked(move |_| {
            gtk4::prelude::EditableExt::set_text(&e, "");
            rj();
        });
    }

    // ══════════════════════════════════════════════════════════════════════════

    JumpWin {
        jump_win,
        jump_entry,
        jump_box,
        jump_indices,
        jump_queue_mode,
        rebuild_jump,
        open_jump_mode,
    }
}

/// Wire the Jump window's handlers: typing refilters, Enter plays the
/// selection, double-click plays a row, and the key controller navigates.
///
/// Separate from [`build`] because it must run *after* the key dispatcher
/// exists — the jump window's own controller delegates transport keys to it.
/// That ordering is why the two halves sit far apart in `build`, and keeping
/// them as two functions is what let this move without a hoist.
pub(super) fn connect(ctx: &PlayerCtx, jw: &JumpWin) {
    let state = ctx.state.clone();
    let rebuild_playlist = ctx.rebuild_playlist.clone();
    let patch_pl_row = ctx.patch_pl_row.clone();
    let play_and_update = ctx.play_and_update.clone();
    let jump_win = jw.jump_win.clone();
    let jump_entry = jw.jump_entry.clone();
    let jump_box = jw.jump_box.clone();
    let jump_indices = jw.jump_indices.clone();
    let jump_queue_mode = jw.jump_queue_mode.clone();
    let rebuild_jump = jw.rebuild_jump.clone();

    // Jump window callbacks (wired after handle_key so the key controller can
    // delegate transport shortcuts to it).
    // ══════════════════════════════════════════════════════════════════════════

    // Typing in the jump entry: refilter once typing pauses.
    //
    // Not immediate, because a rebuild is not cheap on a large playlist:
    // `search_indices` builds a lowercase haystack per track (measured at
    // 14-37 ms over 36,329 tracks), `ensure_ids` walks every entry, and up to
    // MAX_JUMP_RESULTS Label/ListBoxRow trees are built and thrown away. Doing
    // that per character, on the main loop, is felt as typing lag.
    //
    // 300 ms of quiet, cancelling any rebuild still pending — the same shape
    // the Files-view search uses. Only this path is debounced: the clear
    // button, the queue-badge refresh and the key dispatcher all still rebuild
    // at once, because those follow a deliberate action rather than a
    // keystroke.
    let jump_pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    jump_entry.connect_changed({
        let rebuild_jump = rebuild_jump.clone();
        let jump_pending = jump_pending.clone();
        move |_| {
            if let Some(src) = jump_pending.borrow_mut().take() {
                src.remove();
            }
            let rebuild = rebuild_jump.clone();
            let pending_inner = jump_pending.clone();
            let src = glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
                // Clear the slot before rebuilding: `rebuild_jump` can re-enter
                // this handler (it does not today, but it touches the entry),
                // and a stale SourceId here would be removed after it fired.
                *pending_inner.borrow_mut() = None;
                rebuild();
                glib::ControlFlow::Break
            });
            *jump_pending.borrow_mut() = Some(src);
        }
    });

    // Bring the results list up to date right now, if a debounced rebuild is
    // still waiting.
    //
    // Anything that acts on the list has to call this first. Type a query and
    // press Enter inside the debounce window and `jump_indices` still holds
    // the previous query's rows — on the very first query it holds none at
    // all, so Enter would close the window having played nothing. A no-op when
    // nothing is pending, which is the common case.
    let flush_jump: Rc<dyn Fn()> = {
        let rebuild_jump = rebuild_jump.clone();
        let jump_pending = jump_pending.clone();
        Rc::new(move || {
            let pending = jump_pending.borrow_mut().take();
            if let Some(src) = pending {
                src.remove();
                rebuild_jump();
            }
        })
    };

    // Enter: play the selected (or first) result and close the window.
    jump_entry.connect_activate({
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        let patch_pl_row = patch_pl_row.clone();
        let jump_box = jump_box.clone();
        let jump_indices = jump_indices.clone();
        let jump_win_wk = jump_win.downgrade();
        let flush_jump = flush_jump.clone();
        move |_| {
            // Enter can arrive mid-debounce; act on the current query, not the
            // one the list happens to be showing.
            flush_jump();
            let sel_row_idx = jump_box.selected_row().map(|r| r.index() as usize);
            if let Some(list_pos) = sel_row_idx {
                if let Some(&track_idx) = jump_indices.borrow().get(list_pos) {
                    let old_idx = state.borrow().playlist.current_index;
                    state.borrow_mut().playlist.jump_to(track_idx);
                    play_and_update();
                    if old_idx != track_idx {
                        patch_pl_row(old_idx);
                    }
                }
            }
            if let Some(w) = jump_win_wk.upgrade() {
                w.close();
            }
        }
    });

    // SearchEntry emits stop-search (and consumes Escape) before window-level
    // key controllers see it.  Wire the signal directly so Escape always closes.
    jump_entry.connect_stop_search({
        let jw = jump_win.clone();
        move |_| {
            jw.close();
        }
    });

    // Key controller for the jump window: ↑↓ navigate rows; Escape as a
    // fallback in case focus is on the list box rather than the entry.
    // PropagationPhase::Capture ensures we intercept before child widgets.
    {
        let key_ctrl = EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let jb = jump_box.clone();
        let jw_wk = jump_win.downgrade();
        let state_jq = state.clone();
        let jump_indices_jq = jump_indices.clone();
        let rebuild_pl_jq = rebuild_playlist.clone();
        let qmode = jump_queue_mode.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, modifier| match key {
            // Esc closes the window in either mode.
            gdk::Key::Escape => {
                if let Some(w) = jw_wk.upgrade() {
                    w.close();
                }
                glib::Propagation::Stop
            }
            // Arrow nav drives the search-results list — Jump mode only; in
            // Queue mode the queue ListBox handles Up/Down natively.
            gdk::Key::Up if !qmode.get() => {
                let cur = jb.selected_row().map(|r| r.index()).unwrap_or(1);
                if let Some(row) = jb.row_at_index((cur - 1).max(0)) {
                    jb.select_row(Some(&row));
                }
                glib::Propagation::Stop
            }
            gdk::Key::Down if !qmode.get() => {
                let cur = jb.selected_row().map(|r| r.index()).unwrap_or(-1);
                if let Some(row) = jb.row_at_index(cur + 1) {
                    jb.select_row(Some(&row));
                }
                glib::Propagation::Stop
            }
            // Ctrl+Q queues the highlighted match (Jump mode only — plain `q`
            // stays a search character in the entry). Updates the jump,
            // playlist, and queue-panel badges.
            gdk::Key::q | gdk::Key::Q
                if !qmode.get() && modifier.contains(gdk::ModifierType::CONTROL_MASK) =>
            {
                let sel = jb.selected_row().map(|r| r.index() as usize);
                if let Some(list_pos) = sel {
                    let track_idx = jump_indices_jq.borrow().get(list_pos).copied();
                    if let Some(track_idx) = track_idx {
                        {
                            let mut s = state_jq.borrow_mut();
                            s.playlist.ensure_ids();
                            if let Some(id) = s.playlist.tracks.get(track_idx).map(|t| t.id) {
                                s.queue.toggle(id);
                            }
                        }
                        // refresh_queue_manager rebuilds the jump list + queue
                        // panel; rebuild the playlist badges separately.
                        rebuild_pl_jq();
                        refresh_queue_manager();
                    }
                }
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        jump_win.add_controller(key_ctrl);
    }

    // Double-clicking a result plays it immediately.
    jump_box.connect_row_activated({
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        let patch_pl_row = patch_pl_row.clone();
        let jump_indices = jump_indices.clone();
        let jump_win_wk = jump_win.downgrade();
        move |_, row| {
            let list_pos = row.index() as usize;
            if let Some(&track_idx) = jump_indices.borrow().get(list_pos) {
                let old_idx = state.borrow().playlist.current_index;
                state.borrow_mut().playlist.jump_to(track_idx);
                play_and_update();
                if old_idx != track_idx {
                    patch_pl_row(old_idx);
                }
            }
            if let Some(w) = jump_win_wk.upgrade() {
                w.close();
            }
        }
    });

    // ══════════════════════════════════════════════════════════════════════════
}
