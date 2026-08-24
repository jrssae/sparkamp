//! The saved-playlist manager — the `pl-manage` sub-page — and the
//! load-a-playlist-into-the-editor seam it shares with the sidebar.
//!
//! Split from [`super::playlists`] (plan step 7). Three things that all move
//! together because they are the same job from three directions:
//!
//! - **`load_pl_by_id`** — read a playlist out of the library into editing
//!   state and render it. Called from the manage list, from the sidebar's
//!   playlist sub-rows, and after a rename or a save-as.
//! - **`add_pl_sub_row`** — add one playlist to *both* the sidebar sub-rows
//!   and the manage list, so the two never drift.
//! - **the `pl-manage` page** — the full-width list plus New, Rename and
//!   Delete.
//!
//! **Deletion Rule**: deleting a playlist removes its `.m3u8` from disk and
//! nothing else. Its tracks stay in the library and their audio stays where
//! it is (see CLAUDE.md).
//!
//! The two editor buttons built at the end look out of place here and are
//! not: `btn_rename_pl_inline` and `btn_save_pl_outer` live in the editor's
//! header and button rows, but the manage page's rename and save-as paths
//! drive their sensitivity, so they are declared where both sides can reach
//! them and handed back to the caller.

use gtk4::prelude::*;
use gtk4::{gio, Align, Box as GtkBox, Button, Entry, Label, ListBox, ListBoxRow,
    Orientation, PolicyType, ScrolledWindow, Stack};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::sidebar::{self, Sidebar};
use super::{
    attach_pl_row_drag, gtk_safe, make_view_search_row,
    run_playlist_save_dialog, show_playlist_save_error,
    sidebar_pl_end_index, MlCtx, EDITOR_CURRENT_REFRESH_HOOK, EDITOR_REFRESH_HOOK,
    PLAYLIST_NAV_REFRESH_HOOK,
};

/// What the manager needs from the page around it.
pub(super) struct ManageUi<'a> {
    /// The playlist sub-stack, so opening one switches to the editor.
    pub pl_sub_stack: &'a Rc<Stack>,
    /// The manage page's own list of saved playlists.
    pub pl_manage_list: &'a Rc<ListBox>,
    /// Editing state the load seam fills.
    pub editing_pl_id: &'a Rc<Cell<i64>>,
    pub editing_tracks: &'a Rc<RefCell<Vec<crate::media_library::LibTrack>>>,
    /// The saved baseline a Revert restores to, and the dirty check.
    pub saved_track_ids: &'a Rc<RefCell<Vec<i64>>>,
    /// Re-render the editor table, and re-apply the shared column config.
    pub rebuild_track_list: &'a Rc<dyn Fn()>,
    pub apply_editor_columns: &'a Rc<dyn Fn()>,
    /// The editor's inline error line.
    pub edit_error_label: &'a Label,
    /// The editor's search entry, cleared (or restored) on load.
    pub pl_search_entry: &'a Entry,
}

/// What the rest of the page needs back.
pub(super) struct Manage {
    /// Open a playlist by database id into the editor.
    pub load_pl_by_id: Rc<dyn Fn(i64)>,
    /// The editor header's rename button and the button row's Save.
    pub btn_rename_pl_inline: Button,
    pub btn_save_pl_outer: Button,
    /// The editor header's title and path labels.
    pub edit_header: Label,
    pub edit_path_label: Label,
    /// Shown beside the name when the playlist file can't be written.
    pub edit_ro_badge: Label,
}

/// Build the `pl-manage` page and the load seam.
pub(super) fn build(ctx: &MlCtx, sb: &Sidebar, ui: ManageUi<'_>) -> Manage {
    // Local names for what this takes from its context and its caller, so the
    // body below reads as it did inside `playlists::build`.
    let state = ctx.host.state.clone();
    let win = ctx.win.clone();
    let sidebar = sb.list.clone();
    let pl_sub_rows = sb.pl_sub_rows.clone();
    let playlists_expanded = sb.playlists_expanded.clone();
    let pl_sub_stack = ui.pl_sub_stack.clone();
    let pl_manage_list = ui.pl_manage_list.clone();
    let editing_pl_id = ui.editing_pl_id.clone();
    let editing_tracks = ui.editing_tracks.clone();
    let saved_track_ids = ui.saved_track_ids.clone();
    let rebuild_track_list = ui.rebuild_track_list.clone();
    let apply_editor_columns = ui.apply_editor_columns.clone();
    let edit_error_label = ui.edit_error_label.clone();
    let pl_search_entry = ui.pl_search_entry.clone();

    // ── Helper: load a playlist by DB id into editing state ───────────────
    // ── Editor header widgets ────────────────────────────────────────────
    // Declared before `load_pl_by_id` because that closure is the one place
    // that fills them in: every route into the editor — a sidebar sub-row,
    // the manage list, a rename, a Save As, a Revert — goes through it, so
    // the header, the path bar, the read-only badge and Save's sensitivity
    // can never disagree about which playlist is open.
    let edit_header: Label = Label::builder()
        .label("Playlist Editor")
        .halign(Align::Start)
        .hexpand(true)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .margin_start(8).margin_top(4).margin_bottom(0)
        .build();
    edit_header.add_css_class("ml-section-header");

    let btn_rename_pl_inline: Button = {
        let b = Button::with_label("Rename");
        b.add_css_class("pl-btn");
        b.set_margin_end(8);
        b.set_margin_top(2);
        b
    };

    // Read-only badge, beside the playlist's name. Shown when the playlist
    // file itself can't be written, which is the only thing that now stops
    // Save — the file's location does not (2026-08-10).
    let edit_ro_badge: Label = Label::new(Some("🔒 Read-only"));
    edit_ro_badge.add_css_class("badge");
    edit_ro_badge.set_margin_end(8);
    edit_ro_badge.set_visible(false);
    edit_ro_badge.set_tooltip_text(Some(
        "This playlist file is read-only, so Save is unavailable. \
         Use Save As to write an editable copy.",
    ));

    // File path bar — shows where the .m3u lives, which Save now writes to
    // in place.
    let edit_path_label: Label = Label::builder()
        .label("")
        .halign(Align::Start)
        .margin_start(8).margin_top(0).margin_bottom(4)
        .ellipsize(gtk4::pango::EllipsizeMode::Middle)
        .selectable(true)
        .build();
    edit_path_label.add_css_class("status-label");

    let btn_save_pl_outer: Button = {
        let b = Button::with_label("Save");
        b.add_css_class("pl-btn");
        b
    };

    let load_pl_by_id: Rc<dyn Fn(i64)> = {
        let state_rc   = state.clone();
        let et         = editing_tracks.clone();
        let saved      = saved_track_ids.clone();
        let rebuild    = rebuild_track_list.clone();
        let ep_id      = editing_pl_id.clone();
        let apply_cols = apply_editor_columns.clone();
        let err_lbl    = edit_error_label.clone();
        let search     = pl_search_entry.clone();
        let hdr_lbl    = edit_header.clone();
        let path_lbl   = edit_path_label.clone();
        let ro_badge   = edit_ro_badge.clone();
        let save_btn   = btn_save_pl_outer.clone();
        Rc::new(move |id: i64| {
            ep_id.set(id);
            // Header, path bar, read-only badge and Save's sensitivity, all
            // from one place so no route into the editor can leave them
            // describing a different playlist than the one loaded below.
            if let Some(ref lib) = state_rc.borrow().media_lib {
                if let Ok(pl) = lib.playlist_by_id(id) {
                    hdr_lbl.set_text(&gtk_safe(&pl.name));
                    path_lbl.set_text(&gtk_safe(&pl.path));
                }
                // Save writes the playlist's own file wherever it lives; only
                // a read-only file stops it.
                let writable = lib.playlist_is_writable(id);
                save_btn.set_sensitive(writable);
                ro_badge.set_visible(!writable);
            }
            // A previous playlist's search query must not filter this one —
            // but F12.1: if remember_search is on, restore the "playlists"
            // view's saved query instead of clearing.
            if state_rc.borrow().config.media_library.remember_search {
                let last = state_rc
                    .borrow()
                    .config
                    .media_library
                    .last_search
                    .get("playlists")
                    .cloned();
                search.set_text(last.as_deref().unwrap_or(""));
            } else {
                search.set_text("");
            }
            // Re-apply files-view column state so customizations made
            // while the editor was elsewhere take effect immediately.
            apply_cols();
            let loaded = state_rc
                .borrow()
                .media_lib
                .as_ref()
                .map(|lib| {
                    lib.playlist_by_id(id)
                        .and_then(|pl| lib.load_playlist_tracks(&pl))
                });
            let tracks = match loaded {
                Some(Ok(tracks)) => {
                    err_lbl.set_visible(false);
                    tracks
                }
                Some(Err(e)) => {
                    // Playlist entries live only in the .m3u8 file, so an
                    // unreadable file means there is nothing to show — say
                    // why instead of presenting a silently empty playlist.
                    err_lbl.set_text(&gtk_safe(&format!(
                        "This playlist has not been scanned yet and its \
                         file is not accessible from here ({e:#})."
                    )));
                    err_lbl.set_visible(true);
                    Vec::new()
                }
                None => {
                    err_lbl.set_visible(false);
                    Vec::new()
                }
            };
            let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();
            *et.borrow_mut() = tracks;
            *saved.borrow_mut() = ids;
            rebuild();
        })
    };

    // Register the editor-refresh hook so any cross-window add-to-saved
    // action that targets the currently-open playlist reloads the editor.
    {
        let load = load_pl_by_id.clone();
        let ep_id = editing_pl_id.clone();
        let hook: Rc<dyn Fn(i64)> = Rc::new(move |target_pid: i64| {
            if ep_id.get() == target_pid {
                load(target_pid);
            }
        });
        EDITOR_REFRESH_HOOK.with(|h| *h.borrow_mut() = Some(hook));
    }
    // Refresh-current hook: reloads whatever playlist is open in the
    // editor.  Fired after a track is recorded as played so the editor
    // mirrors the files view's updated metadata + unread state.
    {
        let load = load_pl_by_id.clone();
        let ep_id = editing_pl_id.clone();
        let hook: Rc<dyn Fn()> = Rc::new(move || {
            let id = ep_id.get();
            if id >= 0 { load(id); }
        });
        EDITOR_CURRENT_REFRESH_HOOK.with(|h| *h.borrow_mut() = Some(hook));
    }
    // Nav-refresh hook: re-sync the playlist sidebar sub-rows and the
    // manage list with the playlists table after a playlist is created
    // from another window (e.g. active-playlist "Add to new playlist").
    {
        let state_rc     = state.clone();
        let sidebar_ref  = sidebar.clone();
        let sub_rows_ref = pl_sub_rows.clone();
        let expanded_ref = playlists_expanded.clone();
        let manage_ref   = pl_manage_list.clone();
        let hook: Rc<dyn Fn()> = Rc::new(move || {
            let playlists = state_rc
                .borrow()
                .media_lib
                .as_ref()
                .and_then(|lib| lib.all_playlists().ok())
                .unwrap_or_default();

            // Remember the selected sidebar playlist (if any) so the
            // rebuild doesn't visually drop the user's place.
            let selected = sidebar_ref
                .selected_row()
                .map(|r| r.widget_name().to_string());

            // Clear both lists, then rebuild from the playlists table.
            // Sidebar sub-rows are tracked in `pl_sub_rows`, so drain that;
            // the manage list isn't tracked, so empty it by index.
            for row in sub_rows_ref.borrow_mut().drain(..) {
                sidebar_ref.remove(&row);
            }
            while let Some(row) = manage_ref.row_at_index(0) {
                manage_ref.remove(&row);
            }

            // Insert the rebuilt rows right after the Playlists header — not at
            // the sidebar end, which is below the Devices section.
            let mut insert_at = {
                let mut idx = 0i32;
                let mut after = 1i32;
                while let Some(r) = sidebar_ref.row_at_index(idx) {
                    if r.widget_name() == "playlists" {
                        after = idx + 1;
                        break;
                    }
                    idx += 1;
                }
                after
            };

            for pl in &playlists {
                let s_lbl = Label::builder()
                    .label(&pl.name)
                    .halign(Align::Start)
                    .xalign(0.0)
                    .margin_start(sidebar::SUB_ROW_INSET).margin_end(8)
                    .margin_top(4).margin_bottom(4)
                    .build();
                let s_row = ListBoxRow::new();
                s_row.set_widget_name(&format!("pl:{}", pl.id));
                s_row.set_child(Some(&s_lbl));
                s_row.set_visible(expanded_ref.get());
                attach_pl_row_drag(&s_row, pl.id);
                sidebar_ref.insert(&s_row, insert_at);
                insert_at += 1;
                if selected.as_deref() == Some(s_row.widget_name().as_str()) {
                    sidebar_ref.select_row(Some(&s_row));
                }
                sub_rows_ref.borrow_mut().push(s_row);

                let m_lbl = Label::builder()
                    .label(&pl.name)
                    .halign(Align::Start)
                    .margin_start(8).margin_end(8)
                    .margin_top(3).margin_bottom(3)
                    .build();
                let m_row = ListBoxRow::new();
                m_row.set_widget_name(&pl.id.to_string());
                m_row.set_child(Some(&m_lbl));
                attach_pl_row_drag(&m_row, pl.id);
                manage_ref.append(&m_row);
            }
        });
        PLAYLIST_NAV_REFRESH_HOOK.with(|h| *h.borrow_mut() = Some(hook));
    }

    // ── Helper: add a sub-row to both the sidebar and pl_manage_list ──────
    // Returns the sidebar row so the caller can select it.
    let _add_pl_sidebar_row = {
        let sidebar_ref  = sidebar.clone();
        let sub_rows_ref = pl_sub_rows.clone();
        let expanded_ref = playlists_expanded.clone();
        Rc::new(move |id: i64, name: &str| -> ListBoxRow {
            // Sidebar sub-row
            let s_lbl = Label::builder()
                .label(name)
                .halign(Align::Start)
                .xalign(0.0)
                .margin_start(sidebar::SUB_ROW_INSET).margin_end(8)
                .margin_top(4).margin_bottom(4)
                .build();
            let s_row = ListBoxRow::new();
            s_row.set_widget_name(&format!("pl:{}", id));
            s_row.set_child(Some(&s_lbl));
            s_row.set_visible(expanded_ref.get());
            attach_pl_row_drag(&s_row, id);
            sidebar_ref.append(&s_row);
            sub_rows_ref.borrow_mut().push(s_row.clone());
            s_row
        })
    };

    // ── Build "pl-manage" page ────────────────────────────────────────────
    {
        let manage_vbox = GtkBox::new(Orientation::Vertical, 0);

        // ── Search ───────────────────────────────────────────────────────
        // Every other list in the Media Library has one; this page never got
        // it, which stopped mattering somewhere below a dozen playlists and
        // started mattering well before forty (2026-08-10).
        //
        // A `ListBox` filters through `set_filter_func` rather than the
        // `FilterListModel` the ColumnView pages use, and it applies to rows
        // added later too — so a playlist created while a query is active
        // obeys it without any extra wiring.
        let (pl_search_row, pl_manage_search) = make_view_search_row("Search playlists…");
        // Marks the entry Ctrl+F should focus when the manage sub-page is the
        // visible one — see the widget-name walk in media_library.rs.
        pl_manage_search.set_widget_name("ml-search-entry");
        manage_vbox.append(&pl_search_row);
        let manage_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        {
            let q = manage_query.clone();
            pl_manage_list.set_filter_func(move |row| {
                let needle = q.borrow();
                if needle.is_empty() {
                    return true;
                }
                row.child()
                    .and_then(|c| c.downcast::<Label>().ok())
                    .map(|l| l.label().to_lowercase().contains(needle.as_str()))
                    .unwrap_or(true)
            });
        }
        {
            let q = manage_query.clone();
            let list = pl_manage_list.clone();
            pl_manage_search.connect_changed(move |e| {
                *q.borrow_mut() = e.text().to_lowercase();
                list.invalidate_filter();
            });
        }

        // Populate the manage list from DB
        let playlists_initial = state
            .borrow()
            .media_lib
            .as_ref()
            .and_then(|lib| lib.all_playlists().ok())
            .unwrap_or_default();
        for pl in &playlists_initial {
            let lbl = Label::builder()
                .label(&pl.name)
                .halign(Align::Start)
                .margin_start(8).margin_end(8)
                .margin_top(3).margin_bottom(3)
                .build();
            let row = ListBoxRow::new();
            row.set_widget_name(&pl.id.to_string());
            row.set_child(Some(&lbl));
            attach_pl_row_drag(&row, pl.id);
            pl_manage_list.append(&row);
        }

        let manage_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .child(&*pl_manage_list)
            .build();
        manage_vbox.append(&manage_scroll);

        // Clicking a manage-list row → select its sidebar sub-row
        {
            let sidebar_ref   = sidebar.clone();
            let pl_sub_ref    = pl_sub_stack.clone();
            pl_manage_list.connect_row_selected(move |_, opt_row| {
                let row = match opt_row { Some(r) => r, None => return };
                let id_str = row.widget_name().to_string();
                // Find matching sidebar "pl:ID" row and select it
                let target = format!("pl:{}", id_str);
                let mut i = 0i32;
                loop {
                    match sidebar_ref.row_at_index(i) {
                        Some(sr) if sr.widget_name() == target => {
                            sidebar_ref.select_row(Some(&sr));
                            break;
                        }
                        Some(_) => { i += 1; }
                        None => break,
                    }
                }
                // Also switch sub-stack directly (sidebar handler may not fire
                // if the row is already selected)
                pl_sub_ref.set_visible_child_name("pl-edit");
            });
        }

        // Manage list bottom buttons: New / Rename / Delete
        let manage_btn_row = GtkBox::new(Orientation::Horizontal, 4);
        manage_btn_row.set_margin_start(4);
        manage_btn_row.set_margin_end(4);
        manage_btn_row.set_margin_top(4);
        manage_btn_row.set_margin_bottom(4);

        let btn_new_pl    = Button::with_label("+ New");
        btn_new_pl.add_css_class("pl-btn");
        let btn_rename_pl = Button::with_label("Rename");
        btn_rename_pl.add_css_class("pl-btn");
        btn_rename_pl.set_sensitive(false);
        let btn_delete_pl = Button::with_label("Delete");
        btn_delete_pl.add_css_class("pl-btn");
        btn_delete_pl.set_sensitive(false);

        manage_btn_row.append(&btn_new_pl);
        manage_btn_row.append(&btn_rename_pl);
        manage_btn_row.append(&btn_delete_pl);
        manage_vbox.append(&manage_btn_row);

        // Enable/disable rename+delete based on manage list selection
        {
            let btn_ren = btn_rename_pl.clone();
            let btn_del = btn_delete_pl.clone();
            pl_manage_list.connect_row_selected(move |_, opt| {
                let has = opt.is_some();
                btn_ren.set_sensitive(has);
                btn_del.set_sensitive(has);
            });
        }

        // ── New playlist ──────────────────────────────────────────────────
        {
            let state_rc      = state.clone();
            let pl_list_ref   = pl_manage_list.clone();
            let sidebar_ref   = sidebar.clone();
            let sub_rows_ref  = pl_sub_rows.clone();
            let expanded_ref  = playlists_expanded.clone();
            let pl_sub_ref    = pl_sub_stack.clone();
            let load          = load_pl_by_id.clone();
            let win_wk        = win.downgrade();
            btn_new_pl.connect_clicked(move |_| {
                let Some(win) = win_wk.upgrade() else { return };
                let state2  = state_rc.clone();
                let pl_ref2 = pl_list_ref.clone();
                let sid2    = sidebar_ref.clone();
                let sub2    = sub_rows_ref.clone();
                let exp2    = expanded_ref.clone();
                let pls2    = pl_sub_ref.clone();
                let load2   = load.clone();
                // Save dialog replaces the previous name-only popup —
                // user picks BOTH the filename and the target folder so
                // the new playlist no longer lands silently in Sparkamp's
                // managed `~/.config/sparkamp/playlists/` directory (which
                // had the side effect of registering itself as a watched
                // folder via `add_playlist_file`).
                run_playlist_save_dialog(state_rc.clone(), win, "New Playlist", move |path, win_cb| {
                    let name = path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Untitled")
                        .to_string();
                    let save_result = state2.borrow().media_lib.as_ref()
                        .map(|lib| lib.save_playlist_tracks_to_path(&path, &[]));
                    let new_id = match save_result {
                        Some(Ok(id)) => id,
                        Some(Err(e)) => {
                            eprintln!("save_playlist_tracks_to_path: {e}");
                            show_playlist_save_error(&win_cb, &path, &e);
                            return;
                        }
                        None => return,
                    };

                    // Add to manage list
                    let row_lbl = Label::builder().label(&name)
                        .halign(Align::Start)
                        .margin_start(8).margin_end(8)
                        .margin_top(3).margin_bottom(3).build();
                    let manage_row = ListBoxRow::new();
                    manage_row.set_widget_name(&new_id.to_string());
                    manage_row.set_child(Some(&row_lbl));
                    attach_pl_row_drag(&manage_row, new_id);
                    pl_ref2.append(&manage_row);
                    pl_ref2.select_row(Some(&manage_row));

                    // Add sidebar sub-row and select it
                    let s_lbl = Label::builder().label(&name)
                        .halign(Align::Start)
                        .xalign(0.0)
                        .margin_start(sidebar::SUB_ROW_INSET).margin_end(8)
                        .margin_top(4).margin_bottom(4).build();
                    let s_row = ListBoxRow::new();
                    s_row.set_widget_name(&format!("pl:{}", new_id));
                    s_row.set_child(Some(&s_lbl));
                    s_row.set_visible(exp2.get());
                    attach_pl_row_drag(&s_row, new_id);
                    sid2.insert(&s_row, sidebar_pl_end_index(&sid2));
                    sub2.borrow_mut().push(s_row.clone());
                    sid2.select_row(Some(&s_row));

                    load2(new_id);
                    pls2.set_visible_child_name("pl-edit");
                });
            });
        }

        // ── Rename playlist ───────────────────────────────────────────────
        {
            let state_rc    = state.clone();
            let pl_list_ref = pl_manage_list.clone();
            let sidebar_ref = sidebar.clone();
            let win_wk      = win.downgrade();
            btn_rename_pl.connect_clicked(move |_| {
                let sel_row = match pl_list_ref.selected_row() { Some(r) => r, None => return };
                let id = match sel_row.widget_name().to_string().parse::<i64>() {
                    Ok(v) => v, Err(_) => return,
                };
                let current = sel_row.child()
                    .and_then(|c| c.downcast::<Label>().ok())
                    .map(|l| l.text().to_string()).unwrap_or_default();

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
                let dialog_btns = GtkBox::new(Orientation::Horizontal, 6);
                dialog_btns.set_halign(Align::End);
                let cancel_btn = Button::with_label("Cancel");
                let ok_btn     = Button::with_label("Rename");
                ok_btn.add_css_class("suggested-action");
                dialog_btns.append(&cancel_btn); dialog_btns.append(&ok_btn);
                vbox.append(&lbl); vbox.append(&name_entry); vbox.append(&dialog_btns);
                dialog.set_child(Some(&vbox));
                let d = dialog.clone();
                cancel_btn.connect_clicked(move |_| { d.close(); });
                let d        = dialog.clone();
                let e        = name_entry.clone();
                let state2   = state_rc.clone();
                let sel2     = sel_row.clone();
                let sid2     = sidebar_ref.clone();
                ok_btn.connect_clicked(move |_| {
                    let name = e.text().to_string();
                    if name.is_empty() { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.rename_playlist(id, &name);
                    }
                    // Update manage-list label
                    if let Some(c) = sel2.child() {
                        if let Ok(l) = c.downcast::<Label>() { l.set_text(&gtk_safe(&name)); }
                    }
                    // Update sidebar sub-row label
                    let target = format!("pl:{}", id);
                    let mut i = 0i32;
                    loop {
                        match sid2.row_at_index(i) {
                            Some(sr) if sr.widget_name() == target => {
                                if let Some(c) = sr.child() {
                                    if let Ok(l) = c.downcast::<Label>() {
                                        l.set_text(&gtk_safe(&name));
                                    }
                                }
                                break;
                            }
                            Some(_) => { i += 1; }
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

        // ── Delete playlist ───────────────────────────────────────────────
        {
            let state_rc    = state.clone();
            let pl_list_ref = pl_manage_list.clone();
            let sidebar_ref = sidebar.clone();
            let sub_rows_ref = pl_sub_rows.clone();
            let pl_sub_ref  = pl_sub_stack.clone();
            let et          = editing_tracks.clone();
            let saved       = saved_track_ids.clone();
            let rebuild     = rebuild_track_list.clone();
            let win_wk      = win.downgrade();
            btn_delete_pl.connect_clicked(move |_| {
                let sel_row = match pl_list_ref.selected_row() { Some(r) => r, None => return };
                let id = match sel_row.widget_name().to_string().parse::<i64>() {
                    Ok(v) => v, Err(_) => return,
                };
                let pl_name = sel_row.child()
                    .and_then(|c| c.downcast::<Label>().ok())
                    .map(|l| l.text().to_string()).unwrap_or_default();

                let dialog = gtk4::AlertDialog::builder()
                    .message(format!("Delete \"{}\"?", pl_name))
                    .detail("The playlist file on disk is not deleted.")
                    .buttons(vec!["Cancel".to_string(), "Delete".to_string()])
                    .cancel_button(0).default_button(1).modal(true).build();

                let state2    = state_rc.clone();
                let pl_ref2   = pl_list_ref.clone();
                let sid2      = sidebar_ref.clone();
                let sub2      = sub_rows_ref.clone();
                let pls2      = pl_sub_ref.clone();
                let sel2      = sel_row.clone();
                let et2       = et.clone();
                let saved2    = saved.clone();
                let rebuild2  = rebuild.clone();
                dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |result| {
                    if result != Ok(1) { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.remove_playlist(id);
                    }
                    // Remove from manage list
                    pl_ref2.remove(&sel2);
                    // Remove sidebar sub-row
                    let target = format!("pl:{}", id);
                    let mut sub = sub2.borrow_mut();
                    sub.retain(|r| {
                        if r.widget_name() == target { sid2.remove(r); false } else { true }
                    });
                    // Go back to manage page
                    et2.borrow_mut().clear();
                    saved2.borrow_mut().clear();
                    rebuild2();
                    pls2.set_visible_child_name("pl-manage");
                });
            });
        }

        pl_sub_stack.add_named(&manage_vbox, Some("pl-manage"));
    }

    Manage {
        load_pl_by_id,
        btn_rename_pl_inline,
        btn_save_pl_outer,
        edit_header,
        edit_path_label,
        edit_ro_badge,
    }
}
