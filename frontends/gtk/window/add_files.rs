use super::*;

/// The playlist window's three add buttons — Add File, Add Files, Add
/// Folder — and the two-phase scan behind them: known tracks appear at
/// once, the rest arrive as the duration probe finishes with them.
///
/// Split out of `player::build` (breakup step 9b). Flows ten bindings in
/// and none out.
///
/// `btn_save_active` and `btn_cancel` are separate arguments rather than
/// [`PlayerCtx`] fields: the Save button and the scan-cancel button are
/// read here and by the playlist window that made them, and nowhere else.
pub(super) fn install(ctx: &PlayerCtx, btn_save_active: &Button, btn_cancel: &Button) {
    // Aliased under their original names so the moved body is unchanged.
    let state = ctx.state.clone();
    let playlist_win = ctx.playlist_win.clone();
    let set_track = ctx.set_track.clone();
    let probe_tx = ctx.probe_tx.clone();
    let broken_tx = ctx.broken_tx.clone();
    let pl_status_label = ctx.pl_status_label.clone();
    let btn_add_files = ctx.btn_add_files.clone();
    let btn_add_dir = ctx.btn_add_dir.clone();
    let rebuild_playlist = ctx.rebuild_playlist.clone();
    let patch_pl_row = ctx.patch_pl_row.clone();
    let btn_save_active = btn_save_active.clone();
    let btn_cancel = btn_cancel.clone();

    // Playlist window: Add-file buttons
    // ══════════════════════════════════════════════════════════════════════════

    // Helper: build a FileFilter matching all common audio formats.
    // Used by all three add dialogs to avoid re-creating the filter object.
    let make_audio_filter = || {
        let f = gtk4::FileFilter::new();
        f.set_name(Some("Audio files"));
        // MIME types cover most desktop environments and file managers.
        for mime in &[
            "audio/mpeg",
            "audio/flac",
            "audio/ogg",
            "audio/opus",
            "audio/wav",
            "audio/x-wav",
            "audio/aac",
            "audio/mp4",
            "audio/x-m4a",
            "audio/x-ms-wma",
        ] {
            f.add_mime_type(mime);
        }
        // Extension patterns as fallback for systems without full MIME support.
        for pat in &[
            "*.mp3", "*.flac", "*.ogg", "*.opus", "*.wav", "*.aac", "*.m4a", "*.wma", "*.ape",
            "*.aiff",
        ] {
            f.add_pattern(pat);
        }
        f
    };

    // Cancel button: stops any active playlist scan (Add Folder or Add Files).
    // Wired once here, before the add handlers, so it is always connected.
    btn_cancel.connect_clicked({
        let state = state.clone();
        let pl_status = pl_status_label.clone();
        let cancel_btn = btn_cancel.clone();
        move |_| {
            let s = state.borrow();
            if let Some(ref scan) = s.playlist_scan {
                scan.cancel
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            drop(s);
            pl_status.set_text("Cancelling…");
            cancel_btn.set_visible(false);
        }
    });

    // [+ Files]: open the desktop file browser to pick one or more audio files.
    // For small selections this is near-instant; for large selections it uses the
    // same two-phase background scan as Add Folder to avoid blocking the UI.
    btn_add_files.connect_clicked({
        let state = state.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let pl_status = pl_status_label.clone();
        let window_wk = playlist_win.downgrade();
        let make_filt = make_audio_filter.clone();
        let probe_tx = probe_tx.clone();
        let broken_tx = broken_tx.clone();
        let cancel_btn = btn_cancel.clone();
        let patch_pl_row_af = patch_pl_row.clone();
        let set_track_af = set_track.clone();
        move |_| {
            let dialog = gtk4::FileDialog::builder().title("Add Audio Files").build();
            let filter_store = gio::ListStore::new::<gtk4::FileFilter>();
            filter_store.append(&make_filt());
            dialog.set_filters(Some(&filter_store));

            let state_cb = state.clone();
            let rebuild_cb = rebuild_playlist.clone();
            let status_cb = pl_status.clone();
            let probe_tx_cb = probe_tx.clone();
            let broken_tx_cb = broken_tx.clone();
            let cancel_ref = cancel_btn.clone();
            let patch_cb = patch_pl_row_af.clone();
            let set_track_cb = set_track_af.clone();
            let parent = window_wk.upgrade();
            dialog.open_multiple(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                let Ok(list) = result else { return };

                // Collect selected paths on the main thread before spawning.
                let files: Vec<PathBuf> = (0..list.n_items())
                    .filter_map(|i| list.item(i))
                    .filter_map(|obj| obj.downcast::<gio::File>().ok())
                    .filter_map(|f| f.path())
                    .collect();

                if files.is_empty() {
                    return;
                }

                let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                {
                    let mut s = state_cb.borrow_mut();
                    s.playlist_scan = Some(ScanState {
                        scan_type: ScanType::AddFiles,
                        current: 0,
                        total: 0,
                        cancel: cancel.clone(),
                    });
                    s.pending_bg_ops.set(s.pending_bg_ops.get() + 1);
                }

                status_cb.set_text("Scanning…");
                cancel_ref.set_visible(true);

                // Capture where the new tracks will start before any are added.
                let scan_start = state_cb.borrow().playlist.len();

                let (fast_tx, fast_rx) = std::sync::mpsc::channel::<crate::model::Track>();
                let (meta_tx, meta_rx) =
                    std::sync::mpsc::channel::<(usize, String, String, String, String)>();
                let (done_tx, done_rx) = std::sync::mpsc::channel::<usize>();
                let (phase1_done_tx, phase1_done_rx) = std::sync::mpsc::channel::<usize>();

                crate::model::Playlist::scan_files_for_ui(
                    files,
                    cancel,
                    fast_tx,
                    meta_tx,
                    done_tx,
                    phase1_done_tx,
                );

                start_playlist_scan_poller(
                    state_cb.clone(),
                    status_cb.clone(),
                    rebuild_cb.clone(),
                    cancel_ref.clone(),
                    probe_tx_cb.clone(),
                    broken_tx_cb.clone(),
                    patch_cb.clone(),
                    set_track_cb.clone(),
                    fast_rx,
                    meta_rx,
                    done_rx,
                    phase1_done_rx,
                    scan_start,
                );
            });
        }
    });

    // [⤓ Save] active playlist: open the native Save dialog, write the
    // current queue's track paths to the chosen .m3u8 file via the core
    // helper (which emits #EXTINF lines and registers the playlist in
    // the library), then refresh the sidebar so the new entry appears.
    btn_save_active.connect_clicked({
        let state = state.clone();
        let window_wk = playlist_win.downgrade();
        move |_| {
            let Some(win) = window_wk.upgrade() else { return };
            let paths: Vec<String> = state.borrow().playlist.tracks
                .iter().map(|t| t.path.to_string_lossy().into_owned()).collect();
            if paths.is_empty() { return }
            // Timestamped default name (readable, sortable, no colons).
            // Uses glib's local time so we don't add a chrono dependency.
            let default_stem = glib::DateTime::now_local()
                .ok()
                .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Playlist".to_string());
            let state_cb = state.clone();
            run_playlist_save_dialog(state.clone(), win, &default_stem, move |path, win_cb| {
                if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                    if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths) {
                        eprintln!("save_playlist_tracks_to_path: {e}");
                        show_playlist_save_error(&win_cb, &path, &e);
                    }
                }
                notify_playlist_nav_refresh();
            });
        }
    });

    // [+ Folder]: open the desktop folder browser; recursively add all audio files.
    // Uses the same two-phase scan as Add Files: fast tracks appear immediately,
    // metadata fills in as it is read in the background.
    btn_add_dir.connect_clicked({
        let state = state.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let pl_status = pl_status_label.clone();
        let window_wk = playlist_win.downgrade();
        let probe_tx = probe_tx.clone();
        let broken_tx = broken_tx.clone();
        let cancel_btn = btn_cancel.clone();
        let patch_pl_row_adir = patch_pl_row.clone();
        let set_track_adir = set_track.clone();
        move |_| {
            let dialog = gtk4::FileDialog::new();
            dialog.set_title("Add Folder to Playlist");

            let state_cb = state.clone();
            let rebuild_cb = rebuild_playlist.clone();
            let status_cb = pl_status.clone();
            let probe_tx_cb = probe_tx.clone();
            let broken_tx_cb = broken_tx.clone();
            let cancel_ref = cancel_btn.clone();
            let patch_cb = patch_pl_row_adir.clone();
            let set_track_cb = set_track_adir.clone();
            let parent = window_wk.upgrade();
            dialog.select_folder(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                let Ok(file) = result else { return };
                let Some(folder) = file.path() else { return };

                let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                {
                    let mut s = state_cb.borrow_mut();
                    s.playlist_scan = Some(ScanState {
                        scan_type: ScanType::AddFolder,
                        current: 0,
                        total: 0,
                        cancel: cancel.clone(),
                    });
                    s.pending_bg_ops.set(s.pending_bg_ops.get() + 1);
                }

                status_cb.set_text("Scanning…");
                cancel_ref.set_visible(true);

                // Capture where the new tracks will start before any are added.
                let scan_start = state_cb.borrow().playlist.len();

                let (fast_tx, fast_rx) = std::sync::mpsc::channel::<crate::model::Track>();
                let (meta_tx, meta_rx) =
                    std::sync::mpsc::channel::<(usize, String, String, String, String)>();
                let (done_tx, done_rx) = std::sync::mpsc::channel::<usize>();
                let (phase1_done_tx, phase1_done_rx) = std::sync::mpsc::channel::<usize>();

                crate::model::Playlist::scan_folder_for_ui(
                    folder,
                    cancel,
                    fast_tx,
                    meta_tx,
                    done_tx,
                    phase1_done_tx,
                );

                start_playlist_scan_poller(
                    state_cb.clone(),
                    status_cb.clone(),
                    rebuild_cb.clone(),
                    cancel_ref.clone(),
                    probe_tx_cb.clone(),
                    broken_tx_cb.clone(),
                    patch_cb.clone(),
                    set_track_cb.clone(),
                    fast_rx,
                    meta_rx,
                    done_rx,
                    phase1_done_rx,
                    scan_start,
                );
            });
        }
    });

    // ══════════════════════════════════════════════════════════════════════════
}
