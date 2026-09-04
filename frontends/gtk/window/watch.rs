//! Live filesystem watcher lifecycle for the GTK frontend (Phase 8 Task 10).
//!
//! Child module of [`super`] (window/mod.rs), following the same pattern as
//! `disc`/`mpris`: presentation-and-glue only. The real watch logic lives in
//! `sparkamp::watch` (pure event classification + the `notify` debouncer) and
//! `sparkamp::media_library` (the DB writes an event maps to).
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
//!
//! F12.3 (`skip_db_load`) needed no changes here: [`rebuild_watcher`] already
//! gates on `s.media_lib.as_ref()` being `Some` (see below), and
//! [`trigger_startup_rescan`] already returns early when `media_lib.is_none()`
//! — so with the DB left unopened at startup, both already no-op instead of
//! forcing an open, exactly as the binding user decision requires. The
//! watcher starts the first time something actually opens the library — see
//! `state.rs`'s `ensure_media_lib_open`, which calls [`rebuild_watcher`]
//! right after a successful lazy open.

use gtk4::glib;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::{
    complete_ml_scan, notify_playlist_nav_refresh, start_ml_scan, update_ml_scan_progress,
    AppState, ScanType,
};
use sparkamp::watch::FolderWatcher;

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
                let audio_exts: Vec<String> = sparkamp::model::AUDIO_EXTENSIONS
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
            {
                let mut s = state.borrow_mut();
                s.watch = Some(watcher);
                s.watch_rx = Some(rx);
            }
            // There is now a channel to drain. The tick stops itself whenever
            // there isn't, so restarting it here is what turns watching back
            // on after a Settings toggle.
            start_drain_tick(state);
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

/// How long the watcher has to stay quiet before the drain tick refreshes the
/// UI. One tick's worth of slack on top of the 500 ms period: long enough that
/// a sustained ingest collapses to a single rebuild, short enough that adding
/// one file to a watched folder still feels live.
const REBUILD_QUIET: Duration = Duration::from_millis(1000);

/// How much of one 500 ms tick the drain may spend applying events.
///
/// `apply_watch_action` is not cheap: an Upsert runs
/// `ProbedTrackMetadata::probe`, which reads the file's tags and audio header
/// off disk, then writes a row — call it ~10 ms on a spinning disk. It runs
/// here, synchronously, on the GTK main thread, because SQLite is not `Send`
/// and the connection lives in `AppState`.
///
/// Until 2026-08-11 the tick drained the whole channel and applied every
/// event it found, with no bound at all. A folder sync delivering ~700 files
/// a minute therefore handed the main thread hundreds of disk reads in a
/// single main-loop iteration, and the app stopped responding — a user
/// scrolling the Files view through one had to force-quit it twice.
///
/// A budget rather than a fixed count, because it is disk time that matters
/// and a fixed count means something different on an SSD than on the HDD this
/// showed up on. Leftover events stay queued; a backlog drains at roughly
/// 200 ms of work per second of wall clock instead of all at once.
const APPLY_BUDGET: Duration = Duration::from_millis(100);

/// Repair track paths stored under a non-canonical spelling of their folder,
/// on a worker thread. Call once, at window build.
///
/// `MediaLibrary::open` canonicalizes folder rows on every start
/// (`dedup_folders`) but has never rewritten the track rows underneath them,
/// so a library can hold the same file twice — once per spelling — and every
/// scan adds more. See `normalize_track_paths`.
///
/// The decision to run is pure SQL and costs nothing; the repair itself is one
/// `stat` per row, which is why it goes to a worker with its own connection
/// (rusqlite `Connection` is not `Send`) rather than blocking the window.
pub(super) fn start_path_normalization(state: &Rc<RefCell<AppState>>) {
    let needed = state
        .borrow()
        .media_lib
        .as_ref()
        .map(|lib| lib.needs_path_normalization())
        .unwrap_or(false);
    if !needed {
        return;
    }
    let db_path = sparkamp::media_library::MediaLibrary::db_path_pub();
    let state = state.clone();
    let (tx, rx) = std::sync::mpsc::channel::<(usize, usize)>();
    std::thread::spawn(move || {
        let Ok(lib) = sparkamp::media_library::MediaLibrary::open_at(&db_path) else {
            return;
        };
        match lib.normalize_track_paths() {
            Ok(counts) => {
                let _ = tx.send(counts);
            }
            Err(e) => eprintln!("[watch] path normalization failed: {e}"),
        }
    });
    // Reopen the main connection once the worker is done, so the window stops
    // reading rows the migration has moved out from under it, then refresh.
    let rx = RefCell::new(rx);
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let Ok((moved, merged)) = rx.borrow().try_recv() else {
            return glib::ControlFlow::Continue;
        };
        eprintln!("[watch] path normalization: {moved} moved, {merged} duplicates merged");
        {
            let mut s = state.borrow_mut();
            s.media_lib = sparkamp::media_library::MediaLibrary::open().ok();
        }
        let cb = state.borrow().rebuild_ml_callback.clone();
        if let Some(cb) = cb {
            cb();
        }
        glib::ControlFlow::Break
    });
}

thread_local! {
    /// Whether a drain tick is currently registered.
    ///
    /// The tick drives the one `AppState` and GTK is single-threaded, so a
    /// thread-local flag is enough. Guards against a second timer being
    /// registered when `rebuild_watcher` runs again while one is still live.
    static DRAIN_TICK_RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// Register the drain tick, unless one is already running.
///
/// Safe to call repeatedly: [`rebuild_watcher`] calls it every time it starts
/// a watcher, and window build calls it once. The tick stops itself when there
/// is no channel left to drain — watching switched off in Settings, no watched
/// folders, or the library not open — so it exists exactly while it has work
/// to do rather than waking 120 times a minute to look at a `None`.
///
/// Every 500 ms: drain all pending `WatchAction`s from `watch_rx` and apply
/// them to `media_lib` (the same DB writes a manual rescan produces), all
/// under one short `borrow_mut()`. The UI refresh that follows is
/// **coalesced**: the tick records that something changed and only invokes
/// `rebuild_ml_callback` once the events have stopped arriving for
/// [`REBUILD_QUIET`].
///
/// It refreshed on every tick that saw an event until 2026-08-11. That is
/// fine for the one-file-at-a-time case it was written for, and ruinous for
/// a bulk ingest: a folder sync delivering ~10 files/s makes every tick
/// dirty, and each refresh is a full pass over the library — 474 ms at 39k
/// tracks, on the GTK main thread, plus a whole-store `splice` that rebinds
/// every visible row and re-reads its artwork. Twice a second that starves
/// the main loop; a user dragging the scrollbar through one such ingest saw
/// the view jump to the end mid-drag, and then the app hang hard enough to
/// need a force-quit.
///
/// Borrow discipline: the callback is never invoked while `state` is still
/// borrowed. A callback that itself needs `state.borrow()` (very likely,
/// e.g. to re-read the track list) would otherwise panic — this is the
/// project's core `Rc<RefCell<AppState>>` rule, and it applies with extra
/// force here since this tick runs unconditionally, forever, on a timer.
pub(super) fn start_drain_tick(state: &Rc<RefCell<AppState>>) {
    if DRAIN_TICK_RUNNING.with(|f| f.replace(true)) {
        return;
    }
    let state = state.clone();
    // Set when events have been applied but the UI has not caught up yet, and
    // the moment the last one landed. A steady stream keeps pushing the
    // deadline out, so one burst costs one rebuild instead of one per tick.
    let dirty = Cell::new(false);
    let dirty_playlist = Cell::new(false);
    let last_event = Cell::new(Instant::now());
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let applied = {
            // Immutable borrow only: `watch_rx.try_recv()` and
            // `media_lib.apply_watch_action()` both take `&self` (SQLite's
            // interior mutability, and `mpsc::Receiver::try_recv` likewise),
            // so no field on `AppState` itself needs to change here.
            let s = state.borrow();
            // Nothing to drain: watching was switched off, the folder list is
            // empty, or the library isn't open. Stand down rather than wake
            // twice a second to re-discover that. `rebuild_watcher` starts a
            // fresh tick the moment a channel exists again.
            if s.watch_rx.is_none() {
                DRAIN_TICK_RUNNING.with(|f| f.set(false));
                return glib::ControlFlow::Break;
            }
            let mut applied_any = false;
            // A playlist file needs a different refresh from a track: it
            // changes the sidebar's Playlists sub-rows, which
            // `rebuild_ml_callback` does not touch.
            let mut applied_playlist = false;
            if let (Some(rx), Some(lib)) = (s.watch_rx.as_ref(), s.media_lib.as_ref()) {
                let remove_missing = s.config.media_library.remove_missing_on_rescan;
                // Apply as we receive, under a time budget, rather than
                // draining the channel into a Vec and working through all of
                // it. Anything left stays queued for the next tick.
                let deadline = Instant::now() + APPLY_BUDGET;
                while let Ok(action) = rx.try_recv() {
                    match lib.apply_watch_action(&action, remove_missing) {
                        Ok(()) => {
                            applied_any = true;
                            if matches!(action, sparkamp::watch::WatchAction::PlaylistUpsert(_)) {
                                applied_playlist = true;
                            }
                        }
                        Err(e) => eprintln!("[watch] apply_watch_action failed: {e}"),
                    }
                    if Instant::now() >= deadline {
                        break;
                    }
                }
            }
            (applied_any, applied_playlist)
        };
        let (applied_any, applied_playlist) = applied;

        if applied_any {
            dirty.set(true);
            if applied_playlist {
                dirty_playlist.set(true);
            }
            last_event.set(Instant::now());
            // Still arriving — let the next tick decide.
            return glib::ControlFlow::Continue;
        }

        if !dirty.get() || last_event.get().elapsed() < REBUILD_QUIET {
            return glib::ControlFlow::Continue;
        }
        dirty.set(false);

        // Borrow dropped above before either refresh runs; both read `state`.
        let rebuild_cb = state.borrow().rebuild_ml_callback.clone();
        if let Some(cb) = rebuild_cb {
            cb();
        }
        if dirty_playlist.replace(false) {
            notify_playlist_nav_refresh();
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

    let db_path = sparkamp::media_library::MediaLibrary::db_path_pub();
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
        let lib = match sparkamp::media_library::MediaLibrary::open_at(&db_path) {
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
                s.media_lib = sparkamp::media_library::MediaLibrary::open().ok();
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
