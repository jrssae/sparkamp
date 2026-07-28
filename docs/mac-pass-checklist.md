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
      **Decided/implemented in Phase 9 below (2026-07-28): mac now calls the
      shared `sparkamp_disc_read_cdtext` FFI (drutil-cdtext parse path), same
      as GTK's cdrdao path. See Phase 9 for the verification items and the
      DiscRecording-framework fallback if the drutil parse doesn't hold up
      on real hardware.**
- [ ] **Eject unmount (Linux fix, verify mac path):** GTK eject failed
      "must be superuser to unmount" on a mounted data disc; fixed by
      udisks-unmounting first. macOS `drutil eject` — confirm it ejects a
      mounted data disc without a similar error (drutil usually handles it).

## Phase-0 fixes: ID3 editor extended + passthrough frames (2026-07-17) — mac verify
- [ ] Mac ID3 editor's standard fields — Composer, Copyright, Encoded-by (and
      Original Artist, URL, Lyrics if exposed in the UI) — save via
      `sparkamp_tag_set`/`sparkamp_tag_save` and survive a close/reopen of
      the file (round-trips through `TagFields`, not silently dropped).
- [ ] Customize panel: add a frame not covered by the standard fields (e.g.
      Publisher/TPUB, Key/TKEY, Mood/TMOO, Language/TLAN, ISRC/TSRC,
      Subtitle/TIT3) via `sparkamp_tag_set`, save, close, and reopen the
      file — confirm the value survives (passthrough via
      `write_extra_frame`, not just held in memory until close).
- [ ] Setting a Customize frame, then reading it back via
      `sparkamp_tag_get` **before** saving, shows the just-set value (pending
      writes must win over what was loaded from disk).
- [ ] Setting a standard field and a Customize frame together, then saving
      once: both persist (the extra-frame write path runs after the main
      `write_tag_fields` call and doesn't clobber it).

## Phase-0 fixes: playlist auto-scroll to current track (2026-07-17) — mac verify (D8, BLIND)
- [ ] Playlist scrolls to the playing row on every track change: auto-advance
      to the next track, `z`/`b` (prev/next), and double-click a different
      row to play it — the newly-current row should end up visible without
      manual scrolling.
- [ ] While the same track keeps playing, manually scroll the playlist away
      from the current row (e.g. to look at a track further down) — confirm
      the view does NOT get yanked back to the current row on subsequent
      `updateNSView` passes (selection changes, tag edits, etc. must not
      re-trigger the scroll).
- [ ] Scrolling to a very long playlist's last track (auto-advance reaching
      the final row) actually reveals that row — no off-by-one against
      `table.numberOfRows`.
- [ ] Confirm `ActivePlaylistTable.Coordinator.lastScrolledIndex` compares
      against `model.currentIndex` (a stable playlist id), not a raw row
      number — reordering the playlist via drag should not cause a spurious
      re-scroll purely from a row-index shift while the same track plays.
- [ ] Stop playback, scroll the playlist away from the (former) current
      row, then play that same track again — confirm the view scrolls back
      to it (the guard resets on stop, so replaying the same track re-fires
      the scroll instead of being treated as "already scrolled there").

## Phase-0 fixes: EQ frequency labels removal (D10) — mac verify
- [ ] EQ window shows 10 unlabeled sliders matching GTK, column spacing intact.

## Phase-1: ML technical columns + ID3 tech line (Task 7, BLIND — Swift never compiled)
- [ ] `xcodebuild` succeeds with zero errors/warnings against the updated
      `sparkamp_bridge.h` — `SparkampLibTrack` grew six trailing fields
      (`sample_rate`, `file_size`, `added_at`, `file_mtime`, `bitrate_mode`,
      `channels`); confirm the Swift `MLTrack.init(from:)` field reads still
      line up byte-for-byte with the Rust struct (no silent offset drift).
- [ ] Files view column picker (toolbar icon, `MediaLibraryWindow.swift`)
      shows five new toggles below the existing "Last Played" entry: Sample
      Rate, Size, Date Added, File Modified, Mode — all off by default
      (bits 17–21 aren't in the default `columnMask`), confirm each toggles
      its column's visibility independently and the layout/divider looks
      right.
- [ ] Column content, once shown: Sample Rate renders "44.1 kHz" style (or
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
- [ ] Click each of the five new column headers: table re-sorts via
      `sortDescriptorsDidChange` → `MLFilesTable.keyPathComparator` →
      `MediaLibraryWindow.reload()`'s `colName` switch → `mlFetchTracks`
      with the matching `sortCol` ("sample_rate" / "file_size" /
      "added_at" / "file_mtime" / "bitrate_mode") — confirm ascending AND
      descending both actually reorder rows (not just flip the header
      arrow).
- [ ] These columns also appear in the Saved Playlist editor
      (`MLEditorTable.swift`, which reuses `MLFilesTable.specs` /
      `.cellContent` directly) — confirm they render there too, not just
      in the Files view.
- [ ] Existing columns (Title through Last Played) are visually and
      functionally unaffected — spot-check a few sorts/toggles pre- and
      post-change.
- [ ] ID3 editor: open a file that IS indexed in the library (e.g. via the
      Files view's "Edit / View ID3 Tags") — confirm a dimmed technical
      line appears under the field grid reading uppercase filetype ·
      bitrate ("320k" style, not "320 kbps") · sample rate · channels
      (mono/stereo/Nch) · duration (M:SS), " · "-joined, matching what
      GTK's ID3 editor shows for the SAME file (GTK's `tech_summary`).
- [ ] ID3 editor: open a file NOT indexed in the library (e.g. a playlist
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
- [ ] Saving ID3 tags on a file does not change/blank the tech line
      (technical fields are independent of tag fields; the editor closes
      ~0.4s after a successful save, so this is mostly a "no crash /
      no flicker to blank" check during that window).

## Phase 2 — 2026-07-20: now-playing FFI + artwork set/clear + ML art path (Task 12, BLIND — Swift never compiled)
- [ ] `xcodebuild` succeeds with zero errors/warnings against the updated
      `sparkamp_bridge.h` (new: opaque `SparkampNowPlaying` + its 10
      `sparkamp_now_playing_*` functions; new: `sparkamp_tag_set_artwork`,
      `sparkamp_tag_clear_artwork`; changed: `SparkampLibTrack` gained
      `artwork_path[512]` right after `has_art` — verify every existing
      field read by Swift after `has_art` still lines up positionally).
- [ ] Now-playing panel (A1): on each track-change notification, call
      `sparkamp_now_playing_open`, read all fields, then
      `sparkamp_now_playing_close` — confirm it returns NULL gracefully
      when nothing is playing (panel should show its empty state, not crash).
- [ ] Panel's curated tag rows (`sparkamp_now_playing_tag_count` /
      `_tag_label` / `_tag_value`) match GTK's A1 panel for the SAME file:
      same labels, same order, only non-empty fields shown, filename-stem
      fallback title when a file has no usable ID3 text at all.
- [ ] `sparkamp_now_playing_tech_line` matches the ID3 editor's tech line
      for the same file (shared `tech_summary` under the hood).
- [ ] `sparkamp_now_playing_artwork_path` resolves to the same file GTK's
      A1 panel shows (embedded APIC dump / folder image / library cache),
      and is "" when there is no art — panel shows its no-art placeholder,
      not a broken image.
- [ ] `sparkamp_now_playing_has_play_count` / `_play_count` / `_last_played`:
      an indexed (media-library-scanned) track shows real stats; a track
      played from outside the library (e.g. Testing dir, ad-hoc file) shows
      the "not yet played" / no-stats state instead of 0 or garbage.
- [ ] `sparkamp_now_playing_artist_wiki_url` / `_album_wiki_url` open the
      correct Wikipedia search page (percent-encoded, spaces as `%20`) for
      the current artist/album; empty tag → link is hidden/disabled, not a
      broken URL.
- [ ] ID3 editor: setting a new cover image now calls
      `sparkamp_tag_set_artwork` + `sparkamp_tag_save` — confirm the saved
      file actually embeds the APIC frame (inspect with GTK or `id3v2 -l`)
      and the mac editor's art preview updates immediately after save.
- [ ] ID3 editor: clearing/removing the cover now calls
      `sparkamp_tag_clear_artwork` + `sparkamp_tag_save` — confirm ALL
      embedded pictures are gone afterward, not just hidden in the UI.
- [ ] Set-then-clear-then-set-again on the same file round-trips cleanly
      (no leftover/duplicate APIC frames after repeated saves).
- [ ] Media Library table: add an art thumbnail/indicator column driven by
      `SparkampLibTrack.artwork_path` (fall back to `has_art` alone if no
      thumbnail rendering is wired yet) — confirm it populates for scanned
      tracks with cached art and stays blank for tracks without any.
- [ ] Saved Playlist editor's track rows (same `SparkampLibTrack` source)
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

## Phase 2 — 2026-07-20: A1 panel, A6 window, ML art column, D14 art edit, w/k shortcuts (Task 13, BLIND — Swift never compiled)

Swift files touched: `PlayerWindow.swift` (A1), `ArtworkWindow.swift` (A6),
`MLFilesTable.swift` (A2), `Id3EditorWindow.swift` (D14),
`SparkampModel.swift` / `SparkampModelTypes.swift` (state + `NowPlayingInfo`),
`SparkampModel+Keys.swift` (w/k), `SparkampModel+MediaLibrary.swift`
(`mlViewArtForPath` follow-mode fix), `KeyboardShortcutsView.swift` (w/k rows).
No FFI/bridge.h changes — Task 12's surface was already complete.

### Build
- [ ] `xcodebuild` succeeds with zero errors/warnings. This task added the
      most speculative SwiftUI constructs of the phase — see "Unsure /
      eyeball" below before assuming a clean build means correct behavior.

### A1 — expandable now-playing panel
- [ ] The marquee row (Row 1 of the info panel) now has a small chevron
      button at its right edge; clicking it toggles the panel exactly like
      pressing `w`, and the chevron flips (down = collapsed, up = expanded).
- [ ] `playerExpanded` persists across relaunch via
      `UserDefaults["sparkamp.playerExpanded"]` (same mechanism as
      `playlistVisible`/`equalizerVisible`/`mediaLibraryVisible`) — restored
      in `SparkampModel.init()`, written in both the `w`-key handler, the
      chevron button, and `saveState()`.
- [ ] Collapsed layout is pixel-identical to pre-Task-13 (nothing new renders
      when `playerExpanded == false` beyond the chevron itself).
- [ ] Expanded: art (~100×100, clamped) appears on the left of the panel row,
      a data carousel on the right, page dots beneath the carousel when there
      is more than one page.
- [ ] **Window resize**: confirm the player window's height actually grows on
      expand and shrinks back on collapse. This relies entirely on
      `.windowResizability(.contentSize)` (`SparkampMacApp.swift`) picking up
      the SwiftUI ideal-size change with NO extra `NSWindow` code (unlike
      GTK's manual `set_default_size` + `queue_resize` re-kick) — this is the
      single biggest "does the SwiftUI construct actually do what the doc
      says" bet in this task; if the window does NOT resize, the fix is
      almost certainly `.fixedSize()` somewhere upstream fighting it, not
      the panel code itself.
- [ ] Visualizer (left column, mini bars/waveform/Granite) visibly grows
      taller when the panel expands (it relies on the same HStack-sizing
      side effect as the resize above — the left column has no explicit
      height, only `maxHeight: .infinity` on the `VisualizerView`).
- [ ] Carousel pages match GTK's grouping/order for the same file: tag rows
      chunked 4-per-page (curated order), then Technical (tech line), then
      Stats (play count / last played — only if the track is library-indexed
      or has a last-played value), then Links (artist/album Wikipedia) — a
      page is omitted entirely when its data is all empty, not shown as a
      blank page.
- [ ] Carousel auto-advances every 6 s via `Timer.publish`; clicking a dot
      jumps directly to that page. NOTE: unlike GTK, a manual dot click does
      NOT push out the next auto-advance (GTK's `jump()` doubles the dwell so
      a manual pick lingers) — the mac timer just keeps advancing on schedule
      regardless. Confirm this reads as acceptable UX or file a follow-up.
- [ ] Switching tracks resets the carousel to page 0 (`onChange(of: trackKey)`
      where `trackKey == model.currentIndex`).
- [ ] No artwork: the panel shows the dimmed app-icon + "No artwork
      available" placeholder (matches the A6 window's placeholder wording).
- [ ] Clicking the panel's art (or its placeholder) opens/focuses the A6
      album-art window in follow-mode (same as pressing `k`).
- [ ] Last-played timestamps in the Stats page render as local
      "yyyy-MM-dd HH:mm" (same formatting as the ML table's `lastPlayedDisplay`).

### A6 — standalone album-art window (singleton, follows current track)
- [ ] `k` opens the window if closed, or brings it to front if already open
      (open-or-focus, not toggle — repeat `k` presses never do nothing).
- [ ] While open in follow-mode, changing tracks (next/prev/EOS/jump) updates
      the displayed art live, including flipping to the "No artwork
      available" placeholder when the new track has none.
- [ ] Opening the window via the ID3 editor's artwork thumbnail tap, or the
      Media Library's "View Art" action, shows that SPECIFIC track's art and
      does NOT get silently replaced by the currently-playing track's art a
      moment later (this is the `artworkFollowsPlayback` flag — verify it
      actually stays false for these two entry points and only becomes true
      via `k` / the A1 art tap).
- [ ] Closing the window (Esc / red button) always resets follow-mode off,
      so the next `k` press cleanly re-enters follow-mode rather than
      inheriting stale state.
- [ ] Fullscreen visualizer: `k` is inert while fullscreen is up (added to
      the same disabled-keys list as `p`/`i`/`u`/`d`, so it doesn't yank
      focus out of the fullscreen Space).

### A2 — Media Library artwork thumbnail column
- [ ] The "Art" column in the Files view (`MLFilesTable`) shows a small
      (18×18) rounded thumbnail image for tracks whose `artwork_path` resolves
      to a loadable image, instead of just a "View" text link.
- [ ] A track marked `has_art` but whose thumbnail failed to decode falls
      back to the pre-existing "View" text link (not a blank cell) — the
      pre-Task-13 behavior for that edge case is unchanged.
- [ ] Tracks with no art at all still render a blank cell.
- [ ] Clicking the thumbnail (or the "View" fallback) still opens the
      artwork viewer exactly as before.
- [ ] **Performance**: scroll a large Files view (thousands of rows) with the
      Art column visible — `NSImage(contentsOfFile:)` runs directly in the
      cell-content builder with no caching/lazy-generation (unlike GTK's
      Task 8, which explicitly caches + backgrounds thumbnail generation via
      `thumb_path_for`). NSTableView only builds cells for visible rows, so
      this should be fine in practice, but confirm there's no visible
      scroll jank with a large, art-heavy library. If there is, the fix is a
      small `NSImage` decode cache keyed by path — not a redesign.
- [ ] Same column in the Saved Playlist editor (`MLEditorTable.swift`, which
      reuses `MLFilesTable`'s specs/cellContent) — confirm the thumbnail
      renders there too (not separately touched this task; verify the reuse
      picked it up for free).

### D14 — ID3 editor artwork Browse / Clear
- [ ] The artwork slot in the ID3 editor now ALWAYS shows something (a
      thumbnail, or a "No art" placeholder box) instead of collapsing to
      nothing when a file has no embedded art — confirm the left/right field
      columns' spacing looks right in both states (padding was hardcoded to
      0 now that the slot is never absent).
- [ ] "Browse…" opens an NSOpenPanel restricted to images; picking a file
      updates the on-screen thumbnail immediately (before Save).
- [ ] "Clear" blanks the thumbnail immediately (before Save) and is disabled
      when there's no artwork to clear.
- [ ] Neither Browse nor Clear touches the file on disk until "Save" is
      pressed — `sparkamp_tag_set_artwork` / `sparkamp_tag_clear_artwork` are
      only called from `saveTag()`, mirroring how text-field edits are
      buffered in `fieldValues` and only pushed to the tag ctx at Save time.
- [ ] Save with no Browse/Clear touch (`pendingArtworkPath == nil`) does NOT
      strip existing embedded art — confirm a file's art survives an
      edit-and-save that never touched the artwork controls.
- [ ] Browse → Save → reopen the same file: new art is embedded (inspect
      with GTK's ID3 editor or `id3v2 -l`) and the mac editor shows it.
- [ ] Clear → Save → reopen: all embedded pictures are gone.
- [ ] Browse/Clear buttons are hidden for read-only and missing files (same
      gate as the Save button: `!isReadOnly && !fileMissing`).
- [ ] Loading a different file (Customize… aside) resets any unsaved
      Browse/Clear buffer from the PREVIOUS file (`pendingArtworkPath = nil`
      in `loadTag()`) — confirm switching files via the editor's reload path
      doesn't leak a pending change onto the wrong file.
- [ ] Not implemented for mac (scope call, see Task 9 GTK-only): the
      "Also write folder image" checkbox. GTK has it; mac's D14 spec only
      asked for Browse/Embed/Clear. Flag if this asymmetry should be closed.

### Shortcuts (3-file rule)
- [ ] `KeyboardShortcutsView.swift`'s `sections` list now shows `w` → "Toggle
      now-playing panel (art, tags, links)" and `k` → "Open album-art window"
      under "Playlist & modes" (mac's closest analog to GTK's "View & Tags"
      section, which mac doesn't have — GTK's `d`/`u` rows also aren't listed
      anywhere in mac's shortcuts view; that's a pre-existing gap, not
      something this task introduced or was asked to fix).
- [ ] `SparkampModel+Keys.swift`'s `handleRawKey` handles lowercase `w`
      (toggle `playerExpanded` + persist) and `k` (`openArtworkWindow()`) —
      both no-op with modifier keys held, matching every other single-key
      shortcut.
- [ ] Both keys are inert while a text field has focus (covered for free by
      the existing `NSTextView` firstResponder guard) and while the
      Jump-to-Track overlay is showing (existing `jumpVisible` guard).

### Unsure / eyeball (blind pass — flag anything that doesn't compile or look right)
- [ ] `.windowResizability(.contentSize)` auto-growing the window on
      `playerExpanded` toggle with zero extra `NSWindow` code — the biggest
      "trust SwiftUI" bet in this task (see A1's resize item above).
- [ ] `switch pages[safeIndex] { case .tags(...): ... }` written directly as
      `@ViewBuilder` content (mirrors the existing `switch nav { ... }` in
      `MediaLibraryWindow.swift`, so it should compile, but the carousel's
      case bodies are new).
- [ ] `.task(id: info?.artworkPath ?? "")` for debounced image reload,
      `.onReceive(Timer.publish(...).autoconnect())` for the carousel timer,
      and `.onChange(of: pages.count)` for the page-count safety clamp — all
      standard SwiftUI, but this is their first use in this codebase; eyeball
      that the 6 s cadence feels right and the timer doesn't drift/pile up
      after the window has been open a long time.
- [ ] `NowPlayingPanel` declares its own `@EnvironmentObject var model` and
      `@EnvironmentObject var themeManager` — confirm both are actually in
      scope where it's instantiated inside `PlayerWindow`'s body (they should
      be, since `PlayerWindow` itself receives both via the WindowGroup's
      `.environmentObject` calls in `SparkampMacApp.swift`, and environment
      objects propagate to any descendant view without re-declaring them at
      each level).
- [ ] `Link("Artist on Wikipedia", destination: url)` — confirm it actually
      opens the system browser from inside this app's window context (no
      reason it wouldn't, but it's the first `Link` use found in this
      codebase's mac sources).
- [ ] The ID3 editor's artwork slot padding (now hardcoded `0` instead of the
      old `artwork == nil ? 12 : 0` ternary) — eyeball the left-column
      alignment now that the slot is never absent.

## Phase 3 — 2026-07-21: Now Playing + remote commands (P3-T6, BLIND)

New file `SparkampModel+NowPlaying.swift` (added to project.pbxproj: fileRef AA4…00A1 / buildFile AA5…00A1) + hooks in SparkampModel.swift (updateNowPlayingCenter from refreshCurrentTrackInfo + tick play-state change). Verify on hardware:

- [ ] Control Center / lock-screen Now Playing card shows title, artist, album, artwork, duration for the playing track.
- [ ] Card updates on track change (title/art) and on play/pause/stop (state/rate).
- [ ] Elapsed time advances (macOS extrapolates from rate); pausing freezes it.
- [ ] Hardware media keys (play/pause, next, previous) work with the app unfocused.
- [ ] AirPods play/pause tap + double-tap next / triple-tap previous act on Sparkamp.
- [ ] Control Center scrubber seeks; the app seek bar reflects it (and vice-versa — app seek elapsed may lag one card update, accepted).
- [ ] No-track / stopped → card clears (nowPlayingInfo nil, playbackState .stopped).

**Unsure / eyeball (blind, no Xcode here):**
- New Swift file compiles + is actually in the build target (pbxproj entries added by hand — confirm Xcode sees it; IDs AA4…00A1 / AA5…00A1 chosen unused).
- `import MediaPlayer` on macOS + MPRemoteCommandCenter with no explicit audio-session entitlement (macOS doesn't require the iOS AVAudioSession; confirm commands fire).
- `MPMediaItemArtwork(boundsSize:) { _ in image }` closure returns the NSImage at any requested size (returns the full image regardless of size — verify it renders, not blank).
- Album extracted from `nowPlaying.tags` where label == "Album" (matches the core curated label).

## Phase 4 — 2026-07-22: ReplayGain (P4-T8, BLIND)

Rust FFI (built + tested on Linux: 481 lib + 685 bin, 0 warnings) — 6 config
get/set pairs + a background analysis trigger, mirrored into
`sparkamp_bridge.h`. Swift edits are all in EXISTING files (no new source →
**no project.pbxproj changes needed**, unlike phases 2/3):
`SparkampModelTypes.swift`, `SparkampModel.swift`, `SparkampModel+MediaLibrary.swift`,
`SettingsWindow.swift`, `MLFilesTable.swift`, `MediaLibraryWindow.swift`.

Verify on hardware:

- [ ] Settings → Playback → ReplayGain: "Use ReplayGain", Gain source
      (Track/Album/Automatic), "Prevent clipping", "Fallback gain" stepper all
      load current values on open and persist across a relaunch.
- [ ] Toggling "Use ReplayGain" (or changing source/clip) while **stopped**
      reshapes the chain immediately; while **playing** it takes effect on the
      next track (engine defers — expected, matches GTK/TUI).
- [ ] Loud vs quiet tracks even out in perceived volume with ReplayGain on;
      turning it off restores raw levels.
- [ ] Settings → Media Library → ReplayGain: "Analyze ReplayGain" runs a
      background job; progress bar shows "Analyzing N/M…"; "Cancel Analysis"
      replaces the buttons while running and stops the job.
- [ ] "Force Recalculate" reanalyzes every track (ignores stored values).
- [ ] "Analyze new files on add/scan" and "Write ReplayGain tags to files
      (MP3 only)" toggles load + persist.
- [ ] With write-tags ON, analyzing an MP3 writes REPLAYGAIN_* TXXX frames to
      the file (visible to other taggers); non-MP3 files silently keep DB-only
      values.
- [ ] Media Library Files view → columns menu (tablecells icon) has a
      "ReplayGain" entry (off by default); enabling it shows a "ReplayGain"
      column with e.g. "-6.2 dB", empty for un-analyzed tracks.
- [ ] Sorting by the ReplayGain column works (server-side "rg_gain" order).
- [ ] Right-click one or more Files rows → "Calculate ReplayGain" force-
      analyzes the selection; the column updates when the job finishes;
      the item is disabled while an analysis is already running.

**Unsure / eyeball (blind, no Xcode here):**
- SparkampLibTrack struct field order in `sparkamp_bridge.h` must match the
  Rust `#[repr(C)]` exactly — the 5 new fields (rg_track_gain/peak,
  rg_album_gain/peak as `double`, rg_analyzed as `int32_t`) were appended
  AFTER `channels` in both; confirm no misalignment (wrong gains/garbage would
  signal a mismatch).
- `Stepper("Fallback gain: \(rgFallback, specifier: "%.1f") dB", ...)` — first
  interpolated-specifier Stepper title in this file; confirm it renders.
- RG progress polling was added to `SparkampModel.tick()` alongside the scan
  poll; confirm `rgRunning`/`rgDone`/`rgTotal` drive the Settings progress row
  and clear on completion, refreshing the column.
- Column bit 22 (ReplayGain) is beyond the previous max bit 21; `columnMask` is
  a plain `Int` (AppStorage) so bit 22 is fine — confirm the toggle persists.
- `sparkamp_rg_analyze_selection` takes an `int64_t *ids` array; Swift passes
  it via `withUnsafeBufferPointer`. Confirm large selections analyze correctly.
- Known limitation (matches GTK/TUI): sort by ReplayGain treats un-analyzed
  tracks as 0.0 dB (no sort-key shift like GTK's), so they interleave with
  reference-level tracks. Cosmetic.

## Phase 5 — 2026-07-22: Manual play queue (P5-T8, BLIND)

Rust FFI (built + tested on Linux: 494 lib + 699 bin, 0 warnings) — 8 queue
symbols in `src/ffi/queue.rs`, mirrored into `sparkamp_bridge.h`. The queue
lives in `ctx.queue`; the FFI advance seam (`sparkamp_nav_next` /
`sparkamp_advance_after_eos`) already drains it ahead of shuffle/linear, so
`next()` / `handleEOS()` → `refreshAll()` → `refreshPlaylist()` renumber the
badges automatically.

Swift edits are all in EXISTING files (no new source → **no project.pbxproj
change**): `SparkampModelTypes.swift` (PlaylistItem.queuePos + queueBadge),
`SparkampModel.swift` (queueVisible, refreshQueueBadges, queuePos in the two
playlist refresh sites), `SparkampModel+MediaLibrary.swift` (queuedItems +
queueToggle/Move/Clear/Shuffle/PlayNow), `PlaylistView.swift` (badge prefix +
"Queue / Dequeue" context item), `SparkampModel+Keys.swift` (`q`), `SparkampMacApp.swift`
(Queue window scene), `PlayerWindow.swift` (queueVisible → open/dismiss),
`JumpToTrackView.swift` (new `QueueView`), `KeyboardShortcutsView.swift`.

Verify on hardware:

- [ ] `q` opens the Play Queue window; `q` again (or Esc) closes it.
- [ ] Right-click one or more playlist rows → "Queue / Dequeue" adds/removes
      them; the `[n]` badge appears/updates on the playlist rows immediately.
- [ ] Badges renumber as the queue drains during playback (queued tracks play
      before shuffle/linear, then playback resumes from that position).
- [ ] Queue window: rows listed in order "1. Artist — Title"; Up / Down reorder
      the selected entry; Remove dequeues it; Clear empties; Randomize shuffles.
- [ ] Double-click a Queue-window row → plays it now (dequeues + jumps + plays).
- [ ] Queue survives shuffle toggling; a queued track still wins, then shuffle
      resumes.
- [ ] Removing a queued track from the playlist drops it from the queue
      (badge disappears; queue count decreases).

**Unsure / eyeball (blind, no Xcode here):**
- `QueueView` uses `List(selection:)` with `PlaylistItem.ID` (= Int playlist
  index), `.onTapGesture(count: 2)` for play-now, `.onExitCommand` for Esc,
  `.scrollContentBackground(.hidden)` (macOS 13+). Confirm selection, double-
  click, and Esc all work and the theme colours apply.
- Enqueue on mac is via the row **context menu** (the app-wide key monitor in
  `SparkampModel+Keys.swift` guards `!hasModifiers`, so a global Ctrl+Q can't be
  routed, and the playlist selection lives in the view, not the model). If a
  Ctrl+Q shortcut is wanted later, lift the playlist `selection: Set<Int>` into
  the model or add a focused-view key handler. Not a regression — GTK/TUI keep
  Ctrl+Q; mac uses the context menu + Queue window.
- New Window scene id "queue" wired through `queueVisible` exactly like
  "jump-to-track" — confirm it opens/closes and doesn't fight fullscreen focus.
- `refreshQueueBadges()` mutates `playlistItems` only when a badge changed;
  confirm it doesn't churn SwiftUI re-renders during idle playback.

---

## Phase 6 — F9 shortcuts + dialog sweep (2026-07-26, blind)

New keys wired in `SparkampModel+Keys.swift` (raw handler) and the app
`Commands` menu (`SparkampMacApp.swift`). Stop-after-current is an engine flag
reached over FFI (`sparkamp_get/set_stop_after_current`) mirrored into
`@Published var stopAfterCurrent`.

- [ ] `m` toggles the Media Library window (open when hidden, close when shown).
- [ ] `t` arms stop-after-current: a small stop-square appears on the
      play/pause/stop indicator next to the time index (NOT on the play button);
      the "Stop After Current Track" menu item (Playback menu) toggles the same
      state.
- [ ] With `t` armed, the current track finishes → playback stops, badge clears.
- [ ] `t` twice = toggles off (playback continues to the next track).
- [ ] `t` armed with queued tracks → stops before the queue; next play resumes
      the queue.
- [ ] Manual stop (`v`), next (`b`), prev (`z`), and jumping to another track
      (double-click a row / jump window) clear the arming + badge.
- [ ] Pause then resume (`c`) KEEPS the arming + badge (must not clear).
- [ ] `n` opens the file picker (add file[s]); `Shift+N` opens the folder picker
      (add folder) — same as the playlist bottom-bar "Add Files"/"Add Folder".
- [ ] `⌘S` saves the active playlist (same as the bottom-bar Save button).
- [ ] `⌘,` opens Settings; `⌘I` inverts the playlist selection.
- [ ] `↑ ↓` still adjust volume; `← →` still seek (unchanged).
- [ ] Keyboard Shortcuts window (`i`) lists every new binding and each line is
      true (matches the GTK dialog content).

**Unsure / eyeball (blind, no Xcode here):**
- Play-button badge is a `.overlay(alignment: .bottomTrailing)` `Image("stop.fill")`
  as a `.overlay(alignment: .bottomTrailing)` on the state-icon `Image` beside
  the time display (`stateIcon`). Confirm it reads as a small badge on that
  indicator without clipping the time text, and uses `stateColor`.
- `⌘I` invert is a zero-size hidden `Button` in `PlaylistView.bottomBar` that
  sets `selection = Set(playlistItems.map { $0.id }).subtracting(selection)`.
  Confirm the shortcut fires while the playlist window is key and the table
  reflects the new selection.
- `⌘,` is attached to the "Settings" command button (toggles `settingsVisible`).
  Confirm it opens the Settings window and doesn't collide with a system pref.
- Stop-after-current is NOT persisted (transient), matching GTK/TUI.

---

## Phase 7 — Task 10: Winamp playlist menu bar + status line (2026-07-27, BLIND — Swift never compiled)

`PlaylistView.bottomBar` replaced the five flat buttons (Add Files, Add
Folder, Save, Remove, Remove All) with four SwiftUI `Menu`s — Add / Select /
Sort / List — over the same underlying actions plus the new phase-7 reorder
FFI (`sparkamp_playlist_sort/reverse/randomize`, wrapped as
`model.sortPlaylist(_:)` / `reversePlaylist()` / `randomizePlaylist()` in
`SparkampModel+Transport.swift`). The old count/duration header was replaced
by a single status line mirroring core `playlist_status_line`
(`src/playlist_status.rs`) via `PlaylistView.formatStatus`.

- [ ] **Add** menu opens; "Add Files…" / "Add Folder…" behave exactly as the
      old buttons (same file/folder pickers).
- [ ] **Select** menu opens (NOT disabled on an empty playlist — deliberately
      left enabled so its nested ⌘I keeps firing; only Sort and List are
      disabled-on-empty); "Select All" / "Select None" / "Invert Selection" set
      `selection` correctly against the currently loaded playlist.
- [ ] After a Sort/Randomize/Reverse, the selection is CLEARED (the index-based
      `selection` set would otherwise highlight whatever tracks landed on the old
      rows) — matches GTK, which clears selection on rebuild.
- [ ] **Sort** menu opens (disabled when playlist empty); Title / Artist /
      Album / Filename / Path each call `sparkamp_playlist_sort` with the
      matching `kind` (0–4) and the table re-renders in the new order.
- [ ] Randomize / Reverse (below the divider in Sort) reorder the playlist;
      in all five sort cases AND Randomize/Reverse, confirm:
      - the currently PLAYING track (waveform icon row) stays the same
        logical track after reorder — core keeps `current` pointed at the
        same entry across the shuffle-history reset, so this should hold,
        but verify visually since Swift is blind here;
      - queue `[n]` badges follow their tracks to the new row positions
        (`refreshAll()` → `refreshPlaylist()` re-reads `queuePos` per index
        from the ctx after the reorder, so badges should track correctly).
- [ ] **List** menu opens (disabled when playlist empty); "Save Playlist…"
      behaves exactly as the old Save button (same NSSavePanel flow);
      "Remove Selected" is disabled with an empty selection and removes
      exactly the selected rows; "Remove All" clears the playlist and the
      selection.
- [ ] Status line (top of the playlist window, where the old count/duration
      header sat) reads `"N tracks · MM:SS total"` with 0 selected rows, and
      `"N tracks · MM:SS total · MM:SS selected"` once ≥1 row is selected —
      confirm singular "1 track" with exactly one row, and H:MM:SS rollover
      once total (or selected) duration reaches an hour.
- [ ] `⌘S` (Save Playlist) and `⌘I` (Invert Selection) still work with the
      playlist window key — both `keyboardShortcut` modifiers now live on the
      `Button`s *inside* the List/Select `Menu` content (moved off the old
      hidden zero-size buttons) rather than as standalone bottom-bar buttons.

**Unsure / eyeball (blind, no Xcode here):**
- `.keyboardShortcut` on a `Button` nested inside `Menu { ... }` content is
  expected to register the shortcut globally (SwiftUI hoists it into the
  window's command set) exactly like a standalone button did before — this
  is the one behavioral bet in this task; if `⌘S`/`⌘I` stop firing, move
  those two modifiers back onto small hidden top-level buttons in
  `bottomBar` (the pre-phase-7 pattern) instead of inside the menus.
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

## Task 3 — 2026-07-27: status bar on the four Media Library views (BLIND — Swift never compiled)

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

- [ ] Each of the four views shows `N tracks · MM:SS total`, directly below
      its list/table and above its control-button row, with nothing selected.
- [ ] Each adds `· K selected · MM:SS` (selected COUNT + duration) the
      moment ≥1 row is selected, and drops it again back to no
      selected-clause when selection clears.
- [ ] Format matches the active playlist exactly: singular "1 track" with
      exactly one row, `M:SS` under an hour, `H:MM:SS` at/above an hour, for
      both the total and the selected clause independently.
- [ ] The bar updates live on selection change (click/⌘-click/shift-click)
      and on list reload (rescan, add/remove tracks, playlist Save/Revert,
      device sync, disc swap) — no stale count/duration lingering after any
      of these.
- [ ] Playlist editor: confirm the status bar reflects the SEARCH-FILTERED
      view (type in "Search this playlist…" and confirm the count drops to
      match only matching rows), not the full unsearched playlist.
- [ ] Device detail: confirm the status bar reflects the selected playlist
      chip filter too (switch from "All files" to a device playlist chip and
      confirm the count matches just that playlist's entries).
- [ ] Disc-files browser: confirm the bar is only present for a non-blank
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

## Phase 8 — F10 watch folders (2026-07-27, BLIND — Swift never compiled)

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

- [ ] All 5 new toggles ("Watch folders for changes", "Automatically add
      played tracks", "Remove missing files on rescan", "Compact database
      after rescan", "Rescan all folders on startup") in Settings ▸ Media
      Library persist across a quit/relaunch and correctly reflect the
      saved value when the pane reopens.
- [ ] Each watched folder row in Settings ▸ Media Library ▸ Watched Folders
      shows a "Recurse" checkbox that reflects that folder's actual
      recurse setting (not a shared/global value) and, when toggled,
      changes only that folder's behavior.
- [ ] With "Watch folders for changes" ON: dropping a new audio file into a
      watched folder in Finder makes it appear in the Files view within
      ~2–5 seconds, with no manual Rescan needed.
- [ ] With "Remove missing files on rescan" ON: deleting a file from a
      watched folder in Finder removes its row from Files within a similar
      window. With it OFF: the row is KEPT (not marked broken/missing) —
      confirm no row silently vanishes when the toggle is off.
- [ ] Editing a tag externally (e.g. in another app) on a file already in
      the library updates that file's row (title/artist/etc.) live, without
      a manual rescan.
- [ ] Saving a tag edit from Sparkamp's own ID3 editor does NOT trigger a
      visible rescan/refresh storm — the watcher's cache-prefix / self-write
      suppression (Task 5) should make Sparkamp's own writes invisible to
      the watch-event drain.
- [ ] A non-recursive folder (Recurse OFF) ignores new files dropped into a
      SUBdirectory of that folder — they should not appear in Files until
      Recurse is turned on (or the subfolder is separately watched).
- [ ] With "Rescan all folders on startup" ON: quit Sparkamp, add a file to
      a watched folder from Finder while it's closed, relaunch — the new
      file is present in Files without a manual Rescan.
- [ ] With "Automatically add played tracks" ON: play a file that lives
      OUTSIDE every watched folder (e.g. via File ▸ Open or a drag-and-drop
      onto the player) — it should appear as a row in Files shortly after
      playback starts. With the toggle OFF, it should NOT appear.
- [ ] Play a file that's already INSIDE a watched folder — confirm no
      duplicate row is created (the inside/outside guard in
      `sparkamp_ml_note_played` should skip it entirely).
- [ ] With "Compact database after rescan" ON: run Rescan All (or trigger a
      startup rescan) after removing several watched folders/files, and
      confirm the on-disk DB file doesn't keep growing — a rough
      before/after file-size check is enough (exact shrink amount isn't the
      point, "doesn't just grow forever" is).
- [ ] Simulate a watcher start failure (e.g. remove/rename a watched folder
      out from under Sparkamp right as it's about to start watching, or
      revoke folder permissions) and confirm the app does NOT crash —
      it should silently fall back to manual/interval rescans (per
      `rebuild_watcher`'s documented degrade-gracefully contract).

---

## Phase 9 — CD-TEXT read (2026-07-28, BLIND — Swift never compiled)

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

- [ ] **CD-TEXT disc absent from gnudb**: insert an audio disc gnudb has no
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
      B's CD-TEXT read either succeeds cleanly or fails silently — the
      exclusive-read guard is held INSIDE the core `sparkamp_disc_read_cdtext`
      FFI call for its whole duration (per its bridge-header doc), so mac
      Swift does not wrap it with its own begin/end calls the way GTK's raw
      `disc::cdtext::read_cdtext` call does; confirm this built-in guard is
      actually sufficient on mac (i.e. it doesn't need a Swift-side
      `disc_reading`-style flag the way GTK's rip loop sets one).
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
- [ ] **Acquisition path + drutil dump capture**: confirm which path was
      actually used to acquire CD-TEXT for a live disc on real hardware.
      This task lands ONLY the FFI-based path (`sparkamp_disc_read_cdtext` →
      core `parse_drutil_cdtext`, mirrored in `DiscService.readCdtext`) — the
      DiscRecording-framework fallback (`DRDevice` + `DRCDTextBlock`/
      `DRDeviceMediaInfoKey`) described in that function's doc comment and in
      Task 2's brief is NOT implemented. If a real `drutil cdtext` dump
      parses cleanly (non-nil `XmcdEntry` with sane fields), the FFI path
      stands. If it returns nil/garbage on real hardware, paste the raw
      `drutil cdtext -drive N` output here so the core `parse_drutil_cdtext`
      fixture can be corrected, and open a follow-up task for the
      DiscRecording-framework path:
      ```
      (paste real `drutil cdtext` output here during the hardware pass)
      ```

**Unsure / eyeball (blind, no Xcode here):**
- `maybeReadDiscCdtext` guards re-render staleness with `loadedDiscId == id`
  (set at the end of `loadDiscTracks`) rather than GTK's "is this drive
  still the one the view holder points at" check — functionally equivalent
  (both stop a late CD-TEXT arrival for a disc the user has since navigated
  away from from clobbering `discTracks`), but it's a different mechanism
  than GTK's, so eyeball a same-drive rapid disc-swap (eject mid-read,
  insert a different disc before the FFI call returns) for a stale overlay.
- `sparkamp_disc_read_cdtext` is called with `ctx: nil`, mirroring
  `sparkamp_disc_track_entries`/`sparkamp_disc_id` (disc detection is
  ctx-free, subprocess-backed) — confirm the header's `SparkampCtx *ctx`
  parameter genuinely tolerates NULL here the same as those siblings.
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
- [ ] **gnudb-unknown CD-TEXT disc → `CD-TEXT` pill**: a disc gnudb doesn't
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
- [ ] **FOLLOW-UP (fix while in Xcode) — titles-only CD-TEXT disc pill**: the
      pill in `DiscDriveView.swift` is currently nested INSIDE the
      `if let t = discTags, !t.artist.isEmpty || !t.album.isEmpty` header
      conditional, so a rare CD-TEXT disc that carries track titles but empty
      artist AND album shows NO pill on mac while GTK/TUI DO show `CD-TEXT`.
      Lift the pill computation out to use `discMetaSourceBadge(id)`
      independently of the artist/album line (so it renders whenever a source
      exists, even titles-only), then eyeball the header layout. Matches
      GTK/TUI behavior.

---

## Phase 10 — Task 2: F11 play-count threshold FFI (2026-07-28, BLIND — Swift never compiled)

Adds the C FFI surface for the configurable play-count threshold: a deadline
helper (`sparkamp_play_deadline_secs`) plus enabled/mode/seconds/percent
get/set pairs over `[playback.play_stats]`, mirrored byte-for-byte into
`sparkamp_bridge.h`. This task is Core + header only — no Swift UI wiring
yet; the Settings controls and the transport call site that actually calls
`sparkamp_play_deadline_secs` per-track land in Task 4. Nothing here is
user-visible on mac until then.

- [ ] **Header signatures match Rust exactly**: once Task 4 wires Swift
      bindings, confirm Xcode compiles clean against the 9 declarations added
      to `sparkamp_bridge.h` (`sparkamp_play_deadline_secs`,
      `sparkamp_get/set_play_stats_enabled`,
      `sparkamp_get/set_play_stats_mode`,
      `sparkamp_get/set_play_stats_seconds`,
      `sparkamp_get/set_play_stats_percent`) — param types, const-ness, and
      `uint32_t` mode encoding (0 = seconds, 1 = percent) must line up
      byte-for-byte with `src/ffi/settings.rs`.
- [ ] **No persistence surprise**: like the neighboring watch-folders/media-
      library setters, none of the four `sparkamp_set_play_stats_*` calls
      persist on their own — Task 4's Swift call sites must call
      `sparkamp_save_config` explicitly after a change, same as every other
      Settings toggle on mac.

---

## Phase 10 — Task 4: F11 mac deadline wiring + settings UI (2026-07-28, BLIND — Swift never compiled)

Wires the Task 2 FFI into the mac frontend: `SparkampModel.tick()`'s play-
count gate now computes its deadline per-track from
`sparkamp_play_deadline_secs(ctx, dur)` instead of the old hardcoded
20 s constant (`playCountThresholdSecs` deleted), and `SettingsWindow.swift`
gets a new "Play Count" section on the Playback pane (Count plays toggle,
Seconds/Percent mode picker, seconds stepper 1–3600, percent stepper
1–100), mirroring the existing ReplayGain section's Toggle/Picker/Stepper +
`onChange` + `sparkamp_save_config` idiom.

- [ ] **Toggle off → counts freeze**: in Settings ▸ Playback, turn off
      "Count plays", play any track past its old threshold — the Media
      Library's play count and last-played date for that track do NOT
      change. Turn it back on and the next full playthrough counts normally.
- [ ] **Seconds mode, seconds = 5 → counts at 5 s**: set mode to "N seconds",
      seconds to 5, play a track — the Media Library row's play count
      increments once position crosses ~5 s (not at the old 20 s default).
- [ ] **Percent mode, percent = 50 on a 4-minute track → counts past 2:00**:
      set mode to "N% of track", percent to 50, play a track that is close
      to 4 minutes — the play count increments once position crosses
      roughly the track's halfway point (~2:00), not before.
- [ ] **Short track (shorter than the configured threshold) → counts near
      its end**: with seconds mode and a threshold longer than a short
      track's duration (or percent mode near 100%), play that short track to
      completion — it still counts exactly once, at/near EOS, and does not
      silently fail to count just because the track never reaches the
      configured deadline mid-playback.
- [ ] **Settings persist across relaunch**: change enabled/mode/seconds/
      percent, quit (Cmd+Q, not Xcode Stop), relaunch — Settings ▸ Playback
      shows the same values, and the gate behaves accordingly (confirms
      `sparkamp_save_config` actually fired on each `onChange`, not just
      `sparkamp_set_play_stats_*` in memory).
- [ ] **Fullscreen-visualizer does NOT skew the deadline**: the gate feeds
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

## Phase 10 — Task 5: F12.1 remember search per view (2026-07-28, BLIND — Swift never compiled)

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

- [ ] **Toggle off (default) → unchanged behavior**: with "Remember search
      per view" off in Settings ▸ Media Library, open the Media Library,
      type a search in Files, close the window, reopen it — the Files search
      box is empty, same as before this feature existed. Same for switching
      playlists/devices/discs: each switch still clears that view's search
      box.
- [ ] **Toggle on → Files search survives a window close/reopen**: turn on
      "Remember search per view", open the Media Library, type "beatles" in
      the Files search box, wait ~1 s (debounce), close the Media Library
      window, reopen it — the Files search box shows "beatles" again and the
      track list is filtered accordingly.
- [ ] **Toggle on → Playlists editor search survives switching playlists and
      reopening**: open a saved playlist's editor, type a query in its
      search box, switch to a different saved playlist, switch back — the
      query reappears (not cleared) and re-filters that playlist's rows.
      Close and reopen the Media Library window; opening any playlist shows
      the same remembered query.
- [ ] **Toggle on → Devices search survives switching devices**: with two
      devices connected, type a query while viewing device A, switch to
      device B, switch back to A — the query is restored (not cleared) for
      both, since `last_search["devices"]` is per-view, not per-device.
- [ ] **Toggle on → Discs search survives switching drives and does NOT get
      clobbered by the 10 s poll**: with a disc inserted, type a query in
      the Discs search box, wait through at least one 10 s drive-poll cycle
      on the SAME drive — the query must NOT be cleared (mirrors the GTK
      `last_drive` guard: same-drive repopulation is not a "switch"). Switch
      to a different drive and back — the query is restored, not cleared.
- [ ] **Toggle off after having it on → next open clears again**: with
      "Remember search per view" on and a saved query, turn the toggle back
      off in Settings, then reopen the Media Library (or switch playlists/
      devices/drives) — search boxes clear as they did before the feature
      existed, even though `last_search` may still hold stale entries from
      when the toggle was on (the map is only ever consulted while the flag
      is on).
- [ ] **Settings persist across relaunch**: turn on "Remember search per
      view", quit (Cmd+Q, not Xcode Stop), relaunch — Settings ▸ Media
      Library still shows it on, and a previously-typed Files query (typed
      before quitting, given the ~1 s debounce time to fire) is restored on
      the next Media Library open.
- [ ] **Header signatures match Rust exactly**: confirm Xcode compiles clean
      against the 4 declarations added to `sparkamp_bridge.h`
      (`sparkamp_get/set_remember_search`, `sparkamp_get_last_search`,
      `sparkamp_set_last_search`) — `sparkamp_get_last_search` returns a
      heap `char *` (free with `sparkamp_free_string`) and never crashes on
      an unknown `view_id`, returning `""` instead.
