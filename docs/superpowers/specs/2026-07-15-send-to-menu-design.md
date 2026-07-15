# "Send to" menu + per-drive burn queues — design

Date: 2026-07-15 · Status: approved by user · Origin: first hardware burn
session with the new Slimtype DS8A5SH; interactive GTK testing surfaced four
UX issues plus one crash.

## Problems observed (live GTK testing)

1. "Add to Burn List" is misnamed and drive-blind — with more than one burner
   there is no way to say which drive the files are meant for.
2. The action only exists in the Media Library files view; playlist detail
   views, device detail views, and the active playlist have no path to a
   burner.
3. Files whose duration the library has not read yet queue with unknown
   length, so the over-capacity gate undercounts and more audio can be queued
   than fits.
4. "Add to Playlist" (button and context-menu submenu) and the burn/device
   actions are separate ideas that should be one consistent "Send to" surface.
5. Crash (separate fix, lands first): RefCell double-borrow panic at
   `frontends/gtk/window/disc.rs:385` during burn-panel interaction —
   `refresh_cb` re-entered while a `shown_drive` borrow was held.

## Decisions (user-confirmed)

- **Per-drive burn queues.** Each drive owns an independent burn list;
  sending to Drive B queues onto B only; B's burn panel shows only B's queue.
- **Device send = immediate background copy** via `devices::io::copy_to_device`
  (skip-if-present), not the sync planner.
- **Scope: all three frontends this session** — core + GTK verified here;
  Swift written blind, flagged for a Mac xcodebuild + manual pass.
- **Full "Send to" submenu everywhere**, replacing the "Add to Playlist"
  submenu/button; playlist entries live under Send to ▸ Saved Playlist.
- **Partial adds:** in a batch, readable files queue; unreadable files are
  skipped and listed in one error dialog ("could not be read, not added").

## Design

### Core — `src/disc/burnlist.rs`

- `BurnQueues`: `HashMap<String /* drive id */, BurnList>`.
  - `queue(&mut self, drive_id) -> &mut BurnList` (creates empty on first use).
  - `remove_gone(&mut self, live: &[&str])` prunes queues whose drive left.
- `add_files(list, paths, meta_fn, probe_fn) -> AddOutcome`:
  - `meta_fn(path) -> (display, Option<u32> /* secs */, u64 /* bytes */)` —
    frontend supplies the library lookup.
  - Unknown duration → `probe_fn(path)` (production:
    `duration_probe::probe_duration`). Probe failure → path goes to
    `failed`, item is NOT added. Duplicates counted separately.
  - Pure; probe injected so the matrix is unit-testable without media.
- `AddOutcome { added, duplicate, failed: Vec<PathBuf> }` with
  `status_message()` / `failed_message()` for shared frontend wording
  (mirrors `RipOutcome`).

### FFI — `src/ffi/disc.rs` + hand-maintained header

- Burnlist symbols gain a `drive_id` parameter.
- New `sparkamp_burnlist_add_files(drive_id, paths_json) -> outcome_json`
  (meta + probe run core-side for mac).
- `sparkamp_bridge.h` updated by hand (no cbindgen).

### GTK — `frontends/gtk/window/`

- `util.rs`: `build_send_to_menu(...)` →
  - Active Playlist
  - Saved Playlist ▸ (reuses `build_add_to_playlist_submenu`; "New
    Playlist…" first)
  - Disc Drive — hidden at 0 drives; direct item at 1; submenu (per drive
    label) at >1
  - Removable Device — same 0/1/N rule
  - Drive/device lists come from the cached poll state; no fresh probes at
    popup time (drive-contention rule).
- Consumers (submenu in context menus; "Send to ▾" MenuButton replaces the
  "▶ Add to Playlist" button): files view, playlist editor(s), device detail
  view, active playlist (player.rs).
- Send → drive: meta lookup on main thread → `spawn_blocking` probes →
  idle-callback queues + status "Queued N for burning on <label> (M on the
  list)"; failures → modal listing unreadable files.
- Send → device: background copy thread, status "Copying i/N…", completion
  "Sent N to <device> (K skipped)", failures in the same dialog pattern.
- Burn panel binds to the shown drive's queue only.

### TUI — `frontends/tui/`

- Burn queue keyed by the selected drive (the TUI disc view is already
  per-drive, so `b` targets the shown drive — no picker needed).
  Probe-on-add with the same failure wording in the status line.
- Send-to-device is OUT OF SCOPE for the TUI here: the TUI has no device
  integration at all today (nothing under `frontends/tui/` touches
  `devices::`), so device sends there are a separate feature, not a menu
  change. Flagged as a follow-up, pending user interest.

### Mac — `frontends/SparkampMac/` (blind)

- Send-to menu mirroring the GTK structure (files list, MLPlaylistEditor,
  DiscDriveView, DeviceDetailView); per-drive queues through the new FFI.
  Needs a Mac xcodebuild + manual pass before it counts as done.

## Error handling

- Unreadable file(s): one dialog per send action, lists every failed path,
  those files never enter the queue.
- Unplugged drive: `remove_gone` drops its queue; existing disconnect banner
  logic unchanged.
- Device copy failure: listed per-file in the completion dialog; copy
  continues past individual failures.

## Testing

- Core unit: queue isolation across two drives, prune, `add_files` matrix
  (known/probed/failed/dup/mixed), outcome messages.
- GTK: 0/1/N menu-visibility logic extracted pure + unit-tested.
- Live: re-run interactive matrix with both drives (sr0 Slimtype DS8A5SH,
  sr1 MATSHITA UJ8C2) — per-drive queues, sends from every view, unreadable
  batch, device copy.

## Out of scope

- Sync-planner integration for device sends (immediate copy chosen).
- Multisession/append burning (unchanged deferral).
- Mac verification (separate Mac session).
