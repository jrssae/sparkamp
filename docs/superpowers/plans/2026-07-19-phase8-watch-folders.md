# Phase 8 — F10 Watch Folders & Scan Behaviors (design-level plan)

> Read `2026-07-19-opus-handoff.md` first. USER DECISION: true filesystem
> watching, NOT interval polling (interval rescan is not built). Production
> scan seam facts in the handoff are load-bearing here.

**Goal:** Live library: filesystem watching picks up new/changed/removed
files; plus rescan-on-startup, auto-add played tracks, remove-missing
toggle, per-folder recurse, compact-on-rescan.

## Architecture

### Watcher (new `src/watch.rs`, core)

- Dependency: `notify` crate (recommended v6/v7 + `notify-debouncer-mini`)
  — NEW DEPENDENCY, flag in the phase's first commit; core-side so TUI
  benefits too (gio FileMonitor rejected: ties core to glib).
- One recursive watcher per library folder (honoring per-folder recurse,
  below). Debounced (2 s) event batches → classified actions:
  - Created/Modified audio file (extension filter = the scanner's set) →
    targeted `upsert_track` (+ fast-insert first if row absent, matching
    the production seam) on the DB thread.
  - Removed → mark-broken (existing behavior) or delete row when the
    remove-missing setting is ON.
  - Renamed → treat as remove+add (notify gives both or a rename pair —
    handle both shapes).
- **Self-write suppression (CRITICAL):** Sparkamp writes tags
  (`write_tag_fields`), artwork cache, and RG write-back (phase 4) — those
  events must not trigger re-scan storms or fight the editor. Maintain a
  short-lived suppression set: paths Sparkamp wrote in the last N seconds
  (register at every write site via a small core hook — one function,
  called from write_tag_fields/write_extra_frame/folder-image writes).
  Cache-dir events: ignore by path prefix.
- Frontend wiring: watcher emits on a channel; GTK drains on the main loop
  and patches ML rows (reuse the scan-completion refresh callbacks); mac:
  watcher runs in core — events reach mac via the existing scan-progress
  polling/refresh FFI (verify the pattern the scan uses; mirror it). TUI
  refreshes its ML view on its tick.
- Watcher lifecycle: start on ML open (or app start if ML loaded),
  rebuild on folder add/remove, stop on shutdown (join cleanly —
  DEVICE_IO_SHUTDOWN-style flag exists as a pattern).

### Settings (Settings → Media Library; config `media_library.*`)

1. `rescan_on_startup: bool = false` — run `scan_all_folders` (background)
   at app start.
2. Watching itself: always-on when folders exist? NO — toggle
   `watch_folders: bool = true` (replaces Winamp's interval option;
   default ON since it supersedes polling).
3. `auto_add_played: bool = false` — play-start hook (phase 2 seam): if
   the path is outside the library and setting ON → fast-insert + upsert
   into a designated folder-less bucket (folder_id NULL — verify schema
   tolerates; else nearest ancestor library folder or skip when none.
   DESIGN NOTE: Winamp adds regardless; propose: add with folder_id NULL
   and ensure Files view shows folder-less rows — check the folder JOIN).
4. `remove_missing_on_rescan: bool = false` — rescan + watcher-remove
   delete rows instead of mark-broken (default keeps Sparkamp's gentler
   behavior).
5. Per-folder `recurse: bool = true` — schema: `folders` table gains
   `recurse INTEGER NOT NULL DEFAULT 1` (additive migration beside
   new_cols pattern but for `folders` — write the same
   pragma_table_info-guarded ALTER). `walk_dir` honors it; watcher
   configures recursive flag per watch accordingly. UI: checkbox per
   folder row in the ML folder list (GTK + mac).
6. `compact_on_rescan: bool = false` — VACUUM after a full rescan
   completes (DB thread, after batch commit).

## Automated tests

- Event classification: tempdir + real watcher — create/modify/remove
  audio + non-audio files, assert action stream (debounce makes this
  timing-sensitive: use generous waits, mark `#[ignore]`-style if flaky in
  the suite and cover classification via the pure classifier fn instead —
  SPLIT the design so classification (event→action) is pure and unit-tested
  without the OS watcher; OS-watcher integration gets one smoke test).
- Self-write suppression: registered path's event within window → dropped;
  after expiry → processed.
- Per-folder recurse: walk_dir honors flag (fixture tree with subdir).
- Remove-missing: OFF → row marked broken; ON → row gone.
- auto_add_played hook: outside-library path + ON → row appears (drive
  the hook directly); OFF → untouched.
- folders migration: recurse column added once, default 1.
- VACUUM smoke (runs without error, honors toggle).

## Manual test plan

1. With ML open: copy an mp3 into a watched folder → row appears within
   ~2-5 s, tags filled shortly after; delete it → row marked broken (or
   gone with remove-missing ON).
2. Edit tags in an EXTERNAL editor → row refreshes.
3. Save tags IN Sparkamp's editor → no scan storm / no duplicate refresh
   fight (self-write suppression).
4. Non-recursive folder: file in subdir ignored by scan AND watcher.
5. rescan_on_startup ON → fresh files found at launch without pressing
   Rescan.
6. Play a file from outside the library with auto-add ON → appears in
   Files view.
7. Compact: large delete then rescan with toggle ON → db file shrinks.
8. Watcher toggle OFF → none of the above live behavior; manual Rescan
   still works.
9. mac parity walk + checklist; TUI view refresh.

## Performance notes

- Debounce 2 s; batch DB writes per debounce window (100-row batching
  exists). A bulk copy of 1000 files must not run 1000 individual scans —
  the debouncer coalesces; process the batch through the normal scan-batch
  path with progress if large (>50 → route to the scan-status UI).
- inotify watch limits: recursive watches on huge trees can exhaust
  `max_user_watches` — on watcher-init failure, log + degrade to manual
  rescan (never crash the ML; house error rule), and surface one status
  line so the user knows watching is off.

## Open questions

1. auto-add bucket for folder-less rows: folder_id NULL vs skip-if-outside
   — needs a look at the Files-view folder JOIN; propose NULL + view fix.
2. Watch-toggle default ON — confirm with user (supersedes Winamp
   polling; OFF-by-default would be more conservative).
