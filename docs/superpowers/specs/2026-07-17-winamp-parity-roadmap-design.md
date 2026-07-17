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

## Phase order

Each phase ends with: full `cargo build && cargo test` green with zero
warnings inside distrobox, mac verification items appended to
`docs/mac-pass-checklist.md`, user interactive GTK check, conventional commit.

| # | Phase | Contents | Ordering rationale |
|---|-------|----------|--------------------|
| 0 | Fixes pass | B1+B2+B7 (ID3 extra-frame wiring, GTK save + mac FFI, wire-or-delete dead machinery), B3 (bind `u`, fix dialog claims), B4 (SparkAmp→Sparkamp titles), B5 (correct APIC mime for GIF/WebP), B6 (CLAUDE.md skins path), D8 (mac playlist autoscroll), D10 (strip mac EQ labels), D13 (GTK genre dropdown = predefined-only), D16 (GTK verify-discs toggle), D17 (GTK granite beat settings) | User choice: fixes first. All small and independent. D14 (mac art set/clear) deferred to phase 2 where it pairs with A5 |
| 1 | Metadata foundations | F13 scanner/schema capture (sample rate, file size, `added_at`, stored mtime, VBR/CBR) + ML columns GTK/mac; F3 read-only tech line in ID3 window both frontends; F2 folder-image fallback (folder/cover/front .jpg/.png, case-insensitive) in `read_track_tags`/`refresh_artwork` | Unblocks phase 2 (A1 needs kHz; art panel inherits folder fallback). Scanner schema settles before later scanner work (F7 analysis, F10 watching). Rating column stays deferred with the ratings UI |
| 2 | F14 album art | A1 expandable now-playing panel (core play-start snapshot hook before `record_play`; GTK marquee↔panel swap + viz stretch; mac panel; TUI data-as-text), A6 standalone art window (shared `handle_key` routing, `k`), A2 inline ML thumbnails (+ mac art column), A5 set-art refinements + D14 mac set/clear parity. `w`/`k` added to the shortcuts dialog | The primary feature; its dependencies land in phase 1. Builds the core "now playing changed" notification seam that phase 3 consumes |
| 3 | F6 MPRIS + NowPlaying | Linux MPRIS2 D-Bus service (metadata incl. art URL, status, position, transport commands); mac MPNowPlayingInfoCenter + MPRemoteCommandCenter | Consumes the phase-2 seam; OS-widget art comes free right after art lands |
| 4 | F7 ReplayGain | Pipeline `rgvolume` (+`rglimiter`) before EQ/volume; `rganalysis` scan path → DB always, tag write-back toggle (default OFF); 4 playback settings (master ON, source track/album/auto, clip protection ON, non-RG adjustment −6 dB default); 2 library settings (auto-analyze on add, write-back); context + bulk analyze actions; opt-in ML column | Todo calls it hugely important — earliest slot after its scanner (phase 1) and engine-adjacent (phase 2/3 seams stable) prerequisites. Isolated pipeline work, low conflict with later UI phases |
| 5 | F8 play queue | Core ordered queue consulted before shuffle/linear advance, survives playlist mutation, resumes from last-queued position; playlist badges, right-click + `q` toggle, jump-window `q`; Queue Manager view optional, only if time allows | Advance-logic core; precedes phase 6 whose stop-after-current flag hooks the same advance seam |
| 6 | F9 shortcuts + dialog sweep | Bind `m` (ML), GTK `↑/↓` volume, GTK playlist `Enter`, `n`/`Shift+N` add file/folder (GTK+mac+TUI), stop-after-current (non-colliding key + engine flag at advance), `Ctrl+S` save playlist, GTK `Ctrl+.` settings, invert selection; shortcuts dialog becomes single source of truth for every binding | After phases 2/5 so the dialog sweep documents `w`/`k`/`q` too; stop-after-current lands right after phase 5's advance work while it is fresh |
| 7 | F1 playlist ops | Sort title/filename/path, randomize, reverse via playlist button-bar menu; `ShuffleState::reset` after reorder; status row = count + total + selected duration on both frontends | Independent quick win; queue (phase 5) already handles reorder invalidation by then |
| 8 | F10 watch folders | Filesystem watching (decided above), rescan-on-startup toggle, auto-add played tracks, remove-missing toggle (default OFF), per-folder recurse toggle, compact-on-rescan | Scanner is mature (phases 1 and 4 done); watching integrates with the settled scan path |
| 9 | F5 CD-TEXT | Read CD-TEXT (libburn `cdtext_to_v07t` path) when gnudb misses or as overlay; probe-time only, drive-contention aware | Independent disc-subsystem work; no coupling to the phases above |
| 10 | F11 + F12 | Play-stats toggle + N-seconds / N-percent threshold feeding `record_play` (closes the 20 s open thread); remember-search-per-view, artist→album-artist fallback, skip-DB-load-at-startup | Small settings cluster; F11 touches the play path phase 2 instrumented, safer after it settles |
| 11 | A4 album gallery | ML browse-by-album cover grid; clicking an album shows its tracks; needs album-grouping infra | Explicitly "larger, note only" in the todo — last, and re-confirm scope with the user before building |

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

## Error handling defaults

Missing art → placeholder (never an error). Missing tags → skip the row in
the A1 panel. GStreamer elements missing (rgvolume/rganalysis) → silent no-op
per house rule. Filesystem watcher failures → degrade to manual rescan with a
log line, never crash the ML.
