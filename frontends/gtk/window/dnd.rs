use super::*;

/// Drag and drop for the active playlist: the `DragSource` that lifts
/// selected rows out of the playlist `TreeView`, and the `DropTarget` that
/// accepts audio files dragged in from a file manager or from the Media
/// Library's own views.
///
/// Lifted out of `player::build` whole. It reads a wide slice of the window
/// but writes nothing back that the rest of `build` reads afterwards, which
/// is what made it the cleanest cut in the file: every binding below flows
/// in, none flows out.
pub(super) fn install(ctx: &PlayerCtx) {
    // Aliased under their original names so the moved body is unchanged from
    // the day it lived in `build` — a rename here would be a diff no reviewer
    // could check against the original.
    let state = ctx.state.clone();
    let window = ctx.window.clone();
    let playlist_win = ctx.playlist_win.clone();
    let pl_view = ctx.pl_view.clone();
    let pl_scroll = ctx.pl_scroll.clone();
    let pl_status_label = ctx.pl_status_label.clone();
    let seek_bar = ctx.seek_bar.clone();
    let repeat_icon = ctx.repeat_icon.clone();
    let repeat_label = ctx.repeat_label.clone();
    let btn_prev = ctx.btn_prev.clone();
    let btn_play = ctx.btn_play.clone();
    let btn_pause = ctx.btn_pause.clone();
    let btn_stop = ctx.btn_stop.clone();
    let btn_next = ctx.btn_next.clone();
    let btn_repeat = ctx.btn_repeat.clone();
    let btn_shuffle = ctx.btn_shuffle.clone();
    let btn_pl = ctx.btn_pl.clone();
    let set_track = ctx.set_track.clone();
    let rebuild_playlist = ctx.rebuild_playlist.clone();
    let patch_pl_row = ctx.patch_pl_row.clone();
    let scroll_to_row_if_needed = ctx.scroll_to_row_if_needed.clone();
    let play_and_update = ctx.play_and_update.clone();
    let refresh_now_playing = ctx.refresh_now_playing.clone();
    let refresh_pl_status = ctx.refresh_pl_status.clone();
    let remove_selected = ctx.remove_selected.clone();
    let queue_toggle_selection = ctx.queue_toggle_selection.clone();
    let current_drives = ctx.current_drives.clone();
    let current_devices = ctx.current_devices.clone();
    let burn_queues = ctx.burn_queues.clone();
    let copy_files_holder = ctx.copy_files_holder.clone();
    let burn_refresh_holder = ctx.burn_refresh_holder.clone();
    let current_track_meta_tx = ctx.current_track_meta_tx.clone();

    // ── DragSource on the TreeView ──────────────────────────────────────────
    // Persistent multi-selection snapshot for the active playlist.
    // Updated by selection.connect_changed whenever count > 1.
    // Cleared only on count==0 AND when a drag isn't in progress —
    // GtkTreeView transiently drops to 0 selected rows during the drag
    // event chain, and clearing then would wipe the snapshot before
    // the drop target gets a chance to read it.
    let pl_drag_selection: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let pl_drag_active: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    {
        let snap_obs = pl_drag_selection.clone();
        let active_obs = pl_drag_active.clone();
        #[allow(deprecated)]
        pl_view.selection().connect_changed(move |sel| {
            #[allow(deprecated)]
            let count = sel.count_selected_rows() as usize;
            if count > 1 {
                #[allow(deprecated)]
                let (paths, _model) = sel.selected_rows();
                let v: Vec<usize> = paths.iter()
                    .filter_map(|p| p.indices().first().copied())
                    .map(|i| i as usize)
                    .collect();
                *snap_obs.borrow_mut() = v;
            } else if count == 0 && !active_obs.get() {
                snap_obs.borrow_mut().clear();
            } else {
            }
        });
    }

    // Snapshot upkeep around a press.
    //
    // GTK collapses the selection to the clicked row on press, and a drag may
    // or may not follow. The snapshot therefore has to survive the press — a
    // drag needs it — and be reconciled on *release*, once it is known that no
    // drag happened and the collapse is what the user meant.
    //
    // The visual multi-select restore used to run from here and now runs from
    // `connect_drag_begin` below. On press it fired for every click that
    // landed inside the snapshot, so after Select All — where the snapshot is
    // every row — each click re-applied the whole set and the selection could
    // not be changed with the mouse at all, with or without Ctrl/Shift.
    // Invert Selection had the same effect for the same reason; Select None
    // did not, because it empties the snapshot.
    {
        let press = GestureClick::new();
        press.set_button(gtk4::gdk::BUTTON_PRIMARY);
        let pl_view_p = pl_view.clone();
        let pl_view_rel = pl_view.clone();
        let snap = pl_drag_selection.clone();
        let snap_rel = pl_drag_selection.clone();
        let active_press = pl_drag_active.clone();
        let active_rel = pl_drag_active.clone();
        press.connect_pressed(move |_, _n, x, y| {
            #[allow(deprecated)]
            let row_under = pl_view_p.path_at_pos(x as i32, y as i32)
                .and_then(|(p, _, _, _)| p)
                .and_then(|p| p.indices().first().copied())
                .map(|i| i as usize);
            let inside_multi = {
                let s = snap.borrow();
                s.len() > 1 && row_under.map_or(false, |r| s.contains(&r))
            };
            if !inside_multi && !active_press.get() {
                // Click landed outside the multi-selection (and no drag is in
                // progress) → forget the snapshot, so a later click on a row
                // that used to be selected doesn't resurrect the whole group.
                snap.borrow_mut().clear();
            }
        });
        // The button came back up without a drag starting, so the collapse GTK
        // did on press is the selection the user asked for. Mirror it back into
        // the snapshot — this is what lets a plain click out of a
        // multi-selection actually stick.
        press.connect_released(move |_, _n, _x, _y| {
            if active_rel.get() {
                return; // a drag owns the snapshot until it ends
            }
            #[allow(deprecated)]
            let (paths, _model) = pl_view_rel.selection().selected_rows();
            *snap_rel.borrow_mut() = paths
                .iter()
                .filter_map(|p| p.indices().first().copied())
                .map(|i| i as usize)
                .collect();
        });
        pl_view.add_controller(press);
    }
    // Emits a FileList of every currently-selected row's path so the drag
    // is consumable by both the active-playlist internal reorder target
    // and external destinations like sidebar pl rows / GNOME Files.  Uses
    // the press-time snapshot when GTK has already collapsed selection to
    // a single row.
    {
        let drag_src = DragSource::new();
        drag_src.set_actions(gdk::DragAction::COPY);
        let pl_view_ds = pl_view.clone();
        let state_ds   = state.clone();
        let drag_sel_ds = pl_drag_selection.clone();
        // Flip drag_active on drag begin / end so the selection-changed
        // observer doesn't wipe the snapshot during the drag chain.
        {
            let active = pl_drag_active.clone();
            let snap_begin = pl_drag_selection.clone();
            let pl_view_begin = pl_view.clone();
            drag_src.connect_drag_begin(move |_, _| {
                active.set(true);
                // Re-apply the multi-select GTK collapsed on press, so the drag
                // icon shows every dragged row highlighted rather than just the
                // one under the cursor. `connect_prepare` has already run and
                // written the final source indices here, so this is exactly the
                // set being dragged. Restoring here instead of on press is what
                // keeps a plain click from re-selecting the whole group.
                let rows = snap_begin.borrow().clone();
                if rows.len() > 1 {
                    #[allow(deprecated)]
                    let selection = pl_view_begin.selection();
                    #[allow(deprecated)]
                    selection.unselect_all();
                    #[allow(deprecated)]
                    if let Some(model) = pl_view_begin.model() {
                        for idx in &rows {
                            if let Some(iter) = model.iter_nth_child(None, *idx as i32) {
                                #[allow(deprecated)]
                                selection.select_iter(&iter);
                            }
                        }
                    }
                }
            });
        }
        {
            let active = pl_drag_active.clone();
            drag_src.connect_drag_end(move |_, _, _| {
                active.set(false);
            });
        }
        {
            let active = pl_drag_active.clone();
            drag_src.connect_drag_cancel(move |_, _, _| {
                active.set(false);
                false
            });
        }
        let drag_active_pp = pl_drag_active.clone();
        drag_src.connect_prepare(move |_, x, y| {
            // Flip drag_active up-front — selection-changed events
            // between prepare and drag-begin would otherwise wipe the
            // snapshot before drop reads it.
            drag_active_pp.set(true);
            #[allow(deprecated)]
            let row_under = match pl_view_ds.path_at_pos(x as i32, y as i32) {
                Some((Some(p), _, _, _)) => p.indices().first().copied().map(|i| i as usize),
                _ => None,
            };
            // Prefer the connect_changed snapshot (multi-select); fall back
            // to the live selection; final fallback is the row under cursor.
            let snapshot = drag_sel_ds.borrow().clone();
            let sel_indices: Vec<usize> = if snapshot.len() > 1
                && row_under.map_or(false, |r| snapshot.contains(&r))
            {
                snapshot
            } else {
                #[allow(deprecated)]
                let (selected_paths, _model) = pl_view_ds.selection().selected_rows();
                let live: Vec<usize> = selected_paths
                    .iter()
                    .filter_map(|p| p.indices().first().copied())
                    .map(|i| i as usize)
                    .collect();
                if !live.is_empty() { live }
                else { row_under.into_iter().collect() }
            };
            // Stash final source indices so the drop target can do a
            // precise reorder without round-tripping through paths.
            *drag_sel_ds.borrow_mut() = sel_indices.clone();
            let s = state_ds.borrow();
            let paths: Vec<std::path::PathBuf> = sel_indices.iter()
                .filter_map(|i| s.playlist.tracks.get(*i))
                .map(|t| t.path.clone())
                .collect();
            if paths.is_empty() { return None }
            let files: Vec<gio::File> = paths.iter()
                .map(|p| gio::File::for_path(p))
                .collect();
            let fl = gdk::FileList::from_array(&files);
            Some(gdk::ContentProvider::for_value(&fl.to_value()))
        });
        pl_view.add_controller(drag_src);
    }

    // Active-playlist internal reorder via FileList drop on pl_view.
    // Source indices are recovered by looking up each dropped path in the
    // current playlist; any path not found is treated as a new track and
    // appended (so cross-window drops from ML/editor also work here).
    {
        let drop_tgt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let state_dnd = state.clone();
        let rebuild_dnd = rebuild_playlist.clone();
        let pl_view_dnd = pl_view.clone();
        let drag_sel_drop = pl_drag_selection.clone();

        drop_tgt.connect_drop(move |_, value, x, y| {
            let file_list = match value.get::<gdk::FileList>() {
                Ok(fl) => fl,
                Err(_) => {
                    return false;
                }
            };
            let dropped: Vec<std::path::PathBuf> = file_list.files().iter()
                .filter_map(|f| f.path())
                .collect();
            if dropped.is_empty() { return false }

            let n = state_dnd.borrow().playlist.len();
            // Use dest_row_at_pos so the drop position honors the
            // before/after halves of the target row — dropping on the
            // bottom half of row 13 inserts at 14, top half at 13.
            // Without this, dropping [10,11,12,13] onto row 14 always
            // computed insert_at=10 (no visible move).
            #[allow(deprecated)]
            let dst_pos = match pl_view_dnd.dest_row_at_pos(x as i32, y as i32) {
                Some((Some(p), drop_pos)) => {
                    let row = p.indices().first().copied().unwrap_or(0) as usize;
                    match drop_pos {
                        gtk4::TreeViewDropPosition::Before
                        | gtk4::TreeViewDropPosition::IntoOrBefore => row,
                        gtk4::TreeViewDropPosition::After
                        | gtk4::TreeViewDropPosition::IntoOrAfter => row + 1,
                        _ => row,
                    }
                }
                _ => n,
            };

            // Prefer the press-time drag selection snapshot — it's the
            // authoritative source-row list and avoids the path-comparison
            // mismatch that round-trips through gio::File.  Empty snapshot
            // means the drop came from another window, so fall back to
            // path matching to decide reorder-vs-add.
            let snapshot: Vec<usize> = drag_sel_drop.borrow().clone();
            let mut existing_src_indices: Vec<usize>;
            let mut new_paths: Vec<std::path::PathBuf>;
            if !snapshot.is_empty() {
                existing_src_indices = snapshot;
                new_paths = Vec::new();
            } else {
                existing_src_indices = Vec::new();
                new_paths = Vec::new();
                let s = state_dnd.borrow();
                // Index the playlist once. This used to scan it per dropped
                // path, which is fine for the handful of files a file manager
                // sends and quadratic for the 36,000 a "select all" in the
                // Media Library sends.
                let mut by_path: std::collections::HashMap<&std::path::Path, usize> =
                    std::collections::HashMap::with_capacity(s.playlist.tracks.len());
                for (i, t) in s.playlist.tracks.iter().enumerate() {
                    by_path.entry(t.path.as_path()).or_insert(i);
                }
                for dp in &dropped {
                    match by_path.get(dp.as_path()) {
                        Some(i) => existing_src_indices.push(*i),
                        None    => new_paths.push(dp.clone()),
                    }
                }
            }

            let did_move = !existing_src_indices.is_empty();
            // Capture the post-move range so the idle rebuild below can
            // re-select the moved rows — without this, the drop appears
            // to clear selection and the user can't see what was moved.
            let mut moved_range: Option<(usize, usize)> = None;
            if did_move {
                let mut s = state_dnd.borrow_mut();
                // Remove highest-first so earlier removes don't invalidate later indices.
                let mut sorted = existing_src_indices.clone();
                sorted.sort_unstable_by(|a, b| b.cmp(a));
                let mut adjusted_dst = dst_pos;
                let mut removed: Vec<crate::model::Track> = Vec::new();
                for src in sorted.iter() {
                    if *src < s.playlist.tracks.len() {
                        let t = s.playlist.tracks.remove(*src);
                        if *src < adjusted_dst { adjusted_dst -= 1; }
                        removed.push(t);
                    }
                }
                // Reinsert in original drop order at adjusted_dst.
                removed.reverse();
                let cap = s.playlist.tracks.len();
                let insert_at = adjusted_dst.min(cap);
                let removed_n = removed.len();
                for (i, t) in removed.into_iter().enumerate() {
                    s.playlist.tracks.insert(insert_at + i, t);
                }
                moved_range = Some((insert_at, removed_n));
            }

            // Everything a dropped row needs comes from the library, and what
            // does not is measured in the background. This used to call
            // `add_path` per file, which opens and re-parses each one on the
            // main thread — 27.974 ms apiece, so a Media Library "select all"
            // dropped here froze the window for about seventeen minutes.
            // Honour the append-vs-replace setting — for any file list,
            // whatever produced it. A file manager knows nothing about
            // Sparkamp's preferences, so the playlist window applies them on
            // arrival (decision, 2026-08-18).
            //
            // Only the add half is subject to the rule. `did_move` above is an
            // internal reorder of rows that are already in the playlist, and
            // clearing the playlist to "replace" it with its own rows would
            // delete the very tracks being dragged. A drop that both reorders
            // and adds keeps the reorder and appends, for the same reason.
            let did_add = if did_move {
                playlist_add::add_paths(&state_dnd, &new_paths).any()
            } else {
                playlist_add::add_with_mode(
                    &state_dnd,
                    &new_paths,
                    crate::playlist_add::AddMode::Behavior,
                )
                .any()
            };
            // Clear the press-time selection snapshot so a subsequent
            // single-row drag doesn't accidentally reorder the whole set.
            drag_sel_drop.borrow_mut().clear();
            if did_move || did_add {
                // Defer to next idle tick — splicing the model while GTK
                // is still unwinding the drop event can segfault.
                let rb = rebuild_dnd.clone();
                let pv = pl_view_dnd.clone();
                let snap_restore = drag_sel_drop.clone();
                glib::idle_add_local_once(move || {
                    rb();
                    // Restore selection to the moved range so the user
                    // sees what was just reordered.
                    if let Some((start, n)) = moved_range {
                        #[allow(deprecated)]
                        let selection = pv.selection();
                        #[allow(deprecated)]
                        selection.unselect_all();
                        let mut restored: Vec<usize> = Vec::new();
                        #[allow(deprecated)]
                        if let Some(model) = pv.model() {
                            for i in 0..n {
                                let row_idx = start + i;
                                if let Some(iter) = model.iter_nth_child(None, row_idx as i32) {
                                    #[allow(deprecated)]
                                    selection.select_iter(&iter);
                                    restored.push(row_idx);
                                }
                            }
                        }
                        // Re-seed the multi-select snapshot so a follow-up
                        // drag of the same rows works without redoing the
                        // shift-click sequence.
                        if restored.len() > 1 {
                            *snap_restore.borrow_mut() = restored;
                        }
                    }
                });
            }
            true
        });

        pl_view.add_controller(drop_tgt);
    }

    // ── Drop target: accept files dragged from an external file manager ───────
    // Handles gdk::FileList drops (the standard type produced by GNOME Files
    // and most GTK4-aware file managers).  Files are appended to the playlist;
    // directories are scanned recursively.  Attached to the ScrolledWindow so
    // the full visible playlist area is a valid drop zone.
    {
        let file_drop = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let state_fd = state.clone();
        let rebuild_fd = rebuild_playlist.clone();
        let status_fd = pl_status_label.clone();
        file_drop.connect_drop(move |_, value, _, _| {
            let file_list = match value.get::<gdk::FileList>() {
                Ok(fl) => fl,
                Err(_) => return false,
            };
            let dropped: Vec<std::path::PathBuf> =
                file_list.files().iter().filter_map(|f| f.path()).collect();
            // Same shared path as every other add: library rows are a data
            // copy, anything unknown is read on the background pass, and a
            // dropped folder still expands to the files under it.
            let added = playlist_add::add_paths(&state_fd, &dropped).count;
            if added > 0 {
                status_fd.set_text(&format!(
                    "Dropped {} file{}",
                    added,
                    if added == 1 { "" } else { "s" }
                ));
                rebuild_fd();
            }
            added > 0
        });
        pl_scroll.add_controller(file_drop);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Transport button callbacks
    // ══════════════════════════════════════════════════════════════════════════

    // ▶ Play / resume.
    btn_play.connect_clicked({
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        move |_| {
            let ps = state.borrow().player.state().clone();
            match ps {
                PlayerState::Stopped => play_and_update(),
                PlayerState::Paused => {
                    let _ = state.borrow_mut().player.toggle_pause();
                }
                PlayerState::Playing => {}
            }
        }
    });

    // ⏸ Pause / resume toggle.
    btn_pause.connect_clicked({
        let state = state.clone();
        move |_| {
            let _ = state.borrow_mut().player.toggle_pause();
        }
    });

    // ⏹ Stop.
    btn_stop.connect_clicked({
        let state = state.clone();
        let seek_bar = seek_bar.clone();
        let patch_pl_row = patch_pl_row.clone();
        move |_| {
            let old_idx = state.borrow().playlist.current_index;
            {
                let mut s = state.borrow_mut();
                let _ = s.player.stop();
                // Manual stop cancels a pending stop-after-current (phase 6).
                s.player.set_stop_after_current(false);
            }
            seek_bar.set_value(0.0);
            // Remove the bold/arrow from the now-stopped track.
            patch_pl_row(old_idx);
        }
    });

    // ⏭ Next track.
    btn_next.connect_clicked({
        let state = state.clone();
        let set_track = set_track.clone();
        let patch_pl_row = patch_pl_row.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let scroll_to_row_if_needed = scroll_to_row_if_needed.clone();
        let current_track_meta_tx = current_track_meta_tx.clone();
        let refresh_now_playing = refresh_now_playing.clone();
        move |_| {
            let old_idx = state.borrow().playlist.current_index;
            let q_before = state.borrow().queue.len();
            if let Some(display) = { state.borrow_mut().play_next() } {
                let new_idx = state.borrow().playlist.current_index;
                set_track(&display);
                scan_current_track_metadata(&state, current_track_meta_tx.clone());
                scroll_to_row_if_needed(new_idx);
                // A queued entry was consumed → renumber every badge.
                if state.borrow().queue.len() != q_before {
                    rebuild_playlist();
                    refresh_queue_manager();
                } else {
                    if old_idx != new_idx {
                        patch_pl_row(old_idx);
                    }
                    patch_pl_row(new_idx);
                }
                refresh_now_playing();
            }
        }
    });

    // ⏮ Previous / restart (PRD back-button logic).
    btn_prev.connect_clicked({
        let state = state.clone();
        let set_track = set_track.clone();
        let patch_pl_row = patch_pl_row.clone();
        let scroll_to_row_if_needed = scroll_to_row_if_needed.clone();
        let current_track_meta_tx = current_track_meta_tx.clone();
        let refresh_now_playing = refresh_now_playing.clone();
        move |_| {
            let old_idx = state.borrow().playlist.current_index;
            if let Some(display) = { state.borrow_mut().play_prev() } {
                let new_idx = state.borrow().playlist.current_index;
                set_track(&display);
                scan_current_track_metadata(&state, current_track_meta_tx.clone());
                scroll_to_row_if_needed(new_idx);
                if old_idx != new_idx {
                    patch_pl_row(old_idx);
                }
                patch_pl_row(new_idx);
                // Explicit so a Prev-restart (≥5s → replays the SAME track) still
                // refreshes; the tick's path-keyed choke point would skip it.
                refresh_now_playing();
            }
        }
    });

    // 🔁 Repeat — cycle Off → Song → Playlist → Off.
    // Updates the button label and tooltip immediately so the user can see
    // the current mode without opening the help window.
    btn_repeat.connect_clicked({
        let state = state.clone();
        let btn_repeat = btn_repeat.clone();
        let repeat_icon = repeat_icon.clone();
        let repeat_label = repeat_label.clone();
        move |_| {
            let new_mode = {
                let mut s = state.borrow_mut();
                let m = s.config.playback.repeat_mode.cycle();
                s.config.playback.repeat_mode = m;
                m
            };
            // Update both icon and text so the button matches macOS visuals.
            repeat_icon.set_icon_name(Some(repeat_btn_icon(new_mode)));
            repeat_label.set_text(repeat_btn_text(new_mode));
            // Highlight with accent class when not off.
            if new_mode == crate::shuffle::RepeatMode::Off {
                btn_repeat.remove_css_class("mode-btn-active");
            } else {
                btn_repeat.add_css_class("mode-btn-active");
            }
        }
    });

    // 🔀 Shuffle — toggle on/off; accent-highlighted when on.
    btn_shuffle.connect_clicked({
        let state = state.clone();
        let btn_shuffle = btn_shuffle.clone();
        move |_| {
            let enabled = {
                let mut s = state.borrow_mut();
                s.shuffle_state.toggle();
                // Reset the shuffle history so the new setting takes effect cleanly.
                s.shuffle_state.reset();
                let on = s.shuffle_state.enabled;
                // Mirror to config so the setting survives to the next session.
                s.config.playback.shuffle_enabled = on;
                on
            };
            if enabled {
                btn_shuffle.add_css_class("mode-btn-active");
            } else {
                btn_shuffle.remove_css_class("mode-btn-active");
            }
        }
    });

    // PL — toggle the playlist window.
    btn_pl.connect_clicked({
        let playlist_win = playlist_win.clone();
        let state = state.clone();
        move |_| {
            if playlist_win.is_visible() {
                let (w, h) = (playlist_win.width(), playlist_win.height());
                {
                    let mut s = state.borrow_mut();
                    s.config.window.playlist_width = w;
                    s.config.window.playlist_height = h;
                }
                let _ = state.borrow().config.save();
            }
            playlist_win.set_visible(!playlist_win.is_visible());
        }
    });

    // ℹ Info button — connected after handle_key is defined (see below).

    // ══════════════════════════════════════════════════════════════════════════
    // Playlist TreeView interactions
    // ══════════════════════════════════════════════════════════════════════════

    // Double-click / Enter on a row: jump to that track and start playback.
    #[allow(deprecated)]
    pl_view.connect_row_activated({
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        let patch_pl_row = patch_pl_row.clone();
        move |_, path, _| {
            if let Some(&idx) = path.indices().first() {
                // Record the previously-playing track before changing current_index
                // so we can de-highlight it after the jump.
                let old_idx = state.borrow().playlist.current_index;
                state.borrow_mut().playlist.jump_to(idx as usize);
                play_and_update();
                if old_idx != idx as usize {
                    patch_pl_row(old_idx);
                }
            }
        }
    });

    // Right-click context menu on a row: Play / View+Edit ID3 / Remove.
    // NOTE: Attached to ScrolledWindow instead of TreeView to work around GTK4 bug
    // where PopoverMenu doesn't receive hover events when attached directly to TreeView.
    {
        let ctx_click = GestureClick::new();
        ctx_click.set_button(3); // right mouse button
        // Capture phase pre-empts GtkTreeView's default Bubble-phase right-
        // click handler, which otherwise clears the multi-selection before
        // our `path_is_selected` guard sees it.  Claimed state at the end
        // prevents the default handler from running afterward.
        ctx_click.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let pl_view_ctx = pl_view.clone();
        let pl_scroll_ctx = pl_scroll.clone();

        // Create action group and attach to the ScrolledWindow (not TreeView)
        let pl_action_group = gio::SimpleActionGroup::new();
        pl_scroll_ctx.insert_action_group("pl", Some(&pl_action_group));

        // Store the current row index for the action handlers
        let current_row: Rc<RefCell<Option<i64>>> = Rc::new(RefCell::new(None));

        // Register playlist menu actions in the action group
        let action_play = gio::SimpleAction::new("play", None); // Short name
        let state_play = state.clone();
        let play_callback = play_and_update.clone();
        let patch_callback = patch_pl_row.clone();
        let scroll_callback = scroll_to_row_if_needed.clone();
        let row_play = current_row.clone();
        action_play.connect_activate(move |_, _| {
            let row_idx = *row_play.borrow();
            if let Some(idx) = row_idx {
                let idx = idx as usize;
                let old_idx = state_play.borrow().playlist.current_index;
                state_play.borrow_mut().playlist.jump_to(idx);
                play_callback();
                scroll_callback(idx);
                if old_idx != idx {
                    patch_callback(old_idx);
                }
            }
        });
        pl_action_group.add_action(&action_play);

        let action_id3 = gio::SimpleAction::new("edit-id3", None); // Short name
        let state_id3 = state.clone();
        let win_id3 = window.downgrade();
        let rebuild_id3 = rebuild_playlist.clone();
        let row_id3 = current_row.clone();
        action_id3.connect_activate(move |_, _| {
            let row_idx = *row_id3.borrow();
            if let Some(idx) = row_idx {
                let path = state_id3
                    .borrow()
                    .playlist
                    .tracks
                    .get(idx as usize)
                    .map(|t| t.path.clone());
                if let Some(path) = path {
                    open_id3_editor_window(
                        win_id3.upgrade().as_ref(),
                        path,
                        state_id3.clone(),
                        rebuild_id3.clone(),
                        None,
                        None,
                    );
                }
            }
        });
        pl_action_group.add_action(&action_id3);

        // View/Search Lyrics (F15): saved USLT opens the viewer, else browser search.
        let action_lyrics = gio::SimpleAction::new("lyrics", None);
        let state_lyr = state.clone();
        let rebuild_lyr = rebuild_playlist.clone();
        let row_lyr = current_row.clone();
        action_lyrics.connect_activate(move |_, _| {
            let row_idx = *row_lyr.borrow();
            if let Some(idx) = row_idx {
                let t = state_lyr
                    .borrow()
                    .playlist
                    .tracks
                    .get(idx as usize)
                    .map(|t| (t.path.clone(), t.artist.clone(), t.title.clone(), t.album_artist.clone()));
                if let Some((path, artist, title, album_artist)) = t {
                    view_or_search_lyrics(
                        &state_lyr, &path, &artist, &title, &album_artist, rebuild_lyr.clone(),
                        LyricsMode::Specific,
                    );
                }
            }
        });
        pl_action_group.add_action(&action_lyrics);

        // View Album Art for the right-clicked row (single-selection item).
        let action_view_art = gio::SimpleAction::new("view-art", None);
        let state_art = state.clone();
        let row_art = current_row.clone();
        action_view_art.connect_activate(move |_, _| {
            let Some(idx) = *row_art.borrow() else { return };
            let path = state_art
                .borrow()
                .playlist
                .tracks
                .get(idx as usize)
                .map(|t| t.path.clone());
            if let Some(path) = path {
                art_window::open_track_art(&state_art, &path);
            }
        });
        pl_action_group.add_action(&action_view_art);

        let action_remove = gio::SimpleAction::new("remove", None); // Short name
        let remove_callback = remove_selected.clone();
        action_remove.connect_activate(move |_, _| {
            remove_callback();
        });
        pl_action_group.add_action(&action_remove);

        // Queue / Dequeue the selected rows (manual play queue). Toggles each
        // selected entry's queue membership, then rebuilds so the [n] badges
        // update. Also bound to `q` on the playlist window (capture phase).
        let action_queue_toggle = gio::SimpleAction::new("queue-toggle", None);
        {
            let toggle = queue_toggle_selection.clone();
            action_queue_toggle.connect_activate(move |_, _| toggle());
        }
        pl_action_group.add_action(&action_queue_toggle);

        // Seed a brand new saved playlist from the current selection.
        // Opens the standard playlist save dialog so the user picks the
        // filename + folder; the resulting M3U8 contains EXTINF metadata
        // for every selected active-playlist row.
        let action_add_to_new = gio::SimpleAction::new("add-to-new", None);
        {
            let state_atn  = state.clone();
            let pl_view_atn = pl_view.clone();
            let win_atn    = playlist_win.clone();
            action_add_to_new.connect_activate(move |_, _| {
                #[allow(deprecated)]
                let (sel_paths, _) = pl_view_atn.selection().selected_rows();
                let indices: Vec<usize> = sel_paths.iter()
                    .filter_map(|p| p.indices().first().copied())
                    .map(|i| i as usize)
                    .collect();
                let paths: Vec<String> = {
                    let s = state_atn.borrow();
                    indices.iter()
                        .filter_map(|i| s.playlist.tracks.get(*i))
                        .map(|t| t.path.to_string_lossy().into_owned())
                        .collect()
                };
                if paths.is_empty() { return }
                let default_stem = glib::DateTime::now_local()
                    .ok()
                    .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Playlist".to_string());
                let state_cb = state_atn.clone();
                run_playlist_save_dialog(
                    state_atn.clone(),
                    win_atn.clone(),
                    &default_stem,
                    move |path, win_cb| {
                        if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                            if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths) {
                                eprintln!("save_playlist_tracks_to_path: {e}");
                                show_playlist_save_error(&win_cb, &path, &e);
                            }
                        }
                        notify_playlist_nav_refresh();
                    },
                );
            });
        }
        pl_action_group.add_action(&action_add_to_new);

        // Add selection to a saved playlist (parameterised by id).
        // Multi-select aware: pulls every selected row from the active
        // playlist and appends their paths to the chosen saved playlist.
        let state_add_pl = state.clone();
        let pl_view_add  = pl_view.clone();
        let action_add_to_saved = gio::SimpleAction::new(
            "add-to-saved",
            Some(glib::VariantTy::INT64),
        );
        action_add_to_saved.connect_activate(move |_, param| {
            let Some(pid) = param.and_then(|p| p.get::<i64>()) else { return };
            #[allow(deprecated)]
            let (paths_models, _model) = pl_view_add.selection().selected_rows();
            let indices: Vec<i64> = paths_models
                .iter()
                .filter_map(|p| p.indices().first().copied())
                .map(|i| i as i64)
                .collect();
            let paths: Vec<String> = {
                let s = state_add_pl.borrow();
                indices.iter()
                    .filter_map(|i| s.playlist.tracks.get(*i as usize))
                    .map(|t| t.path.to_string_lossy().into_owned())
                    .collect()
            };
            if paths.is_empty() { return }
            let mut ok = false;
            if let Some(lib) = state_add_pl.borrow().media_lib.as_ref() {
                match lib.append_paths_to_playlist(pid, &paths) {
                    Ok(_)  => ok = true,
                    Err(e) => eprintln!("append_paths_to_playlist {pid}: {e}"),
                }
            }
            if ok { notify_playlist_changed(pid); }
        });
        pl_action_group.add_action(&action_add_to_saved);

        // Send to Disc Drive: probe-on-add, then queue onto THAT drive.
        // Same body as the Media Library's ml.send-drive (media_library.rs)
        // — shares burn_queues so the burn panel sees items queued here.
        {
            let state_burn = state.clone();
            let pl_view_drv = pl_view.clone();
            let burn_queues = burn_queues.clone();
            let burn_refresh_holder = burn_refresh_holder.clone();
            let current_drives = current_drives.clone();
            let status = pl_status_label.clone();
            let win_wk: glib::WeakRef<gtk4::Window> =
                playlist_win.clone().upcast::<gtk4::Window>().downgrade();
            let action = gio::SimpleAction::new(
                "send-drive",
                Some(glib::VariantTy::STRING),
            );
            action.connect_activate(move |_, target| {
                let Some(drive_id) =
                    target.and_then(|v| v.get::<String>()) else { return };
                let drive_label = current_drives
                    .borrow()
                    .iter()
                    .find(|d| d.id == drive_id)
                    .map(|d| d.label.clone())
                    .unwrap_or_else(|| drive_id.clone());
                #[allow(deprecated)]
                let (sel_paths, _) = pl_view_drv.selection().selected_rows();
                let indices: Vec<usize> = sel_paths
                    .iter()
                    .filter_map(|p| p.indices().first().copied())
                    .map(|i| i as usize)
                    .collect();
                let paths: Vec<std::path::PathBuf> = {
                    let s = state_burn.borrow();
                    indices.iter()
                        .filter_map(|i| s.playlist.tracks.get(*i))
                        .map(|t| t.path.clone())
                        .collect()
                };
                // Metadata from the library NOW (SQLite is not Send).
                let metas: std::collections::HashMap<_, _> = {
                    let s = state_burn.borrow();
                    paths.iter().map(|path| {
                        let row = s.media_lib.as_ref().and_then(|l| {
                            l.track_by_path(&path.display().to_string()).ok()
                        });
                        let display = row.as_ref()
                            .map(|t| match (&t.artist, &t.title) {
                                (Some(a), Some(ti)) if !a.is_empty() =>
                                    format!("{a} - {ti}"),
                                (_, Some(ti)) => ti.clone(),
                                _ => t.filename.clone(),
                            })
                            .unwrap_or_else(|| path.file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.display().to_string()));
                        let secs = row.as_ref()
                            .and_then(|t| t.length_secs).map(|s| s as u32);
                        let bytes = std::fs::metadata(path)
                            .map(|m| m.len()).unwrap_or(0);
                        (path.clone(), (display, secs, bytes))
                    }).collect()
                };
                let status = status.clone();
                queue_paths_to_drive(
                    drive_id,
                    drive_label,
                    paths,
                    metas,
                    burn_queues.clone(),
                    burn_refresh_holder.clone(),
                    Rc::new(move |s: String| status.set_text(&gtk_safe(&s))),
                    win_wk.clone(),
                );
            });
            pl_action_group.add_action(&action);
        }

        // Send to Removable Device: the Media Library's copy runner when it is
        // available, and a self-contained copy when it is not.
        //
        // The fallback copies straight onto the device using the same core
        // plan/copy helpers as the ML's copy_files_run (device_plan_fs /
        // device_recorded_relpath / device_record_pair / devices::io::
        // for_device), but without any of that runner's ML-only widgets
        // (sidebar row, progress bar, eject button — copy_files_run captures
        // those directly and they don't exist until the ML window has been
        // built). That coupling is why this path once went through
        // copy_files_holder *unconditionally* and silently did nothing when
        // the holder was still empty: the device shows up in this menu via the
        // app-start device poll, independent of the ML window.
        //
        // Going holder-first-with-a-fallback keeps that fixed while restoring
        // the progress UI when the ML window is open, which is what a user
        // dragging the same files onto the same device already sees
        // (2026-08-10). Fallback progress goes to the playlist status label
        // and the result to an alert on the playlist window, mirroring
        // pl.send-drive.
        {
            let current_devices = current_devices.clone();
            let pl_view_dev = pl_view.clone();
            let state_dev = state.clone();
            let status = pl_status_label.clone();
            let win_wk = playlist_win.downgrade();
            let copy_files_holder = copy_files_holder.clone();
            let action = gio::SimpleAction::new(
                "send-device",
                Some(glib::VariantTy::STRING),
            );
            action.connect_activate(move |_, target| {
                let Some(dev_id) =
                    target.and_then(|v| v.get::<String>()) else { return };
                let Some(dev) = current_devices
                    .borrow()
                    .iter()
                    .find(|d| d.id == dev_id)
                    .cloned()
                else { return };
                #[allow(deprecated)]
                let (sel_paths, _) = pl_view_dev.selection().selected_rows();
                let indices: Vec<usize> = sel_paths
                    .iter()
                    .filter_map(|p| p.indices().first().copied())
                    .map(|i| i as usize)
                    .collect();
                let paths: Vec<std::path::PathBuf> = {
                    let s = state_dev.borrow();
                    indices.iter()
                        .filter_map(|i| s.playlist.tracks.get(*i))
                        .map(|t| t.path.clone())
                        .collect()
                };
                let paths: Vec<std::path::PathBuf> =
                    paths.into_iter().filter(|p| p.exists()).collect();
                if paths.is_empty() {
                    return;
                }
                // Prefer the Media Library's copy runner when it exists, so
                // this reports progress exactly where a drag-and-drop onto the
                // same device does: the device's sidebar row and the detail
                // view's bar. The standalone path below is the fallback for
                // when the ML window has never been opened and the holder is
                // still empty — see the comment above this block for why that
                // fallback has to exist at all.
                let ml_runner = copy_files_holder.borrow().clone();
                if let Some(run) = ml_runner {
                    run(dev, paths);
                    return;
                }
                let win_for_alert = || win_wk.upgrade().map(|w| w.upcast::<gtk4::Window>());
                if dev.read_only {
                    let n = if dev.label.is_empty() { "This device" } else { &dev.label };
                    show_alert_parented(
                        win_for_alert().as_ref(),
                        &format!("{n} is read-only — can't copy files to it."),
                    );
                    return;
                }
                if device_fs_unsupported(&dev.fs_type) {
                    show_alert_parented(
                        win_for_alert().as_ref(),
                        &format!(
                            "{} is an unsupported filesystem — can't write to this device yet.",
                            dev.fs_type
                        ),
                    );
                    return;
                }
                let device_id = device_sync_id(&dev);
                let mount = dev.mount_path.clone();
                // Free-space guard — only when capacity is known (mirrors
                // copy_files_run: skips slow per-file device checks on
                // devices that can't report capacity, e.g. MTP).
                if dev.free_bytes > 0 {
                    let mut need = 0u64;
                    for p in &paths {
                        if !device_plan_one(&state_dev, &mount, &device_id, p).1 {
                            need += std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                        }
                    }
                    if need > dev.free_bytes {
                        show_alert_parented(
                            win_for_alert().as_ref(),
                            &format!(
                                "Not enough space on the device: need {:.1} GB, {:.1} GB free.",
                                need as f64 / 1e9,
                                dev.free_bytes as f64 / 1e9
                            ),
                        );
                        return;
                    }
                }
                let dname = if dev.label.is_empty() { "device".to_string() } else { dev.label.clone() };
                let total = paths.len();
                let dev_for_copy = dev.clone();
                let state2 = state_dev.clone();
                let status2 = status.clone();
                let win2 = win_wk.clone();
                status.set_text("Reading files…");
                // Same spawn_future_local + spawn_blocking pattern as
                // pl.send-drive above: only Send data (paths, dev, ids)
                // crosses into spawn_blocking; the Rcs stay in the local
                // future, and the state borrow inside each step is short-lived.
                glib::spawn_future_local(async move {
                    let (mut copied, mut skipped, mut failed) = (0usize, 0usize, 0usize);
                    for (i, src) in paths.iter().enumerate() {
                        status2.set_text(&gtk_safe(&format!(
                            "Copying {}/{total} to {dname}…", i + 1
                        )));
                        let recorded = device_recorded_relpath(&state2, &device_id, src);
                        let s = src.clone();
                        let m = mount.clone();
                        let dc = dev_for_copy.clone();
                        let joined = gio::spawn_blocking(
                            move || -> Result<(std::path::PathBuf, bool), ()> {
                                let (rel, present) = device_plan_fs(&m, &s, recorded);
                                if present {
                                    return Ok((rel, false)); // already there
                                }
                                match crate::devices::io::for_device(&dc)
                                    .copy_to_device(&s, &rel)
                                {
                                    Ok(_) => Ok((rel, true)),
                                    Err(_) => Err(()),
                                }
                            },
                        )
                        .await;
                        match joined {
                            Ok(Ok((rel, copied_now))) => {
                                if copied_now {
                                    copied += 1;
                                } else {
                                    skipped += 1;
                                }
                                device_record_pair(&state2, &device_id, src, &rel);
                            }
                            _ => failed += 1,
                        }
                    }
                    status2.set_text(&gtk_safe(&format!(
                        "Copied {copied}, skipped {skipped}, failed {failed} to {dname}."
                    )));
                    show_alert_parented(
                        win2.upgrade().map(|w| w.upcast::<gtk4::Window>()).as_ref(),
                        &gtk_safe(&format!(
                            "Copied {copied}, skipped {skipped}, failed {failed} to {dname}."
                        )),
                    );
                });
            });
            pl_action_group.add_action(&action);
        }

        let state_menu_pl = state.clone();
        let current_drives_ctx = current_drives.clone();
        let current_devices_ctx = current_devices.clone();
        let pl_drag_sel_ctx = pl_drag_selection.clone();
        ctx_click.connect_pressed(move |gest, _, x, y| {
            #[allow(deprecated)]
            let row_idx = match pl_view_ctx.path_at_pos(x as i32, y as i32) {
                Some((Some(path), _, _, _)) => match path.indices().first().copied() {
                    Some(i) => i as i64,
                    None => return,
                },
                _ => return,
            };

            // Store the row index for the action handlers
            *current_row.borrow_mut() = Some(row_idx);

            // Restore the press-time multi-selection snapshot when the
            // clicked row was part of a prior multi-select.  GtkTreeView
            // (deprecated) collapses selection to the clicked row on
            // secondary-button press even when our Capture-phase
            // handler claims the event, so we explicitly re-select the
            // snapshot rows here.
            let snapshot = pl_drag_sel_ctx.borrow().clone();
            let row_idx_u = row_idx as usize;
            let should_restore = snapshot.len() > 1 && snapshot.contains(&row_idx_u);

            #[allow(deprecated)]
            let path = gtk4::TreePath::from_indices(&[row_idx as i32]);
            #[allow(deprecated)]
            let already_selected = pl_view_ctx.selection().path_is_selected(&path);
            if should_restore {
                #[allow(deprecated)]
                pl_view_ctx.selection().unselect_all();
                let model = pl_view_ctx.model();
                #[allow(deprecated)]
                if let Some(model) = model {
                    for idx in &snapshot {
                        if let Some(iter) = model.iter_nth_child(None, *idx as i32) {
                            #[allow(deprecated)]
                            pl_view_ctx.selection().select_iter(&iter);
                        }
                    }
                }
            } else if !already_selected {
                #[allow(deprecated)]
                pl_view_ctx.selection().unselect_all();
                #[allow(deprecated)]
                if let Some(iter) = pl_view_ctx
                    .model()
                    .and_then(|m| m.iter_nth_child(None, row_idx as i32))
                {
                    #[allow(deprecated)]
                    pl_view_ctx.selection().select_iter(&iter);
                }
            }

            // Number of currently-selected rows — drives single-only menu
            // items (Edit ID3 is hidden in multi-select since the editor
            // can only bind to one track at a time).
            #[allow(deprecated)]
            let sel_count = pl_view_ctx.selection().count_selected_rows() as usize;

            // Build menu model with prefixed action names. Item order matches
            // the macOS row menu (PlaylistView.buildContextMenu) so the two
            // frontends read the same top to bottom.
            let send = build_send_to_menu(
                &state_menu_pl,
                &SendToActions {
                    active: "", // tracks are already in the active playlist
                    new_playlist: "pl.add-to-new",
                    saved_playlist: "pl.add-to-saved",
                    drive: "pl.send-drive",
                    device: "pl.send-device",
                    drives: current_drives_ctx.borrow().iter()
                        .map(|d| (d.id.clone(), d.label.clone())).collect(),
                    devices: current_devices_ctx.borrow().iter()
                        .map(|d| (d.id.clone(), d.label.clone())).collect(),
                },
            );
            // Order: Play · Enqueue/Dequeue · Send to · ID3 · Album Art ·
            // Lyrics · Remove. Matches the macOS row menu
            // (PlaylistView.buildContextMenu). Flat (no sections) — GtkPopover-
            // Menu doesn't allocate height for section separators when the
            // popover's parent has no LayoutManager, so a sectioned menu came
            // up too short and scrolled; Remove stays last regardless.
            let menu = gio::Menu::new();
            menu.append_item(&gio::MenuItem::new(Some("▶ Play"), Some("pl.play")));
            menu.append_item(&gio::MenuItem::new(
                Some("⯈ Enqueue / Dequeue"),
                Some("pl.queue-toggle"),
            ));
            menu.append_submenu(Some("↪ Send to"), &send);
            if sel_count <= 1 {
                menu.append_item(&gio::MenuItem::new(
                    Some("🎵 View/Edit ID3"),
                    Some("pl.edit-id3"),
                ));
                menu.append_item(&gio::MenuItem::new(
                    Some("🖼 View Album Art"),
                    Some("pl.view-art"),
                ));
                menu.append_item(&gio::MenuItem::new(
                    Some("📝 View/Search Lyrics"),
                    Some("pl.lyrics"),
                ));
            }
            menu.append_item(&gio::MenuItem::new(
                Some("✕ Remove"),
                Some("pl.remove"),
            ));

            // Create popover menu — NESTED keeps the Add-to-Playlist
            // submenu from being clipped to the parent menu's height.
            //
            // Parent the popover to `pl_view` — the SAME widget this gesture is
            // on — so the pointing rect below is in the parent's coordinate
            // space. Parenting to the enclosing ScrolledWindow instead put the
            // (x, y) from `pl_view` into the scroller's space; once the list was
            // scrolled the anchor landed outside the viewport and GTK gave the
            // menu scroll arrows to keep it on screen. Action dispatch still
            // resolves: the "pl" group lives on `pl_scroll_ctx`, pl_view's
            // parent, so the popover's action walk reaches it either way.
            let popover = context_popover(&menu);
            #[allow(deprecated)]
            popover.set_parent(&pl_view_ctx);
            let rect = gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            popover.set_pointing_to(Some(&rect));
            popover.popup();
            gest.set_state(gtk4::EventSequenceState::Claimed);
        });
        #[allow(deprecated)]
        pl_view.add_controller(ctx_click);
    }

    // Selection change: show a status hint when a broken track is selected.
    #[allow(deprecated)]
    pl_view.selection().connect_changed({
        let state     = state.clone();
        let pl_status = pl_status_label.clone();
        let pl_view_sc = pl_view.clone();
        let refresh_pl_status = refresh_pl_status.clone();
        move |_| {
            // set_model(None) during a bulk rebuild fires this signal with a null
            // model; selected_rows() would then panic.  Bail early if no model.
            #[allow(deprecated)]
            if pl_view_sc.model().is_none() { return; }
            #[allow(deprecated)]
            let (paths, _) = pl_view_sc.selection().selected_rows();
            let Some(path) = paths.first() else {
                refresh_pl_status();
                return;
            };
            let Some(&idx) = path.indices().first() else {
                refresh_pl_status();
                return;
            };
            let idx = idx as usize;
            let is_broken = state.borrow().playlist.tracks
                .get(idx)
                .map(|t| t.broken)
                .unwrap_or(false);
            if is_broken {
                let path_hint = state.borrow().playlist.tracks
                    .get(idx)
                    .map(|t| t.path.display().to_string())
                    .unwrap_or_default();
                pl_status.set_text(&format!(
                    "⚠  This file can't be played — it may have been moved, renamed, or deleted.  ({})",
                    path_hint
                ));
            } else {
                refresh_pl_status();
            }
        }
    });
}
