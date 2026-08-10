//! The Media Library's "Playlists" page — saved playlists and their editor.
//!
//! Child module of [`super`] (window.rs), extracted from
//! `open_media_library_window` by plan step 7. Two sub-pages live inside one
//! stack page:
//!
//! - **`pl-manage`** — the full-width list of saved playlists, with New,
//!   Rename and Delete.
//! - **`pl-edit`** — the track editor for whichever playlist is open: its
//!   table, add/remove/reorder, Revert, Save and Save As, the row context
//!   menu and the "Send to ▾" menu.
//!
//! ## Why this one lifted cleanly
//!
//! Unlike Discs (step 5) and Devices (step 6), this page was already
//! contiguous — no hoist commit, because its widgets and its wiring had never
//! been separated by another page's code. Measured before the move, it
//! declared **nothing** the rest of the window read back.
//!
//! The one thing in the way was shared rather than owned: a single
//! `connect_row_selected` handler routed Files, Albums *and* Playlists and
//! happened to sit here. That was split out to the pages it belongs to first,
//! in its own commit, so this move stayed a pure move.
//!
//! ## Deletion Rule
//!
//! Removing a row from a playlist removes it from *that list only* — the file
//! stays on disk and in the library. Deleting a playlist deletes its `.m3u8`
//! and nothing else: its tracks survive in the library. Neither path may ever
//! delete audio (see CLAUDE.md).

use gtk4::prelude::*;
use gtk4::{
    gdk, gio, glib, Align, Box as GtkBox, Button, DropTarget, Entry, EventControllerKey, Label,
    ListBox, ListBoxRow, Orientation, PolicyType, ScrolledWindow, Stack, StackTransitionType,
};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use super::sidebar::{self, Sidebar};
use super::{playlists_columns, playlists_manage, playlists_menu};
// Everything else is private to the parent module, which a child may still
// use: the shared column model, the Send-to menu and its targets, the
// playlist-save dialogs, and the cross-window refresh hooks the editor
// publishes itself through.
use super::{
    attach_pl_row_drag, build_send_to_menu, editor_cell_positions, gtk_safe,
    lib_track_matches_query, make_view_search_row, ml_status_bar_for, queue_paths_to_drive,
    run_playlist_save_dialog, show_playlist_save_error, sidebar_pl_end_index,
    view_or_search_lyrics, LyricsMode, MlCtx, SendToActions,
};

/// Wrapper put into the editor's `ListStore`. Carrying `canonical_idx`
/// alongside the track lets every cell — even duplicates of the same file in
/// the playlist — bind to its own play-order slot, so the position column
/// reads the correct row instead of all duplicates collapsing onto the last
/// occurrence's index. Cloned cheaply on splice because `LibTrack` is `Clone`
/// already.
///
/// At module scope rather than inside `build` because the row context menu in
/// [`super::playlists_menu`] reads the same wrapper back out of the store.
#[derive(Clone)]
pub(super) struct EditorEntry {
    pub track: crate::media_library::LibTrack,
    pub canonical_idx: usize,
}

/// Build the Playlists page and attach it to `ctx.stack` under the name
/// `"playlists"`.
///
/// Takes `sb` as well as `ctx` for the three sidebar cells this page alone
/// touches — its playlist sub-rows, the Playlists chevron state, and the
/// send-a-playlist holder the device page fills. By the plan's §3.2 test
/// those belong here rather than on [`MlCtx`].
pub(super) fn build(ctx: &MlCtx, sb: &Sidebar) {
    // Local names for what this page takes from its context, so the body below
    // reads exactly as it did inside `open_media_library_window`. Same device
    // steps 1–6 used: cloning an `Rc` is an integer increment, and rewriting
    // several hundred capture sites would bury a move in an unreviewable diff.
    let state = ctx.host.state.clone();
    let rebuild_playlist = ctx.host.rebuild_playlist.clone();
    let set_track = ctx.host.set_track.clone();
    let current_drives = ctx.host.current_drives.clone();
    let current_devices = ctx.host.current_devices.clone();
    let burn_queues = ctx.host.burn_queues.clone();
    let copy_files_holder = ctx.host.copy_files_holder.clone();
    let burn_refresh_holder = ctx.host.burn_refresh_holder.clone();
    let win = ctx.win.clone();
    let stack = ctx.stack.clone();
    let sidebar = sb.list.clone();
    let pl_sub_rows = sb.pl_sub_rows.clone();
    let playlists_expanded = sb.playlists_expanded.clone();
    let send_playlist_holder = sb.send_playlist_holder.clone();

    let pl_sub_stack: Rc<Stack> = Rc::new({
        let s = Stack::new();
        s.set_hexpand(true);
        s.set_vexpand(true);
        s.set_transition_type(StackTransitionType::None);
        s
    });

    // Shared: currently-editing playlist id and LibTrack list
    let editing_tracks: Rc<RefCell<Vec<crate::media_library::LibTrack>>> =
        Rc::new(RefCell::new(Vec::new()));
    let saved_track_ids: Rc<RefCell<Vec<i64>>> = Rc::new(RefCell::new(Vec::new()));
    // The DB row id of the playlist currently open in the editor (-1 = none)
    let editing_pl_id: Rc<Cell<i64>> = Rc::new(Cell::new(-1));

    // "Send to" and row-scoped actions for the editor's per-cell context
    // menu. Task 8 originally built this as a flat Popover of plain Buttons
    // because a PopoverMenu parented on the ColumnView cell tree lost
    // dispatch. The Files-view menu (~col_view, "ml" prefix) proves the
    // real fix: put the SimpleActionGroup on the SAME stable widget the
    // PopoverMenu is parented to (single widget, no ancestor walk) instead
    // of scattering it across track_list/win as the abandoned "ple" group
    // above did. Here that stable widget is the editor's ScrolledWindow
    // (`track_scroll`, exposed via `track_scroll_holder` since it doesn't
    // exist yet at this point in the function) — see its
    // `insert_action_group("ed", ...)` call right after it's built. The
    // group is *also* inserted directly on each popped-up PopoverMenu
    // instance (see the per-cell gesture) as defense in depth: the
    // `ple_action_group_holder` comment above documents a GTK4 version
    // where the NESTED PopoverMenu flag breaks the ancestor-chain walk
    // entirely, and installing the group straight on the popup sidesteps
    // that regardless of GTK version.
    // Canonical play-order indices (selection, or the single clicked row
    // as fallback) captured once per right-click so every "ed.*" action —
    // not just send-drive/send-device — can read row-scoped context
    // without needing per-item closures. Still used by the row-scoped
    // "Replace Current Playlist" / "Edit ID3" / "Remove" items, which are
    // right-click-only (never exposed on the "Send to ▾" button below).
    let ed_ctx_indices: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    // Live "currently selected editor rows" reader for the actions that
    // ARE exposed on the "Send to ▾" button (send-active, add-to-new,
    // add-to-saved, send-drive, send-device): the button doesn't go
    // through the per-cell right-click gesture, so it must read the
    // editor's own MultiSelection directly at dispatch time instead of a
    // stash the gesture populates (same G1 fix as the files view's "Send
    // to ▾" button). `edit_multi_sel` doesn't exist yet at this point in
    // the function, so a holder defers the actual model until it's built
    // below (filled in right after `edit_multi_sel` is constructed).
    let edit_multi_sel_holder: Rc<RefCell<Option<gtk4::MultiSelection>>> =
        Rc::new(RefCell::new(None));
    //
    // **Nothing selected means the whole playlist.** That is what the editor's
    // other whole-list controls already do (▶ Play and Enqueue both ignore the
    // selection and act on `editing_tracks`), and it is what a user reaching
    // for "Send to ▾" with no rows highlighted means. Before 2026-08-10 every
    // one of those five actions returned an empty Vec and bailed without a
    // word, so the button looked dead — the failure mode this file's own
    // §"holder" comments warn about, arrived at from the other direction.
    //
    // The fallback cannot leak into the per-row right-click menu: that
    // gesture selects the clicked row before it pops the menu (see
    // `g_sel.select_item` below), so the selection is never empty when the
    // menu dispatches. Only the button can reach the fallback.
    let ed_selected_tracks: Rc<dyn Fn() -> Vec<crate::media_library::LibTrack>> = {
        let sel_holder = edit_multi_sel_holder.clone();
        let all_tracks = editing_tracks.clone();
        Rc::new(move || {
            let Some(sel) = sel_holder.borrow().clone() else { return Vec::new() };
            let mut out = Vec::new();
            for i in 0..sel.n_items() {
                if sel.is_selected(i) {
                    if let Some(obj) = sel
                        .item(i)
                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                    {
                        out.push(obj.borrow::<EditorEntry>().track.clone());
                    }
                }
            }
            if out.is_empty() {
                return all_tracks.borrow().clone();
            }
            out
        })
    };
    // Quiet status line (G3) — the editor's Send to Disc Drive reports
    // here instead of dropping the success message on the floor.
    let ed_status = Label::builder()
        .label("")
        .halign(Align::Start)
        .margin_start(6)
        .margin_end(6)
        .margin_bottom(2)
        .build();
    ed_status.add_css_class("status-label");
    let ed_action_group = gio::SimpleActionGroup::new();
    // Kept as named bindings (not scoped to a block) so later code in this
    // function (the "ed"-group insertion on track_scroll, and the
    // additional actions registered further down once
    // rebuild_track_list_holder/track_scroll_holder exist) can add more
    // actions to the same group.
    let ed_action_drive = gio::SimpleAction::new("send-drive", Some(glib::VariantTy::STRING));
    let ed_action_device = gio::SimpleAction::new("send-device", Some(glib::VariantTy::STRING));
    {
        let state_burn = state.clone();
        let burn_queues = burn_queues.clone();
        let burn_refresh_holder = burn_refresh_holder.clone();
        let current_drives = current_drives.clone();
        let sel_tracks = ed_selected_tracks.clone();
        let win_wk = win.downgrade();
        let status = ed_status.clone();
        ed_action_drive.connect_activate(move |_, target| {
            let Some(drive_id) = target.and_then(|v| v.get::<String>()) else { return };
            let drive_label = current_drives
                .borrow()
                .iter()
                .find(|d| d.id == drive_id)
                .map(|d| d.label.clone())
                .unwrap_or_else(|| drive_id.clone());
            // Live selection at dispatch (G1) — read straight from the
            // editor's MultiSelection, not a right-click stash, so the
            // "Send to ▾" button sees the actual current selection.
            let paths: Vec<std::path::PathBuf> = sel_tracks()
                .iter().map(|t| std::path::PathBuf::from(&t.path)).collect();
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
        ed_action_group.add_action(&ed_action_drive);
    }
    {
        // Send to Removable Device. Two runners, chosen by whether the user
        // picked rows:
        //
        // - **Some rows selected** → the Files view's copy runner via
        //   `copy_files_holder` (populated once the Device view's widgets
        //   exist — see that holder's own doc comment). Files only; a subset
        //   of a playlist is not a playlist, so no .m3u8 is written.
        // - **Nothing selected** → the whole playlist, which means the
        //   *playlist*, not just its audio: `send_playlist_holder` copies the
        //   files AND writes the device .m3u8 and the sync baseline that the
        //   device view's playlist chips and two-way sync are built on.
        //
        // Until 2026-08-10 the second case did nothing at all, and reaching
        // the .m3u8 path took a separate "Entire playlist to device" submenu
        // that duplicated this item for anyone who had not noticed the
        // difference. That submenu is gone; this is the one way in.
        let current_devices = current_devices.clone();
        let copy_files_holder = copy_files_holder.clone();
        let send_playlist_holder = send_playlist_holder.clone();
        let sel_holder = edit_multi_sel_holder.clone();
        let sel_tracks = ed_selected_tracks.clone();
        let ep_id = editing_pl_id.clone();
        let state_dev = state.clone();
        ed_action_device.connect_activate(move |_, target| {
            let Some(dev_id) = target.and_then(|v| v.get::<String>()) else { return };
            let Some(dev) = current_devices
                .borrow()
                .iter()
                .find(|d| d.id == dev_id)
                .cloned()
            else { return };
            // Live selection at dispatch (G1). Read the model rather than
            // `sel_tracks()`, whose whole-playlist fallback is exactly the
            // case being distinguished here.
            let has_selection = sel_holder
                .borrow()
                .as_ref()
                .map(|sel| (0..sel.n_items()).any(|i| sel.is_selected(i)))
                .unwrap_or(false);
            if !has_selection {
                let id = ep_id.get();
                if id < 0 {
                    return;
                }
                let name = state_dev
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|l| l.playlist_by_id(id).ok())
                    .map(|p| p.name)
                    .unwrap_or_default();
                if let Some(send) = send_playlist_holder.borrow().clone() {
                    send(dev, id, name);
                }
                return;
            }
            let paths: Vec<std::path::PathBuf> = sel_tracks()
                .iter().map(|t| std::path::PathBuf::from(&t.path)).collect();
            if !paths.is_empty() {
                if let Some(run) = copy_files_holder.borrow().clone() {
                    run(dev, paths);
                }
            }
        });
        ed_action_group.add_action(&ed_action_device);
    }
    win.insert_action_group("ed", Some(&ed_action_group));

    // Widget handles for pl-manage playlist list (shared with sidebar)
    let pl_manage_list: Rc<ListBox> = Rc::new({
        let lb = ListBox::new();
        lb.add_css_class("playlist");
        lb.set_selection_mode(gtk4::SelectionMode::Single);
        lb.set_vexpand(true);
        lb
    });

    // Canonical play-order index of the row most recently right-clicked
    // in the editor; the ple.edit-id3 / ple.remove actions read this when
    // they need a single row to operate on.  Used instead of LibTrack.id
    // so duplicate entries (same track listed several times in the
    // playlist file) can be disambiguated by position.
    let ctx_canonical_idx: Rc<Cell<i64>> = Rc::new(Cell::new(-1));

    // Canonical play-order indices selected for an in-progress drag from
    // the editor.  Populated by the per-cell DragSource at prepare time
    // and consumed by the editor DropTarget when handling a reorder.
    // Cleared on every new drag prepare so a previous drag's selection
    // can't leak into a subsequent unrelated drop.
    let drag_selection: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    // Path → first canonical slot.  Used by the editor DropTarget when a
    // cross-window drop ships only paths (no canonical indices) and we
    // need to know whether every dropped path is already in the playlist.
    // For duplicates only the first slot is recorded; the drag_selection
    // path is preferred when the drag originated in the editor itself.
    let position_map: Rc<RefCell<std::collections::HashMap<String, usize>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));

    // True when the editor's current display sort allows intra-list drag
    // reorder (only the canonical play-order ascending state preserves the
    // bijection between display index and play-order index).  Flipped by
    // a sorter-change handler installed once the ColumnView exists.
    let reorder_allowed: Rc<Cell<bool>> = Rc::new(Cell::new(true));

    // Track editor: ListStore → SortListModel → MultiSelection → ColumnView.
    // Sort lives in the SortListModel so the user's column-header clicks
    // produce a display-only sort.  `editing_tracks` (the canonical play
    // order) is never reordered by sort — Save always writes that order.
    let edit_store: gio::ListStore = gio::ListStore::new::<glib::BoxedAnyObject>();
    // Per-view search over this playlist's rows: store → filter → sort →
    // selection. Rows keep their canonical_idx, so delete/context actions
    // stay correct under a filter; drag-reorder is refused while one is
    // active (display order no longer maps onto play order).
    let pl_edit_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let edit_filter = gtk4::CustomFilter::new({
        let q = pl_edit_query.clone();
        move |obj| {
            let Some(boxed) = obj.downcast_ref::<glib::BoxedAnyObject>() else {
                return true;
            };
            lib_track_matches_query(&boxed.borrow::<EditorEntry>().track, &q.borrow())
        }
    });
    let edit_filter_model =
        gtk4::FilterListModel::new(Some(edit_store.clone()), Some(edit_filter.clone()));
    // Search filters just this playlist's rows (drag-reorder pauses while a
    // query is active — see the drop handler). Created here so
    // load_pl_by_id can clear it when a different playlist opens; packed
    // into the pl-edit page below.
    let (pl_search_row, pl_search_entry) =
        make_view_search_row("Search this playlist — artist, title, album…");
    // F12.1: restore this view's last search query if the feature is on.
    if state.borrow().config.media_library.remember_search {
        let last = state.borrow().config.media_library.last_search.get("playlists").cloned();
        if let Some(last) = last {
            pl_search_entry.set_text(&last);
        }
    }
    {
        // 150 ms debounce — same rationale as the device search: the filter
        // walks every row per change, heavy on multi-thousand-row playlists.
        let q = pl_edit_query.clone();
        let filter = edit_filter.clone();
        let state_rc = state.clone();
        let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        pl_search_entry.connect_changed(move |e| {
            let raw_text = e.text().to_string();
            let text = raw_text.to_lowercase();
            if let Some(src) = pending.borrow_mut().take() {
                src.remove();
            }
            let q = q.clone();
            let filter = filter.clone();
            let state_inner = state_rc.clone();
            let pending_inner = pending.clone();
            let src = glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
                *q.borrow_mut() = text.clone();
                filter.changed(gtk4::FilterChange::Different);
                // F12.1: remember this view's query for next open.
                {
                    let mut s = state_inner.borrow_mut();
                    if s.config.media_library.remember_search {
                        s.config
                            .media_library
                            .last_search
                            .insert("playlists".to_string(), raw_text.clone());
                    }
                }
                pending_inner.borrow_mut().take();
                glib::ControlFlow::Break
            });
            *pending.borrow_mut() = Some(src);
        });
    }
    let edit_sort_model = gtk4::SortListModel::new(
        Some(edit_filter_model),
        None::<gtk4::Sorter>,
    );
    let edit_multi_sel: gtk4::MultiSelection =
        gtk4::MultiSelection::new(Some(edit_sort_model.clone()));
    // Fill the deferred holder now that the real model exists — see its
    // declaration above (`edit_multi_sel_holder`) for why this is deferred.
    *edit_multi_sel_holder.borrow_mut() = Some(edit_multi_sel.clone());
    let track_list: Rc<gtk4::ColumnView> = Rc::new({
        let cv = gtk4::ColumnView::new(Some(edit_multi_sel.clone()));
        cv.add_css_class("playlist");
        cv.set_vexpand(true);
        cv.set_show_row_separators(false);
        cv.set_show_column_separators(false);
        cv
    });

    // ── Editor columns ───────────────────────────────────────────────────
    // Split to `playlists_columns.rs` (plan step 7): the shared ALL_COLUMNS
    // table plus the editor-only position column, the reorder drag sources
    // and each cell's right-click gesture.
    let playlists_columns::Columns {
        apply_editor_columns,
        ple_action_group_holder,
        rebuild_track_list_holder,
        track_scroll_holder,
    } = playlists_columns::build(
        ctx,
        playlists_columns::ColumnUi {
            track_list: &track_list,
            edit_multi_sel: &edit_multi_sel,
            edit_sort_model: &edit_sort_model,
            editing_pl_id: &editing_pl_id,
            editing_tracks: &editing_tracks,
            ctx_canonical_idx: &ctx_canonical_idx,
            ed_ctx_indices: &ed_ctx_indices,
            ed_selected_tracks: &ed_selected_tracks,
            ed_action_group: &ed_action_group,
            drag_selection: &drag_selection,
            reorder_allowed: &reorder_allowed,
        },
    );


    // Rebuild track editor: splice the entire `editing_tracks` Vec into the
    // backing ListStore as `EditorEntry` items so each row carries its
    // canonical slot.  ColumnView recycles visible rows so this stays
    // cheap for big playlists.  Also rebuilds `position_map` for first-
    // occurrence path lookups by the cross-window drop target.
    let rebuild_track_list: Rc<dyn Fn()> = {
        let store    = edit_store.clone();
        let et       = editing_tracks.clone();
        let pos_map  = position_map.clone();
        Rc::new(move || {
            let mut map = pos_map.borrow_mut();
            map.clear();
            let items: Vec<glib::BoxedAnyObject> = et
                .borrow()
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    map.entry(t.path.clone()).or_insert(i);
                    glib::BoxedAnyObject::new(EditorEntry {
                        track: t.clone(),
                        canonical_idx: i,
                    })
                })
                .collect();
            drop(map);
            store.splice(0, store.n_items(), &items);
        })
    };
    // Populate the holder so the column factories' per-cell drop targets
    // can refresh the editor after a successful reorder.
    *rebuild_track_list_holder.borrow_mut() = Some(rebuild_track_list.clone());

    // Error banner shown when a playlist's file can't be read (e.g. the
    // library was scanned in another sandbox and the stored path doesn't
    // resolve here).  Hidden while the playlist loads normally.  Hoisted
    // here so load_pl_by_id below can capture it; packed into the
    // pl-edit page further down.
    let edit_error_label: Label = Label::builder()
        .label("")
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .margin_start(8).margin_end(8)
        .margin_top(4).margin_bottom(4)
        .visible(false)
        .build();
    edit_error_label.add_css_class("broken");

    // ── The saved-playlist manager ───────────────────────────────────────
    // Split to `playlists_manage.rs` (plan step 7): the `pl-manage` page, the
    // load-by-id seam the sidebar also calls, and the sub-row helper that
    // keeps the sidebar and the manage list in step.
    let playlists_manage::Manage {
        load_pl_by_id,
        btn_rename_pl_inline,
        btn_save_pl_outer,
        edit_header,
        edit_path_label,
    } = playlists_manage::build(
        ctx,
        sb,
        playlists_manage::ManageUi {
            pl_sub_stack: &pl_sub_stack,
            pl_manage_list: &pl_manage_list,
            editing_pl_id: &editing_pl_id,
            editing_tracks: &editing_tracks,
            saved_track_ids: &saved_track_ids,
            rebuild_track_list: &rebuild_track_list,
            apply_editor_columns: &apply_editor_columns,
            edit_error_label: &edit_error_label,
            pl_search_entry: &pl_search_entry,
        },
    );


    // ── Build "pl-edit" page ──────────────────────────────────────────────
    {
        let edit_vbox = GtkBox::new(Orientation::Vertical, 0);

        let header_row = GtkBox::new(Orientation::Horizontal, 4);
        header_row.append(&edit_header);
        header_row.append(&btn_rename_pl_inline);
        edit_vbox.append(&header_row);
        edit_vbox.append(&edit_path_label);
        edit_vbox.append(&edit_error_label);

        edit_vbox.append(&pl_search_row);

        let track_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Automatic)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .hexpand(true)
            .child(&*track_list)
            .build();
        edit_vbox.append(&track_scroll);
        // Expose track_scroll so cell right-click popovers can parent
        // themselves to it (parented-to-leaf popovers don't render).
        *track_scroll_holder.borrow_mut() = Some(track_scroll.clone());
        // Install the "ed" action group ONCE on this stable ScrolledWindow —
        // mirrors dev_tracks_scroll.insert_action_group("dev", ...) in the
        // device-tracks view. The per-cell PopoverMenu is parented here too
        // (see the per-cell gesture), so action lookup never has to walk
        // more than zero ancestors.
        track_scroll.insert_action_group("ed", Some(&ed_action_group));

        // Delete key on the editor's ColumnView removes the selected
        // rows from the playlist (canonical play order) and rewrites
        // the on-disk M3U8 — same behavior as the Remove from Playlist
        // menu item.
        {
            let key = EventControllerKey::new();
            let sel    = edit_multi_sel.clone();
            let et     = editing_tracks.clone();
            let ep_id  = editing_pl_id.clone();
            let rb     = rebuild_track_list.clone();
            let st     = state.clone();
            key.connect_key_pressed(move |_, keyval, _keycode, _mods| {
                // `l` — View/Search Lyrics for the single selected editor row
                // in Specific mode. No-op (Proceed) on a multi-row or empty
                // selection, matching the row menu's single-selection rule.
                if matches!(keyval, gdk::Key::l | gdk::Key::L) {
                    let sel_tracks: Vec<crate::media_library::LibTrack> = (0..sel.n_items())
                        .filter(|i| sel.is_selected(*i))
                        .filter_map(|i| sel.item(i))
                        .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        .map(|o| o.borrow::<EditorEntry>().track.clone())
                        .collect();
                    if let [t] = sel_tracks.as_slice() {
                        let path = std::path::PathBuf::from(&t.path);
                        let artist = t.artist.clone().unwrap_or_default();
                        let title = t.title.clone().unwrap_or_default();
                        let album_artist = t.album_artist.clone().unwrap_or_default();
                        view_or_search_lyrics(
                            &st, &path, &artist, &title, &album_artist,
                            rb.clone(), LyricsMode::Specific,
                        );
                        return glib::Propagation::Stop;
                    }
                    return glib::Propagation::Proceed;
                }
                if keyval != gdk::Key::Delete && keyval != gdk::Key::KP_Delete {
                    return glib::Propagation::Proceed;
                }
                let mut idxs: Vec<usize> = (0..sel.n_items())
                    .filter(|i| sel.is_selected(*i))
                    .filter_map(|i| sel.item(i))
                    .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .collect();
                if idxs.is_empty() { return glib::Propagation::Proceed }
                idxs.sort_unstable_by(|a, b| b.cmp(a));
                {
                    let mut e = et.borrow_mut();
                    for i in idxs.iter() {
                        if *i < e.len() { e.remove(*i); }
                    }
                }
                let pid = ep_id.get();
                if pid >= 0 {
                    let s = st.borrow();
                    if let Some(lib) = s.media_lib.as_ref() {
                        let paths: Vec<String> = et.borrow()
                            .iter().map(|t| t.path.clone()).collect();
                        if let Ok(pl) = lib.playlist_by_id(pid) {
                            let _ = lib.save_playlist_tracks_to_path(
                                std::path::Path::new(&pl.path),
                                &paths,
                            );
                        }
                    }
                }
                rb();
                glib::Propagation::Stop
            });
            track_list.add_controller(key);
        }

        // Editor DropTarget — handles two drop kinds:
        //
        //   1. Reorder (every dropped path already in `editing_tracks`):
        //      splice the rows to the canonical insert position resolved
        //      from the drop coordinate.  Gated by `reorder_allowed` so
        //      drops while a non-position sort is active no-op rather than
        //      adding duplicates at the bottom.
        //   2. External add (any dropped path not in `editing_tracks`):
        //      append the *new* paths to the on-disk M3U8 via
        //      `append_paths_to_playlist` and mirror them into the
        //      editor's in-memory state.
        {
            let dt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
            let state_drop  = state.clone();
            let et_drop     = editing_tracks.clone();
            let ep_drop     = editing_pl_id.clone();
            let rebuild_drop = rebuild_track_list.clone();
            let _posmap_drop = position_map.clone();
            let ra_drop     = reorder_allowed.clone();
            let query_drop  = pl_edit_query.clone();
            let tl_drop     = track_list.clone();
            let dragsel_drop = drag_selection.clone();
            dt.connect_drop(move |_, value, x, y| {
                let file_list = match value.get::<gdk::FileList>() {
                    Ok(fl) => fl,
                    Err(_) => return false,
                };
                let paths: Vec<String> = file_list.files().iter()
                    .filter_map(|f| f.path())
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                if paths.is_empty() { return false }
                let pid = ep_drop.get();
                let lib_opt_has = state_drop.borrow().media_lib.is_some();
                if !lib_opt_has { return false }

                // Prefer drag_selection (canonical indices captured by
                // our DragSource) so duplicates in the playlist resolve
                // correctly.  If the drag came from another window the
                // selection is empty — treat the paths as external add.
                let drag_src_indices: Vec<usize> = dragsel_drop.borrow().clone();
                let is_internal_reorder = !drag_src_indices.is_empty();

                if is_internal_reorder {
                    // Pure reorder.  Refuse silently when the current sort
                    // doesn't make reorder semantically sensible — avoids
                    // appending duplicates at the bottom in that case.  A
                    // live search filter breaks the display↔play-order
                    // mapping the same way, so it refuses too.
                    if !ra_drop.get() || !query_drop.borrow().is_empty() {
                        dragsel_drop.borrow_mut().clear();
                        return true;
                    }

                    // Resolve the drop coordinate to a canonical insert
                    // position.  First try pick(x, y) + walk up — works
                    // when the cursor is over a cell.  Falls back to a
                    // scan of every visible cell when the cursor lands
                    // between rows (no cell directly under it), inserting
                    // before the first cell whose vertical midpoint is
                    // past the drop y.  Last-resort default is append.
                    let dst_canon: usize = (|| {
                        let mut w = tl_drop.pick(x, y, gtk4::PickFlags::DEFAULT)?;
                        loop {
                            let name = w.widget_name().to_string();
                            if let Some(rest) = name.strip_prefix("pos:") {
                                if let Ok(n) = rest.parse::<usize>() {
                                    return Some(n);
                                }
                            }
                            w = w.parent()?;
                        }
                    })()
                    .or_else(|| {
                        let root_widget: &gtk4::Widget = tl_drop.upcast_ref();
                        let mut cells = editor_cell_positions(root_widget);
                        cells.sort_by(|a, b| a.1.partial_cmp(&b.1)
                            .unwrap_or(std::cmp::Ordering::Equal));
                        let drop_y = y as f32;
                        cells.iter()
                            .find(|c| c.1 + c.2 / 2.0 > drop_y)
                            .map(|c| c.0)
                    })
                    .unwrap_or_else(|| et_drop.borrow().len());

                    let mut sorted = drag_src_indices.clone();
                    sorted.sort_unstable_by(|a, b| b.cmp(a));
                    let mut adjusted_dst = dst_canon;
                    let mut removed: Vec<crate::media_library::LibTrack> = Vec::new();
                    {
                        let mut et = et_drop.borrow_mut();
                        for src in sorted.iter() {
                            if *src < et.len() {
                                let t = et.remove(*src);
                                if *src < adjusted_dst { adjusted_dst -= 1; }
                                removed.push(t);
                            }
                        }
                        removed.reverse();
                        let cap = et.len();
                        let insert_at = adjusted_dst.min(cap);
                        for (i, t) in removed.into_iter().enumerate() {
                            et.insert(insert_at + i, t);
                        }
                    }

                    if pid >= 0 {
                        let s = state_drop.borrow();
                        if let Some(lib) = s.media_lib.as_ref() {
                            let paths_now: Vec<String> = et_drop.borrow()
                                .iter().map(|t| t.path.clone()).collect();
                            if let Ok(pl) = lib.playlist_by_id(pid) {
                                if let Err(e) = lib.save_playlist_tracks_to_path(
                                    std::path::Path::new(&pl.path),
                                    &paths_now,
                                ) {
                                    eprintln!("editor reorder persist {pid}: {e}");
                                }
                            }
                        }
                    }
                    dragsel_drop.borrow_mut().clear();
                    let rb = rebuild_drop.clone();
                    glib::idle_add_local_once(move || rb());
                    return true;
                }

                // External add: append every dropped path; the user's
                // playlist may already contain some of them but treating
                // a cross-window drop as add is the least-surprising
                // semantics (duplicates can be removed afterwards).
                let new_paths: Vec<String> = paths.clone();
                if new_paths.is_empty() { return true }
                // Persist to disk first; only mutate in-memory editor state
                // if the save succeeded so failures don't leave the editor
                // diverged from the file on disk.
                if pid >= 0 {
                    let s = state_drop.borrow();
                    let lib = s.media_lib.as_ref().unwrap();
                    if let Err(e) = lib.append_paths_to_playlist(pid, &new_paths) {
                        eprintln!("editor drop append_paths_to_playlist {pid}: {e}");
                        return false;
                    }
                }
                let paths = new_paths;
                // Mirror the new entries into editing_tracks so the visible
                // ColumnView reflects them without needing a full reload.
                let new_libtracks: Vec<crate::media_library::LibTrack> = {
                    let s = state_drop.borrow();
                    let lib = s.media_lib.as_ref().unwrap();
                    paths.iter()
                        .map(|p| {
                            if let Ok(t) = lib.track_by_path(p) { return t }
                            let filename = std::path::Path::new(p)
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            crate::media_library::LibTrack {
                                id: 0,
                                path: p.clone(),
                                filename,
                                artist: None, title: None, album: None,
                                track_num: None, genre: None, year: None,
                                bpm: None, length_secs: None, bitrate: None,
                                channels: None, filetype: None,
                                play_count: 0, last_played: None,
                                comment: None, album_artist: None,
                                disc_num: None, disc_total: None,
                                composer: None, original_artist: None,
                                copyright: None, url: None, encoded_by: None,
                                lyric: None, artwork_path: None,
                                last_scanned: None,
                                sample_rate: None, file_size: None,
                                file_mtime: None, added_at: None,
                                bitrate_mode: None,
                                rg_track_gain: None,
                                rg_track_peak: None,
                                rg_album_gain: None,
                                rg_album_peak: None,
                                sort_keys: Default::default(),
                            }
                        })
                        .collect()
                };
                et_drop.borrow_mut().extend(new_libtracks);
                let rb = rebuild_drop.clone();
                glib::idle_add_local_once(move || rb());
                true
            });
            track_scroll.add_controller(dt);
        }

        // Track editor controls
        let edit_btn_row = GtkBox::new(Orientation::Horizontal, 4);
        edit_btn_row.set_margin_start(4); edit_btn_row.set_margin_end(4);
        edit_btn_row.set_margin_top(4);  edit_btn_row.set_margin_bottom(4);

        let btn_add_files_pl  = Button::with_label("+ Files");    btn_add_files_pl.add_css_class("pl-btn");
        let btn_add_folder_pl = Button::with_label("+ Folder");   btn_add_folder_pl.add_css_class("pl-btn");
        let btn_remove_tracks = Button::with_label("− Remove");   btn_remove_tracks.add_css_class("pl-btn");
        let btn_delete_pl     = Button::with_label("🗑 Delete Playlist"); btn_delete_pl.add_css_class("pl-btn");
        let spring_pl         = GtkBox::new(Orientation::Horizontal, 0); spring_pl.set_hexpand(true);
        let btn_revert_pl     = Button::with_label("↺ Revert");  btn_revert_pl.add_css_class("pl-btn");
        let btn_save_as_pl    = Button::with_label("Save As…");  btn_save_as_pl.add_css_class("pl-btn");
        let btn_save_pl       = btn_save_pl_outer.clone();
        let btn_enqueue_pl    = Button::with_label("Enqueue"); btn_enqueue_pl.add_css_class("pl-btn");
        let btn_send_to_ed    = gtk4::MenuButton::builder().label("Send to ▾").build();
        btn_send_to_ed.add_css_class("pl-btn");
        // Install "ed" directly on the button too — mirrors the files
        // view's btn_send_to: window-level alone enables the top-level
        // items but the NESTED submenu popovers (Saved Playlist ▸, Disc
        // Drive ▸, Removable Device ▸) resolve actions against
        // the button's own popover chain, so their items don't dispatch
        // unless the group also sits on the button itself.
        btn_send_to_ed.insert_action_group("ed", Some(&ed_action_group));
        let btn_play_pl       = Button::with_label("▶ Play");  btn_play_pl.add_css_class("pl-btn");

        edit_btn_row.append(&btn_add_files_pl);
        edit_btn_row.append(&btn_add_folder_pl);
        edit_btn_row.append(&btn_remove_tracks);
        edit_btn_row.append(&btn_delete_pl);
        edit_btn_row.append(&spring_pl);
        edit_btn_row.append(&btn_revert_pl);
        edit_btn_row.append(&btn_save_as_pl);
        edit_btn_row.append(&btn_save_pl);
        edit_btn_row.append(&btn_enqueue_pl);
        edit_btn_row.append(&btn_send_to_ed);
        edit_btn_row.append(&btn_play_pl);
        edit_vbox.append(&ed_status);
        edit_vbox.append(&edit_btn_row);

        // ── Playlist editor status bar ──────────────────────────────────────
        // Rows are `BoxedAnyObject<EditorEntry>` (a LibTrack + its canonical
        // play-order index, not a bare LibTrack — see EditorEntry's doc
        // comment above), so this goes through `ml_status_bar_for` with an
        // extractor into `.track.length_secs`. `rebuild_track_list` (above)
        // always `edit_store.splice(...)`s the SAME store on load/reorder/
        // save-revert rather than swapping in a new one, and it's the store
        // `edit_multi_sel` wraps (via edit_filter_model/edit_sort_model), so
        // items_changed keeps this live without an explicit refresh call.
        let (pl_status_bar, _) = ml_status_bar_for::<EditorEntry>(&edit_multi_sel, |e| {
            e.track.length_secs
        });
        edit_vbox.append(&pl_status_bar);
        // Directly below the playlist track list (above the button row), matching
        // the active playlist window.
        edit_vbox.reorder_child_after(&pl_status_bar, Some(&track_scroll));

        // Rebuild the Send-to menu model fresh on every open — drives/
        // devices may have come or gone. `set_create_popup_func` is
        // invoked by GTK right before the popover is shown; mirrors the
        // files view's btn_send_to.
        {
            let state_menu = state.clone();
            let current_drives = current_drives.clone();
            let current_devices = current_devices.clone();
            btn_send_to_ed.set_create_popup_func(move |btn| {
                let menu = build_send_to_menu(
                    &state_menu,
                    &SendToActions {
                        active: "ed.send-active",
                        new_playlist: "ed.add-to-new",
                        saved_playlist: "ed.add-to-saved",
                        drive: "ed.send-drive",
                        device: "ed.send-device",
                        drives: current_drives.borrow().iter()
                            .map(|d| (d.id.clone(), d.label.clone())).collect(),
                        devices: current_devices.borrow().iter()
                            .map(|d| (d.id.clone(), d.label.clone())).collect(),
                    },
                );
                btn.set_menu_model(Some(&menu));
            });
        }

        // ── Add Files ─────────────────────────────────────────────────────
        {
            let state_rc = state.clone();
            let et       = editing_tracks.clone();
            let rebuild  = rebuild_track_list.clone();
            let win_wk   = win.downgrade();
            btn_add_files_pl.connect_clicked(move |_| {
                let dialog = gtk4::FileDialog::builder().title("Add Audio Files").build();
                let filter = gtk4::FileFilter::new();
                filter.set_name(Some("Audio files"));
                // add_suffix (not add_mime_type) so files appear even when
                // the desktop has no MIME registration (.ape, .tta, …).
                for ext in crate::model::AUDIO_EXTENSIONS {
                    filter.add_suffix(ext);
                }
                let fs = gio::ListStore::new::<gtk4::FileFilter>();
                fs.append(&filter);
                dialog.set_filters(Some(&fs));
                let state2  = state_rc.clone();
                let et2     = et.clone();
                let rebuild2 = rebuild.clone();
                let parent  = win_wk.upgrade();
                dialog.open_multiple(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                    let Ok(list) = result else { return };
                    let paths: Vec<PathBuf> = (0..list.n_items())
                        .filter_map(|i| list.item(i))
                        .filter_map(|o| o.downcast::<gio::File>().ok())
                        .filter_map(|f| f.path())
                        .collect();
                    if paths.is_empty() { return; }
                    let s = state2.borrow();
                    if let Some(ref lib) = s.media_lib {
                        let existing: std::collections::HashSet<String> =
                            et2.borrow().iter().map(|t| t.path.clone()).collect();
                        for p in &paths {
                            if let Some(p_str) = p.to_str() {
                                if !existing.contains(p_str) {
                                    // A file the library does not know cannot
                                    // be added, because the playlist is saved
                                    // by row id. Say so rather than dropping
                                    // it: silently ignoring the file the user
                                    // just picked reads as "Save did nothing".
                                    match lib.track_by_path(p_str) {
                                        Ok(t) => et2.borrow_mut().push(t),
                                        Err(e) => eprintln!(
                                            "add to playlist: skipping {p_str}: {e:#}"
                                        ),
                                    }
                                }
                            }
                        }
                    }
                    drop(s);
                    rebuild2();
                });
            });
        }

        // ── Add Folder ────────────────────────────────────────────────────
        {
            let state_rc = state.clone();
            let et       = editing_tracks.clone();
            let rebuild  = rebuild_track_list.clone();
            let win_wk   = win.downgrade();
            btn_add_folder_pl.connect_clicked(move |_| {
                let dialog = gtk4::FileDialog::builder().title("Add Folder").build();
                let state2   = state_rc.clone();
                let et2      = et.clone();
                let rebuild2 = rebuild.clone();
                let parent   = win_wk.upgrade();
                dialog.select_folder(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                    let Ok(file) = result else { return };
                    let Some(folder) = file.path() else { return };
                    let Some(folder_str) = folder.to_str() else { return };
                    let s = state2.borrow();
                    if let Some(ref lib) = s.media_lib {
                        let existing: std::collections::HashSet<String> =
                            et2.borrow().iter().map(|t| t.path.clone()).collect();
                        let new_tracks: Vec<_> = lib.all_tracks().unwrap_or_default()
                            .into_iter()
                            .filter(|t| t.path.starts_with(folder_str) && !existing.contains(&t.path))
                            .collect();
                        et2.borrow_mut().extend(new_tracks);
                    }
                    drop(s);
                    rebuild2();
                });
            });
        }

        // ── Remove selected tracks ────────────────────────────────────────
        {
            let sel     = edit_multi_sel.clone();
            let et      = editing_tracks.clone();
            let rebuild = rebuild_track_list.clone();
            btn_remove_tracks.connect_clicked(move |_| {
                // Map display-index selection through EditorEntry so each
                // selected row resolves to its canonical play-order slot.
                // Otherwise duplicates / a non-default sort cause the wrong
                // rows to be removed from `editing_tracks`.
                let mut to_remove: Vec<usize> = (0..sel.n_items())
                    .filter(|i| sel.is_selected(*i))
                    .filter_map(|i| sel.item(i))
                    .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .collect();
                if to_remove.is_empty() { return }
                to_remove.sort_unstable_by(|a, b| b.cmp(a));
                let mut tracks = et.borrow_mut();
                for idx in to_remove.into_iter() {
                    if idx < tracks.len() { tracks.remove(idx); }
                }
                drop(tracks);
                rebuild();
            });
        }

        // ── Revert ────────────────────────────────────────────────────────
        {
            let load    = load_pl_by_id.clone();
            let sidebar_ref = sidebar.clone();
            btn_revert_pl.connect_clicked(move |_| {
                // Find currently-selected sidebar pl: row
                let mut i = 0i32;
                loop {
                    match sidebar_ref.row_at_index(i) {
                        Some(row) => {
                            let name = row.widget_name().to_string();
                            if row.is_selected() {
                                if let Some(id_str) = name.strip_prefix("pl:") {
                                    if let Ok(id) = id_str.parse::<i64>() { load(id); }
                                }
                                break;
                            }
                            i += 1;
                        }
                        None => break,
                    }
                }
            });
        }

        // ── Save As playlist ──────────────────────────────────────────────
        {
            let state_rc     = state.clone();
            let et           = editing_tracks.clone();
            let ep_id        = editing_pl_id.clone();
            let load         = load_pl_by_id.clone();
            let sidebar_ref  = sidebar.clone();
            let pl_ml_ref    = pl_manage_list.clone();
            let win_wk       = win.downgrade();
            btn_save_as_pl.connect_clicked(move |_| {
                let Some(win) = win_wk.upgrade() else { return };
                // Pre-fill the Save dialog with the current playlist's name
                // (or "New Playlist" when the editor has no playlist loaded).
                let initial_stem = if ep_id.get() >= 0 {
                    state_rc.borrow().media_lib.as_ref()
                        .and_then(|lib| lib.playlist_by_id(ep_id.get()).ok())
                        .map(|pl| pl.name)
                        .unwrap_or_else(|| "New Playlist".to_string())
                } else {
                    "New Playlist".to_string()
                };
                let paths: Vec<String> = et.borrow().iter().map(|t| t.path.clone()).collect();
                let state2   = state_rc.clone();
                let ep_id2   = ep_id.clone();
                let load2    = load.clone();
                let sidebar2 = sidebar_ref.clone();
                let pl_ml2   = pl_ml_ref.clone();
                // Native Save dialog replaces the previous name-only popup —
                // user chooses both filename and folder so the new .m3u8
                // doesn't silently land in the managed-playlists dir (which
                // `add_playlist_file` then registered as a watched folder).
                run_playlist_save_dialog(state_rc.clone(), win, &initial_stem, move |path, win_cb| {
                    let new_name = path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Untitled")
                        .to_string();
                    let save_result = state2.borrow().media_lib.as_ref()
                        .map(|lib| lib.save_playlist_tracks_to_path(&path, &paths));
                    let new_id = match save_result {
                        Some(Ok(id)) => id,
                        Some(Err(e)) => {
                            eprintln!("save_playlist_tracks_to_path: {e}");
                            show_playlist_save_error(&win_cb, &path, &e);
                            return;
                        }
                        None => return,
                    };

                    // Add row to manage list + sidebar
                    let lbl = Label::builder()
                        .label(&new_name)
                        .halign(Align::Start)
                        .margin_start(8).margin_end(8)
                        .margin_top(3).margin_bottom(3)
                        .build();
                    let manage_row = ListBoxRow::new();
                    manage_row.set_widget_name(&new_id.to_string());
                    manage_row.set_child(Some(&lbl));
                    attach_pl_row_drag(&manage_row, new_id);
                    pl_ml2.append(&manage_row);

                    let s_lbl = Label::builder()
                        .label(&new_name)
                        .halign(Align::Start)
                        .xalign(0.0)
                        .margin_start(sidebar::SUB_ROW_INSET).margin_end(8)
                        .margin_top(4).margin_bottom(4)
                        .build();
                    let s_row = ListBoxRow::new();
                    s_row.set_widget_name(&format!("pl:{}", new_id));
                    s_row.set_child(Some(&s_lbl));
                    attach_pl_row_drag(&s_row, new_id);
                    sidebar2.insert(&s_row, sidebar_pl_end_index(&sidebar2));
                    sidebar2.select_row(Some(&s_row));

                    ep_id2.set(new_id);
                    load2(new_id);
                });
            });
        }

        // ── Save playlist ─────────────────────────────────────────────────
        {
            let state_rc    = state.clone();
            let et          = editing_tracks.clone();
            let saved       = saved_track_ids.clone();
            let ep_id       = editing_pl_id.clone();
            btn_save_pl.connect_clicked(move |_| {
                let id = ep_id.get();
                if id < 0 { return; }
                let track_ids: Vec<i64> = et.borrow().iter().map(|t| t.id).collect();
                if let Some(ref lib) = state_rc.borrow().media_lib {
                    // Report a failed write instead of discarding it. Save is
                    // the one button whose whole purpose is a side effect, so
                    // swallowing the Result made a failure indistinguishable
                    // from success — and left `saved` claiming state that
                    // never reached disk, so the dirty indicator cleared too.
                    if let Err(e) = lib.save_playlist_tracks(id, &track_ids) {
                        eprintln!("save_playlist_tracks {id}: {e:#}");
                        return;
                    }
                    *saved.borrow_mut() = track_ids;
                }
            });
        }

        // ── Play (replace active playlist; honour autoplay) ──────────────
        {
            let state_rc   = state.clone();
            let et         = editing_tracks.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let set_track2 = set_track.clone();
            btn_play_pl.connect_clicked(move |_| {
                let tracks: Vec<crate::media_library::LibTrack> = et.borrow().clone();
                if tracks.is_empty() { return; }
                let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                {
                    let mut s = state_rc.borrow_mut();
                    let _ = s.player.stop();
                    s.playlist = crate::model::Playlist::new();
                    for lt in &tracks {
                        s.playlist.add(crate::model::Track::from(lt));
                    }
                }
                if autoplay {
                    if let Some(display) = state_rc.borrow_mut().play_current() {
                        set_track2(&display);
                    }
                }
                rebuild_pl();
            });
        }

        // ── Enqueue (append to active playlist) ──────────────────────────
        {
            let state_rc   = state.clone();
            let et         = editing_tracks.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let set_track2 = set_track.clone();
            btn_enqueue_pl.connect_clicked(move |_| {
                let tracks: Vec<crate::media_library::LibTrack> = et.borrow().clone();
                if tracks.is_empty() { return; }
                let was_empty = state_rc.borrow().playlist.is_empty();
                let autoplay  = state_rc.borrow().config.behavior.autoplay_on_add;
                {
                    let mut s = state_rc.borrow_mut();
                    for lt in &tracks {
                        s.playlist.add(crate::model::Track::from(lt));
                    }
                }
                // Don't interrupt a track the user is already listening to.
                if autoplay && was_empty {
                    if let Some(display) = state_rc.borrow_mut().play_current() {
                        set_track2(&display);
                    }
                }
                rebuild_pl();
            });
        }

        // ── Delete this playlist ─────────────────────────────────────────
        {
            let state_rc      = state.clone();
            let ep_id         = editing_pl_id.clone();
            let pl_list_ref   = pl_manage_list.clone();
            let sidebar_ref   = sidebar.clone();
            let sub_rows_ref  = pl_sub_rows.clone();
            let pl_sub_ref    = pl_sub_stack.clone();
            let et            = editing_tracks.clone();
            let saved         = saved_track_ids.clone();
            let rebuild       = rebuild_track_list.clone();
            let win_wk        = win.downgrade();
            btn_delete_pl.connect_clicked(move |_| {
                let id = ep_id.get();
                if id < 0 { return; }
                let pl_name = state_rc.borrow().media_lib.as_ref()
                    .and_then(|lib| lib.playlist_by_id(id).ok())
                    .map(|pl| pl.name.clone())
                    .unwrap_or_default();

                let dialog = gtk4::AlertDialog::builder()
                    .message(format!("Delete \"{}\"?", pl_name))
                    .detail("The playlist file on disk is not deleted.")
                    .buttons(vec!["Cancel".to_string(), "Delete".to_string()])
                    .cancel_button(0).default_button(1).modal(true).build();

                let state2   = state_rc.clone();
                let ep_id2   = ep_id.clone();
                let pl_ref2  = pl_list_ref.clone();
                let sid2     = sidebar_ref.clone();
                let sub2     = sub_rows_ref.clone();
                let pls2     = pl_sub_ref.clone();
                let et2      = et.clone();
                let saved2   = saved.clone();
                let rebuild2 = rebuild.clone();
                dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |result| {
                    if result != Ok(1) { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.remove_playlist(id);
                    }
                    // Drop the manage-list row whose widget_name == id.
                    let target = id.to_string();
                    let mut i = 0i32;
                    loop {
                        match pl_ref2.row_at_index(i) {
                            Some(r) if r.widget_name() == target => {
                                pl_ref2.remove(&r);
                                break;
                            }
                            Some(_) => i += 1,
                            None => break,
                        }
                    }
                    // Drop the matching sidebar sub-row.
                    let target_s = format!("pl:{}", id);
                    sub2.borrow_mut().retain(|r| {
                        if r.widget_name() == target_s {
                            sid2.remove(r);
                            false
                        } else { true }
                    });
                    // Clear editing state and bounce back to the manage page.
                    ep_id2.set(-1);
                    et2.borrow_mut().clear();
                    saved2.borrow_mut().clear();
                    rebuild2();
                    pls2.set_visible_child_name("pl-manage");
                });
            });
        }

        // ── Rename this playlist (header-row button) ─────────────────────
        {
            let state_rc      = state.clone();
            let ep_id         = editing_pl_id.clone();
            let header_ref    = edit_header.clone();
            let pl_list_ref   = pl_manage_list.clone();
            let sidebar_ref   = sidebar.clone();
            let win_wk        = win.downgrade();
            btn_rename_pl_inline.connect_clicked(move |_| {
                let id = ep_id.get();
                if id < 0 { return; }
                let current = state_rc.borrow().media_lib.as_ref()
                    .and_then(|lib| lib.playlist_by_id(id).ok())
                    .map(|pl| pl.name.clone())
                    .unwrap_or_default();

                let dialog = gtk4::Window::builder()
                    .title("Rename Playlist").modal(true).resizable(false).default_width(300)
                    .build();
                if let Some(w) = win_wk.upgrade() { dialog.set_transient_for(Some(&w)); }
                let vbox = GtkBox::new(Orientation::Vertical, 8);
                vbox.set_margin_top(12); vbox.set_margin_bottom(12);
                vbox.set_margin_start(12); vbox.set_margin_end(12);
                let lbl = Label::builder().label("New name:").halign(Align::Start).build();
                let name_entry = Entry::new();
                name_entry.set_text(&gtk_safe(&current));
                name_entry.set_hexpand(true);
                let btns_box = GtkBox::new(Orientation::Horizontal, 6);
                btns_box.set_halign(Align::End);
                let cancel_btn = Button::with_label("Cancel");
                let ok_btn     = Button::with_label("Rename");
                ok_btn.add_css_class("suggested-action");
                btns_box.append(&cancel_btn); btns_box.append(&ok_btn);
                vbox.append(&lbl); vbox.append(&name_entry); vbox.append(&btns_box);
                dialog.set_child(Some(&vbox));

                let d = dialog.clone();
                cancel_btn.connect_clicked(move |_| { d.close(); });

                let d        = dialog.clone();
                let e        = name_entry.clone();
                let state2   = state_rc.clone();
                let header2  = header_ref.clone();
                let pl_ref2  = pl_list_ref.clone();
                let sid2     = sidebar_ref.clone();
                ok_btn.connect_clicked(move |_| {
                    let name = e.text().to_string();
                    let name = name.trim();
                    if name.is_empty() { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.rename_playlist(id, name);
                    }
                    header2.set_text(&gtk_safe(name));
                    // Update manage-list row label.
                    let target = id.to_string();
                    let mut i = 0i32;
                    loop {
                        match pl_ref2.row_at_index(i) {
                            Some(r) if r.widget_name() == target => {
                                if let Some(c) = r.child() {
                                    if let Ok(l) = c.downcast::<Label>() {
                                        l.set_text(&gtk_safe(name));
                                    }
                                }
                                break;
                            }
                            Some(_) => i += 1,
                            None => break,
                        }
                    }
                    // Update sidebar sub-row label.
                    let target_s = format!("pl:{}", id);
                    let mut j = 0i32;
                    loop {
                        match sid2.row_at_index(j) {
                            Some(r) if r.widget_name() == target_s => {
                                if let Some(c) = r.child() {
                                    if let Ok(l) = c.downcast::<Label>() {
                                        l.set_text(&gtk_safe(name));
                                    }
                                }
                                break;
                            }
                            Some(_) => j += 1,
                            None => break,
                        }
                    }
                    d.close();
                });
                let ok2 = ok_btn.clone();
                name_entry.connect_activate(move |_| { ok2.activate(); });
                dialog.present();
            });
        }

        // ── Right-click context menu on track rows ───────────────────────
        // Split to `playlists_menu.rs` (plan step 7).
        playlists_menu::connect(
            ctx,
            playlists_menu::EditorMenuUi {
                track_scroll_holder: &track_scroll_holder,
                track_list: &track_list,
                ctx_canonical_idx: &ctx_canonical_idx,
                editing_pl_id: &editing_pl_id,
                editing_tracks: &editing_tracks,
                rebuild_track_list: &rebuild_track_list,
                ple_action_group_holder: &ple_action_group_holder,
                edit_multi_sel: &edit_multi_sel,
            },
        );

        pl_sub_stack.add_named(&edit_vbox, Some("pl-edit"));
    }

    {
        let pl_vbox = GtkBox::new(Orientation::Vertical, 0);
        pl_vbox.append(&*pl_sub_stack);
        stack.add_named(&pl_vbox, Some("playlists"));
    }

    // ── Sidebar routing ──────────────────────────────────────────────────
    // This page's own row-selected handler. The Files and Albums branches
    // that used to share it moved to `files.rs` and `albums.rs` on
    // 2026-08-10 — they were only here because this is where the code sat,
    // and keeping them would have dragged both pages' navigation into the
    // Playlists module in step 7.
    {
        let stack_ref      = stack.clone();
        let pl_sub_ref     = pl_sub_stack.clone();
        let load           = load_pl_by_id.clone();
        let state_rc       = state.clone();
        let expanded_rc    = playlists_expanded.clone();
        let hdr_lbl        = edit_header.clone();
        let path_lbl       = edit_path_label.clone();
        let save_btn       = btn_save_pl_outer.clone();
        sidebar.connect_row_selected(move |_, opt_row| {
            let row = match opt_row { Some(r) => r, None => return };
            let name = row.widget_name().to_string();

            if name == "playlists" {
                stack_ref.set_visible_child_name("playlists");
                pl_sub_ref.set_visible_child_name("pl-manage");
                // Expand sub-rows on navigation
                if !expanded_rc.get() {
                    expanded_rc.set(true);
                }
            } else if let Some(id_str) = name.strip_prefix("pl:") {
                if let Ok(id) = id_str.parse::<i64>() {
                    stack_ref.set_visible_child_name("playlists");
                    load(id);
                    pl_sub_ref.set_visible_child_name("pl-edit");
                    // Update editor header, path bar, and Save sensitivity.
                    if let Some(ref lib) = state_rc.borrow().media_lib {
                        if let Ok(pl) = lib.playlist_by_id(id) {
                            hdr_lbl.set_text(&gtk_safe(&pl.name));
                            path_lbl.set_text(&gtk_safe(&pl.path));
                            // Disable Save for external playlists; user should
                            // use Save As to get a Sparkamp-managed copy.
                            let is_managed = lib.playlist_is_managed(id);
                            save_btn.set_sensitive(is_managed);
                        }
                    }
                }
            }
        });
    }
}
