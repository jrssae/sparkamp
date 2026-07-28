//! Live filesystem watcher lifecycle for the GTK frontend (Phase 8 Task 10).
//!
//! Child module of [`super`] (window/mod.rs), following the same pattern as
//! `disc`/`mpris`: presentation-and-glue only. The real watch logic lives in
//! `crate::watch` (pure event classification + the `notify` debouncer) and
//! `crate::media_library` (the DB writes an event maps to).
//!
//! Three entry points:
//! - [`rebuild_watcher`] — (re)starts `AppState.watch`/`watch_rx` from the
//!   current config + folder list. Called at window build, and again
//!   whenever folders, a folder's recurse flag, or the watch-folders toggle
//!   change (settings.rs, media_library.rs call sites).
//! - [`start_drain_tick`] — registers ONE glib timer (call once, at window
//!   build) that drains `watch_rx` and applies events to the library.
//! - [`trigger_startup_rescan`] — fires the same `scan_all_folders` core
//!   call the Settings/ML window Rescan buttons use, for
//!   `config.media_library.rescan_on_startup`.

use gtk4::glib;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use super::{complete_ml_scan, start_ml_scan, update_ml_scan_progress, AppState, ScanType};
use crate::watch::FolderWatcher;

/// Sparkamp's own cache directory. Must match the prefix every other cache
/// consumer uses (`tags.rs`, `now_playing.rs`, `media_library/queries.rs`)
/// so `classify_paths` correctly excludes Sparkamp's own writes (cached
/// artwork, thumbnails) from watch events.
fn cache_prefix() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("sparkamp")
}

/// (Re)build the live folder watcher from the current config + folder list.
///
/// Always tears down any existing watcher first (a stale watcher pointed at
/// removed/renamed folders is worse than a brief gap with none running),
/// then starts a fresh one if `config.media_library.watch_folders` is on,
/// `media_lib` is open, and there is at least one watched folder.
///
/// Graceful degradation only: if the underlying OS watcher fails to start
/// (e.g. the inotify `max_user_watches` limit is exhausted), this logs and
/// leaves `watch`/`watch_rx` at `None` — manual Rescan still works. Never
/// panics.
pub(super) fn rebuild_watcher(state: &Rc<RefCell<AppState>>) {
    // Snapshot everything needed to start the watcher, then drop the borrow
    // before the (I/O-bound) `FolderWatcher::start` call.
    let plan = {
        let s = state.borrow();
        if !s.config.media_library.watch_folders {
            None
        } else {
            s.media_lib.as_ref().map(|lib| {
                let folders: Vec<(PathBuf, bool)> = lib
                    .list_folders()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(id, path)| {
                        let recurse = lib.folder_recurse(id).unwrap_or(true);
                        (PathBuf::from(path), recurse)
                    })
                    .collect();
                let audio_exts: Vec<String> = crate::model::AUDIO_EXTENSIONS
                    .iter()
                    .map(|ext| ext.to_string())
                    .collect();
                (folders, audio_exts)
            })
        }
    };

    // Tear down whatever was running before (re)starting — see doc comment.
    {
        let mut s = state.borrow_mut();
        if let Some(old) = s.watch.take() {
            old.stop();
        }
        s.watch_rx = None;
    }

    let Some((folders, audio_exts)) = plan else {
        return;
    };
    if folders.is_empty() {
        return;
    }

    match FolderWatcher::start(folders, audio_exts, cache_prefix()) {
        Ok((watcher, rx)) => {
            let mut s = state.borrow_mut();
            s.watch = Some(watcher);
            s.watch_rx = Some(rx);
        }
        Err(e) => {
            // Not surfaced in the UI — this mirrors how other background
            // pollers (disc watcher, MPRIS init) degrade on failure: log and
            // move on, rather than a dialog for something the user didn't
            // directly trigger. Manual Rescan remains available.
            eprintln!("[watch] failed to start folder watcher: {e}");
        }
    }
}

/// Register the drain tick. Call exactly ONCE, at window build.
///
/// Every 500 ms: drain all pending `WatchAction`s from `watch_rx` and apply
/// them to `media_lib` (the same DB writes a manual rescan produces), all
/// under one short `borrow_mut()` — then, only if anything was actually
/// applied, and only AFTER that borrow is dropped, invoke
/// `rebuild_ml_callback` so an open Files view picks up the change live.
///
/// Borrow discipline: the callback is never invoked while `state` is still
/// borrowed. A callback that itself needs `state.borrow()` (very likely,
/// e.g. to re-read the track list) would otherwise panic — this is the
/// project's core `Rc<RefCell<AppState>>` rule, and it applies with extra
/// force here since this tick runs unconditionally, forever, on a timer.
pub(super) fn start_drain_tick(state: &Rc<RefCell<AppState>>) {
    let state = state.clone();
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let rebuild_cb = {
            // Immutable borrow only: `watch_rx.try_recv()` and
            // `media_lib.apply_watch_action()` both take `&self` (SQLite's
            // interior mutability, and `mpsc::Receiver::try_recv` likewise),
            // so no field on `AppState` itself needs to change here.
            let s = state.borrow();
            let mut actions = Vec::new();
            if let Some(ref rx) = s.watch_rx {
                while let Ok(action) = rx.try_recv() {
                    actions.push(action);
                }
            }
            let mut applied_any = false;
            if !actions.is_empty() {
                let remove_missing = s.config.media_library.remove_missing_on_rescan;
                if let Some(ref lib) = s.media_lib {
                    for action in &actions {
                        match lib.apply_watch_action(action, remove_missing) {
                            Ok(()) => applied_any = true,
                            Err(e) => eprintln!("[watch] apply_watch_action failed: {e}"),
                        }
                    }
                }
            }
            if applied_any {
                s.rebuild_ml_callback.clone()
            } else {
                None
            }
        };
        if let Some(cb) = rebuild_cb {
            cb();
        }
        glib::ControlFlow::Continue
    });
}

/// Trigger a full rescan of every watched folder in the background, for
/// `config.media_library.rescan_on_startup`. Called once, at window build,
/// before any window (including Settings/ML) has necessarily been built —
/// so unlike the Settings/ML "Rescan" buttons, there are no local widgets to
/// drive directly. Reuses the exact same core call (`scan_all_folders`) and
/// `ml_scan` state machinery those buttons use (`start_ml_scan` /
/// `update_ml_scan_progress` / `complete_ml_scan`), so a Settings or ML
/// window opened while this runs shows the same "Scanning…" progress
/// instead of racing a second, independent scan — and refuses to start if
/// one is already running (`ml_scan.is_some()`), same as those buttons do.
pub(super) fn trigger_startup_rescan(state: &Rc<RefCell<AppState>>) {
    {
        let s = state.borrow();
        if s.ml_scan.is_some() || s.media_lib.is_none() {
            return;
        }
    }

    let db_path = crate::media_library::MediaLibrary::db_path_pub();
    // Read before spawning (short borrow, dropped before the thread starts):
    // `scan_all_folders` only re-reads metadata for `tracks` rows that
    // already exist — it never walks the filesystem — so a startup rescan
    // that skipped straight to it would miss files added/removed while the
    // app was closed. Fix 3 (Phase 8 review): walk+prune every folder with
    // `rescan_folder_fast` first, same as the Settings/ML "Rescan" button
    // and TUI/mac startup paths, gated on the same setting they use.
    let remove_missing = state.borrow().config.media_library.remove_missing_on_rescan;
    let cancel_flag = start_ml_scan(state, ScanType::Rescan, 0);

    let (progress_tx, progress_rx) = std::sync::mpsc::channel::<(usize, usize)>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<(usize, usize, usize), String>>();
    std::thread::spawn(move || {
        let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
            Ok(l) => l,
            Err(e) => {
                let _ = result_tx.send(Err(format!("DB error: {e}")));
                return;
            }
        };
        match lib.list_folders() {
            Ok(folders) => {
                for (id, path) in folders {
                    if let Err(e) = lib.rescan_folder_fast(id, &path, remove_missing) {
                        eprintln!("[watch] startup rescan: fast walk of {path}: {e}");
                    }
                }
            }
            Err(e) => eprintln!("[watch] startup rescan: list_folders failed: {e}"),
        }
        let _ = lib.reset_unscanned_metadata();
        let result = lib
            .scan_all_folders(&cancel_flag, |current, total| {
                let _ = progress_tx.send((current, total));
            })
            .map_err(|e| e.to_string());
        let _ = result_tx.send(result);
    });

    let progress_rx = RefCell::new(progress_rx);
    let result_rx = RefCell::new(result_rx);
    let state = state.clone();
    glib::timeout_add_local(Duration::from_millis(500), move || {
        while let Ok((current, total)) = progress_rx.borrow().try_recv() {
            update_ml_scan_progress(&state, current, total);
        }
        if let Ok(result) = result_rx.borrow().try_recv() {
            {
                let mut s = state.borrow_mut();
                s.media_lib = crate::media_library::MediaLibrary::open().ok();
            }
            complete_ml_scan(&state);
            let succeeded = result.is_ok();
            if let Err(e) = result {
                eprintln!("[watch] startup rescan failed: {e}");
            }
            // Compact after a successful FULL rescan only, gated on the
            // setting — VACUUM is too heavy to run after every fast
            // folder-add, which is why this lives here and not in the
            // shared complete_ml_scan.
            if succeeded {
                let compact = state.borrow().config.media_library.compact_on_rescan;
                if compact {
                    if let Some(ref lib) = state.borrow().media_lib {
                        if let Err(e) = lib.compact() {
                            eprintln!("[watch] compact_on_rescan: VACUUM failed: {e}");
                        }
                    }
                }
            }
            let cb = state.borrow().rebuild_ml_callback.clone();
            if let Some(cb) = cb {
                cb();
            }
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}
