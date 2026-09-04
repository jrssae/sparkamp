//! The Media Library tab: watched folders and scanning.
//!
//! Split out of `open_settings_window`, which was one 2,775-line function
//! holding every tab inline. The body is unchanged; what it used to close
//! over arrives as arguments.

use super::*;

pub(super) fn build(notebook: &Notebook, state: &Rc<RefCell<AppState>>, win: &gtk4::Window) {
    let state = state.clone();
    let win = win.clone();
        let grid = Grid::new();
        grid.set_row_spacing(8);
        grid.set_column_spacing(12);
        grid.set_margin_top(12);
        grid.set_margin_bottom(12);
        grid.set_margin_start(12);
        grid.set_margin_end(12);

        // Row 0: Label + button row
        let lbl_folders = Label::new(Some("Watched folders:"));
        lbl_folders.set_halign(Align::Start);

        let btn_add_folder = Button::with_label("Add Folder…");
        let btn_remove = Button::with_label("Remove");
        btn_remove.set_sensitive(false);

        let folder_list = ListBox::new();
        folder_list.add_css_class("playlist");
        folder_list.set_selection_mode(gtk4::SelectionMode::Single);

        let folder_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .min_content_height(200)
            .width_request(300)
            .child(&folder_list)
            .build();

        let status_lbl = Label::new(None);
        status_lbl.set_halign(Align::Start);
        status_lbl.add_css_class("dim-label");

        let rebuild_list = {
            let state_rc = state.clone();
            let folder_list_rc = folder_list.clone();
            let status_rc = status_lbl.clone();
            let btn_rm = btn_remove.clone();
            Rc::new(move || {
                // Snapshot folders before mutating the list.
                let folders: Vec<(i64, String)> = state_rc
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|lib| lib.list_folders().ok())
                    .unwrap_or_default();

                // Remove all rows.
                while let Some(child) = folder_list_rc.first_child() {
                    folder_list_rc.remove(&child);
                }

                // Repopulate.
                for (folder_id, path) in &folders {
                    let row = gtk4::ListBoxRow::new();
                    let row_box = GtkBox::new(Orientation::Horizontal, 6);
                    let icon = Image::from_icon_name("folder-open");
                    let lbl = Label::new(Some(&gtk_safe(path)));
                    lbl.set_hexpand(true);
                    lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                    lbl.set_halign(Align::Start);
                    row_box.append(&icon);
                    row_box.append(&lbl);

                    // Per-folder recurse toggle (Phase 8 Task 10). Set
                    // active before connecting `toggled` so populating the
                    // list never fires a spurious DB write / watcher
                    // rebuild. Wired independently of `row.set_activatable`
                    // below — clicking it must not also select the row.
                    let chk_recurse = CheckButton::with_label("Recurse");
                    chk_recurse.set_active(
                        state_rc
                            .borrow()
                            .media_lib
                            .as_ref()
                            .and_then(|lib| lib.folder_recurse(*folder_id).ok())
                            .unwrap_or(true),
                    );
                    {
                        let state_recurse = state_rc.clone();
                        let fid = *folder_id;
                        chk_recurse.connect_toggled(move |c| {
                            let active = c.is_active();
                            if let Some(ref lib) = state_recurse.borrow().media_lib {
                                let _ = lib.set_folder_recurse(fid, active);
                            }
                            watch::rebuild_watcher(&state_recurse);
                        });
                    }
                    row_box.append(&chk_recurse);

                    row.set_child(Some(&row_box));
                    row.set_activatable(true);
                    folder_list_rc.append(&row);
                }

                btn_rm.set_sensitive(!folders.is_empty());

                let count = folders.len();
                status_rc.set_text(&match count {
                    0 => "No folders — click \"Add Folder…\" to add music".to_string(),
                    1 => "1 folder".to_string(),
                    n => format!("{n} folders"),
                });
            })
        };

        rebuild_list();

        // Filled once the Rescan button is built (below). Lets "Add Folder"
        // trigger a rescan after a concurrent scan finishes.
        let rescan_holder: Rc<RefCell<Option<Button>>> = Rc::new(RefCell::new(None));

        let rebuild_for_add = rebuild_list.clone();
        let status_for_add = status_lbl.clone();
        let state_for_add = state.clone();
        let win_add = win.downgrade();
        let rescan_holder_add = rescan_holder.clone();
        btn_add_folder.connect_clicked(move |_| {
            let dialog = gtk4::FileDialog::builder()
                .title("Select Music Folder")
                .build();
            let rebuild_cb = rebuild_for_add.clone();
            let status_rc = status_for_add.clone();
            let state_rc = state_for_add.clone();
            let rescan_holder = rescan_holder_add.clone();
            dialog.select_folder(
                win_add.upgrade().as_ref(),
                None::<&gio::Cancellable>,
                move |result| {
                    let path = match result {
                        Ok(f) => f.path().map(|p| p.to_string_lossy().into_owned()),
                        Err(_) => None,
                    };
                    let Some(path_str) = path else {
                        return;
                    };
                    // Under skip_db_load the shared media_lib may still be
                    // closed at this point. Adding a folder is a genuine
                    // demand to use the library, so open it now (no-op if
                    // already open) — this populates state.media_lib so the
                    // folder list rebuild and the live watcher reflect the
                    // add instead of silently staying on "No folders…".
                    ensure_media_lib_open(&state_rc);
                    // A scan is already running (only one metadata scan may run
                    // at a time). Register + fast-scan the folder now so it
                    // appears immediately, then queue a full rescan to pick up
                    // its metadata once the current scan finishes.
                    if state_rc.borrow().ml_scan.is_some() {
                        let db_path = crate::media_library::MediaLibrary::db_path_pub();
                        let path_for_thread = path_str.clone();
                        status_rc.set_text(
                            "Adding folder — it will be scanned after the current scan finishes…",
                        );
                        let (fast_tx, fast_rx) =
                            std::sync::mpsc::channel::<Result<(), String>>();
                        // Read the config bool on the GTK thread before handing off —
                        // AppState (holds Player/GStreamer state) isn't Send.
                        let remove_missing =
                            state_rc.borrow().config.media_library.remove_missing_on_rescan;
                        std::thread::spawn(move || {
                            let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
                                Ok(l) => l,
                                Err(e) => {
                                    let _ = fast_tx.send(Err(format!("DB error: {e}")));
                                    return;
                                }
                            };
                            let folder_id = match lib.add_folder(&path_for_thread) {
                                Ok(r) => r.id(),
                                Err(e) => {
                                    let _ = fast_tx.send(Err(format!("Could not add: {e}")));
                                    return;
                                }
                            };
                            // `add_folder` stores the folder's canonical path.
                            // If tracks were already indexed under another
                            // spelling of it (a symlinked pick — /mnt vs
                            // /var/mnt), the scan below would insert a second
                            // row for every one of them. Repair first; the
                            // check is pure SQL, and this is already a worker
                            // thread.
                            if lib.needs_path_normalization() {
                                match lib.normalize_track_paths() {
                                    Ok((moved, merged)) => eprintln!(
                                        "[library] path normalization: {moved} moved, {merged} duplicates merged"
                                    ),
                                    Err(e) => eprintln!("[library] path normalization failed: {e}"),
                                }
                            }
                            if let Err(e) =
                                lib.rescan_folder_fast(folder_id, &path_for_thread, remove_missing)
                            {
                                let _ = fast_tx.send(Err(format!("Fast scan error: {e}")));
                                return;
                            }
                            let _ = fast_tx.send(Ok(()));
                        });
                        let fast_rx = std::cell::RefCell::new(fast_rx);
                        let fast_done = std::cell::Cell::new(false);
                        let rebuild_q = rebuild_cb.clone();
                        let status_q = status_rc.clone();
                        let state_q = state_rc.clone();
                        let rescan_q = rescan_holder.clone();
                        glib::timeout_add_local(
                            std::time::Duration::from_millis(400),
                            move || {
                                if !fast_done.get() {
                                    match fast_rx.borrow().try_recv() {
                                        Ok(Ok(())) => {
                                            fast_done.set(true);
                                            rebuild_q();
                                            if let Some(ref cb) =
                                                state_q.borrow().rebuild_ml_callback
                                            {
                                                cb();
                                            }
                                            // New folder registered — restart
                                            // the live watcher so it's covered.
                                            watch::rebuild_watcher(&state_q);
                                            status_q.set_text("Folder added — waiting to scan…");
                                        }
                                        Ok(Err(e)) => {
                                            status_q.set_text(&e);
                                            return glib::ControlFlow::Break;
                                        }
                                        Err(_) => {}
                                    }
                                    return glib::ControlFlow::Continue;
                                }
                                // Fast add done; once the running scan ends,
                                // trigger a rescan to scan the new folder.
                                if state_q.borrow().ml_scan.is_none() {
                                    if let Some(btn) = rescan_q.borrow().as_ref() {
                                        btn.emit_clicked();
                                    }
                                    return glib::ControlFlow::Break;
                                }
                                glib::ControlFlow::Continue
                            },
                        );
                        return;
                    }
                    let path_for_thread = path_str.clone();

                    let cancel_flag = start_ml_scan(&state_rc, ScanType::AddFolder, 0);
                    status_rc.set_text("Reading tags…");

                    // Three channels: fast done, metadata progress, final result.
                    let (fast_tx, fast_rx) = std::sync::mpsc::channel::<Result<usize, String>>();
                    let (progress_tx, progress_rx) = std::sync::mpsc::channel::<(usize, usize)>();
                    let (result_tx, result_rx) =
                        std::sync::mpsc::channel::<Result<(bool, usize), String>>();
                    // Read the config bool on the GTK thread before handing off —
                    // AppState (holds Player/GStreamer state) isn't Send.
                    let remove_missing =
                        state_rc.borrow().config.media_library.remove_missing_on_rescan;

                    std::thread::spawn(move || {
                        let lib = match crate::media_library::MediaLibrary::open_at(
                            &crate::media_library::MediaLibrary::db_path_pub(),
                        ) {
                            Ok(l) => l,
                            Err(e) => {
                                let _ = fast_tx.send(Err(format!("DB error: {e}")));
                                return;
                            }
                        };

                        let folder_id = match lib.add_folder(&path_for_thread) {
                            Ok(r) => r.id(),
                            Err(e) => {
                                let _ = fast_tx.send(Err(format!("Could not add: {e}")));
                                return;
                            }
                        };

                        // Same repair as the other add-folder path above: the
                        // folder is stored canonicalized, so tracks indexed
                        // under a different spelling of it must be moved
                        // before the scan re-adds them.
                        if lib.needs_path_normalization() {
                            match lib.normalize_track_paths() {
                                Ok((moved, merged)) => eprintln!(
                                    "[library] path normalization: {moved} moved, {merged} duplicates merged"
                                ),
                                Err(e) => eprintln!("[library] path normalization failed: {e}"),
                            }
                        }

                        // Phase 1: fast scan
                        if let Err(e) =
                            lib.rescan_folder_fast(folder_id, &path_for_thread, remove_missing)
                        {
                            let _ = fast_tx.send(Err(format!("Fast scan error: {e}")));
                            return;
                        }
                        let _ = fast_tx.send(Ok(0usize));

                        // Phase 2: metadata scan. Reset tracks with no metadata first
                        // so scan_folder picks up any that a previous scan missed.
                        let _ = lib.reset_unscanned_metadata();
                        let count = lib
                            .scan_folder(folder_id, &cancel_flag, |c, t| {
                                let _ = progress_tx.send((c, t));
                            })
                            .map(|(scanned, _, _)| scanned)
                            .unwrap_or(0);
                        let _ = result_tx.send(Ok((true, count)));
                    });

                    let fast_rx = std::cell::RefCell::new(fast_rx);
                    let progress_rx = std::cell::RefCell::new(progress_rx);
                    let result_rx = std::cell::RefCell::new(result_rx);
                    let fast_handled = std::cell::Cell::new(false);
                    let path_str_clone = path_str.clone();
                    glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                        // Handle fast scan completion
                        if !fast_handled.get() {
                            if let Ok(fast_result) = fast_rx.borrow().try_recv() {
                                fast_handled.set(true);
                                if let Err(e) = fast_result {
                                    status_rc.set_text(&e);
                                    complete_ml_scan(&state_rc);
                                    return glib::ControlFlow::Break;
                                }
                                rebuild_cb();
                                // Rebuild ML window to show added files
                                if let Some(ref cb) = state_rc.borrow().rebuild_ml_callback {
                                    cb();
                                }
                                // New folder registered — restart the live
                                // watcher so it's covered.
                                watch::rebuild_watcher(&state_rc);
                            }
                        }

                        // Drain progress updates
                        while let Ok((current, total)) = progress_rx.borrow().try_recv() {
                            update_ml_scan_progress(&state_rc, current, total);
                            status_rc.set_text(&format!("Reading tags {}/{}…", current, total));
                        }

                        // Check for completion
                        if let Ok(result) = result_rx.borrow().try_recv() {
                            rebuild_cb();
                            match result {
                                Err(e) => status_rc.set_text(&e),
                                Ok((_, count)) => {
                                    let path_short = truncate_display(&path_str_clone, 40);
                                    status_rc.set_text(&format!(
                                        "Added: {} ({} tracks)",
                                        path_short, count
                                    ));
                                }
                            }
                            if let Some(ref cb) = state_rc.borrow().rebuild_ml_callback {
                                cb();
                            }
                            complete_ml_scan(&state_rc);
                            return glib::ControlFlow::Break;
                        }

                        glib::ControlFlow::Continue
                    });
                },
            );
        });

        let btn_rm_state = state.clone();
        let btn_rm_rebuild = rebuild_list.clone();
        let btn_rm_status = status_lbl.clone();
        let btn_rm_list = folder_list.clone();
        let btn_rm_win = win.downgrade();
        btn_remove.connect_clicked(move |_| {
            let idx = btn_rm_list.selected_row().map(|r| r.index() as usize);
            if let Some(idx) = idx {
                let folders: Vec<(i64, String)> = btn_rm_state
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|lib| lib.list_folders().ok())
                    .unwrap_or_default();
                if idx < folders.len() {
                    let (folder_id, folder_path) = folders[idx].clone();

                    // Clone for use in dialog callback
                    let state_for_dialog = btn_rm_state.clone();
                    let rebuild_for_dialog = btn_rm_rebuild.clone();
                    let status_for_dialog = btn_rm_status.clone();
                    let win_for_dialog = btn_rm_win.clone();

                    let dialog = gtk4::AlertDialog::builder()
                        .message("Remove Folder from Library")
                        .detail("Removing this folder will remove all files in this folder from the media library.\n\nNo files will be deleted from your disk, but they will not appear in the library any longer.\n\nContinue?")
                        .buttons(vec!["Cancel".to_string(), "Continue".to_string()])
                        .cancel_button(0)
                        .default_button(0)
                        .modal(true)
                        .build();

                    let folder_id_cb = folder_id;
                    let folder_path_cb = folder_path.clone();

                    dialog.choose(
                        win_for_dialog.upgrade().as_ref(),
                        None::<&gio::Cancellable>,
                        move |result| {
                            if result == Ok(1) {
                                status_for_dialog.set_text(&format!("Removing: {}", folder_path_cb));

                                // Soft-delete the tracks AND delete the folder
                                // row on the main thread so the watched-folder
                                // list reflects the removal immediately (the
                                // folder row is what `list_folders` reads). The
                                // heavy purge runs in the background.
                                if let Some(ref lib) = state_for_dialog.borrow().media_lib {
                                    if let Ok(track_ids) = lib.track_ids_for_folder(folder_id_cb) {
                                        let _ = lib.soft_delete_tracks(&track_ids);
                                    }
                                    let _ = lib.remove_folder(folder_id_cb);
                                }

                                // Rebuild UI immediately — folder is now gone.
                                rebuild_for_dialog();
                                status_for_dialog.set_text(&format!("Removed: {}", folder_path_cb));

                                // Trigger Media Library window to refresh if open
                                if let Some(ref cb) = state_for_dialog.borrow().rebuild_ml_callback {
                                    cb();
                                }
                                // Folder gone — restart the live watcher so
                                // it stops watching the removed folder.
                                watch::rebuild_watcher(&state_for_dialog);

                                // Background: purge the soft-deleted track rows.
                                let db_path = crate::media_library::MediaLibrary::db_path_pub();
                                std::thread::spawn(move || {
                                    if let Ok(lib) =
                                        crate::media_library::MediaLibrary::open_at(&db_path)
                                    {
                                        let _ = lib.purge_deleted_tracks();
                                    }
                                });
                            }
                        },
                    );
                }
            }
        });

        grid.attach(&lbl_folders, 0, 0, 2, 1);
        grid.attach(&btn_add_folder, 2, 0, 1, 1);
        grid.attach(&btn_remove, 3, 0, 1, 1);
        grid.attach(&folder_scroll, 0, 1, 4, 1);
        grid.attach(&status_lbl, 0, 2, 4, 1);

        // Row 3: Rescan button (shares state with media library window).
        let lbl_rescan = Label::new(Some("Scan:"));
        lbl_rescan.set_halign(Align::Start);

        let btn_rescan = Button::with_label("⟳ Rescan");
        let btn_cancel_scan = Button::with_label("✕ Cancel Scan");
        btn_cancel_scan.set_visible(false);
        // Let "Add Folder" trigger a rescan once a concurrent scan finishes.
        *rescan_holder.borrow_mut() = Some(btn_rescan.clone());

        let status_scan = Label::new(None);
        status_scan.set_halign(Align::Start);
        status_scan.add_css_class("dim-label");

        // Update button visibility based on scan state.
        // Clone references for the closure to avoid moving the originals.
        let state_rc_for_update = state.clone();
        let btn_rescan_ref = btn_rescan.clone();
        let btn_cancel_ref = btn_cancel_scan.clone();
        let btn_add_folder_ref = btn_add_folder.clone();
        let status_ref = status_scan.clone();
        let update_scan_ui = Rc::new(move || {
            let scan_state = state_rc_for_update.borrow().ml_scan.clone();
            if let Some(scan) = scan_state {
                btn_rescan_ref.set_visible(false);
                btn_cancel_ref.set_visible(true);
                // Disable Add Folder so a second concurrent scan cannot be started.
                btn_add_folder_ref.set_sensitive(false);
                if scan.total > 0 {
                    status_ref.set_text(&format!("Scanning {} / {}…", scan.current, scan.total));
                } else {
                    status_ref.set_text("Scanning…");
                }
            } else {
                btn_rescan_ref.set_visible(true);
                btn_cancel_ref.set_visible(false);
                btn_add_folder_ref.set_sensitive(true);
                status_ref.set_text("");
            }
        });

        // Initial UI state.
        update_scan_ui();

        // Refresh scan UI when this tab is shown.
        {
            let update_cb = update_scan_ui.clone();
            notebook.connect_switch_page(move |_, _, _| {
                update_cb();
            });
        }

        // Rescan button: trigger a full rescan of all watched folders.
        // Note: This shares state with the media library window via state.ml_scan.
        {
            let state_rc = state.clone();
            let btn_rescan_ref = btn_rescan.clone();
            let btn_cancel_ref = btn_cancel_scan.clone();
            let status_ref = status_scan.clone();

            btn_rescan.connect_clicked(move |_| {
                if state_rc.borrow().ml_scan.is_some() {
                    status_ref.set_text("Scan already in progress");
                    return;
                }
                if state_rc.borrow().media_lib.is_none() {
                    status_ref.set_text("Error: Media library not available");
                    return;
                }

                let db_path = crate::media_library::MediaLibrary::db_path_pub();

                let cancel_flag = start_ml_scan(&state_rc, ScanType::Rescan, 0);
                status_ref.set_text("Reading tags…");
                btn_rescan_ref.set_sensitive(false);
                btn_cancel_ref.set_visible(true);

                let (progress_tx, progress_rx) = std::sync::mpsc::channel();
                let (result_tx, result_rx) = std::sync::mpsc::channel();

                std::thread::spawn(move || {
                    let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
                        Ok(l) => l,
                        Err(e) => {
                            let _ = result_tx.send(Err(format!("DB error: {e}")));
                            return;
                        }
                    };
                    // Clear last_scanned for tracks with no metadata so scan_folder
                    // re-processes them (handles recovery from a prior broken scan).
                    let _ = lib.reset_unscanned_metadata();
                    let result = lib
                        .scan_all_folders(&cancel_flag, |current, total| {
                            let _ = progress_tx.send((current, total));
                        })
                        .map_err(|e| e.to_string());
                    let _ = result_tx.send(result);
                });

                let progress_rx = std::cell::RefCell::new(progress_rx);
                let result_rx = std::cell::RefCell::new(result_rx);
                let state_rc2 = state_rc.clone();
                let status_ref2 = status_ref.clone();
                let btn_rescan_ref2 = btn_rescan_ref.clone();
                let btn_cancel_ref2 = btn_cancel_ref.clone();
                glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                    // Check for progress updates
                    while let Ok((current, total)) = progress_rx.borrow().try_recv() {
                        update_ml_scan_progress(&state_rc2, current, total);
                        status_ref2.set_text(&format!("Reading tags {}/{}…", current, total));
                    }

                    // Check for completion
                    if let Ok(result) = result_rx.borrow().try_recv() {
                        {
                            let mut s = state_rc2.borrow_mut();
                            s.media_lib = crate::media_library::MediaLibrary::open().ok();
                        }
                        complete_ml_scan(&state_rc2);
                        if let Some(ref cb) = state_rc2.borrow().rebuild_ml_callback {
                            cb();
                        }
                        // Compact after a successful FULL rescan only, gated
                        // on the setting — VACUUM is too heavy to run after
                        // every fast folder-add, which is why this lives
                        // here and not in the shared complete_ml_scan.
                        if result.is_ok() {
                            let compact = state_rc2.borrow().config.media_library.compact_on_rescan;
                            if compact {
                                if let Some(ref lib) = state_rc2.borrow().media_lib {
                                    if let Err(e) = lib.compact() {
                                        eprintln!("compact_on_rescan: VACUUM failed: {e}");
                                    }
                                }
                            }
                        }
                        match result {
                            Err(e) => status_ref2.set_text(&format!("Rescan error: {}", e)),
                            Ok(_) => status_ref2.set_text("Scan complete"),
                        }
                        btn_rescan_ref2.set_sensitive(true);
                        btn_cancel_ref2.set_visible(false);
                        glib::ControlFlow::Break
                    } else {
                        glib::ControlFlow::Continue
                    }
                });
            });
        }

        // Cancel scan button.
        {
            let state_rc = state.clone();
            let status_ref = status_scan.clone();
            btn_cancel_scan.connect_clicked(move |_| {
                cancel_ml_scan(&state_rc);
                status_ref.set_text("Cancelling…");
            });
        }

        // Polling timer to sync scan state with UI.
        {
            let update_ui = update_scan_ui.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                update_ui();
                glib::ControlFlow::Continue
            });
        }

        grid.attach(&lbl_rescan, 0, 3, 1, 1);
        grid.attach(&btn_rescan, 1, 3, 1, 1);
        grid.attach(&btn_cancel_scan, 1, 3, 1, 1);
        grid.attach(&status_scan, 2, 3, 2, 1);

        // Row 4: Deduplication
        let sep_row4 = gtk4::Separator::new(Orientation::Horizontal);
        sep_row4.set_margin_top(4);
        sep_row4.set_margin_bottom(4);
        grid.attach(&sep_row4, 0, 4, 4, 1);

        let btn_dedupe = Button::with_label("Deduplicate Music…");
        btn_dedupe.set_tooltip_text(Some(
            "Find tracks that appear more than once in your library",
        ));
        btn_dedupe.set_hexpand(false);
        btn_dedupe.set_halign(Align::Start);
        {
            let state_rc = state.clone();
            let win_wk = win.downgrade();
            btn_dedupe.connect_clicked(move |_| {
                open_dedupe_window(
                    win_wk.upgrade().as_ref(),
                    state_rc.clone(),
                );
            });
        }
        grid.attach(&btn_dedupe, 0, 5, 4, 1);

        // Row 6/7: ReplayGain analysis (phase 4). These are library-scan
        // behavior, not the playback chain itself — that lives on the
        // Behavior tab — so config-save only, no `apply_replaygain()` call.
        let sep_row6 = gtk4::Separator::new(Orientation::Horizontal);
        sep_row6.set_margin_top(4);
        sep_row6.set_margin_bottom(4);
        grid.attach(&sep_row6, 0, 6, 4, 1);

        let lbl_rg_auto = Label::new(Some("Analyze ReplayGain on add/scan"));
        lbl_rg_auto.set_halign(Align::Start);
        grid.attach(&lbl_rg_auto, 0, 7, 1, 1);
        let chk_rg_auto = CheckButton::new();
        chk_rg_auto.set_active(state.borrow().config.playback.replaygain.auto_analyze);
        {
            let state_rc = state.clone();
            chk_rg_auto.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.playback.replaygain.auto_analyze = c.is_active();
                let _ = s.config.save();
            });
        }
        grid.attach(&chk_rg_auto, 1, 7, 1, 1);

        let lbl_rg_write = Label::new(Some("Write ReplayGain tags to files"));
        lbl_rg_write.set_halign(Align::Start);
        grid.attach(&lbl_rg_write, 0, 8, 1, 1);
        let chk_rg_write = CheckButton::new();
        chk_rg_write.set_active(state.borrow().config.playback.replaygain.write_tags);
        {
            let state_rc = state.clone();
            chk_rg_write.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.playback.replaygain.write_tags = c.is_active();
                let _ = s.config.save();
            });
        }
        grid.attach(&chk_rg_write, 1, 8, 1, 1);

        // Row 9/10: on-demand whole-library ReplayGain analysis. One button
        // drives both modes; the "Force recalculate" checkbox picks which:
        //   unchecked → analyze only tracks missing or stale (needs_analysis)
        //   checked   → recompute every track regardless.
        // Shares the `analyze_job` worker/progress plumbing with the Files
        // view's bulk button and context action.
        let rg_available = crate::replaygain::rg_analysis_available();

        // Analyze and Cancel share one cell — `sync_rg_ui` shows exactly one
        // at a time (Analyze idle, Cancel while running), matching the Files
        // view's Analyze⇄Cancel toggle.
        let rg_btn_box = gtk4::Box::new(Orientation::Horizontal, 8);
        rg_btn_box.set_halign(Align::Start);

        let btn_rg_analyze = Button::with_label("Analyze ReplayGain");
        btn_rg_analyze.set_tooltip_text(Some(
            "Analyze the whole library. Without 'Force recalculate' only \
             tracks missing a value or changed since the last scan are done.",
        ));
        if !rg_available {
            btn_rg_analyze.set_sensitive(false);
            btn_rg_analyze
                .set_tooltip_text(Some("rganalysis GStreamer element not available"));
        }
        rg_btn_box.append(&btn_rg_analyze);

        let btn_rg_cancel = Button::with_label("✕ Cancel Analysis");
        btn_rg_cancel.add_css_class("destructive");
        btn_rg_cancel.set_visible(false);
        rg_btn_box.append(&btn_rg_cancel);
        grid.attach(&rg_btn_box, 0, 9, 1, 1);

        let chk_rg_force = CheckButton::with_label("Force recalculate");
        chk_rg_force.set_halign(Align::Start);
        chk_rg_force.set_tooltip_text(Some(
            "Recompute ReplayGain for every track, even ones already analyzed",
        ));
        grid.attach(&chk_rg_force, 1, 9, 1, 1);

        let lbl_rg_status = Label::new(None);
        lbl_rg_status.set_halign(Align::Start);
        lbl_rg_status.add_css_class("dim-label");
        grid.attach(&lbl_rg_status, 0, 10, 4, 1);

        {
            let state_rc = state.clone();
            let force_chk = chk_rg_force.clone();
            let status = lbl_rg_status.clone();
            btn_rg_analyze.connect_clicked(move |_| {
                // Snapshot every track first — drop the borrow before
                // `analyze_job` (it borrows AppState internally).
                let tracks = {
                    let s = state_rc.borrow();
                    match s.media_lib.as_ref() {
                        Some(lib) => lib.all_tracks().unwrap_or_default(),
                        None => Vec::new(),
                    }
                };
                let force = force_chk.is_active();
                // Settings has no track view to rebuild; the Files view picks
                // up new values on its next open. No-op rebuild.
                let rebuild: Rc<dyn Fn()> = Rc::new(|| {});
                analyze_job(&state_rc, tracks, force, &status, rebuild);
            });
        }
        {
            let state_rc = state.clone();
            let status = lbl_rg_status.clone();
            btn_rg_cancel.connect_clicked(move |_| {
                cancel_rg_job(&state_rc);
                status.set_text("Cancelling…");
            });
        }
        // Poll timer drives progress / completion / Analyze⇄Cancel toggle via
        // the same `sync_rg_ui` the Files view uses, so both windows stay in
        // lock-step (even for a job started from the other one). Weak window
        // ref so closing Settings ends the timer instead of ticking on dead
        // widgets forever.
        {
            let state_rc = state.clone();
            let analyze_ref = btn_rg_analyze.clone();
            let cancel_ref = btn_rg_cancel.clone();
            let status_ref = lbl_rg_status.clone();
            let win_weak = win.downgrade();
            let rg_was_running = std::cell::Cell::new(false);
            glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                if win_weak.upgrade().is_none() {
                    return glib::ControlFlow::Break;
                }
                let running = sync_rg_ui(
                    &state_rc,
                    &analyze_ref,
                    &cancel_ref,
                    &status_ref,
                    rg_available,
                    false,
                    true,
                    rg_was_running.get(),
                );
                rg_was_running.set(running);
                glib::ControlFlow::Continue
            });
        }

        // Row 11: separator before the live-watch / auto-add settings below.
        let sep_row11 = gtk4::Separator::new(Orientation::Horizontal);
        sep_row11.set_margin_top(4);
        sep_row11.set_margin_bottom(4);
        grid.attach(&sep_row11, 0, 11, 4, 1);

        // Row 12: watch folders for changes (Phase 8 Task 10). Rebuilds the
        // live watcher immediately on toggle so turning it off actually
        // stops watching, rather than waiting for the next folder change.
        let lbl_watch = Label::new(Some("Watch folders for changes"));
        lbl_watch.set_halign(Align::Start);
        grid.attach(&lbl_watch, 0, 12, 1, 1);
        let chk_watch = CheckButton::new();
        chk_watch.set_active(state.borrow().config.media_library.watch_folders);
        {
            let state_rc = state.clone();
            chk_watch.connect_toggled(move |c| {
                {
                    let mut s = state_rc.borrow_mut();
                    s.config.media_library.watch_folders = c.is_active();
                    let _ = s.config.save();
                }
                watch::rebuild_watcher(&state_rc);
            });
        }
        grid.attach(&chk_watch, 1, 12, 1, 1);

        // Row 13: auto-add-played.
        let lbl_auto_add = Label::new(Some("Automatically add played tracks"));
        lbl_auto_add.set_halign(Align::Start);
        grid.attach(&lbl_auto_add, 0, 13, 1, 1);
        let chk_auto_add = CheckButton::new();
        chk_auto_add.set_active(state.borrow().config.media_library.auto_add_played);
        {
            let state_rc = state.clone();
            chk_auto_add.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.media_library.auto_add_played = c.is_active();
                let _ = s.config.save();
            });
        }
        grid.attach(&chk_auto_add, 1, 13, 1, 1);

        // Row 14: remove missing files on rescan (also gates the live
        // watcher's Remove handling — see `apply_watch_action`).
        let lbl_remove_missing = Label::new(Some("Remove missing files on rescan"));
        lbl_remove_missing.set_halign(Align::Start);
        grid.attach(&lbl_remove_missing, 0, 14, 1, 1);
        let chk_remove_missing = CheckButton::new();
        chk_remove_missing
            .set_active(state.borrow().config.media_library.remove_missing_on_rescan);
        {
            let state_rc = state.clone();
            chk_remove_missing.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.media_library.remove_missing_on_rescan = c.is_active();
                let _ = s.config.save();
            });
        }
        grid.attach(&chk_remove_missing, 1, 14, 1, 1);

        // Row 15: compact database after rescan.
        let lbl_compact = Label::new(Some("Compact database after rescan"));
        lbl_compact.set_halign(Align::Start);
        grid.attach(&lbl_compact, 0, 15, 1, 1);
        let chk_compact = CheckButton::new();
        chk_compact.set_active(state.borrow().config.media_library.compact_on_rescan);
        {
            let state_rc = state.clone();
            chk_compact.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.media_library.compact_on_rescan = c.is_active();
                let _ = s.config.save();
            });
        }
        grid.attach(&chk_compact, 1, 15, 1, 1);

        // Row 16: rescan all folders on startup.
        let lbl_rescan_startup = Label::new(Some("Rescan all folders on startup"));
        lbl_rescan_startup.set_halign(Align::Start);
        grid.attach(&lbl_rescan_startup, 0, 16, 1, 1);
        let chk_rescan_startup = CheckButton::new();
        chk_rescan_startup.set_active(state.borrow().config.media_library.rescan_on_startup);
        {
            let state_rc = state.clone();
            chk_rescan_startup.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.media_library.rescan_on_startup = c.is_active();
                let _ = s.config.save();
            });
        }
        grid.attach(&chk_rescan_startup, 1, 16, 1, 1);

        // Row 17: remember each view's search query (F12.1).
        let lbl_remember_search = Label::new(Some("Remember search per view"));
        lbl_remember_search.set_halign(Align::Start);
        grid.attach(&lbl_remember_search, 0, 17, 1, 1);
        let chk_remember_search = CheckButton::new();
        chk_remember_search.set_active(state.borrow().config.media_library.remember_search);
        {
            let state_rc = state.clone();
            chk_remember_search.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.media_library.remember_search = c.is_active();
                let _ = s.config.save();
            });
        }
        grid.attach(&chk_remember_search, 1, 17, 1, 1);

        // Row 18: treat artist as album artist when the tag is blank (F12.2).
        let lbl_artist_as_album = Label::new(Some("Treat artist as album artist"));
        lbl_artist_as_album.set_halign(Align::Start);
        grid.attach(&lbl_artist_as_album, 0, 18, 1, 1);
        let chk_artist_as_album = CheckButton::new();
        chk_artist_as_album
            .set_active(state.borrow().config.media_library.artist_as_album_artist);
        {
            let state_rc = state.clone();
            chk_artist_as_album.connect_toggled(move |c| {
                {
                    let mut s = state_rc.borrow_mut();
                    s.config.media_library.artist_as_album_artist = c.is_active();
                    let _ = s.config.save();
                }
                // F12.2: the ML window is a singleton, so an already-open
                // window's Files/Editor cells won't re-bind on their own —
                // force a refresh of both so the toggle is live within the
                // session (mirrors the ID3-save precedent, which refreshes
                // the same two views after a tag edit).
                if let Some(ref cb) = state_rc.borrow().rebuild_ml_callback {
                    cb();
                }
                notify_editor_refresh();
            });
        }
        grid.attach(&chk_artist_as_album, 1, 18, 1, 1);

        // Row 19: skip database load at startup (F12.3). Takes effect on the
        // NEXT launch — this session's `media_lib` (already open or already
        // deferred) is untouched, matching how `rescan_on_startup` above is
        // a next-launch setting too.
        let lbl_skip_db_load = Label::new(Some("Skip database load at startup"));
        lbl_skip_db_load.set_halign(Align::Start);
        grid.attach(&lbl_skip_db_load, 0, 19, 1, 1);
        let chk_skip_db_load = CheckButton::new();
        chk_skip_db_load.set_active(state.borrow().config.media_library.skip_db_load);
        {
            let state_rc = state.clone();
            chk_skip_db_load.connect_toggled(move |c| {
                let mut s = state_rc.borrow_mut();
                s.config.media_library.skip_db_load = c.is_active();
                let _ = s.config.save();
            });
        }
        grid.attach(&chk_skip_db_load, 1, 19, 1, 1);

        let tab_lbl = Label::with_mnemonic(SETTINGS_TAB_LABELS[3]);
        notebook.append_page(&settings_scroll_page(&grid), Some(&tab_lbl));
}
