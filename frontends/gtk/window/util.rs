/// Read the user's GNOME accent-colour choice from gsettings and return
/// the matching hex string.  Falls back to GNOME's default blue when
/// gsettings is unavailable or the value is unrecognised.
/// Returns the label for the repeat button based on the current mode.
fn repeat_btn_icon(mode: crate::shuffle::RepeatMode) -> &'static str {
    match mode {
        // Song mode shows the dedicated "repeat single" icon. Off and All
        // share the generic "repeat" icon — the .mode-btn-active class on
        // the button distinguishes Off (inactive) from All (active).
        crate::shuffle::RepeatMode::Song => "media-playlist-repeat-song-symbolic",
        crate::shuffle::RepeatMode::Off | crate::shuffle::RepeatMode::Playlist =>
            "media-playlist-repeat-symbolic",
    }
}

/// Returns the visible text for the repeat button — mirrors the macOS
/// PlayerWindow repeatLabel ("Repeat", "Repeat 1", "Repeat All").
fn repeat_btn_text(mode: crate::shuffle::RepeatMode) -> &'static str {
    match mode {
        crate::shuffle::RepeatMode::Off => "Repeat",
        crate::shuffle::RepeatMode::Song => "Repeat 1",
        crate::shuffle::RepeatMode::Playlist => "Repeat All",
    }
}

fn gtk_safe(s: &str) -> String {
    if s.contains('\0') {
        s.replace('\0', "")
    } else {
        s.to_owned()
    }
}

fn sanitize_id3_text(s: &str) -> String {
    gtk_safe(s.trim())
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(256)
        .collect()
}

/// Outcome of one [`refresh_device_cache`] poll, handed to `on_done` after
/// the shared cache has already been written (or cleared). Distinguishes a
/// udisks listing failure — worth diagnosing (permissions, missing udisks2,
/// ...) — from a panicked worker thread, which has no useful error to show.
enum DeviceRefreshOutcome {
    Ok,
    UdisksError(zbus::Error),
    WorkerPanicked,
}

/// One async device-list refresh into the shared `current_devices` cache.
/// Enumerates MTP mount metadata on the main thread (cheap, no FUSE IO),
/// then lists udisks devices + resolves MTP storage roots on a worker
/// thread so a stalled D-Bus/gvfs call can never freeze the UI. Filters out
/// mounted optical data discs (those belong to Disc Drives, not the
/// removable-devices list), merges in the MTP devices, and sorts by label.
/// Guards against overlapping polls via `in_flight`.
///
/// Used by both the player-level poll (fills the cache from app start so
/// the Send-to menu isn't empty until the ML window has been opened once)
/// and the ML window's device poll (rebuilds its sidebar/banner). `on_done`
/// runs after the cache is updated — ML passes its UI-rebuild hook, the
/// player passes a no-op — so a future filter change here reaches both
/// pollers instead of silently missing one.
fn refresh_device_cache(
    current_devices: Rc<RefCell<Vec<crate::devices::Device>>>,
    in_flight: Rc<Cell<bool>>,
    on_done: Rc<dyn Fn(DeviceRefreshOutcome)>,
) {
    if in_flight.get() {
        return;
    }
    in_flight.set(true);
    glib::spawn_future_local(async move {
        let mtp_raw = enumerate_mtp_raw();
        let result = gio::spawn_blocking(move || {
            let udisks = crate::devices::detect::list_devices();
            let mtp: Vec<crate::devices::Device> =
                mtp_raw.into_iter().filter_map(mtp_raw_to_device).collect();
            (udisks, mtp)
        })
        .await;
        in_flight.set(false);
        let outcome = match result {
            Ok((Ok(devs), mtp)) => {
                let mut devs = devs;
                // Mounted optical data discs belong to Disc Drives, not the
                // removable-Devices list — drop them here too.
                devs.retain(|d| !is_optical_fs(&d.fs_type));
                devs.extend(mtp);
                devs.sort_by(|a, b| {
                    a.label
                        .to_lowercase()
                        .cmp(&b.label.to_lowercase())
                        .then_with(|| a.mount_path.cmp(&b.mount_path))
                });
                *current_devices.borrow_mut() = devs;
                DeviceRefreshOutcome::Ok
            }
            // udisks failed — leave the cache empty; the caller decides how
            // (or whether) to surface the diagnostic.
            Ok((Err(e), _mtp)) => {
                current_devices.borrow_mut().clear();
                DeviceRefreshOutcome::UdisksError(e)
            }
            // The worker thread panicked.
            Err(_) => {
                current_devices.borrow_mut().clear();
                DeviceRefreshOutcome::WorkerPanicked
            }
        };
        on_done(outcome);
    });
}

fn sanitize_id3_numeric(s: &str) -> String {
    let trimmed = s.trim();
    let numeric: String = trimmed.chars().filter(|c| c.is_ascii_digit()).collect();
    numeric.chars().take(8).collect()
}

fn format_last_played(iso_timestamp: &str) -> String {
    if iso_timestamp.is_empty() {
        return String::new();
    }
    let parts: Vec<&str> = iso_timestamp
        .trim_end_matches('Z')
        .split(|c| c == 'T' || c == ':' || c == '-')
        .collect();
    if parts.len() < 5 {
        return iso_timestamp.to_string();
    }
    let year = parts[0];
    let month = parts[1];
    let day = parts[2];
    let hour: u32 = parts.get(3).and_then(|h| h.parse().ok()).unwrap_or(0);
    let minute = parts.get(4).unwrap_or(&"00");
    let (hour_12, am_pm) = if hour == 0 {
        (12, "AM")
    } else if hour < 12 {
        (hour, "AM")
    } else if hour == 12 {
        (12, "PM")
    } else {
        (hour - 12, "PM")
    };
    format!(
        "{}-{}-{} {:02}:{} {}",
        year, month, day, hour_12, minute, am_pm
    )
}

#[allow(deprecated)] // EntryCompletion/ListStore — no GTK4 replacement yet
fn make_genre_entry(initial_value: &str) -> gtk4::Entry {
    // Free-text entry with typeahead over the predefined ID3v1 list only —
    // matches the mac editor (D13): suggestions come from the list, but
    // any typed value is accepted and saved verbatim.
    let entry = Entry::new();
    entry.set_text(initial_value);

    let mut genres: Vec<&str> = crate::id3_editor::ID3V1_GENRES.to_vec();
    genres.sort_unstable_by_key(|g| g.to_ascii_lowercase());
    let store = gtk4::ListStore::new(&[glib::types::Type::STRING]);
    for g in &genres {
        store.set(&store.append(), &[(0, g)]);
    }
    let completion = gtk4::EntryCompletion::new();
    completion.set_model(Some(&store));
    completion.set_text_column(0);
    completion.set_popup_completion(true);
    completion.set_minimum_key_length(1);
    entry.set_completion(Some(&completion));
    entry
}

/// Make a sidebar/manage playlist row draggable, carrying `pl:<id>` so a drop
/// onto a device row can send the whole playlist.
fn attach_pl_row_drag(row: &gtk4::ListBoxRow, id: i64) {
    let src = gtk4::DragSource::new();
    src.set_actions(gdk::DragAction::COPY);
    let payload = format!("pl:{id}");
    src.connect_prepare(move |_, _, _| {
        Some(gdk::ContentProvider::for_value(&payload.to_value()))
    });
    row.add_controller(src);
}

/// Index of the ML sidebar's Devices header (= the end of the Playlists
/// section). New playlist rows insert here so they land inside the Playlists
/// section rather than below Devices.
fn sidebar_pl_end_index(sidebar: &gtk4::ListBox) -> i32 {
    let mut idx = 0i32;
    while let Some(r) = sidebar.row_at_index(idx) {
        if r.widget_name() == "devices" {
            return idx;
        }
        idx += 1;
    }
    idx
}

/// Find a ListBoxRow by its widget name.
fn find_row_by_name(listbox: &gtk4::ListBox, name: &str) -> Option<gtk4::ListBoxRow> {
    let mut child = listbox.first_child();
    while let Some(c) = child {
        if let Ok(row) = c.clone().downcast::<gtk4::ListBoxRow>() {
            if row.widget_name().as_str() == name {
                return Some(row);
            }
        }
        child = c.next_sibling();
    }
    None
}

/// Show a modal alert parented to `parent` (avoids the "GtkDialog mapped
/// without a transient parent" warning).
fn show_alert_parented(parent: Option<&gtk4::Window>, msg: &str) {
    let alert = gtk4::AlertDialog::builder()
        .message("Sparkamp")
        .detail(msg)
        .modal(true)
        .build();
    alert.show(parent);
}

/// Modal listing files that could not be read (and were not queued).
pub(super) fn show_unreadable_dialog(win: &gtk4::Window, body: &str) {
    let dlg = gtk4::AlertDialog::builder()
        .message("Some files could not be read")
        .detail(gtk_safe(body))
        .modal(true)
        .build();
    dlg.show(Some(win));
}

/// The one Send-to ▸ Disc Drive path: metadata is supplied by the caller
/// (SQLite lookups must happen before any spawn), unknown durations are
/// probed off-thread, readable files queue onto the drive's burn list,
/// unreadable ones are skipped and listed, and an open burn panel is
/// live-refreshed. `status` receives interim ("Reading files…") and final
/// (AddOutcome::status_message) text — every view routes it to its quiet
/// status label (G3: no success modals anywhere).
pub(super) fn queue_paths_to_drive(
    drive_id: String,
    drive_label: String,
    paths: Vec<std::path::PathBuf>,
    metas: std::collections::HashMap<std::path::PathBuf, (String, Option<u32>, u64)>,
    burn_queues: std::rc::Rc<std::cell::RefCell<crate::disc::burnlist::BurnQueues>>,
    burn_refresh_holder: std::rc::Rc<std::cell::RefCell<Option<std::rc::Rc<dyn Fn()>>>>,
    status: std::rc::Rc<dyn Fn(String)>,
    win_wk: glib::WeakRef<gtk4::Window>,
) {
    if paths.is_empty() {
        status("Select tracks first".to_string()); // G4
        return;
    }
    status("Reading files…".to_string());
    glib::spawn_future_local(async move {
        let probe_metas: Vec<(std::path::PathBuf, Option<u32>)> = paths
            .iter()
            .map(|p| (p.clone(), metas.get(p).and_then(|m| m.1)))
            .collect();
        let probed: Vec<(std::path::PathBuf, Option<u32>)> =
            gio::spawn_blocking(move || {
                probe_metas
                    .into_iter()
                    .map(|(p, known)| {
                        let secs = known.or_else(|| {
                            crate::duration_probe::probe_duration_full(&p)
                                .map(|d| d.as_secs() as u32)
                        });
                        (p, secs)
                    })
                    .collect()
            })
            .await
            .unwrap_or_default();
        let out;
        let total;
        {
            let mut queues = burn_queues.borrow_mut();
            let list = queues.queue(&drive_id);
            out = crate::disc::burnlist::add_files(
                list,
                &paths,
                |p| metas.get(p).cloned().unwrap_or_else(|| {
                    (p.display().to_string(), None, 0)
                }),
                |p| probed.iter().find(|(pp, _)| pp == p).and_then(|(_, s)| *s),
            );
            total = list.len();
        } // queues borrow drops before any UI call
        if let Some(refresh) = burn_refresh_holder.borrow().as_ref() {
            refresh();
        }
        status(out.status_message(&drive_label, total));
        if let (Some(body), Some(win)) = (out.failed_message(), win_wk.upgrade()) {
            show_unreadable_dialog(&win, &body);
        }
    });
}

/// Embedded app logo PNG bytes (compiled into the binary).
/// Replace `square logo.png` in the project root with the Sparkamp logo asset.
static LOGO_BYTES: &[u8] = include_bytes!("../../../square logo.png");

/// Load the app logo as a pixbuf scaled to `size × size` pixels.
/// Returns `None` if the PNG fails to decode (handled gracefully so the
/// rest of the UI still starts up even if the asset is missing).
fn load_logo_pixbuf(size: i32) -> Option<gdk_pixbuf::Pixbuf> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    loader.write(LOGO_BYTES).ok()?;
    loader.close().ok()?;
    let pb = loader.pixbuf()?;
    pb.scale_simple(size, size, gdk_pixbuf::InterpType::Bilinear)
}

/// Set up a 100ms polling timer that drains the three scan channels and updates
/// the playlist UI.  Shared by "Add Folder" and "Add Files" so both use identical
/// behaviour.
///
/// `scan_start` is the index into `playlist.tracks` where this scan's tracks begin.
/// It is captured at the moment the user confirms the dialog, before any tracks are
/// added, so that `playlist.tracks[scan_start + scan_index]` always addresses the
/// right track during the metadata phase.
///
/// ## Poller phases
/// 1. **Fast phase** – drain up to 100 fast tracks per tick, rebuild once per batch.
/// 2. **Transition** – when the first metadata message arrives, all fast tracks are
///    guaranteed to have been sent (the background thread completes Phase 1 before
///    starting Phase 2).  Drain any remaining fast tracks, rebuild, spawn duration
///    probes for all newly-added tracks.
/// 3. **Metadata phase** – patch `playlist.tracks[scan_start + idx]` in O(1);
///    rebuild every 5 ticks (~500 ms) so tags fill in gradually.
/// 4. **Done** – drain any remaining metadata, final rebuild, clear scan state.
fn start_playlist_scan_poller(
    state: std::rc::Rc<RefCell<AppState>>,
    status: Label,
    rebuild: std::rc::Rc<dyn Fn()>,
    cancel_btn: Button,
    probe_tx: std::sync::mpsc::Sender<(std::path::PathBuf, std::time::Duration)>,
    broken_tx: std::sync::mpsc::Sender<std::path::PathBuf>,
    patch_row: std::rc::Rc<dyn Fn(usize)>,
    // Called when Phase 2 updates the currently playing track's metadata so the
    // marquee immediately reflects the new "Artist - Title" display name.
    set_track: std::rc::Rc<dyn Fn(&str)>,
    fast_rx: std::sync::mpsc::Receiver<crate::model::Track>,
    meta_rx: std::sync::mpsc::Receiver<(usize, String, String, String, String)>,
    done_rx: std::sync::mpsc::Receiver<usize>,
    phase1_done_rx: std::sync::mpsc::Receiver<usize>,
    scan_start: usize,
) {
    use gtk4::prelude::*;
    use std::cell::Cell;

    // How many fast tracks this scan has added to state.playlist so far.
    let fast_added = Cell::new(0usize);
    // True once the scan thread has confirmed it finished sending all Phase 1 tracks.
    // We wait for this signal before treating an empty fast_rx as "exhausted" —
    // without it we'd give up on Phase 1 as soon as the channel is momentarily
    // empty (e.g. while the scan thread is still walking the directory).
    let phase1_signal_received = Cell::new(false);
    // True once fast_rx is empty AND phase1_signal_received — all fast tracks are
    // now in state.playlist and Phase 2 / probe spawning can proceed.
    let fast_exhausted = Cell::new(false);
    // True once duration probes have been spawned for the fast tracks.
    let probes_spawned = Cell::new(false);
    // Set when done_rx fires; we keep polling until meta_rx is also empty.
    let completion_pending = Cell::new(false);
    // True once we have done the one intermediate rebuild that shows initial filenames.
    let phase1_rebuilt = Cell::new(false);

    // Phase 1 and Phase 2 update only the in-memory model during the scan.
    // The TreeView is rebuilt once after Phase 1 (first_display) and again at
    // FINALISING.  Because TreeView virtualizes rows, a full rebuild() is O(n)
    // in data and O(visible_rows) in paint cost — no row cap needed.

    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        // ── Phase 1: add tracks to the in-memory model ───────────────────
        // We update the model here and let the TreeView render whatever is
        // visible on demand — no O(n²) layout penalty from per-row widgets.

        // Check whether the scan thread has finished sending all Phase 1 tracks.
        // We must receive this signal before treating an empty fast_rx as truly
        // exhausted — without it we would give up on tick 1 when the channel is
        // momentarily empty while the scan thread is still walking the directory.
        if !phase1_signal_received.get() && phase1_done_rx.try_recv().is_ok() {
            phase1_signal_received.set(true);
        }

        let p1_before = fast_added.get();
        if !fast_exhausted.get() {
            // Drain all available fast tracks with no per-tick cap.  The scan
            // thread produces them almost instantly (filesystem stat + canonicalize
            // only), so all tracks usually land in the channel within the first
            // 100 ms and are consumed in a single tick.
            loop {
                match fast_rx.try_recv() {
                    Ok(track) => {
                        state.borrow_mut().playlist.add(track);
                        fast_added.set(fast_added.get() + 1);
                    }
                    Err(_) => {
                        // Channel temporarily empty.  Only mark Phase 1 exhausted if
                        // the scan thread has confirmed it sent everything — otherwise
                        // the directory walk may still be in progress and more tracks
                        // will arrive on a future tick.
                        if phase1_signal_received.get() {
                            fast_exhausted.set(true);
                        }
                        break;
                    }
                }
            }
            if fast_added.get() > p1_before {
                status.set_text(&format!("Adding {}…", fast_added.get()));
            }
        }
        // Rebuild to show all Phase 1 filenames once the channel is drained.
        // Phase 2 starts immediately after and updates rows in place via
        // patch_row(), so the user sees names replace filenames live.
        if !phase1_rebuilt.get() && fast_exhausted.get() {
            phase1_rebuilt.set(true);
            rebuild();
        }

        // Once all fast tracks are in, spawn duration probes.
        if fast_exhausted.get() && !probes_spawned.get() {
            probes_spawned.set(true);
            let paths = state.borrow().uncached_paths_from(scan_start);
            if !paths.is_empty() {
                duration_probe::spawn_probes(paths, probe_tx.clone(), broken_tx.clone());
            }
            let total = fast_added.get();
            if total > 0 {
                status.set_text(&format!("Reading tags… 0/{}", total));
            }
        }

        // ── Phase 2: apply metadata and update individual rows ───────────
        // patch_row is O(1) per call: it finds the store iter by position
        // and updates that row's text in place, so live updates are cheap.
        let mut meta_drained = 0usize;
        while meta_drained < 200 {
            let Ok((idx, title, artist, album_artist, album)) = meta_rx.try_recv() else {
                break;
            };
            let playlist_idx = scan_start + idx;
            let is_current = {
                let mut s = state.borrow_mut();
                if let Some(track) = s.playlist.tracks.get_mut(playlist_idx) {
                    track.title = title;
                    track.artist = artist;
                    track.album_artist = album_artist;
                    track.album = album;
                }
                if let Some(ref mut scan) = s.playlist_scan {
                    scan.current += 1;
                }
                s.playlist.current_index == playlist_idx
            };
            // Update just this row in the ListView store; O(1), no full rebuild needed.
            patch_row(playlist_idx);
            // If Phase 2 just filled in metadata for the currently playing track,
            // refresh the marquee so it shows "Artist - Title" instead of the
            // filename that was used as a placeholder during Phase 1.
            if is_current {
                let display = state
                    .borrow()
                    .playlist
                    .tracks
                    .get(playlist_idx)
                    .map(|t| t.display_name())
                    .unwrap_or_default();
                if !display.is_empty() {
                    set_track(&display);
                }
            }
            meta_drained += 1;
        }
        // Update the status label with metadata progress.
        if meta_drained > 0 {
            let s = state.borrow();
            let current = s.playlist_scan.as_ref().map(|sc| sc.current).unwrap_or(0);
            let total = fast_added.get();
            drop(s);
            status.set_text(&format!("Reading tags… {}/{}", current, total));
        }

        // ── Completion ────────────────────────────────────────────────────
        if !completion_pending.get() && done_rx.try_recv().is_ok() {
            completion_pending.set(true);
            // Edge case: folder had no files or all failed Phase 1.
            if !probes_spawned.get() {
                probes_spawned.set(true);
                let paths = state.borrow().uncached_paths_from(scan_start);
                if !paths.is_empty() {
                    duration_probe::spawn_probes(paths, probe_tx.clone(), broken_tx.clone());
                }
            }
        }

        // Finalise when done_rx has fired, all fast tracks are received, and
        // meta_rx is drained for this tick.
        if completion_pending.get() && fast_exhausted.get() && meta_drained == 0 {
            let added = fast_added.get();
            {
                let mut s = state.borrow_mut();
                s.playlist_scan = None;
                s.pending_bg_ops.set(s.pending_bg_ops.get() - 1);
            }
            status.set_text(&format!(
                "Added {} track{}",
                added,
                if added == 1 { "" } else { "s" }
            ));
            // Apply any durations that are already in the on-disk cache for the
            // newly-added tracks, so the final rebuild can show them immediately
            // without waiting for background probes to return.
            state.borrow_mut().apply_cached_durations();
            // TreeView rebuild() is O(n) in data and O(visible_rows) in paint —
            // no row cap needed; all tracks are inserted and rendered efficiently.
            rebuild();
            cancel_btn.set_visible(false);
            return ControlFlow::Break;
        }

        ControlFlow::Continue
    });
}

/// Determine the default initial folder for the playlist Save dialog.
/// Mirrors `SparkampModel.mlDefaultSaveAsDir()` on macOS:
///
/// 1. First watched ML folder if one exists on disk.
/// 2. The current user's `~/Music` folder.
/// 3. The home directory as a last-resort fallback.
///
/// Avoids defaulting to Sparkamp's managed `~/.config/sparkamp/playlists/`
/// directory — saving there has the side effect of registering that
/// internal dir as a watched folder via `add_playlist_file`.
fn default_playlist_save_dir(
    state: &std::rc::Rc<std::cell::RefCell<AppState>>,
) -> std::path::PathBuf {
    if let Some(lib) = state.borrow().media_lib.as_ref() {
        if let Ok(folders) = lib.list_folders() {
            if let Some((_, p)) = folders.first() {
                let pb = std::path::PathBuf::from(p);
                if pb.exists() { return pb; }
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        let music = home.join("Music");
        if music.exists() { return music; }
        return home;
    }
    std::path::PathBuf::from("/")
}

/// Run the native Save dialog for a playlist file (`.m3u8`).  Initial
/// name is `initial_stem.m3u8`, initial folder is the first watched ML
/// folder or `~/Music`.  On accept, calls `on_accept` with the chosen
/// absolute path (extension is forced to `.m3u8` if the user didn't add
/// one).  Single helper used by every playlist-creation flow so all
/// paths share the same defaults.
fn run_playlist_save_dialog<W, F>(
    state: std::rc::Rc<std::cell::RefCell<AppState>>,
    win: W,
    initial_stem: &str,
    on_accept: F,
) where
    W: gtk4::prelude::IsA<gtk4::Window>,
    F: 'static + FnOnce(std::path::PathBuf, gtk4::Window),
{
    let ext = state
        .borrow()
        .config
        .media_library
        .playlist_format
        .extension();
    let dialog = gtk4::FileDialog::new();
    dialog.set_title("Save Playlist As");
    dialog.set_initial_name(Some(&format!("{initial_stem}.{ext}")));
    let initial_folder = default_playlist_save_dir(&state);
    if initial_folder.exists() {
        dialog.set_initial_folder(Some(&gio::File::for_path(&initial_folder)));
    }
    let win_for_cb: gtk4::Window = win.clone().upcast();
    let ext = ext.to_string();
    dialog.save(Some(&win), gio::Cancellable::NONE, move |res| {
        let Ok(file) = res else { return };
        let Some(mut path) = file.path() else { return };
        if path.extension().is_none() {
            path.set_extension(&ext);
        }
        on_accept(path, win_for_cb);
    });
}

thread_local! {
    /// Editor-refresh callback registered by the ML window when it opens.
    /// Any view that appends to a saved playlist (active-playlist menu,
    /// ML files menu, drag/drop onto sidebar) invokes this with the
    /// target playlist id so the open editor reloads when its current
    /// playlist is the one being modified.  No-op when no ML window is
    /// open or the hook hasn't been registered yet.
    static EDITOR_REFRESH_HOOK: RefCell<Option<Rc<dyn Fn(i64)>>> =
        const { RefCell::new(None) };

    /// Refresh the editor's currently-open playlist, regardless of which
    /// pid changed.  Fired after a track is recorded as played so the
    /// editor reflects updated last_played / play_count + the unread
    /// glyph clears alongside the files view's own refresh.
    static EDITOR_CURRENT_REFRESH_HOOK: RefCell<Option<Rc<dyn Fn()>>> =
        const { RefCell::new(None) };

    /// Re-sync the ML window's playlist navigation (sidebar sub-rows +
    /// manage list) with the playlists table.  Fired after a playlist is
    /// created outside the ML window (e.g. "Add to new playlist" in the
    /// active-playlist window) so it appears immediately.  No-op when no
    /// ML window is open.
    static PLAYLIST_NAV_REFRESH_HOOK: RefCell<Option<Rc<dyn Fn()>>> =
        const { RefCell::new(None) };
}

fn notify_playlist_changed(pid: i64) {
    EDITOR_REFRESH_HOOK.with(|h| {
        if let Some(cb) = h.borrow().as_ref() {
            cb(pid);
        }
    });
}

fn notify_editor_refresh() {
    EDITOR_CURRENT_REFRESH_HOOK.with(|h| {
        if let Some(cb) = h.borrow().as_ref() {
            cb();
        }
    });
}

fn notify_playlist_nav_refresh() {
    PLAYLIST_NAV_REFRESH_HOOK.with(|h| {
        if let Some(cb) = h.borrow().as_ref() {
            cb();
        }
    });
}

/// Build an "Add to Playlist" submenu with a leading "New Playlist…" entry
/// (bound to `new_action`, no parameter) followed by one entry per saved
/// playlist (each bound to `append_action(<playlist-id>: i64)`).  Always
/// returns a menu — "New Playlist…" is shown even when no saved playlists
/// exist so the user can seed a fresh playlist from any selection.
fn build_add_to_playlist_submenu(
    state: &std::rc::Rc<std::cell::RefCell<AppState>>,
    new_action: &str,
    append_action: &str,
) -> gio::Menu {
    let submenu = gio::Menu::new();
    let new_item = gio::MenuItem::new(Some("New Playlist…"), Some(new_action));
    submenu.append_item(&new_item);

    let playlists: Vec<(i64, String)> = state.borrow()
        .media_lib.as_ref()
        .and_then(|lib| lib.all_playlists().ok())
        .map(|v| v.into_iter().map(|p| (p.id, p.name)).collect())
        .unwrap_or_default();
    if !playlists.is_empty() {
        // Separator between "New" and the saved-playlist list — matches the
        // macOS frontend's Add-to-Playlist submenu structure.
        let saved_section = gio::Menu::new();
        for (pid, name) in playlists {
            let item = gio::MenuItem::new(Some(&name), None);
            item.set_action_and_target_value(
                Some(append_action),
                Some(&pid.to_variant()),
            );
            saved_section.append_item(&item);
        }
        submenu.append_section(None, &saved_section);
    }
    submenu
}

/// Walk every descendant of `root` looking for cell labels tagged with a
/// `pos:<N>` widget name (set by the editor column binder).  Returns the
/// canonical play-order index plus the label's vertical bounds (top + height)
/// relative to `root`.  Used by the editor's drop target to resolve a drop
/// coordinate that lands between two rows — the picked widget at that y is
/// the inner ListView, not a cell, so a coordinate-to-row scan is needed.
fn editor_cell_positions(root: &gtk4::Widget) -> Vec<(usize, f32, f32)> {
    use gtk4::prelude::*;
    let mut out: Vec<(usize, f32, f32)> = Vec::new();
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    fn walk(
        w: &gtk4::Widget,
        root: &gtk4::Widget,
        out: &mut Vec<(usize, f32, f32)>,
        seen: &mut std::collections::HashSet<usize>,
    ) {
        let name = w.widget_name().to_string();
        if let Some(rest) = name.strip_prefix("pos:") {
            if let Ok(canonical) = rest.parse::<usize>() {
                if seen.insert(canonical) {
                    if let Some(b) = w.compute_bounds(root) {
                        out.push((canonical, b.y(), b.height()));
                    }
                }
            }
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            walk(&c, root, out, seen);
            child = c.next_sibling();
        }
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        walk(&c, root, &mut out, &mut seen);
        child = c.next_sibling();
    }
    out
}

/// Show a modal AlertDialog reporting a playlist-save failure.
/// Caller-side error reporting for [`run_playlist_save_dialog`] callbacks.
fn show_playlist_save_error(parent: &gtk4::Window, target: &std::path::Path, err: &anyhow::Error) {
    let dialog = gtk4::AlertDialog::builder()
        .message("Couldn't save playlist")
        .detail(format!(
            "Failed to write {}\n\n{}",
            target.display(),
            err
        ))
        .modal(true)
        .build();
    dialog.show(Some(parent));
}

/// What the "Send to" menu shows, as data — pure so the 0/1/N visibility
/// rules are unit-testable without GTK.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum SendEntry {
    ActivePlaylist,
    SavedPlaylist,
    /// One drive attached: direct item (id, label), no submenu.
    DriveDirect(String, String),
    /// Multiple drives: submenu, one item per (id, label).
    DriveMenu(Vec<(String, String)>),
    DeviceDirect(String, String),
    DeviceMenu(Vec<(String, String)>),
}

pub(super) fn send_to_spec(
    drives: &[(String, String)],
    devices: &[(String, String)],
) -> Vec<SendEntry> {
    let mut out = vec![SendEntry::ActivePlaylist, SendEntry::SavedPlaylist];
    match drives {
        [] => {}
        [(id, label)] => out.push(SendEntry::DriveDirect(id.clone(), label.clone())),
        many => out.push(SendEntry::DriveMenu(many.to_vec())),
    }
    match devices {
        [] => {}
        [(id, label)] => out.push(SendEntry::DeviceDirect(id.clone(), label.clone())),
        many => out.push(SendEntry::DeviceMenu(many.to_vec())),
    }
    out
}

/// Action names for one consumer of the Send-to menu; each consumer
/// registers its own action group and passes prefixed names here.
pub(super) struct SendToActions<'a> {
    pub active: &'a str,         // e.g. "ml.send-active" (no target)
    pub new_playlist: &'a str,   // e.g. "ml.add-to-new" (no target)
    pub saved_playlist: &'a str, // e.g. "ml.add-to-saved" (i64 playlist id)
    pub drive: &'a str,          // e.g. "ml.send-drive" (String drive id)
    pub device: &'a str,         // e.g. "ml.send-device" (String device id)
    pub drives: Vec<(String, String)>,
    pub devices: Vec<(String, String)>,
}

/// Build the full "Send to" menu: Active Playlist / Saved Playlist ▸ /
/// Disc Drive [▸] / Removable Device [▸]. Drive + device lists must come
/// from the cached poll state — never probe from a menu handler.
// AppState (state.rs) is private to the `window` module; pub(super) here
// (needed so a future sibling submodule can call this) makes the fn more
// visible than that parameter type, which rustc flags as private_interfaces.
// Widening AppState's own visibility is out of scope for this file.
#[allow(private_interfaces)]
pub(super) fn build_send_to_menu(
    state: &std::rc::Rc<std::cell::RefCell<AppState>>,
    actions: &SendToActions<'_>,
) -> gio::Menu {
    let menu = gio::Menu::new();
    for entry in send_to_spec(&actions.drives, &actions.devices) {
        match entry {
            SendEntry::ActivePlaylist => {
                if !actions.active.is_empty() {
                    menu.append_item(&gio::MenuItem::new(
                        Some("Active Playlist"),
                        Some(actions.active),
                    ));
                }
            }
            SendEntry::SavedPlaylist => {
                let sub = build_add_to_playlist_submenu(
                    state,
                    actions.new_playlist,
                    actions.saved_playlist,
                );
                menu.append_submenu(Some("Saved Playlist"), &sub);
            }
            SendEntry::DriveDirect(id, _label) => {
                let item = gio::MenuItem::new(Some("Disc Drive"), None);
                item.set_action_and_target_value(
                    Some(actions.drive),
                    Some(&id.to_variant()),
                );
                menu.append_item(&item);
            }
            SendEntry::DriveMenu(drives) => {
                let sub = gio::Menu::new();
                for (id, label) in drives {
                    let item = gio::MenuItem::new(Some(&label), None);
                    item.set_action_and_target_value(
                        Some(actions.drive),
                        Some(&id.to_variant()),
                    );
                    sub.append_item(&item);
                }
                menu.append_submenu(Some("Disc Drive"), &sub);
            }
            SendEntry::DeviceDirect(id, _label) => {
                let item = gio::MenuItem::new(Some("Removable Device"), None);
                item.set_action_and_target_value(
                    Some(actions.device),
                    Some(&id.to_variant()),
                );
                menu.append_item(&item);
            }
            SendEntry::DeviceMenu(devices) => {
                let sub = gio::Menu::new();
                for (id, label) in devices {
                    let item = gio::MenuItem::new(Some(&label), None);
                    item.set_action_and_target_value(
                        Some(actions.device),
                        Some(&id.to_variant()),
                    );
                    sub.append_item(&item);
                }
                menu.append_submenu(Some("Removable Device"), &sub);
            }
        }
    }
    menu
}

// Named `send_to_tests` (not `tests`) because util.rs is `include!`d flat
// into the `window` module alongside tests.rs, which already defines a
// `mod tests` in that same namespace — a second `mod tests` here would
// collide.
#[cfg(test)]
mod send_to_tests {
    use super::*;

    #[test]
    fn send_to_spec_visibility_matrix() {
        let d1 = vec![("sr0".to_string(), "Drive A".to_string())];
        let d2 = vec![
            ("sr0".to_string(), "Drive A".to_string()),
            ("sr1".to_string(), "Drive B".to_string()),
        ];
        // 0 drives, 0 devices: playlist entries only.
        let spec = send_to_spec(&[], &[]);
        assert_eq!(spec, vec![SendEntry::ActivePlaylist, SendEntry::SavedPlaylist]);
        // 1 drive: direct item, no submenu.
        let spec = send_to_spec(&d1, &[]);
        assert!(spec.contains(&SendEntry::DriveDirect("sr0".into(), "Drive A".into())));
        // 2 drives: submenu with both.
        let spec = send_to_spec(&d2, &[]);
        assert!(spec.contains(&SendEntry::DriveMenu(d2.clone())));
        // devices mirror the same rule.
        let dev = vec![("usb1".to_string(), "Stick".to_string())];
        let spec = send_to_spec(&[], &dev);
        assert!(spec.contains(&SendEntry::DeviceDirect("usb1".into(), "Stick".into())));
    }
}

