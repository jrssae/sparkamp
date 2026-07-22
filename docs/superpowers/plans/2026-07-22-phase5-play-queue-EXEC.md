# Phase 5 — F8 Manual Play Queue Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Winamp JTFE-style manual play queue — a session-only ordered list of playlist entries that preempts normal/shuffle advance, with `[n]` badges, a `q` toggle, jump-window integration, and a Queue Manager window — across GTK, TUI, and macOS.

**Architecture:** A pure core `Queue(Vec<u64>)` in `src/queue.rs` keyed on a new stable per-entry `id: u64` added to `model::Track`. The controller's advance seam consults the queue before shuffle/linear. Each frontend owns a `Queue` in its app state, renders badges, and drives the same core ops. macOS reaches the core through new FFI symbols.

**Tech Stack:** Rust core; GTK4 (gtk4-rs); Ratatui/crossterm TUI; Swift/SwiftUI + C-FFI (macOS). GStreamer engine unchanged this phase.

## Global Constraints

- Build/test ONLY inside distrobox dev-box (`distrobox enter dev-box -- bash -lc 'cargo …'`). NEVER gate on `cargo build --lib` alone.
- Zero warnings AND zero test failures before any task is "done". Gate: `cargo build && cargo test` clean.
- New `src/` modules MUST be declared `mod` in BOTH `src/lib.rs` AND `src/main.rs`.
- Ask before refactoring; focus on the requested change. New files soft-capped ~800 lines.
- Comments explain WHY, not WHAT (CLAUDE.md rule). User-facing name is "Sparkamp".
- NO `git push` without a fresh explicit user instruction.
- **Queue is SESSION-ONLY** (user decision 2026-07-22): never persisted; ids reassigned at playlist load.
- **Badge is PREFIX** (user decision 2026-07-22): `"[1] Song Title"`.
- All work on branch `album-art-improvements`. Every item = GTK + mac full parity; TUI wherever its surface reaches.
- SQLite is not `Send`; GTK `RefCell` borrow discipline (never hold an `AppState` borrow across a UI call / callback / channel drain).

---

### Task 1: Stable per-entry id on `model::Track`

Playlist entries need identity that survives reorder/removal and is distinct even for duplicate paths. Session-only, so `#[serde(skip)]` + reassign at load.

**Files:**
- Modify: `src/model.rs` (`struct Track` ~:77-100; `impl Playlist` ~:318; insertion + load paths)
- Test: inline `#[cfg(test)]` in `src/model.rs`

**Interfaces:**
- Produces: `Track.id: u64` (public field). `Playlist::assign_ids(&mut self)` — (re)stamps every entry with a fresh monotonic id. `Playlist::next_entry_id: u64` counter (private). New `Track`s get `id: 0` from constructors; the owning `Playlist` stamps a real id on push/insert/load.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn every_entry_gets_a_distinct_id_even_for_duplicate_paths() {
    let mut pl = Playlist::new();
    let t = Track::from_path_fast(std::path::Path::new("/tmp/a.mp3"));
    // Duplicate the SAME path twice.
    if let Ok(track) = t {
        pl.add_track(track.clone());
        pl.add_track(track);
        assert_eq!(pl.tracks.len(), 2);
        assert_ne!(pl.tracks[0].id, pl.tracks[1].id, "duplicate paths must get distinct ids");
        assert_ne!(pl.tracks[0].id, 0, "id 0 is the unstamped sentinel");
    }
}

#[test]
fn assign_ids_restamps_all_entries_uniquely() {
    let mut pl = Playlist::new();
    for _ in 0..3 {
        if let Ok(t) = Track::from_path_fast(std::path::Path::new("/tmp/a.mp3")) {
            pl.tracks.push(t); // raw push, unstamped
        }
    }
    pl.assign_ids();
    let ids: std::collections::HashSet<u64> = pl.tracks.iter().map(|t| t.id).collect();
    assert_eq!(ids.len(), 3, "all ids distinct");
    assert!(!ids.contains(&0), "no unstamped entries remain");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `distrobox enter dev-box -- bash -lc 'cargo test -p sparkamp every_entry_gets_a_distinct_id assign_ids_restamps -- --nocapture'`
Expected: FAIL (`id` field / `assign_ids` missing).

- [ ] **Step 3: Implement**

In `struct Track`, add (after `read_only`):
```rust
    /// Session-only stable identity for the play queue (phase 5). Distinct for
    /// duplicate paths; reassigned at load. Never persisted — the queue it
    /// keys is session-only (Winamp behavior).
    #[serde(skip)]
    pub id: u64,
```
Set `id: 0` in every `Track` struct-literal / constructor in `model.rs` (each `from_path*`, `Default`-like builders). In `struct Playlist`, add `#[serde(skip)] next_entry_id: u64` (starts 0). Add:
```rust
impl Playlist {
    /// Stamp `t` with the next monotonic id and push it. All queue-aware
    /// insertion goes through here so no entry ever keeps the id-0 sentinel.
    pub fn add_track(&mut self, mut t: Track) {
        self.next_entry_id += 1;
        t.id = self.next_entry_id;
        self.tracks.push(t);
    }
    /// Reassign a fresh id to every entry (call after any bulk load/replace
    /// of `tracks`, e.g. `load_last`, playlist open, replace-with). Existing
    /// callers that `push` directly must be migrated to `add_track` OR call
    /// this afterwards.
    pub fn assign_ids(&mut self) {
        for t in &mut self.tracks {
            self.next_entry_id += 1;
            t.id = self.next_entry_id;
        }
    }
}
```
Find every site that mutates `playlist.tracks` (push/extend/insert/`= vec`) — grep `\.tracks\.push\|\.tracks\.extend\|\.tracks =\|tracks: `. Route single inserts through `add_track`; after any bulk replace or deserialize (`Playlist::load_last`, load-from-file, replace-current-with), call `assign_ids()`. (Reorder/drag mutates order in place — ids ride along, no restamp.)

- [ ] **Step 4: Run to verify pass**

Run: `distrobox enter dev-box -- bash -lc 'cargo test -p sparkamp'`
Expected: PASS, 0 warnings. Fix any `Track { … }` literal missing `id` (E0063) across the codebase (grep `Track \{`).

- [ ] **Step 5: Commit**

```bash
git add src/model.rs
git commit -m "feat(core): stable per-entry id on playlist Track for the play queue"
```

---

### Task 2: Core `Queue` (`src/queue.rs`)

Pure, highly testable. No engine, no I/O.

**Files:**
- Create: `src/queue.rs`
- Modify: `src/lib.rs` (add `pub mod queue;`), `src/main.rs` (add `mod queue;`)
- Test: inline `#[cfg(test)]` in `src/queue.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct Queue { order: Vec<u64> }
  impl Queue {
      pub fn new() -> Self
      pub fn is_empty(&self) -> bool
      pub fn len(&self) -> usize
      pub fn ids(&self) -> &[u64]                    // Manager render order
      pub fn contains(&self, id: u64) -> bool
      pub fn position_of(&self, id: u64) -> Option<usize>  // 0-based; badge = +1
      pub fn toggle(&mut self, id: u64)              // enqueue if absent, else dequeue
      pub fn enqueue(&mut self, id: u64)             // append if absent (no dup)
      pub fn dequeue(&mut self, id: u64)             // remove if present
      pub fn pop_next(&mut self) -> Option<u64>      // remove + return front
      pub fn retain_ids(&mut self, live: &std::collections::HashSet<u64>) // drop ids not in live
      pub fn clear(&mut self)
      pub fn shuffle(&mut self)                      // Fisher–Yates via rand
      pub fn move_up(&mut self, idx: usize)          // no-op at 0 / oob
      pub fn move_down(&mut self, idx: usize)        // no-op at last / oob
  }
  ```
  (`shuffle` uses the `rand` crate already in the tree — confirm `rand` is a dependency; `shuffle.rs` uses it. If deterministic tests are needed, `shuffle` stays untested for order, only membership.)

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn toggle_enqueues_then_dequeues() {
        let mut q = Queue::new();
        q.toggle(7);
        assert!(q.contains(7));
        assert_eq!(q.position_of(7), Some(0));
        q.toggle(7);
        assert!(!q.contains(7));
        assert!(q.is_empty());
    }

    #[test]
    fn enqueue_is_idempotent_and_ordered() {
        let mut q = Queue::new();
        q.enqueue(3);
        q.enqueue(9);
        q.enqueue(3); // dup ignored
        assert_eq!(q.ids(), &[3, 9]);
        assert_eq!(q.position_of(9), Some(1));
    }

    #[test]
    fn pop_next_drains_front_in_order() {
        let mut q = Queue::new();
        q.enqueue(1); q.enqueue(2); q.enqueue(3);
        assert_eq!(q.pop_next(), Some(1));
        assert_eq!(q.pop_next(), Some(2));
        assert_eq!(q.ids(), &[3]);
        assert_eq!(q.pop_next(), Some(3));
        assert_eq!(q.pop_next(), None);
    }

    #[test]
    fn retain_ids_drops_dead_entries_keeps_order() {
        let mut q = Queue::new();
        for id in [1, 2, 3, 4] { q.enqueue(id); }
        let live: HashSet<u64> = [1, 3].into_iter().collect();
        q.retain_ids(&live);
        assert_eq!(q.ids(), &[1, 3]);
    }

    #[test]
    fn move_up_down_bounds_are_noops() {
        let mut q = Queue::new();
        for id in [1, 2, 3] { q.enqueue(id); }
        q.move_up(0);          // no-op
        assert_eq!(q.ids(), &[1, 2, 3]);
        q.move_down(2);        // no-op
        assert_eq!(q.ids(), &[1, 2, 3]);
        q.move_up(2);          // 3 rises
        assert_eq!(q.ids(), &[1, 3, 2]);
        q.move_down(0);        // 1 sinks
        assert_eq!(q.ids(), &[3, 1, 2]);
    }

    #[test]
    fn shuffle_preserves_membership() {
        let mut q = Queue::new();
        for id in 1..=20 { q.enqueue(id); }
        let before: HashSet<u64> = q.ids().iter().copied().collect();
        q.shuffle();
        let after: HashSet<u64> = q.ids().iter().copied().collect();
        assert_eq!(before, after);
        assert_eq!(q.len(), 20);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `distrobox enter dev-box -- bash -lc 'cargo test -p sparkamp queue::'`
Expected: FAIL (module missing).

- [ ] **Step 3: Implement `src/queue.rs`**

```rust
//! Manual play queue (Winamp JTFE-style). A session-only ordered list of
//! playlist-entry ids that the controller drains before normal/shuffle
//! advance. Keyed on `model::Track.id` (stable per session) so it survives
//! reorder and distinguishes duplicate paths. Never persisted.

use std::collections::HashSet;

#[derive(Debug, Default, Clone)]
pub struct Queue {
    order: Vec<u64>,
}

impl Queue {
    pub fn new() -> Self { Self { order: Vec::new() } }
    pub fn is_empty(&self) -> bool { self.order.is_empty() }
    pub fn len(&self) -> usize { self.order.len() }
    pub fn ids(&self) -> &[u64] { &self.order }
    pub fn contains(&self, id: u64) -> bool { self.order.contains(&id) }
    pub fn position_of(&self, id: u64) -> Option<usize> {
        self.order.iter().position(|&x| x == id)
    }
    pub fn toggle(&mut self, id: u64) {
        if self.contains(id) { self.dequeue(id) } else { self.enqueue(id) }
    }
    pub fn enqueue(&mut self, id: u64) {
        if !self.contains(id) { self.order.push(id); }
    }
    pub fn dequeue(&mut self, id: u64) {
        self.order.retain(|&x| x != id);
    }
    pub fn pop_next(&mut self) -> Option<u64> {
        if self.order.is_empty() { None } else { Some(self.order.remove(0)) }
    }
    pub fn retain_ids(&mut self, live: &HashSet<u64>) {
        self.order.retain(|id| live.contains(id));
    }
    pub fn clear(&mut self) { self.order.clear(); }
    pub fn shuffle(&mut self) {
        use rand::seq::SliceRandom;
        self.order.shuffle(&mut rand::thread_rng());
    }
    pub fn move_up(&mut self, idx: usize) {
        if idx > 0 && idx < self.order.len() { self.order.swap(idx, idx - 1); }
    }
    pub fn move_down(&mut self, idx: usize) {
        if idx + 1 < self.order.len() { self.order.swap(idx, idx + 1); }
    }
}
```
Add `pub mod queue;` to `src/lib.rs` and `mod queue;` to `src/main.rs`. (Verify `rand`'s `SliceRandom` import matches the version in `shuffle.rs` — copy its exact use path if it differs.)

- [ ] **Step 4: Run to verify pass**

Run: `distrobox enter dev-box -- bash -lc 'cargo test -p sparkamp queue::'`
Expected: PASS, 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add src/queue.rs src/lib.rs src/main.rs
git commit -m "feat(core): pure manual play Queue with position/pop/retain/move/shuffle"
```

---

### Task 3: Advance precedence — queue drains before shuffle/linear

The controller consults the queue at the top of forward advance. When a queued entry plays it is popped and `current_index` set to that entry's playlist position, so linear advance resumes from there.

**Files:**
- Modify: `src/controller.rs` (`struct Controller` ~:95-105 add `queue: &'a mut Queue`; `nav_next` ~:167-214; `advance_to_next_playable` ~:287-340)
- Modify: every `Controller { … }` construction site (frontends build the controller each call — grep `Controller {` / `.ctrl()` helpers): thread a `&mut Queue` in. Each frontend's app state gains a `queue: Queue` field (done per-frontend in later tasks; here, add the borrow to the struct + a shared helper).
- Test: inline `#[cfg(test)]` in `src/controller.rs`

**Interfaces:**
- Consumes: `Queue::pop_next`, `Queue::contains` (Task 2); `Track.id` (Task 1).
- Produces: forward advance pops the queue first. Helper `Controller::queue_next_index(&mut self) -> Option<usize>` — pops the front queued id, resolves it to a current playlist index via `self.playlist.tracks.iter().position(|t| t.id == id)`, returns it (skips ids no longer present, popping through them). Both `nav_next` and `advance_to_next_playable` call it before their shuffle/linear branch.

- [ ] **Step 1: Write the failing test**

```rust
// In controller.rs tests. Uses the existing test harness pattern (grep an
// existing `fn make_controller`/`Controller {` test builder in this file and
// mirror it; construct with a `&mut Queue`).
#[test]
fn queued_entries_play_before_linear_then_resume_from_position() {
    // playlist: [A,B,C,D]; queue C then A.
    let mut env = test_env_with_tracks(4); // helper: builds playlist+shuffle+queue+config
    let id_a = env.playlist.tracks[0].id;
    let id_c = env.playlist.tracks[2].id;
    env.queue.enqueue(id_c);
    env.queue.enqueue(id_a);

    // First forward advance drains C.
    let _ = env.controller().nav_next();
    assert_eq!(env.playlist.current_index, 2, "queued C plays first");
    // Second drains A.
    let _ = env.controller().nav_next();
    assert_eq!(env.playlist.current_index, 0, "queued A plays next");
    assert!(env.queue.is_empty(), "queue drained");
    // Third: queue empty → linear resume from A's position → B.
    let _ = env.controller().nav_next();
    assert_eq!(env.playlist.current_index, 1, "linear resumes from last-queued position");
}

#[test]
fn shuffle_state_untouched_while_queue_drains() {
    let mut env = test_env_with_tracks(4);
    env.shuffle_state.enabled = true;
    let id_d = env.playlist.tracks[3].id;
    env.queue.enqueue(id_d);
    let hist_before = env.shuffle_state.history_len(); // or snapshot via existing API
    let _ = env.controller().nav_next();
    assert_eq!(env.playlist.current_index, 3);
    assert_eq!(env.shuffle_state.history_len(), hist_before,
        "draining the queue must not disturb shuffle history");
}
```
(Adapt `test_env_with_tracks` / `history_len` to the real test scaffolding already in `controller.rs`. If no builder exists, write a minimal one in the test module: it must produce owned `Playlist`, `ShuffleState`, `Queue`, `Config`, `Player` mocks exactly as the existing controller tests do.)

- [ ] **Step 2: Run to verify failure**

Run: `distrobox enter dev-box -- bash -lc 'cargo test -p sparkamp controller:: -- queued shuffle_state_untouched'`
Expected: FAIL (`queue` field / precedence missing).

- [ ] **Step 3: Implement**

Add to `struct Controller<'a>`: `pub queue: &'a mut Queue,` (import `use crate::queue::Queue;`). Add the helper:
```rust
    /// Pop the next still-present queued entry and return its current playlist
    /// index, or `None` when the queue is empty (or only holds ids no longer
    /// in the playlist — those are popped and skipped). On a hit, the queue
    /// has already been drained of that id.
    fn queue_next_index(&mut self) -> Option<usize> {
        while let Some(id) = self.queue.pop_next() {
            if let Some(idx) = self.playlist.tracks.iter().position(|t| t.id == id) {
                return Some(idx);
            }
        }
        None
    }
```
In `nav_next`, immediately after computing `total`/`current` and BEFORE the shuffle history/linear branches:
```rust
        if let Some(idx) = self.queue_next_index() {
            self.playlist.jump_to(idx);
            // Resume point is this entry's position; do NOT record into shuffle
            // history (queue playback is manual, not a shuffle pick).
            return NavResult::Target { was_playing };
        }
```
In `advance_to_next_playable`, add the same guard at the top (before the `shuffle_state.next_index` call ~:292), returning the queued index as the advance target (match that function's `AdvanceResult` shape — jump to `idx`, mark played per its existing convention but WITHOUT `shuffle_state.record_played` for queued hits; verify against the function body).

- [ ] **Step 4: Run to verify pass**

Run: `distrobox enter dev-box -- bash -lc 'cargo test -p sparkamp'`
Expected: PASS, 0 warnings. Fix all `Controller {` construction sites (frontends) to pass a `&mut Queue`. For frontends whose `queue` field doesn't exist yet, add a temporary owned `Queue` at the construction helper so the tree compiles; later UI tasks replace it with the real app-state field. (Prefer adding the real field now if the frontend's ctrl-builder is small.)

- [ ] **Step 5: Commit**

```bash
git add src/controller.rs frontends/
git commit -m "feat(core): queue takes precedence over shuffle/linear in forward advance"
```

---

### Task 4: Playlist invalidation hooks

Removing or clearing playlist entries must drop their queued ids; reorder is already safe (ids ride along).

**Files:**
- Modify: `src/model.rs` (playlist remove / clear-all methods) OR the controller seam that owns both `playlist` and `queue` (queue lives in app state, `model` can't see it — so hooks live wherever both are in scope: a `Controller` method + frontend remove/clear call sites).
- Test: inline `#[cfg(test)]` in `src/controller.rs`

**Interfaces:**
- Consumes: `Queue::retain_ids`, `Queue::clear` (Task 2).
- Produces: `Controller::sync_queue_to_playlist(&mut self)` — builds a `HashSet<u64>` of live entry ids and calls `queue.retain_ids`. Frontends call it after any playlist removal/clear (beside the existing rebuild/refresh call site).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn removing_a_queued_track_dequeues_it() {
    let mut env = test_env_with_tracks(3);
    let id_b = env.playlist.tracks[1].id;
    env.queue.enqueue(id_b);
    // Remove B from the playlist.
    env.playlist.tracks.remove(1);
    env.controller().sync_queue_to_playlist();
    assert!(!env.queue.contains(id_b), "removed track leaves the queue");
    assert!(env.queue.is_empty());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `distrobox enter dev-box -- bash -lc 'cargo test -p sparkamp removing_a_queued_track'`
Expected: FAIL (`sync_queue_to_playlist` missing).

- [ ] **Step 3: Implement**

```rust
    /// Drop any queued ids whose entries no longer exist in the playlist.
    /// Call after any playlist removal / clear (reorder needs no call — ids
    /// are stable across reorder).
    pub fn sync_queue_to_playlist(&mut self) {
        let live: std::collections::HashSet<u64> =
            self.playlist.tracks.iter().map(|t| t.id).collect();
        self.queue.retain_ids(&live);
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `distrobox enter dev-box -- bash -lc 'cargo test -p sparkamp'`
Expected: PASS, 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add src/controller.rs
git commit -m "feat(core): sync_queue_to_playlist drops dead queued entries"
```

---

### Task 5: GTK — queue state, badges, `q` toggle, context item

**Files:**
- Modify: `frontends/gtk/window/state.rs` (`AppState` gains `queue: crate::queue::Queue`; ctrl-builder passes `&mut queue`)
- Modify: playlist row build/patch path (grep `patch_pl_row` / the fn that sets a row's label text) — prefix `"[n] "` when `queue.position_of(entry.id)` is `Some(n-1)`.
- Modify: jump window controller (capture-phase key controller already exists) — `q` = toggle highlighted match; render badge on jump rows.
- Modify: playlist window — attach a **capture-phase** `EventControllerKey` ON THE PLAYLIST WINDOW ONLY mapping `q`/`Q` → toggle-queue-on-selection, returning `Propagation::Stop` so the shared `handle_key` quit arm never sees it. Main window keeps `q` = Quit.
- Modify: playlist row context menu (exists from Send-to work) — append a "Queue"/"Dequeue" toggle item.

**Interfaces:**
- Consumes: `Queue` API (Task 2); `Track.id` (Task 1); `sync_queue_to_playlist` (Task 4).
- Produces: a `queue_toggle_selection(&state)` helper + a `refresh_queue_badges(&state)` helper (patches affected playlist rows + jump rows, NOT a full rebuild). Every queue mutation calls `refresh_queue_badges`. Every playlist remove/clear call site also calls `sync_queue_to_playlist` then `refresh_queue_badges`.

- [ ] **Step 1: Add `queue` to `AppState`** — field + init `Queue::new()`; thread `&mut self.queue` into the ctrl builder (replace any temporary owned Queue from Task 3). Build.

- [ ] **Step 2: Badge render** — in the row-label path, compute `let badge = state.queue.position_of(entry.id).map(|i| format!("[{}] ", i + 1)).unwrap_or_default();` and prepend to the displayed title. Verify against the existing markup/escaping helper (`gtk_safe`).

- [ ] **Step 3: `q` capture controller on the playlist window** — mirror the phase-6 sync rule; add controller, on `q`/`Q` call `queue_toggle_selection` + `refresh_queue_badges`, return `Propagation::Stop`. Verify main-window `q` still quits.

- [ ] **Step 4: Jump window `q` + badges** — in the jump controller add `q` = toggle on highlighted; render `[n]` prefix on match rows.

- [ ] **Step 5: Context menu toggle** — append a `GioMenuItem` "Queue"/"Dequeue" (label reflects current membership) to the playlist row menu; action calls toggle + refresh.

- [ ] **Step 6: Invalidation** — at each playlist remove / clear-all handler, after the existing refresh, call `sync_queue_to_playlist` (via ctrl) + `refresh_queue_badges`.

- [ ] **Step 7: Build + manual smoke**

Run: `distrobox enter dev-box -- bash -lc 'cargo build'` (0 warnings). Launch `--ui`, queue 3 rows out of order, confirm badges `[1][2][3]`, `q` toggles, main-window `q` quits.

- [ ] **Step 8: Commit**

```bash
git add frontends/gtk/window/
git commit -m "feat(gtk): play queue — badges, q toggle, context item, invalidation"
```

---

### Task 6: GTK — Queue Manager window (REQUIRED)

**Files:**
- Create: `frontends/gtk/window/queue_manager.rs` (`include!`d in `window/mod.rs` like the other window modules — verify the include! list; add it there)
- Modify: `frontends/gtk/window/mod.rs` (include), playlist button bar (add a "Queue" button opening the singleton)

**Interfaces:**
- Consumes: `Queue` (Task 2), `refresh_queue_badges` (Task 5).
- Produces: `open_or_focus_queue_manager(state)` singleton (mirror `art_window::open_or_focus`). A list (`.ml-col-view` CSS class for skin selection colours) of queued entries in order; buttons Up / Down / Remove / Clear / Randomize; double-click plays now (jump to that entry + dequeue + start). Every op mutates `state.queue`, then refreshes the Manager list AND calls `refresh_queue_badges` on the main playlist.

- [ ] **Step 1** — Create the module: singleton window, list model of `state.queue.ids()` resolved to `playlist` entries (title/artist). Rebuild on open + after each op.
- [ ] **Step 2** — Buttons: Up/Down call `queue.move_up/move_down(sel)`; Remove `queue.dequeue(id)`; Clear `queue.clear()`; Randomize `queue.shuffle()`. Double-click → jump_to entry index + dequeue + `play_and_update`.
- [ ] **Step 3** — Wire the playlist button-bar "Queue" button to `open_or_focus_queue_manager`.
- [ ] **Step 4: Build + smoke** — reorder, remove, clear, randomize, double-click-plays-now; badges on the main list stay in sync.

Run: `distrobox enter dev-box -- bash -lc 'cargo build'` (0 warnings).

- [ ] **Step 5: Commit**

```bash
git add frontends/gtk/window/queue_manager.rs frontends/gtk/window/mod.rs
git commit -m "feat(gtk): Queue Manager window (reorder/remove/clear/randomize/play-now)"
```

---

### Task 7: TUI — queue screen, `q` toggle, badges

**Files:**
- Modify: `frontends/tui/mod.rs` (App gains `queue: crate::queue::Queue`; ctrl builder threads it), `frontends/tui/keys.rs` (`q` in playlist/jump context = toggle; keep global quit binding intact where it is not a playlist/jump row), `frontends/tui/ui/*` (playlist + jump render `[n]` prefix; new queue overlay/screen), a new queue-screen handler (mirror `settings_eq.rs` overlay pattern).

**Interfaces:**
- Consumes: `Queue` API; `Track.id`; `sync_queue_to_playlist`.
- Produces: TUI queue screen (list + Up/Down/Remove/Clear/Randomize/Enter-plays-now via keys), badge prefix on playlist/jump rows, `q` toggle on the focused playlist/jump row.

- [ ] **Step 1** — App `queue` field + ctrl threading; build.
- [ ] **Step 2** — Badge prefix in playlist + jump render (mirror GTK format `"[n] "`).
- [ ] **Step 3** — `q` in playlist/jump handlers toggles queue on the highlighted row + (invalidation) call `sync_queue_to_playlist` on remove/clear key paths.
- [ ] **Step 4** — Queue screen overlay: list `queue.ids()` resolved to titles; keys for move up/down, remove, clear, randomize, Enter = play now. Add its `Mode` variant + key handler + render (follow the equalizer/settings overlay scaffolding).
- [ ] **Step 5: Build + test**

Run: `distrobox enter dev-box -- bash -lc 'cargo build && cargo test'` (0 warnings, all pass).

- [ ] **Step 6: Commit**

```bash
git add frontends/tui/
git commit -m "feat(tui): play queue screen, q toggle, row badges"
```

---

### Task 8: macOS (BLIND) — FFI, badges, context, Queue Manager

Gate = the Rust suite (Swift is unbuildable here; follow existing FFI + SwiftUI patterns; add a mac-pass-checklist section).

**Files:**
- Modify: `src/ffi/mod.rs` (ctx gains `queue: crate::queue::Queue`; thread into its controller/advance seam), new `src/ffi/queue.rs` (or append to `ffi/playlist.rs`), `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`
- Modify: `frontends/SparkampMac/Sources/*` (SparkampModel queue wrappers; playlist/jump badges; row context "Queue"/"Dequeue"; new Queue Manager window/sheet)
- Modify: `docs/mac-pass-checklist.md` (phase-5 section)

**Interfaces (FFI — mirror the phase-4 FFI conventions, `#[unsafe(no_mangle)] pub unsafe extern "C"`):**
- `sparkamp_queue_toggle(ctx, playlist_index: c_int)` — resolve index → entry id → `queue.toggle`.
- `sparkamp_queue_position(ctx, playlist_index: c_int) -> c_int` — badge number (0-based +1) or `-1`.
- `sparkamp_queue_count(ctx) -> c_int`
- `sparkamp_queue_entry_index(ctx, queue_pos: c_int) -> c_int` — the playlist index of the Nth queued entry (Manager render), or `-1`.
- `sparkamp_queue_clear(ctx)`, `sparkamp_queue_shuffle(ctx)`
- `sparkamp_queue_move(ctx, queue_pos: c_int, delta: c_int)` — up = −1, down = +1.
- `sparkamp_queue_play_now(ctx, queue_pos: c_int)` — jump to that entry + dequeue + play.

- [ ] **Step 1** — ctx `queue` field + thread into the advance seam (the ffi play/advance path mirrors `controller`). Add `src/ffi/queue.rs` with the symbols; `mod queue;` in `ffi/mod.rs`.
- [ ] **Step 2** — Build + test the Rust side: `distrobox enter dev-box -- bash -lc 'cargo build --workspace && cargo test'` (0 warnings).
- [ ] **Step 3** — Mirror every symbol into `sparkamp_bridge.h` (exact C signatures).
- [ ] **Step 4 (blind)** — Swift: SparkampModel wrappers; `[n]` badge prefix in the playlist + jump row rendering; row context "Queue"/"Dequeue"; Queue Manager window (list + Up/Down/Remove/Clear/Randomize/double-click-plays-now); `q` = queue toggle where a playlist/jump row has focus (mac has no `q`=Quit). NO new-file pbxproj entries unless a new Swift file is added — prefer extending existing files; if a new file is unavoidable, document its pbxproj fileRef/buildFile ids in the checklist.
- [ ] **Step 5** — `docs/mac-pass-checklist.md`: add a "Phase 5 — Play Queue (BLIND)" section (settings/badges/manager/`q`/context, and the struct-order / bit caveats).
- [ ] **Step 6: Commit**

```bash
git add src/ffi/ frontends/SparkampMac/ docs/mac-pass-checklist.md
git commit -m "feat(mac): play queue FFI, badges, context, Queue Manager (blind)"
```

---

### Task 9: Shortcuts documentation sync (`q` dual meaning)

**Files:**
- Modify: the 3 shortcut-sync surfaces (see `[[sparkamp-gtk-parity-gaps]]` — the shortcut sync rule spans 3 files; grep the shortcuts dialog builders in GTK + mac + the shared list). Document: **`q` — Quit (main window) / Queue selection (playlist, jump)**. mac note: `q` = queue toggle (⌘Q quits at OS level). Add queue keys (Queue Manager open, move/remove/clear/randomize) to both dialogs.

- [ ] **Step 1** — Update GTK shortcuts dialog + mac KeyboardShortcutsView + any shared shortcut table with the `q` dual meaning and the Manager/queue ops.
- [ ] **Step 2: Build** — `distrobox enter dev-box -- bash -lc 'cargo build'` (0 warnings).
- [ ] **Step 3: Commit**

```bash
git add frontends/
git commit -m "docs(gtk,mac): document q dual meaning + queue shortcuts"
```

---

### Task 10: Phase close-out

**Files:**
- Modify: `docs/superpowers/specs/2026-07-17-winamp-parity-roadmap-design.md` (known-limitations for phase 5), `docs/mac-pass-checklist.md` (already has the section from Task 8 — verify), the roadmap memory ledger (mark phase 5 complete).

- [ ] **Step 1: Full gate** — `distrobox enter dev-box -- bash -lc 'cargo build --workspace && cargo test'`. Record counts; 0 warnings.
- [ ] **Step 2: Known limitations** — append a "recorded during phase 5 — F8 Play Queue" section: queue is session-only (cleared on quit / not persisted); badge is a text prefix (no separate column); TUI queue screen parity level; any mac blind caveats.
- [ ] **Step 3: Self-review the whole phase-5 diff** against this plan + the design doc's 7-item manual test plan. Note anything unverifiable blind.
- [ ] **Step 4: Manual test list** — surface the design doc's 7-item manual test plan to the user; do NOT auto-run interactive UI.
- [ ] **Step 5: Ledger** — update the parity-roadmap memory: phase 5 complete, commits, pending human passes. Confirm with the user whether/when to push (NO push without a fresh ask).

**Manual test plan (from the design doc — surface to user at close-out):**
1. Queue 3 out of order → play in queue order, badges `[1][2][3]` renumber as the queue drains; after the last, linear playback continues from that track's position.
2. Shuffle ON + queue → queued tracks still win, then shuffle resumes.
3. `q` in playlist (row selected), right-click toggle, `q` in jump window — all enqueue/dequeue; main-window `q` still quits (GTK).
4. Remove a queued row / Clear All → badges vanish, no stale entries; reorder playlist → badges follow the tracks.
5. Manager: reorder, remove, clear, randomize; double-click plays now.
6. Jump window shows badges on matches.
7. mac + TUI parity walk.

---

## Notes for the executor
- **Stop-after-current (phase 6) precedence:** the design specifies order stop-after-current → queue pop → shuffle/linear. That flag lands in phase 6; this plan implements queue-before-shuffle now. When phase 6 adds the flag, its precedence test re-verifies the full order — leave a `// phase 6: stop-after-current guards ABOVE this` comment at the `queue_next_index` call sites.
- **Prev (`z`) during queue playback:** normal prev semantics; queue is untouched (no special handling — verify `nav_prev` does not consult the queue).
- **Duplicate paths:** the whole reason for `Track.id` — never key the queue on path.
