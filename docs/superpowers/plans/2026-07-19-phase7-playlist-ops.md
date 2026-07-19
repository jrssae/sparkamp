# Phase 7 — F1 Playlist Ops + Duration Status (design-level plan)

> Read `2026-07-19-opus-handoff.md` first. Small phase; queue (phase 5)
> already keeps references stable through reorder.

**Goal:** Sort (title/filename/path), Randomize, Reverse on the active
playlist; status rows show count + total duration + selected duration on
both frontends.

## Architecture

- Core ops on `playlist.tracks` (Vec) in `src/model.rs` (or wherever the
  active-playlist mutations live — the existing remove/jump paths):
  `sort_by_title() / sort_by_filename() / sort_by_path() / randomize() /
  reverse()`. Case-insensitive sorts; title falls back to filename when
  blank (same display fallback the row text uses). Stable sort.
- After ANY reorder: `ShuffleState::reset()` (existing house rule) +
  playlist rebuild + `current_index` re-pointed at the same TRACK (find by
  entry id from phase 5 — currently playing track must keep playing and
  stay highlighted at its new position).
- Queue interplay: entry ids stable → queue untouched by design; assert in
  tests.
- Surface: small menu on the playlist window button bar (GTK MenuButton +
  popover next to the existing Save/Add buttons; mac equivalent menu; TUI:
  keys on the playlist screen or a mini-menu — follow the TUI's existing
  affordance style).
- Status rows: GTK playlist status shows count only; mac shows total only.
  Target both: `N tracks · MM:SS total · MM:SS selected` (selected part
  hidden when nothing/all selected — propose show when ≥1 selected).
  Duration source: `length_secs` from the model rows (already loaded);
  selection-change hooks update the label. Extract a pure formatter
  `playlist_status_line(count, total_secs, selected_secs: Option) -> String`
  in core, shared GTK/TUI, mirrored on mac (or crossed via FFI if the mac
  playlist lacks lengths — it has them; Swift-side formatter mirroring
  GTK's output exactly).

## Automated tests

- Each op: ordering correct (title fallback, case-insensitivity, path vs
  filename distinction), stability, reverse twice = identity, randomize
  permutes membership (same multiset, order differs for n≥larger fixture
  with seeded RNG or retry-once guard against unlucky identity).
- current_index follows the playing track through every op.
- ShuffleState reset called (observable: shuffle history cleared).
- Queue ids intact across ops (with phase-5 queue populated).
- `playlist_status_line` formatting table: hours rollover (61:05 vs
  1:01:05 — pick H:MM:SS above 60 min), zero-selected omission.

## Manual test plan

1. Each sort on a messy playlist (missing titles, mixed case) — order sane,
   playing track keeps playing + highlight follows.
2. Randomize twice → different orders; Reverse → exact flip.
3. With queue badges present: reorder → badges follow tracks.
4. Status: add/remove tracks updates count+total live; select rows →
   selected duration appears and tracks the selection; deselect → hides.
5. mac: same menu ops + status; TUI: ops reachable, status line shows.
6. Shuffle behaves freshly after reorder (no stale history jumps).

## Open questions

1. Selected-duration visibility rule (≥1 selected shows it) — confirm.
2. TUI affordance: dedicated keys vs a small menu — propose menu-less
   keys documented in TUI help; confirm at expansion.
