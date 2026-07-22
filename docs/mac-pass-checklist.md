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
