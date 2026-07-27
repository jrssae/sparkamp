# Phase 7 — F1 Playlist Ops + Winamp Menu Bar + Duration Status Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add active-playlist Sort (Title/Artist/Album/Filename/Path), Randomize, and Reverse; restructure the GTK + mac playlist button bar into Winamp-style menu buttons (Add▸ / Select▸ / Sort▸ / List▸); give the TUI a playlist-ops popup menu; and show a `N tracks · MM:SS total · MM:SS selected` status line on all frontends.

**Architecture:** Reorder ops live on `Playlist` in `src/model.rs`, operating on the `tracks: Vec<Track>` and re-pointing `current_index` at the SAME track by its stable `id` (phase-5 ids survive reorder, so the manual queue is untouched by construction). A pure `playlist_status_line(...)` formatter lives in core and is shared by GTK/TUI, mirrored on mac. Frontends call the core op, then reset shuffle history + rebuild their view. mac reaches the ops via new C-FFI (`sparkamp_playlist_sort/reverse/randomize`), modelled on the existing `sparkamp_playlist_clear`.

**Tech Stack:** Rust core (`src/`), GTK4 (`frontends/gtk/`), Ratatui/crossterm TUI (`frontends/tui/`), macOS SwiftUI (`frontends/SparkampMac/`) via C-FFI.

## Global Constraints

- Build/test ONLY inside distrobox: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`. Host builds fail (no gstreamer/gtk dev libs). Never gate on `cargo build --lib` — GTK frontend code only compiles in the bin target.
- Zero warnings, zero failures before any "done" claim. Quote BOTH `test result:` lines (lib + bin). Grep warnings with `grep -E "warning:|error\[|error:"` (bare "warning" false-matches `thiserror`).
- New `src/` modules need `mod x;` in BOTH `src/lib.rs` AND `src/main.rs`. (This phase adds no new core module — ops go in existing `src/model.rs`.)
- macOS Swift is BLIND (no compiler here): read whole files before editing; every new/changed C-visible FFI symbol hand-mirrored in `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`; verification items appended to `docs/mac-pass-checklist.md` in the same commit.
- GTK strings reaching a widget MUST pass through `gtk_safe()` (interior-NUL guard) — applies to any track title/artist shown in a menu.
- After ANY reorder: reset shuffle history (`ShuffleState::reset()`, `src/shuffle.rs:118`) and rebuild the view. `current_index` must keep pointing at the same track (by id). The manual queue (phase 5) is id-based → untouched; assert in tests.
- Keyboard shortcuts sync across THREE places if any key changes: GTK dialog (`player.rs` `shortcut_sections()`), mac help (`KeyboardShortcutsView.swift`), mac handler (`SparkampModel+Keys.swift`). This phase adds menu items, not new global keys (existing Ctrl+S save / Ctrl+I invert stay; they gain menu entries too).
- Comments: plain English, why not what. Commits: conventional prefix, body = why + a verification line, trailer `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

## User decisions (2026-07-27)

- Sort keys: **Title, Artist, Album, Filename, Path** + **Randomize** + **Reverse**. Case-insensitive, stable sort; blank field falls back to the same display fallback the row text uses (title→filename).
- Full **Winamp menu bar** on GTK + mac: **Add▸** (Files/Folder), **Select▸** (All/None/Invert), **Sort▸** (the 5 sorts, separator, Randomize/Reverse), **List▸** (Save, separator, Remove Selected/Remove All). Replaces the current flat buttons.
- Selected duration shows the moment **≥1 row is selected** (hidden only when nothing selected).
- TUI ops via a **small popup menu overlay** (like the Jump/Queue overlays).

---

## File Structure

| File | Responsibility | Change |
|------|----------------|--------|
| `src/model.rs` | `Playlist` reorder ops (sort/reverse/randomize), id-preserving `current_index` | Modify (new methods + tests) |
| `src/playlist_status.rs` | Pure `playlist_status_line` formatter | Create (+ `mod` in lib.rs & main.rs) |
| `src/lib.rs`, `src/main.rs` | Register the new module | Modify (`mod playlist_status;`) |
| `src/ffi/playlist.rs` (or existing `src/ffi/mod.rs` playlist fns) | FFI: sort/reverse/randomize | Modify/Create |
| `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` | C header mirror | Modify |
| `frontends/gtk/window/player.rs` | Op closures, menu-bar rewrite, status line | Modify |
| `frontends/gtk/window/state.rs` | `AppState` op wrappers (reset shuffle + re-point) | Modify |
| `frontends/tui/keys.rs`, `frontends/tui/mod.rs` | Ops + popup mode | Modify |
| `frontends/tui/ui/overlays.rs` | Playlist-ops popup overlay | Modify |
| `frontends/SparkampMac/Sources/PlaylistView.swift` | Menu-bar rewrite + status line | Modify |
| `frontends/SparkampMac/Sources/SparkampModel+Transport.swift` | Swift op wrappers over FFI | Modify |
| `docs/mac-pass-checklist.md` | Phase-7 verification section | Modify |
| `docs/superpowers/specs/2026-07-17-winamp-parity-roadmap-design.md` | Known-limitations close-out | Modify |

---

## Task 1: Core — playlist Sort ops (id-preserving)

**Files:**
- Modify: `src/model.rs` (`impl Playlist`, near `move_track:472`)
- Test: `src/model.rs` `#[cfg(test)]`

**Interfaces:**
- Produces on `Playlist`:
  - `fn sort_by(&mut self, key: SortKey)` where `pub enum SortKey { Title, Artist, Album, Filename, Path }`
  - Helper `fn repoint_current_to(&mut self, id: u64)` (find the track with `id`, set `current_index`; no-op if absent)
  - `fn current_id(&self) -> Option<u64>`

- [ ] **Step 1: Add the `SortKey` enum.** Near the top of `src/model.rs` (by other public enums):

```rust
/// Sort criterion for the active-playlist Sort menu (phase 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Title,
    Artist,
    Album,
    Filename,
    Path,
}
```

- [ ] **Step 2: Add id helpers + `sort_by`.** In `impl Playlist` (after `move_track`, ~line 502):

```rust
    /// Id of the current track, if any — used to keep the playing track
    /// selected across a reorder.
    pub fn current_id(&self) -> Option<u64> {
        self.tracks.get(self.current_index).map(|t| t.id)
    }

    /// Point `current_index` at the track with `id`; unchanged if not found.
    pub fn repoint_current_to(&mut self, id: u64) {
        if let Some(pos) = self.tracks.iter().position(|t| t.id == id) {
            self.current_index = pos;
        }
    }

    /// Case-insensitive, stable sort of the active playlist. The playing track
    /// stays current (re-pointed by id afterwards). Blank sort fields fall back
    /// to the filename so untitled rows sort by their visible label.
    pub fn sort_by(&mut self, key: SortKey) {
        let playing = self.current_id();
        // Precompute a lowercase key per track so the comparator is cheap and
        // the sort stays stable (sort_by_key is stable in std).
        self.tracks.sort_by(|a, b| sort_field(a, key).cmp(&sort_field(b, key)));
        if let Some(id) = playing {
            self.repoint_current_to(id);
        }
    }
```

- [ ] **Step 3: Add the field extractor** (free fn in `src/model.rs`):

```rust
/// The lowercase string a track sorts on for `key`. Title/Artist/Album fall
/// back to the filename when blank (mirrors the row display fallback).
fn sort_field(t: &Track, key: SortKey) -> String {
    let filename = || {
        t.path
            .file_name()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    match key {
        SortKey::Title => {
            if t.title.trim().is_empty() { filename() } else { t.title.to_lowercase() }
        }
        SortKey::Artist => {
            if t.artist.trim().is_empty() { filename() } else { t.artist.to_lowercase() }
        }
        SortKey::Album => {
            if t.album.trim().is_empty() { filename() } else { t.album.to_lowercase() }
        }
        SortKey::Filename => filename(),
        SortKey::Path => t.path.to_string_lossy().to_lowercase(),
    }
}
```

- [ ] **Step 4: Write the failing tests.** Use the existing test-module `Track` construction idiom (see the `move_track` tests ~line 1360; build tracks with explicit fields + ids):

```rust
    #[test]
    fn sort_by_title_is_case_insensitive_and_stable_with_filename_fallback() {
        let mut pl = Playlist::new();
        pl.add(track_named(1, "banana", "/z/1.mp3"));
        pl.add(track_named(2, "Apple", "/a/2.mp3"));
        pl.add(track_named(3, "", "/m/aaa.mp3")); // blank title → "aaa.mp3"
        pl.sort_by(SortKey::Title);
        let order: Vec<u64> = pl.tracks.iter().map(|t| t.id).collect();
        assert_eq!(order, vec![3, 2, 1]); // aaa.mp3, apple, banana
    }

    #[test]
    fn sort_keeps_the_playing_track_current() {
        let mut pl = Playlist::new();
        pl.add(track_named(1, "banana", "/1.mp3"));
        pl.add(track_named(2, "apple", "/2.mp3"));
        pl.jump_to(0); // playing "banana"
        pl.sort_by(SortKey::Title);
        assert_eq!(pl.current_id(), Some(1), "still on banana after sort");
        assert_eq!(pl.current_index, 1, "banana moved to the end");
    }
```

Add a `track_named(id, title, path)` test helper if one isn't already present (mirror the existing full-struct literal used by the `move_track` tests — set the other String fields to `String::new()` and `duration: None`).

- [ ] **Step 5: Run tests.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --lib sort_'`
Expected: FAIL before Steps 1–3, PASS after.

- [ ] **Step 6: Commit.**

```bash
git add src/model.rs
git commit -m "feat(core): active-playlist sort ops keeping the playing track current (phase 7)"
```

---

## Task 2: Core — Reverse + Randomize (id-preserving, queue-safe)

**Files:**
- Modify: `src/model.rs`
- Test: `src/model.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `current_id`/`repoint_current_to` (Task 1)
- Produces: `Playlist::reverse()`, `Playlist::randomize()`

- [ ] **Step 1: Implement.** In `impl Playlist`:

```rust
    /// Reverse the active playlist; the playing track stays current.
    pub fn reverse(&mut self) {
        let playing = self.current_id();
        self.tracks.reverse();
        if let Some(id) = playing {
            self.repoint_current_to(id);
        }
    }

    /// Randomly permute the active playlist once (a one-shot reorder, distinct
    /// from shuffle PLAYBACK). The playing track stays current.
    pub fn randomize(&mut self) {
        let playing = self.current_id();
        // Fisher–Yates via the crate's existing RNG (see how shuffle.rs draws
        // randomness — reuse that dependency, do not add a new one).
        let n = self.tracks.len();
        for i in (1..n).rev() {
            let j = rand_index(i + 1); // uniform in [0, i]
            self.tracks.swap(i, j);
        }
        if let Some(id) = playing {
            self.repoint_current_to(id);
        }
    }
```

> `rand_index`: reuse whatever RNG `src/shuffle.rs` already uses (check its imports — likely `rand` or a small xorshift). If `shuffle.rs` exposes a helper, call it; otherwise mirror its RNG construction in a small private fn in `model.rs`. Do NOT add a new crate.

- [ ] **Step 2: Failing tests** (reverse identity; randomize preserves multiset; playing track follows; queue ids intact):

```rust
    #[test]
    fn reverse_twice_is_identity_and_keeps_current() {
        let mut pl = Playlist::new();
        for i in 0..4 { pl.add(track_named(i, &format!("t{i}"), &format!("/{i}.mp3"))); }
        pl.jump_to(1);
        let before: Vec<u64> = pl.tracks.iter().map(|t| t.id).collect();
        pl.reverse();
        pl.reverse();
        let after: Vec<u64> = pl.tracks.iter().map(|t| t.id).collect();
        assert_eq!(before, after);
        assert_eq!(pl.current_id(), Some(1));
    }

    #[test]
    fn randomize_preserves_membership_and_current() {
        let mut pl = Playlist::new();
        for i in 0..20 { pl.add(track_named(i, &format!("t{i}"), &format!("/{i}.mp3"))); }
        pl.jump_to(7);
        let before: std::collections::HashSet<u64> = pl.tracks.iter().map(|t| t.id).collect();
        pl.randomize();
        let after: std::collections::HashSet<u64> = pl.tracks.iter().map(|t| t.id).collect();
        assert_eq!(before, after, "same multiset of tracks");
        assert_eq!(pl.current_id(), Some(7), "playing track still current");
    }
```

> For a permutation-changed assertion, avoid flakiness: with n=20 the identity permutation is astronomically unlikely, but do NOT assert "order differs" (can flake). Assert membership + current only. The reorder is exercised; ordering-differs is covered by the manual test plan.

- [ ] **Step 3: Queue-intact test.** Prove the phase-5 queue (id-based) survives a reorder. In `src/controller.rs` tests (they already build a queue via the `Fixture`), or a model-level test if `Queue` is reachable:

```rust
    // In controller.rs tests, using Fixture(4) with an enqueued id:
    #[test]
    fn reorder_leaves_the_queue_intact() {
        let mut f = Fixture::new(4);
        let id2 = f.playlist.tracks[2].id;
        f.queue.enqueue(id2);
        f.playlist.reverse();
        // Queue still holds id2; it now resolves to a different index but the
        // same track.
        assert!(f.queue.contains(id2));
        assert!(f.playlist.tracks.iter().any(|t| t.id == id2));
    }
```

- [ ] **Step 4: Run + commit.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --lib reverse_ randomize_ reorder_leaves'`
Expected: PASS.

```bash
git add src/model.rs src/controller.rs
git commit -m "feat(core): playlist reverse + randomize, queue ids intact (phase 7)"
```

---

## Task 3: Core — `playlist_status_line` formatter

**Files:**
- Create: `src/playlist_status.rs`
- Modify: `src/lib.rs`, `src/main.rs` (`mod playlist_status;`)
- Test: inline in the new file

**Interfaces:**
- Produces: `pub fn playlist_status_line(count: usize, total_secs: u64, selected_secs: Option<u64>) -> String`

- [ ] **Step 1: Create the module.** `src/playlist_status.rs`:

```rust
//! Shared formatter for the active-playlist status line (phase 7).
//! `N tracks · MM:SS total · MM:SS selected` — the selected clause is present
//! only when `selected_secs` is `Some` (frontends pass it when ≥1 row is
//! selected). Durations roll over to H:MM:SS above one hour.

/// Format a duration as `M:SS` under an hour, `H:MM:SS` at/above an hour.
fn fmt_hms(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn playlist_status_line(count: usize, total_secs: u64, selected_secs: Option<u64>) -> String {
    let noun = if count == 1 { "track" } else { "tracks" };
    let mut line = format!("{count} {noun} · {} total", fmt_hms(total_secs));
    if let Some(sel) = selected_secs {
        line.push_str(&format!(" · {} selected", fmt_hms(sel)));
    }
    line
}
```

- [ ] **Step 2: Register the module.** Add `mod playlist_status;` to `src/lib.rs` AND `src/main.rs` (both — the bin target needs it). If `lib.rs` re-exports, add `pub use playlist_status::playlist_status_line;` where the other `pub use`s live so frontends can reach it as `crate::playlist_status_line` (match the existing re-export style; if there is none, call it `crate::playlist_status::playlist_status_line`).

- [ ] **Step 3: Failing tests** (append to the file):

```rust
#[cfg(test)]
mod tests {
    use super::playlist_status_line;

    #[test]
    fn singular_plural_and_no_selection() {
        assert_eq!(playlist_status_line(1, 65, None), "1 track · 1:05 total");
        assert_eq!(playlist_status_line(12, 2900, None), "12 tracks · 48:20 total");
    }

    #[test]
    fn selected_clause_and_hour_rollover() {
        assert_eq!(
            playlist_status_line(12, 3665, Some(664)),
            "12 tracks · 1:01:05 total · 11:04 selected"
        );
        assert_eq!(playlist_status_line(0, 0, None), "0 tracks · 0:00 total");
    }
}
```

- [ ] **Step 4: Run + commit.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --lib playlist_status'`
Expected: PASS.

```bash
git add src/playlist_status.rs src/lib.rs src/main.rs
git commit -m "feat(core): playlist_status_line formatter (count/total/selected) (phase 7)"
```

---

## Task 4: GTK — AppState op wrappers + apply-reorder helper

**Files:**
- Modify: `frontends/gtk/window/state.rs` (op wrappers), `frontends/gtk/window/player.rs` (an `apply_reorder` closure)
- Test: build-gated + manual

**Interfaces:**
- Consumes: `Playlist::{sort_by, reverse, randomize}` (Tasks 1–2), `SortKey`
- Produces:
  - On `AppState` (state.rs): `pub(crate) fn sort_playlist(&mut self, key: SortKey)`, `reverse_playlist(&mut self)`, `randomize_playlist(&mut self)` — each calls the `Playlist` op then `self.shuffle_state.reset()`.
  - In player.rs: `let apply_reorder: Rc<dyn Fn(&dyn Fn(&mut AppState))>` — runs a mutation, then rebuilds the playlist view, re-points the selection/scroll, and refreshes queue badges.

- [ ] **Step 1: AppState wrappers.** In `frontends/gtk/window/state.rs` (near the other playlist mutators):

```rust
    pub(crate) fn sort_playlist(&mut self, key: crate::model::SortKey) {
        self.playlist.sort_by(key);
        self.shuffle_state.reset();
    }
    pub(crate) fn reverse_playlist(&mut self) {
        self.playlist.reverse();
        self.shuffle_state.reset();
    }
    pub(crate) fn randomize_playlist(&mut self) {
        self.playlist.randomize();
        self.shuffle_state.reset();
    }
```

- [ ] **Step 2: `apply_reorder` closure.** In `player.rs`, near the other playlist closures (after `rebuild_playlist` is defined), add a helper that runs an op and refreshes the view. Reuse the existing `rebuild_playlist` and the queue-badge refresh already used by phase-5 reorders:

```rust
    // Run a reorder op, then rebuild the playlist view and refresh queue badges.
    // The op mutates AppState (which resets shuffle history); the playing track
    // stays current by id, so playback continues and its highlight follows.
    let apply_reorder: Rc<dyn Fn(&(dyn Fn(&mut AppState)))> = {
        let state = state.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        Rc::new(move |op: &dyn Fn(&mut AppState)| {
            {
                let mut s = state.borrow_mut();
                op(&mut s);
            }
            rebuild_playlist();
        })
    };
```

> Confirm the exact `AppState` type name used inside `player.rs` (it may be aliased). If `rebuild_playlist` already renumbers queue badges (phase-5 wired it to), nothing more is needed; if badges are patched separately, also call the badge-refresh closure here. Grep `rebuild_playlist` to see what it fans out.

- [ ] **Step 3: Build.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings (closures unused until Task 5 — if the unused-warning fires, proceed to Task 5 in the same commit).

- [ ] **Step 4: Commit (fold with Task 5 if unused-warnings).** If clean:

```bash
git add frontends/gtk/window/state.rs frontends/gtk/window/player.rs
git commit -m "feat(gtk): AppState reorder wrappers + apply-reorder helper (phase 7)"
```

---

## Task 5: GTK — Winamp menu bar (Add▸ / Select▸ / Sort▸ / List▸)

**Files:**
- Modify: `frontends/gtk/window/player.rs` (button bar at ~888–919; `shortcut_sections()` unaffected)
- Test: build-gated + manual

**Interfaces:**
- Consumes: `apply_reorder` (Task 4), `SortKey`, existing closures `btn_add_files`/`btn_add_dir` handlers, `btn_save_active`, `remove_selected`, `invert_selection` (phase 6), the clear-all handler.

- [ ] **Step 1: Build a MenuButton helper.** Near the button-bar construction (~888), add a small helper that makes a `MenuButton` with a vertical popover of labelled buttons wired to closures:

```rust
    // Build a Winamp-style menu button: a labelled MenuButton whose popover is
    // a vertical list of action buttons. `items` are (label, Some(callback)) or
    // (label, None) for a separator.
    fn menu_button(
        label: &str,
        items: Vec<(&str, Option<Rc<dyn Fn()>>)>,
    ) -> gtk4::MenuButton {
        let vbox = GtkBox::new(Orientation::Vertical, 2);
        for (text, cb) in items {
            match cb {
                None => {
                    let sep = gtk4::Separator::new(Orientation::Horizontal);
                    vbox.append(&sep);
                }
                Some(cb) => {
                    let b = Button::with_label(text);
                    b.add_css_class("flat");
                    b.set_halign(Align::Fill);
                    b.connect_clicked(move |_| cb());
                    vbox.append(&b);
                }
            }
        }
        let popover = gtk4::Popover::new();
        popover.set_child(Some(&vbox));
        let mb = gtk4::MenuButton::new();
        mb.set_label(label);
        mb.set_popover(Some(&popover));
        // Close the popover after any action so it behaves like a menu.
        for child in [] as [gtk4::Widget; 0] { let _ = child; } // (no-op; buttons popdown below)
        mb
    }
```

> Buttons inside a `Popover` don't auto-dismiss it. Give each action button a clone of the popover and call `popover.popdown()` at the end of its callback — simplest is to wrap the callback: inside `connect_clicked`, `cb(); pop.popdown();` where `pop` is a clone of `popover`. Adjust `menu_button` to capture the popover into each button's handler.

- [ ] **Step 2: Construct the four menus.** Replace the flat-button appends (`pl_btn_row.append(&btn_add_files)` … `&btn_clear_all`, lines ~911–918) with four menu buttons. Wrap the EXISTING handlers as `Rc<dyn Fn()>` closures (emit the existing buttons' clicks to reuse their logic, or call the shared closures directly):

```rust
    // Add▸
    let add_menu = menu_button("Add ▾", vec![
        ("Add Files…",  Some({ let b = btn_add_files.clone(); Rc::new(move || b.emit_clicked()) as Rc<dyn Fn()> })),
        ("Add Folder…", Some({ let b = btn_add_dir.clone();   Rc::new(move || b.emit_clicked()) as Rc<dyn Fn()> })),
    ]);
    // Select▸
    let select_menu = menu_button("Select ▾", vec![
        ("Select All",       Some({ let pv = pl_view.clone(); Rc::new(move || pv.selection().select_all()) as Rc<dyn Fn()> })),
        ("Select None",      Some({ let pv = pl_view.clone(); Rc::new(move || pv.selection().unselect_all()) as Rc<dyn Fn()> })),
        ("Invert Selection", Some({ let inv = invert_selection.clone(); Rc::new(move || inv()) as Rc<dyn Fn()> })),
    ]);
    // Sort▸
    let sort_item = |label: &str, key: crate::model::SortKey| {
        let ar = apply_reorder.clone();
        (label, Some(Rc::new(move || ar(&move |s: &mut AppState| s.sort_playlist(key))) as Rc<dyn Fn()>))
    };
    let sort_menu = menu_button("Sort ▾", vec![
        sort_item("Title",    crate::model::SortKey::Title),
        sort_item("Artist",   crate::model::SortKey::Artist),
        sort_item("Album",    crate::model::SortKey::Album),
        sort_item("Filename", crate::model::SortKey::Filename),
        sort_item("Path",     crate::model::SortKey::Path),
        ("", None),
        ("Randomize", Some({ let ar = apply_reorder.clone(); Rc::new(move || ar(&|s: &mut AppState| s.randomize_playlist())) as Rc<dyn Fn()> })),
        ("Reverse",   Some({ let ar = apply_reorder.clone(); Rc::new(move || ar(&|s: &mut AppState| s.reverse_playlist())) as Rc<dyn Fn()> })),
    ]);
    // List▸
    let list_menu = menu_button("List ▾", vec![
        ("Save Playlist…", Some({ let b = btn_save_active.clone(); Rc::new(move || b.emit_clicked()) as Rc<dyn Fn()> })),
        ("", None),
        ("Remove Selected", Some({ let rs = remove_selected.clone(); Rc::new(move || rs()) as Rc<dyn Fn()> })),
        ("Remove All",      Some({ let b = btn_clear_all.clone(); Rc::new(move || b.emit_clicked()) as Rc<dyn Fn()> })),
    ]);

    pl_btn_row.append(&add_menu);
    pl_btn_row.append(&select_menu);
    pl_btn_row.append(&sort_menu);
    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    pl_btn_row.append(&spacer);
    pl_btn_row.append(&list_menu);
    pl_btn_row.append(&btn_cancel); // keep the scan-cancel button
```

> The original buttons (`btn_add_files`, `btn_add_dir`, `btn_save_active`, `btn_remove`, `btn_clear_all`) stay CONSTRUCTED and keep their `connect_clicked` handlers (the menus emit their clicks) — just don't `append` them to the row. Keep `btn_cancel` visible (it toggles during scans). `pl_view.selection().select_all()`/`unselect_all()` are gtk4 `TreeSelectionExt` methods; confirm the exact names (`select_all`, `unselect_all` exist for Multiple selection). Verify `AppState` closure-type coercion compiles; if the `&move |s|` double-reference is awkward, change `apply_reorder` to take `Box<dyn Fn(&mut AppState)>` and box at the call site.

- [ ] **Step 3: Build + manual.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings. Each menu opens; Sort/Randomize/Reverse reorder with the playing track staying put; Add/Save/Remove/Select behave as before.

- [ ] **Step 4: Commit.**

```bash
git add frontends/gtk/window/player.rs
git commit -m "feat(gtk): Winamp menu bar — Add/Select/Sort/List on the playlist (phase 7)"
```

---

## Task 6: GTK — status line (count · total · selected)

**Files:**
- Modify: `frontends/gtk/window/player.rs` (`pl_status_label` ~line 1090; selection-changed + rebuild hooks)
- Test: build-gated + manual

**Interfaces:**
- Consumes: `crate::playlist_status::playlist_status_line` (Task 3), `AppState.playlist.tracks[].duration`

- [ ] **Step 1: Build a status-refresh closure.** Near `pl_status_label`, add:

```rust
    // Refresh the playlist status line: count · total · (selected when ≥1 row).
    let refresh_pl_status: Rc<dyn Fn()> = {
        let state = state.clone();
        let pl_status_label = pl_status_label.clone();
        let pl_view = pl_view.clone();
        Rc::new(move || {
            let (count, total) = {
                let s = state.borrow();
                let total: u64 = s.playlist.tracks.iter()
                    .map(|t| t.duration.map(|d| d.as_secs()).unwrap_or(0))
                    .sum();
                (s.playlist.tracks.len(), total)
            };
            // Selected duration — sum durations of selected TreeView rows.
            let (sel_paths, _) = pl_view.selection().selected_rows();
            let selected = if sel_paths.is_empty() {
                None
            } else {
                let s = state.borrow();
                let sum: u64 = sel_paths.iter()
                    .filter_map(|p| p.indices().first().copied())
                    .filter_map(|i| s.playlist.tracks.get(i as usize))
                    .map(|t| t.duration.map(|d| d.as_secs()).unwrap_or(0))
                    .sum();
                Some(sum)
            };
            pl_status_label.set_text(&crate::playlist_status::playlist_status_line(count, total, selected));
        })
    };
```

- [ ] **Step 2: Call it on the relevant events.** Invoke `refresh_pl_status()`:
  - Once after initial playlist build.
  - On selection change: in `pl_view.selection().connect_changed({... move |_| refresh_pl_status() })` (there is already a `connect_changed` handler ~line 1288/1631 — add the call there rather than a second controller, to avoid churn).
  - After any add/remove/reorder: call it inside `rebuild_playlist` (single choke point) OR right after each `rebuild_playlist()` invocation. Prefer adding one call at the end of the `rebuild_playlist` closure body so every mutation refreshes the line.

> Clone `refresh_pl_status` into each site. Ensure no `RefCell` double-borrow: `refresh_pl_status` borrows `state` immutably; don't call it while a `borrow_mut` is held (call it AFTER the mutation scope closes — the `apply_reorder`/rebuild ordering already does this).

- [ ] **Step 3: Build + manual.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings. Status shows `N tracks · MM:SS total`; selecting rows adds `· MM:SS selected`; deselecting all hides it; add/remove/sort update live.

- [ ] **Step 4: Commit.**

```bash
git add frontends/gtk/window/player.rs
git commit -m "feat(gtk): playlist status line — count, total, selected duration (phase 7)"
```

---

## Task 7: TUI — playlist-ops popup menu

**Files:**
- Modify: `frontends/tui/mod.rs` (a `Mode::PlaylistOps` + op wrappers), `frontends/tui/keys.rs` (open key + menu navigation), `frontends/tui/ui/overlays.rs` (draw the popup)
- Test: build-gated + a pure-ordering smoke test via the core ops (already covered by Tasks 1–2)

**Interfaces:**
- Consumes: `App`'s controller/playlist; core `Playlist::{sort_by,reverse,randomize}`; `ShuffleState::reset`

- [ ] **Step 1: Add the mode.** In `frontends/tui/mod.rs`, extend the `Mode` enum with `PlaylistOps { selected: usize }` (mirror the existing `Mode::Queue { selected }` shape).

- [ ] **Step 2: Op wrappers on `App`.** Add methods that call the core op then reset shuffle + fix the cursor (mirror how phase-5 reorder updated TUI state):

```rust
    pub(super) fn playlist_sort(&mut self, key: crate::model::SortKey) {
        self.playlist.sort_by(key);
        self.shuffle_state.reset();
        self.playlist_cursor = self.playlist.current_index;
    }
    pub(super) fn playlist_reverse(&mut self) {
        self.playlist.reverse();
        self.shuffle_state.reset();
        self.playlist_cursor = self.playlist.current_index;
    }
    pub(super) fn playlist_randomize(&mut self) {
        self.playlist.randomize();
        self.shuffle_state.reset();
        self.playlist_cursor = self.playlist.current_index;
    }
```

> Confirm the field names (`self.playlist`, `self.shuffle_state`, `self.playlist_cursor`) against how phase-5 ops mutate TUI state — reuse the exact cursor-fix idiom the move/remove keys use.

- [ ] **Step 3: Open key + menu handling.** In `frontends/tui/keys.rs` normal-mode dispatch, add a key to open the popup (use a free key — `o` for "ops"; verify it's unused in TUI normal mode via `grep "KeyCode::Char('o')" frontends/tui/keys.rs`). Then add a `handle_playlist_ops(code)` handler (mirror `handle_queue`): Up/Down move `selected`, Enter runs the highlighted op and closes, Esc closes. Menu entries in order: Sort Title / Sort Artist / Sort Album / Sort Filename / Sort Path / Randomize / Reverse / Select All / Select None / Invert. (Selection ops act on the TUI's selection concept if it has one; if the TUI has no multi-select, omit the Select entries and note it.)

```rust
            // o — playlist ops popup (sort / randomize / reverse).
            KeyCode::Char('o') => {
                self.mode = Mode::PlaylistOps { selected: 0 };
            }
```

Dispatch in the mode match (near `Mode::Queue { .. } => self.handle_queue(code)`):

```rust
            Mode::PlaylistOps { .. } => self.handle_playlist_ops(code),
```

- [ ] **Step 4: Draw the popup.** In `frontends/tui/ui/overlays.rs`, add `draw_playlist_ops_overlay(frame, app, area)` modelled on `draw_queue_overlay`: a centered popup listing the op labels, highlighting `selected`. Wire it into `ui/mod.rs`'s overlay match (`Mode::PlaylistOps { .. } => draw_playlist_ops_overlay(...)`).

- [ ] **Step 5: Build + manual.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: 0 warnings; both `test result:` lines pass. `o` opens the popup; arrows navigate; Enter sorts/reorders and the playing track stays; Esc closes.

- [ ] **Step 6: Commit.**

```bash
git add frontends/tui/
git commit -m "feat(tui): playlist-ops popup menu (sort/randomize/reverse) (phase 7)"
```

---

## Task 8: TUI — status line

**Files:**
- Modify: `frontends/tui/ui/mod.rs` (playlist hints/status area) or wherever the playlist footer renders
- Test: build-gated (formatter already unit-tested in Task 3)

- [ ] **Step 1: Render the status line.** Find the TUI playlist footer/hints render (`grep -n "playlist_hints\|draw_playlist" frontends/tui/ui/mod.rs`). Compute count + total from `app.playlist.tracks` (durations) and selected from the TUI selection if present (else `None`), then render `crate::playlist_status::playlist_status_line(...)` in the playlist status/footer row:

```rust
    let total: u64 = app.playlist.tracks.iter()
        .map(|t| t.duration.map(|d| d.as_secs()).unwrap_or(0)).sum();
    let line = crate::playlist_status::playlist_status_line(
        app.playlist.tracks.len(), total, None, // TUI: no multi-select → None
    );
```

> If the TUI has no multi-select selection model, pass `None` for selected and note it in the checklist/limitations. Place the line where the existing count (if any) shows; otherwise add a dim line in the playlist footer.

- [ ] **Step 2: Build + manual.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings; footer shows `N tracks · MM:SS total`, updates on add/remove/reorder.

- [ ] **Step 3: Commit.**

```bash
git add frontends/tui/
git commit -m "feat(tui): playlist status line (count · total) (phase 7)"
```

---

## Task 9: Core FFI — sort / reverse / randomize for mac

**Files:**
- Modify: `src/ffi/mod.rs` (or the file holding `sparkamp_playlist_clear`/`_jump`), `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`
- Test: `cargo build`

**Interfaces:**
- Produces: `sparkamp_playlist_sort(ctx, kind: c_int)`, `sparkamp_playlist_reverse(ctx)`, `sparkamp_playlist_randomize(ctx)`. `kind`: 0=Title 1=Artist 2=Album 3=Filename 4=Path.

- [ ] **Step 1: Find the playlist FFI seam.** `grep -rn "sparkamp_playlist_clear\|sparkamp_playlist_jump" src/ffi/`. Note how it reaches the playlist from `ctx` and whether it resets shuffle after mutation (mirror that; if those fns call a `ctx`-level reset/refresh, do the same).

- [ ] **Step 2: Add the FFI.** Beside `sparkamp_playlist_clear`:

```rust
/// Sort the active playlist (phase 7). kind: 0=Title 1=Artist 2=Album
/// 3=Filename 4=Path. Keeps the playing track current; resets shuffle history.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_sort(ctx: *mut SparkampCtx, kind: c_int) {
    if ctx.is_null() { return; }
    let ctx = &mut *ctx;
    let key = match kind {
        0 => crate::model::SortKey::Title,
        1 => crate::model::SortKey::Artist,
        2 => crate::model::SortKey::Album,
        3 => crate::model::SortKey::Filename,
        _ => crate::model::SortKey::Path,
    };
    ctx.playlist.sort_by(key);
    ctx.shuffle.reset(); // match the field names the clear/jump fns use
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_reverse(ctx: *mut SparkampCtx) {
    if ctx.is_null() { return; }
    let ctx = &mut *ctx;
    ctx.playlist.reverse();
    ctx.shuffle.reset();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_randomize(ctx: *mut SparkampCtx) {
    if ctx.is_null() { return; }
    let ctx = &mut *ctx;
    ctx.playlist.randomize();
    ctx.shuffle.reset();
}
```

> Use the EXACT `ctx` field paths the neighbouring playlist FFI uses (`ctx.playlist`, `ctx.shuffle`/`ctx.shuffle_state` — verify). If `sparkamp_playlist_clear` calls a helper that also persists/refreshes state, call the same helper here so mac reorders behave like clear.

- [ ] **Step 3: Mirror in the header.** In `sparkamp_bridge.h`, beside `sparkamp_playlist_clear`:

```c
/* Active-playlist reorder ops (phase 7). sort kind: 0=Title 1=Artist 2=Album
   3=Filename 4=Path. Playing track stays current; shuffle history resets. */
void sparkamp_playlist_sort(SparkampCtx *ctx, int kind);
void sparkamp_playlist_reverse(SparkampCtx *ctx);
void sparkamp_playlist_randomize(SparkampCtx *ctx);
```

- [ ] **Step 4: Build + commit.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings.

```bash
git add src/ffi/ frontends/SparkampMac/SparkampCore/sparkamp_bridge.h
git commit -m "feat(ffi): playlist sort/reverse/randomize for mac (phase 7)"
```

---

## Task 10: mac (blind) — Winamp menu bar + status line

**Files:**
- Modify: `frontends/SparkampMac/Sources/SparkampModel+Transport.swift` (Swift op wrappers), `frontends/SparkampMac/Sources/PlaylistView.swift` (menu bar + status), `docs/mac-pass-checklist.md`
- Test: read whole files first; verification via checklist

- [ ] **Step 1: Read the files.** Fully read `PlaylistView.swift` and `SparkampModel+Transport.swift` before editing (blind-mac rule).

- [ ] **Step 2: Swift op wrappers.** In `SparkampModel+Transport.swift`, add methods over the FFI + a `refreshAll()`:

```swift
    enum PlaylistSortKey: Int32 { case title = 0, artist = 1, album = 2, filename = 3, path = 4 }

    func sortPlaylist(_ key: PlaylistSortKey) {
        guard let ctx = ctx else { return }
        sparkamp_playlist_sort(ctx, key.rawValue)
        refreshAll(); saveState()
    }
    func reversePlaylist() {
        guard let ctx = ctx else { return }
        sparkamp_playlist_reverse(ctx); refreshAll(); saveState()
    }
    func randomizePlaylist() {
        guard let ctx = ctx else { return }
        sparkamp_playlist_randomize(ctx); refreshAll(); saveState()
    }
```

> Use the model's real refresh method (`refreshAll` is used by `next()`/`prev()` — reuse it so `playlistItems` and the badges update).

- [ ] **Step 3: Rewrite `bottomBar` into menu buttons.** Replace the flat `Button`s in `PlaylistView.bottomBar` (~line 489) with SwiftUI `Menu`s:

```swift
            Menu("Add") {
                Button("Add Files…")  { model.openFilePicker() }
                Button("Add Folder…") { model.openFolderPicker() }
            }
            Menu("Select") {
                Button("Select All")       { selection = Set(model.playlistItems.map { $0.id }) }
                Button("Select None")      { selection.removeAll() }
                Button("Invert Selection") { selection = Set(model.playlistItems.map { $0.id }).subtracting(selection) }
            }
            Menu("Sort") {
                Button("Title")    { model.sortPlaylist(.title) }
                Button("Artist")   { model.sortPlaylist(.artist) }
                Button("Album")    { model.sortPlaylist(.album) }
                Button("Filename") { model.sortPlaylist(.filename) }
                Button("Path")     { model.sortPlaylist(.path) }
                Divider()
                Button("Randomize") { model.randomizePlaylist() }
                Button("Reverse")   { model.reversePlaylist() }
            }
            Spacer()
            Menu("List") {
                Button("Save Playlist…") { saveActivePlaylistAs() }
                Divider()
                Button("Remove Selected") { removeIndices(Array(selection).sorted()) }
                    .disabled(selection.isEmpty)
                Button("Remove All") { model.clearPlaylist(); selection.removeAll() }
            }
```

> Keep the hidden `⌘S`/`⌘I` shortcut buttons (phase 6) OR move those `keyboardShortcut`s onto the corresponding menu items (`Button("Save Playlist…"){...}.keyboardShortcut("s", modifiers: .command)`). Match the existing `.buttonStyle`/`vars.bodyFont` styling used elsewhere in the bar for visual consistency; a `Menu` label can carry the same font via `.font(vars.bodyFont)`.

- [ ] **Step 4: Status line.** Replace the mac count/duration header (PlaylistView "Track count header" ~line 445, and the `totalDuration` helper ~552) with the shared format. Add a computed property and render it:

```swift
    private var statusLine: String {
        let count = model.playlistItems.count
        let total = model.playlistItems.reduce(0) { $0 + max(Int($1.duration), 0) }
        let sel = selection.isEmpty ? nil :
            model.playlistItems.filter { selection.contains($0.id) }
                 .reduce(0) { $0 + max(Int($1.duration), 0) }
        return Self.formatStatus(count: count, totalSecs: total, selectedSecs: sel)
    }

    /// Mirrors core `playlist_status_line` EXACTLY (keep in sync).
    static func formatStatus(count: Int, totalSecs: Int, selectedSecs: Int?) -> String {
        func hms(_ s: Int) -> String {
            let h = s / 3600, m = (s % 3600) / 60, sec = s % 60
            return h > 0 ? String(format: "%d:%02d:%02d", h, m, sec)
                         : String(format: "%d:%02d", m, sec)
        }
        let noun = count == 1 ? "track" : "tracks"
        var line = "\(count) \(noun) · \(hms(totalSecs)) total"
        if let sel = selectedSecs { line += " · \(hms(sel)) selected" }
        return line
    }
```

Render `Text(statusLine)` where the old count header sat.

- [ ] **Step 5: Checklist.** Append a dated phase-7 section to `docs/mac-pass-checklist.md`: each menu (Add/Select/Sort/List) opens and its items work; Sort/Randomize/Reverse reorder with the playing track staying current and queue badges following; status line shows count·total and adds `· selected` when ≥1 row selected; `⌘S`/`⌘I` still work.

- [ ] **Step 6: Build (Rust) + commit.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings (Rust side; Swift is blind).

```bash
git add frontends/SparkampMac/ docs/mac-pass-checklist.md
git commit -m "feat(mac): Winamp playlist menu bar + status line (blind, phase 7)"
```

---

## Task 11: Close-out — spec limitations + final gate

**Files:**
- Modify: `docs/superpowers/specs/2026-07-17-winamp-parity-roadmap-design.md`
- Test: full suite gate

- [ ] **Step 1: Record residuals.** Add a "Known limitations (phase 7 — F1 playlist ops)" section: e.g. TUI selected-duration omitted if the TUI has no multi-select (passes `None`); randomize uses the existing RNG (no seeding control); menu-bar consolidation replaced the flat buttons on GTK/mac (user decision 2026-07-27).

- [ ] **Step 2: Full gate.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test 2>&1 | grep -E "warning:|error|test result:"'`
Expected: two `test result:` lines, 0 failed, no `warning:`/`error`.

- [ ] **Step 3: Commit.**

```bash
git add docs/
git commit -m "docs: phase-7 playlist-ops known-limitations close-out"
```

---

## Manual test plan (user's interactive GTK pass + mac checklist)

1. Each sort (Title/Artist/Album/Filename/Path) on a messy playlist (missing titles, mixed case) — order sane; playing track keeps playing and its highlight follows.
2. Randomize twice → different orders; Reverse → exact flip; Reverse twice → original order.
3. With queue badges present (phase 5): reorder → badges follow their tracks.
4. Status: add/remove updates count+total live; select rows → `· selected` appears and tracks the selection; deselect all → hides.
5. Menu bar: Add▸/Select▸/Sort▸/List▸ each open; all items work; Save/Remove/Remove All behave as before; `⌘S`/`⌘I` (mac) and `Ctrl+S`/`Ctrl+I` (GTK) still work.
6. Shuffle behaves freshly after a reorder (no stale history jumps).
7. TUI: `o` opens the ops popup; sort/randomize/reverse work; status footer shows count·total.

## Self-review notes

- **Spec coverage:** sort (5 keys) + randomize + reverse (Tasks 1–2), status formatter (Task 3), GTK ops+menu+status (Tasks 4–6), TUI popup+status (Tasks 7–8), FFI + mac menu+status (Tasks 9–10), close-out (Task 11). Open questions resolved: full Winamp menu bar; selected shows at ≥1; TUI popup menu; sort keys include Artist/Album.
- **Queue safety:** ids stable across reorder → queue untouched; asserted in Task 2 Step 3.
- **Type consistency:** `SortKey` (Title/Artist/Album/Filename/Path) identical in Tasks 1, 5, 9; `playlist_status_line(count, total_secs, selected_secs)` identical in Tasks 3, 6, 8, and mirrored in Task 10's `formatStatus`.
- **Anchors re-verify at execution:** `player.rs` line numbers shifted across phase 6; re-grep each named symbol before editing. Confirm `ctx` field names for the FFI, `AppState` alias in `player.rs`, and gtk4 `TreeSelectionExt` method names (`select_all`/`unselect_all`).
```
