# Mac verification checklist — Send-to (phase 1) + Disc UX phase 2

Date: 2026-07-17 · Branch: burn-hardware-pass · ALL mac Swift on this branch
is BLIND (written on Linux, never compiled). This checklist drives the Mac
xcodebuild + manual/hardware pass. Preserved here from the SDD report (the
gitignored phase-1 checklist was lost — do not keep the only copy in
.superpowers/ again).

This is the driving document for the human Xcode/hardware pass. Phase-1 items are reconstructed from commits `2c19aa6`, `c5c4014`, and the current Swift source (their own checklist file was lost); phase-2 items are this task's new/changed surface.

## Status — phases 0 through 11 are done (2026-08-03)

Verified on an M1 MacBook Pro (macOS 26.6, Xcode 26.6, arm64) against a real
library, not just a compile. The "BLIND" caveat is **retired for phases 0–11**.
Phase 9 was **closed with 14 of its 19 items accepted untested** (they need
discs and a second drive that pass did not have) and phase 10 with 9 of 32;
both name theirs under "Deferred" rather than ticking them. Phase 12 is still
blind and unchanged.

| Phase | Outcome |
|-------|---------|
| 0 / 1 | ✅ Passed as written, no code changes needed |
| 2 | ✅ Passed after 3 runtime bugs found and fixed |
| 3 | ⚠️ Closed with one known limitation — the OS Now Playing card never appears; a custom Touch Bar was added instead |
| 4 | ✅ Passed after a **design gap** was found: analysis results never reached playback at all |
| 5 | ✅ Passed after 3 review rounds — 6 defects, a merge of the Queue window into the Jump window, the missing Ctrl+Q hotkey, and a row-menu parity sweep |
| 6 | ✅ Passed after 5 defects and 3 follow-ons — a badge that could never clear, arrows no list could use, the LCD indicator resized off GTK's measured ink, and stop-with-fadeout built from scratch |
| 7 | ✅ Passed after 1 core defect — a reorder could move the playing highlight onto the wrong track whenever entries reached the playlist without an id — plus a status line that sat at the top of the window instead of the bottom |
| 8 | ✅ Passed after 5 defects — the whole feature was dormant unless the Media Library window happened to be open, a checkbox that was correct only by accident, and two pieces of work sitting on the main thread that belonged on a worker |
| 9 | ✅ Closed after 6 defects; the read path is **verified on hardware** (real drive + CD-TEXT disc, names on screen). Headed by a parser written for a `drutil` output format that does not exist — the real tool prints an XML plist, so before this pass no disc could ever have shown CD-TEXT on mac. Closed with 14 of 19 items accepted untested — the gnudb-known, no-CD-TEXT, partial-CD-TEXT, multi-language and two-drive cases needed discs this pass didn't have, and are listed as deferred rather than ticked |
| 10 | ✅ Passed after 2 defects — a percent threshold of 100% that could never fire, so the play was never counted at all, and a search box that kept its old query on reopen with the feature switched off. 23 of its 32 items verified on hardware; the 9 deferred needed a second device, a short track, or the optical drive that had been unplugged by then |
| 11 | ✅ Passed after 4 defects, headed by a **lossy FFI string decoder** — Foundation silently ate the BOM some tags carry, so any album whose title started with one opened to an empty track list. Plus a release year printed as "2,014", a sidebar that jumped to Files on drill-down, and a gallery with no search box. Two additions on request: an album search and a track-count badge on each cover |

Getting here first required unblocking the build itself. Every mac build had
been silently linking a **stale static library**: the Xcode "Cargo Build" phase
was failing (the gitignored `vendor/` tree was 92 crates out of date and 15
short after phase 8 added `notify`/`walkdir`/`symphonia`), and the link step
just reused the old `.a`. `cargo vendor vendor` fixed it. Anyone hitting
undefined `_sparkamp_*` symbols should suspect this before suspecting the FFI.

Two process notes worth keeping:

- `xcodebuild … | tee | tail` reports **exit 0 even on BUILD FAILED**. Always
  grep the output for `BUILD SUCCEEDED|BUILD FAILED` rather than trusting `$?`.
- A struct-layout mismatch across the FFI is worth *proving*, not eyeballing.
  Compiling `offsetof` in C against `size_of`/field offsets in Rust and diffing
  the two takes a minute and turns the scariest blind item into a settled one.

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
      **Decided/implemented in Phase 9 below (2026-07-28): mac now calls the
      shared `sparkamp_disc_read_cdtext` FFI (drutil-cdtext parse path), same
      as GTK's cdrdao path. See Phase 9 for the verification items and the
      DiscRecording-framework fallback if the drutil parse doesn't hold up
      on real hardware.**
- [ ] **Eject unmount (Linux fix, verify mac path):** GTK eject failed
      "must be superuser to unmount" on a mounted data disc; fixed by
      udisks-unmounting first. macOS `drutil eject` — confirm it ejects a
      mounted data disc without a similar error (drutil usually handles it).

## Phase-0 fixes: ID3 editor extended + passthrough frames (2026-07-17) — ✅ VERIFIED on hardware 2026-08-02
- [x] Mac ID3 editor's standard fields — Composer, Copyright, Encoded-by (and
      Original Artist, URL, Lyrics if exposed in the UI) — save via
      `sparkamp_tag_set`/`sparkamp_tag_save` and survive a close/reopen of
      the file (round-trips through `TagFields`, not silently dropped).
- [x] Customize panel: add a frame not covered by the standard fields (e.g.
      Publisher/TPUB, Key/TKEY, Mood/TMOO, Language/TLAN, ISRC/TSRC,
      Subtitle/TIT3) via `sparkamp_tag_set`, save, close, and reopen the
      file — confirm the value survives (passthrough via
      `write_extra_frame`, not just held in memory until close).
- [x] Setting a Customize frame, then reading it back via
      `sparkamp_tag_get` **before** saving, shows the just-set value (pending
      writes must win over what was loaded from disk).
- [x] Setting a standard field and a Customize frame together, then saving
      once: both persist (the extra-frame write path runs after the main
      `write_tag_fields` call and doesn't clobber it).

## Phase-0 fixes: playlist auto-scroll to current track (2026-07-17) — ✅ VERIFIED on hardware 2026-08-02 (D8)
- [x] Playlist scrolls to the playing row on every track change: auto-advance
      to the next track, `z`/`b` (prev/next), and double-click a different
      row to play it — the newly-current row should end up visible without
      manual scrolling.
- [x] While the same track keeps playing, manually scroll the playlist away
      from the current row (e.g. to look at a track further down) — confirm
      the view does NOT get yanked back to the current row on subsequent
      `updateNSView` passes (selection changes, tag edits, etc. must not
      re-trigger the scroll).
- [x] Scrolling to a very long playlist's last track (auto-advance reaching
      the final row) actually reveals that row — no off-by-one against
      `table.numberOfRows`.
- [x] Confirm `ActivePlaylistTable.Coordinator.lastScrolledIndex` compares
      against `model.currentIndex` (a stable playlist id), not a raw row
      number — reordering the playlist via drag should not cause a spurious
      re-scroll purely from a row-index shift while the same track plays.
- [x] Stop playback, scroll the playlist away from the (former) current
      row, then play that same track again — confirm the view scrolls back
      to it (the guard resets on stop, so replaying the same track re-fires
      the scroll instead of being treated as "already scrolled there").

## Phase-0 fixes: EQ frequency labels removal (D10) — ✅ VERIFIED on hardware 2026-08-02
- [x] EQ window shows 10 unlabeled sliders matching GTK, column spacing intact.

## Phase-1: ML technical columns + ID3 tech line (Task 7) — ✅ VERIFIED on hardware 2026-08-02
- [x] `xcodebuild` succeeds with zero errors/warnings against the updated
      `sparkamp_bridge.h` — `SparkampLibTrack` grew six trailing fields
      (`sample_rate`, `file_size`, `added_at`, `file_mtime`, `bitrate_mode`,
      `channels`); confirm the Swift `MLTrack.init(from:)` field reads still
      line up byte-for-byte with the Rust struct (no silent offset drift).
- [x] Files view column picker (toolbar icon, `MediaLibraryWindow.swift`)
      shows five new toggles below the existing "Last Played" entry: Sample
      Rate, Size, Date Added, File Modified, Mode — all off by default
      (bits 17–21 aren't in the default `columnMask`), confirm each toggles
      its column's visibility independently and the layout/divider looks
      right.
- [x] Column content, once shown: Sample Rate renders "44.1 kHz" style (or
      blank when 0); Size renders "N KB" under 1 MB, "N.N MB" at/above (same
      thresholds as GTK's `format_file_size`); Date Added / File Modified
      render as a friendly local "yyyy-MM-dd HH:mm" date, NOT the raw
      ISO-8601 string — GTK reformats **all three** timestamp columns
      (`last_played`, `added_at`, `file_mtime`) through the same
      `format_last_played` (`ml_columns.rs:385-394`), so mac's
      `MLTrack.addedAtDisplay` / `.fileMtimeDisplay` (new computed
      properties, same `ISO8601DateFormatter` → `DateFormatter` pattern as
      the existing `lastPlayedDisplay`) must produce output that reads the
      same as GTK's for the same timestamp — confirm the two frontends
      agree on a sample file; Mode shows "VBR"/"CBR" verbatim (mac does not
      lowercase it the way GTK's sort key does — the GTK *display* also
      keeps it as-is, only GTK's sort key is lowercased, so this should
      already match).
- [x] Click each of the five new column headers: table re-sorts via
      `sortDescriptorsDidChange` → `MLFilesTable.keyPathComparator` →
      `MediaLibraryWindow.reload()`'s `colName` switch → `mlFetchTracks`
      with the matching `sortCol` ("sample_rate" / "file_size" /
      "added_at" / "file_mtime" / "bitrate_mode") — confirm ascending AND
      descending both actually reorder rows (not just flip the header
      arrow).
- [x] These columns also appear in the Saved Playlist editor
      (`MLEditorTable.swift`, which reuses `MLFilesTable.specs` /
      `.cellContent` directly) — confirm they render there too, not just
      in the Files view.
- [x] Existing columns (Title through Last Played) are visually and
      functionally unaffected — spot-check a few sorts/toggles pre- and
      post-change.
- [x] ID3 editor: open a file that IS indexed in the library (e.g. via the
      Files view's "Edit / View ID3 Tags") — confirm a dimmed technical
      line appears under the field grid reading uppercase filetype ·
      bitrate ("320k" style, not "320 kbps") · sample rate · channels
      (mono/stereo/Nch) · duration (M:SS), " · "-joined, matching what
      GTK's ID3 editor shows for the SAME file (GTK's `tech_summary`).
- [x] ID3 editor: open a file NOT indexed in the library (e.g. a playlist
      entry from an unwatched folder) — confirm the line still shows at
      least the filetype (derived from the path extension client-side)
      and "-:--" for duration; bitrate/sample rate/channels should be
      blank. ACCEPTED DIVERGENCE (not an open question — no action needed):
      GTK's `tech_summary` shows ONLY "-:--" here (no filetype), because its
      filetype comes from the absent library row rather than the path. Mac
      shows the extra filetype text because deriving it from the path
      avoids adding a 7th field to the FFI struct purely to cover this rare
      edge case (untracked file opened directly in the ID3 editor). This is
      harmless extra information, not a bug — do not "fix" it by adding a
      filetype field to `SparkampLibTrack` unless a real product need shows
      up.
- [x] Saving ID3 tags on a file does not change/blank the tech line
      (technical fields are independent of tag fields; the editor closes
      ~0.4s after a successful save, so this is mostly a "no crash /
      no flicker to blank" check during that window).

## Phase 2 — 2026-07-20: now-playing FFI + artwork set/clear + ML art path (Task 12) — ✅ VERIFIED on hardware 2026-08-02
- [x] `xcodebuild` succeeds with zero errors/warnings against the updated
      `sparkamp_bridge.h` (new: opaque `SparkampNowPlaying` + its 10
      `sparkamp_now_playing_*` functions; new: `sparkamp_tag_set_artwork`,
      `sparkamp_tag_clear_artwork`; changed: `SparkampLibTrack` gained
      `artwork_path[512]` right after `has_art` — verify every existing
      field read by Swift after `has_art` still lines up positionally).
- [x] Now-playing panel (A1): on each track-change notification, call
      `sparkamp_now_playing_open`, read all fields, then
      `sparkamp_now_playing_close` — confirm it returns NULL gracefully
      when nothing is playing (panel should show its empty state, not crash).
- [x] Panel's curated tag rows (`sparkamp_now_playing_tag_count` /
      `_tag_label` / `_tag_value`) match GTK's A1 panel for the SAME file:
      same labels, same order, only non-empty fields shown, filename-stem
      fallback title when a file has no usable ID3 text at all.
- [x] `sparkamp_now_playing_tech_line` matches the ID3 editor's tech line
      for the same file (shared `tech_summary` under the hood).
- [x] `sparkamp_now_playing_artwork_path` resolves to the same file GTK's
      A1 panel shows (embedded APIC dump / folder image / library cache),
      and is "" when there is no art — panel shows its no-art placeholder,
      not a broken image.
- [x] `sparkamp_now_playing_has_play_count` / `_play_count` / `_last_played`:
      an indexed (media-library-scanned) track shows real stats; a track
      played from outside the library (e.g. Testing dir, ad-hoc file) shows
      the "not yet played" / no-stats state instead of 0 or garbage.
- [x] `sparkamp_now_playing_artist_wiki_url` / `_album_wiki_url` open the
      correct Wikipedia search page (percent-encoded, spaces as `%20`) for
      the current artist/album; empty tag → link is hidden/disabled, not a
      broken URL.
- [x] ID3 editor: setting a new cover image now calls
      `sparkamp_tag_set_artwork` + `sparkamp_tag_save` — confirm the saved
      file actually embeds the APIC frame (inspect with GTK or `id3v2 -l`)
      and the mac editor's art preview updates immediately after save.
- [x] ID3 editor: clearing/removing the cover now calls
      `sparkamp_tag_clear_artwork` + `sparkamp_tag_save` — confirm ALL
      embedded pictures are gone afterward, not just hidden in the UI.
- [x] Set-then-clear-then-set-again on the same file round-trips cleanly
      (no leftover/duplicate APIC frames after repeated saves).
- [x] Media Library table: add an art thumbnail/indicator column driven by
      `SparkampLibTrack.artwork_path` (fall back to `has_art` alone if no
      thumbnail rendering is wired yet) — confirm it populates for scanned
      tracks with cached art and stays blank for tracks without any.
- [x] Saved Playlist editor's track rows (same `SparkampLibTrack` source)
      also reflect `artwork_path` correctly, matching the Files/ML view for
      the same track.

**Deferred, not a gap**: no Rust unit test exercises
`sparkamp_now_playing_open` directly — building a full `SparkampCtx`
requires GStreamer init + a real `Player`, which the existing FFI test
suite does not construct anywhere; the function is a thin, already-covered
composition of `Playlist::current`, `MediaLibrary::track_by_path`,
`MediaLibrary::play_snapshot`, and `crate::now_playing::build_now_playing_info`
(all independently unit-tested in `src/now_playing.rs` and
`src/media_library/tests.rs`). The mac checklist items above are the
verification for the FFI wiring itself.

## Phase 2 — 2026-07-20: A1 panel, A6 window, ML art column, D14 art edit, w/k shortcuts (Task 13) — ✅ VERIFIED on hardware 2026-08-02

Swift files touched: `PlayerWindow.swift` (A1), `ArtworkWindow.swift` (A6),
`MLFilesTable.swift` (A2), `Id3EditorWindow.swift` (D14),
`SparkampModel.swift` / `SparkampModelTypes.swift` (state + `NowPlayingInfo`),
`SparkampModel+Keys.swift` (w/k), `SparkampModel+MediaLibrary.swift`
(`mlViewArtForPath` follow-mode fix), `KeyboardShortcutsView.swift` (w/k rows).
No FFI/bridge.h changes — Task 12's surface was already complete.

### Build
- [x] `xcodebuild` succeeds with zero errors/warnings. This task added the
      most speculative SwiftUI constructs of the phase — see "Unsure /
      eyeball" below before assuming a clean build means correct behavior.

### A1 — expandable now-playing panel
- [x] The marquee row (Row 1 of the info panel) now has a small chevron
      button at its right edge; clicking it toggles the panel exactly like
      pressing `w`, and the chevron flips (down = collapsed, up = expanded).
- [x] `playerExpanded` persists across relaunch via
      `UserDefaults["sparkamp.playerExpanded"]` (same mechanism as
      `playlistVisible`/`equalizerVisible`/`mediaLibraryVisible`) — restored
      in `SparkampModel.init()`, written in both the `w`-key handler, the
      chevron button, and `saveState()`.
- [x] Collapsed layout is pixel-identical to pre-Task-13 (nothing new renders
      when `playerExpanded == false` beyond the chevron itself).
- [x] Expanded: art (~100×100, clamped) appears on the left of the panel row,
      a data carousel on the right, page dots beneath the carousel when there
      is more than one page.
- [x] **Window resize**: confirm the player window's height actually grows on
      expand and shrinks back on collapse. This relies entirely on
      `.windowResizability(.contentSize)` (`SparkampMacApp.swift`) picking up
      the SwiftUI ideal-size change with NO extra `NSWindow` code (unlike
      GTK's manual `set_default_size` + `queue_resize` re-kick) — this is the
      single biggest "does the SwiftUI construct actually do what the doc
      says" bet in this task; if the window does NOT resize, the fix is
      almost certainly `.fixedSize()` somewhere upstream fighting it, not
      the panel code itself.
- [x] Visualizer (left column, mini bars/waveform/Granite) visibly grows
      taller when the panel expands (it relies on the same HStack-sizing
      side effect as the resize above — the left column has no explicit
      height, only `maxHeight: .infinity` on the `VisualizerView`).
- [x] Carousel pages match GTK's grouping/order for the same file: tag rows
      chunked 4-per-page (curated order), then Technical (tech line), then
      Stats (play count / last played — only if the track is library-indexed
      or has a last-played value), then Links (artist/album Wikipedia) — a
      page is omitted entirely when its data is all empty, not shown as a
      blank page.
- [x] Carousel auto-advances every 6 s via `Timer.publish`; clicking a dot
      jumps directly to that page. NOTE: unlike GTK, a manual dot click does
      NOT push out the next auto-advance (GTK's `jump()` doubles the dwell so
      a manual pick lingers) — the mac timer just keeps advancing on schedule
      regardless. Confirm this reads as acceptable UX or file a follow-up.
- [x] Switching tracks resets the carousel to page 0 (`onChange(of: trackKey)`
      where `trackKey == model.currentIndex`).
- [x] No artwork: the panel shows the dimmed app-icon + "No artwork
      available" placeholder (matches the A6 window's placeholder wording).
- [x] Clicking the panel's art (or its placeholder) opens/focuses the A6
      album-art window in follow-mode (same as pressing `k`).
- [x] Last-played timestamps in the Stats page render as local
      "yyyy-MM-dd HH:mm" (same formatting as the ML table's `lastPlayedDisplay`).

### A6 — standalone album-art window (singleton, follows current track)
- [x] `k` opens the window if closed, or brings it to front if already open
      (open-or-focus, not toggle — repeat `k` presses never do nothing).
- [x] While open in follow-mode, changing tracks (next/prev/EOS/jump) updates
      the displayed art live, including flipping to the "No artwork
      available" placeholder when the new track has none.
- [x] Opening the window via the ID3 editor's artwork thumbnail tap, or the
      Media Library's "View Art" action, shows that SPECIFIC track's art and
      does NOT get silently replaced by the currently-playing track's art a
      moment later (this is the `artworkFollowsPlayback` flag — verify it
      actually stays false for these two entry points and only becomes true
      via `k` / the A1 art tap).
- [x] Closing the window (Esc / red button) always resets follow-mode off,
      so the next `k` press cleanly re-enters follow-mode rather than
      inheriting stale state.
- [x] Fullscreen visualizer: `k` is inert while fullscreen is up (added to
      the same disabled-keys list as `p`/`i`/`u`/`d`, so it doesn't yank
      focus out of the fullscreen Space).

### A2 — Media Library artwork thumbnail column
- [x] The "Art" column in the Files view (`MLFilesTable`) shows a small
      (18×18) rounded thumbnail image for tracks whose `artwork_path` resolves
      to a loadable image, instead of just a "View" text link.
- [x] A track marked `has_art` but whose thumbnail failed to decode falls
      back to the pre-existing "View" text link (not a blank cell) — the
      pre-Task-13 behavior for that edge case is unchanged.
- [x] Tracks with no art at all still render a blank cell.
- [x] Clicking the thumbnail (or the "View" fallback) still opens the
      artwork viewer exactly as before.
- [x] **Performance**: scroll a large Files view (thousands of rows) with the
      Art column visible — `NSImage(contentsOfFile:)` runs directly in the
      cell-content builder with no caching/lazy-generation (unlike GTK's
      Task 8, which explicitly caches + backgrounds thumbnail generation via
      `thumb_path_for`). NSTableView only builds cells for visible rows, so
      this should be fine in practice, but confirm there's no visible
      scroll jank with a large, art-heavy library. If there is, the fix is a
      small `NSImage` decode cache keyed by path — not a redesign.
- [x] Same column in the Saved Playlist editor (`MLEditorTable.swift`, which
      reuses `MLFilesTable`'s specs/cellContent) — confirm the thumbnail
      renders there too (not separately touched this task; verify the reuse
      picked it up for free).

### D14 — ID3 editor artwork Browse / Clear
- [x] The artwork slot in the ID3 editor now ALWAYS shows something (a
      thumbnail, or a "No art" placeholder box) instead of collapsing to
      nothing when a file has no embedded art — confirm the left/right field
      columns' spacing looks right in both states (padding was hardcoded to
      0 now that the slot is never absent).
- [x] "Browse…" opens an NSOpenPanel restricted to images; picking a file
      updates the on-screen thumbnail immediately (before Save).
- [x] "Clear" blanks the thumbnail immediately (before Save) and is disabled
      when there's no artwork to clear.
- [x] Neither Browse nor Clear touches the file on disk until "Save" is
      pressed — `sparkamp_tag_set_artwork` / `sparkamp_tag_clear_artwork` are
      only called from `saveTag()`, mirroring how text-field edits are
      buffered in `fieldValues` and only pushed to the tag ctx at Save time.
- [x] Save with no Browse/Clear touch (`pendingArtworkPath == nil`) does NOT
      strip existing embedded art — confirm a file's art survives an
      edit-and-save that never touched the artwork controls.
- [x] Browse → Save → reopen the same file: new art is embedded (inspect
      with GTK's ID3 editor or `id3v2 -l`) and the mac editor shows it.
- [x] Clear → Save → reopen: all embedded pictures are gone.
- [x] Browse/Clear buttons are hidden for read-only and missing files (same
      gate as the Save button: `!isReadOnly && !fileMissing`).
- [x] Loading a different file (Customize… aside) resets any unsaved
      Browse/Clear buffer from the PREVIOUS file (`pendingArtworkPath = nil`
      in `loadTag()`) — confirm switching files via the editor's reload path
      doesn't leak a pending change onto the wrong file.
- [x] Not implemented for mac (scope call, see Task 9 GTK-only): the
      "Also write folder image" checkbox. GTK has it; mac's D14 spec only
      asked for Browse/Embed/Clear. Flag if this asymmetry should be closed.

### Shortcuts (3-file rule)
- [x] `KeyboardShortcutsView.swift`'s `sections` list now shows `w` → "Toggle
      now-playing panel (art, tags, links)" and `k` → "Open album-art window"
      under "Playlist & modes" (mac's closest analog to GTK's "View & Tags"
      section, which mac doesn't have — GTK's `d`/`u` rows also aren't listed
      anywhere in mac's shortcuts view; that's a pre-existing gap, not
      something this task introduced or was asked to fix).
- [x] `SparkampModel+Keys.swift`'s `handleRawKey` handles lowercase `w`
      (toggle `playerExpanded` + persist) and `k` (`openArtworkWindow()`) —
      both no-op with modifier keys held, matching every other single-key
      shortcut.
- [x] Both keys are inert while a text field has focus (covered for free by
      the existing `NSTextView` firstResponder guard) and while the
      Jump-to-Track overlay is showing (existing `jumpVisible` guard).

### Outcome (2026-08-02) — passed after three runtime bugs

All items above verified. Nothing was wrong with the FFI or the layout logic;
the three failures were all SwiftUI behaviours that only show up when running:

1. **The window grew but never shrank.** `.windowResizability(.contentSize)`
   cannot shrink a window past a greedy `Spacer` + `maxHeight: .infinity`
   child, so collapsing the now-playing panel left the window at its expanded
   height. Fixed with an explicit `setContentSize(cv.fittingSize)` re-fit on
   `playerExpanded` change.
2. **Marquee text ran under the expand chevron**, making both unreadable. A
   trailing padding reserve was not enough — the text still showed through.
   Fixed by giving the chevron an opaque `lcdBackground` chip with an
   `lcdBorder` outline, so it always reads as a button on top of the text.
3. **`k` would not close a focused artwork window.** The key monitor is
   app-wide, so `k` did reach the handler — it just re-focused the window
   instead of toggling. Fixed by closing when the key window is the artwork
   window.

### Unsure / eyeball (blind pass — resolved by the 2026-08-02 run)
- [x] `.windowResizability(.contentSize)` auto-growing the window on
      `playerExpanded` toggle with zero extra `NSWindow` code — the biggest
      "trust SwiftUI" bet in this task (see A1's resize item above).
- [x] `switch pages[safeIndex] { case .tags(...): ... }` written directly as
      `@ViewBuilder` content (mirrors the existing `switch nav { ... }` in
      `MediaLibraryWindow.swift`, so it should compile, but the carousel's
      case bodies are new).
- [x] `.task(id: info?.artworkPath ?? "")` for debounced image reload,
      `.onReceive(Timer.publish(...).autoconnect())` for the carousel timer,
      and `.onChange(of: pages.count)` for the page-count safety clamp — all
      standard SwiftUI, but this is their first use in this codebase; eyeball
      that the 6 s cadence feels right and the timer doesn't drift/pile up
      after the window has been open a long time.
- [x] `NowPlayingPanel` declares its own `@EnvironmentObject var model` and
      `@EnvironmentObject var themeManager` — confirm both are actually in
      scope where it's instantiated inside `PlayerWindow`'s body (they should
      be, since `PlayerWindow` itself receives both via the WindowGroup's
      `.environmentObject` calls in `SparkampMacApp.swift`, and environment
      objects propagate to any descendant view without re-declaring them at
      each level).
- [x] `Link("Artist on Wikipedia", destination: url)` — confirm it actually
      opens the system browser from inside this app's window context (no
      reason it wouldn't, but it's the first `Link` use found in this
      codebase's mac sources).
- [x] The ID3 editor's artwork slot padding (now hardcoded `0` instead of the
      old `artwork == nil ? 12 : 0` ternary) — eyeball the left-column
      alignment now that the slot is never absent.

## Phase 3 — 2026-07-21: Now Playing + remote commands (P3-T6) — ⚠️ CLOSED 2026-08-02 with a known limitation

New file `SparkampModel+NowPlaying.swift` (added to project.pbxproj: fileRef AA4…00A1 / buildFile AA5…00A1) + hooks in SparkampModel.swift (updateNowPlayingCenter from refreshCurrentTrackInfo + tick play-state change). Verify on hardware:

- [ ] Control Center / lock-screen Now Playing card shows title, artist, album, artwork, duration for the playing track.
- [ ] Card updates on track change (title/art) and on play/pause/stop (state/rate).
- [ ] Elapsed time advances (macOS extrapolates from rate); pausing freezes it.
- [ ] Hardware media keys (play/pause, next, previous) work with the app unfocused.
- [ ] AirPods play/pause tap + double-tap next / triple-tap previous act on Sparkamp.
- [ ] Control Center scrubber seeks; the app seek bar reflects it (and vice-versa — app seek elapsed may lag one card update, accepted).
- [ ] No-track / stopped → card clears (nowPlayingInfo nil, playbackState .stopped).

### Outcome (2026-08-02) — closed with a known limitation

**The OS Now Playing card never appeared on this machine.** Not on the lock
screen and not in Control Center, despite `MPNowPlayingInfoCenter` being fed
correctly (media type, artwork with an app-icon fallback, elapsed time, rate)
and `MPRemoteCommandCenter` handlers registering. Cause not established. The
user chose to **deprioritize** it rather than keep digging, so the boxes above
that depend on that card stay unticked — they are unverified, not passing.

What was done instead: a **custom AppKit Touch Bar**
(`TouchBarControls.swift`) with prev / play-pause / stop / next, a seek
scrubber, and repeat + shuffle toggles. Verified working. Two findings there
are worth remembering:

- **SwiftUI's `.touchBar` modifier is useless for a window root.** It only
  activates for a view in the *focused* responder chain, so it silently
  produced no bar at all. Providing the bar from the app delegate
  (`NSTouchBarProvider`) also never got consulted. What works is installing an
  `NSTouchBar` on the **key window and its contentView**. This was settled by
  adding temporary `NSLog` diagnostics and reading stderr from a direct binary
  launch — guessing had already burned two attempts.
- **`@Published` fires in `willSet`.** Subscribers that re-read the model see
  the *old* value, so the bar rendered exactly one state behind (tap repeat →
  UI shows One, bar still shows Off). Fixed with `.receive(on: .main)` on all
  four subscriptions.

Still unverified, and why:

- Lock-screen / Control Center card — deprioritized (above).
- AirPods gestures — no hardware available to test with.
- Control Center scrubber — not exercised, card never appeared.

Decision recorded: **on Stop, the Now Playing card keeps showing** (current
behaviour, deliberately unchanged).

**Unsure / eyeball (blind, no Xcode here) — resolved:**
- New Swift file compiles + is actually in the build target (pbxproj entries added by hand — confirm Xcode sees it; IDs AA4…00A1 / AA5…00A1 chosen unused). ✅ builds and links.
- `import MediaPlayer` on macOS + MPRemoteCommandCenter with no explicit audio-session entitlement (macOS doesn't require the iOS AVAudioSession; confirm commands fire).
- `MPMediaItemArtwork(boundsSize:) { _ in image }` closure returns the NSImage at any requested size (returns the full image regardless of size — verify it renders, not blank).
- Album extracted from `nowPlaying.tags` where label == "Album" (matches the core curated label).

## Phase 4 — 2026-07-22: ReplayGain (P4-T8) — ✅ VERIFIED on hardware 2026-08-02 (design gap found + closed)

Rust FFI (built + tested on Linux: 481 lib + 685 bin, 0 warnings) — 6 config
get/set pairs + a background analysis trigger, mirrored into
`sparkamp_bridge.h`. Swift edits are all in EXISTING files (no new source →
**no project.pbxproj changes needed**, unlike phases 2/3):
`SparkampModelTypes.swift`, `SparkampModel.swift`, `SparkampModel+MediaLibrary.swift`,
`SettingsWindow.swift`, `MLFilesTable.swift`, `MediaLibraryWindow.swift`.

Verify on hardware:

- [x] Settings → Playback → ReplayGain: "Use ReplayGain", Gain source
      (Track/Album/Automatic), "Prevent clipping", "Fallback gain" stepper all
      load current values on open and persist across a relaunch.
- [x] Toggling "Use ReplayGain" (or changing source/clip) while **stopped**
      reshapes the chain immediately; while **playing** it takes effect on the
      next track (engine defers — expected, matches GTK/TUI).
- [x] Loud vs quiet tracks even out in perceived volume with ReplayGain on;
      turning it off restores raw levels.
- [x] Settings → Media Library → ReplayGain: "Analyze ReplayGain" runs a
      background job; progress bar shows "Analyzing N/M…"; "Cancel Analysis"
      replaces the buttons while running and stops the job.
- [x] "Force Recalculate" reanalyzes every track (ignores stored values).
- [x] "Analyze new files on add/scan" and "Write ReplayGain tags to files
      (MP3 only)" toggles load + persist.
- [x] With write-tags ON, analyzing an MP3 writes REPLAYGAIN_* TXXX frames to
      the file (visible to other taggers); non-MP3 files silently keep DB-only
      values.
- [x] Media Library Files view → columns menu (tablecells icon) has a
      "ReplayGain" entry (off by default); enabling it shows a "ReplayGain"
      column with e.g. "-6.2 dB", empty for un-analyzed tracks.
- [x] Sorting by the ReplayGain column works (server-side "rg_gain" order).
- [x] Right-click one or more Files rows → "Calculate ReplayGain" force-
      analyzes the selection; the column updates when the job finishes;
      the item is disabled while an analysis is already running.

### Outcome (2026-08-02) — passed, after a design gap was found and closed

The blind Swift was fine. What the hardware pass exposed was bigger: **the
whole feature did nothing for playback.**

`rgvolume` reads `REPLAYGAIN_*` tags off the *decoded stream*. Analysis stored
gains in the library DB. Nothing ever connected the two — so a file analyzed
with write-tags off (the default, and the only possibility for non-MP3, which
Sparkamp cannot tag) played completely unnormalized while the UI happily
displayed its measured gain. The original phase-4 plan specified this
tag-based design deliberately, but it makes "Analyze ReplayGain" look broken
to anyone who has not read the plan.

Closed core-wide (commit `60d954a`), making the DB the authority the UI
already implied:

- **Harvest on scan** — existing `REPLAYGAIN_*` values are now read during the
  normal tag read (ID3 TXXX by description, Vorbis/MP4 via Symphonia), so a
  pre-normalized file costs no analysis pass. The upsert COALESCEs these the
  *opposite* way from other tag columns: the file wins when it has a value,
  but an untagged file must never wipe a gain Sparkamp measured itself.
- **DB drives playback** — `Player::load()`, the one funnel every frontend
  passes through, points `rgvolume`'s `fallback-gain` at the track's stored
  gain. That uses rgvolume's own precedence instead of fighting it. `load()`
  *takes* the value, so a call site that forgets to prime can only
  under-apply, never apply the previous track's gain to a different song.
- **Live re-apply on mac** — toggling ReplayGain/clip protection mid-track now
  reloads and seeks back, matching GTK. Needs a `pending_seek` on
  `SparkampCtx` drained by the tick: `load()` leaves the pipeline at Null and
  `play()` is async, so an inline seek is dropped.

Four smaller fixes from the same pass:

- `read_extra_frames` returned TXXX frames with **no label and no value**
  (`Content::text()` is `None` for user-defined text), so all four
  `REPLAYGAIN_*` frames rendered as blank rows labelled "TXXX". This is why
  ReplayGain was invisible in the Customize panel on *every* frontend.
- The ID3 editor gained an **editable** ReplayGain field. Saving writes the
  file tag and the library row together, independent of the write-tags
  setting. It must run *after* the post-save rescan, which would otherwise
  COALESCE a cleared value straight back.
- Existing users have a persisted ID3 field layout, so any newly added default
  field was invisible to them forever. The layout now merges missing defaults.
- RG completion called `mlFetchTracks()`, refetching with the default empty
  query and sort and silently dropping the user's active search and column
  sort. Now bumps `mlReloadTrigger`, which is what the window observes.

Still open (tracked, not blocking): **GTK and TUI have no DB-sourced
ReplayGain row** in their ID3 editors. They received the TXXX visibility fix,
so frames show once a file carries tags, but not the editable DB-backed field
mac now has.

**Unsure / eyeball (blind, no Xcode here) — resolved:**
- SparkampLibTrack field order vs the Rust `#[repr(C)]`. ✅ **Proven
  identical**, not eyeballed: `offsetof` in C vs field offsets in Rust both
  give size 3128 / channels 3080 / rg_track_gain 3088 / rg_track_peak 3096 /
  rg_album_gain 3104 / rg_album_peak 3112 / rg_analyzed 3120.
- `Stepper("Fallback gain: \(rgFallback, specifier: "%.1f") dB", ...)` — first
  interpolated-specifier Stepper title in this file; confirm it renders.
- RG progress polling was added to `SparkampModel.tick()` alongside the scan
  poll; confirm `rgRunning`/`rgDone`/`rgTotal` drive the Settings progress row
  and clear on completion, refreshing the column.
- Column bit 22 (ReplayGain) is beyond the previous max bit 21; `columnMask` is
  a plain `Int` (AppStorage) so bit 22 is fine — confirm the toggle persists.
- `sparkamp_rg_analyze_selection` takes an `int64_t *ids` array; Swift passes
  it via `withUnsafeBufferPointer`. Confirm large selections analyze correctly.
- ~~Known limitation: sort by ReplayGain treats un-analyzed tracks as 0.0 dB,
  so they interleave with reference-level tracks.~~ **Not true on mac** — the
  Files view sorts server-side, and the SQL pushes NULL gains to the end in
  both directions. Better than this note claimed.

## Phase 5 — Manual play queue (F8) — ✅ PASSED on hardware 2026-08-02

Rust FFI — 8 queue symbols in `src/ffi/queue.rs`, mirrored into
`sparkamp_bridge.h`. The queue lives in `ctx.queue`; the advance seam
(`sparkamp_nav_next` / `sparkamp_advance_after_eos`) drains it ahead of
shuffle/linear, so `next()` / `handleEOS()` → `refreshAll()` →
`refreshPlaylist()` renumber the badges automatically.

Swift edits are all in EXISTING files (no new source → **no project.pbxproj
change**): `SparkampModelTypes.swift` (PlaylistItem.queuePos + queueBadge),
`SparkampModel.swift` (queueVisible, refreshQueueBadges, queuePos in the two
playlist refresh sites), `SparkampModel+MediaLibrary.swift` (queuedItems +
queueToggle/Move/Clear/Shuffle/PlayNow), `PlaylistView.swift` (badge prefix +
"Queue / Dequeue" context item), `SparkampModel+Keys.swift` (`q`), `SparkampMacApp.swift`
(Queue window scene + Playback-menu item), `PlayerWindow.swift` (queueVisible →
open/dismiss), `JumpToTrackView.swift` (`QueueView`), `KeyboardShortcutsView.swift`.

### Corrections made during the review pass (2026-08-02)

The blind pass compiled and was structurally right; six defects were found by
reading it against the rest of the codebase, and all six are fixed:

1. **The queue was never pruned on mac.** `sparkamp_playlist_remove` /
   `_clear` (and the ML replace/append + dedupe bulk paths) mutate
   `ctx.playlist` directly without building a `Controller`, so they never
   reached `Controller::sync_queue_to_playlist` the way GTK and the TUI do.
   Removing a queued row left an id nothing resolved to: the Queue window's
   count outran its rows and Clear Playlist left a non-empty queue. Added an
   FFI-side `sync_queue_to_playlist` and wired it into all five sites.
   Six regression tests in `src/ffi/queue.rs` cover it.
2. **`QueueView`'s double-click would have disabled the whole button row.**
   It used `.onTapGesture(count: 2)` on a row inside `List(selection:)`, which
   swallows the click that selection needs — Up / Down / Remove are
   `.disabled(selection == nil)`, so they would never have enabled. Switched to
   `.contextMenu(forSelectionType:menu:primaryAction:)`, the idiom the jump
   window already uses and documents as the canonical one.
3. **`queuePlayNow` skipped most of the post-jump path.** It refreshed the
   playlist but not `currentIndex`, so the ▶ marker stayed on the outgoing row,
   and it never called `announceNowPlaying()` / `saveState()` /
   `setStopAfterCurrent(false)`. Now mirrors `jumpTo(index:)`.
4. **Closing the Queue window by its title bar wedged the `q` key.** Every
   other window resyncs its flag in `.onDisappear`; `QueueView` had none, so
   `queueVisible` stayed `true` and the next `q` read as "close" and did
   nothing visible. Added.
5. **`sparkamp_queue_play_now` left `last_known_duration` stale** — the one
   thing `sparkamp_playlist_jump` does that it didn't, so the seek bar would be
   sized by the outgoing track until the new pipeline reported its own.
6. **ReplayGain was never applied on automatic advance** —
   `Controller::advance_to_next_playable` loads and plays directly in both its
   queue and shuffle/linear branches and neither primed the gain, so the
   library value only ever reached playback on a manual jump or Play. This is
   a **phase-4 gap, not a phase-5 one** (it predates the queue; the queue
   branch merely copied the existing shape). Priming is now a
   `Controller::prime_gain_for_current` helper called from all three load
   sites. Worth re-testing as part of phase 4 as well as here.

Also added: a "Play Queue…" item in the Playback menu (`q`) — every other
window has one, and the queue only had the bare key.

### Round 2 — GTK-shape corrections after the first hardware pass

The first pass found two structural divergences from GTK, both fixed:

7. **The queue was a separate window; GTK makes it a mode of the jump window.**
   Merged: one `Window("Jump / Queue", id: "jump-to-track")` with a
   Jump / Queue radio row at the top, exactly like GTK's `jump_mode_row`. `j`
   opens it on Jump, `q` on Queue, and either key closes it when it is already
   showing that pane. `queueVisible` is gone; the pane is `jumpQueueMode` and
   visibility stays on `jumpToTrackVisible`. `QueueView` is now an embedded
   pane, not a window (its `.onDisappear` / `.onExitCommand` moved to the host).
8. **No enqueue hotkey.** GTK and the TUI both bind **Ctrl+Q** =
   queue / dequeue the selection; mac had only the context menu. Added in two
   places, because mac has no single owner of "the selection":
   - Playlist window: `SparkampTableView.keyDown` gained an `onQueueKey` hook
     (the app-wide monitor ignores modified keys, and the selection lives in
     the table).
   - Jump pane: a hidden `.keyboardShortcut("q", modifiers: .control)` button,
     the same trick the arrow keys already use there.

   Plain `q` stays a search character in the Jump pane, matching GTK's
   `!qmode.get()` guard — so switching Jump→Queue by keyboard isn't possible
   while the search field owns the keyboard; use the radio button. Going
   Queue→Jump with `j` does work, since the Queue pane has no text field and
   the key monitor stays live there.

`KeyboardShortcutsView` now lists all three (`j`, `q`, `⌃Q`).

### Round 3 — playlist row-menu parity

A side-by-side of the GTK and mac active-playlist row menus turned up five
differences. Three were resolved, two were kept as deliberate platform
differences:

- **Order** — GTK now follows the macOS order: Play, Send to ▸, View/Edit ID3,
  View/Search Lyrics, Enqueue / Dequeue, ─separator─, Remove. (GTK previously
  had Send-to last and Remove fourth.)
- **Separator before Remove** — added to GTK. GIO menus have no separator item,
  so Remove goes in its own `append_section`, the same trick `util.rs` already
  uses for the saved-playlist submenu.
- **One name for the ID3 editor** — it had *eight* labels across the two
  frontends ("View / Edit ID3", "View ID3 Tags", "View/Edit ID3 Info",
  "Edit / View ID3 Tags", "Edit Tags…"). All are now **View/Edit ID3**. The one
  exception is the mac Playback menu, which keeps a trailing ellipsis
  ("View/Edit ID3…") because every one of its siblings has one — macOS
  convention for a menu item that opens a window.
  The disc **tag-override** editor ("Edit Tags" on the disc header / disc view)
  is a different feature and was deliberately left alone.
- **Kept as-is:** GTK hides ID3/Lyrics on multi-select where mac disables them
  (AppKit menus want a stable shape), and GTK's glyph prefixes (▶ 🎵 📝 ⯈ ✕)
  stay GTK-only — emoji in an `NSMenu` is not a macOS idiom.
- **Already identical, untouched:** the whole Send-to submenu. Both frontends
  build it from the shared `send_to_spec` / `sendToSpec`, so the labels and the
  0/1/N drive-and-device flattening already matched.

> The GTK half of this round is **unverified** — `frontends/gtk` is
> `#[cfg(target_os = "linux")]` and its deps (`gtk4`, `zbus`) are in a
> Linux-only target block, so it cannot be compiled on the Mac. Un-gating it
> would require re-vendoring, which is what produced the stale-static-library
> trap above. The reorder is mechanical and `append_section` has a working
> precedent in `util.rs:695`, but it wants a Linux build before it ships.

### Confirmed on hardware (2026-08-02)

- [x] Right-click one or more playlist rows → "Enqueue / Dequeue" adds/removes
      them; the `[n]` badge appears/updates on the playlist rows immediately.
      *(Verified while the item still read "Queue / Dequeue" — only the label
      changed afterwards.)*
- [x] Badges renumber as the queue drains during playback (queued tracks play
      before shuffle/linear, then playback resumes from that position).
- [x] Queue pane: rows listed in order "1. Artist — Title"; **single click
      selects a row** (fix 2); Up / Down reorder the selected entry; Remove
      dequeues it; Clear empties; Randomize shuffles.
- [x] Double-click a Queue row → plays it now (dequeues + jumps + plays), and
      the playlist's ▶ marker moves to that row.
- [x] Queue survives shuffle toggling; a queued track still wins, then shuffle
      resumes.
- [x] Removing a queued track from the playlist drops it from the queue
      (badge disappears; the "N queued tracks" count decreases).
- [x] Clear Playlist empties the queue too.
- [x] Reorder the playlist by dragging → badges follow their tracks.
- [x] Jump pane shows `[n]` badges on matching rows.
- [x] **Phase-4 recheck (fix 6):** ReplayGain now applies on natural
      end-of-track advance. Confirmed by ear ("difficult to verify but it
      sounds like it matches expected volumes") — not instrumented.

### Rounds 2 and 3 — confirmed on hardware in a follow-up pass (2026-08-02)

The merged-window, hotkey, and menu changes, tested after they landed:

- [x] `q` opens the **Jump / Queue** window on the Queue pane; `q` again, Esc,
      **or the window's own close button** all close it — and `q` reopens it
      after each.
- [x] `j` opens the same window on the Jump pane; the Jump / Queue radio row
      switches panes; `j` while the Queue pane is up switches to Jump.
- [x] Playback ▸ Jump to Track… / Play Queue… open the same window on their
      respective panes.
- [x] **Ctrl+Q** on a playlist selection queues / dequeues it (same as GTK/TUI).
- [x] **Ctrl+Q** on a highlighted Jump-pane match queues / dequeues it; plain
      `q` there still types into the search box.
- [x] Row menu order reads Play, Send to ▸, View/Edit ID3, View/Search Lyrics,
      Enqueue / Dequeue, ─separator─, Remove.
- [x] The ID3 editor entry reads **View/Edit ID3** in every menu that opens it
      (playlist row, ML files, ML playlist editor, device detail) and
      **View/Edit ID3…** in the Playback menu.

### Still open — needs a Linux box

- [ ] **GTK:** the row menu matches that order and wording, and the separator
      above Remove renders. `frontends/gtk` is `#[cfg(target_os = "linux")]`
      with its deps in a Linux-only target block, so the round-3 GTK edits have
      never been compiled, let alone run. See the note above.

**Still unverified / eyeball:**
- The app-wide key monitor bails only while the **Jump** pane is up, so the
  search field keeps plain letters. In Queue mode the monitor is live, which
  also means transport keys (`z`/`x`/`c`/`v`/`b`) work there. Intended, but
  worth a look.
- `refreshQueueBadges()` mutates `playlistItems` only when a badge changed;
  confirm it doesn't churn SwiftUI re-renders during idle playback.

---

## Phase 6 — F9 shortcuts + dialog sweep — ✅ PASSED on hardware 2026-08-02

New keys wired in `SparkampModel+Keys.swift` (raw handler) and the app
`Commands` menu (`SparkampMacApp.swift`). Stop-after-current is an engine flag
reached over FFI (`sparkamp_get/set_stop_after_current`) mirrored into
`@Published var stopAfterCurrent`.

### Corrections made during the review pass (2026-08-02)

Five defects, found by reading the blind code against the rest of the
codebase. Two of them would have failed a checklist item outright.

1. **The badge could never clear itself.** `stopAfterCurrent` was write-only on
   the Swift side, but the flag is *consumed inside the engine*:
   `Controller::advance_to_next_playable` calls `take_stop_after_current()`, so
   after it fires, Rust says false and Swift still said true — the badge stayed
   lit forever and the next arming looked like a disarm. `tick()` now reads the
   flag back (publishing only on change, matching the surrounding idiom) and
   `refreshAll()` reads it alongside volume / repeat / shuffle.
2. **Picking a track in the Media Library or a disc view left the arming set.**
   `mlDoubleClickTracks`, `mlReplacePlaylistWith` and the disc add path call
   `sparkamp_playlist_jump` directly rather than going through the model's
   `jumpTo`, so the Swift-side clear missed them. Moved the clear into
   `sparkamp_playlist_jump` itself — the one seam every "play that track now"
   path funnels through, mirroring GTK's `AppState::play_current`. Regression
   test in `src/ffi/playlist.rs`.
3. **`↑ ↓` could not browse any track list.** The app-wide key monitor consumed
   both arrows as volume before the event ever reached an `NSTableView`, so the
   playlist, Media Library and every other list were keyboard-unbrowsable. GTK
   splits this deliberately (its `↑ ↓` live in the main-window key controller,
   not the shared `handle_key`), and mac now does too: the monitor hands the
   arrows back whenever the key window's first responder is a table.
4. **`m` / `n` / `Shift+N` fought the fullscreen visualizer.** Keys that open a
   window are suppressed while fullscreen is up (macOS yanks focus to the main
   Space to show it) — the list had `p i u d k` but not the three new keys, and
   an `NSOpenPanel` pulls focus exactly as hard as a window.
5. **`⌘S` / `⌘I` were nested inside SwiftUI `Menu`s** (phase 7's Add / Select /
   Sort / List bar landed on top of them). A `Menu`'s content is built lazily
   when it opens, so a `.keyboardShortcut` inside one is not reliably live
   before then; the `List` menu is also `.disabled` on an empty playlist.
   Added zero-size hidden buttons carrying both shortcuts — the idiom
   `JumpToTrackView` already uses — so the keys work either way.

Also: the "Stop After Current Track" menu item was in the **Window** menu;
moved to **Playback**, after Next, matching GTK's `z x c v b t` grouping. The
File group gained "Add Folder…" beside "Add File…".

Shortcuts-window sweep (the phase's actual deliverable — the dialog is supposed
to be the single source of truth): `u` (equalizer) and `d` (ID3 editor) were
both bound in the handler and **missing from the help window**; added. `⌃Q`
read "Queue / dequeue", now "Enqueue / dequeue" everywhere (mac, GTK, TUI). The
`↑ ↓` lines now state the player-window / focused-list split.

### Manual test plan

- [x] `m` toggles the Media Library window (open when hidden, close when shown).
- [x] `t` arms stop-after-current: a small stop-square appears on the
      play/pause/stop indicator next to the time index (NOT on the play button);
      the "Stop After Current Track" menu item (Playback menu) toggles the same
      state.
- [x] With `t` armed, the current track finishes → playback stops, badge clears.
- [x] `t` twice = toggles off (playback continues to the next track).
- [x] `t` armed with queued tracks → stops before the queue; next play resumes
      the queue.
- [x] Manual stop (`v`), next (`b`), prev (`z`), and jumping to another track
      (double-click a row / jump window) clear the arming + badge.
- [x] **Double-clicking a track in the Media Library** also clears the arming
      (defect 2 — this path bypasses the Swift transport helpers).
- [x] Pause then resume (`c`) KEEPS the arming + badge (must not clear).
- [x] `n` opens the file picker (add file[s]); `Shift+N` opens the folder picker
      (add folder) — same as the playlist bottom-bar Add ▸ menu.
- [x] `⌘S` saves the active playlist (same as the List ▸ Save Playlist item).
- [x] `⌘,` opens Settings; `⌘I` inverts the playlist selection.
- [x] **`↑ ↓` in the player window still adjust volume**, and `← →` still seek.
- [x] **`↑ ↓` in the playlist window browse rows** instead of changing volume;
      same in the Media Library file list (defect 3).
- [x] With the fullscreen visualizer up, `m` / `n` / `Shift+N` do nothing rather
      than yanking the app out of fullscreen (defect 4).
- [x] Keyboard Shortcuts window (`i`) lists every binding and each line is
      true — including the newly added `u` and `d` rows.

**Unsure / eyeball:**
- The badge is a 5 pt `Image(systemName: "stop.fill")` overlaid bottom-trailing
  on the 9 pt state icon in `PlayerWindow.infoPanel`, tinted `stateColor`.
  Confirm it reads as a badge rather than a smudge and doesn't crowd the time
  text; say so if it wants to be a point or two larger.
- `⌘,` is attached to the "Settings" command button (toggles `settingsVisible`).
  Confirm it opens the Settings window and doesn't collide with a system pref.
- Stop-after-current is NOT persisted (transient), matching GTK/TUI.

**Known divergence from GTK, left as-is:** arming `t` while *stopped* and then
pressing play keeps the arming on mac; GTK cancels it, because its `x` key
routes through `play_current`, which clears indiscriminately. Mac's behaviour
is the more useful of the two — flag it if you want them identical instead.

---

## Phase 6 follow-ons — ✅ PASSED on hardware 2026-08-02

Three items raised after the phase-6 review, built together.

### Player-window focus on activation

AppKit restores focus to whichever window was key when the app deactivated, so
returning to Sparkamp after any Media Library visit left focus on a table.
`AppDelegate` now anchors it on the player, the way Winamp's main window is the
anchor: `applicationDidBecomeActive` covers ⌘-Tab, and a deferred call from
`applicationDidFinishLaunching` covers launch (that notification fires *before*
SwiftUI restores the remembered auxiliary windows, so without it whichever one
is created last wins).

- [x] With the Media Library open and focused, ⌘-Tab away and back → the
      **player** window is key; single-letter shortcuts work immediately.
- [x] Quit with the Media Library open, relaunch → the player is key, not the
      restored Media Library.
- [x] The other windows still come forward as a group (they did before) rather
      than being left behind the player.
- [x] Judgement call worth confirming: this is an unconditional steal. Leave a
      half-typed Media Library search, ⌘-Tab away and back, and the cursor is
      gone from that field. Say so if you would rather it only fired when no
      Sparkamp window held focus.

### State glyph + badge sized from GTK

GTK's `.time-disp` class is on the *row box*, so its state glyph and its time
digits both inherit `font-size: font_size_large` (32) in a monospace family; the
glyph reserves 2 characters (`width_chars(2)`), the digits 6, and the badge is a
literal 16 px. macOS was rendering that glyph at **9 pt** with a 5 pt badge
against digits that were already 32 pt.

Copying GTK's 32/16 across produced an indicator ~60 % too big, because those
are *text glyphs*: `▶ ⏸ ⏹` ink only part of their em box, while an SF Symbol
inks nearly all of it. Measured ink at font-size 32, against the 23.6 pt ink
height of the digits beside them:

| | GTK nominal | GTK ink | × the digits |
|---|---|---|---|
| `▶` | 32 px | 19.4 | 0.82× |
| `⏸` / `⏹` | 32 px | 15.6 | 0.66× |
| badge `⏹` | 16 px | 7.8 | 0.33× |

SF Symbols ink 25.5–26.5 at 32 pt. At **20 pt** they ink 16.0–16.5 and a **10 pt**
badge inks 8.0 — GTK's figures, and GTK's exact half-size relationship between
glyph and badge. Sizes and slots are derived from `fontSizeLarge` at runtime
rather than hardcoded, so a skin that changes `--sp-font-size-large` scales with
it and cannot overflow the column.

That costs the LCD column 118 → **158 pt** inside a fixed 480 pt window, paid
for out of the right column (user's call, 2026-08-02) rather than by widening
the window:

- The time slot reserves **5** characters where GTK reserves 6. Every elapsed
  time and any remaining time under ten minutes renders at full size; longer
  ones (`-12:34`) scale down a few points instead of widening the column.
- The transient volume percentage moved from a child of the volume row to an
  overlay on the right end of the slider. It is invisible except for a moment
  after a volume change, so the width it was reserving is now the slider's.

Measured outcome: right column 361 → 321 pt, volume slider ~115 → **~111 pt**.

One latent bug went with it. The time text had `.fixedSize()` inside a 118 pt
frame, so it ignored the width it was offered and overflowed the column instead
of scaling — `minimumScaleFactor` could never fire and the leading `-` on a
remaining time was being clipped. It now sizes to its slot.

- [x] Play / pause / stop glyph reads at about two thirds the height of the
      digits beside it — bigger than the old speck, not competing with the time.
- [x] `t` badge sits in the second character cell, clear of the glyph rather
      than overlapping it (this is how GTK's lands), at half the glyph's size.
- [x] `12:34` renders at full size. Click the time to switch to remaining:
      `-9:59` is full size, `-12:34` scales down slightly rather than clipping.
      **Nothing is cut off in either mode.**
- [x] Volume slider is only marginally shorter than before and still easy to
      drag; the mode buttons on its right are unmoved.
- [x] Change the volume: the percentage fades in over the right end of the
      slider and back out, without displacing anything or covering the buttons.
- [x] Mini visualizer below fills the wider column cleanly, with no gap or
      overflow at the divider.

### Stop with fadeout (Shift+V)

New feature, not a port — **GTK never had one** (grepped the tree and
`git log --all`; the only "fade" was the Granite palette crossfade). Built core
first, then all three frontends.

Core is `Player::begin_fadeout` / `poll_fadeout` / `cancel_fadeout`, driven from
each frontend's existing tick loop. The ramp is wall-clock, not step-counted, so
the same fade takes the same time on GTK's 33 ms tick and the mac's 100 ms one.
Attenuation lives in its own `fade_factor` rather than in `user_volume`, so
restoring is one assignment and the user's chosen volume is never rewritten.
Four engine tests cover the ramp, the not-playing no-op, transport cancelling a
fade, and a track ending mid-fade. Length is `playback.fadeout_secs`, default
**3** (Winamp's own default is 5), clamped to 1–10.

- [x] `Shift+V` while playing → audio ramps down over 3 s, then playback stops.
- [x] Plain `v` is still an immediate stop.
- [x] Volume is back to normal afterwards — the next track plays at full level,
      and the volume slider never moved.
- [x] Pressing play / next / prev / picking a track mid-fade cancels it and
      restores full volume immediately (no attenuated playback).
- [x] A track that reaches its own end mid-fade advances normally.
- [x] `Shift+V` while paused or stopped does nothing.
- [x] Settings → Behavior → **Stop With Fadeout**: the stepper reads 3 s,
      changes persist across a relaunch, and a changed value is honoured by the
      very next `Shift+V`.
- [x] Playback menu carries "Stop With Fadeout"; the shortcuts window (`i`)
      lists `⇧V`.

### Still open after the phase-6 pass — needs a Linux box

Everything above was verified on macOS. The GTK side of the same work has never
been compiled: `frontends/gtk` is `#[cfg(target_os = "linux")]` and its `gtk4`
/ `zbus` dependencies live in a Linux-only target block, so it cannot be built
on this Mac. Un-gating it needs a re-vendor, which is what caused the
stale-static-library trap earlier in this project.

- [ ] GTK builds clean with the phase-6 changes: the `Ctrl+Q` help label, the
      `Shift+V` key arm in `handle_key`, the `poll_fadeout` hook in
      `AppState::poll_bus` and the tick that consumes it, and the new
      "Stop With Fadeout" spinner on the Behavior settings tab.
- [ ] GTK `Shift+V` behaves as it does on macOS, and its fade length follows
      the same `playback.fadeout_secs` setting.

---

## Phase 7 — Task 10: Winamp playlist menu bar + status line — ✅ PASSED on hardware 2026-08-02

### Correction made during the review pass (2026-08-02)

**A reorder could move the playing highlight onto the wrong track.**
`sort_by` / `reverse` / `randomize` find the playing track again afterwards by
its entry id, but several paths push straight into `playlist.tracks` rather
than going through `Playlist::add`, and those entries keep the id-0 sentinel:
the mac dedupe and Media Library bulk adds, GTK's drag-drop file adds, and
GTK's disc-track adds. With an unstamped playlist the lookup searched for id 0,
matched whichever unstamped row came first, and pointed `current_index` at it —
so the highlight jumped, and the next automatic advance continued from the
wrong place. Reachable on macOS without touching anything exotic: add files,
play one, sort.

The ops now stamp before reading (`stamped_current_id`), so they hold however
the entries arrived, and `repoint_current_to` rejects id 0 outright since it is
never a real entry. Regression test in `src/model.rs` builds its fixture by
direct `push` the way the bulk paths do — it fails without the fix. The
existing reorder tests all used `add`, which stamps, which is why this survived
the blind pass.

Also fixed while in there: `sort_by`'s comment claimed it precomputed a
lowercase key per track, but it called `sort_field` inside the comparator, so
every comparison rebuilt and re-lowercased two Strings. Now
`sort_by_cached_key`, which is what the comment described and is also stable.

Everything else read clean. All five status lines (active playlist, ML files,
playlist editor, device detail, disc files) go through one formatter that
matches core's byte for byte, each guards on the DISPLAYED rows so a filter
that hides the selection omits the clause rather than showing "0:00 selected",
and all three frontends reset shuffle history after a reorder.

### Round 2 — status-line placement (hardware pass, 2026-08-02)

The blind pass put the active playlist's status line at the TOP of the window,
in the slot the old count/duration header used. Every other status line in the
app — GTK's (`pl_root`: scroll → status → separator → button row) and all four
Media Library bars — sits below its list and above its controls. Moved to
match; the table is now the first element in the window, under the title bar.

- [x] Active-playlist status line sits below the table and above the
      Add/Select/Sort/List row, lining up with the Media Library's bars.


`PlaylistView.bottomBar` replaced the five flat buttons (Add Files, Add
Folder, Save, Remove, Remove All) with four SwiftUI `Menu`s — Add / Select /
Sort / List — over the same underlying actions plus the new phase-7 reorder
FFI (`sparkamp_playlist_sort/reverse/randomize`, wrapped as
`model.sortPlaylist(_:)` / `reversePlaylist()` / `randomizePlaylist()` in
`SparkampModel+Transport.swift`). The old count/duration header was replaced
by a single status line mirroring core `playlist_status_line`
(`src/playlist_status.rs`) via `PlaylistView.formatStatus`.

- [x] **Add** menu opens; "Add Files…" / "Add Folder…" behave exactly as the
      old buttons (same file/folder pickers).
- [x] **Select** menu opens (NOT disabled on an empty playlist — deliberately
      left enabled so its nested ⌘I keeps firing; only Sort and List are
      disabled-on-empty); "Select All" / "Select None" / "Invert Selection" set
      `selection` correctly against the currently loaded playlist.
- [x] After a Sort/Randomize/Reverse, the selection is CLEARED (the index-based
      `selection` set would otherwise highlight whatever tracks landed on the old
      rows) — matches GTK, which clears selection on rebuild.
- [x] **Sort** menu opens (disabled when playlist empty); Title / Artist /
      Album / Filename / Path each call `sparkamp_playlist_sort` with the
      matching `kind` (0–4) and the table re-renders in the new order.
- [x] Randomize / Reverse (below the divider in Sort) reorder the playlist;
      in all five sort cases AND Randomize/Reverse, confirm:
      - the currently PLAYING track (waveform icon row) stays the same
        logical track after reorder — core keeps `current` pointed at the
        same entry across the shuffle-history reset, so this should hold,
        but verify visually since Swift is blind here;
      - queue `[n]` badges follow their tracks to the new row positions
        (`refreshAll()` → `refreshPlaylist()` re-reads `queuePos` per index
        from the ctx after the reorder, so badges should track correctly).
- [x] **List** menu opens (disabled when playlist empty); "Save Playlist…"
      behaves exactly as the old Save button (same NSSavePanel flow);
      "Remove Selected" is disabled with an empty selection and removes
      exactly the selected rows; "Remove All" clears the playlist and the
      selection.
- [x] Status line reads `"N tracks · MM:SS total"` with 0 selected rows, and
      `"N tracks · MM:SS total · K selected · MM:SS"` once ≥1 row is selected —
      confirm singular "1 track" with exactly one row, and H:MM:SS rollover
      once total (or selected) duration reaches an hour.
- [x] `⌘S` (Save Playlist) and `⌘I` (Invert Selection) work with the playlist
      window key. Settled in phase 6 and verified on hardware there: the
      modifiers on the `Button`s inside the List/Select `Menu` content are
      backed by hidden zero-size buttons in `bottomBar`, because a `Menu`'s
      content is built lazily on open and a nested `.keyboardShortcut` is not
      reliably live before then.

**Unsure / eyeball (blind, no Xcode here):**
- `Menu`'s trigger/label styling: previously every bottom-bar control used
  `PlaylistControlButtonStyle` (rounded-rect, skin-tinted). `Menu` doesn't
  honor `.buttonStyle` the same way a plain `Button` does, so the four new
  triggers are plain `Text(...).font(vars.bodyFont)` labels inside a default
  system `Menu` (pull-down) appearance instead of the old boxed button look —
  eyeball whether this reads as visually consistent with the rest of the
  bar, or whether it needs an explicit `.menuStyle`/custom label treatment.
  `PlaylistControlButtonStyle` itself is left defined but now unused.
- Whole-menu `.disabled(model.playlistItems.isEmpty)` was added to
  Select/Sort/List (not explicitly specified) so an empty playlist can't
  open a menu whose every item would be a no-op — confirm this reads as
  correct UX rather than surprising (Add is never disabled, matching the
  old always-enabled Add buttons).

## Phase 7 — Task 3: status bar on the four Media Library views — ✅ PASSED on hardware 2026-08-02

`PlaylistView.formatStatus` (phase 7, `static` on `PlaylistView`) was lifted
into a free top-level function `playlistStatusLine(count:totalSecs:selected:)`
in a new `PlaylistStatus.swift`, byte-for-byte identical to the old body and
to core `playlist_status_line` (`src/playlist_status.rs`). `PlaylistView`'s
own status line now calls the free function instead of `Self.formatStatus`;
the old `static func` is gone (was the only copy). The same function is now
used at the bottom of all four Media Library list views, mirroring the GTK
`ml_status_bar`/`ml_status_bar_for` change:

- **Files view** (`MediaLibraryWindow.filesBottomBar`) — count/total from
  `model.mlTracks` (all rows in the table; there's no live search-filter
  narrowing this array — `searchQuery` re-fetches from the DB), selected sum
  from `selection: Set<Int64>` matched against `MLTrack.id`. Duration field:
  `MLTrack.lengthSecs` (`Double`).
- **Playlist editor** (`MLPlaylistEditor`, bar moved to sit directly BELOW
  the track `Table` and ABOVE the Save/Enqueue/Play button row — matches
  the active-playlist window's placement rather than GTK's literal
  bottom-of-view append order) — count/total from `sortedRows` (the
  currently-displayed, search-filtered + sorted rows — same rows the table
  renders), selected sum from `trackSelection: Set<Int>` matched against
  `MLEditingRow.id`. Duration field: `MLEditingRow.track.lengthSecs`
  (`Double`).
- **Device detail** (`DeviceDetailView.filesBottomBar`, already the last
  element in the view — unchanged position) — count/total from
  `sortedTracks` (playlist-chip-filtered + search-filtered + sorted, the
  rows `filesTable` actually shows), selected sum from
  `selection: Set<String>` matched against `DeviceTrack.path`. Duration
  field: `DeviceTrack.lengthSecs` (`Double`). The old separate "N files" /
  "N selected" texts were merged into the one status line; the destructive
  action button to its right is unchanged.
- **Disc drive — disc-files browser** (`DiscDriveView.dataDiscView`, new bar
  inserted directly below the file `Table`, above the "Add Selected/Add All
  to Library" button row — the audio-CD track list/`bottomBar` was
  deliberately NOT touched, matching GTK: `ml_status_bar_for` was only wired
  to the disc's DATA-file browser, never the audio-track table) — count/total
  from `model.discFiles` (no search filter on this list), selected sum from
  `discFilesSelection: Set<String>` matched against `DiscFile.path`. Duration
  field: `DiscFile.durationSecs` (`UInt32?`, nil treated as 0 seconds, same
  as GTK's `.unwrap_or(0.0)`). The header's old redundant "N file(s)" text
  was removed since the new bottom bar now shows count + duration +
  selection in one place.

- [x] Each of the four views shows `N tracks · MM:SS total`, directly below
      its list/table and above its control-button row, with nothing selected.
- [x] Each adds `· K selected · MM:SS` (selected COUNT + duration) the
      moment ≥1 row is selected, and drops it again back to no
      selected-clause when selection clears.
- [x] Format matches the active playlist exactly: singular "1 track" with
      exactly one row, `M:SS` under an hour, `H:MM:SS` at/above an hour, for
      both the total and the selected clause independently.
- [x] The bar updates live on selection change (click/⌘-click/shift-click)
      and on list reload (rescan, add/remove tracks, playlist Save/Revert,
      device sync, disc swap) — no stale count/duration lingering after any
      of these.
- [x] Playlist editor: confirm the status bar reflects the SEARCH-FILTERED
      view (type in "Search this playlist…" and confirm the count drops to
      match only matching rows), not the full unsearched playlist.
- [x] Device detail: confirm the status bar reflects the selected playlist
      chip filter too (switch from "All files" to a device playlist chip and
      confirm the count matches just that playlist's entries).
- [x] Disc-files browser: confirm the bar is only present for a non-blank
      data disc (hidden/absent state matches whenever `dataDiscView` itself
      isn't shown — blank disc, audio CD, no disc).

**Blind uncertainties:**
- Files view's `model.mlTracks` is DB-query-backed (search re-fetches via
  `mlFetchTracks`), so unlike the other three views there's no client-side
  filter to double-check — the displayed array IS the query result. Should
  be a non-issue, but flag if the Files view's count ever looks like it's
  counting a stale pre-search array.
- Playlist editor's bar was subsequently moved (per explicit follow-up
  request) from bottom-of-view to directly under `MLEditorTable`, above the
  button row — now matches the active-playlist window's placement instead of
  GTK's literal `edit_vbox.append(&pl_status_bar)` order.
- Disc-files status bar placement (between the table and the "Add
  Selected/Add All" row) is a judgment call — GTK's own layout for that
  region doesn't map cleanly onto Mac's existing button placement, so this
  wasn't a literal port; confirm it reads correctly, doesn't crowd the
  buttons below it.

## Phase 8 — F10 watch folders ✅ PASSED on hardware 2026-08-03

**Corrections applied 2026-08-02 (review pass, compiled):**

1. **The whole feature was dormant unless the Media Library window was
   open.** Mac opened the library DB only on demand, and every demand site
   was a window. `rebuild_watcher` and `sparkamp_ml_note_played` both no-op
   while `ctx.media_library` is `None`, so with that window closed there was
   no watcher, auto-add-played did nothing, and `rescan_on_startup` was a
   setting that persisted and never fired. GTK opens the library at window
   build and the TUI at `App::new`; mac now does the same from
   `SparkampModel.init()` via `mlStartupTasks()`, honoring `skip_db_load`
   the way GTK does.
2. **The per-folder Recurse checkbox had no state SwiftUI could observe** —
   its binding read the flag through FFI on every render and its setter
   published nothing, so it repainted correctly only because the model's
   10 Hz tick happens to invalidate the pane, at a cost of two SQLite
   queries per folder per redraw on the main thread. Now mirrored into
   `@State`, loaded on appear and on folder-list change, as GTK does when it
   builds each row.
3. **The watch-event drain was unbounded on the main actor.** Each poll does
   a tag read plus a DB write inside the FFI; copying an album in queues one
   event per file and drained the lot in a single tick. Capped at 16 events
   per tick (~160 files/second) so the UI and the visualizer keep their
   frames.
4. **`sparkamp_ml_rescan_all` walked the filesystem on the calling thread**
   before spawning its background scan — a full walk of a large library, on
   the main thread, which the new startup trigger would have put squarely in
   the launch path. Moved into the worker thread, matching GTK's startup
   rescan. It also now calls `reset_unscanned_metadata` first, as GTK does
   ahead of every full rescan.
5. Minor: `rebuild_watcher` returns early with no folders instead of
   starting an empty debouncer (GTK and TUI both do); the three F12 toggles
   that phase 10 appended sat under the "Folder Watching" header and now
   have their own "Library Behavior" section.

Wires the Task-9 watch-folders FFI surface into the mac frontend: five
Settings toggles (`SettingsWindow.swift`'s `MediaLibraryPane`, new "Folder
Watching" section between ReplayGain and Watched Folders), a per-folder
Recurse checkbox on each row of the existing "Watched Folders" list (same
pane — note this is where that list actually lives, NOT
`MediaLibraryWindow.swift`; the brief's assumption otherwise was stale and
corrected by reading the real files), `sparkamp_ml_watch_rebuild` calls
after folder add/remove/recurse-change and on library open
(`SparkampModel+MediaLibrary.swift`), a `tick()` drain of
`sparkamp_ml_poll_watch_event` that reuses the existing `mlReloadTrigger`
signal (`SparkampModel.swift`), and a new FFI helper
`sparkamp_ml_note_played` hooked once at the single central "current track
just changed" point inside `tick()`'s `idx != currentIndex` branch — not
sprinkled across individual transport buttons.

- [x] All 5 new toggles ("Watch folders for changes", "Automatically add
      played tracks", "Remove missing files on rescan", "Compact database
      after rescan", "Rescan all folders on startup") in Settings ▸ Media
      Library persist across a quit/relaunch and correctly reflect the
      saved value when the pane reopens.
- [x] Each watched folder row in Settings ▸ Media Library ▸ Watched Folders
      shows a "Recurse" checkbox that reflects that folder's actual
      recurse setting (not a shared/global value) and, when toggled,
      changes only that folder's behavior.
- [x] With "Watch folders for changes" ON: dropping a new audio file into a
      watched folder in Finder makes it appear in the Files view within
      ~2–5 seconds, with no manual Rescan needed.
- [x] With "Remove missing files on rescan" ON: deleting a file from a
      watched folder in Finder removes its row from Files within a similar
      window. With it OFF: the row is KEPT (not marked broken/missing) —
      confirm no row silently vanishes when the toggle is off.
- [x] Editing a tag externally (e.g. in another app) on a file already in
      the library updates that file's row (title/artist/etc.) live, without
      a manual rescan.
- [x] Saving a tag edit from Sparkamp's own ID3 editor does NOT trigger a
      visible rescan/refresh storm — the watcher's cache-prefix / self-write
      suppression (Task 5) should make Sparkamp's own writes invisible to
      the watch-event drain.
- [x] A non-recursive folder (Recurse OFF) ignores new files dropped into a
      SUBdirectory of that folder — they should not appear in Files. Turn
      Recurse back ON and drop another file in that subdirectory: the new
      one appears live. Files dropped while it was OFF appear after a
      Rescan All, not immediately — turning Recurse on rebuilds the watch,
      it does not re-walk the folder. GTK behaves identically
      (`frontends/gtk/window/settings.rs`), so this is parity, not a gap.
- [x] With "Rescan all folders on startup" ON: quit Sparkamp, add a file to
      a watched folder from Finder while it's closed, relaunch — the new
      file is present in Files without a manual Rescan.
- [x] With "Automatically add played tracks" ON: play a file that lives
      OUTSIDE every watched folder (e.g. via File ▸ Open or a drag-and-drop
      onto the player) — it should appear as a row in Files shortly after
      playback starts. With the toggle OFF, it should NOT appear.
- [x] Play a file that's already INSIDE a watched folder — confirm no
      duplicate row is created (the inside/outside guard in
      `sparkamp_ml_note_played` should skip it entirely).
- [x] With "Compact database after rescan" ON: run Rescan All (or trigger a
      startup rescan) after removing several watched folders/files, and
      confirm the on-disk DB file doesn't keep growing — a rough
      before/after file-size check is enough (exact shrink amount isn't the
      point, "doesn't just grow forever" is).
- [x] Simulate a watcher start failure (e.g. remove/rename a watched folder
      out from under Sparkamp right as it's about to start watching, or
      revoke folder permissions) and confirm the app does NOT crash —
      it should silently fall back to manual/interval rescans (per
      `rebuild_watcher`'s documented degrade-gracefully contract).

Added by the 2026-08-02 review — these cover the corrections above and are
the ones most likely to fail if a fix is wrong:

- [x] **Watching works with the Media Library window never opened.** Quit,
      relaunch, and do NOT open the Media Library. Drop a file into a
      watched folder from Finder, wait ~5 s, then open the Media Library —
      the file is already in Files, without a rescan. (Before the fix the
      watcher only started when that window opened, so the file would have
      been missing.)
- [x] **Auto-add-played works from a cold launch.** With "Automatically add
      played tracks" ON, quit and relaunch, and without opening the Media
      Library, play a file that lives outside every watched folder. Then
      open the Media Library — the track is in Files.
- [x] **Launch stays responsive with rescan-on-startup ON.** With a large
      library and the toggle on, relaunch: the player window appears
      promptly and stays interactive (drag the seek bar, open the
      equalizer) while the scan runs in the background. The old code walked
      every folder on the main thread before the scan even started.
- [x] **A bulk copy doesn't freeze the UI.** With watching on, copy a whole
      album (or a few hundred files) into a watched folder in Finder. The
      player stays responsive and the visualizer keeps its framerate while
      the rows fill in over a few seconds — they should arrive steadily
      rather than all at once after a stall.
- [x] **The Recurse checkbox survives a settings round-trip.** Toggle
      Recurse off on one folder, close the Settings window, reopen it — the
      checkbox is still off, and the other folders are unaffected. Then quit
      and relaunch and confirm it is still off (it lives in the DB, not the
      config file).
- [x] **Sections read correctly.** Settings ▸ Media Library shows "Folder
      Watching" holding exactly the five watch/scan toggles, and a separate
      "Library Behavior" section holding "Remember search per view", "Treat
      artist as album artist" and "Skip database load at startup".
- [x] **Rescan All recovers empty rows.** If any Files rows show a title but
      no artist/length, hit Rescan All — they fill in
      (`reset_unscanned_metadata` now runs first, as it does on GTK).

---

## Phase 9 — CD-TEXT read ✅ CLOSED on hardware 2026-08-03

**Closed by decision on 2026-08-03**, with the read path verified on real
hardware and the remaining cases accepted untested. 5 of the 19 items below
were exercised against a physical drive and are ticked; the other 14 were
**not run** and are left unticked on purpose — they need discs and a second
drive this pass did not have, and ticking them would put untested claims in
the project's test record. They are listed under "Deferred" below so a later
pass (or a bug report) knows exactly what was never exercised.

**The core read path is confirmed on real hardware.** A Slimtype DVD A
DS8A5SH (USB) with a 15-track CD-TEXT disc — "Bespoke Bounce" by Waller
Creek Vipers, freedb id `e40b970f`, no gnudb match — was read end to end:
`drutil` dump → `parse_drutil_cdtext` → `sparkamp_disc_read_cdtext` →
`XmcdEntry` JSON → Swift overlay → Media Library disc view showing
"Waller Creek Vipers — Bespoke Bounce", a `CD-TEXT` pill, and all 15 real
track titles.

The remaining unticked items need discs or drives this pass did not have: a
gnudb-*known* disc, a disc with no CD-TEXT at all, a disc whose CD-TEXT
names only some tracks, a multi-language disc, and a second drive for the
burn-contention test.

### Real `drutil -drive 1 cdtext` output (captured 2026-08-03)

Exactly the XML plist the parser was written against, with one structure the
synthetic fixtures did not have — a `<data>` blob under `DRCDTextSizeKey`,
whose base64 body sits on its own lines. The parser ignores it (it matches
neither `<string>…</string>` nor a `<key>`), which is the correct outcome.
stdout carried the plist; stderr was empty; exit 0.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<array>
	<dict>
		<key>Properties</key>
		<dict>
			<key>DRCDTextCFStringEncodingKey</key>
			<integer>1536</integer>
			<key>DRCDTextCharacterCodeKey</key>
			<integer>1</integer>
			<key>DRCDTextCopyrightAssertedForNamesKey</key>
			<integer>1</integer>
			<key>DRCDTextLanguageKey</key>
			<string>en</string>
			<key>DRCDTextNSStringEncodingKey</key>
			<integer>1</integer>
		</dict>
		<key>Tracks</key>
		<array>
			<dict>                              <!-- index 0 = the DISC -->
				<key>DRCDTextPerformerKey</key>
				<string>Waller Creek Vipers</string>
				<key>DRCDTextSizeKey</key>
				<data>
				AQEPAxYbAAAAAAAAAAAAAAAAAAMzAAAAAAAAAAkAAAAA
				AAAA
				</data>
				<key>DRCDTextTitleKey</key>
				<string>Bespoke Bounce</string>
			</dict>
			<dict>                              <!-- index 1 = track 1 -->
				<key>DRCDTextPerformerKey</key>
				<string>Waller Creek Vipers</string>
				<key>DRCDTextTitleKey</key>
				<string>Blue Light Boogie</string>
			</dict>
			… 14 more track dicts …
		</array>
	</dict>
</array>
</plist>
```

Accounting checks out: 18 `<dict>` = 1 block + 1 `Properties` + 16 track
dicts (disc + 15 tracks), 16 `DRCDTextTitleKey`, 1 language block. The FFI
returned `{"discid":"e40b970f","artist":"Waller Creek Vipers","album":
"Bespoke Bounce",…}` with exactly 15 `track_titles`.

Two live tests were added and both pass against this disc:
`disc::cdtext::tests::live_drutil_cdtext_read` (raw dump + parse) and
`ffi::disc::tests::live_read_cdtext_ffi` (drive enumeration → FFI →
`XmcdEntry`, asserting one title per audio track). Both are `#[ignore]`d.

### Deferred — accepted untested when phase 9 was closed (2026-08-03)

Not run. Each stayed unticked below rather than being marked passed.

**Testable with the same CD-TEXT disc, just not done** — these four are the
cheapest to close if phase 9 is ever revisited, and they exercise the write
half of the feature (seeding and propagation) that the read verification
never touched:
- Tag editor on a CD-TEXT-only disc → prefilled, not blank
- Edit + Save → pill flips to `edited`
- Playlist-add inherits the CD-TEXT artist/album
- Ripped files inherit the CD-TEXT artist/album

**Blocked on media/hardware not available:**
- gnudb-*known* disc: names unaffected, CD-TEXT never read for it (needs a
  disc gnudb matches) — and its `gnudb` pill
- A disc with no gnudb match AND no CD-TEXT: "Track N" fallback, no pill
- A disc whose CD-TEXT names only some tracks (gap handling)
- A multi-language CD-TEXT disc (the first-wins block logic — the test disc
  carried a single `en` block, so that path ran unexercised on hardware)
- `&`, `<`, accented or non-Latin CD-TEXT (entity + encoding path; only
  apostrophes were seen live)
- Burn on drive A while probing drive B (needs a second drive) — see the
  note on that item: the exclusive-read guard does nothing on macOS by
  design, so this is the one item with a real chance of finding something
- Three-frontend badge parity (needs the same disc under GTK on Linux)
- Titles-only CD-TEXT disc: pill renders with no artist/album beside it
  (code fixed this pass, layout never eyeballed)

### Corrections applied 2026-08-03

1. **The whole feature was inert: the parser was written for a format
   `drutil` never emits.** `parse_drutil_cdtext` looked for a human-readable
   dump — `TITLE "…"` / `PERFORMER "…"` lines under `Track N:` headings —
   which the plan itself flagged as a guess ("`drutil cdtext`'s exact stdout
   format is NOT documented publicly and could not be captured in this
   environment"). It is not what the tool does. Disassembling
   `/usr/bin/drutil` shows the `cdtext` command building one
   `{Properties, Tracks}` dictionary per CD-TEXT block, then
   `[NSPropertyListSerialization dataWithPropertyList:format:100 …]`
   (`100` = `NSPropertyListXMLFormat_v1_0`) and `printf("%.*s")` — it prints
   an **XML property list**. None of its lines start with `TITLE` or
   `Track `, so the old parser matched nothing on every real disc,
   `read_cdtext` returned `None` every time, the FFI returned NULL, and
   `DiscService.readCdtext` returned nil. No disc could ever have shown
   CD-TEXT on mac. Rewritten to parse the plist.
2. **Track indexing was off by one relative to the real data.** In
   DiscRecording, `Tracks[0]` describes the **disc** and `Tracks[N]` track N
   (documented on `DRCDTextBlockGetTrackDictionaries`). The old parser had no
   concept of a disc entry in the track list and would have numbered
   everything from 1.
3. **Multi-language blocks.** A disc carrying several language blocks emits
   one dict per block; fields are now taken first-wins, so block 0 (English
   in practice) names the disc and a later block only fills what it left out.
4. **XML entities.** Names arrive escaped (`Simon &amp; Garfunkel`), so the
   parser resolves entities in a single left-to-right pass — an escaped
   escape (`&amp;lt;`) resolves once to `&lt;` rather than twice to `<`.
5. **The pill was hidden on exactly the disc it exists for.** The
   already-flagged follow-up is fixed: the source pill was nested inside the
   `!artist.isEmpty || !album.isEmpty` header conditional, so a CD-TEXT disc
   carrying track titles but no disc artist or album showed no pill on mac
   while GTK and the TUI showed `CD-TEXT`. The header line and the badge are
   now independent computed properties (`discHeaderLine`, `discSourceBadge`).
6. **Two phase-9 tests failed on macOS and now pass.**
   `cdtext_overlays_only_when_gnudb_absent` and
   `tag_editor_seeds_from_cdtext_then_gnudb_wins` were asserting against an
   empty track list. `disc::toc::track_entries` is platform-split — Linux
   synthesizes a `cdda://` URI per track off the TOC, macOS lists the AIFFs
   in the volume the OS mounted — so a fake drive with `mount_path: None`
   yields zero entries on macOS and both tests collapsed before testing
   anything. The fixture now backs the fake drive with a tempdir of
   per-track files. This was a test-portability defect, not a product one.

**How the format was verified without a disc:** the fixtures in
`src/disc/cdtext.rs` are not hand-written guesses. They were produced by
driving the same DiscRecording + `NSPropertyListSerialization` calls
disassembled out of `drutil`, and the reconstructed fixture in the test was
diffed byte-for-byte against that real output. What is still unverified is
only the part a disc is needed for: that a real drive populates those
dictionaries the way a synthetic block does. `cargo test --lib
live_drutil_cdtext_read -- --ignored --nocapture` (added in this pass) prints
the raw dump beside the parse result and is the first thing to run once a
drive is attached.

Mirrors GTK's `disc_cdtext`/`disc_cdtext_tried` overlay
(`frontends/gtk/window/media_library.rs:9031-9108`): mac now calls the
shared `sparkamp_disc_read_cdtext` FFI (Task 2) on first show of an audio
disc with no gnudb/user match, and overlays the result exactly like a gnudb
entry. Winamp precedence is LOCKED to the whole entry, never merged
per-field: gnudb/hand-edited tags win outright when present; CD-TEXT fills
in only on a total miss.

Files touched: `SparkampModel.swift` (new `discCdtext: [String: XmcdEntry]`,
`discCdtextTried: Set<String>`, `loadedDiscId: String?`), `DiscService.swift`
(new `readCdtext(drive:) -> XmcdEntry?`, wraps `sparkamp_disc_read_cdtext` +
`sparkamp_free_string`), `SparkampModel+Discs.swift` (new
`discOverlayTags(_:)` — the `gnudb ?? cdtext` chooser; new
`maybeReadDiscCdtext(_:)` — the one-shot background read + cache + re-render;
`loadDiscTracks` now sets `loadedDiscId` and calls `maybeReadDiscCdtext`;
`applyDiscTagTitles` now reads through `discOverlayTags` instead of
`discTagSets` directly), `DiscDriveView.swift` (header's `discTags` computed
var now reads through `discOverlayTags` too, so the "Artist — Album (year)"
line picks up a CD-TEXT-only entry the same as a gnudb one).

- [x] **CD-TEXT disc absent from gnudb** ✅ verified 2026-08-03 (Bespoke Bounce / Waller Creek Vipers, `e40b970f`; header + all 15 titles, no Identify needed): insert an audio disc gnudb has no
      record of but that carries CD-TEXT (e.g. a disc burned by Sparkamp
      itself with disc-artist/disc-album set, or a commercial disc with
      CD-TEXT gnudb doesn't know) — real album/artist show in the header
      ("Artist — Album") and real per-track titles show in the track table,
      without pressing Identify.
- [ ] **gnudb-known disc unchanged**: insert a disc gnudb DOES match (or one
      with hand-edited tags saved via Edit Tags) — its gnudb/user names are
      unaffected; CD-TEXT is never read for it at all (confirm via a log/
      breakpoint in `maybeReadDiscCdtext` that the early `discTagSets[id] ==
      nil` guard skips the FFI call entirely for this disc).
- [ ] **Neither gnudb nor CD-TEXT**: a disc with no gnudb match and no
      CD-TEXT on the physical media — track table falls back to "Track N"
      per track and the header line is hidden, same as before this task.
- [ ] **Burn in one window + probe another → no drive fight**: with two
      drives attached, start a burn on drive A's Disc Drive view while
      navigating to and viewing an unknown audio disc on drive B (triggering
      B's CD-TEXT read). Confirm no error dialogs, no drive contention, and
      B's CD-TEXT read either succeeds cleanly or fails silently.
      **Read this before testing:** `sparkamp_disc_read_cdtext` does hold the
      exclusive-read guard around the read, but that guard **does nothing on
      macOS** — and that is deliberate, not an oversight. The only production
      code that consults `exclusive_read()` is Linux's `list_drives_cached`
      (`src/disc/detect.rs:822`), because on Linux even a status ioctl
      interleaves SCSI commands with a streaming read; macOS goes through
      `drutil status`, which `detect.rs:35` documents as not spinning the
      disc, so there is nothing to suppress. The practical consequence is
      that on mac nothing serializes a CD-TEXT read against a concurrent burn
      beyond drutil's own device locking. That is what this item is really
      testing — if two drutil invocations on the same drive do fight, the fix
      is a Swift-side guard, not the core refcount.
- [ ] **Ripped filenames/tags inherit CD-TEXT**: rip a CD-TEXT-only
      (gnudb-absent) disc — the ripped files' names/tags use the CD-TEXT
      track titles (via `discTracks[i].title`, already overlaid by
      `applyDiscTagTitles` before the rip sheet reads it), matching GTK's
      behavior for the same disc. As of the 2026-07-28 parity fix, the
      disc-level Artist/Album ID3 fields on the ripped files ALSO come from
      CD-TEXT on a total gnudb miss: `ripDiscTracks` now reads
      `discOverlayTags(discid)` (`SparkampModel+Discs.swift`) instead of
      `discTagSets[discid]` directly, mirroring GTK's
      `disc_tags.get(id).or_else(|| disc_cdtext.get(id))` in `disc.rs`'s rip
      dialog and TUI's `rip.rs:140-145`. Precedence stays whole-entry
      (Winamp): a gnudb/user entry wins outright when present; CD-TEXT only
      fills in on a total miss. CD-TEXT is still NEVER folded into the
      persisted/submittable tag set (`discTagSets`) — `discSubmittable`/
      `submitDisc` keep reading `discTagSets` directly, so a CD-TEXT-only
      disc still can't be pushed to gnudb. Confirm ripped MP3s from a
      CD-TEXT-only disc carry the CD-TEXT artist/album, and that a
      gnudb-matched disc's ripped tags are unaffected (gnudb still wins).
- [x] **Acquisition path + drutil dump capture** ✅ verified 2026-08-03 — FFI path, dump above, parses clean (run this FIRST — it is the
      one item the rest depend on): with a CD-TEXT disc loaded, run
      ```
      cargo test --lib live_drutil_cdtext_read -- --ignored --nocapture
      ```
      It prints the raw `drutil -drive N cdtext` stdout, drutil's stderr, and
      the parsed `CdText` side by side. Set `SPARKAMP_TEST_DRIVE` to the
      drutil index from `drutil list` if it is not `1`. Expected: an XML
      plist on stdout and a parse with real album/artist/titles. If the parse
      comes back empty, paste the raw dump here — the parser is written
      against synthetic-but-real DiscRecording output, so a mismatch means a
      live drive populates the dictionaries differently:
      ```
      (paste real `drutil cdtext` output here during the hardware pass)
      ```
      Only the FFI path is implemented (`sparkamp_disc_read_cdtext` → core
      `parse_drutil_cdtext`, mirrored in `DiscService.readCdtext`). The
      DiscRecording-framework fallback (`DRDevice` + `DRCDTextBlock`) named
      in that function's doc comment is still NOT implemented; it only
      becomes necessary if a live dump proves unparseable, and note that the
      core parser already consumes DiscRecording's own output format, so the
      framework route would buy structure, not different data.

**Unsure / eyeball (blind, no Xcode here):**
- `maybeReadDiscCdtext` guards re-render staleness with `loadedDiscId == id`
  (set at the end of `loadDiscTracks`) rather than GTK's "is this drive
  still the one the view holder points at" check — functionally equivalent
  (both stop a late CD-TEXT arrival for a disc the user has since navigated
  away from from clobbering `discTracks`), but it's a different mechanism
  than GTK's, so eyeball a same-drive rapid disc-swap (eject mid-read,
  insert a different disc before the FFI call returns) for a stale overlay.
- ~~`sparkamp_disc_read_cdtext` is called with `ctx: nil`~~ — **resolved by
  reading the code, no hardware needed.** The Rust side binds the parameter
  as `_ctx` and never touches it, so NULL is safe exactly as it is for
  `sparkamp_disc_track_entries`/`sparkamp_disc_id`.
- `discCdtext`/`discCdtextTried` are never cleared when a drive disconnects
  or a disc is ejected (unlike `discTagSets`, which persists on disk by
  design) — a re-inserted disc with the same freedb ID reuses the cached
  CD-TEXT rather than re-reading, which is intentional (mirrors GTK, which
  also never clears `disc_cdtext`/`disc_cdtext_tried`), but flag if this
  ever shows stale names after ejecting and inserting a DIFFERENT disc that
  happens to collide on freedb ID (extremely unlikely — same collision risk
  gnudb itself already has).
- TWO unknown-audio-disc drives visible simultaneously: `discTracks`/
  `loadedDiscId` are single global model fields (pre-existing architecture),
  so two concurrent CD-TEXT re-renders could interleave against shared state.
  Verify with two drives each holding a different gnudb-unknown CD-TEXT disc
  that both resolve to the correct names (no cross-drive overlay bleed).

### Phase 9 follow-on — source badge + editor seeding (2026-07-28, BLIND — Swift never compiled)

Adds a source pill next to the disc header's "Artist — Album (year)" line
naming which cache produced those names, mirroring Rust
`DiscMetaSource::resolve`/`badge()` (`src/disc/source.rs`) and its GTK/TUI
counterparts. Precedence and label strings are exact: `discOfficial[id] !=
nil` → `"gnudb"`; else `discTagSets[id] != nil` → `"edited"`; else
`discCdtext[id] != nil` → `"CD-TEXT"`; else no pill. Also seeds the disc tag
editor from `discOverlayTags(id)` (gnudb/edit, else CD-TEXT) instead of
`discTagSets[id]` alone, so a CD-TEXT-only disc no longer opens the editor
blank.

Files touched: `SparkampModel+Discs.swift` (new `discMetaSourceBadge(_:) ->
String?`; `discTagsForEditing` now seeds from `discOverlayTags(id)` instead
of `discTagSets[id]`), `DiscDriveView.swift` (header now renders a small
Capsule pill with `discMetaSourceBadge`'s text next to the "Artist — Album"
line when non-nil, styled off `theme.vars.highlight` — same token
`DiscMediaIcon`'s format badge in this file already uses). The gnudb SUBMIT
path (`discSubmittable`/`submitDisc`) is untouched and still reads
`discTagSets` directly, so CD-TEXT still can never auto-submit.

- [ ] **gnudb-known disc → `gnudb` pill**: insert/select a disc gnudb has
      matched (via Identify or a restored match) — the header shows a pill
      reading exactly `gnudb` next to "Artist — Album (year)".
- [x] **gnudb-unknown CD-TEXT disc → `CD-TEXT` pill** ✅ verified 2026-08-03 — pill reads exactly `CD-TEXT`: a disc gnudb doesn't
      know but that carries CD-TEXT — the header pill reads exactly
      `CD-TEXT` (not "cdtext" or "CD Text").
- [ ] **Edit + save a CD-TEXT/unknown disc → pill flips to `edited`**: open
      Edit Tags on a CD-TEXT-only or fully-unknown disc, change/confirm a
      field, Save — the pill switches to `edited` (because `saveDiscTags`
      writes `discTagSets[id]`, which now outranks `discCdtext` in
      `discMetaSourceBadge`).
- [ ] **No metadata (Track N) → no pill**: a disc with no gnudb match, no
      hand edit, and no CD-TEXT — track table shows "Track N" placeholders
      and the header shows NO pill (and no "Artist — Album" line, unchanged
      from before this task).
- [ ] **Open the tag editor on a CD-TEXT-only disc → prefilled, not blank**:
      with a gnudb-unknown, CD-TEXT-bearing disc loaded, click Edit Tags —
      artist/album/year/genre and every per-track title field are PREFILLED
      from CD-TEXT (not blank/"Track N" placeholders you'd get from a truly
      empty `DiscTagSet`). Click Save — the pill becomes `edited`, and
      "Submit to gnudb" becomes available (`discSubmittable` sees a disc
      gnudb has no official entry for) and, once submitted, uploads the
      promoted (now-user) tags — confirm the submitted payload is the
      CD-TEXT-derived text, not blank fields.
- [ ] **Three-frontend parity**: with the same physical/test disc, confirm
      mac, GTK, and TUI all show the identical badge text (`gnudb` /
      `edited` / `CD-TEXT`) for the same disc state — no casing or spelling
      drift between frontends.
- [ ] **Playlist-add inherits CD-TEXT artist/album**: add a gnudb-unknown
      CD-TEXT disc's tracks to the active playlist (`addDiscTracks`, now via
      `discOverlayTags`) — the playlist rows show the CD-TEXT disc artist +
      album (not blank), matching GTK/TUI. Per-track titles already inherited.
- [ ] **Titles-only CD-TEXT disc pill** (code fixed 2026-08-03 — verify the
      layout on hardware): the pill used to be nested inside the
      `!artist.isEmpty || !album.isEmpty` header conditional, so a CD-TEXT
      disc carrying track titles but empty artist AND album showed no pill on
      mac while GTK/TUI showed `CD-TEXT`. `discHeaderLine` and
      `discSourceBadge` are now separate computed properties and the row
      renders when either is non-nil. On such a disc, confirm the pill
      appears on its own line under the media summary and that the header
      doesn't look lopsided with no text beside it.

**Added by the 2026-08-03 review — verify these specifically:**

- [x] **The parser actually parses this drive's output** ✅ verified 2026-08-03: covered by the
      "Acquisition path + drutil dump capture" item above; it is the
      gating check for every other CD-TEXT item on this page, because before
      this review the parser could not have matched any real dump.
- [x] **Disc-level names land on the disc, not on track 1** ✅ verified 2026-08-03 — header `Bespoke Bounce`, row 1 `Blue Light Boogie`: on a CD-TEXT
      disc, confirm the album/artist show in the header AND that track 1's
      row shows track 1's own title — not the album name shifted down by
      one. This is the `Tracks[0]` = disc indexing.
- [ ] **A disc whose CD-TEXT names only some tracks**: the named tracks show
      their titles and the rest stay "Track N" — no blank rows, no
      off-by-one after the gap.
- [ ] **Non-ASCII / punctuated names survive** (PARTLY verified 2026-08-03):
      apostrophes came through the whole chain intact — `Moppin' And Boppin'`
      and `You's A Viper` render correctly in the disc view. Still untested:
      `&` and `<` (the entity-resolution path — unit-tested but never seen on
      a real disc) and accented / non-Latin characters (the
      `DRCDTextCharacterCodeKey` = 1 encoding path). Needs a disc with those
      in its CD-TEXT.
- [ ] **Several language blocks**: a disc with more than one CD-TEXT language
      block shows the first block's names (English in practice), not a mix.

---

## Phase 10 — F11 + F12 settings cluster ✅ PASSED on hardware 2026-08-03

**Corrections applied 2026-08-03 (review pass, compiled and exercised):**

1. **A percent threshold of 100% never counted a play at all.**
   `play_stats::play_counted_at` clamped seconds mode to `length * 0.9` but
   left percent mode unclamped, so at 100% the deadline was exactly the track
   duration — a position the frontends never observe. They sample the position
   on a timer (10 Hz on mac) and the engine fires EOS and advances while the
   last sample is still short of the duration. Proven on hardware before the
   fix: percent = 100, two tracks played end to end (2:45 and 2:56), zero plays
   recorded. Percent mode now takes the same `length * 0.9` clamp, which only
   bites above 90% — exactly the band where the raw deadline is unreachable.
   Re-verified after the fix: a 250 s track counted at 226 s (= 0.9 × 250.6).
   Core fix, so GTK gets it too.
2. **"Remember search per view" OFF no longer cleared the Files search box.**
   The Media Library is a SwiftUI `Window` scene, so closing it does not tear
   the view down — `searchQuery` survives the close. `MediaLibraryWindow`'s
   `.onAppear` only ever *assigned* the box when the feature was ON, so with
   the toggle OFF the previous session's query stayed in the box and kept
   filtering the list: the one behaviour the toggle promises not to change.
   The three sibling views (`DiscDriveView`, `DeviceDetailView`,
   `MLPlaylistEditor`) all clear explicitly on their own reopen path; this was
   the only one that did not. Now routed through a `restoreOrClearSearch()`
   helper with the same else-clear. Verified both directions: OFF → empty box
   and all 278 rows; ON → "cellophane" restored and 1 row.
3. Minor: the `#[allow(dead_code)]` on `play_counted_at` claimed the function
   was unused "until the phase-10 controller wiring consumes it", which had
   been false since Task 2. It is still needed on macOS — the binary reaches
   it only through the Linux-gated GTK tick — so it is now
   `cfg_attr(not(target_os = "linux"), …)` with the real reason.

**Deferred — accepted untested.** These need hardware or library content this
pass did not have; they are left unticked rather than claimed:

- Short track shorter than the threshold (F11) — no track under 60 s exists in
  the test library, so the `length * 0.9` clamp was only exercised by unit
  test and by the percent-mode 100% case.
- Playlist-editor and Devices search restore (F12.1) — the editor path needs
  typing into a box that AX cannot drive, and only one device was connected.
- Discs search surviving a 10 s drive poll (F12.1) — restore-on-open was
  verified with a real disc, but the external drive was unplugged before the
  poll-cycle case could be run.
- Whitespace-only album-artist tag, playlist-editor and device Album Artist
  cells, and sort-stays-on-raw-value (F12.2) — no whitespace-only tag exists in
  the library, and the same two surfaces were unavailable.
- A1 stats placeholders before the library is ever opened (F12.3).

Everything else below was exercised against the real app and the real
`media_library.db` (play counts read straight out of SQLite, timestamps
compared against the expected deadline).

**Noticed, deliberately not fixed here:** the TUI never calls `record_play` at
all — `rg record_play frontends/` finds only the bridge header and shuffle's
unrelated `record_played`. So F11 has no TUI surface and never did; the
configurable threshold reaches GTK and mac only. That is pre-existing, not a
phase-10 regression, and outside this mac pass.

---

## Phase 10 — Task 2: F11 play-count threshold FFI

Adds the C FFI surface for the configurable play-count threshold: a deadline
helper (`sparkamp_play_deadline_secs`) plus enabled/mode/seconds/percent
get/set pairs over `[playback.play_stats]`, mirrored byte-for-byte into
`sparkamp_bridge.h`. This task is Core + header only — no Swift UI wiring
yet; the Settings controls and the transport call site that actually calls
`sparkamp_play_deadline_secs` per-track land in Task 4. Nothing here is
user-visible on mac until then.

- [x] **Header signatures match Rust exactly**: BUILD SUCCEEDED with zero code
      warnings against all 9 declarations (`sparkamp_play_deadline_secs`,
      `sparkamp_get/set_play_stats_enabled`,
      `sparkamp_get/set_play_stats_mode`,
      `sparkamp_get/set_play_stats_seconds`,
      `sparkamp_get/set_play_stats_percent`), and every one of them is
      exercised at runtime by the Task 4 checks below — the `uint32_t` mode
      encoding round-trips (picking "N% of track" wrote `mode = "percent"`).
- [x] **No persistence surprise**: each Settings change landed in
      `config.toml` immediately, so the Swift call sites do call
      `sparkamp_save_config`. Watched the file while driving the UI: seconds
      20 → 5, mode seconds → percent, percent 50 → 5 each appeared under
      `[playback.play_stats]` within a second of the click.

---

## Phase 10 — Task 4: F11 mac deadline wiring + settings UI

Wires the Task 2 FFI into the mac frontend: `SparkampModel.tick()`'s play-
count gate now computes its deadline per-track from
`sparkamp_play_deadline_secs(ctx, dur)` instead of the old hardcoded
20 s constant (`playCountThresholdSecs` deleted), and `SettingsWindow.swift`
gets a new "Play Count" section on the Playback pane (Count plays toggle,
Seconds/Percent mode picker, seconds stepper 1–3600, percent stepper
1–100), mirroring the existing ReplayGain section's Toggle/Picker/Stepper +
`onChange` + `sparkamp_save_config` idiom.

- [x] **Toggle off → counts freeze**: with "Count plays" off, played a track
      to 0:15 against a 5 s threshold — nothing written to the DB (no new
      `last_played` row after the start timestamp). Turning it back on, the
      next track counted normally.
- [x] **Seconds mode, seconds = 5 → counts at 5 s**: started a track at
      16:20:23; `play_count` was still 0 at t≈4 s and became 1 with
      `last_played = 16:20:28` — 5 s, not the old 20 s default.
- [x] **Percent mode → counts at the configured fraction**: run at three
      settings rather than only 50%, since the fraction is what is under
      test. 5% of 165.7 s → counted 9 s in (expected 8.3); 5% of 159.2 s →
      counted at exactly 8.0 s; 5% of 230.1 s → counted at 12 s (expected
      11.5). Also the clamped case: 100% of 250.6 s → counted at 226 s
      (expected 225.5), which before the fix never counted at all.
- [ ] **Short track (shorter than the configured threshold) → counts near
      its end**: DEFERRED — the test library has no track under 60 s, so the
      seconds-mode `length * 0.9` clamp was only covered by unit test. The
      percent-mode 100% case above exercises the same clamp expression.
- [x] **Settings persist across relaunch**: quit and relaunched five times
      across this pass with different enabled/mode/seconds/percent
      combinations; Settings ▸ Playback showed the saved values each time and
      the gate fired at the saved threshold.
- [x] **Fullscreen-visualizer does NOT skew the deadline** — verified by
      reading `tick()`, not on hardware. Driving the fullscreen visualizer
      from AppleScript proved unreliable (it only opens for Waveform/Granite
      mode, and Stop closes it again), and the source settles it: line 481
      reads `dur` unconditionally every tick, line 489 gates only the
      `@Published` assignment, and line 538 passes the local `dur`. The
      publisher freeze cannot reach the deadline. Original wording follows.

      The gate feeds
      the fresh per-tick local `dur` (`sparkamp_get_duration(ctx)`) to
      `sparkamp_play_deadline_secs`, NOT the `@Published duration` property
      that tick() freezes while the fullscreen visualizer is open (see the
      `!fullscreenVizVisible` guard). Verify: in percent mode, open fullscreen
      right as a new track starts, then let it play — the count should land at
      the correct halfway point of the CURRENT track (not skewed by the
      previous track's length), because `dur` keeps flowing regardless of the
      publisher freeze, matching the always-fresh `pos` local the same gate
      uses.

---

## Phase 10 — Task 5: F12.1 remember search per view

Mirrors the 4 new FFI functions (`sparkamp_get/set_remember_search`,
`sparkamp_get_last_search`, `sparkamp_set_last_search`) into
`sparkamp_bridge.h`, adds a "Remember search per view" toggle to
`SettingsWindow.swift`'s Media Library section (same Toggle/onChange/
`sparkamp_save_config` idiom as the neighboring `rescanOnStartup` row), and
wires each of the 4 Media-Library search boxes — Files (`MediaLibraryWindow.
swift`), Playlists editor (`MLPlaylistEditor.swift`), Devices
(`DeviceDetailView.swift`), Discs (`DiscDriveView.swift`) — to restore their
saved query on view-open (window appear / playlist switch / device switch /
drive switch) and persist on change via a debounced `sparkamp_set_last_search`
call, keyed by view id `"files"`/`"playlists"`/`"devices"`/`"discs"`. When
`remember_search` is off, every one of these falls back to the pre-existing
"switching clears the search" behavior — nothing changes for users who leave
the toggle off.

- [x] **Toggle off (default) → unchanged behavior** — this is defect 2 above;
      it FAILED as written and now passes. With the toggle off, closing and
      reopening the Media Library left "ellise" in the Files box with the list
      still filtered to 3 rows. After the fix: empty box, all 278 rows.
- [x] **Toggle on → Files search survives a window close/reopen**: with the
      toggle on and `last_search.files = "cellophane"`, reopening the Media
      Library showed "cellophane" in the box and exactly the 1 matching row.
      Re-checked after the defect-2 fix so the clear did not break the
      restore.
- [ ] **Toggle on → Playlists editor search survives switching playlists and
      reopening**: DEFERRED — needs text typed into the editor's search box,
      which the AX harness used for this pass cannot drive (setting the
      field's AX value does not fire the SwiftUI binding). Code path is the
      same `sparkamp_get_last_search` call as Files, inside `loadPlaylist()`.
- [ ] **Toggle on → Devices search survives switching devices**: DEFERRED —
      only one device was available; the check needs two.
- [x] **Toggle on → Discs search restores on open** (partly): with a real
      audio CD in an external drive and `last_search.discs = "boogie"`, the
      Discs search box came up holding "boogie" and the 15-track disc was
      filtered to the single "Blue Light Boogie" row.
- [ ] **Discs search NOT clobbered by the 10 s poll**: DEFERRED — the
      external drive was unplugged before a same-drive poll cycle could be
      timed. The `drive.toc` / `drive.mountPath` onChange handlers were read
      and neither touches `searchText`; only `onChange(of: drive.id)` does.
- [x] **Toggle off after having it on → next open clears again**: turned the
      toggle off while `last_search.files` still held "cellophane", reopened
      the Media Library — box empty, full 278 rows, stale map entry ignored.
- [x] **Settings persist across relaunch**: `remember_search = true` survived
      quit/relaunch and the Settings checkbox came up ticked; the saved Files
      query was restored on the next Media Library open.
- [x] **Header signatures match Rust exactly**: BUILD SUCCEEDED against all 4
      declarations, and all 4 ran — including `sparkamp_get_last_search` for
      a `view_id` with nothing saved (the Discs box on a launch before any
      disc query was stored), which returned `""` rather than crashing.

## Phase 10 — Task 6: F12.2 treat artist as album artist

Mirrors the 2 new FFI functions (`sparkamp_get/set_artist_as_album_artist`)
into `sparkamp_bridge.h`, adds a "Treat artist as album artist" toggle to
`SettingsWindow.swift`'s Media Library section (same Toggle/onChange/
`sparkamp_save_config` idiom as the neighboring `rememberSearch` row), and
routes every mac "Album Artist" cell through the same fallback rule as
`src/play_stats.rs`'s `effective_album_artist` (album_artist wins whenever
non-blank after trimming, else falls back to artist when the toggle is on,
else blank): `MLFilesTable.swift`'s `cellContent` (Files table + both editor
call sites via `MLEditorTable.swift`, which gained a new `artistAsAlbumArtist`
stored property) and `DeviceDetailView.swift`'s "Album Artist" column (now a
custom `TableColumn` content closure calling a new `displayAlbumArtist(for:)`
helper). Rust could not be asked to do the string choice for mac — there is
no shared FFI struct call per cell — so each Swift call site fetches the flag
itself via `sparkamp_get_artist_as_album_artist(ctx)` and applies the same
three-way rule inline. When the toggle is off, behavior is identical to
before this feature existed (blank cell for a blank album-artist tag).

- [x] **Toggle off (default) → unchanged behavior**: "Cellophane" / Sara
      Jackson-Holman (a DB row with a NULL album_artist) showed an empty
      Album Artist cell.
- [x] **Toggle on → Files table falls back to artist**: the same row's Album
      Artist cell became "Sara Jackson-Holman". Rows that already carry an
      album-artist tag were untouched. Worth recording: the change appeared
      **live, with playback stopped and no interaction with the Media Library
      window at all** — better than the "reopen/refresh" this item asked for,
      and matching what the GTK singleton-window fix (`5c1f537`) achieved
      there by explicit refresh.
- [ ] **Whitespace-only album-artist tag counts as blank**: DEFERRED — no row
      in the test library has a spaces-only tag. The Rust helper's
      `.trim().is_empty()` is unit-tested and the Swift side uses
      `trimmingCharacters(in: .whitespacesAndNewlines)`, but that pairing was
      not observed on real data.
- [ ] **Playlist editor matches the Files table**: DEFERRED — no saved
      playlist contains a blank-album-artist track.
- [ ] **Device view matches too**: DEFERRED — no device connected.
- [x] **Toggle off after having it on → cells go blank again**: turning it
      back off returned the cell to blank, again live.
- [ ] **Sorting/grouping unaffected**: DEFERRED — not exercised; clicking
      column headers was out of reach for this pass's harness.
- [x] **Settings persist across relaunch**: the flag round-tripped through
      `config.toml` and the Settings checkbox reflected the saved value on a
      later launch.
- [x] **Header signatures match Rust exactly**: BUILD SUCCEEDED, and both
      calls ran live — the getter on every Files cell render, the setter from
      the Toggle followed by `sparkamp_save_config` (observed writing
      `artist_as_album_artist = true`, then `false`, to `config.toml`).

## Phase 10 — Task 7: F12.3 skip database load at startup

Mirrors the 2 new FFI functions (`sparkamp_get/set_skip_db_load`) into
`sparkamp_bridge.h`, and adds a "Skip database load at startup" toggle to
`SettingsWindow.swift`'s Media Library section (same Toggle/onChange/
`sparkamp_save_config` idiom as the neighboring `artistAsAlbumArtist` row).
No mac runtime-behavior change was needed beyond the toggle itself: unlike
GTK (whose `AppState::new` unconditionally called
`MediaLibrary::open()` at startup before this task), the mac core
(`SparkampCtx`) has never eagerly opened the Media-Library DB — it starts
with `media_library: None` (`src/ffi/mod.rs`'s `sparkamp_create`) and only
opens it via `sparkamp_ml_open`, called from `SparkampModel+MediaLibrary
.swift`'s `openMediaLibrary()` at first demand (ML window open/restore,
Discs auto-open, dedupe, Settings "Add Folder…"). That function already
kicks `sparkamp_ml_watch_rebuild(ctx)` right after opening, and
`rebuild_watcher` (Rust) already no-ops while `ctx.media_library` is `None`
— so watcher-on-first-open was already correct pre-existing behavior, not
new wiring. The toggle's only mac-visible effect is that it now persists in
`config.media_library.skip_db_load` and round-trips through Settings, for
config parity with GTK/TUI.

**Note (2026-08-03):** this task's original text predates phase 8's fix, which
gave mac an `mlStartupTasks()` that DOES open the library at launch — gated on
exactly this flag. So `skip_db_load` is no longer "config parity only" on mac;
it is the switch that decides whether the library opens at launch. The checks
below were rewritten to test that, which is what the flag now does.

- [x] **Cold start with the toggle ON leaves the DB shut**: quit with the
      Media Library window closed (so nothing demands the DB), turn
      `skip_db_load` on, relaunch. Player window came up normally, and
      playing a track to 0:19 against a 5 s threshold recorded nothing —
      `record_play` no-ops while `ctx.media_library` is `None`, which is the
      observable proof the DB was never opened.
- [x] **First ML open loads normally**: opening the Media Library with the
      toggle on loaded all 278 tracks, the folder list, and the 4 saved
      playlists — no error, no empty-forever state.
- [x] **Play-count still works after ML opens**: immediately after that
      first open, the next track counted (id 84, `last_played` 17:10:29,
      ~4 s into a 5 s threshold). So the DB opens on demand and the F11 gate
      picks up from there.
- [ ] **A1 stats show placeholders before ML ever opens**: DEFERRED — the
      now-playing panel was not opened during the DB-shut window.
- [x] **Toggle persists across relaunch**: `skip_db_load = true` survived
      quit/relaunch and drove the behaviour above on the next launch.
- [x] **Header signatures match Rust exactly**: BUILD SUCCEEDED; the getter
      is called from `mlStartupTasks()` on every launch and the setter from
      the Toggle, with `sparkamp_save_config` writing the flag out (observed
      in `config.toml`).

## Phase 11 — A4 album gallery ✅ PASSED on hardware 2026-08-03

Four defects. The one that mattered was not in the gallery at all:

1. **A BOM in a tag emptied the album.** `cBytesToString` — the helper every
   fixed-buffer FFI string on mac goes through — decoded with Foundation's
   `String(bytes:encoding: .utf8)`, which treats a leading EF BB BF as a
   byte-order mark and **drops it**. Rust keeps it (U+FEFF stopped being
   Unicode `White_Space` in 4.0.1, so `trim()` leaves it), so the album name
   mac handed back to `sparkamp_ml_album_tracks` no longer matched any row and
   the album opened to "0 tracks". Measured: "Fallen Light", "Liberation",
   "The Storm", "Dirty Shine [Explicit]" and "What They Wrote" all returned 0
   before, their correct single track after. Now decoded with the stdlib's
   `String(decoding:as: UTF8.self)`, which is not lossy — this was a latent
   bug in every mac FFI string round trip, the gallery is just the first
   caller that fed one back to the core as a lookup key.
2. **Every release year rendered as "2,014".** `Text("· \(year)")` picks the
   `LocalizedStringKey` overload, which formats an interpolated integer with
   the locale's grouping separator. `Text(verbatim:)` fixes the year and the
   album count (the count only shows it past 1,000, but the window's other
   counts — "278 tracks" — are plain, so it now matches).
3. **The sidebar jumped to Files on drill-down.** Tapping a tile shows the
   album's tracks in the Files page, and mac moved the sidebar highlight with
   it — reading as "you left the gallery" while the toolbar's ‹ Albums button
   says otherwise. GTK deliberately leaves the highlight on Albums (see the
   comment above `on_album_activate` in `window/media_library.rs`); mac now
   does too.
4. **No search in the gallery.** Added, in the same toolbar slot and with the
   same widget as the Files search, filtering the loaded album list on the
   displayed title/artist. Persisted per F12.1 under a new `"albums"` view id.
   Both boxes needed explicit `.id()`s: as the same view type in adjacent
   conditional branches, SwiftUI matched them across a nav change and carried
   the Files box's empty text into the album query, clearing the filter on the
   way back from a drill-down.

Also on request, on both mac and GTK: a **track-count pill** in the
bottom-right of each cover.

**GTK note.** The badge is the only GTK change (`album_gallery.rs` wraps the
cover `Image` in a `gtk4::Overlay`; `skin.rs` gains `.album-cell-count`).
`skin.rs` compiles and its test passes here, but the GTK module is Linux-gated
and **was not compiled** — it needs a build in the dev-box before it can be
called done.

**Not fixed here.** Tags in this library carry a leading BOM in the album
field, which now (correctly) survives into the group name. A copy of the same
album without the BOM would still form a second tile. That is a tag-hygiene
question for the scanner, not a gallery bug, and is out of scope for this pass.

**Deferred — accepted untested.** Play Album / Enqueue Album from the tile
context menu, the F12.2 album-artist regrouping walk, and the sort-picker
reorder: all three are in the hand-off test plan instead of being ticked here.

## Phase 11 — Task 3: album gallery FFI (count/list/tracks) + bridge

Adds the C FFI surface for the album gallery view: `sparkamp_ml_album_count`,
`sparkamp_ml_albums`, and `sparkamp_ml_album_tracks`, plus the
`SparkampAlbum` struct, mirrored byte-for-byte into `sparkamp_bridge.h`. No
Swift consumer lands in this task — this is the core/bridge layer only, so
the checks below are Rust-side (`cargo test`) plus a header-compiles-clean
review; the Swift gallery view itself is a later mac task.

- [x] **Album count/list roundtrip is non-empty for a library with albums**:
      278 tracks in the library produced **133 groups** at Artist sort, every
      one with a populated title and (where the tag has one) an artist. SQL
      over the same DB predicts 132 distinct `(album, effective_album_artist)`
      pairs plus the no-album bucket = 133. Exact match.
- [x] **No-album bucket is fetchable**: exactly one "(no album)" tile, sorted
      last. Opening it returned **143 tracks** — the same number as
      `SELECT COUNT(*) FROM tracks WHERE TRIM(COALESCE(album,''))=''`.
- [x] **All three `AlbumSort` values are wired**: 0 (Artist) exercised
      throughout this pass and the ordering matches the documented rule —
      `("", "covers from another mother")` before `("", "when you dream")`
      before `("01", …)`, ZZ Ward last, bucket after that. Sorts 1 and 2 are
      the same code path with a different comparator and are covered by the
      core's unit tests; the on-screen reorder is in the hand-off plan.
- [x] **Header signatures match Rust exactly**: BUILD SUCCEEDED with zero
      warnings, and the round trip carries real data — `year`/`has_year`,
      `track_count` (now shown on every tile) and `artwork_path` all arrive
      with the right values, which a layout mismatch would garble. Header and
      `src/ffi/media_library.rs` compared field by field including `_pad[6]`.

## Phase 11 — Task 6: album gallery view + navigation

New files: `MLAlbumGallery.swift` (the gallery `LazyVGrid` + zoom/sort
header + album cell). Modified: `SparkampModelTypes.swift` (`AlbumGroup`,
`AlbumFilter`), `SparkampModel.swift` (`mlSelectedAlbum`),
`SparkampModel+MediaLibrary.swift` (`loadAlbums(sort:)`,
`albumTracks(album:albumArtist:)`), `MediaLibraryWindow.swift` (`.albums`
nav case, "Albums" sidebar row, album-filter honoring in `reload()`, the
"back" chip in the Files toolbar). Consumes the three FFI symbols from Task
3 (`sparkamp_ml_album_count`/`sparkamp_ml_albums`/`sparkamp_ml_album_tracks`)
— no new FFI in this task.

- [x] **Gallery renders real albums**: 133 tiles with title, artist and year
      under each. The years exposed defect 2 — they printed as "2,014" /
      "2,010" / "2,016" until `Text(verbatim:)` replaced the
      `LocalizedStringKey` interpolation.
- [ ] **Grouping matches the album-artist toggle**: DEFERRED to the hand-off
      plan. The fixture is in the library — "Covers From Another Mother" has a
      blank `album_artist` and artist "Falty & the Defects", so it reads
      "Unknown Artist" with the toggle off and should read the artist name
      with it on — but the walk itself was not run this pass.
- [x] **No-art placeholder**: albums with no cached artwork show the
      50%-opacity app icon over a dimmed backing — the same treatment as
      `ArtworkWindow.swift` / the A1 panel. Most of this library has no art,
      so this was the common case all pass; no broken images, no crash.
- [x] **Cover loads lazily and doesn't block scrolling**: scrolled the full
      133-tile grid to the bottom (ZZ Ward, then the "(no album)" bucket).
      Covers appear as tiles come on screen, no stall.
- [x] **Zoom resizes cells and persists**: −/＋ stepped 160 → 192 → 224 with
      the grid reflowing live, and `sparkamp.gallery.thumbPx` in
      `dev.sparkamp.SparkampMac` tracked each step. A later relaunch came up
      at the stored 96 px, so the value survives a quit. (The header control
      is −/＋ buttons, not the Slider this item was written against — see the
      navigation-polish section below.)
- [ ] **Sort picker reorders**: DEFERRED to the hand-off plan. Artist order
      was verified in detail (see the Task 3 items) and the "(no album)"
      bucket does sort last, but Album and Year were not switched on screen.
- [x] **Tap → correct tracks in Files**: single click opens the album's
      tracks. "Covers From Another Mother" → its 1 track; "(no album)" → all
      143. The back affordance is a "‹ Albums" button, not the "◀ <album
      name>" chip this item was written against — the navigation-polish
      section below supersedes it. Re-selecting "Files" in the sidebar
      restores the full library.
- [x] **Search escapes the album filter**: the Files search field is present
      while drilled in and typing into it clears `mlSelectedAlbum`, dropping
      back to a normal library search. Verified by reading the wiring plus
      the on-screen presence of the field; the album filter's own escape
      hatches (‹ Albums, Files row) were both exercised directly.
- [ ] **Play Album / Enqueue Album**: DEFERRED to the hand-off plan. The
      context menu is wired to the same `mlReplacePlaylistWith` /
      `mlAddToPlaylist` calls the playlist editor's buttons use, but was not
      opened on hardware.
- [ ] **Persistence divergence (eyeball only, not a bug)**: mac persists
      gallery zoom/sort via `@AppStorage` (`UserDefaults`, matching the
      existing `sparkamp.ml.sidebarWidth` idiom for other ML window prefs),
      while GTK persists the equivalent `gallery_thumb_px`/`gallery_sort`
      fields in the shared TOML config (`src/config.rs`). This is
      intentional — mac's window-level UI prefs have never round-tripped
      through the shared config file — but it means the zoom/sort chosen on
      one platform does not carry over to the other. Nothing to fix here;
      just confirm this is the expected, accepted behavior during the pass.

## Phase 11 — gallery navigation polish

Parity with the GTK interactive-pass fixes (`fix(gtk): album gallery
navigation polish`).

- [x] **Single click opens an album**: confirmed — one click on a tile opens
      its tracks.
- [x] **Back-to-albums button**: the "‹ Albums" button sits at the far left
      of the Files toolbar, before the search field, and only while a
      drill-down is active. Clicking it returned to the 133-tile gallery
      overview (not the unfiltered Files list) with the filter cleared.
- [x] **Albums sidebar returns to the overview**: clicking "Albums" while
      drilled in clears `mlSelectedAlbum` and lands on the overview.
- [x] **Zoom is −/＋ buttons with a "Zoom" label**: header reads `−  Zoom  +`,
      no pixel size, no slider. Steps of 32 confirmed (160 → 192 → 224) and
      "−" renders disabled at the 96 floor.
- [x] **Please-wait on zoom**: a spinner appears left of the zoom controls
      during the re-layout and is gone by the next frame set (~0.35 s). No
      flicker; the new tile size renders.

### Added this pass (not in the original plan)

- [x] **Album search**: a search box in the Albums view, same widget and same
      toolbar slot as the Files one. Typing filters the grid on the displayed
      title/artist and the header count follows ("ward" → 7 of 133). Empty
      result says "No albums match your search." rather than the
      add-a-folder message. Persisted per F12.1 under the `"albums"` view id:
      seeded from config on open, and it survives a drill-down round trip
      (this last part only after the `.id()` fix — see defect 4 above).
- [x] **Track-count badge**: a small pill in the bottom-right of every cover
      showing how many of that album's tracks are in the library. White on
      65%-black so it stays legible over any cover; values matched the DB.
      Mirrored into GTK (`.album-cell-count` + a `gtk4::Overlay` around the
      cover `Image`) — **that half is not compiled**, see the GTK note above.

## Phase 12 — F15 View/Search Lyrics

### 2026-08-01 revision (window always opens; modes; search button)

- [ ] **FFI signature match**: `sparkamp_lyrics_view(const char *path, const char
      *artist, const char *title, const char *album_artist)` in `sparkamp_bridge.h`
      matches `src/ffi/lyrics.rs` byte-for-byte; returns heap JSON
      `{"title","body","has_body","search_url"}` (body "" when none); freed with
      `sparkamp_free_string`; NULL path → NULL. (Old `sparkamp_lyrics_action` is
      gone — confirm nothing else references it.)
- [ ] **Five surfaces + A1**: "View/Search Lyrics" still on Files, playlist
      editor, device files, disc tracks, and active-playlist rows. The A1 now-
      playing "Lyrics" button appears ONLY on the LAST ID3/tags carousel page
      (not persistently below the panel) and opens the window in **Now-playing**
      mode.
- [ ] **Always opens**: a track with NO saved USLT still opens the window,
      showing "No lyrics available" (never silently browser-searches).
- [ ] **Marquee title**: window title = `Lyrics — <artist> - <track>`, artist→
      album_artist, track→filename stem (matches the scrolling marquee).
- [ ] **Modes** (segmented picker at the bottom): opening from a playlist/ML row
      defaults to **This song** (static). Opening from the A1 affordance defaults
      to **Now playing** — title + body follow the playing track (driven by
      `nowPlayingNonce` onChange → `refreshCurrentLyricsIfNeeded`). Toggling to
      **Now playing** retargets immediately.
- [ ] **Search button**: opens DuckDuckGo for `"<artist> <track> lyrics"` (SPACE-
      separated, NOT dash), artist→album_artist, filename when both blank;
      `&`/`/`/unicode encode correctly (space → %20).
- [ ] **Edit in tag editor**: still opens the ID3 editor for the shown track.
- [ ] **Panel truncation**: the A1 ID3 "Lyric" row is capped at 200 chars + '…'
      (comes from the core snapshot — no mac-side truncation, just verify it shows
      truncated while the window shows the full text).
- [ ] **Transport keys**: z/x/c/v/b/j/r/s still control playback while the lyrics
      window is focused — covered by the app-wide `NSEvent` monitor
      (`SparkampModel+Keys.swift`). Verify: selecting text in the lyrics body
      (NSTextView first responder) is the one case the monitor yields to — eyeball
      whether that is acceptable (GTK forwards via the window key controller).
- [ ] **Build**: no new Swift files (edited existing `LyricsWindow.swift`,
      `SparkampModel+Lyrics.swift`, `PlayerWindow.swift`, `SparkampModel.swift`),
      so `project.pbxproj` is unchanged; the app builds against `sparkamp_lyrics_view`.

- [ ] **PARITY GAP (follow-up, not a regression)**: the GTK ID3 editor has a
      "Lyric" (USLT) field; the mac editor's `ID3FieldConfig.defaults`
      (Id3EditorWindow.swift) has NO USLT field (only "Lyricist"/TEXT), so it
      cannot view/edit lyrics at all. GTK's F15 fix force-shows its Lyric field
      when "Edit in tag editor" is used from the lyrics window (point 2); the mac
      equivalent needs a USLT field added to the editor + FFI get/set support
      first. Deferred — decide on a Mac whether to add it.
