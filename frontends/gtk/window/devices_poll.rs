//! Device detection: the udisks2 poll, the overview cards, and the sidebar
//! rows they keep live.
//!
//! Split from [`super::devices_page`] (plan step 6, third cut). Everything
//! here answers one question — *which devices are attached right now?* — and
//! pushes the answer into three places: the sidebar's Devices sub-rows, the
//! overview card list, and the shared `current_devices` vector the rest of
//! the window (including player.rs's Send-to menu) reads.
//!
//! A 2 s poll rather than D-Bus signal wiring: it keeps this simple while
//! still updating in place, so devices appear and disappear and free space
//! refreshes without reopening the window.
//!
//! The two runner holders are declared here rather than in
//! [`super::devices_actions`], which fills them, because each overview card's
//! Sync and Eject buttons are built *here* and have to call through them —
//! the cards exist long before the detail view's buttons are wired. That is
//! the holder pattern from docs/gtk-breakup-plan.md §3.1: **a holder left
//! `None` is not an error, it is a silent no-op.**

use gtk4::prelude::*;
use gtk4::{gio, glib, Align, Box as GtkBox, Button, Image, Label, ListBoxRow, Orientation};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::sidebar::{self, Sidebar};
use super::{
    apply_card_progress, counts_text, device_fs_unsupported, device_glyph_prefix,
    device_icon_name, device_io_shutting_down, find_row_by_name, gtk_safe, refresh_device_cache,
    set_levelbar_fullness, DeviceRefreshOutcome, MlCtx, UNSUPPORTED_FS_TOOLTIP,
};

/// The page-local cells the poll writes into.
pub(super) struct PollUi<'a> {
    /// The overview card list — rebuilt from scratch on every poll tick.
    pub dev_overview_list: &'a GtkBox,
    /// udisks2 diagnostics banner, shown only when the daemon can't be reached.
    pub dev_banner: &'a GtkBox,
    pub dev_banner_lbl: &'a Label,
    pub dev_banner_retry: &'a Button,
    /// Per-device (song, playlist) counts, and the guard against counting the
    /// same device twice concurrently.
    pub device_counts: &'a Rc<RefCell<std::collections::HashMap<String, (usize, usize)>>>,
    pub counts_in_flight: &'a Rc<RefCell<std::collections::HashSet<String>>>,
    /// Live copy progress per device, and each card's progress bar.
    pub device_transfers: &'a Rc<RefCell<std::collections::HashMap<String, (usize, usize)>>>,
    pub device_card_progress: &'a Rc<RefCell<std::collections::HashMap<String, gtk4::ProgressBar>>>,
    /// The currently-browsed device's cached file list (`devices_page.rs`'s
    /// `dev_all_tracks`), so an overview card can be dragged onto the active
    /// playlist too — every file on the device, per the container rule.
    pub dev_all_tracks: &'a Rc<RefCell<Vec<sparkamp::media_library::LibTrack>>>,
    /// Which device `dev_all_tracks` currently holds, by `backend_id`. A card
    /// drag checks this before shipping anything, since the cache is single
    /// and describes only the device whose detail view was populated last.
    pub dev_all_tracks_owner: &'a Rc<RefCell<Option<String>>>,
}

/// What the rest of the page needs back from the poll.
pub(super) struct Poll {
    /// Re-render the overview cards from the latest detection results.
    pub rebuild_overview: Rc<dyn Fn()>,
    /// Re-poll udisks2, then refresh the sidebar rows and the overview.
    pub refresh_devices: Rc<dyn Fn()>,
    /// Filled by [`super::devices_actions`]; called by each card's button.
    pub eject_run_holder: Rc<RefCell<Option<Rc<dyn Fn(String)>>>>,
    pub sync_run_holder: Rc<RefCell<Option<Rc<dyn Fn(sparkamp::devices::Device, Button)>>>>,
}

/// Build the overview list and start the 2 s detection poll.
pub(super) fn start(ctx: &MlCtx, sb: &Sidebar, ui: PollUi<'_>) -> Poll {
    // Local names for what this takes from its context and its caller, so the
    // body below reads as it did inside `devices_page::build`.
    let current_devices = ctx.host.current_devices.clone();
    let sidebar = sb.list.clone();
    let devices_expanded = sb.devices_expanded.clone();
    let dev_sub_rows = sb.dev_sub_rows.clone();
    let dev_overview_list = ui.dev_overview_list.clone();
    let dev_banner = ui.dev_banner.clone();
    let dev_banner_lbl = ui.dev_banner_lbl.clone();
    let dev_banner_retry = ui.dev_banner_retry.clone();
    let device_counts = ui.device_counts.clone();
    let counts_in_flight = ui.counts_in_flight.clone();
    let device_transfers = ui.device_transfers.clone();
    let device_card_progress = ui.device_card_progress.clone();
    let dev_all_tracks = ui.dev_all_tracks.clone();
    let dev_all_tracks_owner = ui.dev_all_tracks_owner.clone();

    // ── Device detection: poll udisks2 and keep the sidebar live ──────────
    // A 2 s poll (rather than D-Bus signal wiring) keeps this simple while
    // still updating in place — devices appear/disappear and free space
    // refreshes without reopening the window.
    // Deferred handles to the eject / sync runners (defined further down, once
    // the refresh + reload closures they need exist). The overview rows' Sync
    // and Eject buttons call through these.
    let eject_run_holder: Rc<RefCell<Option<Rc<dyn Fn(String)>>>> =
        Rc::new(RefCell::new(None));
    let sync_run_holder: Rc<RefCell<Option<Rc<dyn Fn(sparkamp::devices::Device, Button)>>>> =
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
        let all_tracks_ov = dev_all_tracks.clone();
        let all_tracks_owner_ov = dev_all_tracks_owner.clone();
        Rc::new(move || {
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            // Card progress bars are rebuilt below; drop the stale references.
            card_bars.borrow_mut().clear();
            let devs = current.borrow();
            if devs.is_empty() {
                // This is the one sub-view of the Devices page that has no
                // search entry of its own — the search box down in the
                // per-device track browser filters a different pane, once a
                // device is selected — so there is no "no results" variant
                // to distinguish here, only this single condition.
                list.append(&super::util::empty_state(
                    "drive-removable-media-symbolic",
                    "No devices connected",
                    Some("Connect a music player or USB drive"),
                ));
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
                                        sparkamp::devices::browse::list_audio_files(&mount).len();
                                    let pls = sparkamp::devices::browse::device_playlist_files(&mount)
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

                // Dragging the device drags everything on it, per the
                // container rule.
                //
                // `dev_all_tracks` is a single cache holding whichever
                // device's detail view was populated last (see
                // `reload_device_store` in devices_page.rs), not one per card,
                // so a card must confirm the cache is describing ITS device
                // before shipping any of it. Without that check, a card for a
                // device you had not opened dragged the open device's files
                // instead — wrong contents, no error.
                //
                // Mismatched or unpopulated, the closure returns nothing and
                // `attach_uri_drag` refuses the drag outright, which is the
                // honest answer: this card cannot say what is on its device
                // until that device has been opened.
                {
                    let entries_drag = all_tracks_ov.clone();
                    let owner_drag = all_tracks_owner_ov.clone();
                    let this_device = d.backend_id.clone();
                    super::ml_drag::attach_uri_drag(&card, move || {
                        if owner_drag.borrow().as_deref() != Some(this_device.as_str()) {
                            return Vec::new();
                        }
                        entries_drag.borrow().iter().map(|t| t.path.clone()).collect()
                    });
                }

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
                            use sparkamp::devices::diagnostics::{self, Diagnosis};
                            let diag = diagnostics::classify(
                                diagnostics::has_udisks_grant(&diagnostics::read_flatpak_info()),
                                &diagnostics::read_distro_info(),
                                sparkamp::devices::detect::classify_error(&e),
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

    Poll {
        rebuild_overview,
        refresh_devices,
        eject_run_holder,
        sync_run_holder,
    }
}
