//! Device playlists — sending one to a device, and managing the ones already
//! there.
//!
//! Split from [`super::devices_page`] (plan step 6, first cut). Everything
//! here is about the `.m3u`/`.m3u8` files that live *on* a device, as opposed
//! to the library playlists in the Playlists page.
//!
//! **Send** copies a whole library playlist — its audio files and a written
//! `.m3u8` — onto a device on a worker thread, with live progress on the
//! device's sidebar row and on the detail view's bar. It is published through
//! `sb.send_playlist_holder` so the playlist editor's "Entire playlist to
//! device" action can call it without this page existing first (the holder
//! pattern, docs/gtk-breakup-plan.md §3.1).
//!
//! **Rename / Duplicate / New / Delete** act on the device's own playlist
//! files. Two Deletion Rule notes carried over verbatim from the page:
//! deleting a device playlist removes the `.m3u`/`.m3u8` only — the audio
//! files stay on the device — and renaming one that is linked to a library
//! playlist keeps the two in step.

use gtk4::prelude::*;
use gtk4::{gio, glib, Align, Box as GtkBox, Button, Entry, Label, Orientation};
use std::cell::RefCell;
use std::rc::Rc;

use super::sidebar::Sidebar;
use super::{
    device_fs_unsupported, device_glyph_prefix, device_plan_fs, device_record_pair, device_recorded_relpath, find_row_by_name, gtk_safe,
    linked_library_playlist, prepare_playlist_send, safe_playlist_filename, show_toast,
    MlCtx,
};

/// The device-view widgets and state these actions read and write.
pub(super) struct PlaylistUi<'a> {
    pub dev_pl_new: &'a Button,
    pub dev_pl_rename: &'a Button,
    pub dev_pl_duplicate: &'a Button,
    pub dev_pl_delete: &'a Button,
    /// Disabled while a copy is running, so the device can't be pulled out
    /// mid-transfer.
    pub dev_eject: &'a Button,
    /// The detail view's live copy-status line and progress bar.
    pub dev_hint: &'a Label,
    pub dev_progress: &'a gtk4::ProgressBar,
    /// Backend object id of the device currently shown.
    pub selected_dev_backend: &'a Rc<RefCell<Option<String>>>,
    /// The device playlist file the active chip points at; `None` = All files.
    pub selected_dev_playlist: &'a Rc<RefCell<Option<std::path::PathBuf>>>,
    /// Rebuild the chips, and re-read the tracks, after a change.
    pub reload_dev_playlists: &'a Rc<dyn Fn(sparkamp::devices::Device)>,
    pub reload_device_store: &'a Rc<dyn Fn(sparkamp::devices::Device)>,
    /// Mirror copy progress onto the device's overview card.
    pub update_card_progress: &'a Rc<dyn Fn(&str, Option<(usize, usize)>)>,
}

/// Wire the send runner and the four management buttons.
///
/// Returns the shared "which device are these buttons acting on?" lookup, which
/// the track view's action row needs too.
pub(super) fn connect(
    ctx: &MlCtx,
    sb: &Sidebar,
    ui: PlaylistUi<'_>,
) -> Rc<dyn Fn() -> Option<sparkamp::devices::Device>> {
    // Local names for what this takes from its context and its caller, so the
    // body below reads as it did inside `devices_page::build`.
    let state = ctx.host.state.clone();
    let current_devices = ctx.host.current_devices.clone();
    let win = ctx.win.clone();
    let sidebar = sb.list.clone();
    let send_playlist_holder = sb.send_playlist_holder.clone();
    let dev_pl_new = ui.dev_pl_new.clone();
    let dev_pl_rename = ui.dev_pl_rename.clone();
    let dev_pl_duplicate = ui.dev_pl_duplicate.clone();
    let dev_pl_delete = ui.dev_pl_delete.clone();
    let dev_eject = ui.dev_eject.clone();
    let dev_hint = ui.dev_hint.clone();
    let dev_progress = ui.dev_progress.clone();
    let selected_dev_backend = ui.selected_dev_backend.clone();
    let selected_dev_playlist = ui.selected_dev_playlist.clone();
    let reload_dev_playlists = ui.reload_dev_playlists.clone();
    let reload_device_store = ui.reload_device_store.clone();
    let update_card_progress = ui.update_card_progress.clone();

    // Send a whole playlist (files + .m3u8) to a device, copying on a worker
    // thread with live progress shown on the device's sidebar row and detail.
    let send_playlist_run: Rc<dyn Fn(sparkamp::devices::Device, i64, String)> = {
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
        Rc::new(move |dev: sparkamp::devices::Device, playlist_id: i64, name: String| {
            let plan = match prepare_playlist_send(&state, &dev, playlist_id, &name) {
                Ok(p) => p,
                Err(e) => {
                    // Non-fatal: nothing was sent yet, the user can retry.
                    if let Some(w) = win_wk.upgrade() {
                        show_toast(&w, &e);
                    }
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
                        match sparkamp::devices::io::for_device(&dc).copy_to_device(&s, &rel) {
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
                        let _ = lib.upsert_playlist_baseline(&sparkamp::media_library::PlaylistBaseline {
                            device_id: device_id.clone(),
                            library_playlist_id: playlist_id,
                            device_filename: dev_fname,
                            entries_hash: sparkamp::devices::sync::entries_hash(&basenames),
                            last_sync_at: Some(sparkamp::timeutil::format_current_timestamp()),
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
                // Completion summary, not a gate — the send already ran.
                if let Some(w) = win2.upgrade() {
                    show_toast(
                        &w,
                        &format!(
                            "Sent to {dname}: {copied} copied, {skipped} skipped, {failed} \
                             failed, plus the playlist."
                        ),
                    );
                }
            });
        })
    };
    *send_playlist_holder.borrow_mut() = Some(send_playlist_run.clone());

    // ── Device playlist management actions (New / Rename / Duplicate / Delete) ─
    // Resolve the Device backing the currently-selected device row.
    let current_device_for_actions = {
        let current_devices = current_devices.clone();
        let sel_backend = selected_dev_backend.clone();
        move || -> Option<sparkamp::devices::Device> {
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
                // Precondition block, not a destructive gate — nothing to undo.
                if let Some(w) = win_wk.upgrade() {
                    show_toast(&w, "Device is read-only.");
                }
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
                        // Non-fatal: the rename dialog stays open for a retry.
                        if let Some(w) = win_wk2.upgrade() {
                            show_toast(&w, &format!("Couldn't rename the playlist file: {err}"));
                        }
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
                // Precondition block, not a destructive gate — nothing to undo.
                if let Some(w) = win_wk.upgrade() {
                    show_toast(&w, "Device is read-only.");
                }
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
                // Both are non-fatal: the Duplicate dialog stays open for a retry.
                if dest.exists() {
                    if let Some(w) = win_wk2.upgrade() {
                        show_toast(&w, "A playlist with that name already exists on the device.");
                    }
                    return;
                }
                if let Err(err) = std::fs::copy(&pl_path2, &dest) {
                    if let Some(w) = win_wk2.upgrade() {
                        show_toast(&w, &format!("Couldn't duplicate the playlist: {err}"));
                    }
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
                // Precondition block, not a destructive gate — nothing to undo.
                if let Some(w) = win_wk.upgrade() {
                    show_toast(&w, "Device is read-only.");
                }
                return;
            }
            if device_fs_unsupported(&dev.fs_type) {
                if let Some(w) = win_wk.upgrade() {
                    show_toast(&w, "This filesystem is unsupported. Can't create a playlist on it yet.");
                }
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
                // Both are non-fatal: the New Playlist dialog stays open for a retry.
                if dest.exists() {
                    if let Some(w) = win_wk2.upgrade() {
                        show_toast(&w, "A playlist with that name already exists on the device.");
                    }
                    return;
                }
                if let Err(err) = std::fs::write(&dest, "#EXTM3U\n") {
                    if let Some(w) = win_wk2.upgrade() {
                        show_toast(&w, &format!("Couldn't create the playlist: {err}"));
                    }
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
                if let Err(err) = sparkamp::devices::io::for_device(&dev2).delete(&pl_path2) {
                    // Non-fatal: the removal was already confirmed above, this
                    // just reports why the (already-approved) delete failed.
                    if let Some(w) = win_wk2.upgrade() {
                        show_toast(&w, &format!("Couldn't remove the playlist file: {err}"));
                    }
                    return;
                }
                reload_pls2(dev2.clone());
                reload_store2(dev2.clone());
            });
        });
    }

    Rc::new(current_device_for_actions)
}
