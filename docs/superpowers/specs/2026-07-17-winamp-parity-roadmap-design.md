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
