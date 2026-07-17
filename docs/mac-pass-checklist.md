# Mac verification checklist — Send-to (phase 1) + Disc UX phase 2

Date: 2026-07-17 · Branch: burn-hardware-pass · ALL mac Swift on this branch
is BLIND (written on Linux, never compiled). This checklist drives the Mac
xcodebuild + manual/hardware pass. Preserved here from the SDD report (the
gitignored phase-1 checklist was lost — do not keep the only copy in
.superpowers/ again).

This is the driving document for the human Xcode/hardware pass. Phase-1 items are reconstructed from commits `2c19aa6`, `c5c4014`, and the current Swift source (their own checklist file was lost); phase-2 items are this task's new/changed surface.

### Build
- [ ] `xcodebuild` succeeds with zero errors/warnings against the updated `sparkamp_bridge.h` (new: `sparkamp_disc_default_meta`, `sparkamp_disc_mount_list`; changed: `sparkamp_disc_burn_job_start`'s job JSON, `sparkamp_disc_burn_job_poll`'s reply JSON already had `fraction` from Task 6/pre-11).
- [ ] Rust static lib cross-compiled for macOS actually contains the new symbols (`nm`/`otool -Iv` the archive, or just let the Swift link fail loudly if not).
- [ ] **Specifically verify `src/disc/detect.rs`'s `#[cfg(target_os = "macos")] mod platform` block compiles** — this entire block (including this task's new data-disc `mount_path` resolution) was never type-checked by the Linux dev-box build; only its cfg-neutral helper functions (`parse_mount_output`, `parse_drutil_status`, `data_disc_mount_path`) were.

### Phase-1: Send-to menu (commits 2c19aa6, c5c4014)
- [ ] Files view (Media Library) right-click → "Send to" shows, in order: Active Playlist, Saved Playlist ▸ (New Playlist… + each saved playlist), Disc Drive (direct item with exactly one drive, ▸ submenu with 2+), Removable Device (same 0/1/N rule) — entries absent entirely when the corresponding list (drives/devices) is empty.
- [ ] Files view toolbar "Send to ▾" button (multi-select) shows the same spec, `includeActive: true`.
- [ ] Saved-playlist editor (MLPlaylistEditor) row context menu: same "Send to" spec.
- [ ] Active-playlist (PlaylistView) row context menu: same spec but **`includeActive: false`** (no "Active Playlist" entry — the tracks are already there).
- [ ] Device detail view (DeviceDetailView) selected-file context menu: same spec via the SwiftUI `SendToMenu`.
- [ ] "Send to ▸ Disc Drive" from every one of the above actually lands in that drive's burn queue (not another drive's) and shows the "Queued N for burning on <label>" status line.
- [ ] "Send to ▸ Disc Drive" with an unreadable file shows the "Some files could not be read" alert (`model.burnUnreadableFiles`) listing exactly the unreadable paths, and readable files in the same batch still queue.
- [ ] "Send to ▸ Removable Device" copies correctly and only lists writable (`fsVisible && !readOnly`) devices.
- [ ] Per-drive burn queues are genuinely isolated: queue different files on drive A and drive B, confirm A's queue/artist/album fields never show B's data and vice versa.
- [ ] Ejecting/unplugging a drive with a nonempty queue drops that queue silently (`pruneBurnQueues`) — no leftover panel, no crash.
- [ ] "Clear List" empties the queue and resets the disc-artist/disc-album fields back to computed defaults.

### Phase-2: burn progress fraction (Task 6 FFI, Task 11 Swift bind)
- [ ] Burning on the Linux backend's counterpart behavior aside — on mac (drutil), confirm burn phases show the indeterminate spinner (drutil reports no percent) and never get stuck showing a stale/wrong percent.
- [ ] Erase phase: indeterminate spinner, no percent text.
- [ ] "Preparing i/N" phase (per-track WAV prep before an audio burn): confirm this DOES show a moving determinate bar (this phase's fraction comes from GStreamer position feed, computed in `run_job` regardless of platform) — verify the percent text and bar stay in sync and don't visually jump/reset oddly between tracks.
- [ ] Cancel button remains responsive and correctly placed whether the bar is determinate or indeterminate (layout didn't shift/clip).

### Phase-2: disc artist/album (Task 11)
- [ ] Burn panel shows "Disc artist"/"Disc album" text fields whenever the panel itself is shown (blank and non-blank writable media both), pre-filled with computed defaults (common artist from queued items' "Artist - Title" display lines, else "Various Artists"; album "Sparkamp Disc YYYY-MM-DD").
- [ ] Adding/removing queue items updates the *displayed* defaults live, UNTIL either field is hand-edited.
- [ ] Editing either field sticks (survives re-render, survives switching to another drive and back) until Clear List or a successful burn.
- [ ] Burning an audio CD on mac: confirm (expected, not a bug) the resulting disc has **no CD-TEXT** — drutil has no input for it. If this ever changes (a future drutil version, or a switch to a different mac burn tool), revisit `burn::burn_audio`'s doc comment and wire the sheet through.
- [ ] Burning a **data** disc: confirm the artist/album fields are visually present (harmless) but have zero effect on the burned disc.

### Phase-2: data-disc browsing (Task 11)
- [ ] Insert a burned/pressed data CD: confirm `sparkamp_disc_list_drives`'s `mount_path` becomes non-empty once macOS finishes auto-mounting (may take a moment after insert — the view should NOT show an empty file list forever; `.onChange(of: drive.mountPath)` should catch the mount landing).
- [ ] "Disc Files" section lists the audio files with correct Title (tag-derived display, falls back to filename), Duration (M:SS or "—" if unreadable), Size.
- [ ] Double-click a file: adds + plays per the app's replace/append + autoplay-on-add settings, same as any ordinary file.
- [ ] Context menu "Add to Library" (selection) and "Add All to Library" button: refuses with a clear status message when no library folder is watched; otherwise copies into the first watched folder with collision-safe renaming (burn two discs each containing "track.mp3" and confirm the second import doesn't clobber the first — expect "track.mp3" and "track (2).mp3").
- [ ] After "Add to Library", eject the disc and confirm the imported copies are still playable (they're independent files under the watched folder, not still pointing at the ejected mount).
- [ ] Context menu "Send to" on data-disc files reaches Active Playlist / Saved Playlist / Disc Drive / Removable Device correctly — **note**: unlike GTK, this does NOT exclude the currently-browsed drive from the "Disc Drive" submenu; confirm this is acceptable or file a follow-up to add the exclusion.
- [ ] A non-blank **rewritable** disc (e.g., a used CD-RW) shows BOTH the Disc Files browser above AND the burn panel below in the same view; confirm the layout doesn't clip/overflow vertically with a long file list AND a nonempty burn queue simultaneously visible (flagged as a layout risk in this task — no scroll wrapper was added around the combined content; verify or add one).
- [ ] Eject while Disc Files is showing: file list clears; re-inserting a disc in the same drive reloads correctly (no stale rows from the previous disc).

### Phase-2: auto-refresh (Task 11 — verified conceptually equivalent to GTK's fingerprint, not literally ported)
- [ ] Swap an audio CD for a different audio CD without navigating away from the drive's detail view: track list refreshes (via existing `.onChange(of: drive.toc)`).
- [ ] Insert a data disc while the drive's (empty-tray) detail view is already open: Disc Files section populates once macOS mounts it, with no manual navigation needed.
- [ ] Eject a data disc while its Disc Files view is open: file list clears promptly (via `.onChange(of: drive.mountPath)` going nil, not just the next poll cycle happening to fire).

### Phase-2: drag-to-drive (Task 11)
- [ ] Drag one or more files from the Files view (or a playlist) onto a Disc Drive sidebar row: navigates to that drive and queues the files (status line + queue update), same as using its "Send to ▸ Disc Drive" menu entry.
- [ ] Dragging onto a drive row does NOT accept a saved-playlist drag payload (only `.fileURL`, unlike the device row which also special-cases the playlist drag) — confirm this asymmetry is intentional/acceptable, or extend it to match if playlist-to-drive drag is wanted.
- [ ] Dropping a mix of readable/unreadable files behaves like the Send-to menu path (unreadable ones reported, readable ones queued).

### General regression pass
- [ ] Existing rip flow (unrelated to this task) still works — the FFI/model files touched here (`SparkampModel+Discs.swift`, `DiscService.swift`) also carry rip code; confirm nothing there regressed from nearby edits.
- [ ] Existing gnudb identify/edit-tags/submit flow unaffected.
- [ ] `sparkamp_disc_list_drives` payload size/shape didn't change in a way that breaks decoding on an old cached build (it's additive — `mount_path` merely gets populated more often now).

## Blind macOS Swift fixes (commit 4263ae6)

Two critical compiler/correctness issues fixed blind on Linux (no Xcode available):

1. **Compile error**: `startBurnJob` line 609 — added explicit `DiscMeta?` type annotation to the ternary expression `let meta = audio ? burnMeta(for: drive.id) : nil`. Swift cannot unify `DiscMeta` with bare `nil` without contextual type guidance.

2. **Stale disc-file list on fast mount change**: Added private property `discFilesPendingReload` and updated `loadDiscFiles` to defer one reload when the function is called while a load is in-flight. The guard now sets this flag instead of silently dropping the request; the completion block checks the flag and recursively calls `loadDiscFiles` for the current drive once the busy state clears. Prevents stale file lists when the OS rapidly unmounts/remounts a disc.

Verification: Rust gate `cargo build` (zero warnings) + `cargo test` (all 603 tests pass) confirm no accidental breakage in the core.

- [ ] Data-disc file list remains responsive and consistent during rapid mount/unmount cycles (specifically: verify the assertion at line 102 — "re-inserting a disc in the same drive reloads correctly").

## Phase-2b: burn UX bugs found in GTK live testing (2026-07-17) — verify/port on mac
Fixed on GTK+core; mac equivalents to check during the Xcode pass:
- [ ] **Unmount before burn (core, shared):** run_job now calls
      `disc::mount::unmount_for_burn(drive)` before erase/burn. On Linux it
      udisks-unmounts a mounted data disc (else cdrskin fails "SG_IO"). On
      mac it's a no-op assuming `drutil burn` self-unmounts — CONFIRM a
      data burn works when the disc is auto-mounted in /Volumes; if drutil
      fails, add a `diskutil unmount` in the mac arm.
- [ ] **DVD over-capacity gate:** GTK bug was capacity=0 for DVD (no ATIP).
      mac parses drutil free/used blocks — verify the data capacity meter
      goes red + blocks the burn when the queue exceeds a DVD's ~4.7 GB.
- [ ] **Burn queue multiselect removal:** GTK now allows selecting several
      queued rows and Remove/Delete clears all. Verify the mac burn queue
      (SwiftUI Table) supports multi-row selection + delete.
- [ ] **Burn progress overlay readability:** GTK card was translucent (osd
      style) — made opaque. Eyeball the mac overlay for contrast/readability.

## Phase-2c: CD-TEXT read + eject (2026-07-17) — mac verify
- [ ] **CD-TEXT read on unknown discs (GTK-only so far):** GTK now reads
      CD-TEXT off an audio disc with no gnudb match (cdrskin cdtext_to_v07t)
      and shows real track titles + an "Artist — Album" header. macOS uses
      drutil, which doesn't expose CD-TEXT the same way — decide whether to
      surface CD-TEXT on mac (DiscRecording can read it) or leave the mac
      disc view showing "Track N" for unknown discs. Core
      cdtext::{CdText, parse_v07t_readback, to_xmcd} is reusable; only the
      read source is platform-specific.
- [ ] **Eject unmount (Linux fix, verify mac path):** GTK eject failed
      "must be superuser to unmount" on a mounted data disc; fixed by
      udisks-unmounting first. macOS `drutil eject` — confirm it ejects a
      mounted data disc without a similar error (drutil usually handles it).
