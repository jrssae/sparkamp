# Android (MTP) Device Support — Design

**Status:** planned, not started.
**Goal:** Let Android phones (and other MTP/PTP devices) appear in the Media Library device list and participate in copy and sync, behind the existing `Device` abstraction.

Modern Android does not expose USB Mass Storage; it speaks **MTP**, surfaced by **gvfs** as a FUSE mount at `/run/user/<uid>/gvfs/mtp:host=…/`. The current detection (`src/devices/detect.rs`) only finds udisks2 *block* filesystems, so Android never appears today. Mass-storage players and SD readers already work and are unaffected.

The whole orchestration layer is reusable: flat `Music/<file>` layout, root `.m3u8`, the sidebar/track-view/send/sync UI, sync pairs keyed by canonical library path + device relpath, progress indicators. This is a **new detection + IO backend**, not a rewrite — plus one sync change and one new conflict dialog (which also improves non-MTP sync).

---

## 1. Device backend abstraction

`Device` currently assumes a POSIX `mount_path` and a udisks2 block-object `backend_id` (used for eject/PowerOff). MTP has neither.

- Add a `DeviceBackend` tag to `Device`: `Udisks` (today) or `Mtp`.
- Route per backend:
  - **eject:** Udisks → `Filesystem.Unmount` + `Drive.PowerOff` (today). Mtp → gvfs unmount of the `gio::Mount`.
  - **free space:** Udisks → `statvfs` (today). Mtp → gio filesystem info (below).
  - **browse/transfer:** Udisks → `std::fs` on the mount (today). Mtp → gio `gio::File` over the gvfs root (below).
  - **identity:** Udisks → IdUUID / marker file (today). Mtp → MTP serial from the `gio::Volume`; do **not** rely on the `.sparkamp-device-id` dotfile (MTP stacks may hide or reject dotfiles).

Keep `mount_path` populated with the gvfs FUSE root for MTP so existing path-relative logic (relpath, m3u entries) still works.

## 2. Detection (MTP path)

Separate from the udisks2 loop. Use gio `gio::VolumeMonitor` to enumerate mounted volumes; keep those whose root URI scheme is `mtp://` (or `gphoto2://` for PTP cameras — out of scope but same mechanism).

- Requires the `gvfs-mtp` backend installed, the phone in **File transfer / MTP** mode, and the user authorizing on the phone.
- Each MTP mount yields a root `gio::File` at `/run/user/<uid>/gvfs/mtp:host=…/`. Android exposes storage roots ("Internal shared storage", SD card) as top-level children; pick the writable internal storage (or let the user choose when more than one).
- `is_pseudo_mount` currently skips everything under `/run/user/` (portal + gvfs). The MTP path must **bypass that guard** — it does not go through the udisks2 loop, so this is just a note for future refactors, not a blocker.

## 3. Free space (gio, not statvfs)

gvfs FUSE mounts usually do not report `statvfs`, so the free-space guard would read 0 and misbehave. For MTP, query the gio mount instead:

- `G_FILE_ATTRIBUTE_FILESYSTEM_FREE` and `…_SIZE` on the root `gio::File`.
- Fall back to "unknown" (skip the guard, warn) if the device does not report it.

## 4. Browse / transfer (gio)

`browse.rs` / `transfer.rs` use `std::fs` (`read_dir`, `fs::copy`, `metadata`). Over the gvfs FUSE path these are slow and partially unreliable. For MTP, route through gio:

- **list:** `gio::File::enumerate_children` recursively.
- **copy (to device):** `gio::File::copy` (object creation — MTP supports this).
- **copy (from device):** `gio::File::copy` to the local target.
- Keep the flat `Music/<file>` layout and root `.m3u8`; only the IO primitive changes.

Implement as a small `DeviceIo` trait with `Posix` and `Gio` impls, selected by `Device.backend`, so `browse`/`transfer` call the trait instead of `std::fs` directly.

## 5. Sync engine adaptation

The engine already routes by **content change vs a stored baseline** (`baseline_tag_hash` / `baseline_rating` / `baseline_playcount` in `device_sync_pairs`), not by mtime — so direction detection already works over MTP (reading device tags via gio is fine). Two changes:

1. **LibraryToDevice apply on MTP.** Today: in-place `apply_tags(dev_path)`. MTP cannot edit in place, but the **local file already holds the desired tags**, so the MTP-native equivalent is **delete the device object + re-upload the local file** (reuse `copy_to_device` + one `gio` delete), then re-read it to refresh the baseline. End state is identical to an in-place write.
2. **DeviceToLibrary apply unchanged** — it writes the *local* file (normal POSIX in-place).

Do **not** switch to a raw "newest mtime wins" model: MTP mtime over gvfs is unreliable (often the upload time, not the edit time) and would misroute. The baseline/content model stays.

### The both-changed case

When *both* sides changed since the baseline, there is no automatic answer (true conflict) and mtime cannot be trusted on MTP. Today the engine silently tiebreaks by mtime. Replace that tiebreak with the **conflict dialog** below.

---

## 6. Conflict-resolution dialog

### When it appears (trigger condition)

The dialog appears **only for a genuine ID3-level conflict: a paired file whose tags changed on *both* the device and the computer since the last sync** (each side's current tag-hash differs from the stored baseline). This rule is the same for every backend, MTP or not.

- **Single-side-changed pairs always auto-apply, silently, with no dialog** — on USB sticks and every other device exactly as today. A normal sync where only one side was edited never shows the conflict view.
- The dialog is therefore rare: it only surfaces when the user (or another program) edited the *same song* in two places between syncs.
- For non-MTP devices this is the **only** change from today's behavior: the silent mtime tiebreak for the both-changed case becomes this dialog. Everything else about USB sync is unchanged.

When the plan contains one or more such conflicts, present the dialog so the user decides per song; the non-conflicting pairs in the same run apply automatically regardless.

### Per-field diff

For each conflicting pair, compute a **diff of only the fields that differ** between the computer file and the device file. Nothing identical is shown.

Comparable fields:
- **All ID3 text frames**: title, artist, album, album artist, year, genre, track #, disc #, comment, composer, original artist, copyright, URL, encoded-by, lyric, BPM.
- **Rating** (POPM stars 0–5).
- **Play count** (POPM counter).
- **Artwork**: compare embedded picture bytes by hash; if different, show both as thumbnails. (User called this the "image file".)
- **Date modified**: shown for context. Mark the device value as *approximate* on MTP (gvfs may report upload time). Informational only — it does not decide anything.

Diff model:

```rust
struct FieldDiff {
    label: String,        // "Title", "Rating", "Artwork", "Date modified", …
    computer: DiffValue,  // text, stars, "Image (240 KB)", timestamp, or "(none)"
    device:   DiffValue,
    kind: DiffKind,       // Text | Rating | PlayCount | Artwork | DateModified
}
// A pair with an empty FieldDiff list is NOT a conflict and never reaches the dialog.
```

### Layout

Two columns — **"On this computer"** vs **"On <device name>"** — one row per differing field. Identical fields are omitted entirely. Artwork rows render thumbnails side by side; a missing side shows "(no artwork)".

```
┌─ Resolve sync conflicts — 3 songs differ ───────────────────────────┐
│  Daft Punk — Aerodynamic                         [Keep computer ▼]  │
│  ┌───────────────┬──────────────────┬──────────────────────────┐    │
│  │ Field         │ On this computer │ On Pixel 8               │    │
│  ├───────────────┼──────────────────┼──────────────────────────┤    │
│  │ Comment       │ "Testing 9"      │ "PMEDIA NETWORK"         │    │
│  │ Rating        │ ★★★★☆            │ ★★★☆☆                    │    │
│  │ Artwork       │ [thumb]          │ [thumb]                  │    │
│  │ Date modified │ 2026-06-16 01:12 │ 2026-06-10 (approx)      │    │
│  └───────────────┴──────────────────┴──────────────────────────┘    │
│                                                                      │
│  … (next conflicting song) …                                         │
│                                                                      │
│  Bulk:  [Keep all computer]  [Keep all device]                       │
│                                   [Cancel]   [Apply choices]         │
└──────────────────────────────────────────────────────────────────────┘
```

### Choice model

The user picks, **per song, which whole file to keep** (the diff only informs the choice — it is not a per-field merge in v1):
- **Keep computer** → LibraryToDevice. POSIX: in-place tag write. MTP: delete + re-upload the local file.
- **Keep device** → DeviceToLibrary (write the local file's tags from the device's).
- Per-row dropdown defaults to **unset**; "Apply choices" is disabled until every conflict has a choice (or the user uses a bulk button).
- Bulk **Keep all computer / Keep all device** set every row at once.
- **Cancel** applies nothing (non-conflicting pairs from the same sync run still apply, or also cancel — pick one; recommend: apply the auto-resolved pairs, leave conflicts untouched, and report the count skipped).

After applying, refresh each pair's baseline from the now-agreed side (read the winning tag state, store `baseline_tag_hash` / `baseline_rating` / `baseline_playcount` / `last_sync_at`), so the next sync sees no change.

### Where it hooks in

- `device_sync_plan` already produces per-pair `SyncAction`s. Extend it to mark `Conflict` for both-changed pairs (instead of the mtime tiebreak) and to carry the `Vec<FieldDiff>` for each.
- `apply_device_sync` applies the non-conflict actions; conflicts are handed to the dialog, and the user's per-row choice is converted to `DeviceToLibrary` / `LibraryToDevice` and applied through the same code paths (with the MTP delete+reupload branch for to-device).
- GTK: a new modal built from the diff list. macOS: the SwiftUI equivalent (same diff data over FFI: a flat array of `(pair_index, field_label, kind, computer_value, device_value)` plus artwork bytes accessors).

### v2 (not now)

- **Per-field merge** (take comment from the phone, rating from the computer) — the diff model already supports it; just add per-row field checkboxes.
- A "review all changes" mode that shows the dialog even for single-side-changed pairs, for users who want to confirm everything.

---

## 7. Flatpak permissions

Add gvfs access (currently only `--system-talk-name=org.freedesktop.UDisks2` + `home:ro`):

- `--filesystem=xdg-run/gvfs` — read the FUSE mount.
- `--talk-name=org.gtk.vfs.*` (and the gvfs MTP backend) — so the sandbox sees the gio mounts.

## 8. Out of scope / platform notes

- **macOS:** Android over USB has no native MTP mount (needs the separate Android File Transfer app). The macOS approach is a **bundled-libmtp** `DeviceIo` backend — the macOS analogue of this doc's gvfs/gio path — specced as **Task 11** in `docs/superpowers/plans/2026-06-23-macos-device-sync-parity.md` (with mtp-ng as a fallback and the IOKit device-claim risk called out). The conflict dialog and the `Device` backend abstraction land cross-platform; only MTP *detection/IO* differs per platform (gio on Linux, libmtp on macOS). That same plan also covers macOS block-volume (USB/SD) parity; the core sync/plan/IO logic is already platform-neutral (`src/devices/plan.rs`, `sync.rs`, `browse.rs`, `transfer.rs`, `io.rs`).
- **iOS / iPad / iPhone:** music sync is impossible on every platform (no filesystem-reachable music store; the Music app uses a proprietary signed DB). On Linux these surface over gvfs as `gphoto2://` (read-only camera roll) + `afc://` (per-app sandboxes); the GTK frontend now recognizes them as `DeviceBackend::Unsupported` (`NullIo`) and shows an explanatory banner with Sync disabled instead of failing silently (shipped). On macOS an iPhone/iPad never mounts under `/Volumes`, so volume enumeration misses it; it is instead detected via ImageCaptureCore (`ICDeviceBrowser`) and shown with the same `DeviceBackend::Unsupported` banner — specced as **Task 10** in `docs/superpowers/plans/2026-06-23-macos-device-sync-parity.md`.
- **PTP cameras / gphoto2:** same gio mechanism, classified `Unsupported` (photo-transfer mode), not a sync target.

## 9. Phasing

1. **Backend abstraction** — add `DeviceBackend` + `DeviceIo` trait; move existing udisks2/`std::fs` logic behind it (no behavior change). Ships independently.
2. **Conflict dialog** — replace the both-changed mtime tiebreak with the diff dialog (fires only on genuine both-changed ID3 conflicts; single-side syncs stay silent). Ships independently; affects USB sync only in the rare conflict case.
3. **MTP detection + gio IO + free space** — Android shows up; copy/browse work.
4. **MTP sync** — LibraryToDevice delete+reupload; baseline refresh.
5. **Flatpak perms + real-device testing** on an Android phone.
