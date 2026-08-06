//! The Media Library Files page's row context menu and Send-to actions.
//!
//! Child module of [`super`] (window.rs), split out of `files.rs` by plan
//! step 4 so neither half sits far over the 800-line goal.
//!
//! Everything here is a `gio::SimpleAction` in one `"ml"` action group. The
//! group is attached to two widgets, not one: the `ColumnView` that owns the
//! rows, and the window itself — the "Send to ▾" button in the page's button
//! row is not a descendant of the ColumnView, and without the second
//! attachment its menu items render disabled.
//!
//! The split runs along the action/widget line rather than by feature. The
//! actions read the live selection and the library and then act; the table,
//! search row and status bar in `files.rs` are what they act on. That keeps
//! the interface to four values rather than the thirty-odd locals a
//! feature-wise cut would have had to thread.

use gtk4::prelude::*;
use gtk4::{gio, glib, ColumnView, Label, MultiSelection};
use std::cell::RefCell;
use std::rc::Rc;

use super::art_window;
use super::{
    analyze_job, complete_ml_scan, gtk_safe, notify_playlist_changed,
    notify_playlist_nav_refresh, open_id3_editor_window, queue_paths_to_drive,
    run_playlist_save_dialog, show_alert_parented, show_playlist_save_error, start_ml_scan,
    update_ml_scan_progress, view_or_search_lyrics, LyricsMode, MlCtx, ScanType,
};

/// What `install` hands back to the Files page.
pub(super) struct FilesActions {
    /// The `"ml"` action group, already attached to the ColumnView and window.
    pub group: gio::SimpleActionGroup,
    /// Late-bound status label. The burn action reports "Queued N…" through
    /// it; the label itself is created further down in `files.rs`.
    pub files_status_holder: Rc<RefCell<Option<Label>>>,
    /// Paths captured when the context menu opened — what the menu acts on.
    pub selected_tracks: Rc<RefCell<Vec<std::path::PathBuf>>>,
    /// Reads the selection live instead of from the capture above, for the
    /// actions that must see the current rows rather than the ones that were
    /// selected when the menu was raised.
    pub live_selected_paths: Rc<dyn Fn() -> Vec<std::path::PathBuf>>,
}

/// Build the `"ml"` actions and attach them.
pub(super) fn install(
    ctx: &MlCtx,
    col_view: &ColumnView,
    multi_sel: &MultiSelection,
    track_store: &gio::ListStore,
) -> FilesActions {
    // Local names for what the actions use, so the bodies below read as they
    // did inside files.rs.
    let state = ctx.host.state.clone();
    let rebuild_playlist = ctx.host.rebuild_playlist.clone();
    let current_drives = ctx.host.current_drives.clone();
    let current_devices = ctx.host.current_devices.clone();
    let burn_queues = ctx.host.burn_queues.clone();
    let burn_refresh_holder = ctx.host.burn_refresh_holder.clone();
    let copy_files_run = ctx.copy_files_run.clone();
    let win = ctx.win.clone();
    let col_view = col_view.clone();
    let multi_sel = multi_sel.clone();
    let track_store = track_store.clone();

        let ml_action_group = gio::SimpleActionGroup::new();
        col_view.insert_action_group("ml", Some(&ml_action_group));
        // Also on the window so the "Send to ▾" MenuButton in the button row
        // — which is NOT a descendant of col_view — can reach these actions.
        // Without this its menu items rendered disabled (2026-07-16).
        win.insert_action_group("ml", Some(&ml_action_group));

        // The files status label is created further down — the burn action
        // reports "Queued N…" through this holder.
        let files_status_holder: Rc<RefCell<Option<Label>>> = Rc::new(RefCell::new(None));

        // Store for selected tracks (used by action handlers)
        let ml_selected_tracks: Rc<RefCell<Vec<std::path::PathBuf>>> =
            Rc::new(RefCell::new(Vec::new()));

        // Live "currently selected files-view rows" reader. The "Send to ▾"
        // button doesn't go through the per-row right-click gesture, so its
        // actions must read `multi_sel` directly at dispatch time instead
        // of the `ml_selected_tracks` stash above (G1: that stash only
        // updates on right-click and went stale for the button path — the
        // button kept acting on whatever was last right-clicked). Mirrors
        // how `add_selected` (below) already reads `multi_sel` live for
        // "Active Playlist".
        let ml_live_selected_paths: Rc<dyn Fn() -> Vec<std::path::PathBuf>> = {
            let sel = multi_sel.clone();
            Rc::new(move || {
                let mut out = Vec::new();
                for i in 0..sel.n_items() {
                    if sel.is_selected(i) {
                        if let Some(obj) = sel
                            .item(i)
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            let t = obj.borrow::<crate::media_library::LibTrack>();
                            out.push(std::path::PathBuf::from(&t.path));
                        }
                    }
                }
                out
            })
        };

        // Live "currently selected" reader that hands back full `LibTrack`s
        // rather than bare paths — the "Calculate ReplayGain" context action
        // needs the row's `id` (to call `set_replaygain`) and album/artist
        // (to batch the analysis), not just a path. Mirrors
        // `ml_live_selected_paths` above for the same live-vs-stale reason.
        let ml_live_selected_lib_tracks: Rc<dyn Fn() -> Vec<crate::media_library::LibTrack>> = {
            let sel = multi_sel.clone();
            Rc::new(move || {
                let mut out = Vec::new();
                for i in 0..sel.n_items() {
                    if sel.is_selected(i) {
                        if let Some(obj) = sel
                            .item(i)
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            out.push(obj.borrow::<crate::media_library::LibTrack>().clone());
                        }
                    }
                }
                out
            })
        };

        // Append to Playlist action
        let ml_action_append_state = state.clone();
        let _ml_action_append_sel = multi_sel.clone();
        let ml_action_append_rebuild = rebuild_playlist.clone();
        let ml_action_append_tracks = ml_selected_tracks.clone();
        let action_append = gio::SimpleAction::new("append", None); // Note: action name without "ml." prefix
        action_append.connect_activate(move |_, _| {
            let tracks: Vec<_> = ml_action_append_tracks.borrow().clone();
            if tracks.is_empty() {
                return;
            }
            let was_empty = ml_action_append_state.borrow().playlist.is_empty();
            for path in tracks {
                let track = crate::model::Track::from_path(&path).ok();
                if let Some(track) = track {
                    ml_action_append_state.borrow_mut().playlist.add(track);
                }
            }
            if ml_action_append_state
                .borrow()
                .config
                .behavior
                .autoplay_on_add
                && was_empty
            {
                ml_action_append_state.borrow_mut().play_current();
            }
            ml_action_append_rebuild();
        });
        ml_action_group.add_action(&action_append);

        // Send to Disc Drive: probe-on-add, then queue onto THAT drive.
        {
            let state_burn = state.clone();
            let paths_src = ml_live_selected_paths.clone();
            let burn_queues = burn_queues.clone();
            let burn_refresh_holder = burn_refresh_holder.clone();
            let current_drives = current_drives.clone();
            let status = files_status_holder.clone();
            let win_wk = win.downgrade();
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
                // Live selection at dispatch (G1) — not the right-click
                // gesture's stash, which the button never populates.
                let paths: Vec<_> = paths_src();
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
                    Rc::new(move |s: String| {
                        if let Some(lbl) = status.borrow().as_ref() {
                            lbl.set_text(&gtk_safe(&s));
                        }
                    }),
                    win_wk.clone(),
                );
            });
            ml_action_group.add_action(&action);
        }

        // Send to Removable Device: hand off to the existing copy runner,
        // which already reports progress through the files status line.
        {
            let current_devices = current_devices.clone();
            let paths_src = ml_live_selected_paths.clone();
            let copy_files_run = copy_files_run.clone();
            let action = gio::SimpleAction::new(
                "send-device",
                Some(glib::VariantTy::STRING),
            );
            action.connect_activate(move |_, target| {
                let Some(dev_id) =
                    target.and_then(|v| v.get::<String>()) else { return };
                let dev = current_devices
                    .borrow()
                    .iter()
                    .find(|d| d.id == dev_id)
                    .cloned();
                // Live selection at dispatch (G1).
                let paths: Vec<_> = paths_src();
                if let (Some(dev), false) = (dev, paths.is_empty()) {
                    copy_files_run(dev, paths);
                }
            });
            ml_action_group.add_action(&action);
        }

        // Replace current playlist action
        let ml_action_replace_state = state.clone();
        let ml_action_replace_tracks = ml_selected_tracks.clone();
        let ml_action_replace_rebuild = rebuild_playlist.clone();
        let action_replace = gio::SimpleAction::new("replace", None); // Note: action name without "ml." prefix
        action_replace.connect_activate(move |_, _| {
            let tracks: Vec<_> = ml_action_replace_tracks.borrow().clone();
            if tracks.is_empty() {
                return;
            }
            let _ = ml_action_replace_state.borrow_mut().player.stop();
            ml_action_replace_state.borrow_mut().playlist.clear();
            for path in tracks {
                let track = crate::model::Track::from_path(&path).ok();
                if let Some(track) = track {
                    ml_action_replace_state.borrow_mut().playlist.add(track);
                }
            }
            if ml_action_replace_state
                .borrow()
                .config
                .behavior
                .autoplay_on_add
                && !ml_action_replace_state.borrow().playlist.is_empty()
            {
                ml_action_replace_state.borrow_mut().play_current();
            }
            ml_action_replace_rebuild();
        });
        ml_action_group.add_action(&action_replace);

        // View/Edit ID3 Info action (for single selection)
        let ml_action_id3_state = state.clone();
        let ml_action_id3_tracks = ml_selected_tracks.clone();
        let ml_action_id3_rebuild = rebuild_playlist.clone();
        let action_id3 = gio::SimpleAction::new("edit-id3", None); // Note: action name without "ml." prefix
        action_id3.connect_activate(move |_, _| {
            let tracks: Vec<_> = ml_action_id3_tracks.borrow().clone();
            if tracks.is_empty() {
                return;
            }
            // Only open for the first (single) selected track
            let path = tracks[0].clone();
            open_id3_editor_window(
                None::<&gtk4::Window>,
                path,
                ml_action_id3_state.clone(),
                ml_action_id3_rebuild.clone(),
                None,
                None,
            );
        });
        ml_action_group.add_action(&action_id3);

        // View/Search Lyrics (F15) on Files rows. The row stash holds only
        // paths, so the search fallback pulls artist/title from the ML row
        // (same source as the row label), or the file stem when unindexed.
        let ml_action_lyrics_state = state.clone();
        let ml_action_lyrics_tracks = ml_selected_tracks.clone();
        let ml_action_lyrics_rebuild = rebuild_playlist.clone();
        let action_lyrics = gio::SimpleAction::new("lyrics", None);
        action_lyrics.connect_activate(move |_, _| {
            let tracks: Vec<_> = ml_action_lyrics_tracks.borrow().clone();
            let Some(path) = tracks.first().cloned() else { return };
            let path_str = path.to_string_lossy().into_owned();
            let (artist, title, album_artist) = {
                let s = ml_action_lyrics_state.borrow();
                let lt = s.media_lib.as_ref().and_then(|ml| ml.track_by_path(&path_str).ok());
                match lt {
                    Some(t) => (
                        t.artist.clone().unwrap_or_default(),
                        t.title.clone().unwrap_or_default(),
                        t.album_artist.clone().unwrap_or_default(),
                    ),
                    None => (
                        String::new(),
                        path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string(),
                        String::new(),
                    ),
                }
            };
            view_or_search_lyrics(
                &ml_action_lyrics_state, &path, &artist, &title, &album_artist,
                ml_action_lyrics_rebuild.clone(), LyricsMode::Specific,
            );
        });
        ml_action_group.add_action(&action_lyrics);

        // View Album Art for the single selected library row.
        let ml_action_art_tracks = ml_selected_tracks.clone();
        let ml_action_art_state = state.clone();
        let action_view_art = gio::SimpleAction::new("view-art", None);
        action_view_art.connect_activate(move |_, _| {
            let tracks: Vec<_> = ml_action_art_tracks.borrow().clone();
            let Some(path) = tracks.first().cloned() else { return };
            art_window::open_track_art(&ml_action_art_state, &path);
        });
        ml_action_group.add_action(&action_view_art);

        // Rescan Metadata action
        let ml_action_rescan_state = state.clone();
        let ml_action_rescan_tracks = ml_selected_tracks.clone();
        let action_rescan = gio::SimpleAction::new("rescan", None); // Note: action name without "ml." prefix
        action_rescan.connect_activate(move |_, _| {
            let tracks: Vec<_> = ml_action_rescan_tracks.borrow().clone();
            if tracks.is_empty() {
                return;
            }
            if ml_action_rescan_state.borrow().ml_scan.is_some() {
                return;
            }
            let paths: Vec<String> = tracks
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let total = paths.len();
            let cancel_flag = start_ml_scan(&ml_action_rescan_state, ScanType::AddFiles, total);
            let (progress_tx, progress_rx) = std::sync::mpsc::channel();
            let (result_tx, result_rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let db_path = crate::media_library::MediaLibrary::db_path_pub();
                let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
                    Ok(l) => l,
                    Err(_) => {
                        let _ = result_tx.send(());
                        return;
                    }
                };
                for (i, path) in paths.iter().enumerate() {
                    if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let _ = lib.rescan_track(path);
                    let _ = progress_tx.send(i + 1);
                }
                let _ = result_tx.send(());
            });
            let progress_rx = std::cell::RefCell::new(progress_rx);
            let result_rx = std::cell::RefCell::new(result_rx);
            let state_for_timer = ml_action_rescan_state.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                while let Ok(current) = progress_rx.borrow().try_recv() {
                    update_ml_scan_progress(&state_for_timer, current, total);
                }
                if result_rx.borrow().try_recv().is_ok() {
                    {
                        let mut s = state_for_timer.borrow_mut();
                        s.media_lib = crate::media_library::MediaLibrary::open().ok();
                    }
                    complete_ml_scan(&state_for_timer);
                    if let Some(ref cb) = state_for_timer.borrow().rebuild_ml_callback {
                        cb();
                    }
                    return glib::ControlFlow::Break;
                }
                glib::ControlFlow::Continue
            });
        });
        ml_action_group.add_action(&action_rescan);

        // Calculate ReplayGain action — force-analyzes the current
        // selection (skips the missing-or-stale filter; "Calculate" always
        // re-measures exactly what the user picked). Shares the
        // `analyze_job` worker/progress plumbing with the bulk "Analyze
        // ReplayGain" button defined further down. `files_status` isn't
        // built yet at this point in the function, so — like `action_rescan`
        // above reaches for `rebuild_ml_callback` instead of the not-yet-
        // built `rebuild_files` — this reads the label out of
        // `files_status_holder`, populated once the button row is built.
        let ml_action_calc_rg_state = state.clone();
        let ml_action_calc_rg_tracks = ml_live_selected_lib_tracks.clone();
        let ml_action_calc_rg_status = files_status_holder.clone();
        let action_calc_rg = gio::SimpleAction::new("calc-rg", None);
        action_calc_rg.connect_activate(move |_, _| {
            if !crate::replaygain::rg_analysis_available() {
                return; // feature silently unavailable (house rule)
            }
            let tracks = ml_action_calc_rg_tracks();
            let Some(status_label) = ml_action_calc_rg_status.borrow().clone() else {
                return;
            };
            let state_rc = ml_action_calc_rg_state.clone();
            let rebuild: Rc<dyn Fn()> = {
                let state_for_rb = state_rc.clone();
                Rc::new(move || {
                    let cb = state_for_rb.borrow().rebuild_ml_callback.clone();
                    if let Some(cb) = cb {
                        cb();
                    }
                })
            };
            analyze_job(&state_rc, tracks, true, &status_label, rebuild);
        });
        ml_action_group.add_action(&action_calc_rg);

        // Remove from Media Library action
        let ml_action_remove_tracks = ml_selected_tracks.clone();
        let ml_action_remove_store = track_store.clone();
        let action_remove = gio::SimpleAction::new("remove", None);
        action_remove.connect_activate(move |_, _| {
            let paths = ml_action_remove_tracks.borrow().clone();
            if paths.is_empty() {
                return;
            }

            let path_set: std::collections::HashSet<String> = paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let paths_owned: Vec<String> = path_set.iter().cloned().collect();

            let db_path = crate::media_library::MediaLibrary::db_path_pub();
            std::thread::spawn(move || {
                if let Ok(lib) = crate::media_library::MediaLibrary::open_at(&db_path) {
                    let _ = lib.soft_delete_tracks_by_paths(&paths_owned);
                    let _ = lib.purge_deleted_tracks();
                }
            });

            let mut rows_to_remove: Vec<u32> = Vec::new();
            for i in 0..ml_action_remove_store.n_items() {
                if let Some(item) = ml_action_remove_store.item(i) {
                    if let Some(boxed) = item.downcast_ref::<glib::BoxedAnyObject>() {
                        let track = boxed.borrow::<crate::media_library::LibTrack>();
                        if path_set.contains(&track.path) {
                            rows_to_remove.push(i);
                        }
                    }
                }
            }

            for idx in rows_to_remove.into_iter().rev() {
                ml_action_remove_store.remove(idx);
            }
        });
        ml_action_group.add_action(&action_remove);

        // Seed a brand new saved playlist from the current ML selection.
        let ml_action_new_state  = state.clone();
        let ml_action_new_paths  = ml_live_selected_paths.clone();
        let ml_action_new_win    = win.clone();
        let action_add_to_new    = gio::SimpleAction::new("add-to-new", None);
        action_add_to_new.connect_activate(move |_, _| {
            // Live selection at dispatch (G1).
            let paths: Vec<String> = ml_action_new_paths()
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            if paths.is_empty() { return }
            let default_stem = glib::DateTime::now_local()
                .ok()
                .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Playlist".to_string());
            let state_cb = ml_action_new_state.clone();
            let paths_cb = paths.clone();
            run_playlist_save_dialog(
                ml_action_new_state.clone(),
                ml_action_new_win.clone(),
                &default_stem,
                move |path, win_cb| {
                    if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                        if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths_cb) {
                            eprintln!("save_playlist_tracks_to_path: {e}");
                            show_playlist_save_error(&win_cb, &path, &e);
                        }
                    }
                    // The new playlist must appear in the sidebar + manage
                    // list right away (same call the active playlist's
                    // Save-as flow already makes).
                    notify_playlist_nav_refresh();
                },
            );
        });
        ml_action_group.add_action(&action_add_to_new);

        // Add-to-saved-playlist action (parameterised by target playlist id).
        // Append currently selected ML file paths to the chosen saved playlist.
        let ml_action_add_state = state.clone();
        let ml_action_add_paths = ml_live_selected_paths.clone();
        let ml_action_add_win = win.downgrade();
        let action_add_to_saved = gio::SimpleAction::new(
            "add-to-saved",
            Some(glib::VariantTy::INT64),
        );
        action_add_to_saved.connect_activate(move |_, param| {
            let Some(pid) = param.and_then(|p| p.get::<i64>()) else { return };
            // Live selection at dispatch (G1).
            let paths: Vec<String> = ml_action_add_paths()
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            if paths.is_empty() { return }
            let mut ok = false;
            let mut err_msg: Option<String> = None;
            if let Some(lib) = ml_action_add_state.borrow().media_lib.as_ref() {
                match lib.append_paths_to_playlist(pid, &paths) {
                    Ok(_)  => ok = true,
                    // Surface the failure instead of only logging it — e.g.
                    // a read-only playlist folder must not fail silently.
                    Err(e) => err_msg = Some(format!("Couldn't save to the playlist:\n{e}")),
                }
            }
            if ok {
                notify_playlist_changed(pid);
            } else if let Some(msg) = err_msg {
                show_alert_parented(
                    ml_action_add_win.upgrade().as_ref(),
                    &gtk_safe(&msg),
                );
            }
        });
        ml_action_group.add_action(&action_add_to_saved);
    FilesActions {
        group: ml_action_group,
        files_status_holder,
        selected_tracks: ml_selected_tracks,
        live_selected_paths: ml_live_selected_paths,
    }
}
