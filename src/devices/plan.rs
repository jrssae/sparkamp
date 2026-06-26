//! Device sync planning and apply logic, extracted from the GTK frontend so it
//! is unit-testable without a UI and lives next to the rest of the device core.
//!
//! These functions operate purely on `MediaLibrary`, `Device`, the `DeviceIo`
//! trait, and the filesystem — never the frontend's `AppState`. The GTK layer
//! keeps thin `state`-based shims that pull `media_lib` and forward here, so the
//! AppState coupling stays in the frontend where it belongs.

// Dead on the macOS build, where the GTK frontend (the only caller) is absent;
// mirrors the allow used across the other device modules.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::devices::Device;
use crate::media_library::MediaLibrary;

// ─────────────────────────── identity / paths ───────────────────────────

/// Device identity for sync pairs: the volume UUID, or a marker id written now
/// (the first time a file is paired to this device).
pub(crate) fn device_sync_id(dev: &Device) -> String {
    if dev.id.is_empty() {
        crate::devices::marker::ensure_marker(&dev.mount_path).unwrap_or_default()
    } else {
        dev.id.clone()
    }
}

/// Canonical identity for a library file, used as the sync-pair key. Every
/// copy path (drag-drop, playlist send, sync) resolves the same file to the
/// same string so dedup works regardless of which view supplied the path.
/// Falls back to the raw path when the file can't be canonicalized.
pub(crate) fn canonical_lib_path(src: &Path) -> String {
    src.canonicalize()
        .unwrap_or_else(|_| src.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// File modification time in whole seconds since the epoch (0 on error).
fn file_mtime(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Filesystems Sparkamp can't reliably read/write yet — shown with a warning.
pub(crate) fn device_fs_unsupported(fs_type: &str) -> bool {
    matches!(fs_type.to_ascii_lowercase().as_str(), "ntfs" | "exfat")
}

/// Sanitize a playlist name into the bare filename stem used for its `.m3u`/
/// `.m3u8` on a device: strip path-hostile characters and surrounding dots/
/// spaces, falling back to "Playlist" when nothing usable remains.
pub(crate) fn safe_playlist_filename(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    let safe = safe.trim().trim_matches('.').trim();
    if safe.is_empty() {
        "Playlist".to_string()
    } else {
        safe.to_string()
    }
}

// ─────────────────────────── copy placement ───────────────────────────

/// The DB half of [`device_plan_one`]: the recorded sync-pair device relpath for
/// `src` on this device, if any. Touches only the SQLite library; no filesystem
/// IO, so the FS half can run on a worker thread.
pub(crate) fn recorded_relpath(
    lib: &MediaLibrary,
    device_id: &str,
    src: &Path,
) -> Option<PathBuf> {
    if device_id.is_empty() {
        return None;
    }
    let lib_path = canonical_lib_path(src);
    lib.sync_pairs_for_library_path(&lib_path)
        .ok()
        .and_then(|ps| ps.into_iter().find(|p| p.device_id == device_id))
        .map(|p| PathBuf::from(p.device_relpath))
}

/// The filesystem half of [`device_plan_one`]: given the recorded relpath (from
/// [`recorded_relpath`]), decide the final relpath and whether the file is
/// already present, using `metadata`/`exists` checks on the device. This is the
/// part that can be slow over a gvfs/MTP FUSE mount, so callers run it on a
/// worker thread.
pub(crate) fn device_plan_fs(
    mount: &Path,
    src: &Path,
    recorded: Option<PathBuf>,
) -> (PathBuf, bool) {
    use crate::devices::transfer;
    if let Some(rel) = recorded {
        // Only honour the recorded slot if it still matches the flat layout
        // (Music/<file>, two components) and the file is actually present.
        let flat = rel.starts_with("Music") && rel.components().count() == 2;
        if flat && mount.join(&rel).exists() {
            return (rel, true);
        }
    }
    // No usable pair: plan flat, deduping by name+size against the device.
    let base = transfer::device_flat_relpath(src);
    let dest = mount.join(&base);
    let src_len = std::fs::metadata(src).ok().map(|m| m.len());
    match std::fs::metadata(&dest) {
        Ok(dmeta) if Some(dmeta.len()) == src_len => (base, true), // same file already there
        Ok(_) => (transfer::resolve_collision(mount, &base), false), // different file → suffix
        Err(_) => (base, false),                                    // free slot
    }
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
/// [`recorded_relpath`] (main thread) + [`device_plan_fs`] (worker).
pub(crate) fn device_plan_one(
    lib: &MediaLibrary,
    mount: &Path,
    device_id: &str,
    src: &Path,
) -> (PathBuf, bool) {
    device_plan_fs(mount, src, recorded_relpath(lib, device_id, src))
}

/// Record (or refresh) the sync pair for a just-copied file with its REAL tag
/// baseline, so a later sync sees no change until a tag is actually edited.
pub(crate) fn record_pair(lib: &MediaLibrary, device_id: &str, src: &Path, relpath: &Path) {
    if device_id.is_empty() {
        return;
    }
    use crate::devices::sync;
    let st = sync::read_tag_state(src);
    let _ = lib.upsert_sync_pair(&crate::media_library::SyncPair {
        device_id: device_id.to_string(),
        device_relpath: relpath.to_string_lossy().into_owned(),
        library_path: canonical_lib_path(src),
        baseline_tag_hash: sync::tag_hash(&st),
        baseline_rating: st.rating as i64,
        baseline_playcount: st.play_count as i64,
        last_sync_at: Some(crate::timeutil::format_current_timestamp()),
    });
}

/// If a device playlist file is linked to a library playlist — i.e. some library
/// playlist's safe filename equals the device file's stem — return its
/// `(id, name)`. Device-only playlists (no library match) return `None`.
pub(crate) fn linked_library_playlist(
    lib: &MediaLibrary,
    dev_playlist: &Path,
) -> Option<(i64, String)> {
    let stem = dev_playlist.file_stem()?.to_string_lossy().into_owned();
    lib.all_playlists()
        .ok()?
        .into_iter()
        .find(|p| safe_playlist_filename(&p.name) == stem)
        .map(|p| (p.id, p.name))
}

// ─────────────────────────── tag sync ───────────────────────────

/// Compute the per-pair sync decisions for a device: for each recorded sync
/// pair, hash the current tags on each side and decide the direction. Also
/// adopts unpaired device files that match a library track (so a file already on
/// both sides still participates), preferring the recorded source over a name
/// guess.
pub(crate) fn device_sync_plan(
    lib: &MediaLibrary,
    dev: &Device,
) -> Vec<(crate::media_library::SyncPair, crate::devices::sync::SyncAction)> {
    use crate::devices::sync::{self, SideState};
    let device_id = if dev.id.is_empty() {
        crate::devices::marker::read_marker(&dev.mount_path).unwrap_or_default()
    } else {
        dev.id.clone()
    };
    if device_id.is_empty() {
        return Vec::new();
    }
    let side = |p: &Path| {
        p.exists().then(|| SideState {
            hash: sync::tag_hash(&sync::read_tag_state(p)),
            mtime: file_mtime(p),
        })
    };
    let pairs = lib.sync_pairs_for_device(&device_id).unwrap_or_default();
    let mut out: Vec<(crate::media_library::SyncPair, sync::SyncAction)> = pairs
        .iter()
        .map(|pair| {
            let lib_path = PathBuf::from(&pair.library_path);
            let dev_path = dev.mount_path.join(&pair.device_relpath);
            let action = sync::decide(
                &pair.baseline_tag_hash,
                side(&lib_path).as_ref(),
                side(&dev_path).as_ref(),
            );
            (pair.clone(), action)
        })
        .collect();

    // Adopt unpaired device files that match a library track by filename, so a
    // file already on both sides (copied externally, or under an old device id)
    // still participates in sync. Baseline = the device's current tags, so a
    // differing library copy pushes to the device (the common "edited on the
    // computer" case); identical files resolve to no-op.
    let paired: HashSet<String> =
        pairs.iter().map(|p| p.device_relpath.replace('\\', "/")).collect();
    let by_filename: HashMap<String, String> = lib
        .all_tracks()
        .unwrap_or_default()
        .into_iter()
        .map(|t| (t.filename, t.path))
        .collect();
    for dev_file in crate::devices::io::for_device(dev).list_audio_files() {
        let Ok(rel) = dev_file.strip_prefix(&dev.mount_path) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if paired.contains(&rel_str) {
            continue;
        }
        // Prefer the exact source this file was copied from (recorded under any
        // device id), so a re-detected device keeps its real pairing rather than
        // guessing among same-named library files. Fall back to a unique
        // filename match only when there's no recorded source.
        let recorded = lib
            .library_paths_for_device_relpath(&rel_str)
            .unwrap_or_default();
        let lib_path: String = if recorded.len() == 1 {
            recorded.into_iter().next().unwrap()
        } else {
            let Some(fname) = dev_file.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            match by_filename.get(fname) {
                Some(p) => p.clone(),
                None => continue,
            }
        };
        let lib_path = &lib_path;
        let dev_side = side(&dev_file);
        let baseline = dev_side
            .as_ref()
            .map(|s| s.hash.clone())
            .unwrap_or_default();
        let pair = crate::media_library::SyncPair {
            device_id: device_id.clone(),
            device_relpath: rel_str,
            library_path: lib_path.clone(),
            baseline_tag_hash: baseline.clone(),
            baseline_rating: 0,
            baseline_playcount: 0,
            last_sync_at: None,
        };
        let _ = lib.upsert_sync_pair(&pair);
        let action = sync::decide(
            &baseline,
            side(Path::new(lib_path)).as_ref(),
            dev_side.as_ref(),
        );
        out.push((pair, action));
    }
    out
}

/// Apply one tag-sync direction to a single pair and refresh its baseline.
/// `to_device` true = library→device, false = device→library. Returns ok.
pub(crate) fn apply_tag_pair(
    lib: &MediaLibrary,
    dev: &Device,
    pair: &crate::media_library::SyncPair,
    to_device: bool,
) -> bool {
    use crate::devices::{sync, DeviceBackend};
    let lib_path = PathBuf::from(&pair.library_path);
    let dev_path = dev.mount_path.join(&pair.device_relpath);
    let result: Result<sync::TagState, ()> = if to_device {
        let st = sync::read_tag_state(&lib_path);
        if dev.backend == DeviceBackend::Mtp {
            // MTP can't rewrite tags in place — delete the device file, then
            // re-upload the local one (which already carries the desired tags).
            // Use copy_to_device (fresh create), NOT a truncating overwrite,
            // which corrupts MTP files when the delete hasn't taken yet.
            let io = crate::devices::io::for_device(dev);
            let _ = io.delete(&dev_path);
            io.copy_to_device(&lib_path, Path::new(&pair.device_relpath))
                .map(|_| st)
                .map_err(|_| ())
        } else {
            sync::apply_tags(&st, &dev_path).map(|_| st).map_err(|_| ())
        }
    } else {
        let st = sync::read_tag_state(&dev_path);
        sync::apply_tags(&st, &lib_path).map(|_| st).map_err(|_| ())
    };
    match result {
        Ok(st) => {
            let mut p = pair.clone();
            p.baseline_tag_hash = sync::tag_hash(&st);
            p.baseline_rating = st.rating as i64;
            p.baseline_playcount = st.play_count as i64;
            p.last_sync_at = Some(crate::timeutil::format_current_timestamp());
            let _ = lib.upsert_sync_pair(&p);
            true
        }
        Err(_) => false,
    }
}

/// Apply a sync plan: propagate the winning side's tags for the unambiguous
/// directions (conflicts are handled separately by the prompt) and refresh each
/// pair's baseline. Returns `(applied, failed)`.
pub(crate) fn apply_device_sync(
    lib: &MediaLibrary,
    dev: &Device,
    plan: &[(crate::media_library::SyncPair, crate::devices::sync::SyncAction)],
) -> (usize, usize) {
    use crate::devices::sync::SyncAction;
    let (mut applied, mut failed) = (0usize, 0usize);
    for (pair, action) in plan {
        let to_device = match action {
            SyncAction::LibraryToDevice => true,
            SyncAction::DeviceToLibrary => false,
            _ => continue,
        };
        if apply_tag_pair(lib, dev, pair, to_device) {
            applied += 1;
        } else {
            failed += 1;
        }
    }
    (applied, failed)
}

/// One song whose tags changed on both the computer and the device since the
/// last sync, with the differing fields, for the per-file conflict prompt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TagConflictItem {
    pub(crate) pair: crate::media_library::SyncPair,
    pub(crate) song: String,
    pub(crate) diffs: Vec<crate::devices::sync::FieldDiff>,
}

/// Build the per-file tag-conflict items from a sync plan: for each pair marked
/// `Conflict`, read both sides' tags and compute the differing fields.
pub(crate) fn build_tag_conflicts(
    dev: &Device,
    plan: &[(crate::media_library::SyncPair, crate::devices::sync::SyncAction)],
) -> Vec<TagConflictItem> {
    use crate::devices::sync::{self, SyncAction};
    let mut out = Vec::new();
    for (pair, action) in plan {
        if *action != SyncAction::Conflict {
            continue;
        }
        let lib_path = PathBuf::from(&pair.library_path);
        let dev_path = dev.mount_path.join(&pair.device_relpath);
        let lib_st = sync::read_tag_state(&lib_path);
        let dev_st = sync::read_tag_state(&dev_path);
        let diffs = sync::tag_field_diffs(&lib_st, &dev_st);
        if diffs.is_empty() {
            continue; // tag-hash differed but no comparable field did
        }
        // Prefer "Artist - Title"; fall back to the filename.
        let song = if !lib_st.title.is_empty() {
            if lib_st.artist.is_empty() {
                lib_st.title.clone()
            } else {
                format!("{} - {}", lib_st.artist, lib_st.title)
            }
        } else {
            lib_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        out.push(TagConflictItem {
            pair: pair.clone(),
            song,
            diffs,
        });
    }
    out
}

// ─────────────────────────── FFI plan DTOs ───────────────────────────

/// One auto-resolved (single-side-changed) pair, projected for the JSON FFI so
/// the Swift side can render it. `dev_path` is the device-relative path (the
/// same key echoed back in [`ConflictChoice`]); `field_summary` is the
/// comma-joined labels of the tag fields that differ.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub(crate) struct SyncPairDto {
    pub(crate) lib_path: String,
    pub(crate) dev_path: String,
    pub(crate) field_summary: String,
}

/// A flat, JSON-able sync plan for the macOS FFI. The internal
/// [`device_sync_plan`] keys actions to `SyncPair` rows; this projects them into
/// the three buckets the UI shows — auto to-device, auto to-library, and
/// both-changed conflicts that need the dialog.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub(crate) struct SyncPlanDto {
    pub(crate) to_device: Vec<SyncPairDto>,
    pub(crate) to_library: Vec<SyncPairDto>,
    pub(crate) conflicts: Vec<TagConflictItem>,
    /// File-body bytes a sync will copy. Nonzero only for MTP library→device
    /// (delete + re-upload); POSIX tag writes are in-place, so 0 on macOS.
    pub(crate) bytes_to_copy: u64,
}

/// Which side the user kept for a both-changed conflict.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KeepSide {
    Computer,
    Device,
}

/// The user's resolution for one conflicting pair, echoed back from the UI.
/// `dev_path` matches the conflict's `pair.device_relpath`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub(crate) struct ConflictChoice {
    pub(crate) dev_path: String,
    pub(crate) keep: KeepSide,
}

/// Comma-joined labels of the tag fields that differ between the two copies of
/// a pair, for the UI's at-a-glance "what's syncing" hint.
fn pair_field_summary(dev: &Device, pair: &crate::media_library::SyncPair) -> String {
    use crate::devices::sync;
    let lib_st = sync::read_tag_state(Path::new(&pair.library_path));
    let dev_st = sync::read_tag_state(&dev.mount_path.join(&pair.device_relpath));
    sync::tag_field_diffs(&lib_st, &dev_st)
        .into_iter()
        .map(|d| d.label)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Build the flat [`SyncPlanDto`] for a device by running the existing
/// [`device_sync_plan`] + [`build_tag_conflicts`] and projecting the result —
/// the decision logic is reused, not reimplemented.
pub(crate) fn sync_plan_dto(lib: &MediaLibrary, dev: &Device) -> SyncPlanDto {
    use crate::devices::sync::SyncAction;
    use crate::devices::DeviceBackend;
    let plan = device_sync_plan(lib, dev);
    let (mut to_device, mut to_library) = (Vec::new(), Vec::new());
    let mut bytes_to_copy = 0u64;
    for (pair, action) in &plan {
        let dto = SyncPairDto {
            lib_path: pair.library_path.clone(),
            dev_path: pair.device_relpath.clone(),
            field_summary: pair_field_summary(dev, pair),
        };
        match action {
            SyncAction::LibraryToDevice => {
                if dev.backend == DeviceBackend::Mtp {
                    bytes_to_copy += std::fs::metadata(&pair.library_path)
                        .map(|m| m.len())
                        .unwrap_or(0);
                }
                to_device.push(dto);
            }
            SyncAction::DeviceToLibrary => to_library.push(dto),
            _ => {}
        }
    }
    let conflicts = build_tag_conflicts(dev, &plan);
    SyncPlanDto {
        to_device,
        to_library,
        conflicts,
        bytes_to_copy,
    }
}

/// Apply a sync plan from the FFI: auto pairs unconditionally, then each
/// resolved conflict per the user's [`ConflictChoice`]. The live device state is
/// re-derived (the passed `_plan` is advisory) so a stale UI snapshot can't
/// misapply. Returns `(applied, skipped)`; skipped counts failed writes and
/// unresolved conflicts.
pub(crate) fn apply_sync_plan_dto(
    lib: &MediaLibrary,
    dev: &Device,
    _plan: &SyncPlanDto,
    choices: &[ConflictChoice],
) -> (usize, usize) {
    use crate::devices::sync::SyncAction;
    let plan = device_sync_plan(lib, dev);
    // Single-side-changed pairs apply unconditionally (conflicts are skipped
    // inside apply_device_sync); `failed` folds into the skipped count.
    let (mut applied, mut skipped) = apply_device_sync(lib, dev, &plan);
    let choice: HashMap<&str, KeepSide> =
        choices.iter().map(|c| (c.dev_path.as_str(), c.keep)).collect();
    for (pair, action) in &plan {
        if *action != SyncAction::Conflict {
            continue;
        }
        match choice.get(pair.device_relpath.as_str()) {
            Some(KeepSide::Computer) => {
                if apply_tag_pair(lib, dev, pair, true) {
                    applied += 1;
                } else {
                    skipped += 1;
                }
            }
            Some(KeepSide::Device) => {
                if apply_tag_pair(lib, dev, pair, false) {
                    applied += 1;
                } else {
                    skipped += 1;
                }
            }
            None => skipped += 1, // unresolved — leave for the next sync
        }
    }
    (applied, skipped)
}

// ─────────────────────────── playlist sync ───────────────────────────

/// One library playlist's two-way sync decision against a device.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct PlaylistSyncItem {
    pub(crate) library_playlist_id: i64,
    pub(crate) library_name: String,
    /// The library playlist's `.m3u8` file on disk.
    pub(crate) library_path: PathBuf,
    pub(crate) device_id: String,
    /// The current device playlist file, if one was found.
    pub(crate) device_file: Option<PathBuf>,
    /// Where the device file should live (safe name + configured extension).
    pub(crate) desired_device_filename: String,
    /// Existing library files for this playlist, in order (used when pushing).
    pub(crate) srcs: Vec<PathBuf>,
    /// Device entry order (basenames), used when pulling.
    pub(crate) dev_basenames: Vec<String>,
    pub(crate) dir: crate::devices::sync::PlaylistSyncDir,
    /// Number of entries that differ between the two sides (for the prompt).
    pub(crate) differ: usize,
}

/// Count of entries differing between two ordered basename lists (multiset
/// symmetric difference: additions + removals, counting duplicates).
fn multiset_diff_count(a: &[String], b: &[String]) -> usize {
    let mut counts: HashMap<&str, i64> = HashMap::new();
    for x in a {
        *counts.entry(x.as_str()).or_default() += 1;
    }
    for y in b {
        *counts.entry(y.as_str()).or_default() -= 1;
    }
    counts.values().map(|v| v.unsigned_abs() as usize).sum()
}

/// Build the two-way playlist sync plan for a device: for each library playlist
/// that is on the device (or was, per a stored baseline), decide whether to
/// push to the device, pull into the library, or flag a conflict.
pub(crate) fn device_playlist_sync_plan(
    lib: &MediaLibrary,
    dev: &Device,
    ext: &str,
) -> Vec<PlaylistSyncItem> {
    use crate::devices::sync::{decide_playlist, entries_hash};
    let device_id = device_sync_id(dev);
    if device_id.is_empty() || dev.read_only || device_fs_unsupported(&dev.fs_type) {
        return Vec::new();
    }
    let playlists = lib.all_playlists().unwrap_or_default();
    let baselines = lib
        .playlist_baselines_for_device(&device_id)
        .unwrap_or_default();
    let mut out = Vec::new();
    for pl in playlists {
        let safe = safe_playlist_filename(&pl.name);
        let baseline = baselines.iter().find(|b| b.library_playlist_id == pl.id);
        // Locate the device file: configured extension, legacy variants, then
        // the baseline's recorded filename (catches a library-side rename).
        let mut candidates = vec![
            dev.mount_path.join(format!("{safe}.{ext}")),
            dev.mount_path.join(format!("{safe}.m3u8")),
            dev.mount_path.join(format!("{safe}.m3u")),
        ];
        if let Some(b) = baseline {
            candidates.push(dev.mount_path.join(&b.device_filename));
        }
        let device_file = candidates.into_iter().find(|p| p.exists());
        // A playlist never sent to this device (no baseline, no file) is not
        // part of sync — it is sent explicitly via the Send action.
        if baseline.is_none() && device_file.is_none() {
            continue;
        }
        let loaded = lib.load_playlist_tracks(&pl).unwrap_or_default();
        let lib_basenames: Vec<String> = loaded.iter().map(|t| t.filename.clone()).collect();
        let srcs: Vec<PathBuf> = loaded
            .iter()
            .map(|t| PathBuf::from(&t.path))
            .filter(|p| p.exists())
            .collect();
        let dev_basenames: Vec<String> = device_file
            .as_ref()
            .map(|p| crate::devices::browse::playlist_entry_order(p))
            .unwrap_or_default();
        let lib_hash = entries_hash(&lib_basenames);
        let dev_hash = entries_hash(&dev_basenames);
        let dir = decide_playlist(
            baseline.map(|b| b.entries_hash.as_str()),
            device_file.is_some(),
            &lib_hash,
            &dev_hash,
        );
        let differ = multiset_diff_count(&lib_basenames, &dev_basenames);
        out.push(PlaylistSyncItem {
            library_playlist_id: pl.id,
            library_name: pl.name,
            library_path: PathBuf::from(&pl.path),
            device_id: device_id.clone(),
            device_file,
            desired_device_filename: format!("{safe}.{ext}"),
            srcs,
            dev_basenames,
            dir,
            differ,
        });
    }
    out
}

/// Send one library playlist to a device as a unit: copy its (missing) track
/// files under the flat layout and write the device `.m3u`, reusing
/// [`apply_playlist_push`]. Unlike [`device_playlist_sync_plan`], this forces a
/// push for a playlist that may never have been on the device. Returns
/// `(files_copied, ok)`.
pub(crate) fn send_playlist_to_device(
    lib: &MediaLibrary,
    dev: &Device,
    playlist_id: i64,
    ext: &str,
) -> (usize, bool) {
    let device_id = device_sync_id(dev);
    if device_id.is_empty() || dev.read_only || device_fs_unsupported(&dev.fs_type) {
        return (0, false);
    }
    let Ok(pl) = lib.playlist_by_id(playlist_id) else {
        return (0, false);
    };
    let loaded = lib.load_playlist_tracks(&pl).unwrap_or_default();
    let srcs: Vec<PathBuf> = loaded
        .iter()
        .map(|t| PathBuf::from(&t.path))
        .filter(|p| p.exists())
        .collect();
    let safe = safe_playlist_filename(&pl.name);
    // Reuse an existing device file (any extension variant) so a re-send
    // overwrites rather than duplicates.
    let device_file = [
        format!("{safe}.{ext}"),
        format!("{safe}.m3u8"),
        format!("{safe}.m3u"),
    ]
    .iter()
    .map(|n| dev.mount_path.join(n))
    .find(|p| p.exists());
    let item = PlaylistSyncItem {
        library_playlist_id: pl.id,
        library_name: pl.name.clone(),
        library_path: PathBuf::from(&pl.path),
        device_id,
        device_file,
        desired_device_filename: format!("{safe}.{ext}"),
        srcs,
        dev_basenames: Vec::new(),
        dir: crate::devices::sync::PlaylistSyncDir::Push,
        differ: 0,
    };
    apply_playlist_push(lib, dev, &item)
}

// ─────────────────────── device playlist file ops ───────────────────────
//
// Direct filesystem operations on the device's own `.m3u`/`.m3u8` files (no
// library DB), used by the macOS device-playlists UI. Device playlists live at
// the storage root so their relative `Music/<file>` entries resolve.

/// Create an empty device playlist `<name>.<ext>` at the device root. Returns
/// its device-relative path (the filename), or `None` on failure / read-only.
pub(crate) fn device_playlist_create(dev: &Device, name: &str, ext: &str) -> Option<String> {
    if dev.read_only {
        return None;
    }
    let filename = format!("{}.{ext}", safe_playlist_filename(name));
    let path = dev.mount_path.join(&filename);
    if !path.exists() {
        std::fs::write(&path, "#EXTM3U\n").ok()?;
    }
    Some(filename)
}

/// Rename a device playlist file (keeping its extension). Returns ok.
pub(crate) fn device_playlist_rename(
    dev: &Device,
    relpath: &str,
    new_name: &str,
    ext: &str,
) -> bool {
    if dev.read_only {
        return false;
    }
    let old = dev.mount_path.join(relpath);
    let parent = old.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| dev.mount_path.clone());
    let new = parent.join(format!("{}.{ext}", safe_playlist_filename(new_name)));
    if new == old {
        return true;
    }
    std::fs::rename(&old, &new).is_ok()
}

/// Duplicate a device playlist to "<stem> copy[ N].<ext>". Returns ok.
pub(crate) fn device_playlist_duplicate(dev: &Device, relpath: &str) -> bool {
    if dev.read_only {
        return false;
    }
    let src = dev.mount_path.join(relpath);
    let Some(stem) = src.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
        return false;
    };
    let ext = src
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "m3u8".to_string());
    let parent = src.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| dev.mount_path.clone());
    // Find a free "<stem> copy" / "<stem> copy 2" … name.
    let mut candidate = parent.join(format!("{stem} copy.{ext}"));
    let mut n = 2;
    while candidate.exists() {
        candidate = parent.join(format!("{stem} copy {n}.{ext}"));
        n += 1;
    }
    std::fs::copy(&src, &candidate).is_ok()
}

/// Delete a device playlist file (the `.m3u` only; the audio files stay).
pub(crate) fn device_playlist_delete(dev: &Device, relpath: &str) -> bool {
    if dev.read_only {
        return false;
    }
    std::fs::remove_file(dev.mount_path.join(relpath)).is_ok()
}

/// Record/refresh the per-playlist baseline after a sync resolves it.
fn update_playlist_baseline(
    lib: &MediaLibrary,
    item: &PlaylistSyncItem,
    device_filename: &str,
    entries_hash: &str,
) {
    let _ = lib.upsert_playlist_baseline(&crate::media_library::PlaylistBaseline {
        device_id: item.device_id.clone(),
        library_playlist_id: item.library_playlist_id,
        device_filename: device_filename.to_string(),
        entries_hash: entries_hash.to_string(),
        last_sync_at: Some(crate::timeutil::format_current_timestamp()),
    });
}

/// Push a library playlist to the device: copy any missing tracks (flat
/// `Music/<file>`, deduped), rewrite the device `.m3u8`, drop the old device
/// file if the playlist was renamed, and refresh the baseline. Audio files for
/// tracks removed from the playlist stay on the device (Deletion Rule).
/// Returns `(files_copied, ok)`.
pub(crate) fn apply_playlist_push(
    lib: &MediaLibrary,
    dev: &Device,
    item: &PlaylistSyncItem,
) -> (usize, bool) {
    let io = crate::devices::io::for_device(dev);
    // (device relpath, library source path) pairs, so the written file carries
    // #EXTINF metadata from the library.
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut copied = 0usize;
    for src in &item.srcs {
        let (rel, present) = device_plan_one(lib, &dev.mount_path, &item.device_id, src);
        if !present {
            if io.copy_to_device(src, &rel).is_err() {
                continue;
            }
            copied += 1;
        }
        record_pair(lib, &item.device_id, src, &rel);
        entries.push((
            rel.to_string_lossy().replace('\\', "/"),
            src.to_string_lossy().into_owned(),
        ));
    }
    let dest = dev.mount_path.join(&item.desired_device_filename);
    let body = lib.build_device_m3u(&entries);
    let ok = std::fs::write(&dest, body).is_ok();
    // Library-side rename: remove the stale device file under the old name.
    if let Some(old) = &item.device_file {
        if old != &dest && old.exists() {
            let _ = io.delete(old);
        }
    }
    let basenames: Vec<String> = entries
        .iter()
        .map(|(e, _)| e.rsplit(['/', '\\']).next().unwrap_or(e).to_string())
        .collect();
    update_playlist_baseline(
        lib,
        item,
        &item.desired_device_filename,
        &crate::devices::sync::entries_hash(&basenames),
    );
    (copied, ok)
}

/// Pull a device playlist into the library: rewrite the library playlist file to
/// mirror the device's order/membership (mapping device filenames back to
/// library tracks by filename), then refresh the baseline. Returns ok.
pub(crate) fn apply_playlist_pull(lib: &MediaLibrary, item: &PlaylistSyncItem) -> bool {
    // Map device basenames → library track paths.
    let by_name: HashMap<String, String> = lib
        .all_tracks()
        .unwrap_or_default()
        .into_iter()
        .map(|t| (t.filename, t.path))
        .collect();
    let paths: Vec<String> = item
        .dev_basenames
        .iter()
        .filter_map(|b| by_name.get(b).cloned())
        .collect();
    let ok = lib
        .save_playlist_tracks_to_path(&item.library_path, &paths)
        .is_ok();
    let dev_filename = item
        .device_file
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| item.desired_device_filename.clone());
    update_playlist_baseline(
        lib,
        item,
        &dev_filename,
        &crate::devices::sync::entries_hash(&item.dev_basenames),
    );
    ok
}

// ─────────────────────────── device file ops ───────────────────────────

/// Rewrite a device `.m3u`/`.m3u8`, dropping every track line whose filename
/// (basename of the entry, `/` or `\` separated) is in `remove`. Comment/blank
/// lines are preserved. Returns true if the file changed.
pub(crate) fn device_m3u_remove_basenames(path: &Path, remove: &HashSet<String>) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let mut out = String::new();
    let mut changed = false;
    // A removed track's `#EXTINF` line precedes its path, so buffer it and drop
    // the pair together rather than leaving a dangling EXTINF.
    let mut pending_extinf: Option<String> = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#EXTINF") {
            if let Some(e) = pending_extinf.take() {
                out.push_str(&e);
                out.push('\n');
            }
            pending_extinf = Some(line.to_string());
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            if let Some(e) = pending_extinf.take() {
                out.push_str(&e);
                out.push('\n');
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let base = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
        if remove.contains(base) {
            changed = true;
            pending_extinf = None; // drop the entry's EXTINF too
            continue;
        }
        if let Some(e) = pending_extinf.take() {
            out.push_str(&e);
            out.push('\n');
        }
        out.push_str(line);
        out.push('\n');
    }
    if let Some(e) = pending_extinf.take() {
        out.push_str(&e);
        out.push('\n');
    }
    if changed {
        let _ = std::fs::write(path, out);
    }
    changed
}

/// Delete files from a device and remove them from every device playlist that
/// referenced them. `paths` are absolute on-device paths. Returns the number of
/// files that couldn't be deleted.
pub(crate) fn device_delete_files(dev: &Device, paths: &[PathBuf]) -> usize {
    let io = crate::devices::io::for_device(dev);
    let mut failed = 0usize;
    let mut basenames: HashSet<String> = HashSet::new();
    for p in paths {
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            basenames.insert(name.to_string());
        }
        if io.delete(p).is_err() {
            failed += 1;
        }
    }
    // Drop the deleted files from every playlist on the device.
    for pl in io.playlist_files() {
        device_m3u_remove_basenames(&pl, &basenames);
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_json_round_trips() {
        let d = crate::devices::Device {
            id: "uuid-1".into(),
            label: "Stick".into(),
            mount_path: std::path::PathBuf::from("/Volumes/STICK"),
            fs_type: "exfat".into(),
            total_bytes: 1000,
            free_bytes: 400,
            read_only: false,
            ejectable: true,
            backend_id: "disk2s1".into(),
            backend: crate::devices::DeviceBackend::Udisks,
            fs_visible: true,
        };
        let j = serde_json::to_string(&d).unwrap();
        let back: crate::devices::Device = serde_json::from_str(&j).unwrap();
        assert_eq!(d, back);
    }

    fn write_title(path: &Path, title: &str) {
        use id3::TagLike;
        std::fs::write(path, b"").unwrap();
        let mut t = id3::Tag::new();
        t.set_title(title);
        t.write_to_path(path, id3::Version::Id3v24).unwrap();
    }

    fn test_device(mount: &Path) -> Device {
        Device {
            id: "TESTDEV".into(),
            label: "Test".into(),
            mount_path: mount.to_path_buf(),
            fs_type: "vfat".into(),
            total_bytes: 0,
            free_bytes: 0,
            read_only: false,
            ejectable: true,
            backend_id: String::new(),
            backend: crate::devices::DeviceBackend::Udisks,
            fs_visible: true,
        }
    }

    #[test]
    fn sync_plan_dto_routes_single_side_change_and_applies() {
        use crate::devices::sync;

        // In-memory library DB (kept alive via the NamedTempFile binding).
        let db = tempfile::NamedTempFile::new().unwrap();
        let lib = MediaLibrary::open_at(db.path()).unwrap();

        // Device mount with Music/song.mp3; separate library copy.
        let devdir = tempfile::tempdir().unwrap();
        let music = devdir.path().join("Music");
        std::fs::create_dir_all(&music).unwrap();
        let dev_file = music.join("song.mp3");
        write_title(&dev_file, "Device");

        let libdir = tempfile::tempdir().unwrap();
        let lib_file = libdir.path().join("song.mp3");
        write_title(&lib_file, "Computer"); // library side differs from baseline

        let dev = test_device(devdir.path());

        // Baseline = the device's current tags, so the device is "unchanged"
        // and only the library side differs → LibraryToDevice.
        let baseline = sync::tag_hash(&sync::read_tag_state(&dev_file));
        lib.upsert_sync_pair(&crate::media_library::SyncPair {
            device_id: "TESTDEV".into(),
            device_relpath: "Music/song.mp3".into(),
            library_path: canonical_lib_path(&lib_file),
            baseline_tag_hash: baseline,
            baseline_rating: 0,
            baseline_playcount: 0,
            last_sync_at: None,
        })
        .unwrap();

        let dto = sync_plan_dto(&lib, &dev);
        assert_eq!(dto.to_device.len(), 1, "library change should route to device");
        assert!(dto.to_library.is_empty());
        assert!(dto.conflicts.is_empty());
        assert_eq!(dto.bytes_to_copy, 0, "POSIX tag write copies no file body");
        assert!(dto.to_device[0].field_summary.contains("Title"));

        // No conflicts, so no choices needed; the auto pair applies.
        let (applied, skipped) = apply_sync_plan_dto(&lib, &dev, &dto, &[]);
        assert_eq!((applied, skipped), (1, 0));

        // The device file now carries the library's title.
        assert_eq!(sync::read_tag_state(&dev_file).title, "Computer");
    }

    #[test]
    fn safe_playlist_filename_strips_hostile_chars_and_falls_back() {
        assert_eq!(safe_playlist_filename("Road/Trip:2024"), "Road_Trip_2024");
        assert_eq!(safe_playlist_filename("  ..  "), "Playlist");
        assert_eq!(safe_playlist_filename("Chill"), "Chill");
    }

    #[test]
    fn fs_unsupported_flags_ntfs_and_exfat_only() {
        assert!(device_fs_unsupported("ntfs"));
        assert!(device_fs_unsupported("exFAT"));
        assert!(!device_fs_unsupported("vfat"));
        assert!(!device_fs_unsupported("ext4"));
    }

    #[test]
    fn multiset_diff_counts_additions_and_removals_with_dupes() {
        // identical → 0
        assert_eq!(multiset_diff_count(&s(&["a", "b"]), &s(&["a", "b"])), 0);
        // one added on the right → 1
        assert_eq!(multiset_diff_count(&s(&["a"]), &s(&["a", "b"])), 1);
        // duplicate count matters: a,a vs a → 1
        assert_eq!(multiset_diff_count(&s(&["a", "a"]), &s(&["a"])), 1);
        // disjoint → additions + removals
        assert_eq!(multiset_diff_count(&s(&["a", "b"]), &s(&["c"])), 3);
    }

    #[test]
    fn m3u_remove_drops_matching_entries_and_their_extinf() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("list.m3u8");
        std::fs::write(
            &p,
            "#EXTM3U\n#EXTINF:1,A\nMusic/a.mp3\n#EXTINF:2,B\nMusic/b.mp3\n",
        )
        .unwrap();
        let mut remove = HashSet::new();
        remove.insert("a.mp3".to_string());
        assert!(device_m3u_remove_basenames(&p, &remove));
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(!after.contains("a.mp3"));
        assert!(!after.contains(",A"));
        assert!(after.contains("Music/b.mp3"));
        assert!(after.contains(",B"));
        // No matching entry → unchanged / false.
        let mut none = HashSet::new();
        none.insert("zzz.mp3".to_string());
        assert!(!device_m3u_remove_basenames(&p, &none));
    }

    fn s(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }
}
