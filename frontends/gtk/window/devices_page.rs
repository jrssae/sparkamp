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
    gdk, gio, glib, Align, Box as GtkBox, Button, ColumnView,
    DropTarget, Image, Label, MultiSelection, Orientation, PolicyType, ScrolledWindow,
    SortListModel,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

// `sidebar` is imported as a module (for its `SUB_ROW_INSET`) as well as by
// type — the `let sidebar` binding below shadows only the value namespace, so
// `sidebar::…` still resolves.
use super::sidebar::Sidebar;
// Cuts split out of this page — siblings of it, not children, for the same
// reason the disc modules are flat.
use super::{devices_actions, devices_columns, devices_menu, devices_playlists, devices_poll};
// Everything else is private to the parent module, which a child may still
// use. Three groups: shared Media Library chrome (columns, status bar, search
// row, popovers, sidebar row lookup), the device helpers from the `devices.rs`
// slice, and the playlist-sync helpers the Sync button drives.
use super::{
    apply_card_progress, apply_ml_columns_to, device_delete_files,
    device_fs_unsupported, device_glyph_prefix, device_io_shutting_down,
    device_m3u_remove_basenames, device_plan_fs, device_plan_one, device_record_pair,
    device_recorded_relpath, device_sync_id, find_row_by_name, gtk_safe, invalidate_mtp_meta,
    lib_track_matches_query, make_view_search_row, ml_status_bar,     set_levelbar_fullness, show_toast, unsupported_device_banner,
    MlCtx, ML_SEARCH_ENTRY_NAME,
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
    let current_devices = ctx.host.current_devices.clone();
    let copy_files_holder = ctx.host.copy_files_holder.clone();
    let win = ctx.win.clone();
    let stack = ctx.stack.clone();
    let sidebar = sb.list.clone();
    let devices_expanded = sb.devices_expanded.clone();
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
    // Which device `dev_all_tracks` actually holds, by `backend_id`.
    //
    // The cache is single — it describes whichever device's detail view was
    // populated last — so anything reading it for a SPECIFIC device has to
    // check it is looking at that device's files and not another's. The
    // Devices overview draws one draggable card per device, and without this
    // a card for a device you had not opened dragged the open one's files
    // instead, silently.
    //
    // Deliberately not `selected_dev_backend`: that is a view-state field set
    // to `None` whenever the overview is showing, which is exactly when these
    // cards exist, so testing against it would refuse every card drag —
    // including the single-device case that works. This tracks the cache's
    // contents, not the visible page.
    let dev_all_tracks_owner: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
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
    // Marks the entry Ctrl+F should focus when this page is the visible
    // one — see the widget-name walk in media_library.rs.
    dev_search_entry.set_widget_name(ML_SEARCH_ENTRY_NAME);
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

    // ── Device track view columns ─────────────────────────────────────────
    // Split to `devices_columns.rs` (plan step 6). Driven by the shared
    // ALL_COLUMNS table, plus the two device-only columns.
    let devices_columns::Columns {
        dev_pos_col,
        dev_named_cols,
    } = devices_columns::build(
        &state,
        devices_columns::ColumnUi {
            dev_col_view: &dev_col_view,
            dev_selection: &dev_selection,
            dev_sort_model: &dev_sort_model,
            dev_pair_map: &dev_pair_map,
            dev_row_menu_holder: &dev_row_menu_holder,
        },
    );

    // Backend object id of the currently shown device (Eject/Sync target).
    let selected_dev_backend: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Reload a device's tracks into the column store (tags re-read on a worker
    // thread). Used on device select and after a sync so changed values show
    // immediately.
    let reload_device_store: Rc<dyn Fn(crate::devices::Device)> = {
        let store = dev_store.clone();
        let all_tracks = dev_all_tracks.clone();
        let all_tracks_owner = dev_all_tracks_owner.clone();
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
            let all_tracks_owner2 = all_tracks_owner.clone();
            let owner_id = dev.backend_id.clone();
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
                // Stamped with the cache, never apart from it, so the two can
                // never disagree about whose files these are.
                *all_tracks_owner2.borrow_mut() = Some(owner_id.clone());
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

    // ── Device playlists ──────────────────────────────────────────────────
    // Split to `devices_playlists.rs` (plan step 6): sending a library
    // playlist to a device, and New / Rename / Duplicate / Delete on the ones
    // already there.
    let current_device_for_actions = devices_playlists::connect(
        ctx,
        sb,
        devices_playlists::PlaylistUi {
            dev_pl_new: &dev_pl_new,
            dev_pl_rename: &dev_pl_rename,
            dev_pl_duplicate: &dev_pl_duplicate,
            dev_pl_delete: &dev_pl_delete,
            dev_eject: &dev_eject,
            dev_hint: &dev_hint,
            dev_progress: &dev_progress,
            selected_dev_backend: &selected_dev_backend,
            selected_dev_playlist: &selected_dev_playlist,
            reload_dev_playlists: &reload_dev_playlists,
            reload_device_store: &reload_device_store,
            update_card_progress: &update_card_progress,
        },
    );

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
            // Precondition blocks, not destructive gates — nothing to undo.
            if dev.read_only {
                let n = if dev.label.is_empty() { "This device" } else { &dev.label };
                if let Some(w) = win_wk.upgrade() {
                    show_toast(&w, &format!("{n} is read-only — can't copy files to it."));
                }
                return;
            }
            if device_fs_unsupported(&dev.fs_type) {
                if let Some(w) = win_wk.upgrade() {
                    show_toast(
                        &w,
                        &format!(
                            "{} is an unsupported filesystem — can't write to this device yet.",
                            dev.fs_type
                        ),
                    );
                }
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
                    if let Some(w) = win_wk.upgrade() {
                        show_toast(
                            &w,
                            &format!(
                                "Not enough space on the device: need {:.1} GB, {:.1} GB free.",
                                need as f64 / 1e9,
                                dev.free_bytes as f64 / 1e9
                            ),
                        );
                    }
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
                // Completion summary, not a gate — the copy already ran.
                if let Some(w) = win2.upgrade() {
                    show_toast(
                        &w,
                        &format!("Copied {copied}, skipped {skipped}, failed {failed} to {dname}."),
                    );
                }
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
                super::playlist_add::add_track(&state, crate::model::Track::from(lt), false);
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
                super::playlist_add::add_track(&state, crate::model::Track::from(lt), false);
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
                        // Non-fatal: the delete already happened (confirmed above), this
                        // just reports a partial failure. Nothing left to gate.
                        if failed > 0 {
                            if let Some(w) = win_wk2.upgrade() {
                                show_toast(&w, &format!("{failed} file(s) couldn't be deleted."));
                            }
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

    // ── Device row context menu ───────────────────────────────────────────
    // Split to `devices_menu.rs` (plan step 6), the same way files_menu.rs
    // splits the Files page's row menu.
    devices_menu::connect(
        ctx,
        devices_menu::MenuUi {
            dev_tracks_scroll: &dev_tracks_scroll,
            dev_col_view: &dev_col_view,
            dev_status: &dev_status,
            dev_row_menu_holder: &dev_row_menu_holder,
            selected_dev_backend: &selected_dev_backend,
            selected_device_tracks: &selected_device_tracks,
            reload_device_store: &reload_device_store,
        },
    );

    dev_page.append(&dev_detail);
    stack.add_named(&dev_page, Some("devices"));

    // ── Device detection ──────────────────────────────────────────────────
    // Split to `devices_poll.rs` (plan step 6): the 2 s udisks2 poll, the
    // overview cards, and the sidebar sub-rows they keep live.
    let devices_poll::Poll {
        rebuild_overview,
        refresh_devices,
        eject_run_holder,
        sync_run_holder,
    } = devices_poll::start(
        ctx,
        sb,
        devices_poll::PollUi {
            dev_overview_list: &dev_overview_list,
            dev_banner: &dev_banner,
            dev_banner_lbl: &dev_banner_lbl,
            dev_banner_retry: &dev_banner_retry,
            device_counts: &device_counts,
            counts_in_flight: &counts_in_flight,
            device_transfers: &device_transfers,
            device_card_progress: &device_card_progress,
            dev_all_tracks: &dev_all_tracks,
            dev_all_tracks_owner: &dev_all_tracks_owner,
        },
    );

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

    // Scan / Eject / Sync — split to `devices_actions.rs` (plan step 6).
    devices_actions::connect(
        ctx,
        sb,
        devices_actions::ActionUi {
            dev_scan: &dev_scan,
            dev_eject: &dev_eject,
            dev_sync: &dev_sync,
            selected_dev_backend: &selected_dev_backend,
            reload_device_store: &reload_device_store,
            reload_dev_playlists: &reload_dev_playlists,
            refresh_devices: &refresh_devices,
            eject_run_holder: &eject_run_holder,
            sync_run_holder: &sync_run_holder,
            dev_progress: &dev_progress,
        },
    );
}
