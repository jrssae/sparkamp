//! Scan, Eject and Sync — the three device-wide buttons.
//!
//! Split from [`super::devices_page`] (plan step 6, fourth cut). All three
//! act on whichever device the detail view is showing, and two of them are
//! shared: the overview cards' per-row Eject and Sync buttons call the same
//! runners through `eject_run_holder` and `sync_run_holder`, which is why
//! those holders are declared by the poll and filled here rather than being
//! plain locals.
//!
//! **Scan** re-reads tags and durations off the device and refreshes its
//! playlist chips — the same work selecting the device does, on demand.
//!
//! **Eject** unmounts and powers the device off, then refreshes the list.
//!
//! **Sync** compares tags on each side of every synced pair, confirms the
//! differences en masse, and applies them; the heavy lifting lives in the
//! `devices.rs` helpers this calls.
//!
//! Nothing here is read back by the rest of the page.

use gtk4::prelude::*;
use gtk4::{gio, glib, Button};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::sidebar::Sidebar;
use super::{
    build_tag_conflicts,
    device_io_shutting_down, device_playlist_sync_plan, device_sync_plan, find_row_by_name,
    invalidate_mtp_meta, prompt_playlist_conflicts, prompt_tag_conflicts, set_button_busy,
    show_toast, MlCtx, PlaylistSyncItem,
};
use super::util::refresh_device_cache;

/// What the three buttons need from the page that built them.
pub(super) struct ActionUi<'a> {
    pub dev_scan: &'a Button,
    pub dev_eject: &'a Button,
    pub dev_sync: &'a Button,
    /// Backend object id of the device currently shown in the detail view.
    pub selected_dev_backend: &'a Rc<RefCell<Option<String>>>,
    /// Re-read a device's tracks into the column store.
    pub reload_device_store: &'a Rc<dyn Fn(crate::devices::Device)>,
    /// Rebuild a device's playlist filter chips.
    pub reload_dev_playlists: &'a Rc<dyn Fn(crate::devices::Device)>,
    /// Re-poll udisks2 and rebuild the sidebar rows and overview cards.
    pub refresh_devices: &'a Rc<dyn Fn()>,
    /// Filled here, called by each overview card's Eject button.
    pub eject_run_holder: &'a Rc<RefCell<Option<Rc<dyn Fn(String)>>>>,
    /// Filled here, called by each overview card's Sync button.
    pub sync_run_holder: &'a Rc<RefCell<Option<Rc<dyn Fn(crate::devices::Device, Button)>>>>,
    /// The detail view's progress bar, shared with the copy/playlist actions.
    /// Sync drives it from a worker thread now that it no longer blocks.
    pub dev_progress: &'a gtk4::ProgressBar,
}

/// Wire Scan, Eject and Sync, and publish the latter two through their holders.
pub(super) fn connect(ctx: &MlCtx, sb: &Sidebar, ui: ActionUi<'_>) {
    // Local names for what this takes from its context and its caller, so the
    // body below reads as it did inside `devices_page::build`.
    let state = ctx.host.state.clone();
    let current_devices = ctx.host.current_devices.clone();
    let win = ctx.win.clone();
    let sidebar = sb.list.clone();
    let dev_scan = ui.dev_scan.clone();
    let dev_eject = ui.dev_eject.clone();
    let dev_sync = ui.dev_sync.clone();
    let selected_dev_backend = ui.selected_dev_backend.clone();
    let reload_device_store = ui.reload_device_store.clone();
    let reload_dev_playlists = ui.reload_dev_playlists.clone();
    let refresh_devices = ui.refresh_devices.clone();
    let eject_run_holder = ui.eject_run_holder.clone();
    let sync_run_holder = ui.sync_run_holder.clone();

    // Scan: re-read tags + duration from the files on the selected device, and
    // refresh the playlist chips. Same work the device-select does, on demand.
    //
    // Re-enumerates first. The entry this view is holding was written by the
    // last poll, so its mount path can name somewhere the device no longer is
    // after a remount, and a Scan would then read the wrong directory or none
    // at all. Looking the device up again after the refresh costs one udisks
    // listing and is what makes the button mean "read this device now".
    {
        let devices_scan = current_devices.clone();
        let sel_backend = selected_dev_backend.clone();
        let reload_store = reload_device_store.clone();
        let reload_pls = reload_dev_playlists.clone();
        let scan_btn = dev_scan.clone();
        // Guards this button's own refresh only, so a Scan still runs while the
        // page's periodic poll happens to be in flight.
        let scanning = Rc::new(Cell::new(false));
        dev_scan.connect_clicked(move |_| {
            let Some(backend) = sel_backend.borrow().clone() else { return };
            let devices_scan = devices_scan.clone();
            let reload_store = reload_store.clone();
            let reload_pls = reload_pls.clone();
            let scan_btn = scan_btn.clone();
            set_button_busy(&scan_btn, true, "Scan");
            refresh_device_cache(
                devices_scan.clone(),
                scanning.clone(),
                Rc::new(move |_outcome| {
                    set_button_busy(&scan_btn, false, "Scan");
                    let fresh = devices_scan
                        .borrow()
                        .iter()
                        .find(|d| d.backend_id == backend)
                        .cloned();
                    // Gone between the click and the listing: the poll's own
                    // callback has already rebuilt the sidebar to match, so
                    // there is nothing left here to scan or to say.
                    let Some(fresh) = fresh else { return };
                    reload_pls(fresh.clone());
                    reload_store(fresh);
                }),
            );
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
                            // Non-fatal: nothing changed, the user can retry.
                            if let Some(w) = win2.upgrade() {
                                show_toast(
                                    &w,
                                    &format!(
                                        "Couldn't disconnect the device ({e}). Close anything \
                                         using it and try again."
                                    ),
                                );
                            }
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
                        if let Some(w) = win_wk.upgrade() {
                            show_toast(&w, "Eject failed unexpectedly.");
                        }
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
        let progress_sync = ui.dev_progress.clone();
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
            let progress_sync = progress_sync.clone();
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
                // Informational, not an error — G3 says no success modals either.
                if let Some(w) = win_wk.upgrade() {
                    show_toast(&w, "Already in sync. No tag or playlist changes to apply.");
                }
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
            let sync_btn2 = sync_btn.clone();
            let progress2 = progress_sync.clone();
            dialog.choose(
                win_wk.upgrade().as_ref(),
                None::<&gio::Cancellable>,
                move |res| {
                    if res != Ok(1) {
                        return;
                    }
                    // The apply is disk work: copying songs and rewriting
                    // playlists on removable media. Doing it here froze the
                    // whole app until it finished (reported 2026-08-11) — the
                    // planning above was already off-thread, the applying
                    // never was. It runs on a worker with its own SQLite
                    // connection now, feeding the detail view's progress bar,
                    // and the conflict prompts and summary below resume on the
                    // main thread once it lands.
                    set_button_busy(&sync_btn2, true, "Sync");
                    progress2.set_fraction(0.0);
                    progress2.set_visible(true);

                    let (prog_tx, prog_rx) = std::sync::mpsc::channel::<(usize, usize)>();
                    type SyncOutcome = (usize, usize, usize, usize, Vec<PlaylistSyncItem>);
                    let (res_tx, res_rx) = std::sync::mpsc::channel::<SyncOutcome>();
                    let db_path = crate::media_library::MediaLibrary::db_path_pub();
                    let dev_w = dev2.clone();
                    let plan_w = plan2.clone();
                    let pl_plan_w = pl_plan2.clone();
                    std::thread::spawn(move || {
                        let Ok(lib) = crate::media_library::MediaLibrary::open_at(&db_path) else {
                            let _ = res_tx.send((0, 0, 0, 0, Vec::new()));
                            return;
                        };
                        let (applied, failed) =
                            crate::devices::plan::apply_device_sync_with_progress(
                                &lib,
                                &dev_w,
                                &plan_w,
                                &mut |done, total| {
                                    let _ = prog_tx.send((done, total));
                                },
                            );
                        let mut pl_updated = 0usize;
                        let mut pl_copied = 0usize;
                        let mut conflicts: Vec<PlaylistSyncItem> = Vec::new();
                        for item in &pl_plan_w {
                            match item.dir {
                                PlaylistSyncDir::Push => {
                                    let (c, ok) =
                                        crate::devices::plan::apply_playlist_push(&lib, &dev_w, item);
                                    pl_copied += c;
                                    if ok {
                                        pl_updated += 1;
                                    }
                                }
                                PlaylistSyncDir::Pull => {
                                    if crate::devices::plan::apply_playlist_pull(&lib, item) {
                                        pl_updated += 1;
                                    }
                                }
                                PlaylistSyncDir::Conflict => conflicts.push(item.clone()),
                                PlaylistSyncDir::None => {}
                            }
                        }
                        let _ = res_tx.send((applied, failed, pl_updated, pl_copied, conflicts));
                    });

                    let prog_rx = std::cell::RefCell::new(prog_rx);
                    let res_rx = std::cell::RefCell::new(res_rx);
                    glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
                        while let Ok((done, total)) = prog_rx.borrow().try_recv() {
                            progress2.set_fraction(done as f64 / total.max(1) as f64);
                        }
                        let Ok((applied, failed, pl_updated, pl_copied, conflicts)) =
                            res_rx.borrow().try_recv()
                        else {
                            return glib::ControlFlow::Continue;
                        };
                        progress2.set_visible(false);
                        set_button_busy(&sync_btn2, false, "Sync");
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
                                // Completion summary, not a gate — the sync already ran.
                                if let Some(w) = win_done.upgrade() {
                                    show_toast(&w, &summary);
                                }
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
                        glib::ControlFlow::Break
                    });
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
