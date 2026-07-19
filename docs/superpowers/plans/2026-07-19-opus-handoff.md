# Sparkamp Winamp-parity — controller handoff (phases 2-12)

Written 2026-07-19 by the phase-0/1 controller session for the sessions that
execute the remaining phases. Read this FIRST, then the phase doc you are
executing. Phase docs are DESIGN-LEVEL: architecture, task boundaries,
contracts, test specs. Expand each phase into step-level TDD tasks at
execution time with superpowers:writing-plans, re-verifying every file/line
anchor fresh — earlier phases will have moved things.

## State at handoff

- Branch `album-art-improvements`, all work lands here (user decision — no
  per-phase branches). Pushed to origin through `2eb4391`; NEVER push
  without a fresh explicit user instruction (standing rule, incl. main).
- Phases 0 (fixes) and 1 (metadata foundations) COMPLETE and final-reviewed.
  Suite at handoff: 1055 passed (428 lib + 627 bin), zero warnings.
- Progress ledger: `.superpowers/sdd/progress.md` (gitignored — survives in
  the working copy only; per-task reports there get overwritten across
  phases, copy anything durable into docs/ before relying on it).
- Roadmap spec: `docs/superpowers/specs/2026-07-17-winamp-parity-roadmap-design.md`
  (phase table, user decisions, known limitations). Source todo was
  `/tmp/sparkamp-todo.md` — may be gone; the spec + phase docs are the
  durable copies.
- Pending human verification debt: phase-1 interactive GTK pass; mac Xcode
  pass for phases 0-1 (`docs/mac-pass-checklist.md`).

## Process (what worked for phases 0-1)

Subagent-driven development (superpowers:subagent-driven-development):
brief file per task (`scripts/task-brief`), fresh implementer subagent,
review package (`scripts/review-package BASE HEAD` — record BASE before
dispatch), task reviewer per task, ledger line per task, final whole-branch
review per phase on the most capable available model, ONE fix subagent for
the final findings, re-review, then docs/ledger close-out. Cheap model for
pure-transcription tasks, mid model for everything else. The final reviews
caught 3 shipping bugs the task-level gates missed (phase 0: lyric
truncation; phase 1: two Critical scan-seam defects) — do not skip them.

Per phase: start with a brainstorm-lite pass over the phase doc's "Open
questions" section WITH THE USER (AskUserQuestion), then writing-plans to
expand, then execute. End with: full gate, mac checklist appends, spec
known-limitations updates, ledger, user interactive pass list.

## Environment + gate (bit us repeatedly — verbatim rules)

- Build/test ONLY inside distrobox:
  `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`.
  Host builds fail (no gstreamer/gtk dev libs). NEVER gate on
  `cargo build --lib` — GTK frontend code only compiles in the bin target.
- Zero warnings, zero failures before any "done" claim. Track the floor
  (passed count) in every dispatch; quote BOTH `test result:` lines (lib +
  bin). Gotcha: `src/` modules need `mod x;` in BOTH `src/lib.rs` AND
  `src/main.rs` or their tests/code silently miss the bin target (phase-1
  tripped on this).
- Interactive GTK verification is the user's; implementer gate is build +
  suite. The user's dev-box GTK is 4.22 — renders a few px roomier than the
  Flatpak; accepted, don't chase pixel deltas.

## Conventions

- Core-first: logic in `src/`, frontends adapt. GTK + mac full capability
  parity on EVERY item; TUI wherever its surface reaches (user mandate).
  GTK formatting is the parity reference for mac output.
- macOS Swift is BLIND here (no compiler): read whole files before editing,
  mechanically simple changes, every new/changed C-visible FFI symbol or
  struct hand-mirrored in `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`,
  verification items appended to `docs/mac-pass-checklist.md` in the same
  commit. FFI structs: keep Rust `#[repr(C)]` and the header byte-for-byte
  aligned (phase-1 pattern in `src/ffi/media_library.rs::SparkampLibTrack`).
- Keyboard shortcuts sync across THREE places: mac key handler
  (`SparkampModel+Keys.swift` / `SparkampModel.swift`), mac help
  (`KeyboardShortcutsView.swift`), GTK shortcuts dialog (`player.rs`
  `sections` array). Any phase adding a key updates all three. GTK binds
  upper+lowercase variants; mac lowercase only.
- Key ledger at handoff: claimed — z x c v b (transport), j (jump), i
  (help), f (fullscreen viz), g (overlay), d (ID3), u (EQ), p (playlist),
  r (repeat), s (shuffle), e (granite effect), a (viz mode), n (add file),
  q (QUIT on GTK main window — see phase-5 doc for the queue-key
  resolution), - = (volume). Reserved by these docs: w + k (phase 2),
  m (phase 6, ML toggle), t (phase 6, stop-after-current), Shift+N,
  Ctrl+S, Ctrl+., Ctrl+I (phase 6). Free after all phases: h, o, y.
- RefCell borrows short-lived — never across a UI call, `.await`, or
  `select_row`. Multiple past crashes from this.
- Config: new fields use `#[serde(default)]` + `Default` impl. Settings
  tabs persist via save-on-close (`connect_close_request` → config.save());
  per-toggle saves only where the neighboring rows already do it — copy the
  adjacent idiom, it varies by tab.
- Comments: plain English, why not what. Casing "Sparkamp". Commits:
  conventional prefix, body = why + a verification line, trailer
  `Co-Authored-By:` the executing model.
- Skins: user dir `~/.config/sparkamp/skins/` (deliberately config, shared
  with mac — do not "fix" to XDG data). GTK CSS generated in
  `src/skin.rs::render_gtk_css` — has a `render_gtk_css_covers_*` test
  pattern; every new widget surface gets skin selectors + a test.
  Hand-built GtkListBoxes need `.ml-col-view` for selection colours.

## Architecture map (sizes at handoff — split-as-touched applies)

- `frontends/gtk/window/media_library.rs` ~10.4k lines, `player.rs` ~4.6k:
  when a phase touches them, carve the feature into a new
  `frontends/gtk/window/<feature>.rs` module and move the directly-related
  chunk. New-file soft cap ~800 lines. Planned carve-outs: phase 2 →
  `now_playing.rs` + `art_window.rs`; phase 5 → `queue_manager.rs` (GTK)
  + `src/queue.rs` (core); phase 4 → `src/replaygain.rs`; phase 8 →
  `src/watch.rs`; phase 12 → `lyrics.rs`.
- Core seams that matter across phases:
  - Scan: PRODUCTION flow is `rescan_folder_fast` (path-only rows) then
    `scan_all_folders → scan_folder → needs_metadata_scan` (mtime
    smart-skip + `sample_rate IS NULL` backfill). `rescan_folder_metadata`
    is TEST-ONLY. `upsert_track` is the single metadata write seam.
    `added_at` stamps at fast-insert, heals via COALESCE, never overwrites.
  - Sorting: GTK Files view sorts IN-MEMORY via `ml_columns.rs::ml_sort_key`;
    `queries.rs`'s SQL ORDER BY map serves TUI + mac FFI. New sortable
    columns need BOTH.
  - Artwork: `tags.rs::read_track_tags` = embedded APIC → cache
    (`~/.cache/sparkamp/<hash>.<ext>`) → folder-image fallback
    (cover>folder>front, case-insensitive). `refresh_artwork` deletes ONLY
    under the cache dir (guarded + tested — artwork_path can be the user's
    own folder image; never weaken this).
  - Tags: `TagFields` (id3_editor.rs) carries all 18 fields; WXXX =
    ExtendedLink content, USLT = Lyrics content (not set_text). FFI tag
    ctx passes unknown T-frames through `pending_extra` → `write_extra_frame`.
  - Technical: `src/technical_probe.rs` (header-only symphonia probe,
    avg-bitrate math, VBR/CBR sniff). `tech_summary` formats the ID3-window
    line, shared GTK/TUI, mirrored on mac.
  - Timestamps: `timeutil.rs::format_system_time` family, ISO-8601 TEXT
    columns, displayed via `format_last_played`. mac mirrors with
    `*Display` computed properties (24h — accepted divergence from GTK's
    12h AM/PM).

## Performance notes (accumulated)

- User library ≈ 36k tracks. Anything per-track × 36k must be background,
  batched (existing 100-row insert batching), cancelable, progress-visible.
- SQLite is not Send — DB work stays on its thread; frontends get results
  via the existing channel/callback patterns.
- Known-limitation register lives in the spec — add to it whenever a review
  accepts a residual (current: lyric single-line flattening; unprobeable
  files re-probed each Rescan).

## Manual test protocol (every phase)

1. Implementer gate: full distrobox build + suite, zero warnings.
2. Phase doc's "Manual test plan" → becomes the user's interactive GTK
   pass list, delivered in the phase close-out message.
3. mac items → `docs/mac-pass-checklist.md` (dated phase section, checkbox
   format), written blind, verified later on hardware by the user.
4. TUI: quick keyboard walk of any touched screen.
5. Anything unverifiable by suite + user pass → explicit known-limitation
   or checklist entry. Never silently assume.

## Phase index (execution order — dependency-driven, user-approved)

| Doc | Phase | Headline |
|-----|-------|----------|
| `2026-07-19-phase2-album-art.md` | 2 | F14: A1 expand panel, A6 art window, A2 thumbnails, A5+D14 |
| `2026-07-19-phase3-mpris-nowplaying.md` | 3 | F6: MPRIS2 + mac Now Playing |
| `2026-07-19-phase4-replaygain.md` | 4 | F7: rgvolume/rganalysis + settings + library UI |
| `2026-07-19-phase5-play-queue.md` | 5 | F8: queue core, badges, q keys, Queue Manager (required) |
| `2026-07-19-phase6-shortcuts.md` | 6 | F9: new bindings + dialog source-of-truth sweep |
| `2026-07-19-phase7-playlist-ops.md` | 7 | F1: sort/randomize/reverse + duration status |
| `2026-07-19-phase8-watch-folders.md` | 8 | F10: filesystem watching + scan behaviors |
| `2026-07-19-phase9-cdtext.md` | 9 | F5: CD-TEXT on gnudb miss |
| `2026-07-19-phase10-settings-cluster.md` | 10 | F11 play threshold + F12 niceties |
| `2026-07-19-phase11-album-gallery.md` | 11 | A4: album grid view |
| `2026-07-19-phase12-lyrics.md` | 12 | F15: view/search lyrics |

Each doc ends with "Open questions" — resolve those with the user BEFORE
expanding the plan. Everything else is decided; don't re-litigate settled
decisions recorded in the spec.
