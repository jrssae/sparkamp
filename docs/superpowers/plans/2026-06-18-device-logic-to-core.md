# Device Logic → Core (Option 1) Implementation Plan

**Goal:** Move the GTK-free device sync/plan/apply logic out of the 19k-line
`frontends/gtk/window.rs` into a new core module `src/devices/plan.rs`, making it
unit-testable without GTK and shrinking the frontend file by ~800 lines.

**Architecture:** Same binary crate, but `src/devices/` compiles on every
platform while `gtk_ui` is `#[cfg(target_os="linux")]`. Therefore core code must
not reference the frontend `AppState` type. Two groups of functions:

- **Group A — already pure** (operate on `MediaLibrary`/`Device`/`DeviceIo`/`std`):
  move verbatim; frontend keeps calling them unqualified via `use` imports.
- **Group B — coupled to `&Rc<RefCell<AppState>>`** only to reach
  `media_lib: Option<&MediaLibrary>`: move the body to core taking
  `lib: &MediaLibrary`; keep a thin same-signature **shim** in `window.rs` that
  borrows `state`, pulls `media_lib`, and forwards. Shims isolate the AppState
  coupling in the frontend and keep the delicate copy/sync call sites untouched.

**No behavior change.** Verification: `cargo build && cargo test` in distrobox
`dev-box`, zero warnings/failures.

---

## Group A (move verbatim, frontend uses via `use`)

`device_sync_id`, `canonical_lib_path`, `file_mtime` (private), `device_plan_fs`,
`device_sync_plan`, `device_playlist_sync_plan`, `multiset_diff_count` (private),
`build_tag_conflicts`, `device_m3u_remove_basenames`, `device_delete_files`,
`safe_playlist_filename`, `device_fs_unsupported`, structs `PlaylistSyncItem`,
`TagConflictItem`.

## Group B (decouple `state` → `lib`; frontend shim keeps old signature)

`recorded_relpath` (was `device_recorded_relpath`), `device_plan_one`,
`record_pair` (was `device_record_pair`), `linked_library_playlist`,
`apply_tag_pair`, `apply_device_sync`, `update_playlist_baseline` (private),
`apply_playlist_push`, `apply_playlist_pull`.

---

## Task 1: Create `src/devices/plan.rs`

- [ ] Add `pub mod plan;` to `src/devices/mod.rs`.
- [ ] New file with `#![allow(dead_code)]` (mirrors sibling device modules; the
  fns are dead on the macOS build where the GTK frontend is absent).
- [ ] Group A bodies copied verbatim; `pub(crate)` on everything the frontend
  calls; struct fields `pub(crate)`.
- [ ] Group B bodies with `state: &Rc<RefCell<AppState>>` replaced by
  `lib: &crate::media_library::MediaLibrary`, and every
  `state.borrow().media_lib.as_ref()` access replaced by direct `lib` use.
- [ ] Unit tests for the pure fns (`safe_playlist_filename`,
  `device_fs_unsupported`, `multiset_diff_count`, `device_m3u_remove_basenames`).
- [ ] `cargo build` — passes (window.rs still has its own copies; different
  module, no clash).

## Task 2: Frontend surgery in `window.rs`

- [ ] Delete Group A defs; add `use crate::devices::plan::{ ... };` so existing
  unqualified call sites resolve unchanged.
- [ ] Delete Group B defs; add thin shims (same name/signature) that borrow
  `state`, get `media_lib`, and forward to `crate::devices::plan::*`, returning
  the documented no-lib default (`None` / `false` / `(0,0)` / `(0,false)`).
- [ ] `cargo build && cargo test` — zero warnings/failures.

## Task 3: Commit
