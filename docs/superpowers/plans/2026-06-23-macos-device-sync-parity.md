# macOS Device-Sync Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development or superpowers:executing-plans to implement task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Bring the macOS SwiftUI frontend to parity with the GTK frontend's external-device support (this branch): detect removable volumes, browse them, copy library files onto them, two-way tag/rating/play-count sync with the both-changed conflict dialog, device playlists with playlist sync, capacity display, eject, and graceful recognition of non-syncable (iOS/PTP) devices.

**Architecture:** The macOS app is SwiftUI talking to the Rust core through a C FFI (`frontends/macos/src/lib.rs` re-exports `sparkamp::ffi::*`; cbindgen emits `sparkamp_bridge.h`). All device *logic* already exists, platform-neutral, in `src/devices/` (`plan.rs`, `sync.rs`, `browse.rs`, `transfer.rs`, `io.rs`, `marker.rs`) — only `detect.rs` (udisks2) is Linux-only. So parity is **not** a logic rewrite: it is (1) a thin JSON-over-FFI device API in `src/ffi/devices.rs` that drives the existing core, (2) Swift-side volume enumeration + eject (DiskArbitration), and (3) the SwiftUI device UI mirroring `frontends/gtk/window.rs`.

**Tech Stack:** Rust core + serde_json (already a dependency tree member via serde), C FFI, Swift/SwiftUI/AppKit, DiskArbitration.framework.

**Scope boundaries (parity, not more):**
- **In:** USB mass-storage sticks and SD-card readers (block volumes under `/Volumes`).
- **Out (document, don't build):** **MTP/Android** — macOS has no native MTP mount (needs Android File Transfer / bundled libmtp); deferred exactly as on the GTK side's out-of-scope note. **iOS/iPad music sync** — impossible on every platform (iOS has no filesystem-reachable music store); on macOS an iPhone/iPad never mounts under `/Volumes`, so it simply won't appear via volume enumeration. The Unsupported-device recognition that shipped for GTK (commit `14db2a7`) has no mac trigger today; keep the core `DeviceBackend::Unsupported` path serde-ready so a future ImageCaptureCore detector can light it up, but build no mac UI for it now.

---

## Reference: what the GTK frontend does (parity checklist)

Every item below must have a macOS equivalent. Source of truth: `frontends/gtk/window.rs` device code + `src/devices/`.

1. **Detection** — removable volumes appear in the Media Library sidebar under a "Devices" group.
2. **Overview page** — a card per connected device (label, capacity bar, song/playlist counts); clicking a card navigates to that device's detail page.
3. **Detail page** — title, fs-type · mount path, read-only badge, unsupported-fs (NTFS/exFAT) warning badge + tooltip, capacity text + bar.
4. **Capacity bar color** — identical across sidebar/overview/detail: blue "safe", **yellow < 15% free**, **red < 5% free** (driver: `set_levelbar_fullness`, thresholds on *free* fraction).
5. **Files view** — the device's audio files in a column view sharing the Media Library column config; **"Synced from" column** showing the paired library path (basename + full-path tooltip; "—"/"Not synced from this computer" when absent).
6. **Device playlists** — list of the device's `.m3u/.m3u8`, plus "+ New" to create a device-only playlist; selecting one filters the files view.
7. **Send / Copy to device** — copy selected library tracks under the flat `Music/<file>` layout, recording sync pairs; space-needed guard before copy.
8. **Sync** — two-way tag/rating/play-count sync via baseline model; **Sync button shows a spinner + goes insensitive during the device-comm delay** (`set_button_busy`).
9. **Conflict dialog** — fires only on genuine both-changed ID3 conflicts; per-song "keep computer / keep device", bulk buttons, per-field diff incl. artwork thumbnails (design: `docs/android-mtp-device-support.md` §6).
10. **Scan** — re-read tags/durations from the device on demand.
11. **Eject** — unmount/eject; disabled while a copy to that device is running.
12. **No-filesystem banner** — connected device with no readable storage shows a caution banner instead of empty lists (`fs_visible == false`).
13. **Deletion rule (CLAUDE.md)** — permanently deleting a file from disk is allowed ONLY from the ML file view or ML external-device view, ONLY after explicit confirmation. Removing from a playlist never deletes.

---

## File Structure

- **Create** `src/ffi/devices.rs` — JSON-over-FFI device API (the only new Rust file).
- **Modify** `src/ffi/mod.rs` — `mod devices;`, and add device handle/state fields to `SparkampCtx` if needed (see Task 3).
- **Modify** `src/devices/mod.rs` + `src/devices/plan.rs` + `src/devices/sync.rs` — add `serde::{Serialize, Deserialize}` derives to `Device`, `DeviceBackend`, `PlaylistSyncItem`, `TagConflictItem`, `FieldDiff`, `DiffValue`, `DiffKind`, and a serde-friendly `SyncPlan` DTO.
- **Create** Swift: `frontends/SparkampMac/Sources/DeviceService.swift` (FFI wrapper + Codable models + volume enumeration + eject), `DeviceListView.swift` (sidebar group + overview), `DeviceDetailView.swift` (detail page, capacity bar, files table, playlists), `DeviceConflictSheet.swift` (conflict dialog).
- **Modify** Swift: `SparkampModel.swift` / `SparkampModel+MediaLibrary.swift` (own the device list + selection state), `MediaLibraryWindow.swift` (mount the device UI in the sidebar), `frontends/SparkampMac/SparkampMac.xcodeproj/project.pbxproj` (add the new files), `frontends/SparkampMac/SparkampMac.entitlements` / `Info.plist` (removable-volume access).
- **Regenerate** `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` via the existing cbindgen build (`frontends/macos/build.rs`, `frontends/macos/cbindgen.toml`).

**FFI design rationale (JSON, not flat structs):** device structures (a Device list, a sync plan with per-pair actions, a conflict's `Vec<FieldDiff>` with artwork, playlist-sync items) are deep and variable-length. Marshaling them as `#[repr(C)]` arrays is brittle and verbose. Instead each device call takes/returns a UTF-8 JSON `*mut c_char` (freed with the existing `sparkamp_free_string`); Swift uses `Codable`. Artwork bytes are the one exception — accessed via a separate bytes accessor (Task 4) to avoid base64 bloat. This mirrors the GTK code's in-process struct passing with the minimum FFI surface.

---

## Task 1: Serde-serialize the core device types

**Files:** Modify `src/devices/mod.rs`, `src/devices/plan.rs`, `src/devices/sync.rs`. Test: same files.

- [ ] **Step 1: Add a failing round-trip test** in `src/devices/plan.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn device_json_round_trips() {
    let d = crate::devices::Device {
        id: "uuid-1".into(), label: "Stick".into(),
        mount_path: std::path::PathBuf::from("/Volumes/STICK"),
        fs_type: "exfat".into(), total_bytes: 1000, free_bytes: 400,
        read_only: false, ejectable: true, backend_id: "disk2s1".into(),
        backend: crate::devices::DeviceBackend::Udisks, fs_visible: true,
    };
    let j = serde_json::to_string(&d).unwrap();
    let back: crate::devices::Device = serde_json::from_str(&j).unwrap();
    assert_eq!(d, back);
}
```

- [ ] **Step 2: Run it, verify it fails** (`Device` not `Serialize`).
  Run: `distrobox enter dev-box -- cargo test --lib device_json_round_trips`
  Expected: FAIL (trait bound `Device: Serialize` not satisfied).

- [ ] **Step 3: Derive serde.** Add `use serde::{Serialize, Deserialize};` where needed and `#[derive(Serialize, Deserialize)]` to: `DeviceBackend` and `Device` (`src/devices/mod.rs`); `PlaylistSyncItem`, `TagConflictItem` (`src/devices/plan.rs`); `FieldDiff`, `DiffValue`, `DiffKind`, `TagState`/`SyncAction` as needed (`src/devices/sync.rs`). `serde_json` is already in the workspace lock; if not a direct dep, add `serde_json = "1"` to `Cargo.toml` `[dependencies]`. `PathBuf` and `HashSet` serialize out of the box.

- [ ] **Step 4: Run, verify pass.**
  Run: `distrobox enter dev-box -- cargo test --lib device_json_round_trips`
  Expected: PASS.

- [ ] **Step 5: Commit** `feat(devices): derive serde on device sync DTOs for FFI`.

## Task 2: A serde-friendly sync-plan DTO

The internal `device_sync_plan` returns engine types keyed by `MediaLibrary` rows. The FFI needs a flat, JSON-able plan the Swift side can render and echo back on apply.

**Files:** Modify `src/devices/plan.rs`. Test: same.

- [ ] **Step 1: Define the DTO** (fields `pub`, `#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]`):

```rust
pub struct SyncPlanDto {
    pub to_device: Vec<SyncPairDto>,   // LibraryToDevice auto actions
    pub to_library: Vec<SyncPairDto>,  // DeviceToLibrary auto actions
    pub conflicts: Vec<TagConflictItem>, // both-changed; needs the dialog
    pub bytes_to_copy: u64,
}
pub struct SyncPairDto { pub lib_path: String, pub dev_path: String, pub field_summary: String }
```

- [ ] **Step 2: Add `pub fn sync_plan_dto(lib: &MediaLibrary, dev: &Device) -> SyncPlanDto`** that wraps the existing `device_sync_plan` + `build_tag_conflicts`, projecting engine actions into the DTO. Reuse, do not reimplement, the decision logic.

- [ ] **Step 3: Add `pub fn apply_sync_plan_dto(lib: &MediaLibrary, dev: &Device, plan: &SyncPlanDto, conflict_choices: &[ConflictChoice]) -> (usize, usize)`** where `ConflictChoice { pub dev_path: String, pub keep: KeepSide }` (`enum KeepSide { Computer, Device }`, serde). Auto pairs apply unconditionally; each resolved conflict is converted to a `LibraryToDevice`/`DeviceToLibrary` and applied through the existing `apply_tag_pair` path; baselines refresh as today. Returns `(applied, skipped)`.

- [ ] **Step 4: Unit-test** `sync_plan_dto` against an in-memory `MediaLibrary` with one paired single-side-changed file (lands in `to_device`/`to_library`, `conflicts` empty) and `apply_sync_plan_dto` applying it. Run in dev-box, expect PASS.

- [ ] **Step 5: Commit** `feat(devices): flat SyncPlanDto for cross-FFI sync`.

## Task 3: FFI device API (`src/ffi/devices.rs`)

**Files:** Create `src/ffi/devices.rs`; modify `src/ffi/mod.rs` (`mod devices;`). Test: `src/ffi/devices.rs`.

Conventions (match the existing FFI files): `#[no_mangle] pub unsafe extern "C" fn`, opaque `*mut SparkampCtx`, input C strings via `CStr`, output JSON via `CString::into_raw` (freed by `sparkamp_free_string`), null/empty-string on error, never panic across the boundary.

The library handle comes from `ctx.media_library` (already populated by `sparkamp_ml_open`) — sync pairs live in that DB.

- [ ] **Step 1: Volume-in / device-out refresh.**

```rust
/// Swift passes a JSON array of enumerated volumes; core returns a JSON array
/// of Device. Swift owns volume *enumeration* (DiskArbitration/FileManager);
/// core owns identity (marker file), fs_visible, and the canonical Device shape.
#[no_mangle]
pub unsafe extern "C" fn sparkamp_devices_refresh(
    ctx: *mut SparkampCtx, volumes_json: *const c_char,
) -> *mut c_char
```

Input element: `{ "mount_path","label","fs_type","bsd_name","total_bytes","free_bytes","read_only","ejectable" }`. For each: set `backend = Udisks`, `backend_id = bsd_name`, derive `id` via `crate::devices::marker::ensure_marker(mount)` (UUID/marker fallback), `fs_visible = true`. Return `serde_json::to_string(&Vec<Device>)`.

- [ ] **Step 2: Browse.** `sparkamp_device_browse(ctx, device_json) -> tracks_json` — deserialize `Device`, build IO via `crate::devices::io::for_device`, list audio files, read each into `LibTrack` (`browse::read_device_track`), and attach the paired library path from `lib.sync_pairs_for_device` so Swift can render the "Synced from" column. Return JSON `[{track fields…, "synced_from": "…"|null}]`.

- [ ] **Step 3: Plan / apply.** `sparkamp_device_sync_plan(ctx, device_json) -> plan_json` (calls `plan::sync_plan_dto`); `sparkamp_device_apply_sync(ctx, device_json, plan_json, choices_json) -> result_json` (`{"applied":N,"skipped":M}`, calls `apply_sync_plan_dto`).

- [ ] **Step 4: Copy / playlists / delete.**
  - `sparkamp_device_copy(ctx, device_json, src_paths_json) -> result_json` — flat `Music/<file>` copy + record pairs (`transfer::copy_to_device` + `plan::record_pair`); returns copied/skipped/bytes.
  - `sparkamp_device_playlist_plan(ctx, device_json) -> json` and `sparkamp_device_playlist_apply(ctx, device_json, items_json) -> json` (wrap `device_playlist_sync_plan` / `apply_playlist_push` / `apply_playlist_pull`).
  - `sparkamp_device_delete_files(ctx, device_json, paths_json) -> i32` (count; wraps `device_delete_files`). **Caller (Swift) must have shown the confirmation** — document the deletion rule in the header doc-comment.

- [ ] **Step 5: Conflict artwork bytes accessor** (the non-JSON exception):
  `sparkamp_device_conflict_artwork(ctx, device_json, dev_path, side /*0=computer,1=device*/, out_len*) -> *mut u8` returning malloc'd PNG/JPEG bytes (freed by the existing image-free function used by `id3`/artwork FFI — reuse `sparkamp_free_bytes` if present, else add one). Mirrors `docs/android-mtp-device-support.md` §6 "artwork bytes accessors".

- [ ] **Step 6: Tests.** For each JSON entry point, a Rust test that builds a temp `Device` over a `tempfile` dir + in-memory `MediaLibrary`, calls the `extern "C"` fn with `CString` args, and asserts the returned JSON parses to the expected shape. Run in dev-box. (FFI string lifetimes: free every returned pointer with `sparkamp_free_string` in the test.)

- [ ] **Step 7: Commit** `feat(ffi): JSON device API driving core sync from Swift`.

## Task 4: Regenerate the bridge header

**Files:** `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` (generated), `frontends/macos/cbindgen.toml` (only if new types need exposing — JSON API uses only `c_char`/`u8`/`int`, so likely no config change).

- [ ] **Step 1:** Build the staticlib so cbindgen runs: `cargo build -p sparkamp_macos` (or the project's documented mac build). Confirm `sparkamp_devices_*` appear in the regenerated header.
- [ ] **Step 2: Commit** `chore(macos): regenerate bridge header with device API`.

> Note: the mac staticlib + Swift app build only on macOS. On this Linux dev box, verify Tasks 1–3 with `cargo test` in dev-box; Tasks 4–9 are verified by the engineer on a Mac with `xcodebuild`.

## Task 5: Swift DeviceService (FFI wrapper + volumes + eject)

**Files:** Create `frontends/SparkampMac/Sources/DeviceService.swift`. Modify `project.pbxproj`.

- [ ] **Step 1: Codable models** mirroring the JSON: `Device`, `DeviceTrack`, `SyncPlan`, `SyncPair`, `ConflictItem`, `FieldDiff`, `ConflictChoice`, `PlaylistSyncItem`. Match field names exactly to the Rust serde output (snake_case; set `CodingKeys` or a snake_case decoding strategy).

- [ ] **Step 2: Volume enumeration** (`func enumerateVolumes() -> [VolumeInfo]`): `FileManager.default.mountedVolumeURLs(includingResourceValuesForKeys:options:.skipHiddenVolumes)`; for each, read `URLResourceValues` (`.volumeNameKey`, `.volumeIsRemovableKey`/`.volumeIsEjectableKey`, `.volumeTotalCapacityKey`, `.volumeAvailableCapacityKey`, `.volumeIsReadOnlyKey`) and the BSD name via DiskArbitration (`DADiskCreateFromVolumePath` → `DADiskGetBSDName`). Keep only removable/ejectable volumes (skip the boot disk). Encode to JSON, call `sparkamp_devices_refresh`, decode `[Device]`.

- [ ] **Step 3: Eject** (`func eject(_ device: Device)`): `DASessionCreate` + `DADiskCreateFromBSDName(device.backendId)` + `DADiskUnmount`/`DADiskEject`. Run off the main thread; report errors. (No gvfs/udisks on mac — DiskArbitration is the whole story.)

- [ ] **Step 4: Thin async wrappers** for browse/plan/apply/copy/playlist/delete that call the FFI on a background queue (these touch the device filesystem and the SQLite library connection — keep them off the SwiftUI main thread exactly as GTK uses `gio::spawn_blocking`), free every returned C string, decode JSON, and hop back to `@MainActor` to publish results.

- [ ] **Step 5: Commit** `feat(macos): DeviceService FFI wrapper, volume enumeration, eject`.

## Task 6: Swift device list + overview

**Files:** Create `DeviceListView.swift`. Modify `SparkampModel.swift` (device state: `@Published var devices: [Device]`, `selectedDeviceBSD: String?`), `MediaLibraryWindow.swift` (sidebar "Devices" group), `project.pbxproj`.

- [ ] **Step 1: Poll** — a `Timer`/`Task` every ~2 s calls `DeviceService.enumerateVolumes` → `devices_refresh`, diffs, updates `@Published devices`. (No FUSE wedge risk on mac; statvfs is fast — the GTK MTP cache/shutdown guard is not needed here.)
- [ ] **Step 2: Sidebar group** — a "Devices" section listing each device with a small capacity bar; selecting one sets `selectedDeviceBSD` and shows the detail view. An "All devices" row shows the overview.
- [ ] **Step 3: Overview** — `LazyVGrid` of device cards (label, capacity bar, "N songs · M playlists"); **tapping a card selects that device's sidebar row** (parity item 2). Counts come from `sparkamp_device_browse` / playlist plan (cache per device; refresh on select).
- [ ] **Step 4: Capacity bar component** `CapacityBar(used: Double)` — color by **free** fraction: `free < 0.05` red, `< 0.15` yellow, else accent/blue. One component used by sidebar, overview, and detail so colors always match (parity item 4).
- [ ] **Step 5: Commit** `feat(macos): device sidebar group + overview with capacity bars`.

## Task 7: Swift device detail (files, playlists, copy, scan, eject)

**Files:** Create `DeviceDetailView.swift`. Modify `project.pbxproj`.

- [ ] **Step 1: Header** — title, `fs_type · mount_path`, read-only badge, unsupported-fs warning badge (call a tiny FFI or replicate `device_fs_unsupported` in Swift for NTFS/exFAT; prefer FFI `sparkamp_device_fs_unsupported(fs_type)` to keep one source of truth), capacity text + `CapacityBar`.
- [ ] **Step 2: No-filesystem banner** — when `device.fs_visible == false`, show the caution banner in place of the lists and disable Sync/Scan (parity item 12). (Block volumes are always visible, so this is latent on mac but kept for shape parity and future backends.)
- [ ] **Step 3: Files table** — reuse `MLFilesTable`/`TableSupport` styling; columns track the shared ML column config; add the **"Synced from" column** from `DeviceTrack.synced_from` (basename + full-path tooltip; "—"/"Not synced from this computer" when nil) (parity item 5).
- [ ] **Step 4: Playlists** — list device `.m3u/.m3u8`, "+ New" device-only playlist, selection filters the files table (parity item 6).
- [ ] **Step 5: Copy to device** — multi-select library tracks → `sparkamp_device_copy`; show a space-needed check first; progress indicator while copying.
- [ ] **Step 6: Sync button** — calls `sparkamp_device_sync_plan` on a background task; **show a spinner + disable the button during the call** (parity item 8, mirrors `set_button_busy`); if `plan.conflicts` non-empty, present the conflict sheet (Task 8), else apply directly via `sparkamp_device_apply_sync` and report counts.
- [ ] **Step 7: Scan** — re-browse the device and refresh rows (parity item 10).
- [ ] **Step 8: Eject** — `DeviceService.eject`; disabled while a copy to that device runs (track in-flight copies in the model) (parity item 11).
- [ ] **Step 9: Commit** `feat(macos): device detail view — files, playlists, copy, sync, scan, eject`.

## Task 8: Swift conflict sheet

**Files:** Create `DeviceConflictSheet.swift`. Modify `project.pbxproj`. Design source: `docs/android-mtp-device-support.md` §6.

- [ ] **Step 1: Sheet** listing each `ConflictItem` (song title) with a per-song picker defaulting **unset**: "Keep computer" / "Keep device". Below each, a two-column field diff ("On this computer" / "On <device>") rendering only differing fields; `Artwork` rows show thumbnails fetched via `sparkamp_device_conflict_artwork` (free the bytes after building `NSImage`).
- [ ] **Step 2: Bulk** "Keep all computer" / "Keep all device" buttons; "Apply choices" disabled until every conflict has a choice (or a bulk button used); "Cancel" applies nothing for conflicts (auto pairs from the same run already applied — report skipped count).
- [ ] **Step 3: Apply** — encode `[ConflictChoice]`, call `sparkamp_device_apply_sync` with the plan + choices; refresh on completion.
- [ ] **Step 4: Commit** `feat(macos): two-way sync conflict resolution sheet`.

## Task 9: Deletion rule + entitlements + parity sweep

**Files:** Modify the ML file/device Swift views, `SparkampMac.entitlements`, `Info.plist`, `project.pbxproj`.

- [ ] **Step 1: Deletion** — a "Delete from device" action exists ONLY in the device files view (and ML file view), behind an explicit confirmation alert, calling `sparkamp_device_delete_files`. Removing a track from a device playlist edits only the `.m3u` (never deletes the file) (parity item 13, CLAUDE.md Deletion Rule).
- [ ] **Step 2: Entitlements** — if the app is sandboxed, add removable-volume read/write (`com.apple.security.files.user-selected.read-write`, and a security-scoped bookmark flow for `/Volumes/...`, or disable App Sandbox if the app already ships unsandboxed — check the current `SparkampMac.entitlements`). DiskArbitration needs no special entitlement for unmount of user volumes.
- [ ] **Step 3: Parity sweep** — walk the 13-item checklist above against the running app; fix gaps.
- [ ] **Step 4: Build/verify on a Mac** — `xcodebuild -scheme SparkampMac build`; manual test with a real USB stick: detect → copy → edit a tag on both sides → sync → conflict sheet → eject.
- [ ] **Step 5: Commit** `feat(macos): device deletion rule, entitlements, parity verified`.

---

## Verification

- **Rust (this Linux dev box):** Tasks 1–3, 6 (FFI) — `distrobox enter dev-box -- sh -c 'cargo build && cargo test'`, zero warnings/failures (CLAUDE.md mandatory gate).
- **Swift (engineer on a Mac):** Tasks 4–9 — `xcodebuild` + manual real-device run. The Linux box cannot compile the SwiftUI app or the staticlib for Apple targets.

## Self-review notes (coverage vs GTK)

Mapped each GTK device behavior to a task: detection→T5/6, overview+card-nav→T6, detail/badges→T7, capacity color→T6.4, files+synced-from→T7.3/T3.2, playlists→T7.4, copy→T7.5, sync+spinner→T7.6/T3, conflict dialog→T8, scan→T7.7, eject→T5.3/T7.8, no-fs banner→T7.2, deletion rule→T9.1. MTP/iOS explicitly out of scope with rationale (no mac mount). The GTK MTP-specific hardening (meta cache, FUSE shutdown guard) is intentionally **not** ported — there is no FUSE/gvfs layer on macOS, so its failure mode doesn't exist here.
