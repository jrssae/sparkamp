//! JSON-over-FFI device API for the macOS frontend.
//!
//! Device structures (a device list, a sync plan with per-pair actions, a
//! conflict's field diffs, playlist-sync items) are deep and variable-length,
//! so each call marshals UTF-8 JSON through `*mut c_char` (freed with
//! [`super::sparkamp_free_string`]) and the Swift side uses `Codable`. The one
//! exception is conflict artwork, returned as raw bytes (freed with
//! `sparkamp_tag_free_artwork`) to avoid base64 bloat.
//!
//! All device *logic* lives in `crate::devices` (`plan`, `sync`, `browse`,
//! `transfer`, `io`, `marker`) and is platform-neutral; this file only drives
//! it. Swift owns volume *enumeration* (DiskArbitration) and eject; the core
//! owns identity, the canonical `Device` shape, and every sync decision.
//!
//! Convention: never panic across the boundary — every entry point returns a
//! null/empty pointer (or a sentinel int) on bad input rather than unwinding.
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::devices::{plan, Device};
use crate::media_library::MediaLibrary;

use super::SparkampCtx;

// ─────────────────────────── JSON helpers ───────────────────────────

/// Serialize `v` to a heap C string the caller frees with
/// `sparkamp_free_string`. Returns null on serialization failure.
fn json_out<T: Serialize>(v: &T) -> *mut c_char {
    match serde_json::to_string(v) {
        Ok(s) => CString::new(s).map(|c| c.into_raw()).unwrap_or(std::ptr::null_mut()),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Parse a JSON C string into `T`, or `None` on null/invalid UTF-8/bad JSON.
unsafe fn json_in<T: for<'de> Deserialize<'de>>(p: *const c_char) -> Option<T> {
    if p.is_null() {
        return None;
    }
    let s = CStr::from_ptr(p).to_str().ok()?;
    serde_json::from_str(s).ok()
}

/// Open a fresh, short-lived media-library connection for one device op.
///
/// Device ops deliberately do NOT borrow `ctx.media_library` (the main-thread
/// connection used by the tick): a separate connection lets these calls run on
/// a Swift background queue without sharing the non-`Send` `ctx`. SQLite WAL +
/// the 5 s busy timeout (set in `MediaLibrary::open`) make the two connections
/// safe against each other. `ctx` is therefore unused by every op below.
fn open_lib() -> Option<MediaLibrary> {
    MediaLibrary::open().ok()
}

/// Map the playlist-format code (0 = m3u8, 1 = m3u) to its extension. Passed in
/// from Swift so device ops never read `ctx.config` off the main thread.
fn ext_for_format(format: c_int) -> &'static str {
    if format == 1 { "m3u" } else { "m3u8" }
}

// ─────────────────────────── wire types ───────────────────────────

/// One volume enumerated by Swift (DiskArbitration / FileManager). The core
/// turns these into canonical [`Device`]s, owning identity and `fs_visible`.
#[derive(Deserialize)]
struct VolumeIn {
    mount_path: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    fs_type: String,
    /// BSD device name (e.g. "disk2s1"); kept as `backend_id` for eject.
    #[serde(default)]
    bsd_name: String,
    #[serde(default)]
    total_bytes: u64,
    #[serde(default)]
    free_bytes: u64,
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    ejectable: bool,
    /// Volume UUID when the OS exposes one — a stable identity that needs no
    /// marker-file write. Falls back to the marker when absent.
    #[serde(default)]
    volume_uuid: Option<String>,
}

/// A device audio file projected for the Swift table, with the paired library
/// path ("Synced from") when one exists.
#[derive(Serialize)]
struct DeviceTrackDto {
    path: String,
    title: String,
    artist: String,
    album: String,
    album_artist: String,
    genre: String,
    composer: String,
    comment: String,
    bpm: String,
    year: i64,
    track_num: i64,
    disc_num: i64,
    length_secs: f64,
    bitrate: i64,
    play_count: i64,
    last_played: String,
    has_art: bool,
    /// Canonical library path this device file was synced from, or null.
    synced_from: Option<String>,
}

impl DeviceTrackDto {
    fn from_lib_track(t: &crate::media_library::LibTrack, synced_from: Option<String>) -> Self {
        DeviceTrackDto {
            path: t.path.clone(),
            title: t.title.clone().unwrap_or_else(|| t.filename.clone()),
            artist: t.artist.clone().unwrap_or_default(),
            album: t.album.clone().unwrap_or_default(),
            album_artist: t.album_artist.clone().unwrap_or_default(),
            genre: t.genre.clone().unwrap_or_default(),
            composer: t.composer.clone().unwrap_or_default(),
            comment: t.comment.clone().unwrap_or_default(),
            bpm: t.bpm.clone().unwrap_or_default(),
            year: t.year.unwrap_or(0),
            track_num: t.track_num.unwrap_or(0),
            disc_num: t.disc_num.unwrap_or(0),
            length_secs: t.length_secs.unwrap_or(0.0),
            bitrate: t.bitrate.unwrap_or(0),
            play_count: t.play_count,
            last_played: t.last_played.clone().unwrap_or_default(),
            has_art: t.artwork_path.is_some(),
            synced_from,
        }
    }
}

#[derive(Serialize)]
struct DeviceCountsDto {
    songs: usize,
    playlists: usize,
}

#[derive(Serialize)]
struct ApplyResult {
    applied: usize,
    skipped: usize,
}

#[derive(Serialize)]
struct CopyResult {
    copied: usize,
    skipped: usize,
    bytes: u64,
}

#[derive(Serialize)]
struct PlaylistApplyResult {
    pushed: usize,
    pulled: usize,
    skipped: usize,
}

#[derive(Serialize)]
struct PlaylistSendResult {
    copied: usize,
    ok: bool,
}

// ─────────────────────────── entry points ───────────────────────────

/// Swift passes a JSON array of enumerated volumes; the core returns a JSON
/// array of [`Device`]. Identity is the volume UUID when present, else a
/// marker-file id (created on the first writable refresh; read-only volumes
/// without a marker get an empty id and can't pair, which is correct).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_devices_refresh(
    _ctx: *mut SparkampCtx,
    volumes_json: *const c_char,
) -> *mut c_char {
    let vols: Vec<VolumeIn> = match json_in(volumes_json) {
        Some(v) => v,
        None => return json_out(&Vec::<Device>::new()),
    };
    let devices: Vec<Device> = vols
        .into_iter()
        .map(|v| {
            let mount = PathBuf::from(&v.mount_path);
            let id = match v.volume_uuid.filter(|s| !s.is_empty()) {
                Some(uuid) => uuid,
                None if !v.read_only => {
                    crate::devices::marker::ensure_marker(&mount).unwrap_or_default()
                }
                None => crate::devices::marker::read_marker(&mount).unwrap_or_default(),
            };
            Device {
                id,
                label: v.label,
                mount_path: mount,
                fs_type: v.fs_type,
                total_bytes: v.total_bytes,
                free_bytes: v.free_bytes,
                read_only: v.read_only,
                ejectable: v.ejectable,
                backend_id: v.bsd_name,
                backend: crate::devices::DeviceBackend::Udisks,
                fs_visible: true,
            }
        })
        .collect();
    json_out(&devices)
}

/// List the device's audio files as JSON `[DeviceTrackDto]`, each annotated with
/// the library path it was synced from (if any).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_browse(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
) -> *mut c_char {
    let Some(dev) = json_in::<Device>(device_json) else {
        return std::ptr::null_mut();
    };
    // Map device-relative path → library path for the "Synced from" column.
    let synced: std::collections::HashMap<String, String> = open_lib()
        .and_then(|lib| {
            let id = plan::device_sync_id(&dev);
            lib.sync_pairs_for_device(&id).ok()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.device_relpath.replace('\\', "/"), p.library_path))
        .collect();

    let io = crate::devices::io::for_device(&dev);
    let tracks: Vec<DeviceTrackDto> = io
        .list_audio_files()
        .into_iter()
        .map(|f| {
            let t = crate::devices::browse::read_device_track(&f);
            let rel = f
                .strip_prefix(&dev.mount_path)
                .ok()
                .map(|r| r.to_string_lossy().replace('\\', "/"));
            let synced_from = rel.and_then(|r| synced.get(&r).cloned());
            DeviceTrackDto::from_lib_track(&t, synced_from)
        })
        .collect();
    json_out(&tracks)
}

/// Song / playlist counts for the overview — a directory walk only, NO tag
/// reads (unlike `browse`) and no SQLite, so it's cheap and safe to call on the
/// main thread for every connected device. Returns `{"songs":N,"playlists":M}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_counts(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
) -> *mut c_char {
    let Some(dev) = json_in::<Device>(device_json) else {
        return std::ptr::null_mut();
    };
    let io = crate::devices::io::for_device(&dev);
    json_out(&DeviceCountsDto {
        songs: io.list_audio_files().len(),
        playlists: io.playlist_files().len(),
    })
}

/// Compute the two-way sync plan for a device (JSON [`plan::SyncPlanDto`]).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_sync_plan(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
) -> *mut c_char {
    let (Some(lib), Some(dev)) = (open_lib(), json_in::<Device>(device_json)) else {
        return std::ptr::null_mut();
    };
    json_out(&plan::sync_plan_dto(&lib, &dev))
}

/// Apply a sync plan plus the user's conflict choices. Returns
/// `{"applied":N,"skipped":M}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_apply_sync(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
    plan_json: *const c_char,
    choices_json: *const c_char,
) -> *mut c_char {
    let (Some(lib), Some(dev)) = (open_lib(), json_in::<Device>(device_json)) else {
        return std::ptr::null_mut();
    };
    let Some(p) = json_in::<plan::SyncPlanDto>(plan_json) else {
        return std::ptr::null_mut();
    };
    let choices: Vec<plan::ConflictChoice> = json_in(choices_json).unwrap_or_default();
    let (applied, skipped) = plan::apply_sync_plan_dto(&lib, &dev, &p, &choices);
    json_out(&ApplyResult { applied, skipped })
}

/// Copy the given library files onto the device under the flat `Music/<file>`
/// layout, recording sync pairs. Returns `{"copied":N,"skipped":M,"bytes":B}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_copy(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
    src_paths_json: *const c_char,
) -> *mut c_char {
    let (Some(lib), Some(dev)) = (open_lib(), json_in::<Device>(device_json)) else {
        return std::ptr::null_mut();
    };
    let srcs: Vec<String> = json_in(src_paths_json).unwrap_or_default();
    let io = crate::devices::io::for_device(&dev);
    let device_id = plan::device_sync_id(&dev);
    let (mut copied, mut skipped, mut bytes) = (0usize, 0usize, 0u64);
    for s in srcs {
        let src = PathBuf::from(&s);
        if !src.exists() {
            skipped += 1;
            continue;
        }
        let (rel, present) = plan::device_plan_one(&lib, &dev.mount_path, &device_id, &src);
        if present {
            // Already on the device; still (re)record the pair so it syncs.
            plan::record_pair(&lib, &device_id, &src, &rel);
            skipped += 1;
            continue;
        }
        match io.copy_to_device(&src, &rel) {
            Ok(_) => {
                bytes += std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
                plan::record_pair(&lib, &device_id, &src, &rel);
                copied += 1;
            }
            Err(_) => skipped += 1,
        }
    }
    json_out(&CopyResult { copied, skipped, bytes })
}

/// Plan playlist sync: JSON array of the device's per-playlist sync items.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_playlist_plan(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
    playlist_format: c_int,
) -> *mut c_char {
    let (Some(lib), Some(dev)) = (open_lib(), json_in::<Device>(device_json)) else {
        return std::ptr::null_mut();
    };
    json_out(&plan::device_playlist_sync_plan(&lib, &dev, ext_for_format(playlist_format)))
}

/// Apply playlist sync. The live plan is re-derived (the call is advisory); each
/// non-conflict item is pushed or pulled. Returns `{pushed, pulled, skipped}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_playlist_apply(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
    playlist_format: c_int,
) -> *mut c_char {
    let (Some(lib), Some(dev)) = (open_lib(), json_in::<Device>(device_json)) else {
        return std::ptr::null_mut();
    };
    use crate::devices::sync::PlaylistSyncDir;
    let ext = ext_for_format(playlist_format);
    let (mut pushed, mut pulled, mut skipped) = (0usize, 0usize, 0usize);
    for item in plan::device_playlist_sync_plan(&lib, &dev, ext) {
        match item.dir {
            PlaylistSyncDir::Push => {
                let (_, ok) = plan::apply_playlist_push(&lib, &dev, &item);
                if ok {
                    pushed += 1;
                } else {
                    skipped += 1;
                }
            }
            PlaylistSyncDir::Pull => {
                if plan::apply_playlist_pull(&lib, &item) {
                    pulled += 1;
                } else {
                    skipped += 1;
                }
            }
            PlaylistSyncDir::None | PlaylistSyncDir::Conflict => skipped += 1,
        }
    }
    json_out(&PlaylistApplyResult { pushed, pulled, skipped })
}

/// Send one library playlist (by DB id) to the device as a unit: copy its
/// missing track files under Music/<file> and write the device `.m3u`. Returns
/// `{"copied":N,"ok":bool}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_send_playlist(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
    playlist_id: i64,
    playlist_format: c_int,
) -> *mut c_char {
    let (Some(lib), Some(dev)) = (open_lib(), json_in::<Device>(device_json)) else {
        return std::ptr::null_mut();
    };
    let (copied, ok) =
        plan::send_playlist_to_device(&lib, &dev, playlist_id, ext_for_format(playlist_format));
    json_out(&PlaylistSendResult { copied, ok })
}

// ─────────────────────────── device playlists ───────────────────────────

#[derive(Serialize)]
struct DevicePlaylistDto {
    name: String,
    relpath: String,
    /// Entry basenames in order — lets the UI filter the file list to this
    /// playlist without another round trip.
    entries: Vec<String>,
}

#[derive(Serialize)]
struct OkResult {
    ok: bool,
}

#[derive(Serialize)]
struct PlaylistNewResult {
    ok: bool,
    relpath: String,
}

/// Read a non-null C string into an owned `String`.
unsafe fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok().map(|s| s.to_owned())
}

/// List the device's playlist files as JSON `[{name, relpath, entries}]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_playlists(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
) -> *mut c_char {
    let Some(dev) = json_in::<Device>(device_json) else {
        return std::ptr::null_mut();
    };
    let io = crate::devices::io::for_device(&dev);
    let out: Vec<DevicePlaylistDto> = io
        .playlist_files()
        .into_iter()
        .filter_map(|p| {
            let rel = p
                .strip_prefix(&dev.mount_path)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            let name = p.file_name()?.to_string_lossy().into_owned();
            let entries = crate::devices::browse::playlist_entry_order(&p);
            Some(DevicePlaylistDto { name, relpath: rel, entries })
        })
        .collect();
    json_out(&out)
}

/// Create an empty device playlist. Returns `{"ok":bool,"relpath":string}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_playlist_new(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
    name: *const c_char,
    playlist_format: c_int,
) -> *mut c_char {
    let (Some(dev), Some(name)) = (json_in::<Device>(device_json), cstr(name)) else {
        return json_out(&PlaylistNewResult { ok: false, relpath: String::new() });
    };
    match plan::device_playlist_create(&dev, &name, ext_for_format(playlist_format)) {
        Some(relpath) => json_out(&PlaylistNewResult { ok: true, relpath }),
        None => json_out(&PlaylistNewResult { ok: false, relpath: String::new() }),
    }
}

/// Rename a device playlist file. Returns `{"ok":bool}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_playlist_rename(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
    relpath: *const c_char,
    new_name: *const c_char,
    playlist_format: c_int,
) -> *mut c_char {
    let (Some(dev), Some(rel), Some(name)) =
        (json_in::<Device>(device_json), cstr(relpath), cstr(new_name))
    else {
        return json_out(&OkResult { ok: false });
    };
    let ok = plan::device_playlist_rename(&dev, &rel, &name, ext_for_format(playlist_format));
    // Keep a linked library playlist's name in step (matches GTK): a library
    // playlist whose safe filename equals the device file's stem is the "same"
    // playlist, so rename it too. Looked up by the OLD path (the library copy
    // still carries the old name at this point).
    if ok {
        let old_path = dev.mount_path.join(&rel);
        if let Some(lib) = open_lib() {
            if let Some((id, _)) = plan::linked_library_playlist(&lib, &old_path) {
                let _ = lib.rename_playlist(id, name.trim());
            }
        }
    }
    json_out(&OkResult { ok })
}

/// Duplicate a device playlist to "<stem> copy". Returns `{"ok":bool}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_playlist_duplicate(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
    relpath: *const c_char,
) -> *mut c_char {
    let (Some(dev), Some(rel)) = (json_in::<Device>(device_json), cstr(relpath)) else {
        return json_out(&OkResult { ok: false });
    };
    json_out(&OkResult { ok: plan::device_playlist_duplicate(&dev, &rel) })
}

/// Remove the given files from ONE device playlist's `.m3u` (the files stay on
/// the device and in other playlists) — the "Remove" action, distinct from
/// "Delete". Returns `{"ok":bool}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_playlist_remove_entries(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
    relpath: *const c_char,
    paths_json: *const c_char,
) -> *mut c_char {
    let (Some(dev), Some(rel)) = (json_in::<Device>(device_json), cstr(relpath)) else {
        return json_out(&OkResult { ok: false });
    };
    let paths: Vec<String> = json_in(paths_json).unwrap_or_default();
    let basenames: std::collections::HashSet<String> = paths
        .iter()
        .filter_map(|p| {
            Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned())
        })
        .collect();
    let ok = plan::device_m3u_remove_basenames(&dev.mount_path.join(&rel), &basenames);
    json_out(&OkResult { ok })
}

/// Delete a device playlist file (the audio files stay). Returns `{"ok":bool}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_playlist_delete(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
    relpath: *const c_char,
) -> *mut c_char {
    let (Some(dev), Some(rel)) = (json_in::<Device>(device_json), cstr(relpath)) else {
        return json_out(&OkResult { ok: false });
    };
    json_out(&OkResult { ok: plan::device_playlist_delete(&dev, &rel) })
}

/// Permanently delete the given files from the device (absolute on-device
/// paths) and drop them from any device playlist. Returns the count that
/// could NOT be deleted, or -1 on bad input.
///
/// DELETION RULE: the caller (Swift) MUST have shown an explicit confirmation
/// before invoking this — it is irreversible and only allowed from the device
/// file view (see CLAUDE.md).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_delete_files(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
    paths_json: *const c_char,
) -> c_int {
    let Some(dev) = json_in::<Device>(device_json) else {
        return -1;
    };
    let paths: Vec<String> = json_in(paths_json).unwrap_or_default();
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    plan::device_delete_files(&dev, &paths) as c_int
}

/// Return the embedded artwork bytes for one side of a conflict, or null if
/// none. `side`: 0 = computer (library file), 1 = device file. `dev_relpath` is
/// the conflict's `pair.device_relpath`. Free the result with
/// `sparkamp_tag_free_artwork`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_conflict_artwork(
    _ctx: *mut SparkampCtx,
    device_json: *const c_char,
    dev_relpath: *const c_char,
    side: c_int,
    len_out: *mut c_int,
) -> *mut u8 {
    if !len_out.is_null() {
        *len_out = 0;
    }
    let (Some(lib), Some(dev)) = (open_lib(), json_in::<Device>(device_json)) else {
        return std::ptr::null_mut();
    };
    let Some(rel) = (if dev_relpath.is_null() {
        None
    } else {
        CStr::from_ptr(dev_relpath).to_str().ok()
    }) else {
        return std::ptr::null_mut();
    };

    // Resolve the file to read for the requested side.
    let file: PathBuf = if side == 1 {
        dev.mount_path.join(rel)
    } else {
        let id = plan::device_sync_id(&dev);
        let lib_path = lib
            .sync_pairs_for_device(&id)
            .unwrap_or_default()
            .into_iter()
            .find(|p| p.device_relpath.replace('\\', "/") == rel.replace('\\', "/"))
            .map(|p| p.library_path);
        match lib_path {
            Some(p) => PathBuf::from(p),
            None => return std::ptr::null_mut(),
        }
    };

    match first_picture(&file) {
        Some(bytes) if !bytes.is_empty() => {
            if !len_out.is_null() {
                *len_out = bytes.len() as c_int;
            }
            let mut boxed = bytes.into_boxed_slice();
            let ptr = boxed.as_mut_ptr();
            std::mem::forget(boxed);
            ptr
        }
        _ => std::ptr::null_mut(),
    }
}

/// Whether Sparkamp treats `fs_type` as not reliably writable (NTFS/exFAT) —
/// one source of truth for the UI's unsupported-filesystem badge.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_device_fs_unsupported(fs_type: *const c_char) -> bool {
    if fs_type.is_null() {
        return false;
    }
    match CStr::from_ptr(fs_type).to_str() {
        Ok(s) => plan::device_fs_unsupported(s),
        Err(_) => false,
    }
}

/// First embedded picture in a file's ID3 tag, if any.
fn first_picture(path: &Path) -> Option<Vec<u8>> {
    id3::Tag::read_from_path(path)
        .ok()?
        .pictures()
        .next()
        .map(|p| p.data.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn take_string(p: *mut c_char) -> String {
        assert!(!p.is_null(), "FFI returned null");
        let s = CStr::from_ptr(p).to_str().unwrap().to_owned();
        super::super::sparkamp_free_string(p);
        s
    }

    #[test]
    fn devices_refresh_round_trips_a_volume() {
        let dir = tempfile::tempdir().unwrap();
        let vols = format!(
            r#"[{{"mount_path":"{}","label":"Stick","fs_type":"exfat","bsd_name":"disk9s1","total_bytes":100,"free_bytes":40,"read_only":false,"ejectable":true,"volume_uuid":"UUID-9"}}]"#,
            dir.path().display()
        );
        let cv = CString::new(vols).unwrap();
        let out = unsafe { take_string(sparkamp_devices_refresh(std::ptr::null_mut(), cv.as_ptr())) };
        let devs: Vec<Device> = serde_json::from_str(&out).unwrap();
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].id, "UUID-9");
        assert_eq!(devs[0].backend_id, "disk9s1");
        assert_eq!(devs[0].backend, crate::devices::DeviceBackend::Udisks);
        assert!(devs[0].fs_visible);
    }

    #[test]
    fn device_counts_walks_without_reading_tags() {
        // A temp dir as the "device": two audio files + one playlist.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Music")).unwrap();
        std::fs::write(dir.path().join("Music/a.mp3"), b"x").unwrap();
        std::fs::write(dir.path().join("Music/b.flac"), b"x").unwrap();
        std::fs::write(dir.path().join("list.m3u8"), b"#EXTM3U\n").unwrap();

        let dev = crate::devices::Device {
            id: "T".into(),
            label: "T".into(),
            mount_path: dir.path().to_path_buf(),
            fs_type: "vfat".into(),
            total_bytes: 0,
            free_bytes: 0,
            read_only: false,
            ejectable: true,
            backend_id: String::new(),
            backend: crate::devices::DeviceBackend::Udisks,
            fs_visible: true,
        };
        let dj = CString::new(serde_json::to_string(&dev).unwrap()).unwrap();
        let out = unsafe { take_string(sparkamp_device_counts(std::ptr::null_mut(), dj.as_ptr())) };
        let counts: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(counts["songs"], 2);
        assert_eq!(counts["playlists"], 1);
    }

    #[test]
    fn fs_unsupported_matches_core() {
        let ntfs = CString::new("ntfs").unwrap();
        let vfat = CString::new("vfat").unwrap();
        assert!(unsafe { sparkamp_device_fs_unsupported(ntfs.as_ptr()) });
        assert!(!unsafe { sparkamp_device_fs_unsupported(vfat.as_ptr()) });
    }

    #[test]
    fn delete_files_bad_input_returns_negative_one() {
        // Null device JSON → -1 sentinel, never a panic.
        let rc = unsafe {
            sparkamp_device_delete_files(std::ptr::null_mut(), std::ptr::null(), std::ptr::null())
        };
        assert_eq!(rc, -1);
    }
}
