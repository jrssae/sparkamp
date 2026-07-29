# Phase 12 — F15 View/Search Lyrics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> Read `docs/superpowers/plans/2026-07-19-opus-handoff.md` and
> `docs/superpowers/plans/2026-07-19-phase12-lyrics.md` (design) first.
> This is the FINAL roadmap phase.

**Goal:** A "View/Search Lyrics" action on the track-row context menus of five
surfaces plus a text link in the A1 (now-playing) panel: saved USLT opens a
read-only, skin-styled lyrics viewer; no lyrics falls back to the default
browser on a DuckDuckGo search for `<artist> - <title> lyrics`.

**Architecture:** One pure core decision function (`src/lyrics.rs`) is the sole
source of truth for both the Show/Search branch and the search-URL construction;
it reuses the existing display-fallback chain so the query never drifts from the
row label. One FFI entry point exposes the whole decision to macOS (fresh USLT
read happens in core, not Swift). Each frontend adds a viewer window/screen and
wires the same action onto its surfaces — no per-surface decision logic.

**Tech Stack:** Rust core, `id3` crate (USLT via existing `read_tag_fields`),
GTK4 (`gtk4::TextView`, `gio::AppInfo::launch_default_for_uri`), SwiftUI/AppKit
(`NSWorkspace.open`, `sparkamp_tag_*` FFI), Ratatui/crossterm.

## Global Constraints

- Build/test ONLY inside distrobox: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`. Host builds FAIL. Run cargo in the FOREGROUND. NEVER gate on `cargo build --lib` (GTK/TUI compile only in the bin target).
- ZERO warnings and ZERO test failures before any task is "done". Known flaky parallel races (`disc::detect::exclusive_read_tests::refcount_nesting_and_underflow`, `disc::burn::tests::run_tool_watchdog_kills_a_wedged_child`) confirm green under `-- --test-threads=1`; nothing else may fail.
- New top-level `src/` modules need `mod x;` in BOTH `src/lib.rs` AND `src/main.rs`.
- Never hold a `RefCell` borrow across a UI call, callback, `select_row`, or `.await` (double-borrow panic).
- macOS is BLIND (no Swift compiler here): read whole Swift files, use real property names, mirror every FFI declaration BYTE-FOR-BYTE in `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`, and append verify items to `docs/mac-pass-checklist.md` in the SAME commit.
- New config fields (none expected this phase) would use `#[serde(default)]` + `Default`.
- GTK frontend files are `include!`d into one flat module — private fns are cross-callable; item order does not matter.
- Deletion rule unaffected: this phase reads tags and opens windows/browsers only; it writes nothing.
- Comments explain WHY, not WHAT (CLAUDE.md). Commits end with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. Do NOT push, and do NOT rebase/merge `main` (pbxproj conflict must be resolved on a Mac).
- USER DECISIONS (2026-07-29): A1 affordance = **text link** (not a button). The viewer **includes an "Edit in tag editor" link** that opens the existing ID3 editor for the track.

## File Structure

- Create `src/lyrics.rs` — pure decision fn + URL builder + tests. `mod lyrics;` in `src/lib.rs` and `src/main.rs`.
- Modify `src/ffi/mod.rs` (or the FFI submodule dir) — add `src/ffi/lyrics.rs` with one C entry point; register it.
- Modify `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` — mirror the FFI.
- Create `frontends/gtk/window/lyrics.rs` — singleton viewer window (`include!`'d; add its `include!` line next to the other window includes).
- Modify `src/skin.rs` — add `.lyrics-view` CSS in `render_gtk_css` + a coverage test.
- Modify `frontends/gtk/window/media_library.rs`, `frontends/gtk/window/player.rs`, `frontends/gtk/window/now_playing.rs` — wire the action onto the five GTK surfaces + A1 link.
- Create `frontends/SparkampMac/Sources/LyricsWindow.swift` — mac viewer; modify the five mac surface files + `PlayerWindow.swift` (A1) + `SparkampModel*.swift` (FFI glue).
- Modify `frontends/tui/mod.rs` + `frontends/tui/media_library/mod.rs` + `frontends/tui/ui/` — TUI lyrics screen + key.
- Modify `docs/mac-pass-checklist.md` — phase-12 section.

---

### Task 1: Core decision function `src/lyrics.rs`

**Files:**
- Create: `src/lyrics.rs`
- Modify: `src/lib.rs` (add `pub mod lyrics;`), `src/main.rs` (add `mod lyrics;` — main.rs DOES re-declare modules as `mod x;`, alphabetical block near line 18), `src/now_playing.rs` (widen `percent_encode_query` to `pub(crate)`)
- Test: inline `#[cfg(test)]` in `src/lyrics.rs`

**Interfaces:**
- Consumes: `crate::id3_editor::read_tag_fields(&Path) -> TagFields` (field `.lyric: String`, USLT text). The display-fallback rules currently live in `src/model.rs` (title → filename stem; artist may be empty) and `src/play_stats.rs::effective_album_artist`.
- Produces:
  ```rust
  pub enum LyricsAction {
      Show(String),   // non-empty USLT text, multi-line preserved
      Search(String), // full DuckDuckGo URL
  }
  // Fresh-read decision: reads USLT from `path`, else builds a search URL.
  pub fn lyrics_action(path: &std::path::Path, artist: &str, title: &str, album_artist: &str) -> LyricsAction;
  // Pure URL builder, exposed for direct reuse + focused tests.
  pub fn lyrics_search_url(artist: &str, title: &str, album_artist: &str) -> String;
  // Effective (display) artist for the query: artist, else album_artist, else "".
  fn query_artist(artist: &str, album_artist: &str) -> String;
  ```

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn url_has_artist_dash_title_lyrics() {
        let u = lyrics_search_url("Miles Davis", "So What", "");
        assert_eq!(u, "https://duckduckgo.com/?q=Miles%20Davis%20-%20So%20What%20lyrics");
    }

    #[test]
    fn url_falls_back_to_album_artist_when_artist_blank() {
        let u = lyrics_search_url("", "So What", "Miles Davis");
        assert_eq!(u, "https://duckduckgo.com/?q=Miles%20Davis%20-%20So%20What%20lyrics");
    }

    #[test]
    fn url_omits_dash_when_no_artist() {
        // No artist and no album_artist → just "<title> lyrics", no leading "- ".
        let u = lyrics_search_url("", "So What", "");
        assert_eq!(u, "https://duckduckgo.com/?q=So%20What%20lyrics");
    }

    #[test]
    fn url_percent_encodes_ampersand_and_unicode() {
        let u = lyrics_search_url("AC/DC", "Café & Cream", "");
        // '/', '&', space, and non-ASCII must all be percent-encoded.
        assert_eq!(
            u,
            "https://duckduckgo.com/?q=AC%2FDC%20-%20Caf%C3%A9%20%26%20Cream%20lyrics"
        );
    }

    #[test]
    fn action_shows_uslt_when_present_multiline_preserved() {
        use std::io::Write;
        // SAME temp-mp3 idiom as src/id3_editor.rs tests: a NamedTempFile with a
        // minimal MPEG frame header, then write USLT via the existing writer.
        let mut f = tempfile::NamedTempFile::with_suffix(".mp3").unwrap();
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        let mut fields = crate::id3_editor::read_tag_fields(f.path());
        fields.lyric = "line one\nline two".to_string();
        crate::id3_editor::write_tag_fields(f.path(), &fields).unwrap();

        match lyrics_action(f.path(), "A", "T", "") {
            LyricsAction::Show(text) => assert_eq!(text, "line one\nline two"),
            other => panic!("expected Show, got {other:?}"),
        }
    }

    #[test]
    fn action_searches_when_no_uslt() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::with_suffix(".mp3").unwrap();
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        match lyrics_action(f.path(), "Miles Davis", "So What", "") {
            LyricsAction::Search(u) => {
                assert_eq!(u, "https://duckduckgo.com/?q=Miles%20Davis%20-%20So%20What%20lyrics");
            }
            other => panic!("expected Search, got {other:?}"),
        }
    }

    #[test]
    fn action_searches_when_path_unreadable() {
        // A missing file must degrade to Search, never panic.
        match lyrics_action(Path::new("/no/such/file.mp3"), "A", "T", "") {
            LyricsAction::Search(_) => {}
            other => panic!("expected Search, got {other:?}"),
        }
    }
}
```

Requires `#[derive(Debug)]` on `LyricsAction`. The temp-mp3 idiom above is copied verbatim from `src/id3_editor.rs` tests (`NamedTempFile::with_suffix(".mp3")` + header bytes `[0xFF,0xFB,0x90,0x00]` + `write_tag_fields`) — there is NO `tests/fixtures/` dir; do not use `include_bytes!`. `tempfile` is already a dev-dependency (used across `id3_editor.rs`).

- [ ] **Step 2: Run tests, verify they fail**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --lib lyrics'`
Expected: FAIL (module/functions not defined).

- [ ] **Step 3: Implement**

```rust
//! View/Search-lyrics decision (F15). One place decides Show-vs-Search and
//! builds the search URL so the query can never drift from the row label the
//! user right-clicked.

use std::path::Path;

#[derive(Debug, Clone)]
pub enum LyricsAction {
    /// Non-empty USLT text read fresh from the file (multi-line preserved).
    Show(String),
    /// DuckDuckGo search URL to hand to the default browser.
    Search(String),
}

/// Fresh-read a track's USLT. Non-empty → `Show`; empty/unreadable → `Search`.
/// The ML row may be stale, so this always re-reads from disk.
pub fn lyrics_action(path: &Path, artist: &str, title: &str, album_artist: &str) -> LyricsAction {
    let lyric = crate::id3_editor::read_tag_fields(path).lyric;
    if lyric.trim().is_empty() {
        LyricsAction::Search(lyrics_search_url(artist, title, album_artist))
    } else {
        LyricsAction::Show(lyric)
    }
}

/// `https://duckduckgo.com/?q=<enc>` where the query is `"{artist} - {title} lyrics"`,
/// or `"{title} lyrics"` when no artist is available. Mirrors the row display
/// fallback (artist → album_artist → none).
pub fn lyrics_search_url(artist: &str, title: &str, album_artist: &str) -> String {
    let a = query_artist(artist, album_artist);
    let query = if a.is_empty() {
        format!("{title} lyrics")
    } else {
        format!("{a} - {title} lyrics")
    };
    format!("https://duckduckgo.com/?q={}", encode_query(&query))
}

/// artist, else album_artist, else "" — same precedence as the display label.
fn query_artist(artist: &str, album_artist: &str) -> String {
    if !artist.trim().is_empty() {
        artist.to_string()
    } else if !album_artist.trim().is_empty() {
        album_artist.to_string()
    } else {
        String::new()
    }
}

```

REUSE the existing encoder — do NOT write a new one. `src/now_playing.rs:145`
already has `fn percent_encode_query(s: &str) -> String` (space → `%20`, its
tests confirm `AC%2FDC` and `Miles%20Davis`, exactly the outputs the Task-1
tests expect). Change its visibility to `pub(crate)` and call
`crate::now_playing::percent_encode_query(&query)` from `lyrics_search_url`.
This keeps both URL builders on one encoder — a Minor finding if duplicated.
(Widening `percent_encode_query` from private to `pub(crate)` is the only edit
to `now_playing.rs`; its own tests still pass unchanged.)

- [ ] **Step 4: Run tests, verify pass**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test --lib lyrics'`
Expected: PASS.

- [ ] **Step 5: Full gate + commit**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: 0 warnings; only the known flaky race may fail (re-run `-- --test-threads=1` to confirm).

```bash
git add src/lyrics.rs src/lib.rs src/main.rs src/now_playing.rs
git commit -m "feat(lyrics): core lyrics_action + DuckDuckGo search-URL builder"
```

---

### Task 2: FFI entry point for macOS

**Files:**
- Create: `src/ffi/lyrics.rs` (or add to the nearest existing FFI module — check how `src/ffi/now_playing.rs` is registered in `src/ffi/mod.rs`)
- Modify: `src/ffi/mod.rs` (register), `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` (mirror)
- Test: inline round-trip test in `src/ffi/lyrics.rs`

**Interfaces:**
- Consumes: `crate::lyrics::{lyrics_action, LyricsAction}` from Task 1.
- Produces (mirror the string-return idiom already used in `src/ffi/now_playing.rs` for the Wikipedia URLs and the phase-10 `last_search` getters — `CString::into_raw` + `sparkamp_free_string`):
  ```c
  // kind: 0 = show lyrics text, 1 = search URL. Caller frees *out with sparkamp_free_string.
  // Returns the malloc'd string; writes the discriminator into *out_kind.
  char *sparkamp_lyrics_action(const char *path, const char *artist,
                               const char *title, const char *album_artist,
                               uint32_t *out_kind);
  ```
  Rust side:
  ```rust
  #[no_mangle]
  pub extern "C" fn sparkamp_lyrics_action(
      path: *const c_char, artist: *const c_char, title: *const c_char,
      album_artist: *const c_char, out_kind: *mut u32,
  ) -> *mut c_char { /* ... */ }
  ```

- [ ] **Step 1: Write failing test** — a Rust test that calls `sparkamp_lyrics_action` with a temp file (no USLT) and asserts `out_kind == 1` and the returned C string equals the DDG URL, then frees it via the existing `sparkamp_free_string`. Add a second case with USLT written → `out_kind == 0` and the text round-trips. Use the SAME temp-file construction as Task 1.

- [ ] **Step 2: Run, verify fail.** `cargo test --lib ffi::lyrics`

- [ ] **Step 3: Implement.** Follow the exact null-check / `CStr::from_ptr` / `gtk_safe`-not-needed-here pattern from `src/ffi/now_playing.rs`. Null `path`/`title` → treat as empty string; never deref null. Map `LyricsAction::Show(t) → (0, t)`, `Search(u) → (1, u)`. Guard `out_kind.is_null()`.

- [ ] **Step 4: Run, verify pass.**

- [ ] **Step 5: Mirror in `sparkamp_bridge.h`** byte-for-byte (exact param order, `uint32_t *out_kind`, `char *` return). Add a `docs/mac-pass-checklist.md` phase-12 line: "sparkamp_lyrics_action signature in bridge.h matches src/ffi/lyrics.rs; kind 0=show/1=search; string freed with sparkamp_free_string."

- [ ] **Step 6: Full gate + commit** (Rust + bridge.h + checklist in ONE commit).

```bash
git add src/ffi/lyrics.rs src/ffi/mod.rs frontends/SparkampMac/SparkampCore/sparkamp_bridge.h docs/mac-pass-checklist.md
git commit -m "feat(lyrics): FFI sparkamp_lyrics_action for macOS parity"
```

---

### Task 3: GTK viewer window + `.lyrics-view` skin CSS

**Files:**
- Create: `frontends/gtk/window/lyrics.rs`
- Modify: the file that lists the window `include!`s (grep `include!.*window/now_playing.rs` to find it) — add `include!("window/lyrics.rs");` beside the others
- Modify: `frontends/gtk/window/state.rs` — add `lyrics_window: Option<gtk4::Window>` to `AppState` (mirror `id3_editor_window` at line 54 + its `None` init ~line 525)
- Modify: `src/skin.rs` — `.lyrics-view` block in `render_gtk_css` (~line 544) + a coverage test
- Test: `render_gtk_css` coverage test in `src/skin.rs`

**Interfaces:**
- Consumes: `crate::lyrics::{lyrics_action, LyricsAction}`; `crate::skin` vars; `open_id3_editor_window(...)` (frontends/gtk/window/id3.rs:687) for the Edit link; `gio::AppInfo::launch_default_for_uri` (pattern at dedupe.rs:417) for the browser.
- Produces:
  ```rust
  // Singleton entry point every GTK surface calls. Decides Show vs Search:
  // Show → open/replace the viewer with `title` + text; Search → launch browser.
  fn view_or_search_lyrics(
      state: &Rc<RefCell<AppState>>,
      path: &std::path::Path,
      artist: &str, title: &str, album_artist: &str,
  );
  ```

- [ ] **Step 1: CSS coverage test in `src/skin.rs`** (this is the only automated test for GTK-side work — the window itself is bin-target UI):

```rust
#[test]
fn render_gtk_css_covers_lyrics_view() {
    let css = render_gtk_css(&SkinVars::dark_defaults());
    assert!(css.contains(".lyrics-view"), "lyrics viewer must be skin-styled");
}
```

- [ ] **Step 2: Run, verify fail.** `cargo test --lib render_gtk_css_covers_lyrics_view`

- [ ] **Step 3: Add `.lyrics-view` CSS** in `render_gtk_css`, using the SAME `{ff}`/`{fs}` font-family and font-size vars the base text surfaces use (see the base `font-family: {ff}; font-size: {fs}px;` line ~573). Example, matching the existing string style:

```rust
// Lyrics viewer text uses the skin's body font/size so it reads like the app.
.lyrics-view, .lyrics-view text {{ \
    font-family: {ff}; font-size: {fs}px; color: {text}; \
    background: transparent; padding: 8px; \
}}
```

- [ ] **Step 4: Run, verify pass.**

- [ ] **Step 5: Implement `frontends/gtk/window/lyrics.rs`.** Requirements (no new automated test — verify by compile + manual plan):
  - `view_or_search_lyrics(...)` calls `crate::lyrics::lyrics_action(path, artist, title, album_artist)`.
  - `LyricsAction::Search(url)` → `let _ = gio::AppInfo::launch_default_for_uri(&url, None::<&gio::AppLaunchContext>);` (match dedupe.rs:417 exact signature).
  - `LyricsAction::Show(text)` → open-or-reuse a singleton window (mirror `open_id3_editor_window`'s take/replace pattern with `state.lyrics_window`): title `"Lyrics — {title}"`; a `gtk4::ScrolledWindow` containing a read-only `gtk4::TextView` (`set_editable(false)`, `set_cursor_visible(false)`, `set_wrap_mode(gtk4::WrapMode::WordChar)`), buffer text = `text`; `add_css_class("lyrics-view")` on the TextView. Reopening for another track REPLACES the buffer + title on the existing window (don't spawn a second).
  - Esc closes: add a `gtk4::EventControllerKey`; on `gdk::Key::Escape` call `win.close()`.
  - "Edit in tag editor" link (USER DECISION): a `gtk4::LinkButton`-style or plain `Button` with `.pl-btn` class at the bottom; on click call `open_id3_editor_window(&win.upcast_ref()/parent, state, path, …)` with the SAME argument shape the other 8 call sites use (copy one verbatim, e.g. media_library.rs:2717), then optionally `win.close()`. Grep an existing `open_id3_editor_window(` call to get the exact parameters — do NOT guess them.
  - On window close, clear `state.borrow_mut().lyrics_window = None` (mirror id3.rs:730).
  - No `RefCell` borrow may be held across `open_id3_editor_window`, `launch_default_for_uri`, or `win.present()`.

- [ ] **Step 6: Full gate + commit.**

```bash
git add src/skin.rs frontends/gtk/window/lyrics.rs frontends/gtk/window/state.rs frontends/gtk/window/mod.rs
git commit -m "feat(lyrics): GTK skin-styled read-only viewer window + Edit link"
```

---

### Task 4: Wire the five GTK surfaces + A1 text link

**Files:**
- Modify: `frontends/gtk/window/player.rs` (active-playlist row menu ~2928; the playlist menu is also reused for the jump surface), `frontends/gtk/window/media_library.rs` (Files view menu, playlist-editor menu, device-file menu ~2738, disc view rows), `frontends/gtk/window/now_playing.rs` (A1 panel link ~228)
- Test: none automated (bin-target UI) — covered by the manual plan

**Interfaces:**
- Consumes: `view_or_search_lyrics(state, path, artist, title, album_artist)` from Task 3.

For EACH of the five row-context menus, add one menu item **"View/Search Lyrics"** wired to a `gio::SimpleAction` whose callback resolves the selected row's `path`, `artist`, `title`, and `album_artist` (use each surface's existing row model — the same fields the "View/Edit ID3" action already reads on that surface) and calls `view_or_search_lyrics(...)`. Place the item next to the existing "View/Edit ID3" item.

- [ ] **Step 1: Files view menu** — media_library.rs. Find the Files-view context menu (the one whose `edit-id3` action opens `open_id3_editor_window` at media_library.rs:3531 or the Files block); add a sibling `lyrics` action + menu item.
- [ ] **Step 2: Playlist-editor menu** — media_library.rs (editor rows; `edit-id3` at ~4008/6246). Add the action + item.
- [ ] **Step 3: Device-file menu** — media_library.rs:2738 menu (`edit-id3` action at 2693). Device path may hit slow MTP: `lyrics_action`'s fresh read runs on the caller thread here as the other device row actions do; if that surface already reads tags on a worker for ID3, mirror it, else accept the same synchronous read the ID3 editor uses on this surface. On unreadable path the core fn already returns Search — no hang-forever.
- [ ] **Step 4: Disc view rows** — media_library.rs disc tracks. File-backed rows pass their real path; metadata-only disc rows (no local file) pass a non-existent path so the core fn falls to Search using the disc's artist/title. Verify the disc row model exposes artist/title (disc CD-TEXT/gnudb overlay) and pass those.
- [ ] **Step 5: Active-playlist row menu** — player.rs:2928. Add the action (`pl.lyrics`) + `menu.append_item(&gio::MenuItem::new(Some("View/Search Lyrics"), Some("pl.lyrics")))` beside the existing items; callback reads the selected `Track`'s path + display fields.
- [ ] **Step 6: A1 panel text link** — now_playing.rs. Near the tag column (beside the Wikipedia rows built at ~228), add a **text link** (`gtk4::LinkButton` styled as text, or a flat `Button` with a link CSS class — match how `wiki_row` at now_playing.rs:384 renders) labelled "Lyrics" that calls `view_or_search_lyrics(...)` for the currently-playing track. USER DECISION: text link, not a button.
- [ ] **Step 7: Full gate + commit.**

```bash
git commit -am "feat(lyrics): wire View/Search Lyrics onto 5 GTK surfaces + A1 link"
```

---

### Task 5: macOS viewer + FFI glue + five surfaces + A1 link

**Files:**
- Create: `frontends/SparkampMac/Sources/LyricsWindow.swift`
- Modify: `frontends/SparkampMac/Sources/SparkampModel.swift` (or `SparkampModelTypes.swift`) — Swift wrapper over `sparkamp_lyrics_action`; PlaylistView.swift, MLPlaylistEditor.swift, DeviceDetailView.swift, DiscDriveView.swift, MLFilesTable/Files view, PlayerWindow.swift (A1)
- Modify: `docs/mac-pass-checklist.md`
- Test: none compilable here (BLIND) — checklist items only

**Interfaces:**
- Consumes: `sparkamp_lyrics_action(path, artist, title, album_artist, &kind)` from Task 2; existing `Id3EditorWindow` open path for the Edit link; `NSWorkspace.shared.open(url)` for the browser.

- [ ] **Step 1: Swift FFI wrapper** — a `LyricsResult` enum (`.show(String)` / `.search(URL)`) and `model.lyricsAction(path:artist:title:albumArtist:) -> LyricsResult` that calls the FFI, reads `kind`, converts the C string with the existing `cBytesToString`/`String(cString:)` helper, and frees it with `sparkamp_free_string` (mirror the phase-10 `last_search` getter). Read the existing string-FFI wrapper before writing this and copy its free discipline exactly.
- [ ] **Step 2: `LyricsWindow.swift`** — a single reusable window (mirror `ArtworkWindow.swift`'s open-or-focus singleton on the model): title `"Lyrics — \(title)"`, a scrollable read-only `TextEditor`/`Text` in the theme body font (`theme.vars.bodyFont`), Esc/⌘W closes, reopening replaces content. Add an "Edit in tag editor" link/button that opens the existing `Id3EditorWindow` for the track (USER DECISION). `.search` → `NSWorkspace.shared.open(url)`.
- [ ] **Step 3–7: wire the five surfaces + A1.** On each `.contextMenu` (PlaylistView:556, MLPlaylistEditor:294, DeviceDetailView:495, DiscDriveView:841/946, Files view) add a `Button("View/Search Lyrics")` calling `model.viewOrSearchLyrics(track)`; in PlayerWindow.swift A1 section (beside the Wikipedia `Link` at :746) add a **text `Link`/`Button`** "Lyrics". `viewOrSearchLyrics` calls `lyricsAction`, then `.show` opens `LyricsWindow`, `.search` opens the browser.
- [ ] **Step 8: checklist** — append phase-12 verify items: all 5 surfaces + A1 open lyrics/search; viewer uses skin font; Esc/⌘W close; singleton; Edit-in-tag-editor opens the ID3 editor; browser fallback encodes `&`/unicode; pbxproj still opens in Xcode after adding `LyricsWindow.swift`.
- [ ] **Step 9: Read `project.pbxproj`, hand-add `LyricsWindow.swift`** to the Sources build phase mirroring how `MLAlbumGallery.swift` was added in phase 11 (find its three references: PBXBuildFile, PBXFileReference, and the group + Sources phase entries). Commit Swift + pbxproj + checklist together.

```bash
git commit -m "feat(lyrics): macOS viewer, FFI glue, 5 surfaces + A1 link (blind)"
```

---

### Task 6: TUI lyrics screen + key

**Files:**
- Modify: `frontends/tui/mod.rs`, `frontends/tui/media_library/mod.rs`, `frontends/tui/ui/` (new draw fn or overlay)
- Test: none automated for the UI; the core decision is already unit-tested (Task 1)

**Interfaces:**
- Consumes: `crate::lyrics::lyrics_action`.

- [ ] **Step 1:** Add a key (choose a free key — grep the TUI keymap; `L`/`y` are candidates, confirm unused) on ML Files/Albums track rows and the active playlist that resolves the highlighted row's path + display artist/title/album_artist and calls `lyrics_action`.
- [ ] **Step 2:** `LyricsAction::Show(text)` → a scrollable read-only text overlay/screen (mirror the existing help/overlay pattern in `frontends/tui/ui/overlays.rs`) titled with the track title, wrapped, Esc closes. `Search(url)` → best-effort `std::process::Command::new("xdg-open").arg(&url).spawn()`; on spawn error, show the URL string in a status line (capability note — terminals can't always open a browser). Gate the Esc-close so it doesn't collide with existing overlay/search Esc handling (same guard style phase-11 used: `!add_active && !search_active`).
- [ ] **Step 3: Full gate + commit.**

```bash
git commit -am "feat(lyrics): TUI lyrics view/search screen"
```

---

### Task 7: Search-query/display-fallback consistency guard

**Files:**
- Modify: `src/lyrics.rs` (add the test)
- Test: inline

**Interfaces:** none new.

- [ ] **Step 1:** Add a test that pins the query-artist fallback to the SAME precedence the row display uses, so the two can't silently diverge. If `src/model.rs` exposes a display-artist accessor, assert `query_artist(a, aa)` agrees with it on a shared table of `(artist, album_artist)` fixtures including both-empty, artist-only, album-artist-only, both-present. If no public accessor exists, assert the documented precedence directly and add a `// keep in sync with model.rs display fallback` comment pointing at the exact function.

- [ ] **Step 2: Run, verify pass; full gate; commit.**

```bash
git commit -am "test(lyrics): pin search-query fallback to row-display precedence"
```

---

## Self-Review Notes

- Spec coverage: core decision fn + URL (Task 1), five surfaces + A1 GTK (Task 4) / mac (Task 5) / TUI (Task 6), skin-font viewer + CSS test (Task 3), browser fallback (Tasks 3/5/6), Edit-in-tag-editor link + A1 text link (USER DECISIONS, Tasks 3/5), device slow-path degrades to Search (Task 4 step 3 + Task 1 unreadable test), fallback-consistency guard (Task 7). All design-doc automated tests are present.
- No new config, no writes to disk, deletion rule untouched.
- macOS is blind: bridge.h byte-mirror + checklist + hand-edited pbxproj are in the same commits as their code.
