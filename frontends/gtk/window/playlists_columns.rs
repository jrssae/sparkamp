//! The playlist editor's columns, cells and per-row gestures.
//!
//! Split from [`super::playlists`] (plan step 7) — the editor's half of what
//! [`super::devices_columns`] is to the device view, and built from the same
//! shared `ALL_COLUMNS` table, so a column the user adds, reorders or resizes
//! in the Files view shows up the same here.
//!
//! On top of that table this owns three editor-only things:
//!
//! - the **position column**, which reads each row's canonical play-order
//!   slot out of [`super::playlists::EditorEntry`] rather than its display
//!   index, so duplicates of one file don't all collapse onto the last
//!   occurrence's number;
//! - the **drag sources** for intra-list reorder, live only while the display
//!   sort still preserves play order;
//! - each cell's **right-click gesture**, which records the clicked row's
//!   canonical slot and then pops the menu [`super::playlists_menu`] builds.
//!
//! That last one is why `ple_action_group_holder` is declared here and filled
//! there: the cells exist long before the menu does. The holder pattern from
//! docs/gtk-breakup-plan.md §3.1 — **a holder left `None` is not an error, it
//! is a silent no-op.**

use gtk4::prelude::*;
use gtk4::{
    gdk, gio, glib, Align, ColumnView, ColumnViewColumn, CustomSorter, DropTarget, Label,
    MultiSelection, ScrolledWindow, SortListModel,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::art_window;
use super::playlists::EditorEntry;
use super::{
    apply_ml_columns_to, build_send_to_menu, context_popover, format_last_played, gtk_safe,
    ml_sort_key, notify_playlist_changed, open_id3_editor_window, run_playlist_save_dialog,
    show_playlist_save_error, view_or_search_lyrics, truncate_display, ArtworkCells, LyricsMode, MlCtx,
    SendToActions, ALL_COLUMNS,
};

/// A rebuild-the-editor-table closure, late-bound because the cells are built
/// before the closure that refreshes them exists.
type RebuildClosure = Rc<dyn Fn()>;

/// The editor state the columns bind against.
pub(super) struct ColumnUi<'a> {
    pub track_list: &'a Rc<ColumnView>,
    pub edit_multi_sel: &'a MultiSelection,
    pub edit_sort_model: &'a SortListModel,
    pub editing_tracks: &'a Rc<RefCell<Vec<crate::media_library::LibTrack>>>,
    /// Recorded by each cell's right-click so single-row actions hit the
    /// exact row, duplicates included.
    pub ctx_canonical_idx: &'a Rc<Cell<i64>>,
    /// Rows the row-scoped actions operate on, stashed per right-click.
    pub ed_ctx_indices: &'a Rc<RefCell<Vec<usize>>>,
    /// The live selection reader the Send-to actions share.
    pub ed_selected_tracks: &'a Rc<dyn Fn() -> Vec<crate::media_library::LibTrack>>,
    /// The editor's action group, inserted on the widgets the menus parent to.
    pub ed_action_group: &'a gio::SimpleActionGroup,
    /// Rows picked up by an in-progress intra-list drag.
    pub drag_selection: &'a Rc<RefCell<Vec<usize>>>,
    /// True only while the display sort still preserves play order, which is
    /// the one state where drag-reorder maps cleanly onto the backing list.
    pub reorder_allowed: &'a Rc<Cell<bool>>,
}

/// What the rest of the page needs back.
pub(super) struct Columns {
    /// Re-apply the shared column config after it changes elsewhere.
    pub apply_editor_columns: Rc<dyn Fn()>,
    /// Filled by [`super::playlists_menu`]; read by each cell's gesture.
    pub ple_action_group_holder: Rc<RefCell<Option<gio::SimpleActionGroup>>>,
    /// Filled by the page once its rebuild closure exists.
    pub rebuild_track_list_holder: Rc<RefCell<Option<RebuildClosure>>>,
    /// The editor's scroller — the menus' stable parent.
    pub track_scroll_holder: Rc<RefCell<Option<ScrolledWindow>>>,
}

/// Build the editor's columns and attach them to its `ColumnView`.
pub(super) fn build(ctx: &MlCtx, ui: ColumnUi<'_>) -> Columns {
    // Local names for what this takes from its context and its caller, so the
    // body below reads as it did inside `playlists::build`.
    let state = ctx.host.state.clone();
    let rebuild_playlist = ctx.host.rebuild_playlist.clone();
    let set_track = ctx.host.set_track.clone();
    let current_drives = ctx.host.current_drives.clone();
    let current_devices = ctx.host.current_devices.clone();
    let win = ctx.win.clone();
    let track_list = ui.track_list.clone();
    let edit_multi_sel = ui.edit_multi_sel.clone();
    let edit_sort_model = ui.edit_sort_model.clone();
    let editing_tracks = ui.editing_tracks.clone();
    let ctx_canonical_idx = ui.ctx_canonical_idx.clone();
    let ed_ctx_indices = ui.ed_ctx_indices.clone();
    let ed_selected_tracks = ui.ed_selected_tracks.clone();
    let ed_action_group = ui.ed_action_group.clone();
    let drag_selection = ui.drag_selection.clone();
    let reorder_allowed = ui.reorder_allowed.clone();
    // The artwork column's cell, shared with the Files page and the device
    // view so all three render the same thumbnail (see `ArtworkCells`).
    let artwork_cells = Rc::new(ArtworkCells::new());

    // ── Editor columns: walk ALL_COLUMNS so files view + editor stay in
    //    lock-step on which columns exist and which order they default to.
    // Position column reference is captured here so the sorter-change
    // listener below can detect when the user has selected position-ASC
    // (the only sort that allows intra-list drag-reorder).
    let pos_col_holder: Rc<RefCell<Option<ColumnViewColumn>>> = Rc::new(RefCell::new(None));
    // Editor named columns (skipping the leading status + position pinned
    // columns) — captured so we can apply the files-view saved order so
    // the user only has to arrange columns in one place.
    let mut editor_named_cols: Vec<(String, ColumnViewColumn)> = Vec::new();
    // Holder for the rebuild closure — populated right after the closure
    // is defined.  Cell factories install per-cell drop targets that need
    // to refresh the editor after a successful reorder, but those factory
    // setups live above the rebuild definition in source order.
    type RebuildClosure = Rc<dyn Fn()>;
    let rebuild_track_list_holder: Rc<RefCell<Option<RebuildClosure>>> =
        Rc::new(RefCell::new(None));

    // Holder for the editor's "ple" action group.  Cell factories pop
    // PopoverMenus parented to track_list; the popover's action lookup
    // walks the GTK widget chain back to track_list where the group is
    // also attached, but some GTK4 versions break that walk with the
    // NESTED PopoverMenu flag.  Installing the group directly on each
    // popup makes dispatch reliable regardless of GTK version.
    let ple_action_group_holder: Rc<RefCell<Option<gio::SimpleActionGroup>>> =
        Rc::new(RefCell::new(None));
    // Holder for the editor's ScrolledWindow — populated right after it
    // is built so the cell right-click handler can use it as the popover
    // parent (cell-label parents render invisible on this GTK4 build), and
    // so the "ed" action group can be installed on it once (see below).
    let track_scroll_holder: Rc<RefCell<Option<gtk4::ScrolledWindow>>> =
        Rc::new(RefCell::new(None));

    // Row-scoped "ed" actions for the per-cell context menu's non-Send-to
    // items (Replace Current Playlist, Edit/View ID3, Remove from
    // Playlist) plus the Send-to family's Active Playlist / Saved
    // Playlist entries (send-drive/send-device were already registered
    // above). Registered here — after rebuild_track_list_holder and
    // track_scroll_holder exist — because `remove` needs the rebuild
    // closure holder. All read row context from `ed_ctx_indices`
    // (selection, falling back to the single clicked row), populated once
    // per right-click by the per-cell gesture below instead of being
    // recomputed per menu item.
    {
        // Send to Active Playlist — reachable from both the per-row
        // right-click menu and the "Send to ▾" button, so it reads the
        // live editor selection (G1) rather than a right-click stash.
        let state_c = state.clone();
        let rebuild_pl = rebuild_playlist.clone();
        let set_track_c = set_track.clone();
        let sel_tracks = ed_selected_tracks.clone();
        let action = gio::SimpleAction::new("send-active", None);
        action.connect_activate(move |_, _| {
            let tracks = sel_tracks();
            if tracks.is_empty() { return }
            let was_empty = state_c.borrow().playlist.is_empty();
            let autoplay = state_c.borrow().config.behavior.autoplay_on_add;
            let add_start = state_c.borrow().playlist.tracks.len();
            {
                let mut s = state_c.borrow_mut();
                for lt in &tracks { s.playlist.add(crate::model::Track::from(lt)); }
            }
            super::playlist_add::schedule_from(&state_c, add_start, false);
            if autoplay && was_empty {
                if let Some(d) = state_c.borrow_mut().play_current() { set_track_c(&d); }
            }
            rebuild_pl();
        });
        ed_action_group.add_action(&action);
    }
    {
        // Replace Current Playlist — same body as the old flat button.
        let et = editing_tracks.clone();
        let state_c = state.clone();
        let rebuild_pl = rebuild_playlist.clone();
        let set_track_c = set_track.clone();
        let idxs_src = ed_ctx_indices.clone();
        let action = gio::SimpleAction::new("replace", None);
        action.connect_activate(move |_, _| {
            let tracks: Vec<crate::media_library::LibTrack> = {
                let et_b = et.borrow();
                idxs_src.borrow().iter().filter_map(|&i| et_b.get(i).cloned()).collect()
            };
            if tracks.is_empty() { return }
            let autoplay = state_c.borrow().config.behavior.autoplay_on_add;
            {
                let mut s = state_c.borrow_mut();
                let _ = s.player.stop();
                s.playlist = crate::model::Playlist::new();
                for lt in &tracks { s.playlist.add(crate::model::Track::from(lt)); }
            }
            super::playlist_add::schedule_from(&state_c, 0, false);
            if autoplay {
                if let Some(d) = state_c.borrow_mut().play_current() { set_track_c(&d); }
            }
            rebuild_pl();
        });
        ed_action_group.add_action(&action);
    }
    {
        // Edit / View ID3 — single-row only, reads the clicked row (not
        // the selection) via ctx_canonical_idx, same as the old flat button.
        let et = editing_tracks.clone();
        let state_c = state.clone();
        let rebuild_pl = rebuild_playlist.clone();
        let ctx_c = ctx_canonical_idx.clone();
        let action = gio::SimpleAction::new("edit-id3", None);
        action.connect_activate(move |_, _| {
            let c = ctx_c.get();
            if c < 0 { return }
            let path = et.borrow().get(c as usize).map(|t| t.path.clone());
            let Some(path) = path else { return };
            open_id3_editor_window(
                None::<&gtk4::Window>,
                path.into(),
                state_c.clone(),
                rebuild_pl.clone(),
                None,
                None,
            );
        });
        ed_action_group.add_action(&action);
    }
    {
        // View/Search Lyrics (F15) on saved-playlist editor rows.
        let et = editing_tracks.clone();
        let state_c = state.clone();
        let rebuild_pl = rebuild_playlist.clone();
        let ctx_c = ctx_canonical_idx.clone();
        let action = gio::SimpleAction::new("lyrics", None);
        action.connect_activate(move |_, _| {
            let c = ctx_c.get();
            if c < 0 { return }
            let t = et.borrow().get(c as usize).map(|t| {
                (
                    std::path::PathBuf::from(&t.path),
                    t.artist.clone().unwrap_or_default(),
                    t.title.clone().unwrap_or_default(),
                    t.album_artist.clone().unwrap_or_default(),
                )
            });
            let Some((path, artist, title, album_artist)) = t else { return };
            view_or_search_lyrics(&state_c, &path, &artist, &title, &album_artist, rebuild_pl.clone(), LyricsMode::Specific);
        });
        ed_action_group.add_action(&action);
    }
    {
        // View Album Art for the right-clicked editor row.
        let et = editing_tracks.clone();
        let state_c = state.clone();
        let ctx_c = ctx_canonical_idx.clone();
        let action = gio::SimpleAction::new("view-art", None);
        action.connect_activate(move |_, _| {
            let c = ctx_c.get();
            if c < 0 { return }
            let path = et.borrow().get(c as usize).map(|t| std::path::PathBuf::from(&t.path));
            if let Some(path) = path { art_window::open_track_art(&state_c, &path); }
        });
        ed_action_group.add_action(&action);
    }
    {
        // Remove from Playlist — same body as the old flat button.
        let et = editing_tracks.clone();
        let rb_holder = rebuild_track_list_holder.clone();
        let idxs_src = ed_ctx_indices.clone();
        let action = gio::SimpleAction::new("remove", None);
        action.connect_activate(move |_, _| {
            let mut idxs = idxs_src.borrow().clone();
            if idxs.is_empty() { return }
            idxs.sort_unstable_by(|a, b| b.cmp(a));
            {
                let mut e = et.borrow_mut();
                for i in idxs.iter() {
                    if *i < e.len() { e.remove(*i); }
                }
            }
            // No write here — see the same note in playlists.rs. Edits stay in
            // `editing_tracks` until Save (2026-08-10).
            if let Some(rb) = rb_holder.borrow().as_ref() { rb(); }
        });
        ed_action_group.add_action(&action);
    }
    {
        // Seed a brand new saved playlist — reachable from both the
        // right-click menu and the "Send to ▾" button, so it reads the
        // live editor selection (G1) rather than a right-click stash.
        let state_c = state.clone();
        let sel_tracks = ed_selected_tracks.clone();
        let win_c = win.clone();
        let action = gio::SimpleAction::new("add-to-new", None);
        action.connect_activate(move |_, _| {
            let paths: Vec<String> = sel_tracks().iter().map(|t| t.path.clone()).collect();
            if paths.is_empty() { return }
            let default_stem = glib::DateTime::now_local().ok()
                .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Playlist".to_string());
            let state_cb = state_c.clone();
            let paths_cb = paths.clone();
            run_playlist_save_dialog(
                state_c.clone(),
                win_c.clone(),
                &default_stem,
                move |path, win_cb| {
                    if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                        if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths_cb) {
                            eprintln!("save_playlist_tracks_to_path: {e}");
                            show_playlist_save_error(&win_cb, &path, &e);
                        }
                    }
                },
            );
        });
        ed_action_group.add_action(&action);
    }
    {
        // Append to an existing saved playlist — reachable from both the
        // right-click menu and the "Send to ▾" button, so it reads the
        // live editor selection (G1) rather than a right-click stash.
        let state_c = state.clone();
        let sel_tracks = ed_selected_tracks.clone();
        let action = gio::SimpleAction::new(
            "add-to-saved",
            Some(glib::VariantTy::INT64),
        );
        action.connect_activate(move |_, param| {
            let Some(pid) = param.and_then(|p| p.get::<i64>()) else { return };
            let paths: Vec<String> = sel_tracks().iter().map(|t| t.path.clone()).collect();
            if paths.is_empty() { return }
            let mut ok = false;
            if let Some(lib) = state_c.borrow().media_lib.as_ref() {
                match lib.append_paths_to_playlist(pid, &paths) {
                    Ok(_) => ok = true,
                    Err(e) => eprintln!("append_paths_to_playlist {pid}: {e}"),
                }
            }
            if ok { notify_playlist_changed(pid); }
        });
        ed_action_group.add_action(&action);
    }
    {
        let visible_ids: Vec<String> =
            state.borrow().config.media_library.visible_columns.clone();
        let saved_widths: std::collections::HashMap<String, i32> =
            state.borrow().config.media_library.ml_file_col_widths.clone();

        // Leading status-glyph column (⚠/🔒) — playlist-editor-only, mirrors
        // the unscanned-indicator column on the files side.
        {
            let factory = gtk4::SignalListItemFactory::new();
            factory.connect_setup(|_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() { return }
                let lbl = Label::builder()
                    .halign(Align::Center)
                    .valign(Align::Center)
                    .build();
                li.set_child(Some(&lbl));
            });
            factory.connect_bind(|_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li.item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else { return };
                let entry = boxed.borrow::<EditorEntry>();
                let t = &entry.track;
                let path = std::path::Path::new(&t.path);
                // Missing == the file is gone, mirroring the macOS/FFI
                // `file_missing` flag. `id == 0` only means "not catalogued";
                // an uncatalogued file that exists is a normal playable track.
                let missing  = !path.exists();
                let readonly = !missing && crate::media_library::is_read_only(path);
                let glyph = if missing { "⚠" } else if readonly { "🔒" } else { "" };
                if let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) {
                    lbl.set_label(glyph);
                }
            });
            let col = ColumnViewColumn::new(Some(""), Some(factory));
            col.set_fixed_width(24);
            track_list.append_column(&col);
        }

        // Position column (editor-only) — shows the 1-based playlist slot
        // resolved against the canonical play order in `editing_tracks`.
        // Pinned: fixed width, no resize/reorder.  Sorter is installed
        // below so clicking the header toggles position ASC/DESC.
        {
            let pos_factory = gtk4::SignalListItemFactory::new();
            pos_factory.connect_setup(|_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() { return }
                let lbl = Label::builder()
                    .halign(Align::End)
                    .xalign(1.0)
                    .margin_start(6).margin_end(6)
                    .css_classes(["pl-duration"])
                    .build();
                li.set_child(Some(&lbl));
            });
            pos_factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li.item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else { return };
                let entry = boxed.borrow::<EditorEntry>();
                let text = (entry.canonical_idx + 1).to_string();
                if let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) {
                    lbl.set_label(&text);
                }
            });
            let pos_col = ColumnViewColumn::new(Some("#"), Some(pos_factory));
            pos_col.set_fixed_width(48);
            pos_col.set_resizable(false);
            // Canonical-order sorter: compare each entry's slot directly.
            let sorter = CustomSorter::new(move |a, b| {
                let pa = a.downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .unwrap_or(usize::MAX);
                let pb = b.downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .unwrap_or(usize::MAX);
                pa.cmp(&pb).into()
            });
            pos_col.set_sorter(Some(&sorter));
            track_list.append_column(&pos_col);
            *pos_col_holder.borrow_mut() = Some(pos_col);
        }

        for c in ALL_COLUMNS.iter() {
            let id_str = c.id.to_string();
            let factory = gtk4::SignalListItemFactory::new();

            let setup_sel        = edit_multi_sel.clone();
            let setup_state      = state.clone();
            let setup_ctx_id     = ctx_canonical_idx.clone();
            let setup_et         = editing_tracks.clone();
            let setup_drag_sel   = drag_selection.clone();
            let setup_ra         = reorder_allowed.clone();
            // rebuild_track_list isn't yet defined at this point of the
            // outer scope, so capture the Rc via a deferred holder filled
            // immediately after the rebuild closure is created.
            let setup_rebuild    = rebuild_track_list_holder.clone();
            let setup_scroll     = track_scroll_holder.clone();
            let setup_ed_ctx_idx    = ed_ctx_indices.clone();
            let setup_drives     = current_drives.clone();
            let setup_devices    = current_devices.clone();
            let setup_id         = id_str.clone();
            let is_artwork_col   = id_str == "artwork_path";
            let setup_cells      = artwork_cells.clone();
            let bind_cells       = artwork_cells.clone();
            // F12.2: separate clone for connect_bind — setup_state above is
            // moved into connect_setup.
            let bind_state       = state.clone();
            factory.connect_setup(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() { return }
                // Artwork column gets the shared thumbnail cell instead of a
                // Label. Drag-source / drop-target / right-click gesture
                // attach to the Button just like they would to a Label (both
                // are Widget).
                let child: gtk4::Widget = if setup_id == "artwork_path" {
                    setup_cells.setup().upcast::<gtk4::Widget>()
                } else {
                    let lbl = Label::builder()
                        .margin_start(6).margin_end(6)
                        .margin_top(3).margin_bottom(3)
                        .hexpand(true).vexpand(true)
                        .halign(Align::Fill).valign(Align::Fill)
                        .xalign(0.0)
                        .ellipsize(gtk4::pango::EllipsizeMode::End)
                        .build();
                    lbl.upcast::<gtk4::Widget>()
                };
                let lbl = child.clone();
                let _ = is_artwork_col;

                // Per-cell DropTarget — handles intra-editor reorder.  When
                // the source drag originated in the editor (drag_selection
                // populated) and the current sort allows reorder, splice
                // those canonical rows to this cell's canonical slot.
                // Drops from other windows (drag_selection empty) fall
                // through to the outer track_scroll DropTarget which
                // appends the external paths.
                {
                    let dt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
                    let dt_li      = li.clone();
                    let dt_et      = setup_et.clone();
                    let dt_ra      = setup_ra.clone();
                    let dt_dragsel = setup_drag_sel.clone();
                    let dt_rebuild = setup_rebuild.clone();
                    dt.connect_drop(move |_, value, _, _| {
                        if !dt_ra.get() { return false }
                        // Reject the drop unless the drag originated in
                        // the editor itself — otherwise let the outer
                        // track_scroll DropTarget handle external add.
                        let src_indices: Vec<usize> = dt_dragsel.borrow().clone();
                        if src_indices.is_empty() { return false }
                        // Validate we still received the expected number
                        // of paths (sanity check; not used for indices).
                        if value.get::<gdk::FileList>().is_err() { return false }

                        // Resolve drop slot directly from this cell's
                        // EditorEntry so duplicate paths in the playlist
                        // collapse to the correct row, not the first one.
                        let Some(dst_canon) = dt_li.item()
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                        else { return false };

                        // Splice in canonical order: remove src indices
                        // highest-first, then re-insert in original order
                        // at the adjusted destination.
                        let mut sorted = src_indices.clone();
                        sorted.sort_unstable_by(|a, b| b.cmp(a));
                        let mut adjusted_dst = dst_canon;
                        let mut removed: Vec<crate::media_library::LibTrack> = Vec::new();
                        {
                            let mut et = dt_et.borrow_mut();
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

                        // No write here — see the same note in playlists.rs. Edits stay in
                        // `editing_tracks` until Save (2026-08-10).

                        // Drag completed — clear selection so a stray
                        // subsequent drop (e.g. external) doesn't reorder.
                        dt_dragsel.borrow_mut().clear();

                        // Defer rebuild to next idle tick so we don't
                        // splice the backing ListStore while GTK is still
                        // unwinding the drop event chain — splicing mid-
                        // drop segfaults on some GTK4 versions.
                        if let Some(rb) = dt_rebuild.borrow().as_ref().cloned() {
                            glib::idle_add_local_once(move || rb());
                        }
                        true
                    });
                    lbl.add_controller(dt);
                }

                // Per-cell DragSource — ships every currently-selected editor
                // row as a FileList so the user can drag tracks out of the
                // playlist editor into the active playlist (pl_scroll accepts
                // FileList).  Single-row drag works too: if the row under
                // the pointer is not in the selection it still ships its
                // own path.
                {
                    let ds = gtk4::DragSource::new();
                    ds.set_actions(gtk4::gdk::DragAction::COPY);
                    let ds_sel       = setup_sel.clone();
                    let ds_li        = li.clone();
                    let ds_dragsel   = setup_drag_sel.clone();
                    ds.connect_prepare(move |_, _, _| {
                        // Clear any stale canonical indices from a prior
                        // drag, then record this drag's selection by
                        // canonical_idx so duplicates of the same path
                        // resolve to the correct rows on reorder.
                        ds_dragsel.borrow_mut().clear();
                        let mut paths: Vec<std::path::PathBuf> = Vec::new();
                        let mut indices: Vec<usize> = Vec::new();
                        let mut self_entry: Option<(std::path::PathBuf, usize)> = None;
                        if let Some(obj) = ds_li.item()
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            let entry = obj.borrow::<EditorEntry>();
                            self_entry = Some((
                                std::path::PathBuf::from(&entry.track.path),
                                entry.canonical_idx,
                            ));
                        }
                        for i in 0..ds_sel.n_items() {
                            if ds_sel.is_selected(i) {
                                if let Some(obj) = ds_sel.item(i)
                                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                {
                                    let entry = obj.borrow::<EditorEntry>();
                                    paths.push(std::path::PathBuf::from(&entry.track.path));
                                    indices.push(entry.canonical_idx);
                                }
                            }
                        }
                        if paths.is_empty() {
                            if let Some((p, i)) = self_entry {
                                paths.push(p);
                                indices.push(i);
                            }
                        }
                        if paths.is_empty() { return None }
                        *ds_dragsel.borrow_mut() = indices;
                        let files: Vec<gio::File> = paths.iter()
                            .map(|p| gio::File::for_path(p))
                            .collect();
                        let fl = gdk::FileList::from_array(&files);
                        Some(gdk::ContentProvider::for_value(&fl.to_value()))
                    });
                    lbl.add_controller(ds);
                }

                // Per-cell right-click gesture. Builds a real gio::Menu +
                // PopoverMenu (NESTED), same as the Files view and the
                // device-tracks view — see the big comment above
                // `ed_ctx_indices`/`ed_selected_tracks` for why this now works
                // where Task 8's flat-button popover was a workaround: the
                // "ed" action group lives on `track_scroll` (installed once,
                // not per-cell) and the popover is parented on that same
                // widget, so action lookup never has to walk the ColumnView
                // cell tree at all.
                let gesture = gtk4::GestureClick::new();
                gesture.set_button(gtk4::gdk::BUTTON_SECONDARY);
                let g_sel        = setup_sel.clone();
                let g_state      = setup_state.clone();
                let g_ctx_id     = setup_ctx_id.clone();
                let g_li         = li.clone();
                let g_lbl        = lbl.clone();
                let g_scroll     = setup_scroll.clone();
                let g_ed_ctx_idx = setup_ed_ctx_idx.clone();
                let g_drives     = setup_drives.clone();
                let g_devices    = setup_devices.clone();
                gesture.connect_pressed(move |g, _n, x, y| {
                    let Some(scroll_widget) = g_scroll.borrow().clone() else {
                        return;
                    };
                    let Some(item) = g_li.item() else {
                        return;
                    };
                    let item_clone = item.clone();
                    let mut clicked_idx: Option<u32> = None;
                    for i in 0..g_sel.n_items() {
                        if g_sel.item(i).as_ref() == Some(&item_clone) {
                            clicked_idx = Some(i);
                            break;
                        }
                    }
                    if let Some(idx) = clicked_idx {
                        if !g_sel.is_selected(idx) {
                            g_sel.unselect_all();
                            g_sel.select_item(idx, true);
                        }
                    }
                    // Stash this row's canonical play-order slot so the
                    // single-row actions (edit-id3) operate on the exact
                    // row that was clicked even when the playlist lists
                    // duplicates of the same path.
                    let (cidx, is_lib_track) = item.downcast_ref::<glib::BoxedAnyObject>()
                        .map(|o| {
                            let e = o.borrow::<EditorEntry>();
                            (e.canonical_idx as i64, e.track.id > 0)
                        })
                        .unwrap_or((-1, false));
                    g_ctx_id.set(cidx);

                    let sel_count: usize = (0..g_sel.n_items())
                        .filter(|i| g_sel.is_selected(*i)).count();

                    // Gather canonical indices the row-scoped actions
                    // (Replace / Edit ID3 / Remove) operate on — selection
                    // first, falling back to the single clicked row when
                    // nothing is selected — and stash them once per
                    // right-click. send-active/add-to-new/add-to-saved/
                    // send-drive/send-device instead read the live
                    // selection straight off `edit_multi_sel` at dispatch
                    // (`ed_selected_tracks`), since they're also reachable
                    // from the "Send to ▾" button, which never fires this
                    // gesture.
                    let mut idxs: Vec<usize> = (0..g_sel.n_items())
                        .filter(|i| g_sel.is_selected(*i))
                        .filter_map(|i| g_sel.item(i))
                        .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                        .collect();
                    if idxs.is_empty() {
                        let c = g_ctx_id.get();
                        if c >= 0 { idxs.push(c as usize); }
                    }
                    *g_ed_ctx_idx.borrow_mut() = idxs.clone();

                    // ── Build the real menu model ---------------------
                    // Order: Send to · Replace · ─ · ID3 · Album Art · Lyrics ·
                    // ─ · Remove from Playlist. Matches the macOS editor menu
                    // (MLPlaylistEditor.editorContextMenu).
                    let send = build_send_to_menu(
                        &g_state,
                        &SendToActions {
                            active: "ed.send-active",
                            new_playlist: "ed.add-to-new",
                            saved_playlist: "ed.add-to-saved",
                            drive: "ed.send-drive",
                            device: "ed.send-device",
                            drives: g_drives.borrow().iter()
                                .map(|d| (d.id.clone(), d.label.clone())).collect(),
                            devices: g_devices.borrow().iter()
                                .map(|d| (d.id.clone(), d.label.clone())).collect(),
                        },
                    );
                    let menu = gio::Menu::new();
                    menu.append_submenu(Some("↪ Send to"), &send);
                    menu.append_item(&gio::MenuItem::new(
                        Some("♻ Replace Current Playlist"),
                        Some("ed.replace"),
                    ));
                    // ID3 / Album Art / Lyrics — single library row only.
                    if is_lib_track && sel_count <= 1 {
                        menu.append_item(&gio::MenuItem::new(
                            Some("🎵 View/Edit ID3"),
                            Some("ed.edit-id3"),
                        ));
                        menu.append_item(&gio::MenuItem::new(
                            Some("🖼 View Album Art"),
                            Some("ed.view-art"),
                        ));
                        menu.append_item(&gio::MenuItem::new(
                            Some("📝 View/Search Lyrics"),
                            Some("ed.lyrics"),
                        ));
                    }
                    menu.append_item(&gio::MenuItem::new(
                        Some("✕ Remove from Playlist"),
                        Some("ed.remove"),
                    ));

                    let popover = context_popover(&menu);
                    // EXACT mirror of the working Files-view context menu
                    // (~line 3630): parent the popover on the same widget the
                    // "ed" action group is installed on (track_scroll), and
                    // do NOT unparent on close. An earlier `connect_closed(||
                    // unparent)` severed the widget-tree link to the group as
                    // a nested item dispatched, so ed.send-drive never fired
                    // (2026-07-16). The Files menu leaves its popover parented
                    // too; matching that is what makes nested dispatch work.
                    let (px, py) = g_lbl
                        .translate_coordinates(&scroll_widget, x, y)
                        .unwrap_or((x, y));
                    let rect = gtk4::gdk::Rectangle::new(px as i32, py as i32, 1, 1);
                    popover.set_parent(&scroll_widget);
                    popover.set_pointing_to(Some(&rect));
                    popover.popup();
                    g.set_state(gtk4::EventSequenceState::Claimed);
                });
                lbl.add_controller(gesture);

                li.set_child(Some(&lbl));
            });

            let bind_id = id_str.clone();
            factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li.item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else { return };
                let entry = boxed.borrow::<EditorEntry>();
                let t = &entry.track;
                // F12.2: read live so a Settings toggle applies to
                // already-bound cells on the next rebind, not just at
                // window construction (the ML window is a singleton — see
                // rebuild_ml_callback in player.rs).
                let artist_as_album_artist =
                    bind_state.borrow().config.media_library.artist_as_album_artist;
                // Stash this cell's canonical play-order index on whatever
                // child widget the cell currently holds so the editor-area
                // drop target can resolve a drop coordinate to a canonical
                // insert position via track_list.pick(x, y) → walk_up →
                // parse "pos:<N>".  Works for both Label and Button cells.
                if let Some(c) = li.child() {
                    c.set_widget_name(&format!("pos:{}", entry.canonical_idx));
                }
                // Artwork column gets the Button affordance, mirroring the
                // files view.  Click opens the cached cover-art image.
                if bind_id == "artwork_path" {
                    bind_cells.bind(li, t.artwork_path.as_deref(), |li| {
                        li.item()
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            .and_then(|b| b.borrow::<EditorEntry>().track.artwork_path.clone())
                    });
                    return;
                }
                let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok())
                else { return };
                let text = match bind_id.as_str() {
                    "num" => t.track_num.map(|n| n.to_string()).unwrap_or_default(),
                    "title" => t.title.as_deref().unwrap_or(&t.filename).to_string(),
                    "artist" => t.artist.as_deref().unwrap_or("").to_string(),
                    "album" => t.album.as_deref().unwrap_or("").to_string(),
                    // F12.2: falls back to artist when the album-artist tag
                    // is blank and the toggle is on. A4 (phase 11 album
                    // gallery) MUST also use this helper.
                    "album_artist" => crate::play_stats::effective_album_artist(
                        t.artist.as_deref().unwrap_or(""),
                        t.album_artist.as_deref().unwrap_or(""),
                        artist_as_album_artist,
                    ),
                    "duration" => t.length_secs
                        .map(|s| { let ss = s as u64; format!("{}:{:02}", ss/60, ss%60) })
                        .unwrap_or_else(|| "-:--".to_string()),
                    "filename" => t.filename.clone(),
                    "year" => t.year.map(|y| y.to_string()).unwrap_or_default(),
                    "genre" => t.genre.as_deref().unwrap_or("").to_string(),
                    "bitrate" => t.bitrate.map(|b| format!("{b}k")).unwrap_or_default(),
                    "channels" => match t.channels.unwrap_or(0) {
                        1 => "mono".to_string(),
                        2 => "stereo".to_string(),
                        n => format!("{}ch", n),
                    },
                    "path" => t.path.clone(),
                    "play_count" => t.play_count.to_string(),
                    "last_played" => format_last_played(t.last_played.as_deref().unwrap_or("")),
                    "last_scanned" => t.last_scanned.as_deref().unwrap_or("").to_string(),
                    "disc_num" => {
                        let d = t.disc_num.unwrap_or(0);
                        if d == 0 { String::new() }
                        else if let Some(total) = t.disc_total {
                            if total > 0 { format!("{}/{}", d, total) } else { d.to_string() }
                        } else { d.to_string() }
                    }
                    "disc_total" => t.disc_total.map(|d| d.to_string()).unwrap_or_default(),
                    "composer" => t.composer.as_deref().unwrap_or("").to_string(),
                    "original_artist" => t.original_artist.as_deref().unwrap_or("").to_string(),
                    "copyright" => t.copyright.as_deref().unwrap_or("").to_string(),
                    "url" => t.url.as_deref().unwrap_or("").to_string(),
                    "encoded_by" => t.encoded_by.as_deref().unwrap_or("").to_string(),
                    "bpm" => t.bpm.as_deref().unwrap_or("").to_string(),
                    "lyric" => truncate_display(t.lyric.as_deref().unwrap_or(""), 30),
                    "comment" => t.comment.as_deref().unwrap_or("").to_string(),
                    "artwork_path" => if t.artwork_path.is_some() { "Yes".to_string() } else { String::new() },
                    _ => String::new(),
                };
                lbl.set_text(&gtk_safe(&text));
                // Unavailable file → broken color, mirroring the macOS
                // editor's red rows for missing files. Existence — not library
                // membership — decides this, so an uncatalogued but present
                // file shows normally.
                let missing = !std::path::Path::new(&t.path).exists();
                if missing {
                    lbl.add_css_class("broken");
                } else {
                    lbl.remove_css_class("broken");
                }
            });

            let col = ColumnViewColumn::new(Some(c.header), Some(factory));
            col.set_resizable(true);
            if c.expand { col.set_expand(true); }
            col.set_visible(visible_ids.contains(&id_str));
            if let Some(&w) = saved_widths.get(&id_str) {
                if w > 0 { col.set_fixed_width(w); }
            }

            // Display-only sorter — sort is applied via SortListModel so
            // `editing_tracks` (canonical play order) is never mutated.
            let sort_id = id_str.clone();
            let sorter = CustomSorter::new(move |a, b| {
                let a_val = a
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<EditorEntry>().track, &sort_id))
                    .unwrap_or_default();
                let b_val = b
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<EditorEntry>().track, &sort_id))
                    .unwrap_or_default();
                a_val.cmp(&b_val).into()
            });
            col.set_sorter(Some(&sorter));
            track_list.append_column(&col);
            editor_named_cols.push((id_str, col));
        }

        // Apply the files-view saved column order so the editor matches
        // it — the user only arranges columns once.  Columns not present
        // in saved_order keep their default position at the tail.
        let saved_order = state.borrow().config.media_library.ml_file_col_order.clone();
        if !saved_order.is_empty() {
            for (_, col) in editor_named_cols.iter() {
                track_list.remove_column(col);
            }
            // Position 0 = status glyph, 1 = position; named columns start at 2.
            let mut pos = 2u32;
            for col_id in &saved_order {
                if let Some((_, col)) = editor_named_cols.iter()
                    .find(|(id, _)| id == col_id)
                {
                    track_list.insert_column(pos, col);
                    pos += 1;
                }
            }
            for (id, col) in editor_named_cols.iter() {
                if !saved_order.contains(id) {
                    track_list.insert_column(pos, col);
                    pos += 1;
                }
            }
        }
    }
    // Allow drag-reorder of editor column headers — same affordance as
    // the files view.  Pinned columns (status + position) remain in
    // place because they aren't reorderable individually; GTK keeps them
    // in their declared positions.
    track_list.set_reorderable(true);

    // Shared closure that re-applies the files-view column state
    // (visibility, widths, order) to the editor's ColumnView.  Called
    // every time a saved playlist is loaded so the editor mirrors the
    // user's latest customization without needing a full ML reopen.
    let editor_cols_rc: Rc<Vec<(String, ColumnViewColumn)>> =
        Rc::new(editor_named_cols);
    let apply_editor_columns: Rc<dyn Fn()> = {
        let cols = editor_cols_rc.clone();
        let state_rc = state.clone();
        let tl = track_list.clone();
        // 2 pinned leading columns: status glyph + position.
        Rc::new(move || apply_ml_columns_to(&tl, cols.as_slice(), &state_rc, 2))
    };

    // Connect the sort model to the ColumnView's column-driven sorter so
    // header clicks produce a display sort.  Then listen for sorter changes
    // and update `reorder_allowed` — true when the active sort is "position
    // ASC" or no sort, false for any other column / order.
    {
        let sorter = track_list.sorter();
        edit_sort_model.set_sorter(sorter.as_ref());
        if let Some(s) = sorter {
            let pos_holder = pos_col_holder.clone();
            let ra = reorder_allowed.clone();
            let update = move |s: &gtk4::Sorter| {
                let pos_col = pos_holder.borrow().clone();
                let allowed = if let Some(cv_sorter) =
                    s.downcast_ref::<gtk4::ColumnViewSorter>()
                {
                    let primary = cv_sorter.primary_sort_column();
                    let order   = cv_sorter.primary_sort_order();
                    match (primary, pos_col) {
                        (None, _) => true, // default sort = canonical
                        (Some(pc), Some(target)) =>
                            pc == target && order == gtk4::SortType::Ascending,
                        _ => false,
                    }
                } else {
                    true
                };
                ra.set(allowed);
            };
            update(&s);
            s.connect_changed(move |s, _| update(s));
        }
    }

    Columns {
        apply_editor_columns,
        ple_action_group_holder,
        rebuild_track_list_holder,
        track_scroll_holder,
    }
}
