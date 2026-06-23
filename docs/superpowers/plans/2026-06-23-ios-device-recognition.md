# iOS Device Recognition (Option A) Implementation Plan

> **For agentic workers:** execute task-by-task; steps use checkbox syntax.

**Goal:** Recognize Apple iOS devices (and generic PTP photo mounts) as
non-syncable and show an honest banner, instead of routing them through the
Android/MTP write path where copies silently fail.

**Architecture:** Add a `DeviceBackend::Unsupported` variant. Classify
`gphoto2://` mounts (iPad/iPhone PTP, and Android-in-photo-mode) as Unsupported,
built directly with no FUSE/capacity reads. `afc://` mounts ignored. A `NullIo`
backend guarantees no writes. The GTK detail view branches on the variant to
show a tailored banner and disable Sync/Scan.

**Tech Stack:** Rust core (`src/devices/`), GTK4 frontend (`frontends/gtk/window.rs`).

---

## Task 1: `Unsupported` backend variant

**Files:** Modify `src/devices/mod.rs`

- [ ] Add `Unsupported` variant to `DeviceBackend` with doc comment (Apple iOS /
  generic PTP photo mounts — connected, browsable photos only, never a music
  sync target).

## Task 2: `NullIo` backend

**Files:** Modify `src/devices/io.rs`

- [ ] Add `pub struct NullIo;` + `impl DeviceIo`: `list_audio_files`/`playlist_files`
  return empty; `copy_to_device`/`delete` return `Err(io::Error … Unsupported)`.
- [ ] `for_device` match arm: `DeviceBackend::Unsupported => Box::new(NullIo)`.
- [ ] Unit test: `NullIo` lists empty and copy errors.

## Task 3: Classify gphoto2 as Unsupported

**Files:** Modify `frontends/gtk/window.rs`

- [ ] `is_apple_device_uri(uri) -> bool` (lowercased contains "apple").
- [ ] `unsupported_device_banner(uri) -> &'static str` — Apple vs PTP text.
- [ ] In `mtp_raw_to_device`, before the cache check: if `uri` starts with
  `gphoto2://`, return a `Device` with `backend: Unsupported`, `read_only: true`,
  `fs_visible: false`, zero capacity, `mount_path = fuse_root` — no FUSE reads.

## Task 4: Detail-view branch

**Files:** Modify `frontends/gtk/window.rs` (select handler ~17677)

- [ ] Clone `dev_nofs_lbl` into the handler.
- [ ] Before the `fs_visible` check, branch on `backend == Unsupported`: set
  banner text via `unsupported_device_banner`, show banner, hide
  playlist/files/tracks, clear store, `counts` = "Not a music-sync device",
  disable Sync + Scan, hide `levelbar`, capacity = "Capacity unavailable".
- [ ] Ensure normal branches re-show `levelbar`.

## Task 5: Verify

- [ ] `distrobox enter dev-box -- sh -c 'cargo build && cargo test'` — zero
  warnings/failures.
- [ ] Manual: replug iPad, confirm banner + disabled Sync.

## Task 6: Commit (no push without explicit ask)
