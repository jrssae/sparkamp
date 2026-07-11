/// Filesystems Sparkamp can't reliably read/write yet — shown with a warning.
fn device_fs_unsupported(fs_type: &str) -> bool {
    crate::devices::plan::device_fs_unsupported(fs_type)
}

/// Whether a udisks volume is optical media (a mounted data CD/DVD). These
/// belong to the Disc Drives group, not the removable-Devices list, so the
/// device poll filters them out. `iso9660`/`udf` are the optical data
/// filesystems; audio CDs have no filesystem and never reach the device list.
fn is_optical_fs(fs_type: &str) -> bool {
    matches!(fs_type.to_ascii_lowercase().as_str(), "iso9660" | "udf")
}

/// Case-insensitive substring match of a per-view search query against a
/// track's visible text fields — the in-memory counterpart of the Files
/// view's DB-backed search, used by the playlist-editor and device views.
/// `q` must already be lowercased; an empty query matches everything.
fn lib_track_matches_query(t: &crate::media_library::LibTrack, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    let has = |s: &Option<String>| s.as_deref().map(|v| v.to_lowercase().contains(q)).unwrap_or(false);
    has(&t.title)
        || has(&t.artist)
        || has(&t.album)
        || has(&t.genre)
        || t.filename.to_lowercase().contains(q)
}

/// A search entry + ✕ clear button row, styled like the Files view's search
/// bar. Returns `(row, entry)`; the caller wires `connect_changed`.
fn make_view_search_row(placeholder: &str) -> (GtkBox, Entry) {
    let entry = Entry::new();
    entry.set_placeholder_text(Some(placeholder));
    entry.set_hexpand(true);
    let clear = Button::with_label("✕");
    clear.add_css_class("pl-btn");
    {
        let e = entry.clone();
        clear.connect_clicked(move |_| e.set_text(""));
    }
    let row = GtkBox::new(Orientation::Horizontal, 4);
    row.set_margin_top(4);
    row.set_margin_start(4);
    row.set_margin_end(4);
    row.append(&entry);
    row.append(&clear);
    (row, entry)
}

/// Leading status glyphs for a device label: ⚠ for an unsupported filesystem,
/// 🔒 for read-only (matching the read-only file convention).
fn device_glyph_prefix(read_only: bool, fs_type: &str) -> String {
    let mut p = String::new();
    if device_fs_unsupported(fs_type) {
        p.push_str("⚠ ");
    }
    if read_only {
        p.push_str("🔒 ");
    }
    p
}

/// Themed icon name for a device card. Generic removable-media icon for now;
/// the MTP backend (Android phones) will map to a phone icon when added.
fn device_icon_name(_dev: &crate::devices::Device) -> &'static str {
    "drive-removable-media"
}

/// Apply a copy's progress to an overview card's bar. `Some((done, total))`
/// shows the bar with an `x/y` label and fraction; `None` makes it transparent
/// (idle) while still reserving its space, so the card never changes height.
fn apply_card_progress(bar: &gtk4::ProgressBar, state: Option<(usize, usize)>) {
    match state {
        Some((done, total)) => {
            bar.set_opacity(1.0);
            bar.set_text(Some(&format!("{done}/{total}")));
            bar.set_fraction(done as f64 / total.max(1) as f64);
        }
        None => bar.set_opacity(0.0),
    }
}

/// Color a capacity LevelBar by fullness: normal < 75%, `cap-warn` ≥ 75%,
/// `cap-full` ≥ 90%. The classes are styled in `skin.rs`.
fn set_levelbar_fullness(bar: &gtk4::LevelBar, used: f64) {
    bar.remove_css_class("cap-ok");
    bar.remove_css_class("cap-warn");
    bar.remove_css_class("cap-full");
    // Thresholds are on *free* space: red under 5% free, amber under 15% free,
    // accent/blue otherwise. Exactly one class is set so every capacity bar
    // reads the same color across the sidebar, overview, and detail views.
    let free = 1.0 - used;
    if free < 0.05 {
        bar.add_css_class("cap-full");
    } else if free < 0.15 {
        bar.add_css_class("cap-warn");
    } else {
        bar.add_css_class("cap-ok");
    }
}

/// Toggle a button into a "working" state: a running spinner replaces its label
/// and it goes insensitive, restored to `idle_label` when done. Used so the
/// Sync button shows activity during the (sometimes slow over MTP) device
/// communication before the sync dialog appears.
fn set_button_busy(btn: &Button, busy: bool, idle_label: &str) {
    if busy {
        let spinner = gtk4::Spinner::new();
        spinner.start();
        btn.set_child(Some(&spinner));
        btn.set_sensitive(false);
    } else {
        btn.set_label(idle_label);
        btn.set_sensitive(true);
    }
}

/// Resolve an MTP device's writable **storage root** under its gvfs FUSE mount.
///
/// The mtp:// mount root's children are storage volumes (e.g. "Internal shared
/// storage", "SD card"), and Android rejects files written at the device root —
/// they must live inside a storage. So the device's `mount_path` is set to a
/// storage dir, keeping the flat `Music/<file>` layout valid. Prefers a storage
/// that already has a `Music` folder, then one whose name looks "internal", else
/// the first. Cached per device URI so the poll doesn't `read_dir` every tick;
/// the cache self-heals if the path goes stale (replug).
fn mtp_storage_root(uri: &str, fuse_root: &std::path::Path) -> std::path::PathBuf {
    // Thread-safe cache: this is called from a worker thread (off the UI thread)
    // so the FUSE read_dir never blocks the main loop.
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::path::PathBuf>>,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Some(p) = cache.lock().unwrap().get(uri).cloned() {
        return p;
    }
    let mut chosen = fuse_root.to_path_buf();
    if let Ok(entries) = std::fs::read_dir(fuse_root) {
        let dirs: Vec<std::path::PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .collect();
        chosen = dirs
            .iter()
            .find(|d| d.join("Music").exists())
            .or_else(|| {
                dirs.iter().find(|d| {
                    d.file_name()
                        .map(|n| n.to_string_lossy().to_lowercase().contains("internal"))
                        .unwrap_or(false)
                })
            })
            .or_else(|| dirs.first())
            .cloned()
            .unwrap_or_else(|| fuse_root.to_path_buf());
    }
    // Only cache a real storage — not the device-root fallback (which happens
    // in charge-only mode), so switching the phone to file mode re-resolves.
    if chosen != fuse_root {
        cache.lock().unwrap().insert(uri.to_string(), chosen.clone());
    }
    chosen
}

/// Detect MTP devices (Android phones in File-transfer mode) via gio's
/// `VolumeMonitor`. These are surfaced by gvfs as `mtp://` mounts with a FUSE
/// path under `/run/user/<uid>/gvfs/`. Produces core [`Device`] structs tagged
/// `DeviceBackend::Mtp`; the udisks2 detection path never sees them.
///
/// Must run on the main thread (VolumeMonitor is a GLib main-context object).
/// A device without a local FUSE path is skipped — `PosixIo` can't browse it
/// until the gio IO backend (later phase) lands.
struct MtpRaw {
    uri: String,
    fuse_root: std::path::PathBuf,
    label: String,
    id: String,
    ejectable: bool,
}

/// Enumerate MTP mount *metadata* via gio's VolumeMonitor. Cheap, no filesystem
/// IO (so safe to run on the main thread): only cached GLib mount/volume props
/// and URI→path mapping. The FUSE `read_dir` to find the storage root is done
/// later, off-thread, by [`mtp_raw_to_device`].
fn enumerate_mtp_raw() -> Vec<MtpRaw> {
    let monitor = gio::VolumeMonitor::get();
    // gvfs can expose one MTP device as several mounts sharing the same root URI
    // (a friendly "Pixel 8" plus a generic "mtp"). Dedup by URI, best label wins.
    let mut by_uri: std::collections::HashMap<String, MtpRaw> = std::collections::HashMap::new();
    for mount in monitor.mounts() {
        let root = mount.root();
        let uri = root.uri().to_string();
        if !uri.starts_with("mtp://") && !uri.starts_with("gphoto2://") {
            continue;
        }
        let Some(fuse_root) = root.path() else {
            continue;
        };
        let mount_name = mount.name().to_string();
        let vol_name = mount.volume().map(|v| v.name().to_string()).unwrap_or_default();
        let label = if !mount_name.is_empty() && mount_name != "mtp" {
            mount_name
        } else if !vol_name.is_empty() {
            vol_name
        } else {
            "MTP device".to_string()
        };
        let id = mount
            .uuid()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| uri.clone());
        let raw = MtpRaw {
            uri: uri.clone(),
            fuse_root,
            label,
            id,
            ejectable: mount.can_eject() || mount.can_unmount(),
        };
        match by_uri.get(&uri) {
            Some(existing) if existing.label != "MTP device" => {}
            _ => {
                by_uri.insert(uri, raw);
            }
        }
    }
    by_uri.into_values().collect()
}

/// Resolve one [`MtpRaw`] into a [`Device`]. Runs on a worker thread because it
/// does FUSE `read_dir`s (via [`mtp_storage_root`]) to point `mount_path` at the
/// device's writable storage root.
///
/// Returns `None` for a **dead** mount — a gvfs entry left behind in
/// VolumeMonitor after the phone was unplugged, whose FUSE root can no longer be
/// read. Dropping it keeps a phantom "MTP device" out of the sidebar when
/// nothing is actually connected.
///
/// Returns a device with `fs_visible == false` when the phone is connected but
/// exposes no readable storage volume (file transfer not authorized, or the
/// storage hasn't appeared yet) — the detail view then shows a reconnect banner
/// instead of empty lists.
/// Set true once the main window starts closing. Worker-thread device code
/// checks it before starting any blocking gvfs/MTP FUSE work (directory reads,
/// capacity queries, tag scans): such a read can block in uninterruptible IO on
/// a slow/wedged device, pinning the thread and delaying process exit and
/// Ctrl-C. Not *starting* the read avoids that. (An already in-flight read can't
/// be cancelled — that case is inherent to FUSE.)
static DEVICE_IO_SHUTDOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn device_io_shutting_down() -> bool {
    DEVICE_IO_SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Cached MTP device metadata, filled by the one-time FUSE reads in
/// [`mtp_raw_to_device`] the first time a device URI is seen. Steady-state
/// polling reuses it and NEVER touches the gvfs mount: issuing a blocking,
/// uncancellable FUSE read every 2 s would, on a slow or post-sync-wedged
/// device, hold the mount busy (blocking eject from Sparkamp and GNOME) and pin
/// a worker thread in uninterruptible IO — delaying process exit and Ctrl-C.
/// Invalidated by [`invalidate_mtp_meta`] after any operation that changes the
/// device, so capacity/visibility refresh then rather than on a timer.
struct MtpMeta {
    storage_root: std::path::PathBuf,
    has_storage: bool,
    total_bytes: u64,
    free_bytes: u64,
}

fn mtp_meta_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, MtpMeta>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, MtpMeta>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Drop a device's cached MTP metadata so the next poll re-reads it once — e.g.
/// after a copy/sync/delete changed its contents, or on eject. No-op for
/// non-MTP backend ids (their URIs are never cached here).
fn invalidate_mtp_meta(uri: &str) {
    mtp_meta_cache().lock().unwrap().remove(uri);
}

fn mtp_device_from_meta(raw: &MtpRaw, m: &MtpMeta) -> crate::devices::Device {
    use crate::devices::DeviceBackend;
    crate::devices::Device {
        id: raw.id.clone(),
        label: raw.label.clone(),
        mount_path: m.storage_root.clone(),
        fs_type: "mtp".to_string(),
        total_bytes: m.total_bytes,
        free_bytes: m.free_bytes,
        read_only: false,
        ejectable: raw.ejectable,
        backend_id: raw.uri.clone(),
        backend: DeviceBackend::Mtp,
        fs_visible: m.has_storage,
    }
}

/// Whether a gvfs URI belongs to an Apple device (iPad/iPhone). gphoto2 URIs
/// for Apple hardware embed the vendor, e.g.
/// `gphoto2://Apple_Inc._iPad_00008020.../`.
fn is_apple_device_uri(uri: &str) -> bool {
    uri.to_lowercase().contains("apple")
}

/// Banner text for a device Sparkamp can't sync to. Apple devices get the
/// iOS-specific guidance; everything else on `gphoto2://` is a phone in
/// photo-transfer (PTP) mode that should be switched to file-transfer/MTP.
fn unsupported_device_banner(uri: &str) -> &'static str {
    if is_apple_device_uri(uri) {
        "⚠ iPad / iPhone detected. iOS doesn't allow third-party music transfer — \
         use Apple Music or Finder to add songs. Sparkamp can't sync to this device."
    } else {
        "⚠ Device is in photo-transfer (PTP) mode. Switch it to File Transfer / MTP \
         mode to sync music, then reconnect."
    }
}

fn mtp_raw_to_device(raw: MtpRaw) -> Option<crate::devices::Device> {
    // gphoto2:// mounts are photo-transfer (PTP) interfaces: read-only, camera
    // roll only. Apple devices and Android-in-photo-mode both land here. They
    // are surfaced so the user sees the device is detected, but tagged
    // Unsupported (NullIo) and never offered as a sync target. Built directly,
    // with no FUSE/capacity reads — there is nothing useful to read.
    if raw.uri.starts_with("gphoto2://") {
        use crate::devices::DeviceBackend;
        return Some(crate::devices::Device {
            id: raw.id.clone(),
            label: raw.label.clone(),
            mount_path: raw.fuse_root.clone(),
            fs_type: if is_apple_device_uri(&raw.uri) { "ios" } else { "ptp" }.to_string(),
            total_bytes: 0,
            free_bytes: 0,
            read_only: true,
            ejectable: raw.ejectable,
            backend_id: raw.uri.clone(),
            backend: DeviceBackend::Unsupported,
            fs_visible: false,
        });
    }
    // Cache hit → no FUSE IO at all. This is the steady-state path on every
    // 2 s poll once a device has been seen, so a slow/wedged mount can never
    // block the poll worker or hold the mount busy in the background.
    if let Some(m) = mtp_meta_cache().lock().unwrap().get(&raw.uri) {
        return Some(mtp_device_from_meta(&raw, m));
    }
    // Don't begin first-detect FUSE reads while shutting down.
    if device_io_shutting_down() {
        return None;
    }
    // Cache miss (first sight of this URI): do the one-time FUSE reads.
    // Accessibility gate: an unplugged phone's stale mount still lists in
    // VolumeMonitor, but its FUSE root errors on read — treat as "not
    // connected" and drop it.
    let Ok(entries) = std::fs::read_dir(&raw.fuse_root) else {
        return None;
    };
    // At least one storage-volume directory present? (Internal storage / SD
    // card.) Its absence is the "connected but no visible filesystem" case.
    let has_storage = entries
        .flatten()
        .any(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false));
    let storage_root = mtp_storage_root(&raw.uri, &raw.fuse_root);
    // Capacity via gio (gvfs FUSE rarely reports statvfs). Safe here — this runs
    // on a worker thread, so the blocking query never freezes the UI.
    let (total_bytes, free_bytes) = gio::File::for_uri(&raw.uri)
        .query_filesystem_info("filesystem::size,filesystem::free", gio::Cancellable::NONE)
        .map(|info| {
            (
                info.attribute_uint64("filesystem::size"),
                info.attribute_uint64("filesystem::free"),
            )
        })
        .unwrap_or((0, 0));
    let meta = MtpMeta {
        storage_root,
        has_storage,
        total_bytes,
        free_bytes,
    };
    let dev = mtp_device_from_meta(&raw, &meta);
    mtp_meta_cache().lock().unwrap().insert(raw.uri.clone(), meta);
    Some(dev)
}

/// "N songs · M playlists" with singular/plural agreement.
fn counts_text(songs: usize, playlists: usize) -> String {
    format!(
        "{songs} song{} · {playlists} playlist{}",
        if songs == 1 { "" } else { "s" },
        if playlists == 1 { "" } else { "s" },
    )
}

/// Tooltip shown on the device row / detail for an unsupported filesystem.
const UNSUPPORTED_FS_TOOLTIP: &str =
    "Unsupported filesystem (NTFS/exFAT) — Sparkamp can't reliably read or write this device yet.";

/// Device identity for sync pairs: the volume UUID, or a marker id written now
/// (the first time a file is paired to this device).
fn device_sync_id(dev: &crate::devices::Device) -> String {
    crate::devices::plan::device_sync_id(dev)
}

/// The DB half of [`device_plan_one`]: the recorded sync-pair device relpath for
/// `src` on this device, if any. Frontend shim over
/// [`crate::devices::plan::recorded_relpath`] that pulls the open library.
fn device_recorded_relpath(
    state: &Rc<RefCell<AppState>>,
    device_id: &str,
    src: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let s = state.borrow();
    let lib = s.media_lib.as_ref()?;
    crate::devices::plan::recorded_relpath(lib, device_id, src)
}

/// The filesystem half of [`device_plan_one`]: given the recorded relpath (from
/// [`device_recorded_relpath`]), decide the final relpath and whether the file
/// is already present, using `metadata`/`exists` checks on the device. This is
/// the part that can be slow over a gvfs/MTP FUSE mount, so callers run it on a
/// worker thread.
fn device_plan_fs(
    mount: &std::path::Path,
    src: &std::path::Path,
    recorded: Option<std::path::PathBuf>,
) -> (std::path::PathBuf, bool) {
    crate::devices::plan::device_plan_fs(mount, src, recorded)
}

/// Decide where `src` goes on the device and whether it's already there.
///
/// Resolution order, all yielding the canonical flat `Music/<filename>` layout:
/// 1. A recorded sync pair whose device file still exists *and* matches the
///    current flat layout → reuse it (so editing metadata never duplicates).
/// 2. An identical file (same name, same size) already at `Music/<filename>` →
///    treat as present, so a lost/mismatched pair can't spawn a `-N` duplicate.
/// 3. A *different* file occupying `Music/<filename>` → `-N` collision suffix.
/// 4. Otherwise the free `Music/<filename>` slot.
///
/// Does filesystem IO; on a slow (MTP) device prefer the split
/// [`device_recorded_relpath`] (main thread) + [`device_plan_fs`] (worker).
fn device_plan_one(
    state: &Rc<RefCell<AppState>>,
    mount: &std::path::Path,
    device_id: &str,
    src: &std::path::Path,
) -> (std::path::PathBuf, bool) {
    device_plan_fs(mount, src, device_recorded_relpath(state, device_id, src))
}

/// Record (or refresh) the sync pair for a just-copied file with its REAL tag
/// baseline, so a later sync sees no change until a tag is actually edited.
fn device_record_pair(
    state: &Rc<RefCell<AppState>>,
    device_id: &str,
    src: &std::path::Path,
    relpath: &std::path::Path,
) {
    if let Some(lib) = state.borrow().media_lib.as_ref() {
        crate::devices::plan::record_pair(lib, device_id, src, relpath);
    }
}

/// Sanitize a playlist name into the bare filename stem used for its `.m3u`/
/// `.m3u8` on a device: strip path-hostile characters and surrounding dots/
/// spaces, falling back to "Playlist" when nothing usable remains.
fn safe_playlist_filename(name: &str) -> String {
    crate::devices::plan::safe_playlist_filename(name)
}

/// If a device playlist file is linked to a library playlist — i.e. some
/// library playlist's safe filename equals the device file's stem — return its
/// `(id, name)`. Device-only playlists (no library match) return `None`.
fn linked_library_playlist(
    state: &Rc<RefCell<AppState>>,
    dev_playlist: &std::path::Path,
) -> Option<(i64, String)> {
    let s = state.borrow();
    let lib = s.media_lib.as_ref()?;
    crate::devices::plan::linked_library_playlist(lib, dev_playlist)
}

/// A validated plan for sending a whole playlist to a device: the files to
/// copy (with their on-device paths), the device identity for sync pairs, and
/// where the `.m3u8` will be written on the device.
struct PlaylistSendPlan {
    srcs: Vec<std::path::PathBuf>,
    device_id: String,
    m3u_path: std::path::PathBuf,
}

/// Validate and build a [`PlaylistSendPlan`] for `playlist_id` on `dev`, or a
/// user-facing error (read-only / unsupported device, empty playlist, no space).
fn prepare_playlist_send(
    state: &Rc<RefCell<AppState>>,
    dev: &crate::devices::Device,
    playlist_id: i64,
    playlist_name: &str,
) -> Result<PlaylistSendPlan, String> {
    if dev.read_only {
        let n = if dev.label.is_empty() { "This device" } else { &dev.label };
        return Err(format!("{n} is read-only — can't copy files to it."));
    }
    if device_fs_unsupported(&dev.fs_type) {
        return Err(format!(
            "{} is an unsupported filesystem — can't write to this device yet.",
            dev.fs_type
        ));
    }
    let tracks = {
        let s = state.borrow();
        s.media_lib
            .as_ref()
            .and_then(|lib| {
                lib.playlist_by_id(playlist_id)
                    .ok()
                    .and_then(|pl| lib.load_playlist_tracks(&pl).ok())
            })
            .unwrap_or_default()
    };
    let srcs: Vec<std::path::PathBuf> = tracks
        .iter()
        .map(|t| std::path::PathBuf::from(&t.path))
        .filter(|p| p.exists())
        .collect();
    if srcs.is_empty() {
        return Err("No playable files in this playlist.".to_string());
    }
    let device_id = device_sync_id(dev);
    // Free-space guard — only when capacity is known (0 = unknown, e.g. MTP).
    // Skipping it avoids a whole pass of slow per-file device checks on devices
    // that can't report free space anyway.
    if dev.free_bytes > 0 {
        let mut need = 0u64;
        for src in &srcs {
            if !device_plan_one(state, &dev.mount_path, &device_id, src).1 {
                need += std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
            }
        }
        if need > dev.free_bytes {
            return Err(format!(
                "Not enough space on the device: need {:.1} GB, {:.1} GB free.",
                need as f64 / 1e9,
                dev.free_bytes as f64 / 1e9
            ));
        }
    }
    let safe = safe_playlist_filename(playlist_name);
    let ext = state
        .borrow()
        .config
        .media_library
        .playlist_format
        .extension();
    let m3u_path = dev.mount_path.join(format!("{safe}.{ext}"));
    Ok(PlaylistSendPlan {
        srcs,
        device_id,
        m3u_path,
    })
}

/// Compute the per-pair sync decisions for a device: for each recorded sync
/// pair, hash the current tags on each side and decide the direction.
fn device_sync_plan(
    lib: &crate::media_library::MediaLibrary,
    dev: &crate::devices::Device,
) -> Vec<(crate::media_library::SyncPair, crate::devices::sync::SyncAction)> {
    crate::devices::plan::device_sync_plan(lib, dev)
}

/// Apply one tag-sync direction to a single pair and refresh its baseline.
/// `to_device` true = library→device, false = device→library. Returns ok.
fn apply_tag_pair(
    state: &Rc<RefCell<AppState>>,
    dev: &crate::devices::Device,
    pair: &crate::media_library::SyncPair,
    to_device: bool,
) -> bool {
    match state.borrow().media_lib.as_ref() {
        Some(lib) => crate::devices::plan::apply_tag_pair(lib, dev, pair, to_device),
        None => false,
    }
}

/// Apply a sync plan: propagate the winning side's tags for the unambiguous
/// directions (conflicts are handled separately by the prompt) and refresh each
/// pair's baseline. Returns `(applied, failed)`.
fn apply_device_sync(
    state: &Rc<RefCell<AppState>>,
    dev: &crate::devices::Device,
    plan: &[(crate::media_library::SyncPair, crate::devices::sync::SyncAction)],
) -> (usize, usize) {
    match state.borrow().media_lib.as_ref() {
        Some(lib) => crate::devices::plan::apply_device_sync(lib, dev, plan),
        None => (0, 0),
    }
}

/// Build the two-way playlist sync plan for a device: for each library playlist
/// that is on the device (or was, per a stored baseline), decide whether to
/// push to the device, pull into the library, or flag a conflict.
fn device_playlist_sync_plan(
    lib: &crate::media_library::MediaLibrary,
    dev: &crate::devices::Device,
    ext: &str,
) -> Vec<PlaylistSyncItem> {
    crate::devices::plan::device_playlist_sync_plan(lib, dev, ext)
}

/// Push a library playlist to the device: copy any missing tracks (flat
/// `Music/<file>`, deduped), rewrite the device `.m3u8`, drop the old device
/// file if the playlist was renamed, and refresh the baseline. Audio files for
/// tracks removed from the playlist stay on the device (Deletion Rule).
/// Returns `(files_copied, ok)`.
fn apply_playlist_push(
    state: &Rc<RefCell<AppState>>,
    dev: &crate::devices::Device,
    item: &PlaylistSyncItem,
) -> (usize, bool) {
    match state.borrow().media_lib.as_ref() {
        Some(lib) => crate::devices::plan::apply_playlist_push(lib, dev, item),
        None => (0, false),
    }
}

/// Prompt the user to resolve playlist conflicts one at a time (both sides
/// changed). Each prompt shows how many entries differ; the user keeps the
/// computer's copy (push), the device's copy (pull), or skips. After the last
/// one, `done` runs (refresh + summary).
/// Prompt the user to resolve per-file tag conflicts one at a time. Each prompt
/// lists the differing fields (computer vs device); the user keeps the computer
/// copy (library→device), the device copy (device→library), or skips. After the
/// last one, `done` runs.
fn prompt_tag_conflicts(
    state: Rc<RefCell<AppState>>,
    dev: crate::devices::Device,
    mut conflicts: Vec<TagConflictItem>,
    win_wk: glib::WeakRef<gtk4::Window>,
    done: Rc<dyn Fn()>,
) {
    let Some(item) = conflicts.pop() else {
        (done)();
        return;
    };
    let mut detail = String::new();
    for d in &item.diffs {
        let comp = if d.computer.is_empty() { "(empty)" } else { &d.computer };
        let dev_v = if d.device.is_empty() { "(empty)" } else { &d.device };
        detail.push_str(&format!("{}:\n   This computer: {comp}\n   On device: {dev_v}\n", d.label));
    }
    let dialog = gtk4::AlertDialog::builder()
        .message(format!("\"{}\" changed on both sides", item.song))
        .detail(detail.trim_end().to_string())
        .buttons(vec![
            "Skip".to_string(),
            "Keep device".to_string(),
            "Keep computer".to_string(),
        ])
        .cancel_button(0)
        .default_button(2)
        .modal(true)
        .build();
    dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |res| {
        match res {
            Ok(2) => {
                apply_tag_pair(&state, &dev, &item.pair, true); // keep computer → library→device
            }
            Ok(1) => {
                apply_tag_pair(&state, &dev, &item.pair, false); // keep device → device→library
            }
            _ => {} // Skip — leave both sides, no baseline update.
        }
        prompt_tag_conflicts(state.clone(), dev.clone(), conflicts, win_wk.clone(), done.clone());
    });
}

/// Build the per-file tag-conflict items from a sync plan: for each pair marked
/// `Conflict`, read both sides' tags and compute the differing fields.
fn build_tag_conflicts(
    dev: &crate::devices::Device,
    plan: &[(crate::media_library::SyncPair, crate::devices::sync::SyncAction)],
) -> Vec<TagConflictItem> {
    crate::devices::plan::build_tag_conflicts(dev, plan)
}

fn prompt_playlist_conflicts(
    state: Rc<RefCell<AppState>>,
    dev: crate::devices::Device,
    mut conflicts: Vec<PlaylistSyncItem>,
    win_wk: glib::WeakRef<gtk4::Window>,
    done: Rc<dyn Fn()>,
) {
    let Some(item) = conflicts.pop() else {
        (done)();
        return;
    };
    let dialog = gtk4::AlertDialog::builder()
        .message(format!("\"{}\" changed on both sides", item.library_name))
        .detail(format!(
            "{} file{} differ between this computer and the device. Which copy do you want to keep?",
            item.differ,
            if item.differ == 1 { "" } else { "s" }
        ))
        .buttons(vec![
            "Skip".to_string(),
            "Keep device".to_string(),
            "Keep computer".to_string(),
        ])
        .cancel_button(0)
        .default_button(2)
        .modal(true)
        .build();
    dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |res| {
        match res {
            Ok(2) => {
                apply_playlist_push(&state, &dev, &item);
            }
            Ok(1) => {
                apply_playlist_pull(&state, &item);
            }
            _ => {} // Skip — leave both sides as-is (no baseline update).
        }
        prompt_playlist_conflicts(state.clone(), dev.clone(), conflicts, win_wk.clone(), done.clone());
    });
}

/// Pull a device playlist into the library: rewrite the library playlist file to
/// mirror the device's order/membership (mapping device filenames back to
/// library tracks by filename), then refresh the baseline. Returns ok.
fn apply_playlist_pull(
    state: &Rc<RefCell<AppState>>,
    item: &PlaylistSyncItem,
) -> bool {
    match state.borrow().media_lib.as_ref() {
        Some(lib) => crate::devices::plan::apply_playlist_pull(lib, item),
        None => false,
    }
}

/// Rewrite a device `.m3u`/`.m3u8`, dropping every track line whose filename
/// (basename of the entry, `/` or `\` separated) is in `remove`. Comment/blank
/// lines are preserved. Returns true if the file changed.
fn device_m3u_remove_basenames(
    path: &std::path::Path,
    remove: &std::collections::HashSet<String>,
) -> bool {
    crate::devices::plan::device_m3u_remove_basenames(path, remove)
}

/// Delete files from a device and remove them from every device playlist that
/// referenced them. `paths` are absolute on-device paths. Returns the number of
/// files that couldn't be deleted.
fn device_delete_files(dev: &crate::devices::Device, paths: &[std::path::PathBuf]) -> usize {
    crate::devices::plan::device_delete_files(dev, paths)
}

