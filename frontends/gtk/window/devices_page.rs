//! The Media Library's "Devices" page — removable storage.
//!
//! Child module of [`super`] (window.rs), extracted from
//! `open_media_library_window` by plan step 6. It owns the device overview
//! cards, the device detail view (capacity meter, playlist chips, track list),
//! the 2 s udisks2 poll that keeps both and the sidebar's sub-rows live, and
//! the wiring for scan / sync / eject / copy / device-playlist management.
//!
//! Device *logic* — detection, mount points, tag reads, the copy and sync
//! helpers — lives in `crate::devices` and in the sibling `devices.rs` slice.
//! This file is the page: the widgets, and the closures that drive them.
//!
//! ## Why it is one function
//!
//! Same reason as [`super::disc_page`]: the closures capture each other, the
//! widgets are declared first and the wiring last, and a closure can only
//! capture a local declared above it. Until step 6 those two halves sat
//! ~3,200 lines apart in `media_library.rs` with Files, Albums, Playlists and
//! the whole Disc Drives page between them; the widgets moved down to meet
//! the wiring so both could travel here together.
//!
//! ## What the rest of the window sees
//!
//! Nothing, directly. This page defines no name any other page reads. The two
//! runners it owns that others call — copy loose files onto a device, and
//! send a whole playlist to one — are published by filling
//! `ctx.host.copy_files_holder` and `sb.send_playlist_holder`, the holder
//! pattern from docs/gtk-breakup-plan.md §3.1. That indirection is what lets
//! this page be built *after* the Files page and the playlist editor that
//! call into it. **A holder left `None` is not an error, it is a silent
//! no-op** — smoke-test group X is what catches that.

use gtk4::prelude::*;
use gtk4::{
    gdk, gio, glib, Align, Box as GtkBox, Button, ColumnView, ColumnViewColumn, CustomSorter,
    DropTarget, Entry, EventControllerKey, Image, Label, ListBoxRow, MultiSelection, Orientation,
    PolicyType, ScrolledWindow, SignalListItemFactory, SortListModel,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

// `sidebar` is imported as a module (for its `SUB_ROW_INSET`) as well as by
// type — the `let sidebar` binding below shadows only the value namespace, so
// `sidebar::…` still resolves.
use super::sidebar::{self, Sidebar};
// The ID3 editor and the artwork viewer, both opened from the device row menu.
use super::art_window;
// Everything else is private to the parent module, which a child may still
// use. Three groups: shared Media Library chrome (columns, status bar, search
// row, popovers, sidebar row lookup), the device helpers from the `devices.rs`
// slice, and the playlist-sync helpers the Sync button drives.
use super::{
    apply_card_progress, apply_device_sync, apply_ml_columns_to, apply_playlist_pull,
    apply_playlist_push, attach_cell_context_menu, build_send_to_menu, build_tag_conflicts,
    context_popover, counts_text, device_delete_files, device_fs_unsupported, device_glyph_prefix,
    device_icon_name, device_io_shutting_down, device_m3u_remove_basenames, device_plan_fs,
    device_plan_one, device_playlist_sync_plan, device_record_pair, device_recorded_relpath,
    device_sync_id, device_sync_plan, find_row_by_name, gtk_safe, invalidate_mtp_meta,
    lib_track_matches_query, linked_library_playlist, make_view_search_row, ml_cell_text,
    ml_sort_key, ml_status_bar, notify_playlist_changed, notify_playlist_nav_refresh,
    open_id3_editor_window, open_image_viewer, prepare_playlist_send, prompt_playlist_conflicts,
    prompt_tag_conflicts, queue_paths_to_drive, refresh_device_cache, run_playlist_save_dialog,
    safe_playlist_filename, set_button_busy, set_levelbar_fullness, show_alert_parented,
    show_playlist_save_error, unsupported_device_banner, view_or_search_lyrics, DeviceRefreshOutcome,
    LyricsMode, MlColumnDef, MlCtx, PlaylistSyncItem, SendToActions, ALL_COLUMNS,
    UNSUPPORTED_FS_TOOLTIP,
};

/// Build the Devices page and attach it to `ctx.stack` under the name
/// `"devices"`.
///
/// Takes `sb` as well as `ctx` because three of the cells this page drives —
/// its sidebar sub-rows, the Devices chevron state and the send-a-playlist
/// holder the editor calls through — are built by `sidebar.rs` and handed
/// straight through. They are touched by this page alone, so by the plan's
/// §3.2 test they do not belong on [`MlCtx`].
pub(super) fn build(ctx: &MlCtx, sb: &Sidebar) {
    // Local names for what this page takes from its context, so the body below
    // reads exactly as it did inside `open_media_library_window`. Same device
    // steps 1–5 used: cloning an `Rc` is an integer increment, and rewriting
    // several hundred capture sites would bury a move in an unreviewable diff.
    let state = ctx.host.state.clone();
    let rebuild_playlist = ctx.host.rebuild_playlist.clone();
    let current_drives = ctx.host.current_drives.clone();
    let current_devices = ctx.host.current_devices.clone();
    let burn_queues = ctx.host.burn_queues.clone();
    let copy_files_holder = ctx.host.copy_files_holder.clone();
    let burn_refresh_holder = ctx.host.burn_refresh_holder.clone();
    let win = ctx.win.clone();
    let stack = ctx.stack.clone();
    let sidebar = sb.list.clone();
    let devices_expanded = sb.devices_expanded.clone();
    let dev_sub_rows = sb.dev_sub_rows.clone();
    let send_playlist_holder = sb.send_playlist_holder.clone();

    // Per-device (song, playlist) counts for the overview cards, keyed by
    // backend_id. Computed off-thread on first show and cleared whenever a
    // device's contents change (see reload_device_store). `counts_in_flight`
    // guards against spawning the same count walk twice.
    let device_counts: Rc<RefCell<std::collections::HashMap<String, (usize, usize)>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    let counts_in_flight: Rc<RefCell<std::collections::HashSet<String>>> =
        Rc::new(RefCell::new(std::collections::HashSet::new()));

    // Live copy progress per device (backend_id → (done, total)); absent = idle.
    // `device_card_progress` maps a backend_id to its overview card's progress
    // bar (rebuilt each overview render). Together they let a copy show progress
    // on the card and survive a poll-driven rebuild mid-transfer.
    let device_transfers: Rc<RefCell<std::collections::HashMap<String, (usize, usize)>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    let device_card_progress: Rc<RefCell<std::collections::HashMap<String, gtk4::ProgressBar>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));

    // Apply (or clear) a transfer's progress to a card's bar. The bar always
    // occupies its space; idle just makes it transparent so the card never
    // changes size between copying and not.
    let update_card_progress: Rc<dyn Fn(&str, Option<(usize, usize)>)> = {
        let transfers = device_transfers.clone();
        let bars = device_card_progress.clone();
        Rc::new(move |backend: &str, state: Option<(usize, usize)>| {
            match state {
                Some(v) => {
                    transfers.borrow_mut().insert(backend.to_string(), v);
                }
                None => {
                    transfers.borrow_mut().remove(backend);
                }
            }
            if let Some(bar) = bars.borrow().get(backend) {
                apply_card_progress(bar, state);
            }
        })
    };

    // ── Devices content page widgets (added to the stack below) ───────────
    let dev_page = GtkBox::new(Orientation::Vertical, 8);
    dev_page.set_margin_top(8);
    dev_page.set_margin_start(8);
    dev_page.set_margin_end(8);

    // Diagnostics banner — shown only when udisks2 can't be reached.
    let dev_banner = GtkBox::new(Orientation::Horizontal, 8);
    dev_banner.set_visible(false);
    let dev_banner_lbl = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .build();
    dev_banner_lbl.add_css_class("broken");
    let dev_banner_retry = Button::with_label("Retry");
    dev_banner_retry.add_css_class("pl-btn");
    dev_banner.append(&dev_banner_lbl);
    dev_banner.append(&dev_banner_retry);
    dev_page.append(&dev_banner);

    // ── Overview: a live list of all connected devices (shown when the
    // Devices header is selected). ───────────────────────────────────────
    let dev_overview = GtkBox::new(Orientation::Vertical, 6);
    let dev_overview_title = Label::builder()
        .label("Devices")
        .halign(Align::Start)
        .xalign(0.0)
        .build();
    dev_overview_title.add_css_class("ml-section-header");
    dev_overview.append(&dev_overview_title);
    let dev_overview_list = GtkBox::new(Orientation::Vertical, 12);
    dev_overview_list.set_margin_top(6);
    dev_overview.append(&dev_overview_list);
    dev_page.append(&dev_overview);

    // ── Detail: the selected device (hidden until one is picked) ─────────
    let dev_detail = GtkBox::new(Orientation::Vertical, 8);
    dev_detail.set_visible(false);

    // Header band: device icon · name + (filesystem · path) · status badges ·
    // Sync / Eject. Populated by the device-select handler.
    let dev_icon = Image::from_icon_name("drive-removable-media");
    dev_icon.set_pixel_size(40);
    dev_icon.set_valign(Align::Center);

    let dev_title = Label::builder().halign(Align::Start).xalign(0.0).build();
    dev_title.add_css_class("device-detail-name");
    // Filesystem + mount path subtitle (selectable so the path can be copied).
    let dev_path = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .selectable(true)
        .ellipsize(gtk4::pango::EllipsizeMode::Middle)
        .build();
    dev_path.add_css_class("status-label");
    // Unsupported-filesystem tag sits under the "fs · path" line on the left,
    // left-aligned and a touch smaller than the read-only pill.
    let dev_warn_badge = Label::new(Some("⚠ Unsupported"));
    dev_warn_badge.add_css_class("device-badge");
    dev_warn_badge.add_css_class("device-badge-warn");
    dev_warn_badge.add_css_class("device-badge-sm");
    dev_warn_badge.set_halign(Align::Start);
    dev_warn_badge.set_margin_top(4);
    dev_warn_badge.set_tooltip_text(Some(UNSUPPORTED_FS_TOOLTIP));
    dev_warn_badge.set_visible(false);

    let dev_title_box = GtkBox::new(Orientation::Vertical, 0);
    dev_title_box.set_valign(Align::Center);
    dev_title_box.append(&dev_title);
    dev_title_box.append(&dev_path);
    dev_title_box.append(&dev_warn_badge);

    let dev_ro_badge = Label::new(Some("🔒 Read-only"));
    dev_ro_badge.add_css_class("device-badge");
    dev_ro_badge.set_valign(Align::Center);
    dev_ro_badge.set_visible(false);

    let dev_scan = Button::with_label("Scan");
    dev_scan.add_css_class("pl-btn");
    dev_scan.set_valign(Align::Center);
    dev_scan.set_tooltip_text(Some("Re-read tags + duration from the files on this device"));
    dev_scan.set_sensitive(false);
    let dev_sync = Button::with_label("Sync");
    dev_sync.add_css_class("pl-btn");
    dev_sync.set_valign(Align::Center);
    dev_sync.set_sensitive(false);
    let dev_eject = Button::with_label("Eject");
    dev_eject.add_css_class("pl-btn");
    dev_eject.set_valign(Align::Center);
    dev_eject.set_sensitive(false);

    // Capacity meter — capacity bar + used/free/total text. Lives in the header
    // band (between the name/path and the Sync/Eject buttons) to save vertical
    // space, taking the flexible middle column.
    let dev_levelbar = gtk4::LevelBar::new();
    dev_levelbar.set_min_value(0.0);
    dev_levelbar.set_max_value(1.0);
    dev_levelbar.add_css_class("device-capacity");
    dev_levelbar.set_valign(Align::Center);
    let dev_capacity = Label::builder().halign(Align::Start).xalign(0.0).build();
    dev_capacity.add_css_class("status-label");
    dev_capacity.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    // Third row of the capacity area: "X playlists - Y audio files".
    let dev_counts = Label::builder().halign(Align::Start).xalign(0.0).build();
    dev_counts.add_css_class("status-label");
    dev_counts.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    let dev_capacity_box = GtkBox::new(Orientation::Vertical, 2);
    dev_capacity_box.set_hexpand(true);
    dev_capacity_box.set_valign(Align::Center);
    // Triple the breathing room on either side of the capacity bar.
    dev_capacity_box.set_margin_start(30);
    dev_capacity_box.set_margin_end(30);
    dev_capacity_box.append(&dev_levelbar);
    dev_capacity_box.append(&dev_capacity);
    dev_capacity_box.append(&dev_counts);

    let dev_hdr_row = GtkBox::new(Orientation::Horizontal, 10);
    dev_hdr_row.add_css_class("device-detail-header");
    dev_hdr_row.append(&dev_icon);
    dev_hdr_row.append(&dev_title_box);
    dev_hdr_row.append(&dev_capacity_box);
    dev_hdr_row.append(&dev_ro_badge);
    dev_hdr_row.append(&dev_scan);
    dev_hdr_row.append(&dev_sync);
    dev_hdr_row.append(&dev_eject);
    dev_detail.append(&dev_hdr_row);

    // Copy progress bar — shown only while files are being copied to this
    // device; carries an "x/y · filename" label.
    // Thick accent bar matching the capacity bar above; the live "Copying x/y ·
    // filename" text rides in the status bar (`dev_hint`), so the bar itself
    // carries no inline text and can be slim/tall like the capacity meter.
    let dev_progress = gtk4::ProgressBar::new();
    dev_progress.set_show_text(false);
    dev_progress.set_visible(false);
    dev_progress.add_css_class("device-progress");
    dev_detail.append(&dev_progress);

    // Caution banner for a connected device with no readable filesystem (an
    // MTP phone whose storage isn't shared). Shown in place of the playlist and
    // file lists, which are hidden while it is up.
    let dev_nofs_banner = GtkBox::new(Orientation::Horizontal, 8);
    dev_nofs_banner.set_visible(false);
    dev_nofs_banner.set_margin_top(12);
    dev_nofs_banner.set_margin_bottom(12);
    let dev_nofs_lbl = Label::builder()
        .label(
            "⚠ No visible filesystem on this device. Set the phone to file-transfer \
             mode and allow access, or reconnect it, then press Scan.",
        )
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .build();
    dev_nofs_lbl.add_css_class("broken");
    dev_nofs_banner.append(&dev_nofs_lbl);
    dev_detail.append(&dev_nofs_banner);

    // Playlists section header: a "Playlists" label on the left and an always-
    // available "+ New" button on the right that creates a device-only playlist.
    let dev_pl_header_lbl = Label::builder()
        .label("Playlists")
        .halign(Align::Start)
        .xalign(0.0)
        .hexpand(true)
        .build();
    dev_pl_header_lbl.add_css_class("ml-section-header");
    let dev_pl_new = Button::with_label("+ New");
    dev_pl_new.add_css_class("pl-btn");
    let dev_pl_header = GtkBox::new(Orientation::Horizontal, 6);
    dev_pl_header.append(&dev_pl_header_lbl);
    dev_pl_header.append(&dev_pl_new);
    dev_detail.append(&dev_pl_header);
    // Filter chips: "All files" + one toggle per device .m3u/.m3u8 (grouped so
    // exactly one is active, radio-style). Rebuilt per device by
    // reload_dev_playlists; the active chip drives the track filter.
    // Chips wrap onto multiple rows (no horizontal scroll that hid the names).
    let dev_pl_chips = gtk4::FlowBox::builder()
        .orientation(Orientation::Horizontal)
        .selection_mode(gtk4::SelectionMode::None)
        .row_spacing(4)
        .column_spacing(4)
        .min_children_per_line(1)
        .max_children_per_line(64)
        .homogeneous(false)
        .build();
    dev_pl_chips.add_css_class("device-chips");
    dev_pl_chips.set_valign(Align::Start);
    let dev_pl_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        // One chip row when there's a single row; grow as chips wrap, up to
        // ~2.5 rows before a vertical scrollbar appears. (No propagate-natural-
        // height: the FlowBox over-estimates row count and would inflate to the
        // max even for a single row.)
        .min_content_height(34)
        .max_content_height(80)
        .child(&dev_pl_chips)
        .build();
    dev_pl_scroll.set_vexpand(false);
    dev_detail.append(&dev_pl_scroll);

    // Per-playlist management actions — shown only when a specific playlist chip
    // (not "All files") is selected. Click handlers are wired further down, once
    // the device run-closures they depend on exist. A device playlist linked to
    // a library playlist (same safe name) is renamed via the library; a
    // device-only playlist is acted on in place.
    let dev_pl_rename = Button::with_label("Rename");
    let dev_pl_duplicate = Button::with_label("Duplicate");
    let dev_pl_delete = Button::with_label("Delete");
    for b in [&dev_pl_rename, &dev_pl_duplicate, &dev_pl_delete] {
        b.add_css_class("pl-btn");
    }
    dev_pl_delete.add_css_class("destructive");
    let dev_pl_actions = GtkBox::new(Orientation::Horizontal, 6);
    dev_pl_actions.append(&dev_pl_rename);
    dev_pl_actions.append(&dev_pl_duplicate);
    dev_pl_actions.append(&dev_pl_delete);
    dev_pl_actions.set_visible(false);
    dev_detail.append(&dev_pl_actions);
    // The device playlist file the active chip points at (None = "All files").
    let selected_dev_playlist: Rc<RefCell<Option<std::path::PathBuf>>> =
        Rc::new(RefCell::new(None));

    // Delete/Remove button for the device track view, created early so the
    // playlist filter can flip its label. It is placed into the bottom action
    // row further down. Label is "Delete" in the all-files view (delete off the
    // device + drop from every playlist) and "Remove" in a playlist view (drop
    // from that one playlist, keep the file). Disabled until files are selected.
    let dev_file_remove = Button::with_label("Delete");
    dev_file_remove.add_css_class("pl-btn");
    dev_file_remove.add_css_class("destructive");
    dev_file_remove.set_sensitive(false);

    // Live copy status ("Copying x/y · filename"). Empty when idle, so it acts
    // as the flexible spacer in the bottom action row (no dedicated status bar,
    // which left an empty strip at the bottom of the view).
    let dev_hint = Label::builder()
        .label("")
        .halign(Align::Start)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .build();
    dev_hint.add_css_class("status-label");
    // Kept for the selection handler's unsupported-fs note; not shown directly
    // (the title-section badge now carries that), so it stays unparented.
    let dev_warn = Label::builder()
        .halign(Align::End)
        .xalign(1.0)
        .visible(false)
        .build();
    dev_warn.add_css_class("broken");

    // Track view mirroring the files-view columns, populated from device tags.
    // `dev_store` is the *displayed* model: in the all-files view it holds every
    // device file; in a playlist view it holds that playlist's entries in order,
    // duplicates included (a playlist may reference the same file more than
    // once). `dev_all_tracks` caches the full device file list so switching
    // views doesn't re-scan the device.
    let dev_store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let dev_all_tracks: Rc<RefCell<Vec<crate::media_library::LibTrack>>> =
        Rc::new(RefCell::new(Vec::new()));
    // Device file path → the library file it was copied from (its sync pair), for
    // the device view's "Synced from" column so the user can see exactly which
    // computer file each device file is kept in step with. Rebuilt per device by
    // reload_device_store; read live by the column factory.
    let dev_pair_map: Rc<RefCell<std::collections::HashMap<String, String>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    // Per-view search over whatever the store currently shows (all files or
    // one playlist): store → filter → sort → selection, so every fill site
    // stays filter-oblivious and copy/delete still act on the selection.
    let dev_search_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let dev_filter = gtk4::CustomFilter::new({
        let q = dev_search_query.clone();
        move |obj| {
            let Some(boxed) = obj.downcast_ref::<glib::BoxedAnyObject>() else {
                return true;
            };
            lib_track_matches_query(&boxed.borrow::<crate::media_library::LibTrack>(), &q.borrow())
        }
    });
    let dev_filter_model =
        gtk4::FilterListModel::new(Some(dev_store.clone()), Some(dev_filter.clone()));
    // Search filters just this device view's rows (all-files or the shown
    // playlist). Created here so reload_device_store can clear it when a
    // different device opens; packed above the track table below.
    let (dev_search_row, dev_search_entry) =
        make_view_search_row("Search this device — artist, title, album…");
    // F12.1: restore this view's last search query if the feature is on.
    if state.borrow().config.media_library.remember_search {
        let last = state.borrow().config.media_library.last_search.get("devices").cloned();
        if let Some(last) = last {
            dev_search_entry.set_text(&last);
        }
    }
    {
        // 150 ms debounce: the filter re-scans every row's text fields, so
        // re-running it per keystroke stutters on large device libraries.
        let q = dev_search_query.clone();
        let filter = dev_filter.clone();
        let state_rc = state.clone();
        let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        dev_search_entry.connect_changed(move |e| {
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
                            .insert("devices".to_string(), raw_text.clone());
                    }
                }
                pending_inner.borrow_mut().take();
                glib::ControlFlow::Break
            });
            *pending.borrow_mut() = Some(src);
        });
    }
    let dev_sort_model = SortListModel::new(Some(dev_filter_model), None::<gtk4::Sorter>);
    let dev_selection = MultiSelection::new(Some(dev_sort_model.clone()));
    let dev_col_view = ColumnView::new(Some(dev_selection.clone()));
    // The device row context menu, filled in further down once its action
    // group and menu model exist. Cells are built before that but each needs
    // to reach it — the holder pattern (docs/gtk-breakup-plan.md §3.1).
    let dev_row_menu_holder: Rc<RefCell<Option<Rc<dyn Fn(f64, f64)>>>> =
        Rc::new(RefCell::new(None));
    dev_col_view.add_css_class("ml-col-view");
    dev_col_view.set_hexpand(true);
    dev_col_view.set_vexpand(true);

    // Playlist-order column (front): shown only while a playlist filter is
    // active, then made the default sort — like the editor's position column.
    let dev_pos_col = {
        let sel_ctx = dev_selection.clone();
        let anchor_ctx = dev_col_view.clone();
        let holder_ctx = dev_row_menu_holder.clone();
        let factory = SignalListItemFactory::new();
        factory.connect_setup(move |_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            if li.child().is_some() {
                return;
            }
            let lbl = Label::builder()
                .halign(Align::End)
                .xalign(1.0)
                .margin_start(6)
                .margin_end(6)
                .css_classes(["pl-duration"])
                .build();
            li.set_child(Some(&lbl));
            // Right-click has to be handled per cell: ColumnView has no
            // `row_at_y`, so a ScrolledWindow-level gesture cannot tell which
            // row it hit and the menu did nothing until a left-click had
            // already selected something (2026-08-10).
            attach_cell_context_menu(
                li,
                lbl.upcast_ref(),
                &sel_ctx,
                anchor_ctx.upcast_ref(),
                {
                    let holder = holder_ctx.clone();
                    move |x, y| {
                        let f = holder.borrow().clone();
                        if let Some(f) = f {
                            f(x, y);
                        }
                    }
                },
            );
        });
        // The playlist view holds entries in order (no sort), so the row's
        // position in the model is its 1-based playlist position. Each duplicate
        // entry is its own row and gets its own number.
        factory.connect_bind(move |_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) else {
                return;
            };
            lbl.set_text(&(li.position() + 1).to_string());
        });
        let col = ColumnViewColumn::new(Some("#"), Some(factory));
        col.set_fixed_width(48);
        col.set_visible(false);
        dev_col_view.append_column(&col);
        col
    };

    // "Synced from" column (device view only): the library file each device file
    // was copied from. Lets the user confirm at a glance which computer file a
    // sync keeps in step, instead of guessing among same-named files. Reads the
    // live per-device pair map keyed by on-device path.
    {
        let pair_map = dev_pair_map.clone();
        let sel_ctx = dev_selection.clone();
        let anchor_ctx = dev_col_view.clone();
        let holder_ctx = dev_row_menu_holder.clone();
        let factory = SignalListItemFactory::new();
        factory.connect_setup(move |_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            if li.child().is_some() {
                return;
            }
            let lbl = Label::builder()
                .halign(Align::Start)
                .xalign(0.0)
                .margin_start(6)
                .margin_end(6)
                .ellipsize(gtk4::pango::EllipsizeMode::Middle)
                .css_classes(["status-label"])
                .build();
            li.set_child(Some(&lbl));
            // Right-click has to be handled per cell: ColumnView has no
            // `row_at_y`, so a ScrolledWindow-level gesture cannot tell which
            // row it hit and the menu did nothing until a left-click had
            // already selected something (2026-08-10).
            attach_cell_context_menu(
                li,
                lbl.upcast_ref(),
                &sel_ctx,
                anchor_ctx.upcast_ref(),
                {
                    let holder = holder_ctx.clone();
                    move |x, y| {
                        let f = holder.borrow().clone();
                        if let Some(f) = f {
                            f(x, y);
                        }
                    }
                },
            );
        });
        factory.connect_bind(move |_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) else {
                return;
            };
            let Some(item) = li.item() else { return };
            let Some(boxed) = item.downcast_ref::<glib::BoxedAnyObject>() else {
                return;
            };
            let path = boxed.borrow::<crate::media_library::LibTrack>().path.clone();
            match pair_map.borrow().get(&path) {
                Some(libp) => {
                    let base = std::path::Path::new(libp)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(libp);
                    lbl.set_text(&gtk_safe(base));
                    lbl.set_tooltip_text(Some(&gtk_safe(libp)));
                }
                None => {
                    lbl.set_text("—");
                    lbl.set_tooltip_text(Some("Not synced from this computer"));
                }
            }
        });
        let col = ColumnViewColumn::new(Some("Synced from"), Some(factory));
        col.set_fixed_width(220);
        col.set_resizable(true);
        dev_col_view.append_column(&col);
    }

    let mut dev_named_cols: Vec<(String, ColumnViewColumn)> = Vec::new();
    // Buttons that already have a click handler wired (artwork "View"), so the
    // device factory connects each button instance only once.
    let dev_connected_artwork: Rc<RefCell<std::collections::HashSet<glib::Object>>> =
        Rc::new(RefCell::new(std::collections::HashSet::new()));
    {
        // Columns that are library bookkeeping, not ID3 tags — irrelevant for a
        // device, so never shown here even if visible in the files view.
        const DEVICE_HIDDEN_COLS: &[&str] = &["play_count", "last_played", "last_scanned"];
        let visible_ids: Vec<String> =
            state.borrow().config.media_library.visible_columns.clone();
        let widths: std::collections::HashMap<String, i32> =
            state.borrow().config.media_library.ml_file_col_widths.clone();
        let order = state.borrow().config.media_library.ml_file_col_order.clone();
        // Build columns in the saved order (unknown/leftover ids appended).
        let ordered: Vec<&MlColumnDef> = {
            let mut v: Vec<&MlColumnDef> = Vec::new();
            for id in &order {
                if let Some(c) = ALL_COLUMNS.iter().find(|c| &c.id == id) {
                    v.push(c);
                }
            }
            for c in ALL_COLUMNS.iter() {
                if !order.iter().any(|id| id == c.id) {
                    v.push(c);
                }
            }
            v
        };
        for c in ordered {
            if DEVICE_HIDDEN_COLS.contains(&c.id) {
                continue;
            }
            let id_str = c.id.to_string();
            let is_art = c.id == "artwork_path";
            let sel_ctx = dev_selection.clone();
            let anchor_ctx = dev_col_view.clone();
            let holder_ctx = dev_row_menu_holder.clone();
            let factory = SignalListItemFactory::new();
            factory.connect_setup(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() {
                    return;
                }
                // Artwork column shows a "View" button (mirrors the files view),
                // every other column a plain label.
                let child: gtk4::Widget = if is_art {
                    let btn = Button::with_label("View");
                    btn.add_css_class("link");
                    btn.set_halign(Align::Start);
                    btn.set_margin_start(4);
                    btn.set_margin_end(4);
                    btn.set_visible(false);
                    btn.upcast::<gtk4::Widget>()
                } else {
                    Label::builder()
                        .halign(Align::Start)
                        .xalign(0.0)
                        .margin_start(6)
                        .margin_end(6)
                        .ellipsize(gtk4::pango::EllipsizeMode::End)
                        .css_classes(["ml-col-label"])
                        .build()
                        .upcast::<gtk4::Widget>()
                };
                li.set_child(Some(&child));
                // Right-click has to be handled per cell: ColumnView has no
                // `row_at_y`, so a ScrolledWindow-level gesture cannot tell which
                // row it hit and the menu did nothing until a left-click had
                // already selected something (2026-08-10).
                attach_cell_context_menu(
                    li,
                    &child,
                    &sel_ctx,
                    anchor_ctx.upcast_ref(),
                    {
                        let holder = holder_ctx.clone();
                        move |x, y| {
                            let f = holder.borrow().clone();
                            if let Some(f) = f {
                                f(x, y);
                            }
                        }
                    },
                );
            });
            let bind_id = id_str.clone();
            let bind_connected = dev_connected_artwork.clone();
            let bind_state = state.clone();
            factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li
                    .item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else {
                    return;
                };
                let t = boxed.borrow::<crate::media_library::LibTrack>();
                // F12.2: read live so a Settings toggle applies to already
                // -bound cells on the next rebind, not just at window
                // construction (the ML window is a singleton — see
                // rebuild_ml_callback in player.rs).
                let artist_as_album_artist =
                    bind_state.borrow().config.media_library.artist_as_album_artist;
                if is_art {
                    let Some(btn) = li.child().and_then(|c| c.downcast::<Button>().ok()) else {
                        return;
                    };
                    if let Some(ref art_path) = t.artwork_path {
                        btn.set_visible(true);
                        let btn_obj = btn.clone().upcast::<glib::Object>();
                        if !bind_connected.borrow().contains(&btn_obj) {
                            bind_connected.borrow_mut().insert(btn_obj);
                            let art = art_path.clone();
                            btn.connect_clicked(move |_| open_image_viewer(&art));
                        }
                    } else {
                        btn.set_visible(false);
                    }
                    return;
                }
                let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) else {
                    return;
                };
                lbl.set_text(&gtk_safe(&ml_cell_text(&t, &bind_id, artist_as_album_artist)));
            });
            let col = ColumnViewColumn::new(Some(c.header), Some(factory));
            col.set_resizable(true);
            if c.expand {
                col.set_expand(true);
            }
            col.set_visible(visible_ids.contains(&id_str));
            if let Some(&w) = widths.get(&id_str) {
                if w > 0 {
                    col.set_fixed_width(w);
                }
            }
            let sort_id = id_str.clone();
            let sorter = CustomSorter::new(move |a, b| {
                let ka = a
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<crate::media_library::LibTrack>(), &sort_id))
                    .unwrap_or_default();
                let kb = b
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<crate::media_library::LibTrack>(), &sort_id))
                    .unwrap_or_default();
                ka.cmp(&kb).into()
            });
            col.set_sorter(Some(&sorter));
            dev_named_cols.push((id_str.clone(), col.clone()));
            dev_col_view.append_column(&col);
        }
        // Header clicks drive the sort model.
        dev_sort_model.set_sorter(dev_col_view.sorter().as_ref());
    }
    let dev_named_cols = Rc::new(dev_named_cols);

    // Backend object id of the currently shown device (Eject/Sync target).
    let selected_dev_backend: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Reload a device's tracks into the column store (tags re-read on a worker
    // thread). Used on device select and after a sync so changed values show
    // immediately.
    let reload_device_store: Rc<dyn Fn(crate::devices::Device)> = {
        let store = dev_store.clone();
        let all_tracks = dev_all_tracks.clone();
        let hint = dev_hint.clone();
        let counts_lbl = dev_counts.clone();
        let state = state.clone();
        let counts_cache = device_counts.clone();
        let sel_backend = selected_dev_backend.clone();
        let pair_map = dev_pair_map.clone();
        let search = dev_search_entry.clone();
        Rc::new(move |dev: crate::devices::Device| {
            counts_lbl.set_text("Reading device…");
            hint.set_text(""); // clear any stale copy status
            // A previous device's search query must not filter this one — but
            // F12.1: if remember_search is on, restore the "devices" view's
            // saved query instead of clearing, so switching devices doesn't
            // discard the query the user wants kept.
            if state.borrow().config.media_library.remember_search {
                let last =
                    state.borrow().config.media_library.last_search.get("devices").cloned();
                search.set_text(last.as_deref().unwrap_or(""));
            } else {
                search.set_text("");
            }
            store.remove_all();
            pair_map.borrow_mut().clear(); // drop the previous device's pairings
            // Device contents may have changed (copy/send/sync) — drop the
            // cached overview counts so the cards recompute next time shown, and
            // the cached MTP metadata so the next poll refreshes free space once.
            counts_cache.borrow_mut().remove(&dev.backend_id);
            invalidate_mtp_meta(&dev.backend_id);
            let store2 = store.clone();
            let all_tracks2 = all_tracks.clone();
            let counts_lbl2 = counts_lbl.clone();
            let state2 = state.clone();
            let pair_map2 = pair_map.clone();
            let mount = dev.mount_path.clone();
            // Guard against a slow scan landing after the user switched devices:
            // each scan is tagged with its device, and results are applied only
            // if that device is still the one shown (else a stale scan would
            // overwrite the current device's list — the "275 vs 18" flip).
            let backend = dev.backend_id.clone();
            let sel_backend2 = sel_backend.clone();
            // Non-writing device id (don't drop a marker just to browse).
            let device_id = if dev.id.is_empty() {
                crate::devices::marker::read_marker(&dev.mount_path).unwrap_or_default()
            } else {
                dev.id.clone()
            };
            // Backend-specific IO (POSIX today; gio/MTP later) — move it onto the
            // worker thread for the blocking scan.
            let io = crate::devices::io::for_device(&dev);
            glib::spawn_future_local(async move {
                let (mut tracks, pl_count) = gio::spawn_blocking(move || {
                    if device_io_shutting_down() {
                        return (Vec::new(), 0);
                    }
                    let tracks = io
                        .list_audio_files()
                        .iter()
                        .map(|p| crate::devices::browse::read_device_track(p))
                        .collect::<Vec<crate::media_library::LibTrack>>();
                    let pl_count = io.playlist_files().len();
                    (tracks, pl_count)
                })
                .await
                .unwrap_or_default();

                // Stale-scan guard: bail if the user has since switched devices.
                if sel_backend2.borrow().as_deref() != Some(backend.as_str()) {
                    return;
                }

                // Prefill calculated values (duration, bitrate, channels) from
                // the paired library track for files copied from this computer,
                // so device rows match the files view even when the on-device
                // tags don't carry that info.
                if !device_id.is_empty() {
                    let s = state2.borrow();
                    if let Some(lib) = s.media_lib.as_ref() {
                        if let Ok(pairs) = lib.sync_pairs_for_device(&device_id) {
                            // Populate the "Synced from" map: on-device path → the
                            // library file it was copied from.
                            let mut pm = std::collections::HashMap::new();
                            for p in &pairs {
                                pm.insert(
                                    mount.join(&p.device_relpath).to_string_lossy().into_owned(),
                                    p.library_path.clone(),
                                );
                            }
                            *pair_map2.borrow_mut() = pm;
                            for t in tracks.iter_mut() {
                                let tp = std::path::Path::new(&t.path);
                                let Some(pair) = pairs.iter().find(|p| {
                                    mount.join(&p.device_relpath) == tp
                                }) else {
                                    continue;
                                };
                                let Ok(libt) = lib.track_by_path(&pair.library_path) else {
                                    continue;
                                };
                                if t.length_secs.is_none() {
                                    t.length_secs = libt.length_secs;
                                }
                                if t.bitrate.is_none() {
                                    t.bitrate = libt.bitrate;
                                }
                                if t.channels.is_none() {
                                    t.channels = libt.channels;
                                }
                                t.sort_keys = crate::media_library::SortKeys::from_track(t);
                            }
                        }
                    }
                }

                // Cache the full file list (for playlist views) and show all
                // files. A playlist chip selection re-derives its rows from this
                // cache without re-scanning.
                *all_tracks2.borrow_mut() = tracks.clone();
                store2.remove_all();
                for t in &tracks {
                    store2.append(&glib::BoxedAnyObject::new(t.clone()));
                }
                counts_lbl2.set_text(&format!(
                    "{} playlist{} - {} audio file{}",
                    pl_count,
                    if pl_count == 1 { "" } else { "s" },
                    tracks.len(),
                    if tracks.len() == 1 { "" } else { "s" }
                ));
            });
        })
    };

    // Rebuild the device playlist-filter rows ("All files" + each device
    // .m3u/.m3u8) for a mount. Shared by the device-select handler and the
    // playlist-send completion so a just-copied playlist appears immediately.
    // Apply a playlist filter to the device track view by name ("all" clears
    // it; otherwise the device .m3u/.m3u8 path). Shared by every filter chip.
    let apply_pl_filter: Rc<dyn Fn(&str)> = {
        let store = dev_store.clone();
        let all_tracks = dev_all_tracks.clone();
        let sort_model = dev_sort_model.clone();
        let pos_col = dev_pos_col.clone();
        let col_view = dev_col_view.clone();
        let sel_pl = selected_dev_playlist.clone();
        let actions = dev_pl_actions.clone();
        let remove_btn = dev_file_remove.clone();
        Rc::new(move |name: &str| {
            store.remove_all();
            if name == "all" || name.is_empty() {
                *sel_pl.borrow_mut() = None;
                actions.set_visible(false);
                remove_btn.set_label("Delete");
                pos_col.set_visible(false);
                for t in all_tracks.borrow().iter() {
                    store.append(&glib::BoxedAnyObject::new(t.clone()));
                }
                // Restore column-driven sorting for the all-files view.
                sort_model.set_sorter(col_view.sorter().as_ref());
            } else {
                *sel_pl.borrow_mut() = Some(std::path::PathBuf::from(name));
                actions.set_visible(true);
                remove_btn.set_label("Remove");
                pos_col.set_visible(true);
                // Fixed playlist order: index the device files by filename, then
                // emit one row per playlist entry — duplicates included, in order.
                let order =
                    crate::devices::browse::playlist_entry_order(std::path::Path::new(name));
                let by_name: std::collections::HashMap<String, crate::media_library::LibTrack> =
                    all_tracks
                        .borrow()
                        .iter()
                        .map(|t| (t.filename.clone(), t.clone()))
                        .collect();
                // No sort in the playlist view, so insertion order = playlist order.
                sort_model.set_sorter(None::<&gtk4::Sorter>);
                for fname in order {
                    if let Some(t) = by_name.get(&fname) {
                        store.append(&glib::BoxedAnyObject::new(t.clone()));
                    }
                }
            }
        })
    };

    let reload_dev_playlists: Rc<dyn Fn(crate::devices::Device)> = {
        let chips = dev_pl_chips.clone();
        let apply = apply_pl_filter.clone();
        // Generation token: bumped on every call so an in-flight playlist walk
        // (slow over MTP) that finishes after the user switched devices is
        // discarded instead of appending stale chips.
        let generation = Rc::new(Cell::new(0u64));
        Rc::new(move |dev: crate::devices::Device| {
            let gen_id = generation.get().wrapping_add(1);
            generation.set(gen_id);
            while let Some(c) = chips.first_child() {
                chips.remove(&c);
            }
            // "All files" chip + cleared filter are shown immediately so the
            // detail page paints without waiting on the device walk.
            let all = gtk4::ToggleButton::with_label("All files");
            all.add_css_class("device-chip");
            {
                let apply2 = apply.clone();
                all.connect_toggled(move |btn| {
                    if btn.is_active() {
                        apply2("all");
                    }
                });
            }
            chips.insert(&all, -1);
            all.set_active(true);
            apply("all");

            // Walk the device for playlist files off the main thread (a recursive
            // tree walk over a gvfs/MTP FUSE mount would otherwise freeze the UI),
            // then append a chip per playlist if this is still the shown device.
            let chips2 = chips.clone();
            let all2 = all.clone();
            let apply3 = apply.clone();
            let generation2 = generation.clone();
            let io = crate::devices::io::for_device(&dev);
            glib::spawn_future_local(async move {
                let pls = gio::spawn_blocking(move || io.playlist_files())
                    .await
                    .unwrap_or_default();
                if generation2.get() != gen_id {
                    return; // device switched / chips rebuilt since this walk began
                }
                for pl in pls {
                    let nm = pl
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let path_name = pl.to_string_lossy().into_owned();
                    let chip = gtk4::ToggleButton::with_label(&gtk_safe(&nm));
                    chip.add_css_class("device-chip");
                    chip.set_group(Some(&all2));
                    let apply4 = apply3.clone();
                    chip.connect_toggled(move |btn| {
                        if btn.is_active() {
                            apply4(&path_name);
                        }
                    });
                    chips2.insert(&chip, -1);
                }
            });
        })
    };

    // Send a whole playlist (files + .m3u8) to a device, copying on a worker
    // thread with live progress shown on the device's sidebar row and detail.
    let send_playlist_run: Rc<dyn Fn(crate::devices::Device, i64, String)> = {
        let state = state.clone();
        let sidebar = sidebar.clone();
        let hint = dev_hint.clone();
        let progress = dev_progress.clone();
        let reload = reload_device_store.clone();
        let reload_pls = reload_dev_playlists.clone();
        let sel_backend = selected_dev_backend.clone();
        let update_card = update_card_progress.clone();
        let eject = dev_eject.clone();
        let win_wk = win.downgrade();
        Rc::new(move |dev: crate::devices::Device, playlist_id: i64, name: String| {
            let plan = match prepare_playlist_send(&state, &dev, playlist_id, &name) {
                Ok(p) => p,
                Err(e) => {
                    show_alert_parented(win_wk.upgrade().as_ref(), &e);
                    return;
                }
            };
            let backend = dev.backend_id.clone();
            let dname = if dev.label.is_empty() {
                "device".to_string()
            } else {
                dev.label.clone()
            };
            let row_base = format!(
                "{}{}",
                device_glyph_prefix(dev.read_only, &dev.fs_type),
                if dev.label.is_empty() {
                    "Untitled device".to_string()
                } else {
                    dev.label.clone()
                }
            );
            let set_row_label = {
                let sidebar = sidebar.clone();
                let row_name = format!("dev:{backend}");
                move |text: &str| {
                    if let Some(row) = find_row_by_name(&sidebar, &row_name) {
                        if let Some(bx) = row.child().and_then(|c| c.downcast::<GtkBox>().ok()) {
                            if let Some(lbl) =
                                bx.first_child().and_then(|c| c.downcast::<Label>().ok())
                            {
                                lbl.set_text(text);
                            }
                        }
                    }
                }
            };

            let total = plan.srcs.len();
            let srcs = plan.srcs.clone();
            let device_id = plan.device_id.clone();
            let m3u_path = plan.m3u_path.clone();
            let mount = dev.mount_path.clone();
            let dev_for_reload = dev.clone();
            let state2 = state.clone();
            let hint2 = hint.clone();
            let progress2 = progress.clone();
            let reload2 = reload.clone();
            let reload_pls2 = reload_pls.clone();
            let sel2 = sel_backend.clone();
            let update_card2 = update_card.clone();
            let eject2 = eject.clone();
            let dev_ejectable = dev.ejectable;
            let win2 = win_wk.clone();
            glib::spawn_future_local(async move {
                // (device relpath, library source path) pairs so the written
                // .m3u8 carries #EXTINF metadata from the library.
                let mut entries: Vec<(String, String)> = Vec::new();
                let (mut copied, mut skipped, mut failed) = (0usize, 0usize, 0usize);
                let on_dev = sel2.borrow().as_deref() == Some(backend.as_str());
                if on_dev {
                    eject2.set_sensitive(false); // no eject mid-copy
                }
                for (i, src) in srcs.iter().enumerate() {
                    let prog = format!("{}/{}", i + 1, total);
                    set_row_label(&format!("{row_base} — {prog}"));
                    update_card2(&backend, Some((i + 1, total)));
                    if sel2.borrow().as_deref() == Some(backend.as_str()) {
                        let fname = src.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        hint2.set_text(&format!("Copying {prog} · {fname}"));
                        progress2.set_visible(true);
                        progress2.set_text(Some(&format!("{prog} · {fname}")));
                        progress2.set_fraction((i + 1) as f64 / total.max(1) as f64);
                    }
                    // DB lookup on the main thread; FS plan + copy on the worker
                    // so a slow MTP FUSE op never blocks the UI.
                    let recorded = device_recorded_relpath(&state2, &device_id, src);
                    let s = src.clone();
                    let m = mount.clone();
                    let dc = dev_for_reload.clone();
                    let joined = gio::spawn_blocking(move || -> Result<(std::path::PathBuf, bool), ()> {
                        let (rel, present) = device_plan_fs(&m, &s, recorded);
                        if present {
                            return Ok((rel, false)); // already there → skipped
                        }
                        match crate::devices::io::for_device(&dc).copy_to_device(&s, &rel) {
                            Ok(_) => Ok((rel, true)),
                            Err(_) => Err(()),
                        }
                    })
                    .await;
                    match joined {
                        Ok(Ok((rel, copied_now))) => {
                            if copied_now {
                                copied += 1;
                            } else {
                                skipped += 1;
                            }
                            device_record_pair(&state2, &device_id, src, &rel);
                            entries.push((
                                rel.to_string_lossy().replace('\\', "/"),
                                src.to_string_lossy().into_owned(),
                            ));
                        }
                        _ => failed += 1,
                    }
                }
                // Write the playlist file, carrying #EXTINF metadata from the
                // library for each entry.
                let body = state2
                    .borrow()
                    .media_lib
                    .as_ref()
                    .map(|l| l.build_device_m3u(&entries))
                    .unwrap_or_else(|| {
                        format!(
                            "#EXTM3U\n{}\n",
                            entries.iter().map(|(r, _)| r.clone()).collect::<Vec<_>>().join("\n")
                        )
                    });
                let mp = m3u_path.clone();
                let _ = gio::spawn_blocking(move || std::fs::write(&mp, body)).await;
                // Record the playlist sync baseline so a later edit on either
                // side syncs two-way instead of the library silently winning.
                if !device_id.is_empty() {
                    let dev_fname = m3u_path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let basenames: Vec<String> = entries
                        .iter()
                        .map(|(e, _)| e.rsplit(['/', '\\']).next().unwrap_or(e).to_string())
                        .collect();
                    if let Some(lib) = state2.borrow().media_lib.as_ref() {
                        let _ = lib.upsert_playlist_baseline(&crate::media_library::PlaylistBaseline {
                            device_id: device_id.clone(),
                            library_playlist_id: playlist_id,
                            device_filename: dev_fname,
                            entries_hash: crate::devices::sync::entries_hash(&basenames),
                            last_sync_at: Some(crate::timeutil::format_current_timestamp()),
                        });
                    }
                }
                set_row_label(&row_base);
                progress2.set_visible(false);
                update_card2(&backend, None);
                if sel2.borrow().as_deref() == Some(backend.as_str()) {
                    eject2.set_sensitive(dev_ejectable);
                }
                reload2(dev_for_reload.clone());
                // Refresh the playlist filter so the just-written .m3u8 shows
                // immediately, without needing to reselect the device.
                if sel2.borrow().as_deref() == Some(backend.as_str()) {
                    reload_pls2(dev_for_reload.clone());
                }
                show_alert_parented(
                    win2.upgrade().as_ref(),
                    &format!(
                        "Sent to {dname}: {copied} copied, {skipped} skipped, {failed} failed, \
                         plus the playlist."
                    ),
                );
            });
        })
    };
    *send_playlist_holder.borrow_mut() = Some(send_playlist_run.clone());

    // ── Device playlist management actions (New / Rename / Duplicate / Delete) ─
    // Resolve the Device backing the currently-selected device row.
    let current_device_for_actions = {
        let current_devices = current_devices.clone();
        let sel_backend = selected_dev_backend.clone();
        move || -> Option<crate::devices::Device> {
            let backend = sel_backend.borrow().clone()?;
            current_devices
                .borrow()
                .iter()
                .find(|d| d.backend_id == backend)
                .cloned()
        }
    };

    // Rename: rename the device .m3u/.m3u8; if it is linked to a library
    // playlist, rename that too so the link (safe-name match) is preserved.
    {
        let state = state.clone();
        let sel_pl = selected_dev_playlist.clone();
        let get_dev = current_device_for_actions.clone();
        let reload_pls = reload_dev_playlists.clone();
        let reload_store = reload_device_store.clone();
        let win_wk = win.downgrade();
        dev_pl_rename.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            let Some(pl_path) = sel_pl.borrow().clone() else { return };
            if dev.read_only {
                show_alert_parented(win_wk.upgrade().as_ref(), "Device is read-only.");
                return;
            }
            let current_stem = pl_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = pl_path
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| "m3u8".to_string());

            let dialog = gtk4::Window::builder()
                .title("Rename Playlist")
                .modal(true)
                .resizable(false)
                .default_width(300)
                .build();
            if let Some(w) = win_wk.upgrade() {
                dialog.set_transient_for(Some(&w));
            }
            let vbox = GtkBox::new(Orientation::Vertical, 8);
            vbox.set_margin_top(12);
            vbox.set_margin_bottom(12);
            vbox.set_margin_start(12);
            vbox.set_margin_end(12);
            let lbl = Label::builder().label("New name:").halign(Align::Start).build();
            let name_entry = Entry::new();
            name_entry.set_text(&gtk_safe(&current_stem));
            name_entry.set_hexpand(true);
            let dialog_btns = GtkBox::new(Orientation::Horizontal, 6);
            dialog_btns.set_halign(Align::End);
            let cancel_btn = Button::with_label("Cancel");
            let ok_btn = Button::with_label("Rename");
            ok_btn.add_css_class("suggested-action");
            dialog_btns.append(&cancel_btn);
            dialog_btns.append(&ok_btn);
            vbox.append(&lbl);
            vbox.append(&name_entry);
            vbox.append(&dialog_btns);
            dialog.set_child(Some(&vbox));
            let d = dialog.clone();
            cancel_btn.connect_clicked(move |_| d.close());

            let d = dialog.clone();
            let e = name_entry.clone();
            let state2 = state.clone();
            let pl_path2 = pl_path.clone();
            let dev2 = dev.clone();
            let reload_pls2 = reload_pls.clone();
            let reload_store2 = reload_store.clone();
            let win_wk2 = win_wk.clone();
            let ext2 = ext.clone();
            ok_btn.connect_clicked(move |_| {
                let raw = e.text().to_string();
                if raw.trim().is_empty() {
                    return;
                }
                let safe = safe_playlist_filename(&raw);
                let new_path = pl_path2
                    .parent()
                    .map(|p| p.join(format!("{safe}.{ext2}")))
                    .unwrap_or_else(|| pl_path2.clone());
                if new_path != pl_path2 {
                    if let Err(err) = std::fs::rename(&pl_path2, &new_path) {
                        show_alert_parented(
                            win_wk2.upgrade().as_ref(),
                            &format!("Couldn't rename the playlist file: {err}"),
                        );
                        return;
                    }
                }
                // Keep a linked library playlist's name in step.
                if let Some((id, _)) = linked_library_playlist(&state2, &pl_path2) {
                    if let Some(lib) = state2.borrow().media_lib.as_ref() {
                        let _ = lib.rename_playlist(id, raw.trim());
                    }
                }
                reload_pls2(dev2.clone());
                reload_store2(dev2.clone());
                d.close();
            });
            let ok2 = ok_btn.clone();
            name_entry.connect_activate(move |_| {
                ok2.activate();
            });
            dialog.present();
        });
    }

    // Duplicate: copy the selected device .m3u/.m3u8 to a new name on the same
    // device. The copy is a device-only playlist (referencing the same files).
    {
        let sel_pl = selected_dev_playlist.clone();
        let get_dev = current_device_for_actions.clone();
        let reload_pls = reload_dev_playlists.clone();
        let win_wk = win.downgrade();
        dev_pl_duplicate.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            let Some(pl_path) = sel_pl.borrow().clone() else { return };
            if dev.read_only {
                show_alert_parented(win_wk.upgrade().as_ref(), "Device is read-only.");
                return;
            }
            let stem = pl_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = pl_path
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| "m3u8".to_string());

            let dialog = gtk4::Window::builder()
                .title("Duplicate Playlist")
                .modal(true)
                .resizable(false)
                .default_width(300)
                .build();
            if let Some(w) = win_wk.upgrade() {
                dialog.set_transient_for(Some(&w));
            }
            let vbox = GtkBox::new(Orientation::Vertical, 8);
            vbox.set_margin_top(12);
            vbox.set_margin_bottom(12);
            vbox.set_margin_start(12);
            vbox.set_margin_end(12);
            let lbl = Label::builder().label("Name for the copy:").halign(Align::Start).build();
            let name_entry = Entry::new();
            name_entry.set_text(&gtk_safe(&format!("{stem} copy")));
            name_entry.set_hexpand(true);
            let dialog_btns = GtkBox::new(Orientation::Horizontal, 6);
            dialog_btns.set_halign(Align::End);
            let cancel_btn = Button::with_label("Cancel");
            let ok_btn = Button::with_label("Duplicate");
            ok_btn.add_css_class("suggested-action");
            dialog_btns.append(&cancel_btn);
            dialog_btns.append(&ok_btn);
            vbox.append(&lbl);
            vbox.append(&name_entry);
            vbox.append(&dialog_btns);
            dialog.set_child(Some(&vbox));
            let d = dialog.clone();
            cancel_btn.connect_clicked(move |_| d.close());

            let d = dialog.clone();
            let e = name_entry.clone();
            let pl_path2 = pl_path.clone();
            let dev2 = dev.clone();
            let reload_pls2 = reload_pls.clone();
            let win_wk2 = win_wk.clone();
            let ext2 = ext.clone();
            ok_btn.connect_clicked(move |_| {
                let raw = e.text().to_string();
                if raw.trim().is_empty() {
                    return;
                }
                let safe = safe_playlist_filename(&raw);
                let dest = dev2.mount_path.join(format!("{safe}.{ext2}"));
                if dest == pl_path2 {
                    return;
                }
                if dest.exists() {
                    show_alert_parented(
                        win_wk2.upgrade().as_ref(),
                        "A playlist with that name already exists on the device.",
                    );
                    return;
                }
                if let Err(err) = std::fs::copy(&pl_path2, &dest) {
                    show_alert_parented(
                        win_wk2.upgrade().as_ref(),
                        &format!("Couldn't duplicate the playlist: {err}"),
                    );
                    return;
                }
                reload_pls2(dev2.clone());
                d.close();
            });
            let ok2 = ok_btn.clone();
            name_entry.connect_activate(move |_| {
                ok2.activate();
            });
            dialog.present();
        });
    }

    // New: create an empty device-only playlist (a bare .m3u8) on the device.
    // The user then adds device files to it. Always available (not tied to a
    // selected playlist).
    {
        let get_dev = current_device_for_actions.clone();
        let reload_pls = reload_dev_playlists.clone();
        let win_wk = win.downgrade();
        dev_pl_new.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            if dev.read_only {
                show_alert_parented(win_wk.upgrade().as_ref(), "Device is read-only.");
                return;
            }
            if device_fs_unsupported(&dev.fs_type) {
                show_alert_parented(
                    win_wk.upgrade().as_ref(),
                    "This filesystem is unsupported — can't create a playlist on it yet.",
                );
                return;
            }
            let dialog = gtk4::Window::builder()
                .title("New Playlist")
                .modal(true)
                .resizable(false)
                .default_width(300)
                .build();
            if let Some(w) = win_wk.upgrade() {
                dialog.set_transient_for(Some(&w));
            }
            let vbox = GtkBox::new(Orientation::Vertical, 8);
            vbox.set_margin_top(12);
            vbox.set_margin_bottom(12);
            vbox.set_margin_start(12);
            vbox.set_margin_end(12);
            let lbl = Label::builder().label("Playlist name:").halign(Align::Start).build();
            let name_entry = Entry::new();
            name_entry.set_text("New Playlist");
            name_entry.set_hexpand(true);
            let dialog_btns = GtkBox::new(Orientation::Horizontal, 6);
            dialog_btns.set_halign(Align::End);
            let cancel_btn = Button::with_label("Cancel");
            let ok_btn = Button::with_label("Create");
            ok_btn.add_css_class("suggested-action");
            dialog_btns.append(&cancel_btn);
            dialog_btns.append(&ok_btn);
            vbox.append(&lbl);
            vbox.append(&name_entry);
            vbox.append(&dialog_btns);
            dialog.set_child(Some(&vbox));
            let d = dialog.clone();
            cancel_btn.connect_clicked(move |_| d.close());

            let d = dialog.clone();
            let e = name_entry.clone();
            let dev2 = dev.clone();
            let reload_pls2 = reload_pls.clone();
            let win_wk2 = win_wk.clone();
            ok_btn.connect_clicked(move |_| {
                let raw = e.text().to_string();
                if raw.trim().is_empty() {
                    return;
                }
                let safe = safe_playlist_filename(&raw);
                let dest = dev2.mount_path.join(format!("{safe}.m3u8"));
                if dest.exists() {
                    show_alert_parented(
                        win_wk2.upgrade().as_ref(),
                        "A playlist with that name already exists on the device.",
                    );
                    return;
                }
                if let Err(err) = std::fs::write(&dest, "#EXTM3U\n") {
                    show_alert_parented(
                        win_wk2.upgrade().as_ref(),
                        &format!("Couldn't create the playlist: {err}"),
                    );
                    return;
                }
                reload_pls2(dev2.clone());
                d.close();
            });
            let ok2 = ok_btn.clone();
            name_entry.connect_activate(move |_| {
                ok2.activate();
            });
            dialog.present();
        });
    }

    // Delete: remove the .m3u/.m3u8 from the device only. The audio files are
    // kept (they may belong to other playlists), and no library playlist or
    // on-disk music file is touched (Deletion Rule).
    {
        let sel_pl = selected_dev_playlist.clone();
        let get_dev = current_device_for_actions.clone();
        let reload_pls = reload_dev_playlists.clone();
        let reload_store = reload_device_store.clone();
        let win_wk = win.downgrade();
        dev_pl_delete.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            let Some(pl_path) = sel_pl.borrow().clone() else { return };
            let name = pl_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let dialog = gtk4::AlertDialog::builder()
                .message(format!("Remove \"{name}\" from the device?"))
                .detail("Only the playlist file is removed. The songs stay on the device.")
                .buttons(vec!["Cancel".to_string(), "Remove".to_string()])
                .cancel_button(0)
                .default_button(1)
                .modal(true)
                .build();
            let pl_path2 = pl_path.clone();
            let dev2 = dev.clone();
            let reload_pls2 = reload_pls.clone();
            let reload_store2 = reload_store.clone();
            let win_wk2 = win_wk.clone();
            dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |res| {
                if res != Ok(1) {
                    return;
                }
                if let Err(err) = crate::devices::io::for_device(&dev2).delete(&pl_path2) {
                    show_alert_parented(
                        win_wk2.upgrade().as_ref(),
                        &format!("Couldn't remove the playlist file: {err}"),
                    );
                    return;
                }
                reload_pls2(dev2.clone());
                reload_store2(dev2.clone());
            });
        });
    }

    // Copy loose files (drag-drop from a view) onto a device on a worker
    // thread, with the same sidebar "(x/y)" label and detail progress bar the
    // playlist send uses. No .m3u8 is written — these are just files.
    let copy_files_run: Rc<dyn Fn(crate::devices::Device, Vec<std::path::PathBuf>)> = {
        let state = state.clone();
        let sidebar = sidebar.clone();
        let hint = dev_hint.clone();
        let progress = dev_progress.clone();
        let reload = reload_device_store.clone();
        let sel_backend = selected_dev_backend.clone();
        let update_card = update_card_progress.clone();
        let eject = dev_eject.clone();
        let win_wk = win.downgrade();
        Rc::new(move |dev: crate::devices::Device, srcs: Vec<std::path::PathBuf>| {
            if dev.read_only {
                let n = if dev.label.is_empty() { "This device" } else { &dev.label };
                show_alert_parented(
                    win_wk.upgrade().as_ref(),
                    &format!("{n} is read-only — can't copy files to it."),
                );
                return;
            }
            if device_fs_unsupported(&dev.fs_type) {
                show_alert_parented(
                    win_wk.upgrade().as_ref(),
                    &format!(
                        "{} is an unsupported filesystem — can't write to this device yet.",
                        dev.fs_type
                    ),
                );
                return;
            }
            let device_id = device_sync_id(&dev);
            let mount = dev.mount_path.clone();
            let srcs: Vec<std::path::PathBuf> =
                srcs.into_iter().filter(|p| p.exists()).collect();
            if srcs.is_empty() {
                return;
            }
            // Free-space guard — only when capacity is known (skips a pass of
            // slow per-file device checks on devices that can't report it, MTP).
            if dev.free_bytes > 0 {
                let mut need = 0u64;
                for src in &srcs {
                    if !device_plan_one(&state, &mount, &device_id, src).1 {
                        need += std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
                    }
                }
                if need > dev.free_bytes {
                    show_alert_parented(
                        win_wk.upgrade().as_ref(),
                        &format!(
                            "Not enough space on the device: need {:.1} GB, {:.1} GB free.",
                            need as f64 / 1e9,
                            dev.free_bytes as f64 / 1e9
                        ),
                    );
                    return;
                }
            }

            let backend = dev.backend_id.clone();
            let dname = if dev.label.is_empty() {
                "device".to_string()
            } else {
                dev.label.clone()
            };
            let row_base = format!(
                "{}{}",
                device_glyph_prefix(dev.read_only, &dev.fs_type),
                if dev.label.is_empty() {
                    "Untitled device".to_string()
                } else {
                    dev.label.clone()
                }
            );
            let set_row_label = {
                let sidebar = sidebar.clone();
                let row_name = format!("dev:{backend}");
                move |text: &str| {
                    if let Some(row) = find_row_by_name(&sidebar, &row_name) {
                        if let Some(bx) = row.child().and_then(|c| c.downcast::<GtkBox>().ok()) {
                            if let Some(lbl) =
                                bx.first_child().and_then(|c| c.downcast::<Label>().ok())
                            {
                                lbl.set_text(text);
                            }
                        }
                    }
                }
            };

            let total = srcs.len();
            let dev_for_reload = dev.clone();
            let state2 = state.clone();
            let hint2 = hint.clone();
            let progress2 = progress.clone();
            let reload2 = reload.clone();
            let sel2 = sel_backend.clone();
            let update_card2 = update_card.clone();
            let eject2 = eject.clone();
            let dev_ejectable = dev.ejectable;
            let win2 = win_wk.clone();
            glib::spawn_future_local(async move {
                let (mut copied, mut skipped, mut failed) = (0usize, 0usize, 0usize);
                if sel2.borrow().as_deref() == Some(backend.as_str()) {
                    eject2.set_sensitive(false); // no eject mid-copy
                }
                for (i, src) in srcs.iter().enumerate() {
                    let prog = format!("{}/{}", i + 1, total);
                    set_row_label(&format!("{row_base} — {prog}"));
                    update_card2(&backend, Some((i + 1, total)));
                    if sel2.borrow().as_deref() == Some(backend.as_str()) {
                        let fname = src.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        hint2.set_text(&format!("Copying {prog} · {fname}"));
                        progress2.set_visible(true);
                        progress2.set_text(Some(&format!("{prog} · {fname}")));
                        progress2.set_fraction((i + 1) as f64 / total.max(1) as f64);
                    }
                    // DB lookup on the main thread; the FS plan + copy (slow over
                    // MTP) run on the worker so the UI never blocks on FUSE.
                    let recorded = device_recorded_relpath(&state2, &device_id, src);
                    let s = src.clone();
                    let m = mount.clone();
                    let dc = dev_for_reload.clone();
                    let joined = gio::spawn_blocking(move || -> Result<(std::path::PathBuf, bool), ()> {
                        let (rel, present) = device_plan_fs(&m, &s, recorded);
                        if present {
                            return Ok((rel, false)); // already there → skipped
                        }
                        match crate::devices::io::for_device(&dc).copy_to_device(&s, &rel) {
                            Ok(_) => Ok((rel, true)),
                            Err(_) => Err(()),
                        }
                    })
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
                set_row_label(&row_base);
                progress2.set_visible(false);
                update_card2(&backend, None);
                if sel2.borrow().as_deref() == Some(backend.as_str()) {
                    eject2.set_sensitive(dev_ejectable);
                }
                reload2(dev_for_reload.clone());
                show_alert_parented(
                    win2.upgrade().as_ref(),
                    &format!("Copied {copied}, skipped {skipped}, failed {failed} to {dname}."),
                );
            });
        })
    };
    *copy_files_holder.borrow_mut() = Some(copy_files_run.clone());

    dev_detail.append(&dev_search_row);

    let dev_tracks_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Automatic)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .child(&dev_col_view)
        .build();
    dev_detail.append(&dev_tracks_scroll);

    // ── Bottom action row for the device track view ──────────────────────────
    // Left: add files to the device + delete/remove the selected files. Right
    // (aligned like the rest of the Media Library): play / enqueue the selection.
    let dev_file_add = Button::with_label("Add Files…");
    let dev_file_play = Button::with_label("Play");
    let dev_file_enqueue = Button::with_label("Enqueue");
    for b in [&dev_file_add, &dev_file_play, &dev_file_enqueue] {
        b.add_css_class("pl-btn");
    }
    let dev_file_actions = GtkBox::new(Orientation::Horizontal, 6);
    dev_file_actions.append(&dev_file_add);
    dev_file_actions.append(&dev_file_remove);
    // dev_hint is the flexible middle element: empty (a spacer) when idle, live
    // copy status while files copy.
    dev_file_actions.append(&dev_hint);
    dev_file_actions.append(&dev_file_play);
    dev_file_actions.append(&dev_file_enqueue);
    dev_detail.append(&dev_file_actions);

    // Quiet status line (G3) — Send to Disc Drive reports here instead of
    // a success modal; mirrors the files view's `files_status`.
    let dev_status = Label::builder()
        .label("")
        .halign(Align::Start)
        .margin_start(6)
        .margin_end(6)
        .margin_bottom(2)
        .build();
    dev_status.add_css_class("status-label");
    dev_detail.append(&dev_status);

    // ── Device view status bar ──────────────────────────────────────────────
    // `dev_store` is remove_all()'d and re-appended in place (reload_device_
    // store / apply_pl_filter above) rather than swapped for a new ListStore,
    // and it's the same store wrapped (via dev_filter/dev_sort_model) by
    // `dev_selection`, so the helper's items_changed wiring keeps this live
    // without extra refresh calls at each load/filter site.
    let (dev_status_bar, _) = ml_status_bar(&dev_selection);
    dev_detail.append(&dev_status_bar);
    // Directly below the device file list (above the action buttons), matching
    // the active playlist window.
    dev_detail.reorder_child_after(&dev_status_bar, Some(&dev_tracks_scroll));

    // Collect the currently-selected device track rows (full LibTrack, so
    // already-known metadata like duration carries into the active playlist).
    let selected_device_tracks: Rc<dyn Fn() -> Vec<crate::media_library::LibTrack>> = {
        let sel = dev_selection.clone();
        let model = dev_sort_model.clone();
        Rc::new(move || {
            let mut out = Vec::new();
            for i in 0..model.n_items() {
                if !sel.is_selected(i) {
                    continue;
                }
                if let Some(t) = model.item(i).and_downcast::<glib::BoxedAnyObject>() {
                    out.push(t.borrow::<crate::media_library::LibTrack>().clone());
                }
            }
            out
        })
    };

    // Enable the Delete/Remove button only while one or more files are selected.
    {
        let remove_btn = dev_file_remove.clone();
        let sel_tracks = selected_device_tracks.clone();
        dev_selection.connect_selection_changed(move |_, _, _| {
            remove_btn.set_sensitive(!sel_tracks().is_empty());
        });
    }

    // Add Files…: pick audio files and copy them to the device Music folder.
    {
        let get_dev = current_device_for_actions.clone();
        let copy = copy_files_run.clone();
        let win_wk = win.downgrade();
        dev_file_add.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            let dialog = gtk4::FileDialog::builder().title("Add Files to Device").build();
            let copy2 = copy.clone();
            let dev2 = dev.clone();
            dialog.open_multiple(
                win_wk.upgrade().as_ref(),
                None::<&gio::Cancellable>,
                move |res| {
                    let Ok(files) = res else { return };
                    let paths: Vec<std::path::PathBuf> = (0..files.n_items())
                        .filter_map(|i| files.item(i).and_downcast::<gio::File>())
                        .filter_map(|f| f.path())
                        .collect();
                    if !paths.is_empty() {
                        copy2(dev2.clone(), paths);
                    }
                },
            );
        });
    }

    // Play: replace the active playlist with the selected device files and play
    // from the first one (so "Play" plays just the selection, not whatever was
    // queued before). Built from the device LibTrack so known duration/tags
    // show immediately rather than "-:--" until played.
    {
        let sel_tracks = selected_device_tracks.clone();
        let state = state.clone();
        let rebuild = rebuild_playlist.clone();
        dev_file_play.connect_clicked(move |_| {
            let tracks = sel_tracks();
            if tracks.is_empty() {
                return;
            }
            let _ = state.borrow_mut().player.stop();
            state.borrow_mut().playlist.clear();
            for lt in &tracks {
                state.borrow_mut().playlist.add(crate::model::Track::from(lt));
            }
            if !state.borrow().playlist.is_empty() {
                state.borrow_mut().play_current();
            }
            rebuild();
        });
    }

    // Enqueue: append the selected device files to the active playlist, carrying
    // the device row's known metadata (duration etc.) so it shows immediately.
    {
        let sel_tracks = selected_device_tracks.clone();
        let state = state.clone();
        let rebuild = rebuild_playlist.clone();
        dev_file_enqueue.connect_clicked(move |_| {
            let tracks = sel_tracks();
            if tracks.is_empty() {
                return;
            }
            let was_empty = state.borrow().playlist.is_empty();
            for lt in &tracks {
                state.borrow_mut().playlist.add(crate::model::Track::from(lt));
            }
            if state.borrow().config.behavior.autoplay_on_add && was_empty {
                state.borrow_mut().play_current();
            }
            rebuild();
        });
    }

    // Delete / Remove on the selected device files. Behaviour depends on the
    // active view:
    //   • All files  → "Delete": permanently delete the files from the device
    //     AND drop them from every device playlist (Deletion Rule — allowed from
    //     this Media Library external-device view, after confirmation).
    //   • A playlist → "Remove": drop the files from THAT playlist only; the
    //     files stay on the device and in other playlists.
    {
        let sel_tracks = selected_device_tracks.clone();
        let get_dev = current_device_for_actions.clone();
        let reload_store = reload_device_store.clone();
        let reload_pls = reload_dev_playlists.clone();
        let apply_filter = apply_pl_filter.clone();
        let sel_pl = selected_dev_playlist.clone();
        let state_del = state.clone();
        let win_wk = win.downgrade();
        dev_file_remove.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            let paths: Vec<std::path::PathBuf> = sel_tracks()
                .iter()
                .map(|t| std::path::PathBuf::from(&t.path))
                .collect();
            if paths.is_empty() {
                return;
            }
            let n = paths.len();
            let in_playlist = sel_pl.borrow().clone();

            let (message, detail, confirm) = if let Some(pl) = &in_playlist {
                let pl_name = pl
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (
                    format!(
                        "Remove {n} file{} from \"{pl_name}\"?",
                        if n == 1 { "" } else { "s" }
                    ),
                    "The file(s) stay on the device and in any other playlist.".to_string(),
                    "Remove".to_string(),
                )
            } else {
                (
                    format!(
                        "Delete {n} file{} from the device?",
                        if n == 1 { "" } else { "s" }
                    ),
                    "The file(s) are permanently deleted from the device and removed from every \
                     playlist. This can't be undone."
                        .to_string(),
                    "Delete".to_string(),
                )
            };

            let dialog = gtk4::AlertDialog::builder()
                .message(message)
                .detail(detail)
                .buttons(vec!["Cancel".to_string(), confirm])
                .cancel_button(0)
                .default_button(0)
                .modal(true)
                .build();
            let reload_store2 = reload_store.clone();
            let reload_pls2 = reload_pls.clone();
            let apply_filter2 = apply_filter.clone();
            let dev2 = dev.clone();
            let win_wk2 = win_wk.clone();
            let in_playlist2 = in_playlist.clone();
            let state2 = state_del.clone();
            dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |res| {
                if res != Ok(1) {
                    return;
                }
                match &in_playlist2 {
                    Some(pl_path) => {
                        // Remove from this playlist only — rewrite its .m3u8.
                        let basenames: std::collections::HashSet<String> = paths
                            .iter()
                            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
                            .map(|s| s.to_string())
                            .collect();
                        device_m3u_remove_basenames(pl_path, &basenames);
                        // Re-apply the filter so the removed rows disappear.
                        apply_filter2(&pl_path.to_string_lossy());
                    }
                    None => {
                        // Delete off the device + drop from every playlist.
                        let failed = device_delete_files(&dev2, &paths);
                        reload_store2(dev2.clone());
                        reload_pls2(dev2.clone());
                        // Reconcile the ACTIVE playlist too: device files can
                        // be queued there (device Play/Enqueue), and a deleted
                        // file must show broken immediately — and stop the
                        // player if it was the one playing — instead of
                        // lingering until a read error.
                        let rebuild_pl = {
                            let deleted: std::collections::HashSet<&std::path::PathBuf> =
                                paths.iter().collect();
                            let mut s = state2.borrow_mut();
                            let cur = s.playlist.current_index;
                            let mut touched = false;
                            let mut current_deleted = false;
                            for (i, t) in s.playlist.tracks.iter_mut().enumerate() {
                                if deleted.contains(&t.path) {
                                    t.broken = true;
                                    touched = true;
                                    if i == cur {
                                        current_deleted = true;
                                    }
                                }
                            }
                            if current_deleted
                                && !matches!(
                                    *s.player.state(),
                                    crate::engine::PlayerState::Stopped
                                )
                            {
                                let _ = s.player.stop();
                            }
                            if touched {
                                s.rebuild_pl_callback.clone()
                            } else {
                                None
                            }
                        };
                        if let Some(cb) = rebuild_pl {
                            cb();
                        }
                        if failed > 0 {
                            show_alert_parented(
                                win_wk2.upgrade().as_ref(),
                                &format!("{failed} file(s) couldn't be deleted."),
                            );
                        }
                    }
                }
            });
        });
    }

    // Drop target on the device track list: dropping files (from the active
    // playlist, files view, or editor) copies them to the device currently
    // shown in the detail view; dropping a playlist row sends the playlist.
    // Same routing as the sidebar device row, just with a fixed target.
    {
        let dt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        dt.set_types(&[gdk::FileList::static_type(), glib::Type::STRING]);
        let sel_backend_drop = selected_dev_backend.clone();
        let current_devices_drop = current_devices.clone();
        let state_drop = state.clone();
        let copy_holder = copy_files_holder.clone();
        let send_holder = send_playlist_holder.clone();
        dt.connect_drop(move |_, value, _x, _y| {
            // Resolve the device currently shown in the detail view.
            let Some(backend) = sel_backend_drop.borrow().clone() else {
                return false;
            };
            let Some(dev) = current_devices_drop
                .borrow()
                .iter()
                .find(|d| d.backend_id == backend)
                .cloned()
            else {
                return false;
            };

            // A playlist row (`pl:<id>` String) → send the whole playlist.
            if let Ok(s) = value.get::<String>() {
                if let Some(pid) = s.strip_prefix("pl:").and_then(|n| n.trim().parse::<i64>().ok())
                {
                    let plname = state_drop
                        .borrow()
                        .media_lib
                        .as_ref()
                        .and_then(|l| l.playlist_by_id(pid).ok())
                        .map(|p| p.name)
                        .unwrap_or_default();
                    if let Some(send) = send_holder.borrow().as_ref() {
                        send(dev, pid, plname);
                        return true;
                    }
                    return false;
                }
                // Otherwise a uri/path-list String → copy those files.
                let paths: Vec<std::path::PathBuf> = s
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(|l| {
                        if l.starts_with("file://") {
                            gio::File::for_uri(l)
                                .path()
                                .unwrap_or_else(|| std::path::PathBuf::from(l))
                        } else {
                            std::path::PathBuf::from(l)
                        }
                    })
                    .collect();
                if paths.is_empty() {
                    return false;
                }
                if let Some(copy) = copy_holder.borrow().as_ref() {
                    copy(dev, paths);
                    return true;
                }
                return false;
            }

            // A FileList drag → copy the dragged files.
            if let Ok(file_list) = value.get::<gdk::FileList>() {
                let paths: Vec<std::path::PathBuf> =
                    file_list.files().iter().filter_map(|f| f.path()).collect();
                if paths.is_empty() {
                    return false;
                }
                if let Some(copy) = copy_holder.borrow().as_ref() {
                    copy(dev, paths);
                    return true;
                }
            }
            false
        });
        dev_tracks_scroll.add_controller(dt);
    }

    // ── Right-click context menu on device files: View / Edit ID3 ────────────
    // Mirrors the active-playlist menu. The ID3 editor also shows/edits album
    // art, so this one item covers viewing artwork too. Operates on the current
    // selection (like the Play / Enqueue / Delete buttons in this view); the
    // editor binds one file, so the item appears only for a single selection.
    // Gesture + action group live on the ScrolledWindow, not the ColumnView, to
    // dodge the GTK4 bug where a PopoverMenu parented on the view misses hover.
    {

        let dev_file_action_group = gio::SimpleActionGroup::new();
        dev_tracks_scroll.insert_action_group("dev-file", Some(&dev_file_action_group));

        // Send-to actions (Task 8) live in a separate "dev" group so the
        // pre-existing "dev-file" prefix (edit-id3) is untouched. Same five
        // action names / bodies as the other three Send-to consumers.
        // Device-to-device: current_devices already contains the device
        // being viewed, and it isn't filtered out here — sending to it is
        // a harmless skip-if-present copy, not worth special-casing.
        let dev_send_action_group = gio::SimpleActionGroup::new();
        dev_tracks_scroll.insert_action_group("dev", Some(&dev_send_action_group));

        // Send to Active Playlist — same body as the Enqueue button below.
        {
            let sel_tracks = selected_device_tracks.clone();
            let state = state.clone();
            let rebuild = rebuild_playlist.clone();
            let action = gio::SimpleAction::new("send-active", None);
            action.connect_activate(move |_, _| {
                let tracks = sel_tracks();
                if tracks.is_empty() {
                    return;
                }
                let was_empty = state.borrow().playlist.is_empty();
                for lt in &tracks {
                    state.borrow_mut().playlist.add(crate::model::Track::from(lt));
                }
                if state.borrow().config.behavior.autoplay_on_add && was_empty {
                    state.borrow_mut().play_current();
                }
                rebuild();
            });
            dev_send_action_group.add_action(&action);
        }

        // Seed a brand new saved playlist from the selected device files.
        {
            let sel_tracks = selected_device_tracks.clone();
            let state = state.clone();
            let win_new = win.clone();
            let action = gio::SimpleAction::new("add-to-new", None);
            action.connect_activate(move |_, _| {
                let paths: Vec<String> = sel_tracks().iter().map(|t| t.path.clone()).collect();
                if paths.is_empty() {
                    return;
                }
                let default_stem = glib::DateTime::now_local()
                    .ok()
                    .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Playlist".to_string());
                let state_cb = state.clone();
                let paths_cb = paths.clone();
                run_playlist_save_dialog(
                    state.clone(),
                    win_new.clone(),
                    &default_stem,
                    move |path, win_cb| {
                        if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                            if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths_cb) {
                                eprintln!("save_playlist_tracks_to_path: {e}");
                                show_playlist_save_error(&win_cb, &path, &e);
                            }
                        }
                        notify_playlist_nav_refresh();
                    },
                );
            });
            dev_send_action_group.add_action(&action);
        }

        // Append selected device files to an existing saved playlist.
        {
            let sel_tracks = selected_device_tracks.clone();
            let state = state.clone();
            let action = gio::SimpleAction::new(
                "add-to-saved",
                Some(glib::VariantTy::INT64),
            );
            action.connect_activate(move |_, param| {
                let Some(pid) = param.and_then(|p| p.get::<i64>()) else { return };
                let paths: Vec<String> = sel_tracks().iter().map(|t| t.path.clone()).collect();
                if paths.is_empty() {
                    return;
                }
                let mut ok = false;
                if let Some(lib) = state.borrow().media_lib.as_ref() {
                    match lib.append_paths_to_playlist(pid, &paths) {
                        Ok(_) => ok = true,
                        Err(e) => eprintln!("append_paths_to_playlist {pid}: {e}"),
                    }
                }
                if ok {
                    notify_playlist_changed(pid);
                }
            });
            dev_send_action_group.add_action(&action);
        }

        // Send to Disc Drive: probe-on-add, then queue onto THAT drive.
        // Same body as the Files view's ml.send-drive, but metadata comes
        // straight from the already-fetched device LibTrack rows — no
        // media_lib lookup, since device files are often not indexed there.
        {
            let sel_tracks = selected_device_tracks.clone();
            let burn_queues = burn_queues.clone();
            let burn_refresh_holder = burn_refresh_holder.clone();
            let current_drives = current_drives.clone();
            let win_wk = win.downgrade();
            let status = dev_status.clone();
            let action = gio::SimpleAction::new(
                "send-drive",
                Some(glib::VariantTy::STRING),
            );
            action.connect_activate(move |_, target| {
                let Some(drive_id) = target.and_then(|v| v.get::<String>()) else { return };
                let drive_label = current_drives
                    .borrow()
                    .iter()
                    .find(|d| d.id == drive_id)
                    .map(|d| d.label.clone())
                    .unwrap_or_else(|| drive_id.clone());
                // Live selection at dispatch (already correct here —
                // `selected_device_tracks` reads the selection model
                // fresh on every call, not a right-click stash).
                let tracks = sel_tracks();
                let paths: Vec<std::path::PathBuf> = tracks.iter()
                    .map(|t| std::path::PathBuf::from(&t.path))
                    .collect();
                // Metadata comes straight from the already-fetched device
                // LibTrack rows — no media_lib lookup, since device files
                // are often not indexed there.
                let metas: std::collections::HashMap<_, _> = tracks.iter().map(|t| {
                    let display = match (&t.artist, &t.title) {
                        (Some(a), Some(ti)) if !a.is_empty() => format!("{a} - {ti}"),
                        (_, Some(ti)) => ti.clone(),
                        _ => t.filename.clone(),
                    };
                    let secs = t.length_secs.map(|s| s as u32);
                    let bytes = std::fs::metadata(&t.path).map(|m| m.len()).unwrap_or(0);
                    (std::path::PathBuf::from(&t.path), (display, secs, bytes))
                }).collect();
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
            dev_send_action_group.add_action(&action);
        }

        // Send to Removable Device: hand off to the Files view's copy
        // runner via the shared holder.
        {
            let sel_tracks = selected_device_tracks.clone();
            let current_devices = current_devices.clone();
            let copy_files_holder = copy_files_holder.clone();
            let action = gio::SimpleAction::new(
                "send-device",
                Some(glib::VariantTy::STRING),
            );
            action.connect_activate(move |_, target| {
                let Some(dev_id) = target.and_then(|v| v.get::<String>()) else { return };
                let dev = current_devices
                    .borrow()
                    .iter()
                    .find(|d| d.id == dev_id)
                    .cloned();
                let paths: Vec<std::path::PathBuf> = sel_tracks().iter()
                    .map(|t| std::path::PathBuf::from(&t.path))
                    .collect();
                if let (Some(dev), false) = (dev, paths.is_empty()) {
                    if let Some(run) = copy_files_holder.borrow().clone() {
                        run(dev, paths);
                    }
                }
            });
            dev_send_action_group.add_action(&action);
        }

        let action_id3 = gio::SimpleAction::new("edit-id3", None);
        {
            let state_id3 = state.clone();
            let win_id3 = win.downgrade();
            let sel_tracks = selected_device_tracks.clone();
            let reload_store = reload_device_store.clone();
            let current_devices_id3 = current_devices.clone();
            let sel_backend_id3 = selected_dev_backend.clone();
            action_id3.connect_activate(move |_, _| {
                let tracks = sel_tracks();
                let [track] = tracks.as_slice() else { return };
                let path = std::path::PathBuf::from(&track.path);
                // Re-read the edited device file's row so new tags show.
                let reload = reload_store.clone();
                let devices = current_devices_id3.clone();
                let backend = sel_backend_id3.clone();
                let rebuild_cb: Rc<dyn Fn()> = Rc::new(move || {
                    let Some(b) = backend.borrow().clone() else { return };
                    if let Some(dev) =
                        devices.borrow().iter().find(|d| d.backend_id == b).cloned()
                    {
                        reload(dev);
                    }
                });
                open_id3_editor_window(
                    win_id3.upgrade().as_ref(),
                    path,
                    state_id3.clone(),
                    rebuild_cb,
                    None,
                    None,
                );
            });
        }
        dev_file_action_group.add_action(&action_id3);

        // View/Search Lyrics (F15) on device files. The fresh USLT read runs on
        // the caller thread as the ID3 action's read does; an unreadable/slow
        // MTP path returns Search from core, so it never hangs forever.
        let action_lyrics = gio::SimpleAction::new("lyrics", None);
        {
            let state_lyr = state.clone();
            let sel_tracks = selected_device_tracks.clone();
            let reload_store = reload_device_store.clone();
            let devices_lyr = current_devices.clone();
            let backend_lyr = selected_dev_backend.clone();
            action_lyrics.connect_activate(move |_, _| {
                let tracks = sel_tracks();
                let [track] = tracks.as_slice() else { return };
                let path = std::path::PathBuf::from(&track.path);
                let artist = track.artist.clone().unwrap_or_default();
                let title = track.title.clone().unwrap_or_default();
                let album_artist = track.album_artist.clone().unwrap_or_default();
                let reload = reload_store.clone();
                let devices = devices_lyr.clone();
                let backend = backend_lyr.clone();
                let rebuild_cb: Rc<dyn Fn()> = Rc::new(move || {
                    let Some(b) = backend.borrow().clone() else { return };
                    if let Some(dev) = devices.borrow().iter().find(|d| d.backend_id == b).cloned() {
                        reload(dev);
                    }
                });
                view_or_search_lyrics(&state_lyr, &path, &artist, &title, &album_artist, rebuild_cb, LyricsMode::Specific);
            });
        }
        dev_file_action_group.add_action(&action_lyrics);

        // Replace the active playlist with the selected device files.
        {
            let sel_tracks = selected_device_tracks.clone();
            let state_c = state.clone();
            let rebuild = rebuild_playlist.clone();
            let action = gio::SimpleAction::new("replace", None);
            action.connect_activate(move |_, _| {
                let tracks = sel_tracks();
                if tracks.is_empty() {
                    return;
                }
                let _ = state_c.borrow_mut().player.stop();
                state_c.borrow_mut().playlist.clear();
                for t in &tracks {
                    if let Ok(track) = crate::model::Track::from_path(std::path::Path::new(&t.path)) {
                        state_c.borrow_mut().playlist.add(track);
                    }
                }
                if state_c.borrow().config.behavior.autoplay_on_add {
                    state_c.borrow_mut().play_current();
                }
                rebuild();
            });
            dev_file_action_group.add_action(&action);
        }

        // View Album Art for the single selected device file.
        {
            let sel_tracks = selected_device_tracks.clone();
            let state_c = state.clone();
            let action = gio::SimpleAction::new("view-art", None);
            action.connect_activate(move |_, _| {
                let tracks = sel_tracks();
                let Some(t) = tracks.first() else { return };
                art_window::open_track_art(&state_c, std::path::Path::new(&t.path));
            });
            dev_file_action_group.add_action(&action);
        }

        // Delete the selected files FROM THE DEVICE (permanent). Device view is
        // one of the two surfaces the Deletion Rule allows real file deletion
        // from, and only after explicit confirmation — hence the AlertDialog.
        {
            let sel_tracks = selected_device_tracks.clone();
            let devices_del = current_devices.clone();
            let backend_del = selected_dev_backend.clone();
            let reload_store = reload_device_store.clone();
            let win_wk = win.downgrade();
            let action = gio::SimpleAction::new("delete", None);
            action.connect_activate(move |_, _| {
                let tracks = sel_tracks();
                if tracks.is_empty() {
                    return;
                }
                let Some(b) = backend_del.borrow().clone() else { return };
                let Some(dev) = devices_del.borrow().iter().find(|d| d.backend_id == b).cloned()
                else {
                    return;
                };
                let paths: Vec<std::path::PathBuf> =
                    tracks.iter().map(|t| std::path::PathBuf::from(&t.path)).collect();
                let n = paths.len();
                let dialog = gtk4::AlertDialog::builder()
                    .message(format!(
                        "Delete {n} file{} from the device?",
                        if n == 1 { "" } else { "s" }
                    ))
                    .detail("The files are permanently removed from the device.")
                    .buttons(vec!["Cancel".to_string(), "Delete".to_string()])
                    .cancel_button(0)
                    .default_button(1)
                    .modal(true)
                    .build();
                let dev2 = dev.clone();
                let reload2 = reload_store.clone();
                let win_wk2 = win_wk.clone();
                dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |res| {
                    if res != Ok(1) {
                        return;
                    }
                    let deleted = crate::devices::plan::device_delete_files(&dev2, &paths);
                    if deleted != paths.len() {
                        show_alert_parented(
                            win_wk2.upgrade().as_ref(),
                            "Some files could not be deleted from the device.",
                        );
                    }
                    reload2(dev2.clone());
                });
            });
            dev_file_action_group.add_action(&action);
        }

        // `l` — View/Search Lyrics for the single selected device track in
        // Specific mode. No-op on a multi-row or empty selection, matching the
        // row menu. Rebuild reloads the device store so a tag edit from the
        // lyrics window refreshes this view, mirroring the row action above.
        {
            let key = EventControllerKey::new();
            let state_l = state.clone();
            let sel_tracks = selected_device_tracks.clone();
            let reload_store = reload_device_store.clone();
            let devices_l = current_devices.clone();
            let backend_l = selected_dev_backend.clone();
            key.connect_key_pressed(move |_, keyval, _, _| {
                if !matches!(keyval, gdk::Key::l | gdk::Key::L) {
                    return glib::Propagation::Proceed;
                }
                let tracks = sel_tracks();
                let [track] = tracks.as_slice() else {
                    return glib::Propagation::Proceed;
                };
                let path = std::path::PathBuf::from(&track.path);
                let artist = track.artist.clone().unwrap_or_default();
                let title = track.title.clone().unwrap_or_default();
                let album_artist = track.album_artist.clone().unwrap_or_default();
                let reload = reload_store.clone();
                let devices = devices_l.clone();
                let backend = backend_l.clone();
                let rebuild_cb: Rc<dyn Fn()> = Rc::new(move || {
                    let Some(b) = backend.borrow().clone() else { return };
                    if let Some(dev) = devices.borrow().iter().find(|d| d.backend_id == b).cloned() {
                        reload(dev);
                    }
                });
                view_or_search_lyrics(
                    &state_l, &path, &artist, &title, &album_artist, rebuild_cb,
                    LyricsMode::Specific,
                );
                glib::Propagation::Stop
            });
            dev_col_view.add_controller(key);
        }

        let sel_menu = selected_device_tracks.clone();
        let scroll_menu = dev_tracks_scroll.clone();
        let state_menu_dev = state.clone();
        let drives_menu_dev = current_drives.clone();
        let devices_menu_dev = current_devices.clone();
        // Filled here rather than connected to the ScrolledWindow: each cell
        // calls this through `dev_row_menu_holder` after selecting its own
        // row, so `x`/`y` arrive in ColumnView space and `sel` is never empty
        // just because nothing had been left-clicked yet.
        let col_view_menu_dev = dev_col_view.clone();
        // The previously-opened row popover, kept only so it can be unparented
        // when the next one opens — see the note where it is set.
        let last_popover_dev: Rc<RefCell<Option<gtk4::PopoverMenu>>> =
            Rc::new(RefCell::new(None));
        *dev_row_menu_holder.borrow_mut() = Some(Rc::new(move |x: f64, y: f64| {
            let sel = sel_menu();
            if sel.is_empty() {
                return;
            }
            // Retire the previous menu's popover first, while nothing of this
            // one exists yet — see the note further down for why the order
            // matters.
            if let Some(old) = last_popover_dev.borrow_mut().take() {
                old.unparent();
            }
            // Order: Send to · Replace · ─ · ID3 · Album Art · Lyrics · ─ ·
            // Delete from Device. Matches the macOS device menu
            // (DeviceDetailView). Delete permanently removes from the device.
            let send = build_send_to_menu(
                &state_menu_dev,
                &SendToActions {
                    active: "dev.send-active",
                    new_playlist: "dev.add-to-new",
                    saved_playlist: "dev.add-to-saved",
                    drive: "dev.send-drive",
                    device: "dev.send-device",
                    drives: drives_menu_dev.borrow().iter()
                        .map(|d| (d.id.clone(), d.label.clone())).collect(),
                    // Includes the device currently being viewed — sending
                    // to it is a harmless skip-if-present copy (Task 8).
                    devices: devices_menu_dev.borrow().iter()
                        .map(|d| (d.id.clone(), d.label.clone())).collect(),
                },
            );
            let menu = gio::Menu::new();
            menu.append_submenu(Some("↪ Send to"), &send);
            menu.append_item(&gio::MenuItem::new(
                Some("♻ Replace Current Playlist"),
                Some("dev-file.replace"),
            ));
            // Single-file view items (bind one file).
            if sel.len() == 1 {
                menu.append_item(&gio::MenuItem::new(
                    Some("🎵 View/Edit ID3"),
                    Some("dev-file.edit-id3"),
                ));
                menu.append_item(&gio::MenuItem::new(
                    Some("🖼 View Album Art"),
                    Some("dev-file.view-art"),
                ));
                menu.append_item(&gio::MenuItem::new(
                    Some("📝 View/Search Lyrics"),
                    Some("dev-file.lyrics"),
                ));
            }
            menu.append_item(&gio::MenuItem::new(
                Some("🗑 Delete from Device"),
                Some("dev-file.delete"),
            ));
            let popover = context_popover(&menu);
            popover.set_parent(&scroll_menu);
            // Do NOT unparent on close. Activating an item closes the popover
            // first, and unparenting there severs the link to the widget
            // holding the "dev"/"dev-file" action groups, so every item in
            // this menu silently did nothing — Send to, Replace, Delete from
            // Device, all of it (2026-08-10). The Files and disc views carry
            // the same warning and already avoid it; this one did not.
            //
            // The leak that guard was for is bounded by dropping the PREVIOUS
            // popover instead — but at the TOP of this closure, before any of
            // this one exists. Doing it here, after `set_parent`, removed a
            // child from `scroll_menu` while the new popover was already
            // attached to it, and the new popover did not survive its own
            // popup: the two-right-clicks symptom came straight back
            // (2026-08-10). From `set_parent` onward this now matches the
            // Files and disc recipes exactly.
            *last_popover_dev.borrow_mut() = Some(popover.clone());
            // The cell handed us ColumnView coordinates; the popover is
            // parented to the ScrolledWindow, so make the last hop here.
            let (sx, sy) = col_view_menu_dev
                .translate_coordinates(&scroll_menu, x, y)
                .unwrap_or((x, y));
            let rect = gtk4::gdk::Rectangle::new(sx as i32, sy as i32, 1, 1);
            popover.set_pointing_to(Some(&rect));
            popover.popup();
        }));
    }

    dev_page.append(&dev_detail);
    stack.add_named(&dev_page, Some("devices"));

    // ── Device detection: poll udisks2 and keep the sidebar live ──────────
    // A 2 s poll (rather than D-Bus signal wiring) keeps this simple while
    // still updating in place — devices appear/disappear and free space
    // refreshes without reopening the window.
    // Deferred handles to the eject / sync runners (defined further down, once
    // the refresh + reload closures they need exist). The overview rows' Sync
    // and Eject buttons call through these.
    let eject_run_holder: Rc<RefCell<Option<Rc<dyn Fn(String)>>>> =
        Rc::new(RefCell::new(None));
    let sync_run_holder: Rc<RefCell<Option<Rc<dyn Fn(crate::devices::Device, Button)>>>> =
        Rc::new(RefCell::new(None));

    // Rebuild the device overview list (shown when the Devices header is
    // selected) from the latest detection results. Each device is its own row
    // with Sync and Eject buttons on the right.
    let rebuild_overview: Rc<dyn Fn()> = {
        let list = dev_overview_list.clone();
        let current = current_devices.clone();
        let eject_holder = eject_run_holder.clone();
        let sync_holder = sync_run_holder.clone();
        let counts_cache = device_counts.clone();
        let counts_inflight = counts_in_flight.clone();
        let transfers = device_transfers.clone();
        let card_bars = device_card_progress.clone();
        let sidebar_ov = sidebar.clone();
        Rc::new(move || {
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            // Card progress bars are rebuilt below; drop the stale references.
            card_bars.borrow_mut().clear();
            let devs = current.borrow();
            if devs.is_empty() {
                let l = Label::builder()
                    .label("No devices connected.")
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                l.add_css_class("status-label");
                list.append(&l);
                return;
            }
            for d in devs.iter() {
                let name = if d.label.is_empty() {
                    "Untitled device".to_string()
                } else {
                    d.label.clone()
                };

                // ── Card ────────────────────────────────────────────────
                let card = GtkBox::new(Orientation::Vertical, 6);
                card.add_css_class("device-card");

                // Header: icon · name + filesystem · status badges.
                let header = GtkBox::new(Orientation::Horizontal, 10);
                let icon = Image::from_icon_name(device_icon_name(d));
                icon.set_pixel_size(32);
                icon.set_valign(Align::Center);
                header.append(&icon);

                let title_box = GtkBox::new(Orientation::Vertical, 0);
                title_box.set_hexpand(true);
                title_box.set_valign(Align::Center);
                let name_lbl = Label::builder()
                    .label(&gtk_safe(&name))
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                name_lbl.add_css_class("device-card-name");
                let fs_lbl = Label::builder()
                    .label(if d.fs_type.is_empty() { "unknown" } else { &d.fs_type })
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                fs_lbl.add_css_class("status-label");
                title_box.append(&name_lbl);
                title_box.append(&fs_lbl);
                header.append(&title_box);

                let badges = GtkBox::new(Orientation::Horizontal, 4);
                badges.set_valign(Align::Center);
                if d.read_only {
                    let b = Label::new(Some("🔒 Read-only"));
                    b.add_css_class("device-badge");
                    badges.append(&b);
                }
                if device_fs_unsupported(&d.fs_type) {
                    let b = Label::new(Some("⚠ Unsupported"));
                    b.add_css_class("device-badge");
                    b.add_css_class("device-badge-warn");
                    b.set_tooltip_text(Some(UNSUPPORTED_FS_TOOLTIP));
                    badges.append(&b);
                }
                header.append(&badges);
                // Clicking the card's banner (icon + name area) opens that
                // device's detail page by selecting its sidebar row, which the
                // row-selected handler turns into the detail view. The Sync/Eject
                // buttons live in their own row below and claim their own clicks.
                {
                    let click = gtk4::GestureClick::new();
                    let sidebar = sidebar_ov.clone();
                    let row_name = format!("dev:{}", d.backend_id);
                    click.connect_released(move |_, _, _, _| {
                        if let Some(row) = find_row_by_name(&sidebar, &row_name) {
                            sidebar.select_row(Some(&row));
                        }
                    });
                    header.add_controller(click);
                    header.set_cursor_from_name(Some("pointer"));
                }
                card.append(&header);

                // Capacity bar + free/total text.
                let used = if d.total_bytes > 0 {
                    1.0 - (d.free_bytes as f64 / d.total_bytes as f64)
                } else {
                    0.0
                };
                let bar = gtk4::LevelBar::new();
                bar.set_min_value(0.0);
                bar.set_max_value(1.0);
                bar.set_value(used);
                set_levelbar_fullness(&bar, used);
                card.append(&bar);

                let cap_lbl = Label::builder()
                    .label(&format!(
                        "{:.1} GB free of {:.1} GB",
                        d.free_bytes as f64 / 1e9,
                        d.total_bytes as f64 / 1e9,
                    ))
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                cap_lbl.add_css_class("status-label");
                card.append(&cap_lbl);

                // Song / playlist counts — cached, computed off-thread on miss.
                let counts_lbl = Label::builder()
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                counts_lbl.add_css_class("status-label");
                match counts_cache.borrow().get(&d.backend_id).copied() {
                    Some((songs, pls)) => {
                        counts_lbl.set_text(&counts_text(songs, pls));
                    }
                    None => {
                        counts_lbl.set_text("counting…");
                        let backend = d.backend_id.clone();
                        if counts_inflight.borrow_mut().insert(backend.clone()) {
                            let mount = d.mount_path.clone();
                            let cache = counts_cache.clone();
                            let inflight = counts_inflight.clone();
                            let lbl = counts_lbl.clone();
                            glib::spawn_future_local(async move {
                                let res = gio::spawn_blocking(move || {
                                    if device_io_shutting_down() {
                                        return (0, 0);
                                    }
                                    let songs =
                                        crate::devices::browse::list_audio_files(&mount).len();
                                    let pls = crate::devices::browse::device_playlist_files(&mount)
                                        .len();
                                    (songs, pls)
                                })
                                .await
                                .unwrap_or((0, 0));
                                cache.borrow_mut().insert(backend.clone(), res);
                                inflight.borrow_mut().remove(&backend);
                                lbl.set_text(&counts_text(res.0, res.1));
                            });
                        }
                    }
                }
                card.append(&counts_lbl);

                // Copy progress bar — always present (reserves its space) so the
                // card height is identical whether or not a transfer is running.
                // Transparent when idle; the runners drive it via backend_id.
                let prog = gtk4::ProgressBar::new();
                prog.set_show_text(true);
                apply_card_progress(&prog, transfers.borrow().get(&d.backend_id).copied());
                card.append(&prog);
                card_bars.borrow_mut().insert(d.backend_id.clone(), prog);

                // Sync / Eject buttons, right-aligned.
                let btn_row = GtkBox::new(Orientation::Horizontal, 6);
                btn_row.set_halign(Align::End);
                btn_row.set_margin_top(2);

                let sync_btn = Button::with_label("Sync");
                sync_btn.add_css_class("pl-btn");
                {
                    let holder = sync_holder.clone();
                    let dev = d.clone();
                    sync_btn.connect_clicked(move |btn| {
                        if let Some(run) = holder.borrow().as_ref() {
                            run(dev.clone(), btn.clone());
                        }
                    });
                }
                btn_row.append(&sync_btn);

                let eject_btn = Button::with_label("Eject");
                eject_btn.add_css_class("pl-btn");
                // Unavailable while a copy to this device is running.
                eject_btn.set_sensitive(
                    d.ejectable && !transfers.borrow().contains_key(&d.backend_id),
                );
                {
                    let holder = eject_holder.clone();
                    let backend = d.backend_id.clone();
                    eject_btn.connect_clicked(move |btn| {
                        btn.set_sensitive(false);
                        if let Some(run) = holder.borrow().as_ref() {
                            run(backend.clone());
                        }
                    });
                }
                btn_row.append(&eject_btn);
                card.append(&btn_row);

                list.append(&card);
            }
        })
    };

    let refresh_devices: Rc<dyn Fn()> = {
        let sidebar = sidebar.clone();
        let dev_sub_rows = dev_sub_rows.clone();
        let devices_expanded = devices_expanded.clone();
        let current_devices = current_devices.clone();
        let banner = dev_banner.clone();
        let banner_lbl = dev_banner_lbl.clone();
        let rebuild_overview = rebuild_overview.clone();
        // Guard against overlapping polls stacking up.
        let in_flight = Rc::new(Cell::new(false));
        Rc::new(move || {
            let sidebar = sidebar.clone();
            let dev_sub_rows = dev_sub_rows.clone();
            let devices_expanded = devices_expanded.clone();
            let current_devices_cb = current_devices.clone();
            let banner = banner.clone();
            let banner_lbl = banner_lbl.clone();
            let rebuild_overview = rebuild_overview.clone();
            // udisks2 access runs on a worker thread so a stalled D-Bus call
            // can never freeze the UI — a main-thread block previously made
            // the app impossible to quit or eject after a copy.
            refresh_device_cache(
                current_devices.clone(),
                in_flight.clone(),
                Rc::new(move |outcome| {
                    match outcome {
                        DeviceRefreshOutcome::Ok => {
                            banner.set_visible(false);
                            // `refresh_device_cache` already wrote the merged,
                            // sorted list into `current_devices`.
                            let devs = current_devices_cb.borrow();
                            let want: Vec<String> =
                                devs.iter().map(|d| format!("dev:{}", d.backend_id)).collect();
                            // Remove rows for devices that went away.
                            dev_sub_rows.borrow_mut().retain(|r| {
                                let keep = want.contains(&r.widget_name().to_string());
                                if !keep {
                                    sidebar.remove(r);
                                }
                                keep
                            });
                            // Add rows for new devices; update free-space bars in
                            // place so selection isn't disturbed when unchanged.
                            let expanded = devices_expanded.get();
                            for d in devs.iter() {
                                let name = format!("dev:{}", d.backend_id);
                                let used = if d.total_bytes > 0 {
                                    1.0 - (d.free_bytes as f64 / d.total_bytes as f64)
                                } else {
                                    0.0
                                };
                                let base = if d.label.is_empty() {
                                    "Untitled device".to_string()
                                } else {
                                    d.label.clone()
                                };
                                // Status glyphs: ⚠ unsupported fs, 🔒 read-only.
                                let label_text = format!(
                                    "{}{base}",
                                    device_glyph_prefix(d.read_only, &d.fs_type)
                                );
                                let existing = dev_sub_rows
                                    .borrow()
                                    .iter()
                                    .find(|r| r.widget_name().as_str() == name)
                                    .cloned();
                                match existing {
                                    Some(row) => {
                                        if let Some(bx) =
                                            row.child().and_then(|c| c.downcast::<GtkBox>().ok())
                                        {
                                            // Keep the label current (e.g. an MTP
                                            // device whose friendly name resolved
                                            // after the first poll).
                                            if let Some(lbl) = bx
                                                .first_child()
                                                .and_then(|c| c.downcast::<Label>().ok())
                                            {
                                                lbl.set_text(&gtk_safe(&label_text));
                                            }
                                            if let Some(bar) = bx
                                                .last_child()
                                                .and_then(|c| c.downcast::<gtk4::LevelBar>().ok())
                                            {
                                                bar.set_value(used);
                                                set_levelbar_fullness(&bar, used);
                                            }
                                        }
                                    }
                                    None => {
                                        let bx = GtkBox::new(Orientation::Vertical, 2);
                                        bx.set_margin_start(sidebar::SUB_ROW_INSET);
                                        bx.set_margin_end(8);
                                        bx.set_margin_top(4);
                                        bx.set_margin_bottom(4);
                                        let lbl = Label::builder()
                                            .label(&gtk_safe(&label_text))
                                            .halign(Align::Start)
                                            .xalign(0.0)
                                            .build();
                                        let bar = gtk4::LevelBar::new();
                                        bar.set_min_value(0.0);
                                        bar.set_max_value(1.0);
                                        bar.set_value(used);
                                        set_levelbar_fullness(&bar, used);
                                        bx.append(&lbl);
                                        bx.append(&bar);
                                        let row = ListBoxRow::new();
                                        row.set_widget_name(&name);
                                        row.set_child(Some(&bx));
                                        row.set_visible(expanded);
                                        if device_fs_unsupported(&d.fs_type) {
                                            row.set_tooltip_text(Some(UNSUPPORTED_FS_TOOLTIP));
                                        }
                                        sidebar.append(&row);
                                        dev_sub_rows.borrow_mut().push(row);
                                    }
                                }
                            }
                        }
                        // udisks failed — MTP (if any) is hidden until it recovers.
                        // `refresh_device_cache` already cleared `current_devices`.
                        DeviceRefreshOutcome::UdisksError(e) => {
                            for r in dev_sub_rows.borrow_mut().drain(..) {
                                sidebar.remove(&r);
                            }
                            use crate::devices::diagnostics::{self, Diagnosis};
                            let diag = diagnostics::classify(
                                diagnostics::has_udisks_grant(&diagnostics::read_flatpak_info()),
                                &diagnostics::read_distro_info(),
                                crate::devices::detect::classify_error(&e),
                            );
                            let msg = match diag {
                                Diagnosis::PermissionOff => {
                                    "Can't access drives — Sparkamp needs permission to use the \
                                     system disk service. Enable org.freedesktop.UDisks2 under \
                                     System Bus in Flatseal, then Retry."
                                }
                                Diagnosis::NotInstalled => {
                                    "Can't access drives — your system's disk service (udisks2) \
                                     isn't installed. Install it, then Retry."
                                }
                                Diagnosis::EjectUnavailable => {
                                    "Couldn't reach the disk service. Retry, or manage the device \
                                     through your file browser."
                                }
                            };
                            banner_lbl.set_text(msg);
                            banner.set_visible(true);
                        }
                        // The worker thread panicked. `refresh_device_cache`
                        // already cleared `current_devices`.
                        DeviceRefreshOutcome::WorkerPanicked => {
                            for r in dev_sub_rows.borrow_mut().drain(..) {
                                sidebar.remove(&r);
                            }
                            banner_lbl.set_text("Couldn't query the device service.");
                            banner.set_visible(true);
                        }
                    }
                    // Keep the overview list in sync with the latest results.
                    rebuild_overview();
                }),
            );
        })
    };

    // Initial scan + 2 s poll (stops once the window — hence the sidebar — is gone).
    refresh_devices();
    {
        let refresh = refresh_devices.clone();
        let sidebar_weak = sidebar.downgrade();
        glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
            if sidebar_weak.upgrade().is_none() {
                return glib::ControlFlow::Break;
            }
            refresh();
            glib::ControlFlow::Continue
        });
    }
    {
        let refresh = refresh_devices.clone();
        dev_banner_retry.connect_clicked(move |_| refresh());
    }

    // Selecting a device (or the Devices header) shows the devices page.
    {
        let stack_ref = stack.clone();
        let current = current_devices.clone();
        let title = dev_title.clone();
        let capacity = dev_capacity.clone();
        let levelbar = dev_levelbar.clone();
        let eject = dev_eject.clone();
        let sel_backend = selected_dev_backend.clone();
        let exp = devices_expanded.clone();
        let path_lbl = dev_path.clone();
        let overview = dev_overview.clone();
        let detail = dev_detail.clone();
        let warn = dev_warn.clone();
        let ro_badge = dev_ro_badge.clone();
        let warn_badge = dev_warn_badge.clone();
        let transfers_sel = device_transfers.clone();
        let rebuild_overview_sel = rebuild_overview.clone();
        let reload_dev_playlists_sel = reload_dev_playlists.clone();
        let reload_device_store_sel = reload_device_store.clone();
        let dev_named_cols_sel = dev_named_cols.clone();
        let dev_col_view_sel = dev_col_view.clone();
        let state_devcols = state.clone();
        let sync_btn = dev_sync.clone();
        let scan_btn = dev_scan.clone();
        // Sections hidden behind the "no filesystem" banner.
        let nofs_banner = dev_nofs_banner.clone();
        let nofs_lbl_sel = dev_nofs_lbl.clone();
        let pl_header_sel = dev_pl_header.clone();
        let pl_scroll_sel = dev_pl_scroll.clone();
        let pl_actions_sel = dev_pl_actions.clone();
        let tracks_scroll_sel = dev_tracks_scroll.clone();
        let file_actions_sel = dev_file_actions.clone();
        let store_sel = dev_store.clone();
        let counts_sel = dev_counts.clone();
        sidebar.connect_row_selected(move |_, opt_row| {
            let Some(row) = opt_row else { return };
            let name = row.widget_name().to_string();
            if name == "devices" {
                // Overview mode: list every connected device.
                stack_ref.set_visible_child_name("devices");
                rebuild_overview_sel();
                overview.set_visible(true);
                detail.set_visible(false);
                *sel_backend.borrow_mut() = None;
                if !exp.get() {
                    exp.set(true);
                }
            } else if let Some(backend) = name.strip_prefix("dev:") {
                stack_ref.set_visible_child_name("devices");
                if let Some(d) = current.borrow().iter().find(|d| d.backend_id == backend) {
                    // Detail mode for the selected device.
                    overview.set_visible(false);
                    detail.set_visible(true);
                    // Re-apply the shared column config so device columns track
                    // changes made in the files view (same as the editor does).
                    apply_ml_columns_to(&dev_col_view_sel, &dev_named_cols_sel, &state_devcols, 1);
                    let base = if d.label.is_empty() {
                        "Untitled device".to_string()
                    } else {
                        d.label.clone()
                    };
                    // Name in the header; status shown as pill badges instead
                    // of inline glyphs.
                    title.set_text(&gtk_safe(&base));
                    path_lbl.set_text(&gtk_safe(&format!(
                        "{} · {}",
                        if d.fs_type.is_empty() { "unknown" } else { &d.fs_type },
                        d.mount_path.to_string_lossy(),
                    )));
                    ro_badge.set_visible(d.read_only);
                    let unsupported = device_fs_unsupported(&d.fs_type);
                    warn_badge.set_visible(unsupported);
                    let used_bytes = d.total_bytes.saturating_sub(d.free_bytes);
                    capacity.set_text(&format!(
                        "{:.1} GB used · {:.1} GB free · {:.1} GB total",
                        used_bytes as f64 / 1e9,
                        d.free_bytes as f64 / 1e9,
                        d.total_bytes as f64 / 1e9,
                    ));
                    if unsupported {
                        warn.set_text("⚠ NTFS/exFAT — limited support");
                        warn.set_tooltip_text(Some(UNSUPPORTED_FS_TOOLTIP));
                        warn.set_visible(true);
                    } else {
                        warn.set_visible(false);
                    }
                    let unsupported_dev =
                        d.backend == crate::devices::DeviceBackend::Unsupported;
                    let used = if d.total_bytes > 0 {
                        1.0 - d.free_bytes as f64 / d.total_bytes as f64
                    } else {
                        0.0
                    };
                    levelbar.set_value(used);
                    set_levelbar_fullness(&levelbar, used);
                    // No capacity is knowable for a photo/iOS mount — hide the bar.
                    levelbar.set_visible(!unsupported_dev);
                    // Eject is unavailable while a copy to this device is running.
                    let busy = transfers_sel.borrow().contains_key(&d.backend_id);
                    eject.set_sensitive(d.ejectable && !busy);
                    sync_btn.set_sensitive(true);
                    scan_btn.set_sensitive(true);
                    *sel_backend.borrow_mut() = Some(d.backend_id.clone());

                    if unsupported_dev {
                        // Apple iOS / PTP photo device: detected, but not a music
                        // sync target. Explain why and disable Sync/Scan. Eject
                        // stays available so the user can disconnect cleanly.
                        warn.set_visible(false);
                        capacity.set_text("Capacity unavailable");
                        nofs_lbl_sel.set_text(unsupported_device_banner(&d.backend_id));
                        nofs_banner.set_visible(true);
                        pl_header_sel.set_visible(false);
                        pl_scroll_sel.set_visible(false);
                        pl_actions_sel.set_visible(false);
                        tracks_scroll_sel.set_visible(false);
                        file_actions_sel.set_visible(false);
                        store_sel.remove_all();
                        counts_sel.set_text("Not a music-sync device");
                        sync_btn.set_sensitive(false);
                        scan_btn.set_sensitive(false);
                    } else if d.fs_visible {
                        // Normal device: show the lists, hide the banner.
                        nofs_banner.set_visible(false);
                        pl_header_sel.set_visible(true);
                        pl_scroll_sel.set_visible(true);
                        tracks_scroll_sel.set_visible(true);
                        file_actions_sel.set_visible(true);
                        sync_btn.set_sensitive(true);
                        scan_btn.set_sensitive(true);

                        // Rebuild the playlist filter rows ("All files" + each
                        // device .m3u/.m3u8); selecting "All files" resets the
                        // filter via the playlist-list handler.
                        reload_dev_playlists_sel(d.clone());

                        // Read device tags off the UI thread, then fill columns.
                        reload_device_store_sel(d.clone());
                    } else {
                        // Connected but no readable filesystem: show the banner
                        // in place of empty lists. Eject stays available so the
                        // user can disconnect; Sync/Scan are pointless here.
                        nofs_lbl_sel.set_text(
                            "⚠ No visible filesystem on this device. Set the phone to \
                             file-transfer mode and allow access, or reconnect it, then \
                             press Scan.",
                        );
                        nofs_banner.set_visible(true);
                        pl_header_sel.set_visible(false);
                        pl_scroll_sel.set_visible(false);
                        pl_actions_sel.set_visible(false);
                        tracks_scroll_sel.set_visible(false);
                        file_actions_sel.set_visible(false);
                        store_sel.remove_all();
                        counts_sel.set_text("No visible filesystem");
                        sync_btn.set_sensitive(false);
                        scan_btn.set_sensitive(false);
                    }
                }
            }
        });
    }

    // Scan: re-read tags + duration from the files on the selected device, and
    // refresh the playlist chips. Same work the device-select does, on demand.
    {
        let devices_scan = current_devices.clone();
        let sel_backend = selected_dev_backend.clone();
        let reload_store = reload_device_store.clone();
        let reload_pls = reload_dev_playlists.clone();
        dev_scan.connect_clicked(move |_| {
            let Some(backend) = sel_backend.borrow().clone() else { return };
            let dev = devices_scan
                .borrow()
                .iter()
                .find(|d| d.backend_id == backend)
                .cloned();
            let Some(dev) = dev else { return };
            reload_pls(dev.clone());
            reload_store(dev);
        });
    }

    // Eject: unmount + power off a device, then refresh the list. Shared by
    // the detail Eject button and each overview row's Eject button.
    let eject_run: Rc<dyn Fn(String)> = {
        let refresh = refresh_devices.clone();
        let sidebar_ej = sidebar.clone();
        let win_wk_ej = win.downgrade();
        Rc::new(move |backend: String| {
            let refresh = refresh.clone();
            let sidebar_ej = sidebar_ej.clone();
            let win_wk = win_wk_ej.clone();
            // MTP devices have no udisks2 block object — unmount through gvfs
            // (gio) on the main thread instead; the unmount itself is async.
            if backend.starts_with("mtp://") || backend.starts_with("gphoto2://") {
                // Forget cached metadata so a later replug of the same URI
                // re-reads the device rather than showing stale capacity.
                invalidate_mtp_meta(&backend);
                let monitor = gio::VolumeMonitor::get();
                let mount = monitor
                    .mounts()
                    .into_iter()
                    .find(|m| m.root().uri() == backend);
                let Some(mount) = mount else {
                    refresh();
                    return;
                };
                let refresh2 = refresh.clone();
                let sidebar2 = sidebar_ej.clone();
                let win2 = win_wk.clone();
                mount.unmount_with_operation(
                    gio::MountUnmountFlags::NONE,
                    None::<&gio::MountOperation>,
                    gio::Cancellable::NONE,
                    move |res| match res {
                        Ok(()) => {
                            refresh2();
                            if let Some(r) = find_row_by_name(&sidebar2, "devices") {
                                sidebar2.select_row(Some(&r));
                            }
                        }
                        Err(e) => {
                            show_alert_parented(
                                win2.upgrade().as_ref(),
                                &format!(
                                    "Couldn't disconnect the device ({e}). Close anything \
                                     using it and try again."
                                ),
                            );
                        }
                    },
                );
                return;
            }
            // Run the unmount/power-off on a worker thread so a busy device
            // can't freeze the UI.
            glib::spawn_future_local(async move {
                let res =
                    gio::spawn_blocking(move || crate::devices::detect::eject(&backend)).await;
                match res {
                    Ok(Ok(())) => {
                        refresh();
                        // The detail view may now show a device that's gone —
                        // return to the Devices overview.
                        if let Some(r) = find_row_by_name(&sidebar_ej, "devices") {
                            sidebar_ej.select_row(Some(&r));
                        }
                    }
                    Ok(Err(e)) => {
                        let dialog = gtk4::AlertDialog::builder()
                            .message("Couldn't eject")
                            .detail(format!(
                                "The device is still busy or couldn't be ejected ({e}). \
                                 Close anything using it and try again, or eject it from \
                                 your file browser."
                            ))
                            .modal(true)
                            .build();
                        dialog.show(win_wk.upgrade().as_ref());
                    }
                    Err(_) => {
                        show_alert_parented(
                            win_wk.upgrade().as_ref(),
                            "Eject failed unexpectedly.",
                        );
                    }
                }
            });
        })
    };
    *eject_run_holder.borrow_mut() = Some(eject_run.clone());
    {
        let sel_backend = selected_dev_backend.clone();
        let eject_run = eject_run.clone();
        dev_eject.connect_clicked(move |btn| {
            let Some(backend) = sel_backend.borrow().clone() else { return };
            btn.set_sensitive(false);
            eject_run(backend);
        });
    }

    // Sync: compare tags on each side of every pair, confirm en masse, apply.
    // Shared by the detail Sync button and each overview row's Sync button.
    let sync_run: Rc<dyn Fn(crate::devices::Device, Button)> = {
        let state_sync = state.clone();
        let win_wk = win.downgrade();
        let reload_sync = reload_device_store.clone();
        Rc::new(move |dev: crate::devices::Device, sync_btn: Button| {
            use crate::devices::sync::{PlaylistSyncDir, SyncAction};
            // Show activity while the device is read/planned (slow over MTP);
            // restored on every exit path below, just before a dialog/alert.
            set_button_busy(&sync_btn, true, "Sync");
            // Compute both sync plans on a worker thread — reading device tags
            // and playlist files over a slow MTP FUSE mount on the UI thread
            // froze the app. A throwaway read-only library handle is opened on
            // that thread (same pattern as the scan workers).
            let ext = state_sync
                .borrow()
                .config
                .media_library
                .playlist_format
                .extension()
                .to_string();
            let db_path = crate::media_library::MediaLibrary::db_path_pub();
            let state_sync = state_sync.clone();
            let win_wk = win_wk.clone();
            let reload_sync = reload_sync.clone();
            glib::spawn_future_local(async move {
                let dev_b = dev.clone();
                let (plan, pl_plan) = gio::spawn_blocking(move || {
                    if device_io_shutting_down() {
                        return (Vec::new(), Vec::new());
                    }
                    match crate::media_library::MediaLibrary::open_at(&db_path) {
                        Ok(lib) => (
                            device_sync_plan(&lib, &dev_b),
                            device_playlist_sync_plan(&lib, &dev_b, &ext),
                        ),
                        Err(_) => (Vec::new(), Vec::new()),
                    }
                })
                .await
                .unwrap_or((Vec::new(), Vec::new()));
            let to_lib = plan
                .iter()
                .filter(|(_, a)| *a == SyncAction::DeviceToLibrary)
                .count();
            let to_dev = plan
                .iter()
                .filter(|(_, a)| *a == SyncAction::LibraryToDevice)
                .count();
            let song_conflict = plan
                .iter()
                .filter(|(_, a)| *a == SyncAction::Conflict)
                .count();
            let pl_push = pl_plan.iter().filter(|i| i.dir == PlaylistSyncDir::Push).count();
            let pl_pull = pl_plan.iter().filter(|i| i.dir == PlaylistSyncDir::Pull).count();
            let pl_conflict = pl_plan
                .iter()
                .filter(|i| i.dir == PlaylistSyncDir::Conflict)
                .count();
            if to_lib == 0
                && to_dev == 0
                && song_conflict == 0
                && pl_push == 0
                && pl_pull == 0
                && pl_conflict == 0
            {
                set_button_busy(&sync_btn, false, "Sync");
                show_alert_parented(
                    win_wk.upgrade().as_ref(),
                    "Already in sync — no tag or playlist changes to apply.",
                );
                return;
            }
            let dname = if dev.label.is_empty() {
                "The device".to_string()
            } else {
                dev.label.clone()
            };
            let mut pl_bits: Vec<String> = Vec::new();
            if song_conflict > 0 {
                pl_bits.push(format!(
                    "{song_conflict} song conflict{} to resolve",
                    if song_conflict == 1 { "" } else { "s" }
                ));
            }
            if pl_push + pl_pull > 0 {
                pl_bits.push(format!(
                    "{} playlist{} to update",
                    pl_push + pl_pull,
                    if pl_push + pl_pull == 1 { "" } else { "s" }
                ));
            }
            if pl_conflict > 0 {
                pl_bits.push(format!(
                    "{pl_conflict} playlist conflict{} to resolve",
                    if pl_conflict == 1 { "" } else { "s" }
                ));
            }
            let pl_line = if pl_bits.is_empty() {
                String::new()
            } else {
                format!(" {}.", pl_bits.join(", "))
            };
            let detail = format!(
                "{dname} has {to_lib} updated song{}, this computer has {to_dev} updated song{}.{pl_line} \
                 Sync all changes?",
                if to_lib == 1 { "" } else { "s" },
                if to_dev == 1 { "" } else { "s" },
            );
            // Planning done — restore the button; the modal dialog now drives
            // the rest of the flow.
            set_button_busy(&sync_btn, false, "Sync");
            let dialog = gtk4::AlertDialog::builder()
                .message("Sync device")
                .detail(detail)
                .buttons(vec!["Cancel".to_string(), "Sync".to_string()])
                .cancel_button(0)
                .default_button(1)
                .modal(true)
                .build();
            let state2 = state_sync.clone();
            let dev2 = dev.clone();
            let plan2 = plan;
            let pl_plan2 = pl_plan;
            let win_wk2 = win_wk.clone();
            let reload2 = reload_sync.clone();
            dialog.choose(
                win_wk.upgrade().as_ref(),
                None::<&gio::Cancellable>,
                move |res| {
                    if res != Ok(1) {
                        return;
                    }
                    let (applied, failed) = apply_device_sync(&state2, &dev2, &plan2);
                    // Auto-apply the unambiguous playlist directions; collect the
                    // both-changed conflicts to prompt for afterwards.
                    let mut pl_updated = 0usize;
                    let mut pl_copied = 0usize;
                    let mut conflicts: Vec<PlaylistSyncItem> = Vec::new();
                    for item in &pl_plan2 {
                        match item.dir {
                            PlaylistSyncDir::Push => {
                                let (c, ok) = apply_playlist_push(&state2, &dev2, item);
                                pl_copied += c;
                                if ok {
                                    pl_updated += 1;
                                }
                            }
                            PlaylistSyncDir::Pull => {
                                if apply_playlist_pull(&state2, item) {
                                    pl_updated += 1;
                                }
                            }
                            PlaylistSyncDir::Conflict => conflicts.push(item.clone()),
                            PlaylistSyncDir::None => {}
                        }
                    }
                    reload2(dev2.clone());

                    let summary = {
                        let tail = if failed > 0 {
                            format!(", {failed} failed")
                        } else {
                            String::new()
                        };
                        let pl_tail = if pl_updated > 0 {
                            format!(
                                "; updated {pl_updated} playlist{} ({pl_copied} new file{} copied)",
                                if pl_updated == 1 { "" } else { "s" },
                                if pl_copied == 1 { "" } else { "s" },
                            )
                        } else {
                            String::new()
                        };
                        format!(
                            "Synced {applied} song{}{pl_tail}{tail}.",
                            if applied == 1 { "" } else { "s" }
                        )
                    };

                    // Per-file tag conflicts (both sides changed a song's tags).
                    let tag_conflicts = build_tag_conflicts(&dev2, &plan2);

                    // Final step: refresh + show the summary.
                    let final_done: Rc<dyn Fn()> = {
                        let reload_done = reload2.clone();
                        let dev_done = dev2.clone();
                        let win_done = win_wk2.clone();
                        Rc::new(move || {
                            reload_done(dev_done.clone());
                            show_alert_parented(win_done.upgrade().as_ref(), &summary);
                        })
                    };
                    // After tag conflicts, resolve playlist conflicts, then finish.
                    let after_tags: Rc<dyn Fn()> = if conflicts.is_empty() {
                        final_done
                    } else {
                        let state_pl = state2.clone();
                        let dev_pl = dev2.clone();
                        let win_pl = win_wk2.clone();
                        Rc::new(move || {
                            prompt_playlist_conflicts(
                                state_pl.clone(),
                                dev_pl.clone(),
                                conflicts.clone(),
                                win_pl.clone(),
                                final_done.clone(),
                            );
                        })
                    };
                    if tag_conflicts.is_empty() {
                        (after_tags)();
                    } else {
                        prompt_tag_conflicts(
                            state2.clone(),
                            dev2.clone(),
                            tag_conflicts,
                            win_wk2.clone(),
                            after_tags,
                        );
                    }
                },
            );
            });
        })
    };
    *sync_run_holder.borrow_mut() = Some(sync_run.clone());
    {
        let devices_sync = current_devices.clone();
        let sel_backend = selected_dev_backend.clone();
        let sync_run = sync_run.clone();
        dev_sync.connect_clicked(move |btn| {
            let Some(backend) = sel_backend.borrow().clone() else { return };
            let dev = devices_sync
                .borrow()
                .iter()
                .find(|d| d.backend_id == backend)
                .cloned();
            let Some(dev) = dev else { return };
            sync_run(dev, btn.clone());
        });
    }
}
