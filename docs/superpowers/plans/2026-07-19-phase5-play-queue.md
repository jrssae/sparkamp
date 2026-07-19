# Phase 5 — F8 Manual Play Queue (design-level plan)

> Read `2026-07-19-opus-handoff.md` first. Queue Manager is REQUIRED
> (user decision 2026-07-19, upgrading the todo's "optional").

**Goal:** Winamp JTFE-style queue: a separate ordered list of playlist
entries that preempts normal/shuffle advance, with badges, `q` toggles,
jump-window integration, and a Queue Manager window.

## KEY CONFLICT — the `q` key (resolve exactly this way)

GTK `handle_key` binds `q`/`Q` = Quit on the shared handler (main +
playlist + shortcuts + A6 windows all route there). The todo wants `q` =
queue-toggle on playlist selection and in the jump window. Resolution:
- Jump window: its own capture-phase controller already intercepts keys —
  add `q` there = queue toggle on the highlighted match. Safe.
- Playlist window: attach a capture-phase controller ON THE PLAYLIST
  WINDOW ONLY that maps `q` → queue-toggle-selection BEFORE the shared
  handler's quit arm sees it. Main window keeps `q` = Quit.
- Shortcuts dialog documents both meanings explicitly ("q — Quit (main
  window) / Queue selection (playlist, jump)"). mac mirrors: mac has no
  q=quit (⌘Q is OS-level) — mac binds `q` = queue toggle wherever a
  playlist row/jump row has focus. 3-file sync rule applies.

## Architecture — core `src/queue.rs` (new)

- **Stable identity:** playlist entries need identity that survives
  reorder/removal and duplicate paths. Add `id: u64` to the playlist
  entry model (`src/model.rs` Track rows as stored in
  `playlist.tracks`) assigned from a monotonically increasing counter at
  insertion (persisted playlists re-assign at load — queue is
  session-only, NOT persisted, matching Winamp).
- `Queue(Vec<u64>)` API: `toggle(id)`, `enqueue(id)`, `dequeue(id)`,
  `position_of(id) -> Option<usize>` (badge number), `pop_next() ->
  Option<u64>`, `remove_missing(&playlist)`, `clear()`, `shuffle()`,
  `move_up/down(idx)` (Manager needs these).
- **Advance precedence** (in the controller's play_next/auto-advance seam,
  same place `shuffle.rs` is consulted): stop-after-current flag (phase 6)
  → queue pop → shuffle/linear. When a queued entry plays, it's removed;
  the "resume point" becomes that entry's playlist position (set
  `current_index` accordingly so linear advance continues from there —
  matches todo: "resumes from the last-queued track's playlist position").
  Prev (`z`) during queue playback: normal prev semantics, queue unaffected.
- **Invalidation hooks:** playlist remove/clear-all call
  `remove_missing`/`clear`; reorder is naturally safe (ids stable). These
  hooks land beside the existing rebuild/refresh call sites.

## UI

- Badges: playlist row text gains a `[n]` marker (GTK: in the row label
  the patch_pl_row/rebuild path builds; position-of lookup per row —
  O(queue) per row is fine, queue is small). Jump window rows mirror the
  badge. Badge updates on every queue mutation (patch affected rows, not
  full rebuild).
- Right-click playlist row → "Queue"/"Dequeue" toggle item (GTK popover
  menu — the playlist row context menu exists from the Send-to work;
  append there).
- Queue Manager (REQUIRED): GTK new `frontends/gtk/window/queue_manager.rs`
  — singleton window listing queued tracks in order (`.ml-col-view` class
  for skin selection colours), buttons: Up/Down/Remove/Clear/Randomize;
  double-click plays now (removes from queue). Open via playlist button
  bar. mac: equivalent window/sheet. TUI: queue screen with the same ops
  (keyboard) — full capability parity target.
- mac core parity: queue lives in Rust core → FFI surface:
  `sparkamp_queue_toggle(idx)`, `sparkamp_queue_position(idx) -> c_int`
  (-1 = not queued), `sparkamp_queue_count/`entry accessors for the
  Manager, `sparkamp_queue_clear/shuffle/move`. bridge.h + checklist.

## Automated tests (core is highly testable — be thorough)

- Precedence: queued entries play in order before shuffle/linear; shuffle
  state untouched during queue drain; resume from last-queued position.
- pop removes; toggle twice = no-op; duplicates of the same PATH queue
  independently (distinct ids).
- Invalidation: removing a queued track de-queues it; clear-all empties;
  reorder keeps badges pointing at the same tracks (ids).
- Interplay: stop-after-current (flag exists after phase 6; if phase order
  holds, add the precedence test here with the flag stubbed, and phase 6
  re-verifies) — queue survives stop; next play resumes queue.
- Manager ops: move_up/down bounds, shuffle keeps membership.
- FFI roundtrip smoke for the new symbols.

## Manual test plan

1. Queue 3 tracks out of order → they play in queue order, badges [1][2][3]
   visible and renumbering as the queue drains; after the last, playback
   continues linearly from that track's position.
2. Shuffle ON + queue → queued tracks still win, then shuffle resumes.
3. `q` in playlist (row selected), right-click toggle, `q` in jump window
   — all three enqueue/dequeue; main-window `q` still quits (GTK).
4. Remove a queued row / Clear All → badges vanish, no stale entries;
   reorder playlist → badges follow the tracks.
5. Manager: reorder, remove, clear, randomize; double-click plays now.
6. Jump window shows badges on matches.
7. mac + TUI parity walk.

## Open questions

1. Badge placement: prefix "[1] " before title vs suffix — propose prefix
   (Winamp look); confirm with user at layout time.
2. Queue persistence across restart: proposed NO (session-only, Winamp
   behavior). Confirm.
