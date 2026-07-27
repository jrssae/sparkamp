# Phase 6 — F9 Shortcuts + Dialog Sweep Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bind the approved phase-6 shortcuts (`m`, `↑/↓`, `Enter`, `n`/`Shift+N`, `t` stop-after-current, `Ctrl+S`, `Ctrl+.`, `Ctrl+I`) across GTK + TUI + mac, and make the shortcuts dialog the single source of truth for every binding.

**Architecture:** The one stateful new feature — stop-after-current — lives as a transient `bool` on the shared `engine::Player`, so both the GTK advance loop (`state.rs`) and the shared `Controller` (`controller.rs`, used by TUI + mac) read the same flag at their end-of-stream (EOS) seams. Every other binding routes through an existing button/handler. Visual for the armed flag: GTK/mac overlay a small stop-square on the play button's bottom-right corner; TUI shows a combined `▶⏹` header glyph.

**Tech Stack:** Rust core (`src/`), GTK4 (`frontends/gtk/`), Ratatui/crossterm TUI (`frontends/tui/`), macOS SwiftUI (`frontends/SparkampMac/`) via C-FFI.

## Global Constraints

- Build/test ONLY inside distrobox: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`. Host builds fail (no gstreamer/gtk dev libs). Never gate on `cargo build --lib` — GTK frontend code only compiles in the bin target.
- Zero warnings, zero failures before any "done" claim. Quote BOTH `test result:` lines (lib + bin). Grep warnings with `grep -E "warning:|error\[|error:"` (bare "warning" false-matches the `thiserror` crate).
- New `src/` modules need `mod x;` in BOTH `src/lib.rs` AND `src/main.rs`. (No new core modules in this phase — noted for safety.)
- macOS Swift is BLIND (no compiler here): read whole files before editing, mechanically simple changes, every new/changed C-visible FFI symbol hand-mirrored in `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`, verification items appended to `docs/mac-pass-checklist.md` in the same commit.
- GTK strings reaching a widget MUST pass through `gtk_safe()` (interior-NUL guard). No metadata surfaces are added here, but keep the rule.
- Keyboard shortcuts sync across THREE places every time a key changes: GTK dialog (`player.rs` `sections` array ~line 4485), mac help (`KeyboardShortcutsView.swift` `sections` array ~line 22), mac handler (`SparkampModel+Keys.swift`). GTK binds upper+lowercase variants; mac lowercase only.
- Config fields (none new here — the flag is NOT persisted) would use `#[serde(default)]` + `Default`.
- Comments: plain English, why not what. Commits: conventional prefix, body = why + a verification line, trailer `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- Key ledger free for this phase (per handoff): `m` (ML toggle), `t` (stop-after-current), `Shift+N`, `Ctrl+S`, `Ctrl+.`, `Ctrl+I`. Do not collide with claimed keys (z x c v b j i f g d u p r s e a n q w k - =).

---

## File Structure

| File | Responsibility | Change |
|------|----------------|--------|
| `src/engine.rs` | GStreamer `Player`; holds the transient `stop_after_current` flag + accessors | Modify (~line 101 struct, new methods) |
| `src/controller.rs` | Shared advance logic (TUI + mac EOS path) | Modify (`advance_to_next_playable` ~line 324) |
| `frontends/gtk/window/state.rs` | GTK-local `AppState`; owns its own advance loop | Modify (helper to read/clear flag; badge-state accessor) |
| `frontends/gtk/window/player.rs` | GTK player window: transport widgets, `handle_key`, key-controller wrappers, EOS tick, shortcuts dialog | Modify (play button overlay, EOS guard, key arms, wrappers, `sections`) |
| `frontends/tui/keys.rs` | TUI key dispatch | Modify (`t` binding) |
| `frontends/tui/ui/mod.rs` | TUI header render | Modify (combined `▶⏹` glyph) |
| `src/ffi/settings.rs` | FFI get/set surface | Modify (stop-after-current get/set) |
| `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` | C header mirror | Modify (2 new fn decls) |
| `frontends/SparkampMac/Sources/SparkampModel+Keys.swift` | mac key switch | Modify (`m`, `t`, `n`, `Shift+N`) |
| `frontends/SparkampMac/Sources/SparkampModel.swift` (+ `+Transport`) | mac model state, ⌘-combos | Modify (badge state, combos) |
| `frontends/SparkampMac/Sources/PlayerWindow.swift` | mac play button | Modify (stop-square badge) |
| `frontends/SparkampMac/Sources/KeyboardShortcutsView.swift` | mac help | Modify (`sections` sweep) |
| `docs/mac-pass-checklist.md` | Blind-mac verification ledger | Modify (phase-6 section) |
| `docs/superpowers/specs/2026-07-17-winamp-parity-roadmap-design.md` | Known-limitation register | Modify (close-out, if any residual) |

---

## Task 1: Core — `stop_after_current` flag on the engine Player

**Files:**
- Modify: `src/engine.rs:101` (struct), plus an `impl Player` block (accessors)
- Test: `src/engine.rs` (inline `#[cfg(test)]`) — pure accessor test, no gstreamer transport needed

**Interfaces:**
- Produces:
  - `Player::stop_after_current(&self) -> bool`
  - `Player::set_stop_after_current(&mut self, v: bool)`
  - `Player::take_stop_after_current(&mut self) -> bool` (returns the flag AND clears it — the "fire" primitive)

- [ ] **Step 1: Add the field.** In `src/engine.rs`, inside `pub struct Player {` (line 101), add after the `state: PlayerState,` field:

```rust
    /// Transient "stop after the current track ends" flag (phase 6, key `t`).
    /// Not persisted: it governs a single automatic EOS advance, then clears.
    /// Manual transport (next/prev/play/stop) also clears it — see the
    /// accessors below and the advance seams that consult it.
    stop_after_current: bool,
```

- [ ] **Step 2: Initialise it.** Find the struct literal that builds a `Player` (search `Player {` in the `new`/constructor). Add `stop_after_current: false,` to that literal.

Run to locate: `grep -n "state: PlayerState::Stopped\|Player {" src/engine.rs`
Add the field beside the other `false`/default initialisers.

- [ ] **Step 3: Add accessors.** Add to an existing `impl Player` block (e.g. next to `pub fn state` at line 653):

```rust
    /// Whether playback should stop when the current track reaches EOS.
    pub fn stop_after_current(&self) -> bool {
        self.stop_after_current
    }

    /// Arm/disarm the stop-after-current flag (key `t` toggles it).
    pub fn set_stop_after_current(&mut self, v: bool) {
        self.stop_after_current = v;
    }

    /// Read the flag and clear it in one step — the advance seam calls this
    /// so a single EOS consumes the arming and the next track auto-advances.
    pub fn take_stop_after_current(&mut self) -> bool {
        std::mem::replace(&mut self.stop_after_current, false)
    }
```

- [ ] **Step 4: Write the failing test.** Add near the other engine tests (search `#[cfg(test)]` in `src/engine.rs`; if `Player::new` needs gstreamer, gate with the same helper existing engine tests use — otherwise test the flag via a constructed Player as those tests do):

```rust
    #[test]
    fn stop_after_current_flag_arms_and_takes_once() {
        let mut p = Player::new().expect("player");
        assert!(!p.stop_after_current());
        p.set_stop_after_current(true);
        assert!(p.stop_after_current());
        assert!(p.take_stop_after_current()); // fires
        assert!(!p.stop_after_current());     // cleared by take
        assert!(!p.take_stop_after_current()); // already clear
    }
```

- [ ] **Step 5: Run test to verify it fails, then passes.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --lib stop_after_current_flag'`
Expected: PASS after steps 1–3 are in place.

- [ ] **Step 6: Commit.**

```bash
git add src/engine.rs
git commit -m "feat(core): stop-after-current flag on the engine Player (phase 6)"
```

---

## Task 2: Core — consult the flag at the shared EOS advance seam

**Files:**
- Modify: `src/controller.rs:324` (`advance_to_next_playable`)
- Test: `frontends/tui/tests/playback.rs` (existing advance-behaviour suite) or `src/controller.rs` inline tests — whichever already constructs a `Controller` and drives `advance_to_next_playable`

**Interfaces:**
- Consumes: `Player::take_stop_after_current` (Task 1)
- Produces: `advance_to_next_playable` returns `AdvanceResult::Stopped` (and stops the player) when the flag was armed, WITHOUT consuming the queue.

- [ ] **Step 1: Write the failing test.** Locate the existing advance test file (`grep -rn "advance_to_next_playable" frontends/tui/tests/playback.rs src/controller.rs`). Add a test mirroring the neighbours' construction idiom:

```rust
    #[test]
    fn stop_after_current_halts_eos_advance_before_queue() {
        let mut ctrl = /* build a Controller with a 3-track playlist, playing */;
        // Arm stop-after-current and also enqueue a track: stop must win.
        ctrl.enqueue_next(/* id of track 3 */);
        ctrl.player.set_stop_after_current(true);

        let result = ctrl.advance_to_next_playable();
        assert!(matches!(result, AdvanceResult::Stopped));
        assert!(!ctrl.player.stop_after_current(), "flag cleared after firing");
        // Queue untouched — the queued track is still pending for the next play.
        assert_eq!(ctrl.queue.len(), 1);
    }
```

> Match the exact constructor/enqueue helper names the surrounding tests use (they already exist for phase-5 queue tests). If `ctrl.player`/`ctrl.queue` are private, add the arming through whatever public seam the queue tests use and assert via a public getter.

- [ ] **Step 2: Run test to verify it fails.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test stop_after_current_halts'`
Expected: FAIL (advance still returns `Playing`).

- [ ] **Step 3: Add the guard.** In `src/controller.rs`, at the TOP of `advance_to_next_playable` (immediately after the `let repeat = …;` line ~327, BEFORE the `queue_next_index()` block at line 333), insert:

```rust
        // Stop-after-current (phase 6, key `t`) wins over queue/shuffle/linear
        // on automatic EOS advance only. `take_` clears the arming so the very
        // next EOS advances normally. Manual next/prev never reach this method.
        if self.player.take_stop_after_current() {
            let _ = self.player.stop();
            return AdvanceResult::Stopped;
        }
```

- [ ] **Step 4: Clear the flag on manual play.** So a stale arming can't linger: in `Controller`, find the manual play entry point that starts a user-chosen track (`play_current` / `play_current_no_record` — `grep -n "pub fn play_current" src/controller.rs`). At the top of the user-initiated one(s), add:

```rust
        self.player.set_stop_after_current(false);
```

> Only the MANUAL play seam. Do NOT clear inside the queue/shuffle auto-advance branches (those already ran the `take_` guard). If `play_current` is shared by both manual and auto paths, gate the clear behind the manual caller instead — verify call sites before editing.

- [ ] **Step 5: Run test to verify it passes + full suite.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: new test PASS; both `test result:` lines report 0 failed, 0 warnings.

- [ ] **Step 6: Commit.**

```bash
git add src/controller.rs frontends/tui/tests/playback.rs
git commit -m "feat(core): stop-after-current guards the shared EOS advance (phase 6)"
```

---

## Task 3: GTK — play-button stop-square badge widget

**Files:**
- Modify: `frontends/gtk/window/player.rs:687` (btn_play construction), `:733` (append), plus a new `set_stop_badge` closure and skin CSS in `src/skin.rs`
- Test: build-gated + manual (UI overlay)

**Interfaces:**
- Produces: an `Rc<dyn Fn(bool)>` named `set_play_stop_badge` that shows/hides the badge; consumed by Tasks 4 (`t` key) and the EOS guard (Task 4's GTK seam).

- [ ] **Step 1: Wrap the play button in an overlay.** At `frontends/gtk/window/player.rs:687`, replace:

```rust
    let btn_play = Button::from_icon_name("media-playback-start-symbolic");
```

with:

```rust
    let btn_play = Button::from_icon_name("media-playback-start-symbolic");
    // Stop-after-current (phase 6, key `t`): a small stop-square badged on the
    // play button's bottom-right corner while armed. An Overlay keeps the badge
    // pinned to the button without disturbing the transport row's layout.
    let play_overlay = gtk4::Overlay::new();
    play_overlay.set_child(Some(&btn_play));
    let stop_badge = Label::new(Some("⏹"));
    stop_badge.add_css_class("stop-after-badge");
    stop_badge.set_halign(Align::End);
    stop_badge.set_valign(Align::End);
    stop_badge.set_visible(false);
    play_overlay.add_overlay(&stop_badge);
```

- [ ] **Step 2: Add the button to the row via the overlay.** At `frontends/gtk/window/player.rs:733`, change:

```rust
    transport.append(&btn_play);
```

to:

```rust
    transport.append(&play_overlay);
```

> Verify `btn_play.add_css_class("transport")` at ~line 693 still runs on `btn_play` itself (it should — the overlay only wraps it). Every other `btn_play` reference (clicks, tick accent) keeps working since the button object is unchanged.

- [ ] **Step 3: Build the badge setter.** After the transport row is assembled (near where other `Rc` closures are declared, e.g. before `handle_key`), add:

```rust
    // Toggle the stop-after-current badge on the play button.
    let set_play_stop_badge: Rc<dyn Fn(bool)> = {
        let stop_badge = stop_badge.clone();
        Rc::new(move |armed: bool| stop_badge.set_visible(armed))
    };
```

- [ ] **Step 4: Add skin CSS.** In `src/skin.rs::render_gtk_css`, add a selector near the other transport rules:

```rust
    // Stop-after-current badge — small, high-contrast square pinned bottom-right
    // of the play button. Sized so it reads as a corner badge, not a full icon.
    css.push_str(".stop-after-badge { font-size: 9px; margin: 0 1px 1px 0; padding: 0 1px; border-radius: 2px; }\n");
```

> Match the existing `css.push_str(...)` idiom in that function (colours pull from the active skin's accent/fg vars if the surrounding rules use them — copy the neighbouring pattern for accent colour so the badge follows the skin).

- [ ] **Step 5: Build + manual.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: builds, 0 warnings. Badge invisible by default (wired live in Task 4).

- [ ] **Step 6: Commit.**

```bash
git add frontends/gtk/window/player.rs src/skin.rs
git commit -m "feat(gtk): stop-after-current badge widget on the play button (phase 6)"
```

---

## Task 4: GTK — `t` stop-after-current binding + EOS guard + badge wiring

**Files:**
- Modify: `frontends/gtk/window/player.rs` (`handle_key` `t` arm ~4291 region; EOS tick guard at :3281), `frontends/gtk/window/state.rs` (optional helper)
- Test: build-gated + manual

**Interfaces:**
- Consumes: `Player::set_stop_after_current`/`take_stop_after_current` (Task 1), `set_play_stop_badge` (Task 3), `status_label`.

- [ ] **Step 1: Add the `t` key arm.** In `handle_key`, add a new arm (place it near the `s` shuffle arm ~4291). First add the clone at the top of the `handle_key` builder block (near line 4046):

```rust
        let kbd_set_stop_badge = set_play_stop_badge.clone();
        let kbd_stop_status = status_label.clone();
```

Then the arm:

```rust
                // ── Stop after current track (t) — toggle the engine flag and
                // badge the play button. Fires once at the next EOS, then clears. ─
                gdk::Key::t | gdk::Key::T => {
                    let armed = {
                        let mut s = state.borrow_mut();
                        let now = !s.player.stop_after_current();
                        s.player.set_stop_after_current(now);
                        now
                    };
                    kbd_set_stop_badge(armed);
                    kbd_stop_status.set_text(if armed {
                        "Stopping after current track"
                    } else {
                        "Stop-after-current cancelled"
                    });
                    glib::Propagation::Stop
                }
```

- [ ] **Step 2: Guard the GTK EOS advance.** At `frontends/gtk/window/player.rs:3281`, the tick handles `if let Some(event) = bus_event {`. Insert at the very top of that block (before `let pre_advance_idx …` at 3284), so an armed EOS stops instead of advancing:

```rust
                // Stop-after-current (phase 6): consume the flag on a normal EOS
                // and halt instead of advancing. Errors still fall through to the
                // broken-skip advance below (a failed track isn't "the current
                // track finishing"). Manual next/prev never enter this block.
                if matches!(event, BusEvent::Eos)
                    && state.borrow_mut().player.take_stop_after_current()
                {
                    let _ = state.borrow_mut().player.stop();
                    set_play_stop_badge_tick(false);
                    seek_bar.set_value(0.0);
                    status_label.set_text("Stopped after track");
                    return glib::ControlFlow::Continue;
                }
```

> Add the clones the tick closure needs. The 33ms tick starts at `frontends/gtk/window/player.rs:3117`; add near its other clones:
> ```rust
>         let set_play_stop_badge_tick = set_play_stop_badge.clone();
> ```
> Confirm the tick's return type is `glib::ControlFlow` (it uses `ControlFlow::Continue` elsewhere — match the exact variant already returned in that closure). Confirm `seek_bar` and `status_label` are already cloned into this tick; if not, reuse the names it already has for the position bar and status label.

- [ ] **Step 3: Clear badge on manual stop/play.** So the badge never lies after `v` (stop) or `x`/click play. In the `v` stop arm (~4080) and `x` play arm (~4068), after the transport call add:

```rust
                    // Manual transport cancels a pending stop-after-current.
                    state.borrow_mut().player.set_stop_after_current(false);
                    kbd_set_stop_badge(false);
```

> Add `let kbd_set_stop_badge = set_play_stop_badge.clone();` once (already added in Step 1) — reuse it; a single clone is captured by the whole `handle_key` closure. Also clear in the `btn_play`/`btn_stop`/`btn_next` click handlers (search `btn_stop.connect_clicked`, `btn_play.connect_clicked`) with the same two lines, cloning `set_play_stop_badge` into each.

- [ ] **Step 4: Build.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings.

- [ ] **Step 5: Manual test (record in close-out).** Play a track, press `t` → badge appears + status. Let it end → playback stops, badge clears. Press `t` twice → toggles off. Press `t`, then `b` (manual next) → advances normally, badge cleared. Press `t`, then queue a track → at EOS stops before the queue; next play resumes the queue.

- [ ] **Step 6: Commit.**

```bash
git add frontends/gtk/window/player.rs
git commit -m "feat(gtk): t = stop after current track, badge + EOS guard (phase 6)"
```

---

## Task 5: GTK — `m` toggle Media Library

**Files:**
- Modify: `frontends/gtk/window/player.rs` (`handle_key`, using `btn_ml` at :557)
- Test: build-gated + manual

- [ ] **Step 1: Clone `btn_ml` into `handle_key`.** Near line 4046 add:

```rust
        let kbd_btn_ml = btn_ml.clone();
```

- [ ] **Step 2: Add the `m` arm.** Near the `p` playlist-toggle arm (~4259):

```rust
                // ── Media Library window toggle (m) — routed through the ML
                // button so the open/focus logic stays in one place ──────────
                gdk::Key::m | gdk::Key::M => {
                    kbd_btn_ml.emit_clicked();
                    glib::Propagation::Stop
                }
```

> Verify `btn_ml`'s click handler toggles (opens/focuses) the ML window. If it only opens, that matches the button's own behaviour — `m` should do exactly what the button does (parity). `grep -n "btn_ml.connect_clicked" frontends/gtk/window/player.rs` to confirm the handler exists.

- [ ] **Step 3: Build + manual.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings; `m` opens/focuses the Media Library.

- [ ] **Step 4: Commit.**

```bash
git add frontends/gtk/window/player.rs
git commit -m "feat(gtk): m toggles the Media Library window (phase 6)"
```

---

## Task 6: GTK — `↑/↓` volume on the main window only

**Files:**
- Modify: `frontends/gtk/window/player.rs` (main-window key controller wrapper at :4426–4439)
- Test: build-gated + manual

**Rationale:** `handle_key` is shared with the playlist window, whose TreeView needs native `↑/↓` row browse. Handling `↑/↓` in the MAIN-window controller wrapper (and NOT in `handle_key`) keeps the playlist's native browse intact — the playlist wrapper delegates unknown keys to `handle_key`, which has no `Up`/`Down` arm, so they fall through to `_ => Proceed` and GTK browses.

- [ ] **Step 1: Extract shared volume steps (small altitude fix).** The `-`/`=` arms (4124–4148) duplicate the clamp+set logic. Add a helper closure near the other `Rc` closures (before `handle_key`):

```rust
    // Shared volume step used by the -/= keys and the main-window ↑/↓ keys.
    let step_volume: Rc<dyn Fn(f64)> = {
        let state = state.clone();
        let vol_bar = vol_bar.clone();
        Rc::new(move |delta: f64| {
            let new_vol = {
                let s = state.borrow();
                (s.config.playback.volume + delta).clamp(0.0, 1.0)
            };
            {
                let mut s = state.borrow_mut();
                s.config.playback.volume = new_vol;
                s.player.set_volume(new_vol);
            }
            vol_bar.set_value(new_vol);
        })
    };
```

Then replace the bodies of the `minus` arm (4124) with `{ kbd_step_volume(-0.05); glib::Propagation::Stop }` and the `equal | plus` arm (4137) with `{ kbd_step_volume(0.05); glib::Propagation::Stop }`, adding `let kbd_step_volume = step_volume.clone();` at the top of the `handle_key` builder.

- [ ] **Step 2: Add ↑/↓ to the MAIN-window wrapper.** At `frontends/gtk/window/player.rs:4430`, the main-window `connect_key_pressed` currently swallows Ctrl+Q then calls `handler(key)`. Add volume handling before `handler(key)`:

```rust
        let wrap_step_volume = step_volume.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, modifier| {
            if matches!(key, gdk::Key::q | gdk::Key::Q)
                && modifier.contains(gdk::ModifierType::CONTROL_MASK)
            {
                return glib::Propagation::Stop;
            }
            // Main-window ↑/↓ = volume. The playlist window's own controller
            // does NOT do this, so its TreeView keeps native row browse.
            match key {
                gdk::Key::Up => {
                    wrap_step_volume(0.05);
                    return glib::Propagation::Stop;
                }
                gdk::Key::Down => {
                    wrap_step_volume(-0.05);
                    return glib::Propagation::Stop;
                }
                _ => {}
            }
            handler(key)
        });
```

> Do NOT add `Up`/`Down` to `handle_key` or to the playlist-window wrapper (4455).

- [ ] **Step 3: Build + manual.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings. Main window ↑/↓ changes volume; playlist window ↑/↓ browses rows; jump window arrows unaffected.

- [ ] **Step 4: Commit.**

```bash
git add frontends/gtk/window/player.rs
git commit -m "feat(gtk): main-window up/down adjust volume; playlist keeps browse (phase 6)"
```

---

## Task 7: GTK — `n` multi-file + `Shift+N` add folder

**Files:**
- Modify: `frontends/gtk/window/player.rs` (`n` arm at :4195; new `N` arm)
- Test: build-gated + manual

**Decision:** `n` = add file(s) via multi-select; `Shift+N` (`gdk::Key::N`) = add a folder. This matches the phase-6 table and lets the dialog stop claiming a single key does both.

- [ ] **Step 1: Make `n` multi-select.** In the `n` arm (4195), change the dialog call from single to multiple. Replace `dialog.open(parent.as_ref(), …, move |result| { if let Ok(file) = result { if let Some(path) = file.path() { … } } })` with the multi-file variant:

```rust
                    dialog.open_multiple(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                        if let Ok(files) = result {
                            let before = state_cb.borrow().playlist.tracks.len();
                            let mut last_msg = String::new();
                            for i in 0..files.n_items() {
                                if let Some(obj) = files.item(i) {
                                    if let Ok(file) = obj.downcast::<gio::File>() {
                                        if let Some(path) = file.path() {
                                            if let Ok(msg) = state_cb.borrow_mut().add_path(&path) {
                                                last_msg = msg;
                                            }
                                        }
                                    }
                                }
                            }
                            if !last_msg.is_empty() {
                                status_cb.set_text(&last_msg);
                                pl_stat_cb.set_text(&last_msg);
                                rebuild_cb();
                                let paths = state_cb.borrow().uncached_paths_from(before);
                                if !paths.is_empty() {
                                    duration_probe::spawn_probes(paths, probe_tx_cb.clone(), broken_tx_cb.clone());
                                }
                            }
                        }
                    });
```

> `FileDialog::open_multiple` yields a `gio::ListModel` of `gio::File`. Confirm the downcast import path (`gio::File`, `glib::Cast`) — the file already uses `gio` and `open_multiple` is used elsewhere at `player.rs:2830`; copy that call's exact result-handling idiom to avoid guesswork.

- [ ] **Step 2: Add the `Shift+N` folder arm.** After the `n` arm's closing brace (~4256), add:

```rust
                // ── Add folder (Shift+N) — folder picker; recursively collects
                // audio files. GTK delivers Shift+n as the `N` keyval. ─────────
                gdk::Key::N => {
                    let dialog = gtk4::FileDialog::builder().title("Add Folder").build();
                    let state_cb = state.clone();
                    let rebuild_cb = rebuild_playlist.clone();
                    let status_cb = status_label.clone();
                    let pl_stat_cb = pl_status.clone();
                    let probe_tx_cb = kbd_probe_tx.clone();
                    let broken_tx_cb = kbd_broken_tx.clone();
                    let parent = window_weak.upgrade();
                    dialog.select_folder(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                        if let Ok(folder) = result {
                            if let Some(path) = folder.path() {
                                let before = state_cb.borrow().playlist.tracks.len();
                                match state_cb.borrow_mut().add_path(&path) {
                                    Ok(msg) => {
                                        status_cb.set_text(&msg);
                                        pl_stat_cb.set_text(&msg);
                                        rebuild_cb();
                                        let paths = state_cb.borrow().uncached_paths_from(before);
                                        if !paths.is_empty() {
                                            duration_probe::spawn_probes(paths, probe_tx_cb.clone(), broken_tx_cb.clone());
                                        }
                                    }
                                    Err(msg) => status_cb.set_text(&msg),
                                }
                            }
                        }
                    });
                    glib::Propagation::Stop
                }
```

> Verify `AppState::add_path` handles a directory (it already collects audio files for the TUI's typed-folder path and the existing "Add Folder" button — `grep -n "fn add_path" frontends/gtk/window/state.rs`). If `add_path` is file-only, route the folder through the same fast+background scan helper the "Add Folder" button uses instead (search the button's handler).

- [ ] **Step 3: Build + manual.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings. `n` multi-selects files; `Shift+N` picks a folder and adds its audio.

- [ ] **Step 4: Commit.**

```bash
git add frontends/gtk/window/player.rs
git commit -m "feat(gtk): n adds multiple files, Shift+N adds a folder (phase 6)"
```

---

## Task 8: GTK — `Ctrl+S` save playlist, `Ctrl+.` settings, `Ctrl+I` invert selection

**Files:**
- Modify: `frontends/gtk/window/player.rs` (both key-controller wrappers :4430 and :4455)
- Test: build-gated + manual

**Rationale:** These need modifier detection, so they live in the `connect_key_pressed` wrappers (like Ctrl+Q), not modifier-blind `handle_key`. `Ctrl+S` and `Ctrl+I` act on the playlist, so they belong in the PLAYLIST-window wrapper (4455); `Ctrl+.` (settings) is global, so main-window wrapper (4430). Add `Ctrl+S` to the main window too if a Save is reachable there — verify.

- [ ] **Step 1: Locate the reusable handlers.** Find:
  - Save-playlist entry point: `grep -n "save_playlist_dialog\|Save Playlist As\|run_save_playlist" frontends/gtk/window/*.rs` (util.rs:574 runs the Save dialog). Identify the closure/button the UI already uses to save the active playlist.
  - Settings opener: `open_settings_window(...)` (settings.rs:1) — see its call at player.rs:906 for the exact argument list to reuse; wrap it in an `Rc` closure `open_settings` near `handle_key` if not already one.
  - Invert selection: `pl_view.selection()` is a `TreeSelection` (Multiple mode, player.rs:936).

- [ ] **Step 2: Build an `invert_selection` closure.** Near the other `pl_view` closures, add:

```rust
    // Invert the playlist's multi-selection (Ctrl+I). TreeSelection has no
    // "invert", so walk every row and flip its selected state.
    let invert_selection: Rc<dyn Fn()> = {
        let pl_view = pl_view.clone();
        Rc::new(move || {
            let sel = pl_view.selection();
            let model = match pl_view.model() {
                Some(m) => m,
                None => return,
            };
            let n = gtk4::prelude::TreeModelExt::iter_n_children(&model, None);
            for i in 0..n {
                let path = gtk4::TreePath::from_indices(&[i]);
                if sel.path_is_selected(&path) {
                    sel.unselect_path(&path);
                } else {
                    sel.select_path(&path);
                }
            }
        })
    };
```

> Confirm the `TreeModelExt`/`TreeSelectionExt` method names against the gtk4-rs version in `Cargo.toml` (`path_is_selected`, `select_path`, `unselect_path`, `iter_n_children` are the gtk4-rs names). The file already imports `gtk4::prelude::*` in most scopes — reuse existing prelude imports.

- [ ] **Step 3: Wrap a `save_playlist` and `open_settings` closure** the same way, each cloning what its underlying function/button needs (mirror the argument list from the existing call sites at util.rs:574 / player.rs:906). If a Save button object exists, prefer `save_btn.emit_clicked()` for parity (as `u`/`m` do with their buttons).

- [ ] **Step 4: Wire the combos into the PLAYLIST wrapper (4455).** Extend its `connect_key_pressed` (which already handles Ctrl+Q and Esc):

```rust
            if modifier.contains(gdk::ModifierType::CONTROL_MASK) {
                match key {
                    gdk::Key::s | gdk::Key::S => { wrap_save_playlist(); return glib::Propagation::Stop; }
                    gdk::Key::i | gdk::Key::I => { wrap_invert_selection(); return glib::Propagation::Stop; }
                    _ => {}
                }
            }
```

Add `let wrap_save_playlist = save_playlist.clone();` and `let wrap_invert_selection = invert_selection.clone();` above the closure.

- [ ] **Step 5: Wire `Ctrl+.` into the MAIN wrapper (4430)** and also `Ctrl+S` there if the main window can save:

```rust
            if modifier.contains(gdk::ModifierType::CONTROL_MASK) {
                match key {
                    gdk::Key::period => { wrap_open_settings(); return glib::Propagation::Stop; }
                    gdk::Key::s | gdk::Key::S => { wrap_save_playlist_main(); return glib::Propagation::Stop; }
                    _ => {}
                }
            }
```

with the matching `.clone()` bindings above the closure. (`gdk::Key::period` is `.`; Ctrl+. → this.)

- [ ] **Step 6: Build + manual.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings. Ctrl+S opens Save dialog; Ctrl+. opens Settings; Ctrl+I inverts a partial playlist selection.

- [ ] **Step 7: Commit.**

```bash
git add frontends/gtk/window/player.rs
git commit -m "feat(gtk): Ctrl+S save, Ctrl+. settings, Ctrl+I invert selection (phase 6)"
```

---

## Task 9: GTK — shortcuts dialog source-of-truth sweep

**Files:**
- Modify: `frontends/gtk/window/player.rs:4485` (`sections` array)
- Test: a new unit test asserting the array covers a canonical key list + build + manual read-through

**Interfaces:**
- Produces: `pub(crate) fn shortcut_dialog_keys() -> Vec<&'static str>` (or a `const`) exposing the dialog's keys for the drift test.

- [ ] **Step 1: Reconcile the `sections` array.** Update entries to match reality after Tasks 4–8. Concretely:
  - Playlist section: split the misleading `("n", "Add file(s) or folder(s)")` into `("n", "Add file(s)")` and `("Shift+N", "Add folder")`. Keep `("↑ ↓", "Browse (playlist) / Volume (main)")` — clarify the split. Keep `("Enter", "Play selected track")`. Add `("Ctrl+S", "Save playlist")`, `("Ctrl+I", "Invert selection")`.
  - Volume section: add `("↑ ↓", "Volume up / down (main window)")`.
  - Add a new/appropriate entry `("m", "Toggle Media Library window")` (Playlist or a "Windows" grouping), `("t", "Stop after current track")` (Playback section), `("Ctrl+.", "Open settings")` (near "Click logo → Open settings").

- [ ] **Step 2: Add a drift-guard test.** Expose the keys and assert a canonical set is present. Add below the `sections` definition a small extractor and, in a `#[cfg(test)]` module in `player.rs` (or a sibling test file that can see it), the test:

```rust
    #[test]
    fn shortcut_dialog_lists_every_phase6_key() {
        let keys = shortcut_dialog_keys();
        for k in ["m", "t", "Shift+N", "Ctrl+S", "Ctrl+.", "Ctrl+I", "n", "Enter"] {
            assert!(keys.iter().any(|e| *e == k), "dialog missing `{k}`");
        }
    }
```

> If `player.rs` UI code can't be unit-tested in isolation (it constructs widgets), instead lift the `sections` data into a free `const`/`fn` at module scope (no widget deps) that both the dialog builder and the test consume. This is the "single source of truth" the phase demands — the data becomes testable, the builder just renders it.

- [ ] **Step 3: Run test + build.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test shortcut_dialog'`
Expected: PASS, 0 warnings.

- [ ] **Step 4: Manual read-through.** Open the shortcuts window (`i`); every line must be literally true on GTK.

- [ ] **Step 5: Commit.**

```bash
git add frontends/gtk/window/player.rs
git commit -m "feat(gtk): shortcuts dialog is the single source of truth (phase 6)"
```

---

## Task 10: TUI — `t` stop-after-current + combined `▶⏹` header glyph

**Files:**
- Modify: `frontends/tui/keys.rs` (normal-mode `match code` ~269–307), `frontends/tui/ui/mod.rs:271` (state glyph)
- Test: pure glyph-selection test in `ui/mod.rs`; binding smoke in `frontends/tui/tests/`

**Interfaces:**
- Consumes: core `Player::set_stop_after_current`/`stop_after_current` (Task 1). TUI EOS already routes through `Controller::advance_to_next_playable` (`frontends/tui/mod.rs:759`), which Task 2 guarded — so no TUI advance change is needed, only the toggle key and the glyph.

- [ ] **Step 1: Add the `t` binding.** In the normal-mode dispatch (`frontends/tui/keys.rs` around the `match code` at :269, beside `KeyCode::Char('n')` at :300), add:

```rust
            // t — toggle stop-after-current (phase 6). Fires at the next EOS,
            // then clears; the combined ▶⏹ header glyph shows it is armed.
            KeyCode::Char('t') | KeyCode::Char('T') => {
                let now = !self.ctrl().player.stop_after_current();
                self.ctrl_mut().player.set_stop_after_current(now);
                self.set_status(if now {
                    "Stopping after current track"
                } else {
                    "Stop-after-current cancelled"
                });
            }
```

> Match the exact accessor the TUI uses to reach the player: it may be `self.player` directly (the summary shows `app.player.state()` in `ui/mod.rs`) or via `self.ctrl()`. Use whichever the neighbouring transport keys (`Char('b') => self.play_next()`) resolve to — `grep -n "self.player\|fn ctrl\|fn ctrl_mut" frontends/tui/keys.rs frontends/tui/mod.rs`. Also clear the flag on manual stop (`v`) if the TUI has a stop key, mirroring GTK.

- [ ] **Step 2: Write the failing glyph test.** In `frontends/tui/ui/mod.rs`, extract the state-glyph choice into a pure helper so it is testable, then test it:

```rust
    #[test]
    fn header_glyph_combines_play_and_stop_when_armed() {
        assert_eq!(state_glyph(PlayerState::Playing, false), "▶");
        assert_eq!(state_glyph(PlayerState::Playing, true), "▶⏹");
        assert_eq!(state_glyph(PlayerState::Paused, true), "⏸"); // armed only matters while playing
        assert_eq!(state_glyph(PlayerState::Stopped, false), "⏹");
    }
```

- [ ] **Step 3: Implement `state_glyph` and use it.** Replace the inline match at `frontends/tui/ui/mod.rs:271`:

```rust
    let armed = app.player.stop_after_current();
    let (state_icon, state_color) = match app.player.state() {
        PlayerState::Playing => (state_glyph(PlayerState::Playing, armed), C_PLAYING),
        PlayerState::Paused  => (state_glyph(PlayerState::Paused, armed),  C_WARN),
        PlayerState::Stopped => (state_glyph(PlayerState::Stopped, armed), C_DIM),
    };
```

and add the free fn near the top of the file:

```rust
/// Header state glyph. While *playing* and stop-after-current is armed, show a
/// combined ▶⏹ so the armed flag is glanceable (parity with the GTK/mac play-
/// button badge). Paused/stopped ignore the flag — it only fires from playback.
pub(crate) fn state_glyph(state: PlayerState, stop_after_current: bool) -> &'static str {
    match (state, stop_after_current) {
        (PlayerState::Playing, true)  => "▶⏹",
        (PlayerState::Playing, false) => "▶",
        (PlayerState::Paused, _)      => "⏸",
        (PlayerState::Stopped, _)     => "⏹",
    }
}
```

> `state_icon` is later used in `format!("{} ", state_icon)` (line 316) — a `&'static str` works there unchanged.

- [ ] **Step 4: Run test + build.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test header_glyph'`
Expected: PASS, 0 warnings.

- [ ] **Step 5: Manual TUI walk.** `t` while playing → `▶⏹` in header + status; let track end → stops, glyph back to `⏹`; `t` twice toggles off.

- [ ] **Step 6: Commit.**

```bash
git add frontends/tui/keys.rs frontends/tui/ui/mod.rs
git commit -m "feat(tui): t = stop after current track, combined header glyph (phase 6)"
```

---

## Task 11: TUI — confirm `n` folder capability + help screen sweep

**Files:**
- Modify: `frontends/tui/ui/mod.rs` or wherever the TUI help/keys are listed (verify), plus a doc note
- Test: build-gated + manual

**Decision:** TUI `n` already accepts folder paths (typed path → `commit_add_file` → `path.is_dir()` branch collects audio, `frontends/tui/keys.rs:44`). No separate `Shift+N` folder-picker is added on TUI; the help copy states that `n` accepts files or folders.

- [ ] **Step 1: Find the TUI help/keys list.** `grep -rn "Jump\|Add file\|keybind\|help\|shortcut" frontends/tui/ui/*.rs frontends/tui/ui/overlays.rs`. If a help overlay lists keys, update: `n` = "Add file(s) or folder(s) (type a path)", add `t` = "Stop after current track". If the TUI has no key-list screen, skip the edit and only add the doc note (Step 2).

- [ ] **Step 2: Record the capability decision.** Append to the phase-6 mac/limitations notes (or the spec known-limitations) one line: "TUI add-file (`n`) accepts both file and folder paths via typed input; no separate `Shift+N` binding — parity met through the existing path parser."

- [ ] **Step 3: Build + manual.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: both `test result:` lines 0 failed, 0 warnings.

- [ ] **Step 4: Commit.**

```bash
git add frontends/tui/ docs/
git commit -m "docs(tui): n accepts folders; phase-6 help copy + capability note"
```

---

## Task 12: mac (blind) — stop-after-current FFI

**Files:**
- Modify: `src/ffi/settings.rs` (2 new FFI fns), `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` (2 decls)
- Test: `cargo build` (Rust side); mac verification deferred to checklist

**Interfaces:**
- Produces: `sparkamp_get_stop_after_current(ctx) -> bool`, `sparkamp_set_stop_after_current(ctx, bool)` — modelled on `sparkamp_get/set_gnudb_submit_test` (settings.rs:144/153).

- [ ] **Step 1: Add the Rust FFI.** In `src/ffi/settings.rs`, after the gnudb_submit_test pair (line 159), add:

```rust
/// Stop-after-current-track flag (phase 6, transient — not persisted). Lives on
/// the engine Player so the mac key `t` and any menu item share one source.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_stop_after_current(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.controller.player.stop_after_current()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_stop_after_current(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.controller.player.set_stop_after_current(value);
}
```

> Verify the path from `SparkampCtx` to the `Player`: this file's other fns use `ctx.config…`. Find how `ctx` reaches the controller/player — `grep -n "struct SparkampCtx\|controller\|player" src/ffi/mod.rs`. Use the exact field chain (it may be `ctx.controller.player`, `ctx.ctrl.player`, or a method). If the player is not directly reachable, add a thin `Controller` passthrough (`pub fn set_stop_after_current(&mut self, v: bool)` / getter) and call that.

- [ ] **Step 2: Mirror in the header.** In `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`, beside the other bool get/set decls (search `sparkamp_get_gnudb_submit_test`), add:

```c
bool sparkamp_get_stop_after_current(const SparkampCtx *ctx);
void sparkamp_set_stop_after_current(SparkampCtx *ctx, bool value);
```

> Match the exact `SparkampCtx` typedef spelling and `bool` include already used in the header.

- [ ] **Step 3: Build (Rust).**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings (the FFI compiles even though mac can't link here).

- [ ] **Step 4: Commit.**

```bash
git add src/ffi/settings.rs frontends/SparkampMac/SparkampCore/sparkamp_bridge.h
git commit -m "feat(ffi): stop-after-current get/set for the mac frontend (phase 6)"
```

---

## Task 13: mac (blind) — key bindings, play-button badge, ⌘ combos

**Files:**
- Modify: `SparkampModel+Keys.swift` (switch at :49; keyCode switch at :106), `SparkampModel.swift`/`+Transport.swift` (badge state + combos), `PlayerWindow.swift` (badge overlay), `docs/mac-pass-checklist.md`
- Test: read whole files before editing; verification via checklist

- [ ] **Step 1: Read the whole files.** Read `SparkampModel+Keys.swift`, `SparkampModel.swift`, `SparkampModel+Transport.swift`, `PlayerWindow.swift` fully before any edit (blind-mac rule).

- [ ] **Step 2: Add `m`, `t`, `n`, `Shift+N` to the key switch.** In `SparkampModel+Keys.swift`, in the `switch chars` block (after line 64 `q`), add:

```swift
        case "m": mediaLibraryVisible.toggle();   return true
        case "t":
            let armed = !sparkamp_get_stop_after_current(ctx)
            sparkamp_set_stop_after_current(ctx, armed)
            stopAfterCurrent = armed          // @Published — drives the badge
            return true
        case "n": addFiles();                     return true
        case "N": addFolder();                    return true
```

> Use the model's real property names: ML-window visibility, the FFI `ctx` handle, and add-file/add-folder methods almost certainly exist (mac already has an Add UI). `grep -n "mediaLibraryVisible\|MediaLibrary\|addFiles\|addFolder\|func add\|var ctx\|let ctx" frontends/SparkampMac/Sources/*.swift` and use the exact names. Note the `!hasModifiers` guard at line 39 means `N` here is Shift+n with no other modifier — confirm macOS delivers `chars == "N"` for Shift+n (it does).

- [ ] **Step 3: Add the `@Published var stopAfterCurrent`.** In `SparkampModel.swift`, beside other `@Published` transport state, add `@Published var stopAfterCurrent: Bool = false`. Clear it in the manual `stop()`/`play()`/`next()`/`prev()` transport methods (`+Transport.swift`) with `stopAfterCurrent = false; sparkamp_set_stop_after_current(ctx, false)` so the badge never lies.

- [ ] **Step 4: Badge the play button.** In `PlayerWindow.swift`, find the play/pause button. Overlay a small stop-square on its bottom-right when armed:

```swift
        .overlay(alignment: .bottomTrailing) {
            if model.stopAfterCurrent {
                Image(systemName: "stop.fill")
                    .font(.system(size: 8))
                    .padding(1)
            }
        }
```

> Attach to the play button view specifically. Use the project's existing colour/theme modifiers (match a neighbouring badge/overlay if one exists).

- [ ] **Step 5: ⌘ combos.** The raw handler ignores modifiers (`guard !hasModifiers` at line 39), so wire ⌘S / ⌘, / ⌘I as SwiftUI `.keyboardShortcut` on their buttons or a `CommandMenu` (whatever pattern the mac app already uses — `grep -n "keyboardShortcut\|CommandMenu\|Commands" frontends/SparkampMac/Sources/*.swift`):
  - ⌘S → save active playlist
  - ⌘, → open Settings (macOS convention; `.keyboardShortcut(",", modifiers: .command)`)
  - ⌘I → invert playlist selection (NSTableView selection manipulation, or SwiftUI selection set complement)

If any of these has no existing button to attach to, add the command and note it in the checklist for hardware verification.

- [ ] **Step 6: Append checklist items.** In `docs/mac-pass-checklist.md`, add a dated phase-6 section with checkboxes for: `m` toggles ML; `t` arms/clears + play-button badge appears/clears; `t` stops at EOS before the queue; `n` add files; `Shift+N` add folder; ⌘S save; ⌘, settings; ⌘I invert; arrows still volume; shortcuts window matches.

- [ ] **Step 7: Build (Rust only — mac is blind) + commit.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build'`
Expected: 0 warnings (no Rust change here beyond Task 12, but confirm nothing broke).

```bash
git add frontends/SparkampMac/ docs/mac-pass-checklist.md
git commit -m "feat(mac): m/t/n/Shift+N keys, stop-after badge, Cmd combos (blind, phase 6)"
```

---

## Task 14: mac (blind) — KeyboardShortcutsView sweep + final gate

**Files:**
- Modify: `frontends/SparkampMac/Sources/KeyboardShortcutsView.swift:22` (`sections`)
- Modify: `docs/superpowers/specs/2026-07-17-winamp-parity-roadmap-design.md` (known-limitations, if any residual)
- Test: full suite gate + read-through parity with the GTK dialog

- [ ] **Step 1: Mirror the GTK dialog into the mac `sections`.** Update `KeyboardShortcutsView.swift`'s `sections` array (line 22) so every entry matches the GTK dialog from Task 9 (mac shows lowercase keys; `Shift+N`, `⌘S`, `⌘,`, `⌘I` where GTK shows `Ctrl+`). Add `ShortcutEntry(key: "m", action: "Toggle Media Library")`, `ShortcutEntry(key: "t", action: "Stop after current track")`, `ShortcutEntry(key: "⇧N", action: "Add folder")`, `ShortcutEntry(key: "⌘S", action: "Save playlist")`, `ShortcutEntry(key: "⌘,", action: "Open settings")`, `ShortcutEntry(key: "⌘I", action: "Invert selection")`, and the `↑ ↓` volume line.

- [ ] **Step 2: Verify handler ↔ help agreement.** Cross-check every `sections` entry against the `SparkampModel+Keys.swift` switch (Task 13) — every listed key must have a handler; every handler key must be listed. Note any deliberate divergence in the checklist.

- [ ] **Step 3: Update the spec known-limitations if needed.** Add the TUI `Shift+N` capability note (Task 11) and any accepted residual (e.g. "mac ⌘I invert deferred to hardware verification") to the register in `2026-07-17-winamp-parity-roadmap-design.md`.

- [ ] **Step 4: Full gate.**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test 2>&1 | grep -E "warning:|error|test result:"'`
Expected: two `test result:` lines, 0 failed, no `warning:`/`error`.

- [ ] **Step 5: Commit.**

```bash
git add frontends/SparkampMac/Sources/KeyboardShortcutsView.swift docs/
git commit -m "docs(mac): shortcuts help sweep + phase-6 known-limitations close-out"
```

---

## Manual test plan (user's interactive GTK pass + mac checklist)

1. Every binding: `m` (ML toggle), main-window `↑/↓` (volume), playlist `↑/↓` (browse), playlist `Enter` (play row), `n` (multi-file), `Shift+N` (folder), `Ctrl+S`, `Ctrl+.`, `Ctrl+I`.
2. `t` mid-song → badge on play button; song finishes → playback stops, badge clears; `t` twice → toggles off.
3. `t` with queued tracks → stops before the queue; next play resumes the queue (phase-5 interplay).
4. Jump-window arrows unaffected by the main-window `↑/↓` volume change.
5. Shortcuts dialog (`i`) read-through: every line literally true; mac help identical content.
6. TUI: `t` shows `▶⏹`; `n` with a typed folder path adds its audio.

## Self-review notes

- **Spec coverage:** Every phase-6 table row (m, ↑/↓, Enter, n, Shift+N, t, Ctrl+S, Ctrl+., Ctrl+I) + dialog sweep maps to Tasks 1–14. Open questions resolved: `t` visual = play-button stop-square badge (GTK/mac) + `▶⏹` glyph (TUI); TUI Shift+N = capability already covered by `n` path parser.
- **Enter (GTK):** already native via `pl_view.connect_row_activated` (player.rs:2123) — no GTK task; mac has Return; verify in manual pass.
- **Type consistency:** `set_stop_after_current`/`stop_after_current`/`take_stop_after_current` used identically in Tasks 1, 2, 4, 10, 12. `state_glyph` signature consistent in Task 10. `set_play_stop_badge` consistent in Tasks 3–4.
- **Anchors re-verify at execution:** earlier phases moved lines; every `player.rs` line number is approximate — re-grep the named symbol before editing.
