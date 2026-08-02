# Phase 12 Lyrics — Revision Implementation Plan

> Executing inline (executing-plans). Steps are checkboxes. Revises the shipped
> F15 View/Search Lyrics feature per 7 user change requests (2026-08-01).

**Goal:** Turn the lyrics feature from a "show-or-browser-search" fork into a
first-class lyrics **window** that always opens, tracks either a fixed song or
the currently-playing song, carries its own Search + Edit + mode controls, and
frees the A1 ID3 panel from dumping full lyrics inline.

**Architecture:** One core module (`src/lyrics.rs`) owns every string decision
(marquee title, space-separated DDG search query, USLT body read, 200-char
panel truncation). FFI exposes it as one JSON blob. GTK/mac/TUI render.

**Tech Stack:** Rust core, GTK4 (frontends/gtk), Ratatui TUI (frontends/tui),
SwiftUI (frontends/SparkampMac). Build/test ONLY in distrobox `dev-box`.

## Global Constraints

- Build/test ONLY: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`. Zero warnings/failures before "done". Run cargo FOREGROUND. Known-flaky (parallel races, NOT regressions): `disc::detect::exclusive_read_tests::refcount_nesting_and_underflow`, `disc::burn::tests::run_tool_watchdog_kills_a_wedged_child` — reconfirm green under `--test-threads=1`.
- New top-level modules need `mod x;` in BOTH lib.rs AND main.rs. FFI compiles only in lib; GTK/TUI only in bin — never gate on `--lib` alone.
- macOS is BLIND (no Swift compiler): read whole files, mirror FFI byte-for-byte in `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`, append verify items to `docs/mac-pass-checklist.md` in the SAME commit.
- RefCell borrows NEVER held across UI calls/callbacks. now-playing subscribers are fired WITHOUT a live AppState borrow (play-start seam extracts the Vec, drops the borrow, then loops) — a subscriber MAY borrow state.
- Commits end `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. No push without a fresh explicit ask. Do NOT rebase/merge main.
- USER DECISIONS 2026-08-01: all three frontends; A1 = "Lyrics" button on the LAST Tags/ID3 carousel page + the on-panel Lyric row truncated to 200 chars (full text only in the window).

## Behavior spec (the 7 requests)

1. A1 (GTK expanded panel / mac now-playing): "Lyrics" button lives on the LAST Tags page, not persistently below the whole carousel.
2. No USLT → the window STILL opens, showing "No lyrics available" (never silently browser-searches).
3. Window title = marquee identifier: `<artist> - <track>`, artist→album_artist, then track→filename-stem.
4. Bottom-of-window Specific-song vs Current-song radio. Default: opened from active playlist / any ML view = **Specific** (static); opened from the media-player now-playing affordance = **Current** (title + body follow the playing track).
5. Transport keys `z x c v b j r s` still work while the lyrics window is focused.
6. A "Search" button at the bottom opens DuckDuckGo for `"<artist> <track> lyrics"` (SPACE-separated, NOT dash): artist→album_artist; if BOTH artist and track are missing → `"<filename> lyrics"`.
7. The A1 ID3-panel Lyric row is truncated to 200 chars (the reason the window exists).

---

### Task 1: Core — title, space-search, body, truncation (`src/lyrics.rs` + `src/now_playing.rs`)

**Files:** Modify `src/lyrics.rs`, `src/now_playing.rs`. Tests inline in both.

**Interfaces produced (consumed by FFI + all frontends):**
- `pub struct LyricsView { pub title: String, pub body: Option<String>, pub search_url: String }`
- `pub fn lyrics_view(path: &Path, artist: &str, title: &str, album_artist: &str) -> LyricsView`
- `pub fn lyrics_display_title(artist: &str, title: &str, album_artist: &str, path: &Path) -> String`
- `pub fn lyrics_search_url(artist: &str, title: &str, album_artist: &str, path: &Path) -> String` (SIGNATURE CHANGED — now takes `path`, space-separated, filename fallback)
- `pub const PANEL_LYRIC_MAX_CHARS: usize = 200;` + `pub fn truncate_panel_lyric(s: &str) -> String` (in now_playing.rs; ≤200 chars unchanged, else first 200 chars + '…')

**Remove:** `LyricsAction` enum + `lyrics_action()` (replaced by `lyrics_view`). Update all Rust callers in later tasks.

**Steps:**
- [ ] Write failing tests in `src/lyrics.rs`: `title_is_artist_dash_track`, `title_falls_back_to_album_artist`, `title_falls_back_to_filename_when_both_blank`, `search_is_space_separated_with_lyrics_suffix`, `search_uses_album_artist_when_no_artist`, `search_uses_filename_when_artist_and_title_blank`, `view_body_none_when_no_uslt`, `view_body_some_when_uslt_present`.
- [ ] Implement `eff_artist`, `lyrics_display_title`, `lyrics_search_url`, `lyrics_view`. Reuse `now_playing::percent_encode_query`. Space-join core terms, then append `" lyrics"`, then encode the whole string.
- [ ] In `src/now_playing.rs` add `PANEL_LYRIC_MAX_CHARS` + `truncate_panel_lyric` with tests `truncate_leaves_short_unchanged`, `truncate_caps_long_at_200_plus_ellipsis` (assert `.chars().count() == 201` and `ends_with('…')`).
- [ ] In `build_now_playing_info`, after building `tags`, truncate the `"Lyric"` entry's value via `truncate_panel_lyric`. Add test `panel_lyric_row_is_truncated` (write a >200-char USLT, assert the tags' Lyric value char-count ≤ 201 and ends with '…').
- [ ] Gate (`cargo build && cargo test`), commit.

### Task 2: FFI — one JSON entry point (`src/ffi/lyrics.rs`)

**Files:** Modify `src/ffi/lyrics.rs`, `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`, `docs/mac-pass-checklist.md`.

**Interface produced:** `char *sparkamp_lyrics_view(const char *path, const char *artist, const char *title, const char *album_artist);` → heap JSON `{"title":..,"body":..,"has_body":bool,"search_url":..}` (body "" when none); null `path` → null. Free with `sparkamp_free_string`. REMOVE `sparkamp_lyrics_action`.

**Steps:**
- [ ] Replace the FFI fn; build JSON via `serde_json::json!` (dep already used by disc CD-TEXT FFI — verify with `grep serde_json Cargo.toml`).
- [ ] Tests: `view_json_has_body_true_with_uslt`, `view_json_has_body_false_and_search_url`, `null_path_returns_null`. Parse the returned JSON with serde_json in-test.
- [ ] Mirror the new signature in `sparkamp_bridge.h` (replace the old F15 line). Update the `docs/mac-pass-checklist.md` Phase-12 FFI item to the new signature + JSON shape. Same commit.
- [ ] Gate, commit.

### Task 3: GTK core plumbing — AppState fields + `view_or_search_lyrics` rewrite (`frontends/gtk/window/{state,lyrics}.rs`)

**Files:** Modify `frontends/gtk/window/state.rs`, `frontends/gtk/window/lyrics.rs`.

**Interfaces produced:**
- AppState fields: `pub lyrics_mode: std::rc::Rc<std::cell::Cell<LyricsMode>>` (default Specific), `pub lyrics_refresh: Option<Rc<dyn Fn()>>`, `pub transport_key_handler: Option<Rc<dyn Fn(gdk::Key) -> glib::Propagation>>`. Setters `set_transport_key_handler`, `set_lyrics_refresh`.
- `enum LyricsMode { Specific, Current }` (in lyrics.rs, `pub(crate)`).
- `fn view_or_search_lyrics(state, path, artist, title, album_artist, rebuild_cb, mode: LyricsMode)` — ALWAYS opens the window now (no browser branch).

**Window layout (`show_lyrics_window`):**
- Title = `crate::lyrics::lyrics_display_title(...)` (`Lyrics — <id>`).
- Body TextView: `body` or "No lyrics available" (still `.lyrics-view`, read-only).
- Bottom control row (horizontal): `[Specific ○ Current]` radio (gtk4 `CheckButton` group) + `Search` button + `Edit in tag editor` button.
- Search button → `gio::AppInfo::launch_default_for_uri(view.search_url)`.
- Radio: Current selected → set `state.lyrics_mode = Current` + immediately refresh from `state.playlist.current()`; Specific → set Specific (stops live updates).
- Transport keys: the window's `EventControllerKey` — for `z x c v b j r s` (and their capitals) call `state.borrow().transport_key_handler` if set (clone the Rc out, drop the borrow, then call); Esc closes; else Proceed.
- Store an `lyrics_refresh` closure on state that re-reads `state.playlist.current()`, recomputes title + body, and updates the window widgets (guarded by a Weak to the window). Set it in `show_lyrics_window`.

**Steps:**
- [ ] Add the AppState fields + `LyricsMode` + setters. (`glib`/`gdk` already imported in state.rs? add if needed.)
- [ ] Rewrite `view_or_search_lyrics` + `show_lyrics_window` per layout above. Keep the take-then-close singleton discipline and `connect_close_request` clear (also clear `state.lyrics_refresh` on close).
- [ ] `build && test` (bin target) — GTK compiles; no dedicated unit test (UI), rely on existing suite staying green + manual pass. Commit.

### Task 4: GTK wiring — A1 button move + subscriber + call sites (`now_playing.rs`, `player.rs`, `media_library.rs`)

**Files:** Modify `frontends/gtk/window/now_playing.rs`, `frontends/gtk/window/player.rs`, `frontends/gtk/window/media_library.rs`.

**Steps:**
- [ ] `now_playing.rs`: delete the persistent `lyrics_link` (lines ~104-111). Change `populate` to accept `on_lyrics: &Rc<dyn Fn()>` and append a "Lyrics" `Button` (`np-link`, no frame) to the LAST tag-chunk column before it is wrapped in `page_scroller`. Thread `on_lyrics` from `build_panel` → the update closure → `populate`.
- [ ] `player.rs`: after `handle_key` is built, `state.borrow_mut().set_transport_key_handler(handle_key.clone())`. Register a second now-playing subscriber (alongside `np_panel_update`): `move |_info| { let (mode, refresh) = { let s = state.borrow(); (s.lyrics_mode.get(), s.lyrics_refresh.clone()) }; if mode == LyricsMode::Current { if let Some(r) = refresh { r(); } } }`. Change the A1 `on_lyrics` closure to call `view_or_search_lyrics(..., LyricsMode::Current)` (rebuild via `rebuild_playlist` once available, else no-op as today).
- [ ] `media_library.rs`: every `view_or_search_lyrics(...)` call site (dev-file, disc-files, ml, ed, ple) gains a trailing `LyricsMode::Specific`. The active-playlist `pl.lyrics` in `player.rs` → `LyricsMode::Specific`.
- [ ] Gate (bin), commit.

### Task 5: TUI adaptation (`frontends/tui/**`)

**Files:** Modify `frontends/tui/media_library/mod.rs`, `frontends/tui/keys.rs`, `frontends/tui/ui/overlays.rs`, `frontends/tui/mod.rs`.

**Scope note (surface reaches):** TUI has an overlay, not a window, and its keymap differs — so points 4/5 apply loosely. Deliver: (2) always open the overlay, showing "No lyrics available" when empty; (3) overlay header = marquee title from core; (6) a `d` key inside the overlay opens the DuckDuckGo search in the browser; the existing `y` still opens the overlay. Keep transport keys working by not blocking them where they already are global. Document TUI mode-toggle omission in the ledger.

**Steps:**
- [ ] `open_lyrics` now uses `crate::lyrics::lyrics_view`; Show branch always (title from `view.title`, body = `view.body` or `vec!["No lyrics available"]`); stash `view.search_url` on the `Mode::Lyrics` variant.
- [ ] Add `search_url: String` to `Mode::Lyrics`; add `d` key arm in the Lyrics dispatch → `xdg-open` the URL (status fallback).
- [ ] Update `draw_lyrics_overlay` header to the marquee title + add `d search` to its help line.
- [ ] Gate (bin), commit.

### Task 6: macOS parity (BLIND) (`frontends/SparkampMac/**` + pbxproj + checklist)

**Files:** Modify `SparkampModel.swift`, `SparkampModel+Lyrics.swift`, `LyricsWindow.swift`, plus the 5 surface files + `PlayerWindow.swift` for the Current-mode A1 affordance. Update `docs/mac-pass-checklist.md`.

**Steps:**
- [ ] `SparkampModel+Lyrics.swift`: decode the new JSON (`struct LyricsViewDTO: Decodable { title; body; has_body; search_url }`). `viewOrSearchLyrics(...)` ALWAYS sets `lyricsTitle`/`lyricsText`("No lyrics available" when `!has_body`)/`lyricsSearchURL`/`lyricsVisible`/`lyricsRequest`. Add `lyricsMode` (`enum LyricsMode { specific, current }`) + `lyricsSearchURL` @Published. Player affordance calls with `.current`; ML/playlist with `.specific`.
- [ ] `LyricsWindow.swift`: marquee `lyricsTitle`; body scroll; bottom row = Specific/Current `Picker`/segmented + `Search` button (opens `lyricsSearchURL`) + existing Edit button. Current mode → observe now-playing and refresh (mac already re-publishes now-playing to `PlayerWindow`; mirror that seam).
- [ ] Truncate the mac now-playing panel's lyric display to 200 chars (mirror core) — locate where mac shows the ID3 Lyric row.
- [ ] Update `sparkamp_bridge.h` already done in Task 2; here just consume. Update `docs/mac-pass-checklist.md` Phase-12 section (window always-opens, modes, search button, marquee title, 200-char truncation, transport-key note). No pbxproj file adds (files already in build phase) unless a new .swift is created — if so, hand-edit pbxproj.
- [ ] Commit Swift + checklist together.

### Task 7: Final gate + review

- [ ] Full `cargo build && cargo test` green, 0 warnings (reconfirm flaky under `-j1`).
- [ ] Inline review of the revision diff; update roadmap memory.
