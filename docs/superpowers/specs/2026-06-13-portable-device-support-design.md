# Portable / External Music Device Support — Design

Status: approved design, pre-implementation
Branch: `portable-music-player-device-support`
Date: 2026-06-13

## 1. Goal

Add a **Devices** section to the Media Library that lists connected external
storage holding music — USB sticks, SD cards, and any portable player that
mounts as a drive — and lets the user:

- browse the music already on a device,
- send files to it (from the ML files view, the active playlist, or any saved
  playlist — single, multi-select, or whole playlists),
- copy files from the device back into the library,
- keep ID3 tags (plus rating and play count) in sync between paired copies,
- delete files from a device,
- safely eject a device,

…with any number of devices connected at once, on **both** the GTK (Linux)
and macOS frontends.

## 2. Scope

**Phase 1 (this spec): mass-storage devices only** — anything that mounts as a
filesystem. This covers USB sticks, SD cards, most cheap MP3 players, and
phones/players in file-access mode. MTP-only devices (many Android phones) and
iPod (proprietary `iTunesDB`) are explicitly **future phases** built on the
same `DeviceBackend` abstraction; they are out of scope here.

**Out of scope (now and for the foreseeable future):**

- **Transcoding** on transfer. Files copy as-is; a device that can't play a
  format is the user's concern.
- Bluetooth "devices" — Bluetooth audio is A2DP streaming, not file storage;
  there is no filesystem to browse.
- Auto-fill ("fill device to N GB").

**Included beyond raw transfer:** rating/play-count sync, per-device smart-sync
rules (mirror selected saved playlists), free-space metering, eject, and
skip-already-present.

## 3. Platform reality: the Flatpak sandbox

The shipped Linux artifact is a **Flatpak** (`dev.sparkamp.Sparkamp`). Its
manifest today grants only `--filesystem=home:ro`, the app's own xdg dirs, and
`--talk-name=org.freedesktop.portal.Desktop`. That has three consequences the
design must respect:

1. Removable media (`/run/media/$USER/…`, `/media/$USER/…`, `/mnt/…`) is **not**
   under `home` and is invisible to the sandbox by default.
2. There is no `/dev`, `/sys`, or host mount namespace; raw enumeration via
   `/proc/self/mountinfo` + `/dev/disk/by-uuid` does not work.
3. `home` is read-only; the app already writes user files only through the
   **document portal** (which is why the library DB holds `/run/user/$UID/doc/…`
   paths — folders the user granted via the file chooser).

### 3.1 Access strategy

- **Device management** (enumerate, free space, identity, connect/disconnect
  signals, **eject**) goes through the host **udisks2** service on the system
  bus. The manifest gains exactly one new permission:
  `--system-talk-name=org.freedesktop.UDisks2`. udisks2 is present on all
  mainstream desktops including Bazzite, Silverblue, and Steam Deck. Eject =
  `Filesystem.Unmount` followed by `Drive.PowerOff` (true safe-remove).
- **File content** read/write uses a **per-device document-portal grant**:
  after udisks2 reports a device, the user confirms access to its mount path
  once via the file chooser portal; the grant persists like "Add Folder."
  udisks2 cannot be bundled — it is a host daemon — so this is the only
  sandbox-legal path to the bytes.

### 3.2 Graceful degradation when udisks2 is unreachable

Each capability falls back independently:

| Capability | With udisks2 | Fallback |
|---|---|---|
| Detect device | auto on plug-in | manual "Connect device…" portal pick |
| File read/write | portal grant | portal grant (unchanged) |
| Identity / pairing | volume UUID | marker file `.sparkamp-device-id` written to device root |
| Free space | udisks2 | `statvfs()` on the granted mount path |
| Eject | Unmount + PowerOff | `sync()` + advise the user to eject via their file browser |

The **marker file** also guarantees stable pairing even when no UUID is
available, so sync never depends on udisks2.

### 3.3 udisks2 failure diagnostics (friendly, actionable)

When udisks2 is unreachable the Devices section shows a terse one-line message
+ one action button, with specifics behind a collapsed **Details ▸**. The
variant is chosen by reading three local signals: our own `/.flatpak-info`
(is `org.freedesktop.UDisks2` in the granted `system-talk-name` list?),
`/etc/os-release` + `/run/ostree-booted` (distro + immutability), and the
D-Bus error name.

- **Permission off** (talk-name not granted; also any immutable distro):
  > ⚠ Can't access drives — Sparkamp needs permission to use the system disk service. ⟦Fix permissions…⟧ ⟦Retry⟧
  Details: in Flatseal, select Sparkamp → System Bus → add `org.freedesktop.UDisks2` ⟦Copy⟧, then Retry. *(Advanced: `flatpak override --user --system-talk-name=org.freedesktop.UDisks2 dev.sparkamp.Sparkamp`.)*
  **Fix permissions…** opens `appstream://com.github.tchx84.Flatseal` (Flathub web URL fallback) via the OpenURI portal; the software center shows Open or Install — we never detect or instruct installation.
- **udisks2 not installed** (traditional distro, service genuinely absent — never shown on immutable):
  > ⚠ Can't access drives — your system's disk service isn't installed. ⟦Open Software⟧ ⟦Retry⟧
  Details: install `udisks2` (some distros: `udisks`), then Retry. *(Advanced: `sudo systemctl enable --now udisks2`.)*
- **Eject blocked / unavailable** (polkit `NotAuthorized`, or udisks2 unreachable but device opened via portal):
  > ⚠ Couldn't eject — your system requires you to eject through your file browser. ⟦Retry⟧
  Details: right-click the drive in your file browser → Eject / Safely Remove; writes are already flushed.

All CLI lives only in Details. A "Copy diagnostics" affordance bundles the
relevant `/.flatpak-info` lines, `os-release` ID, and the raw D-Bus error for
bug reports. macOS (unsandboxed DMG, DiskArbitration) rarely fails and gets a
simpler equivalent (e.g. "volume busy — a file is still open").

## 4. Architecture

Device logic lives in the **core (Rust)**, matching the existing
core-first / FFI split. GTK calls it directly; macOS calls it through a new
`src/ffi/devices.rs`. New module `src/devices/`:

- `mod.rs` — `Device` descriptor (id, label, mount path, total/free bytes,
  read-only, connection state) and a `DeviceBackend` trait so MTP/iPod slot in
  later without touching the UI or transfer/sync code.
- `detect.rs` — enumeration + connect/disconnect events. Linux: a udisks2
  client (Rust `zbus`) over the system bus, filtering removable/external
  filesystems, surfacing `InterfacesAdded/Removed` as a stream. macOS:
  DiskArbitration. Detection is inherently platform-specific and is cfg-gated;
  the rest of the module is platform-neutral and path-based.
- `diagnostics.rs` — the `/.flatpak-info` / `os-release` / D-Bus-error
  classifier producing the messages in §3.3.
- `mass_storage.rs` — the phase-1 `DeviceBackend`: a mounted filesystem.
- `transfer.rs` — copy queue (to/from), Music/Artist/Album layout, `.m3u8`
  writing, skip-already-present, free-space guard, progress callbacks.
- `sync.rs` — pair tracking + tag/rating/playcount diff engine (§6).
- `marker.rs` — read/write `.sparkamp-device-id`.

**Detection events and transfer progress are pushed live** to the frontend:
the udisks2 signal stream and progress callbacks are marshaled to the GTK main
thread (macOS: DiskArbitration callbacks) so the Devices nav updates **in
place** — new device appears, ejected device disappears, progress bars advance
— without the ML window reopening. This mirrors the existing
`PLAYLIST_NAV_REFRESH_HOOK` pattern.

## 5. Data model (SQLite, additive migrations only)

Following the existing pattern (`CREATE TABLE IF NOT EXISTS` + pragma-guarded
`ALTER TABLE … ADD COLUMN`):

- `tracks.rating` — new `INTEGER` column (0–5). (`play_count` already exists.)
- `devices` — `id TEXT PRIMARY KEY` (volume UUID or marker-file id), `label`,
  `last_seen`, `smart_rules` (serialized per-device mirror rules). No
  `transcode_profile` (transcoding is out of scope).
- `device_sync_pairs` — the heart of sync. One row **only** for a file
  explicitly copied via Sparkamp (either direction):
  `device_id`, `device_relpath`, `library_path`, `baseline_tag_hash`,
  `baseline_rating`, `baseline_playcount`, `last_sync_at`. Keyed
  `(device_id, device_relpath)`. Coincidental same-named files never get a row,
  so they never sync.

## 6. Sync engine

On **Sync** for a connected device, for every pair where both files still
exist:

1. Hash the normalized ID3 fields + rating + playcount on each side; compare to
   `baseline_tag_hash`.
2. Only the device side differs → device is newer → write device tags into the
   library file. Only the library differs → reverse. **Both** differ → mtime
   tiebreak, newest wins. Neither differs → skip.
3. Collect all decisions, then **one confirmation**:
   *"<Device> has N updated songs, this computer has M updated songs. Sync all
   changes?"* On confirm, apply, then refresh each pair's baseline.

Sync compares **tags only**, never re-copies audio. Pairs whose file vanished
on one side are reported as "no longer present" and offered for unpairing, not
synced. The tag-hash baseline makes change detection robust against FAT/exFAT's
coarse (2 s) and timezone-shifted mtimes; mtime is only the both-changed
tiebreak.

### 6.1 Smart-sync rules

A per-device rule mirrors selected saved playlists. Evaluation (on connect or
on demand) is **add-only**: copy any of the rule's files missing from the
device, funneled through the same single en-masse confirm. If files have
**dropped out** of a mirrored playlist, they are NOT auto-deleted; instead a
**separate deletion-review prompt** lists each drop-out with a per-file
checkbox — every deletion is explicitly reviewed but can still be applied en
masse. Full-mirror (auto-delete) is not a default; add-only + review is the
behavior.

## 7. Transfers

- **To device:** from the files view, active playlist, or saved-playlist view
  (single / multi-select / whole playlist), via right-click "Copy to device ▸
  <device>" and as a **drag-drop target** on the device node. Files land under
  `Music/Artist/Album/Track - Title.<ext>` on the device; a relative-path
  `.m3u8` is written for whole-playlist copies so the device's player sees the
  playlist. Already-present files are skipped; a free-space guard blocks
  over-capacity transfers. Each copy creates a sync pair. Live progress shows in
  the device's nav row.
- **From device:** "Copy to Library ▸ <folder>" and drag device tracks into the
  library; creates pairs likewise.

## 8. Deletion (per the revised standing rule)

CLAUDE.md's Deletion Rule is updated: permanent disk deletion is allowed **only**
from the ML files view or the ML device view, **always** with explicit
confirmation; playlist removal is list-only; skins/plugins UI removal never
deletes from disk.

- **Device view:** right-click "Remove from device" → confirm → deletes the
  file on the device and drops the pair.
- **Library files view:** removing a file (single/multi) → a confirm dialog that
  also offers, per currently-connected device holding a paired copy, to delete
  that device copy (checkbox per device). The user's **local** library file is
  never deleted by this flow (only the Sparkamp-managed device copy); the
  explicit prompt satisfies the "confirmed" requirement. Disconnected devices'
  copies are left untouched.

## 9. UI

- **Devices** section in the ML left nav, below Playlists; one row per connected
  device with a small free-space bar; appears/disappears live. A diagnostics
  banner (§3.3) replaces the list when the device service is unreachable.
- Selecting a device shows its tracks in the same ColumnView the files/editor
  views use (unavailable files in the broken color; a "synced" indicator on
  paired files), with a header showing capacity, **Sync**, **Eject**, and a gear
  for smart-sync rules.
- macOS mirrors all of the above natively (SwiftUI), consuming the core via
  `src/ffi/devices.rs`.

## 10. Iterative delivery (each chunk builds, tests, and merges independently)

| # | Deliverable | Tested |
|---|---|---|
| 0 | `scripts/flatpak-dev.sh` (done) — build+run the Flatpak on the host | builds/runs on Bazzite |
| 1 | Schema: `rating`, `devices`, `device_sync_pairs` + CRUD | distrobox unit tests |
| 2 | udisks2 detection client + diagnostics classifier (headless) | distrobox unit (fixtures) + first on-host run |
| 3 | Devices nav section: live add/remove, free-space bar, eject, diagnostics banner | on-host plug/unplug/eject + pulled-permission banner |
| 4 | Per-device portal grant + browse device tracks | on-host |
| 5 | Copy to device: layout + `.m3u8` + skip + free-space + progress + drag/right-click + pairs | layout/m3u8 unit in distrobox; on-host copy |
| 6 | Copy from device → library | on-host |
| 7 | Sync engine: tag-hash + rating/playcount + tiebreak + en-masse confirm | heavy distrobox unit + on-host e2e |
| 8 | Deletions: device + library-removal device prompt | unit + on-host |
| 9 | Smart-sync rules: add-only + deletion-review checklist | unit + on-host |
| 10 | macOS parity: DiskArbitration + FFI + SwiftUI | DMG build |

## 11. Testing strategy

Two tiers. **Fast logic** (schema, transfer layout, `.m3u8`, sync diff
branches, diagnostics classification, marker file) is unit-tested in the
distrobox dev-box with temp dirs standing in for a device and fixture D-Bus /
`/.flatpak-info` / `os-release` inputs — no hardware, zero warnings, all tests
pass before completion. **Faithful sandbox + real-device** behavior (live
detection, real eject, portal grants, the diagnostics banner) is exercised by
running the actual Flatpak on the Bazzite host via `scripts/flatpak-dev.sh`.
Swift is not compile-checked on Linux; macOS is validated via the DMG build.

## 12. Manifest change

Add to `dev.sparkamp.Sparkamp.yml` finish-args (introduced with chunk 2/3):

```
- --system-talk-name=org.freedesktop.UDisks2
```

This single narrow permission unlocks detection, free-space, identity, and
eject. File content continues through the document portal (no broad
`--filesystem` grant). It is a Flathub-accepted permission for media apps.

## 13. Open risks

- Free-space accuracy through the portal fuse path may differ from the raw
  mount; chunk 4 validates `statvfs` on a granted path and falls back to
  udisks2's reported size.
- polkit policies on locked-down systems may require admin auth for eject;
  handled by the §3.3 eject-blocked message.
- FAT/exFAT lacks UNIX perms and has coarse mtime; mitigated by the tag-hash
  baseline and by treating mtime only as the both-changed tiebreak.
