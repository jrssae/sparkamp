//! The device track view's row context menu, and its Send-to actions.
//!
//! Split from [`super::devices_page`] (plan step 6, second cut) — the same
//! split [`super::files_menu`] is to the Files page, and it mirrors that
//! menu's contents: View / Edit ID3, lyrics, Send to ▸ (active playlist,
//! disc drive, another device), replace the current playlist, and delete
//! from the device.
//!
//! Two GTK details this inherited and must keep:
//!
//! - The gesture and the action group live on the `ScrolledWindow`, not on
//!   the `ColumnView`, to dodge the GTK4 bug where a `PopoverMenu` parented
//!   on the view misses hover.
//! - The previous popover is unparented at the *top* of the popup closure,
//!   before the new one calls `set_parent`. Doing it the other way round —
//!   or from `connect_closed` — is what made the menu need two right-clicks
//!   and then stop dispatching entirely (fixed 2026-08-09).
//!
//! Actions operate on the live selection at dispatch time, like the Play /
//! Enqueue / Delete buttons in the same view. The ID3 editor binds a single
//! file, so that item appears only for a one-row selection.
//!
//! Declares nothing the rest of the page reads back.

use gtk4::prelude::*;
use gtk4::{gdk, gio, glib, ColumnView, EventControllerKey, Label, ScrolledWindow};
use std::cell::RefCell;
use std::rc::Rc;

use super::art_window;
use super::{
    build_send_to_menu, context_popover, gtk_safe, notify_playlist_changed,
    notify_playlist_nav_refresh, open_id3_editor_window, queue_paths_to_drive,
    run_playlist_save_dialog, show_alert_parented, show_playlist_save_error,
    view_or_search_lyrics, LyricsMode, MlCtx, SendToActions,
};

/// The device-view widgets and state the menu reads and writes.
pub(super) struct MenuUi<'a> {
    /// Gesture + action-group host, and the popover's parent.
    pub dev_tracks_scroll: &'a ScrolledWindow,
    /// The view itself — only for translating cell coordinates.
    pub dev_col_view: &'a ColumnView,
    /// The device view's quiet status line — Send-to reports here.
    pub dev_status: &'a Label,
    /// Filled here; called by each row cell's right-click gesture.
    pub dev_row_menu_holder: &'a Rc<RefCell<Option<Rc<dyn Fn(f64, f64)>>>>,
    /// Backend object id of the device currently shown.
    pub selected_dev_backend: &'a Rc<RefCell<Option<String>>>,
    /// The live selection, as full library tracks.
    pub selected_device_tracks: &'a Rc<dyn Fn() -> Vec<crate::media_library::LibTrack>>,
    /// Re-read the device's tracks after a delete.
    pub reload_device_store: &'a Rc<dyn Fn(crate::devices::Device)>,
}

/// Build the row context menu and publish it through `dev_row_menu_holder`.
pub(super) fn connect(ctx: &MlCtx, ui: MenuUi<'_>) {
    // Local names for what this takes from its context and its caller, so the
    // body below reads as it did inside `devices_page::build`.
    let state = ctx.host.state.clone();
    let rebuild_playlist = ctx.host.rebuild_playlist.clone();
    let current_drives = ctx.host.current_drives.clone();
    let current_devices = ctx.host.current_devices.clone();
    let burn_queues = ctx.host.burn_queues.clone();
    let copy_files_holder = ctx.host.copy_files_holder.clone();
    let burn_refresh_holder = ctx.host.burn_refresh_holder.clone();
    let win = ctx.win.clone();
    let dev_tracks_scroll = ui.dev_tracks_scroll.clone();
    let dev_col_view = ui.dev_col_view.clone();
    let dev_status = ui.dev_status.clone();
    let dev_row_menu_holder = ui.dev_row_menu_holder.clone();
    let selected_dev_backend = ui.selected_dev_backend.clone();
    let selected_device_tracks = ui.selected_device_tracks.clone();
    let reload_device_store = ui.reload_device_store.clone();

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
}
