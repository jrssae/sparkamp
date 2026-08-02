# Winamp-parity roadmap — design

Date: 2026-07-17. Status: approved design, drives per-phase implementation plans.
Sources: `/tmp/sparkamp-todo.md` (user-triaged todo), `/tmp/album-art-handoff.md`
(handoff), `/tmp/sparkamp-winamp-gap-report.md` (analysis). This document is the
durable copy of the decisions; the /tmp files may not survive.

## Goal

Implement every feature (F1–F14) and fix (B1–B7, approved D-deltas) from the
triaged todo in small, independently testable phases. Every item covers GTK and
macOS at full capability parity, with TUI support wherever the TUI surface
reaches (user instruction, 2026-07-17). Rejected/deferred items (canned smart
views, balance slider, gapless, ratings UI, streaming, D1) stay out of scope.

## Structure decisions (user-approved)

- **Master roadmap + per-phase plans.** This doc orders the phases. Each phase
  gets its own small spec/plan file in `docs/superpowers/specs/`, written
  just-in-time when the phase starts, so plans never go stale and each is
  self-contained enough for a fresh session (or a smaller model) to execute.
- **Fixes land first** (user choice) — small verifiable wins before features.
- **Ordering within features is dependency-driven** (user delegated; rationale
  per phase below).
- **All work lands on the existing `album-art-improvements` branch** (user
  choice). Never push without a fresh explicit user instruction.
- **Split-as-touched file policy:** new features go in new modules; when a
  phase touches an oversized file (`media_library.rs` ~10.4k lines,
  `player.rs` ~4.5k), carve the directly-related chunk into its own module as
  part of that phase. No big-bang refactor phase. Soft cap ~800 lines for new
  files. Rationale: keeps every working set small enough to hold in context.
- **Comment compliance:** CLAUDE.md style — plain English, explain why not
  what — on all new and touched code.
- **B6 resolution:** CLAUDE.md is corrected to the real skins path
  (`~/.config/sparkamp/skins/`, shared with macOS); code does not move.
- **F10 resolution:** true filesystem watching (gio FileMonitor / notify
  crate) instead of Winamp-style interval polling; the startup-rescan toggle
  is still added. Interval rescan is not built.

> **Design docs for phases 2-12** were pre-written 2026-07-19 (Fable→Opus
> handoff): see `docs/superpowers/plans/2026-07-19-opus-handoff.md` (read
> first) and `2026-07-19-phase{2..12}-*.md`. They supersede this table's
> one-line summaries; the just-in-time step is now writing-plans expansion
> per phase, not doc authoring.

## Phase order

Each phase ends with: full `cargo build && cargo test` green with zero
warnings inside distrobox, mac verification items appended to
`docs/mac-pass-checklist.md`, user interactive GTK check, conventional commit.

| # | Phase | Contents | Ordering rationale |
|---|-------|----------|--------------------|
| 0 | Fixes pass | B1+B2+B7 (ID3 extra-frame wiring, GTK save + mac FFI, wire-or-delete dead machinery), B3 (bind `u`, fix dialog claims), B4 (SparkAmp→Sparkamp titles), B5 (correct APIC mime for GIF/WebP), B6 (CLAUDE.md skins path), D8 (mac playlist autoscroll), D10 (strip mac EQ labels), D13 (GTK genre dropdown = predefined-only), D16 (GTK verify-discs toggle), D17 (GTK granite beat settings) | User choice: fixes first. All small and independent. D14 (mac art set/clear) deferred to phase 2 where it pairs with A5 |
| 1 | Metadata foundations | F13 scanner/schema capture (sample rate, file size, `added_at`, stored mtime, VBR/CBR) + ML columns GTK/mac; F3 read-only tech line in ID3 window both frontends; F2 folder-image fallback (folder/cover/front .jpg/.png, case-insensitive) in `read_track_tags`/`refresh_artwork`; B8 settings-widget skinning — generic skinned scale trough/highlight/slider + settings list/dropdown selectors in `render_gtk_css` (today only `scale.seek-scale`/`scale.vol-scale` are styled; keep those overrides intact) | Unblocks phase 2 (A1 needs kHz; art panel inherits folder fallback). Scanner schema settles before later scanner work (F7 analysis, F10 watching). Rating column stays deferred with the ratings UI. B8 found in the phase-0 user pass (2026-07-17) |
| 2 | F14 album art | A1 expandable now-playing panel (core play-start snapshot hook before `record_play`; GTK marquee↔panel swap + viz stretch; mac panel; TUI data-as-text), A6 standalone art window (singleton like the other windows — toggling/opening focuses the existing one, never a second instance; cover follows every track change; shared `handle_key` routing, `k`), A2 inline ML thumbnails (+ mac art column), A5 set-art refinements + D14 mac set/clear parity. `w`/`k` added to the shortcuts dialog | The primary feature; its dependencies land in phase 1. Builds the core "now playing changed" notification seam that phase 3 consumes |
| 3 | F6 MPRIS + NowPlaying | Linux MPRIS2 D-Bus service (metadata incl. art URL, status, position, transport commands); mac MPNowPlayingInfoCenter + MPRemoteCommandCenter | Consumes the phase-2 seam; OS-widget art comes free right after art lands |
| 4 | F7 ReplayGain | Pipeline `rgvolume` (+`rglimiter`) before EQ/volume; `rganalysis` scan path → DB always, tag write-back toggle (default OFF); 4 playback settings (master ON, source track/album/auto, clip protection ON, non-RG adjustment −6 dB default); 2 library settings (auto-analyze on add, write-back); context + bulk analyze actions; opt-in ML column | Todo calls it hugely important — earliest slot after its scanner (phase 1) and engine-adjacent (phase 2/3 seams stable) prerequisites. Isolated pipeline work, low conflict with later UI phases |
| 5 | F8 play queue | Core ordered queue consulted before shuffle/linear advance, survives playlist mutation, resumes from last-queued position; playlist badges, right-click + `q` toggle, jump-window `q`; Queue Manager view optional, only if time allows | Advance-logic core; precedes phase 6 whose stop-after-current flag hooks the same advance seam |
| 6 | F9 shortcuts + dialog sweep | Bind `m` (ML), GTK `↑/↓` volume, GTK playlist `Enter`, `n`/`Shift+N` add file/folder (GTK+mac+TUI), stop-after-current (non-colliding key + engine flag at advance), `Ctrl+S` save playlist, GTK `Ctrl+.` settings, invert selection; shortcuts dialog becomes single source of truth for every binding | After phases 2/5 so the dialog sweep documents `w`/`k`/`q` too; stop-after-current lands right after phase 5's advance work while it is fresh |
| 7 | F1 playlist ops | Sort title/filename/path, randomize, reverse via playlist button-bar menu; `ShuffleState::reset` after reorder; status row = count + total + selected duration on both frontends | Independent quick win; queue (phase 5) already handles reorder invalidation by then |
| 8 | F10 watch folders | Filesystem watching (decided above), rescan-on-startup toggle, auto-add played tracks, remove-missing toggle (default OFF), per-folder recurse toggle, compact-on-rescan | Scanner is mature (phases 1 and 4 done); watching integrates with the settled scan path |
| 9 | F5 CD-TEXT | Read CD-TEXT (libburn `cdtext_to_v07t` path) when gnudb misses or as overlay; probe-time only, drive-contention aware | Independent disc-subsystem work; no coupling to the phases above |
| 10 | F11 + F12 | Play-stats toggle + N-seconds / N-percent threshold feeding `record_play` (closes the 20 s open thread); remember-search-per-view, artist→album-artist fallback, skip-DB-load-at-startup | Small settings cluster; F11 touches the play path phase 2 instrumented, safer after it settles |
| 11 | A4 album gallery | ML browse-by-album cover grid; clicking an album shows its tracks; needs album-grouping infra | Explicitly "larger, note only" in the todo — re-confirm scope with the user before building |
| 12 | F15 View/Search Lyrics | Right-click "View/Search Lyrics" on track rows in ML Files, saved playlists, disc view, device view, active playlist + affordance in the A1 panel. Has USLT → read-only lyrics window using the skin CSS font/size; none → default browser on DuckDuckGo "<artist> - <song> lyrics" (standard artist/title fallback logic). GTK + mac, TUI lyrics-as-text | User addition (2026-07-17), scheduled last; the viewer window also mitigates the phase-0 single-line-Entry lyric limitation |

## Cross-cutting rules (every phase plan inherits these)

- **Environment:** build/test only inside distrobox
  (`distrobox enter dev-box -- sh -c 'cargo build && cargo test'`). Gate on
  the full build, never `--lib` — GTK code only compiles in the bin target.
- **Verification:** zero warnings, zero failures before any "done" claim.
  TDD for core logic; GTK harness tests in `frontends/gtk/window/tests.rs`
  where feasible. Interactive GTK verification is the user's; the
  implementer's gate is build + full suite. Fail-fast: two consecutive
  failures → stop and ask.
- **macOS:** Swift is written blind from this box — flag it and append every
  item to `docs/mac-pass-checklist.md`. Every new `sparkamp_*` FFI symbol is
  hand-added to `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`.
- **Keyboard shortcuts** stay in sync across three places: the mac key
  handler, the mac help view, and the GTK shortcuts dialog. Any phase adding
  a key updates all three. Free keys as of 2026-06: h, m, o, t, y — phase 2
  claims w and k, phase 6 claims m (ML toggle) and picks stop-after-current
  from the remainder.
- **Core-first:** logic in `src/`; frontends adapt it. New config fields use
  `#[serde(default)]` + a `Default` impl. RefCell borrows stay short-lived —
  never across a UI call, `.await`, or `select_row`.
- **Commit style:** conventional prefix, body explains why + a verification
  line, `Co-Authored-By` trailer.

## Testing strategy

Core features (queue, ReplayGain source selection, snapshot hook, folder-art
probing, threshold logic, CD-TEXT parsing) get unit tests in `src/` next to
the code. Frontend wiring gets GTK harness tests where the harness reaches;
what it can't reach goes on the user's interactive checklist for that phase
plus the mac checklist. Suite currently at 1015 tests — each phase should
leave the count higher, never lower.

## Known limitations (recorded during phases 0-1)

Files whose sample rate the codec probe can never resolve (truly corrupt or
exotic headers) keep a NULL sample_rate and are re-probed on every manual
Rescan — bounded to that broken set, skip logic unaffected for everything
else. Accepted 2026-07-19.


The GTK ID3 editor renders the lyric (USLT) field in a single-line Entry:
long lyrics are no longer truncated on save (the 256-char sanitizer cap is
bypassed for lyric), but multi-line structure is flattened to one line on an
open→save round-trip — inherent to the widget, strictly better than the
pre-phase-0 silent truncation. Full fidelity needs a multi-line TextView for
the lyric field; fold into phase 2 (F14 touches tag display) or later.

## Known limitations (recorded during phase 2 — F14 album art)

- The GTK A1 now-playing panel thumbnail and the Media-Library inline
  thumbnails render a STILL frame (loaded pre-scaled via
  `Pixbuf::from_file_at_scale` into a fixed texture so an oversized cover can
  never exceed its slot). Animated GIF covers do not animate there; the A6
  standalone art window (`k`) still shows the full/animated image. Accepted
  2026-07-20.
- A library row whose `artwork_path` DB column is empty but whose file has an
  embedded APIC (indexed before artwork extraction, or an art-less scan) shows
  an empty artwork field in the ID3 editor, and saving an unrelated tag edit
  then STRIPS the embedded art (`write_tag_fields` treats empty artwork_path as
  "remove pictures"). Pre-existing (not introduced by phase 2); phase 2
  deliberately kept the ID3 editor's art source off the folder/embedded probe
  fallback (that fallback is display-only, in `build_now_playing_info`) to
  avoid the opposite surprise — silently embedding a loose folder image on save.
  A proper fix reads the file's own embedded art into the editor. Accepted
  2026-07-20.
- mac D14 (ID3 art edit) does not include GTK's "Also write folder image"
  checkbox on embed — mac can browse/embed/clear embedded art only. Accepted
  2026-07-20 (mac spec scope).
- mac carousel: a manual page-dot tap does not extend the auto-cycle dwell
  (GTK resets+doubles it); mac just jumps. Minor; accepted 2026-07-20.
- The now-playing panel stats (play count / last played) show the PRE-play
  snapshot and refresh on each play/track-change (incl. same-track replay);
  they do not tick live mid-song. By design (matches the classic behavior).

## Media Library status bars (2026-07-27 consistency pass)

- All four Media Library views (Files, Playlists-tracks, Devices, Discs data-files)
  now carry the same bottom status bar as the active playlist — `N tracks · MM:SS
  total · MM:SS selected` — via the shared core formatter (`playlist_status_line`
  on GTK; `PlaylistStatus.swift::playlistStatusLine` on mac). Plus a 1px spacing
  bump on the active-playlist header count + status bar. GTK: `ml_status_bar_for<T>`
  helper (Files/Devices box `LibTrack`, Discs box `DiscFile`, Playlists box
  `EditorEntry`). mac blind-verified via checklist.
- OPEN placement calls (eyeball on the interactive/Xcode pass): the GTK Disc bar
  sits directly under the file list (not the absolute bottom of the shared
  disc/burn container); the mac playlist-editor bar sits below the button row
  (not directly under the table). Both are one-line moves if a different spot is
  preferred.

## Known limitations (recorded during phase 11 — A4 album gallery)

- The mac gallery loads each cover at full resolution, uncached, per cell
  (`NSImage(contentsOfFile:)` off-main), rather than through the shared
  downsampled thumbnail cache the GTK gallery uses (`thumb_path_for(path,
  px)`). Accepted divergence: LazyVGrid only realizes visible cells, so it is
  functionally correct; possible scroll jank on very large libraries is a
  mac-checklist eyeball item / future optimization (generate + cache mac
  thumbs like GTK).
- Persistence of the gallery zoom size and sort mode diverges by frontend:
  GTK stores them in the Rust config (`window.gallery_thumb_px` /
  `gallery_sort`), mac stores them in SwiftUI `@AppStorage`
  (`sparkamp.gallery.thumbPx` / `.sort`). Same accepted pattern as other mac
  window prefs (e.g. sidebar width).
- A GTK artwork edit made while the album gallery page is currently on screen
  invalidates the thumbnail cache (all sizes) but does not repaint the
  on-screen tile until the next gallery rebuild (sidebar re-select or a
  scan-complete). Benign in practice — ID3/artwork editing happens outside
  the gallery view — so it is left as-is rather than adding a live repaint
  hook.
- Grouping key lowercases the effective album artist without trimming
  incidental whitespace (e.g. `"Band "` vs `"Band"` split into two groups),
  matching the existing library-wide `SortKeys::from_track` convention rather
  than introducing a divergent normalization. Album is trimmed for the
  no-album-bucket test.

## Known limitations (recorded during phase 9 — F5 CD-TEXT)

- Precedence is whole-entry (Winamp): gnudb/user tags win entirely when the
  disc has an entry; CD-TEXT is used only on a TOTAL gnudb miss. There is NO
  per-field/per-track gap-fill and NO "prefer disc CD-TEXT" toggle (user
  decision 2026-07-28) — this intentionally supersedes the design doc's
  `merge_disc_metadata` gap-fill proposal.
- CD-TEXT read is used for DISPLAY and is inherited by RIPPED-FILE tags
  (per-track titles and disc-level artist/album, whole-entry, on a total
  gnudb miss — see the precedence rule above) on all three frontends, but it
  is NEVER submitted/uploaded to gnudb: a CD-TEXT-only disc cannot be pushed
  to the database. It is cached per freedb disc-id, read ONCE on first show
  of an unknown audio disc (a per-id "tried" set); it is never re-read on
  view refresh and the cache is never cleared while running (mirrors GTK). A
  re-inserted disc that collides on freedb ID reuses the cached names (same
  collision surface gnudb itself has).
- macOS BURN-side CD-TEXT is NOT implemented: `drutil -audio` (the mac burn
  CLI) has no CD-TEXT/v07t input. Writing CD-TEXT on mac needs a rewrite to
  Apple's DiscRecording framework (`DRTrack`/`DRCDTextBlock`/`DRBurn`) — a
  documented deferred item (see the appendix in
  `docs/superpowers/plans/2026-07-28-phase9-cdtext-read.md`). Linux burn
  already writes CD-TEXT via `cdrskin input_sheet_v07t=`.
- The macOS READ parser (`parse_drutil_cdtext`) was written blind: `drutil
  cdtext`'s exact stdout format is undocumented and unverifiable off-mac. Its
  test fixture is provisional — a macOS worker must capture a real dump and
  correct the parser/fixture (checklist item). If the format proves
  unparseable, the mac reader falls back to the DiscRecording framework
  (`DRCDTextBlock`) acquisition, documented in the plan/checklist.
- CD-TEXT reads spin the drive; they are wrapped in the exclusive-read guard
  and skipped when the drive is held (burn/rip), so metadata stays
  gnudb/"Track N" under contention rather than fighting for the drive.
- Per-frontend reach: GTK (pre-existing) + TUI + macOS read CD-TEXT. The TUI
  read is audio-CD-only (data discs with a readable TOC are skipped). CD-TEXT
  text is not NUL-sanitized on mac (SwiftUI `Text` is NUL-safe, unlike GTK's
  `gtk_safe()` path).

### Phase 9 follow-on — source badge + CD-TEXT editor seeding (2026-07-28)

- A disc-level source badge (text pill) shows where the displayed track
  metadata came from: `gnudb` / `edited` / `CD-TEXT` (none → no pill). Because
  precedence is whole-entry, the whole displayed set has ONE source, so the
  badge is single (disc-level), not per-track. Shared classifier
  `crate::disc::source::DiscMetaSource::resolve(has_official, has_user_tags,
  has_cdtext)`; GTK `.disc-source-pill`, TUI bracketed tag on the Discs
  column-header row (no Artist/Album header line exists there), mac capsule
  pill — identical label strings across all three.
- The disc tag editor now seeds from CD-TEXT (whole-entry `tags.or(cdtext)`)
  on all three frontends, so a CD-TEXT-only disc prefills artist/album/titles.
  **Submit-to-gnudb process:** open the disc tag editor (titles + now
  artist/album prefilled from CD-TEXT) → optionally adjust → Save (promotes
  the tags into the user tag set) → the Submit action becomes available →
  pick a gnudb category + enter your email (first time) → uploads. CD-TEXT is
  never auto-submitted; that edit+save is the deliberate promotion step.
- Playlist-ADD now folds CD-TEXT into the added tracks' disc-level
  artist/album on ALL THREE frontends (whole-entry `tags.or(cdtext)`): TUI
  (`add_disc_entries`), GTK (`add_disc_entries` closure), mac (`addDiscTracks`
  via `discOverlayTags`). So CD-TEXT names now reach the disc view, ripped
  files, AND the active playlist consistently everywhere. (Per-track titles
  already inherited on all three.)
- Remaining follow-up (non-blocking, deferred): mac hides the source pill for
  a rare *titles-only* CD-TEXT disc (empty artist AND album) because the pill
  is nested under the artist/album header conditional; GTK/TUI show it — a
  macOS worker should lift the pill out of that conditional and eyeball the
  layout (checklist item).

## Known limitations (recorded during phase 8 — F10 watch folders)

- remove_missing_on_rescan defaults OFF, which CHANGES prior behavior: rescans
  (and the live watcher) no longer prune library rows for files that have
  vanished unless the user enables the toggle. This is Winamp offline-media
  parity (entries persist for unplugged/synced drives) — user decision
  2026-07-27. There is no "mark-broken" row state in the codebase; a missing
  file's row is either kept (OFF) or hard-deleted (ON).
- auto_add_played adds ONLY tracks played from OUTSIDE every watched folder
  (folder_id NULL bucket, visible in the Files view which has no folder JOIN).
  In-folder played tracks are left to the watcher/rescan. This outside-only
  guard also sidesteps a latent path-form inconsistency: the library stores
  un-canonicalized scanned paths (fast-insert skips a per-file stat), while a
  played `Track.path` is canonicalized — so adding an in-folder track by its
  canonical path could create a duplicate row under symlinked paths (Flatpak
  FUSE, synced libraries). Guarding to outside-only avoids it.
- Watcher latency: ~2 s debounce before a created/modified/removed file
  reflects in the library (the debouncer coalesces bulk copies to avoid a
  per-file rescan storm). Event classification is pure-unit-tested; the live
  OS watcher has one generous-timeout smoke test (timing-sensitive by nature).
- inotify watch-limit exhaustion (`max_user_watches`) on very large recursive
  trees makes `FolderWatcher::start` fail; the app logs and degrades to manual
  rescan — it never crashes. No in-app "watching is off" banner was built
  (only the log line); a surfaced status line is a possible later refinement.
- Per-folder recurse has UI only in GTK + macOS. The TUI has no watched-folder
  management screen, so TUI-managed folders always watch/scan per the DB
  default (recurse=1). The `folders.recurse` column and the core `walk_dir`
  still honor the flag everywhere; only the TUI toggle is absent.
- TUI: a background watcher event that refreshes the open Files list reuses
  `refresh_ml_search`, which resets the selection to the top — a minor
  scroll-jump while browsing during live fs activity. Follow-up: preserve
  selection by path.
- Deprecated config fields `periodic_rescan` / `rescan_interval_mins` are
  retained for TOML back-compat but no longer surfaced in any UI (superseded by
  `watch_folders`). `MediaLibraryConfig::set_rescan_interval_mins` is now
  reachable only from its own tests (`#[allow(dead_code)]`) — candidate for
  deletion with its tests in a later cleanup.
- Legacy path-form mismatch on symlinked-home systems (Silverblue/uBlue/
  ostree, where `/home`→`/var/home`): `add_folder` canonicalizes the folder
  path (`canonicalize_folder_path`, resolving the symlink), and current scans
  store `/var/home/...` track paths, so a FRESH library is fully consistent
  with the watcher. But a library whose rows were scanned by an OLDER build
  (stored `/home/...`) has track paths in a different string form than the
  watcher's `/var/home/...` events — the watcher can neither remove/replace
  those legacy rows (its path-keyed DELETE misses) nor recognize them on
  Upsert (exact-string existence check misses), so it inserts `/var/home`
  DUPLICATES and leaves the `/home` originals stranded. Confirmed live on the
  user's uBlue system 2026-07-28 (Testing folder: 102 legacy `/home` rows +
  watcher-added `/var/home` dupes for the same physical files). USER DECISION
  2026-07-28: document only, no code change (a one-time path-canonicalization
  migration or watcher symlink-awareness was declined to preserve the
  deliberate un-canonicalized-storage design). WORKAROUND: on such systems,
  remove and re-add each watched folder once after upgrading into the watch
  feature — this purges the legacy rows and rescans them in the canonical
  `/var/home` form the watcher uses. Note the Files view shows the folder/
  embedded cover-art thumbnail per row (not a broken-file marker; the `⚠`
  broken indicator exists only in the active playlist), so a stale row is not
  visually flagged there.
- macOS is BLIND (Swift never compiled here): the 5 settings toggles, the
  per-folder Recurse checkbox (in SettingsWindow's MediaLibraryPane), the
  tick() poll-drain, and the auto-add hook are verified by symbol/name
  cross-check + `docs/mac-pass-checklist.md` (phase-8 section), pending the
  user's Xcode/hardware pass.

## Known limitations (recorded during phase 7 — F1 playlist ops)

- Full Winamp menu-bar consolidation (user decision 2026-07-27): the flat GTK/mac
  playlist buttons (+Files/+Folder/Save/Remove/Remove All) were replaced by four
  menu buttons — Add▸ / Select▸ / Sort▸ / List▸. The old GTK `btn_remove` is now
  vestigial (constructed+wired but unappended); mac's `PlaylistControlButtonStyle`
  is now unreferenced — both are dead-code cleanups for a later pass.
- TUI has NO active-playlist multi-select, so: the ops popup (`o`) omits Select
  All/None/Invert (Sort×5 + Randomize + Reverse only), and the TUI status line
  passes `selected = None` (shows `N tracks · MM:SS total`, never a selected clause).
  Accepted 2026-07-27.
- Randomize uses the process `rand::thread_rng` (same as shuffle playback) — no
  seed control / reproducibility. Accepted.
- Sort uses `path.file_name()` (with extension) for the blank-title / Filename
  fallback, vs the codebase's usual `file_stem()` — dormant (titles are rarely
  blank in normal use). Flagged for a later cleanup.
- macOS `⌘S`/`⌘I` shortcuts live on `Menu`-nested `Button`s; whether SwiftUI
  hoists a menu-nested `.keyboardShortcut` into the window command set is a BLIND
  bet — verify on the Xcode/hardware pass (`docs/mac-pass-checklist.md` phase-7).

## Stop with fadeout (added 2026-08-02, outside the F1–F15 roadmap)

Winamp's Shift+V. Not part of the original F-list — added on user request after
the phase-6 pass, built core-first across all three frontends.

- Ramp lives on `engine::Player` (`begin_fadeout` / `poll_fadeout` /
  `cancel_fadeout`) and is advanced by each frontend's existing tick loop; there
  is no timer thread. Wall-clock, not step-counted, so GTK's 33 ms tick and the
  mac's 100 ms tick produce the same fade length.
- Attenuation is a separate `fade_factor` multiplied into the volume element
  alongside `user_volume` and `user_preamp`, so a fade never rewrites the
  volume the user chose and restoring it is a single assignment.
- Any deliberate transport (play / pause / load / stop) cancels a fade in
  progress. The volume is restored *after* the pipeline goes to Null so the end
  of a fade cannot blip back to full for an instant.
- Length is `playback.fadeout_secs`, default **3** (Winamp defaults to 5),
  clamped to 1–10 on both read and write. 0 is deliberately not allowed — that
  is plain `v`, which already has its own key.
- TUI gets the key and the status line but no separate visual state; GTK and mac
  show the fade only through the status line, since the ramp is audible.

## Known limitations (recorded during phase 6 — F9 Shortcuts + dialog sweep)

- Stop-after-current (`t`) is a TRANSIENT engine flag — never persisted (fires
  once at the next EOS, then clears). Cleared by next / prev / jump-to-track /
  replay-current / stop; NOT cleared by pause+resume (the armed state survives a
  pause). Indicator = a small stop-square badge on the bottom-right corner of
  the play/pause/stop state glyph next to the time index (GTK `state_label`
  Overlay, mac `stateIcon` overlay); TUI shows a combined `▶⏹` header glyph
  (no corner-overlay in a terminal). User-decided 2026-07-26.
- TUI has NO separate `Shift+N`: the `n` add-file prompt already accepts folder
  paths via the typed-path parser (`commit_add_file` → `path.is_dir()`), so
  file+folder parity is met through one key. GTK/mac keep the distinct
  `n`/`Shift+N` file-vs-folder pickers. Accepted 2026-07-26.
- GTK `Enter` = play selected row is the TreeView's native `row_activated`
  (also fires on double-click); phase 6 adds no GTK handler for it. mac uses
  `Return`/double-click in the track-list tables.
- macOS `⌘S` (save) and `⌘I` (invert selection) are wired in `PlaylistView`
  (bottom-bar Save button + a hidden invert button) because the playlist
  selection lives in the view, not the model — deferred to hardware
  verification (`docs/mac-pass-checklist.md` phase-6 section). Accepted
  2026-07-26 (blind).

## Known limitations (recorded during phase 5 — F8 Manual Play Queue)

- The queue is SESSION-ONLY: cleared on quit, never persisted (Winamp JTFE
  behavior; user decision 2026-07-22). Playlist-entry ids (`Track.id`) are
  reassigned at load, so the queue cannot survive a restart by design.
- The `[n]` badge is a TEXT PREFIX on the row label (playlist + jump/queue
  views), not a separate sortable column. Accepted 2026-07-22.
- GTK interaction (evolved from the plan during user testing, 2026-07-22):
  `q` opens the Jump/Queue window in Queue mode; `j` opens Jump mode; a
  Jump/Queue radio switches; `Esc` = Quit on the main window (child windows
  keep Esc = close); `Ctrl+Q` = queue/dequeue the selection (playlist rows or
  jump highlight); the standalone Queue Manager window was folded into the jump
  window. GTK playlist badge updates use in-place row patches (a model-swap
  rebuild from the playlist window's own key handler doesn't repaint until a
  later frame).
- macOS enqueue is via the playlist row CONTEXT MENU ("Queue / Dequeue"), not a
  global Ctrl+Q: the app-wide key monitor guards `!hasModifiers`, and the
  playlist selection lives in the view, not the model. `q` opens the Play Queue
  window. Not a regression — GTK/TUI keep Ctrl+Q. Accepted 2026-07-22.
- The GTK Queue Manager (Queue mode) and MPRIS Next live-refresh the queue/
  playlist badges via a thread_local hook; an open Queue view left up during
  playback stays in sync. Accepted 2026-07-22.

## Known limitations (recorded during phase 4 — F7 ReplayGain)

- ReplayGain tag write-back is MP3-only (id3 `TXXX REPLAYGAIN_*` frames). M4A,
  WMA, FLAC, OGG, WAV keep their analysis values in the library DB (column /
  display / playback via `rgvolume` if the file already carries native RG tags)
  but Sparkamp does not write RG tags into them — writing native tags for those
  formats would need a multi-format tag writer Sparkamp doesn't use (lofty was
  considered and declined). Accepted 2026-07-21.

- ReplayGain analysis decodes whole files (rganalysis measures the full audio),
  so a bulk analyze over a large library is minutes of CPU-bound work. It runs
  on a single cancelable background worker with progress; per-track passes plus
  one extra concat pass per multi-track album (album gain) mean a track in an
  N-track album is decoded twice. Accepted 2026-07-22.

- ReplayGain playback changes (enable/disable, source, clip protection) reshape
  the GStreamer chain only at `State::Null`, so a change made mid-track applies
  from the NEXT `load()` (next track / restart), not instantly. The engine
  defers via `rg_pending`; GTK re-applies immediately when Stopped and reloads
  the current track at position when Playing, but TUI/mac only defer. The
  fallback-gain value is the one live exception (a one-liner on `rgvolume`).
  Accepted 2026-07-22.

- Sorting the ReplayGain library column treats un-analyzed tracks as 0.0 dB
  (GTK shifts its sort key to group them; TUI has no column; mac does not
  shift), so on mac un-analyzed rows interleave with reference-level tracks.
  Cosmetic. Accepted 2026-07-22.

## Known limitations (recorded during phase 3 — F6 MPRIS + mac Now Playing)

- Setting LoopStatus / Shuffle / Volume over D-Bus (playerctl / GNOME widget)
  updates the engine + config (and persists) but does NOT re-render the GTK
  repeat/shuffle button or volume slider until the user next touches that
  control. Behavior is correct; only the on-screen widget lags. Accepted
  2026-07-21.
- MPRIS status/loop/shuffle/volume/track PropertiesChanged signals are driven
  by a 500ms poll (no per-change hook into the GTK transport handlers), so a
  widget can lag a change by up to ~500ms. Position is not signalled at all —
  MPRIS consumers poll it (spec-conformant). Accepted 2026-07-21.
- `Seeked` fires only on D-Bus-initiated seeks (Seek / SetPosition); dragging
  the in-app seek bar does not emit it (the widget's shown position may lag one
  poll). `SetPosition` does not verify its TrackId argument against the current
  track, and `Seek` clamps to >= 0 but not to the track length (no skip-to-next
  on overshoot). Accepted 2026-07-21.
- Metadata assembly reads the track's tags off disk on the GLib main loop once
  per track change (duplicating the now-playing snapshot's read). Fine for local
  files; a slow/network mount could micro-stall. Accepted 2026-07-21.
- mac Now Playing elapsed time is set on track/state change + on Control Center
  scrub; macOS extrapolates from the rate between updates, so an in-app seek-bar
  drag may lag the card by one update. Accepted 2026-07-21.

## Error handling defaults

Missing art → placeholder, never an error: a large Sparkamp logo at 50%
opacity in the background with a "No artwork available" message (same
treatment in the A1 panel art area and the A6 window, user decision
2026-07-17). Missing tags → skip the row in
the A1 panel. GStreamer elements missing (rgvolume/rganalysis) → silent no-op
per house rule. Filesystem watcher failures → degrade to manual rescan with a
log line, never crash the ML.
