# Phase 11 — A4 Album Gallery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Read `2026-07-19-opus-handoff.md` first, then this plan. Re-verify every file/line anchor fresh at execution — earlier phases moved things.

**Goal:** A Media Library view mode that shows albums as a grid of cover thumbnails; clicking an album shows its tracks in the existing Files table; double-click plays.

**Architecture:** Core owns one album-grouping query (`albums`) plus an `album_tracks` fetch; grouping is done Rust-side so phase 10's `effective_album_artist` stays the single source of truth for the album-artist toggle. GTK gets a new recycled-cell `GridView` module (`album_gallery.rs`) driving a new `"albums"` stack page + sidebar row; clicking an album reuses the Files stack page via a shared album-filter override. mac mirrors with a `LazyVGrid`; TUI adds an Albums tab with text list → track drill-down (no art). Thumbnails reuse phase-2's `now_playing::thumb_path_for(path, px)` cache — a zoom control just requests a different `px` key.

**Tech Stack:** Rust core (rusqlite), GTK4 (`gtk4::GridView` + `gio::ListStore` + `SignalListItemFactory`), SwiftUI (`LazyVGrid`), Ratatui.

## Global Constraints

- Build/test ONLY inside distrobox: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`. Host builds fail. NEVER gate on `cargo build --lib` — GTK/TUI code compiles only in the bin target. Run cargo in the FOREGROUND.
- Zero warnings, zero failures before any "done" claim. Quote BOTH `test result:` lines (lib + bin). Suite floor entering this phase: **562 lib + 754 bin** (from phase 10).
- New top-level `src/` modules need `mod x;` in BOTH `src/lib.rs` AND `src/main.rs`.
- Grouping key is `(album, effective_album_artist(artist, album_artist, artist_as_album_artist))`. Blank album → one "(no album)" bucket sorted LAST. This is the ONLY album-artist logic — never re-derive it in SQL.
- macOS is BLIND (no compiler): read whole files before editing; every new/changed C-visible FFI symbol or `#[repr(C)]` struct hand-mirrored byte-for-byte in `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`; verification items appended to `docs/mac-pass-checklist.md` in the SAME commit.
- Config: new fields use `#[serde(default)]` + `Default` impl.
- RefCell borrows never held across a UI call / callback / `select_row` (double-borrow = panic).
- GTK strings through `gtk_safe()`. SQLite is not Send — DB work stays on its thread. Library ≈ 36k tracks: per-open work must be bounded, thumb generation background + placeholder-first.
- Deletion rule unchanged: gallery never deletes files; removing tracks from a playlist never deletes from disk.
- Commits: conventional prefix, body = why + a verification line, trailer `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. Casing "Sparkamp".

---

## File Structure

- `src/media_library/queries.rs` — MODIFY: add `AlbumGroup`, `AlbumSort`, `albums()`, `album_tracks()`, lean `album_rows()`.
- `src/media_library/mod.rs` — MODIFY: `pub use` the two new public types if the module re-exports (verify existing re-export pattern for `LibTrack`).
- `src/ffi/media_library.rs` — MODIFY: `SparkampAlbum` `#[repr(C)]`, `sparkamp_ml_album_count`, `sparkamp_ml_albums`, `sparkamp_ml_album_tracks`.
- `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` — MODIFY: mirror the three FFI symbols + struct.
- `frontends/gtk/window/album_gallery.rs` — CREATE: the gallery `GridView` builder + cell factory + zoom/sort controls; returns the page widget + a rebuild closure. `include!`d into `frontends/gtk/window/mod.rs`.
- `frontends/gtk/window/mod.rs` — MODIFY: add the `include!` line for `album_gallery.rs` (verify how sibling window modules are pulled in).
- `frontends/gtk/window/media_library.rs` — MODIFY: sidebar "Albums" row + stack page wiring; shared `album_filter` override honored by `rebuild_files`; "Play album" / "Enqueue album" actions.
- `frontends/gtk/window/state.rs` — MODIFY: `album_filter` + gallery rebuild handle if needed on `AppState`.
- `src/config.rs` — MODIFY: `WindowConfig.gallery_thumb_px: u32` (default 160), `WindowConfig.gallery_sort: String` (default `"artist"`).
- `frontends/SparkampMac/Sources/MLAlbumGallery.swift` — CREATE: SwiftUI gallery view.
- `frontends/SparkampMac/Sources/SparkampModel+MediaLibrary.swift` + `SparkampModelTypes.swift` — MODIFY: album model structs + FFI bridge calls + album→files-filter navigation.
- `frontends/SparkampMac/Sources/MediaLibraryWindow.swift` — MODIFY: add the gallery to the ML view switcher.
- `frontends/tui/media_library/mod.rs` + `frontends/tui/ui/media_library.rs` — MODIFY: `MediaLibraryTab::Albums`, list + drill-down state, rendering.
- `docs/mac-pass-checklist.md` — MODIFY: phase-11 section (with the mac tasks, same commit).
- `docs/superpowers/specs/2026-07-17-winamp-parity-roadmap-design.md` — MODIFY at close-out: known-limitations if any residual accepted.

---

### Task 1: Core album grouping + track fetch

**Files:**
- Modify: `src/media_library/queries.rs`
- Modify: `src/media_library/mod.rs` (re-export the new public types, matching the existing `LibTrack` export)
- Test: inline `#[cfg(test)]` in `queries.rs` (follow the existing test module pattern there; use the in-memory / temp-DB helper the file already uses).

**Interfaces:**
- Consumes: `LibTrack` (`src/media_library/mod.rs`), `crate::play_stats::effective_album_artist(artist, album_artist, flag) -> String`.
- Produces:
  - `pub enum AlbumSort { Artist, Album, Year }` (add `#[derive(Copy, Clone, Debug, PartialEq, Eq)]`).
  - `pub struct AlbumGroup { pub album: String, pub album_artist: String, pub year: Option<i64>, pub track_count: i64, pub artwork_path: Option<String>, pub is_no_album: bool }` (`#[derive(Clone, Debug, PartialEq)]`).
  - `pub fn albums(&self, sort: AlbumSort, artist_as_album: bool) -> Result<Vec<AlbumGroup>>`.
  - `pub fn album_tracks(&self, album: &str, album_artist: &str, artist_as_album: bool) -> Result<Vec<LibTrack>>`.

**Design notes:**
- Grouping MUST be Rust-side (SQL cannot express `effective_album_artist`). Fetch a lean projection ordered deterministically, then fold.
- Representative artwork = the first `Some(artwork_path)` encountered in group order (deterministic).
- `year` = min non-NULL year in the group (`None` if all NULL).
- Blank/whitespace `album` → the single `AlbumGroup { album: "".into(), is_no_album: true, .. }`; it sorts LAST regardless of `sort`.
- Group ordering: Artist → by `(album_artist.to_lowercase(), album.to_lowercase())`; Album → by `(album.to_lowercase(), album_artist.to_lowercase())`; Year → by `(year unwrap_or(i64::MAX)` ascending so unknown-year sorts after known, then album_artist). The `is_no_album` bucket always appended last after sorting the rest.
- `album_tracks` matches rows whose `(album, effective_album_artist)` equals the requested pair (empty `album_artist` matches the group whose effective artist is empty; empty `album` matches the no-album bucket), ordered by `disc_num` (NULLs→0), `track_num` (NULLs last by filename), `filename`.

- [ ] **Step 1: Write the failing tests**

Add tests covering: (a) two artists sharing an album name → two groups; (b) multi-disc album folds to one group with `track_count` summed; (c) `artist_as_album=true` merges tracks whose `album_artist` is blank but `artist` matches, vs `false` splitting them; (d) blank-album bucket present, `is_no_album`, sorted last; (e) representative artwork = first non-NULL in order, deterministic across repeated calls; (f) `year` = min; (g) each `AlbumSort` orders as specified; (h) `album_tracks` ordering: disc 2 track 1 after disc 1 track 9, NULL `track_num` last by filename. Insert fixtures via the file's existing insert/upsert test helper.

- [ ] **Step 2: Run tests, verify they fail**

Run (in dev-box): `cargo test -p sparkamp media_library::queries 2>&1 | tail -30` — expect compile error / unresolved `albums`.

- [ ] **Step 3: Implement `album_rows`, `albums`, `album_tracks`**

Lean projection helper:
```rust
struct AlbumRow {
    artist: String,
    album: String,
    album_artist: String,
    year: Option<i64>,
    artwork_path: Option<String>,
}

fn album_rows(&self) -> Result<Vec<AlbumRow>> {
    let mut stmt = self.conn.prepare(
        "SELECT COALESCE(artist,''), COALESCE(album,''), COALESCE(album_artist,''),
                year, artwork_path
         FROM tracks
         ORDER BY LOWER(COALESCE(album_artist,'')), LOWER(COALESCE(artist,'')),
                  LOWER(COALESCE(album,'')), COALESCE(disc_num,0), COALESCE(track_num,0)",
    )?;
    let rows = stmt.query_map([], |r| Ok(AlbumRow {
        artist: r.get(0)?, album: r.get(1)?, album_artist: r.get(2)?,
        year: r.get(3)?, artwork_path: r.get(4)?,
    }))?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}
```
`albums()` folds `album_rows()` into an index-map keyed by `(album.trim().to_lowercase(), effective_album_artist(...).to_lowercase())` for blank-album detection use `album.trim().is_empty()`. Accumulate count, min-year, first-art. Then split off the no-album bucket, sort the rest per `sort`, append the bucket. Store the display `album`/`album_artist` from the first row of each group (so casing is preserved).

`album_tracks()` fetches full `LibTrack`s (reuse the full `SELECT … collect_tracks` column list already in this file) with no WHERE on album, then filters in Rust by matching `album.trim()` (case-insensitive) and `effective_album_artist(...)` (case-insensitive) to the requested pair, then sorts by `(disc_num.unwrap_or(0), track_num, filename)` with `track_num: None` last. (Rust filter keeps the effective-artist logic single-sourced; 36k rows is a few ms and this runs only on an album click.)

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p sparkamp media_library::queries 2>&1 | tail -20` — expect PASS.

- [ ] **Step 5: Full gate + commit**

Run: `cargo build && cargo test 2>&1 | tail -15`. Zero warnings. Commit `feat(core): album grouping + album_tracks queries for gallery`.

---

### Task 2: Thumbnail cache invalidation on artwork change

**Files:**
- Modify: `src/media_library/queries.rs` (`refresh_artwork` at ~384, `clear_artwork` at ~374 — re-verify line numbers)
- Modify: `src/now_playing.rs` (add a `thumb_glob_prefix` / delete helper next to `thumb_path_for`)
- Test: `src/now_playing.rs` inline tests.

**Interfaces:**
- Consumes: `now_playing::thumb_path_for`.
- Produces: `pub fn delete_thumbs_for(artwork_path: &Path)` in `now_playing.rs` — removes every `thumbs/<hash>-*.png` for that source path (all `px` sizes), best-effort (ignore missing).

**Design notes:** Phase 2 added no invalidation (verified: no `thumbs` reference in `queries.rs`/`tags.rs`). With a zoom control there are now multiple `px` files per source; deleting one size is insufficient. `delete_thumbs_for` reads the `thumbs` dir and removes entries whose name starts with `<hash>-`. `refresh_artwork` and `clear_artwork` call it for the OLD artwork path so a replaced cover regenerates. Guard: only ever touch paths under the cache `thumbs` dir (same discipline as `refresh_artwork`'s cache-only delete rule).

- [ ] **Step 1: Failing test** — `delete_thumbs_for` removes both a `-160.png` and a `-96.png` for one source hash but leaves another source's thumbs; and is a no-op when the dir is absent. Build the temp thumbs dir with `thumb_path_for` to get real hashed names.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement `delete_thumbs_for`; call it in `refresh_artwork`/`clear_artwork` for the prior artwork path** (fetch the existing `artwork_path` for the row before overwriting).
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Full gate + commit** `fix(core): invalidate all thumbnail sizes when artwork changes`.

---

### Task 3: FFI album surface + bridge header

**Files:**
- Modify: `src/ffi/media_library.rs` (mirror `SparkampLibTrack` fixed-buffer idiom at ~26/86; `sparkamp_ml_get_tracks` at ~894 is the array-return template)
- Modify: `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`
- Modify: `docs/mac-pass-checklist.md` (phase-11 FFI roundtrip item)
- Test: `#[cfg(test)]` roundtrip in `src/ffi/media_library.rs` (or wherever FFI tests live — verify).

**Interfaces:**
- Consumes: `MediaLibrary::albums/album_tracks`, `AlbumSort`, ctx config `artist_as_album_artist`.
- Produces:
  - `#[repr(C)] pub struct SparkampAlbum { album: [c_char; N], album_artist: [c_char; N], artwork_path: [c_char; PATH_N], year: i64, track_count: i64, has_year: u8, is_no_album: u8, _pad: [u8;6] }` — pick `N`/`PATH_N` to match the existing `SparkampLibTrack` buffer sizes (reuse the same consts). Keep it C-ABI clean; mirror byte-for-byte in the header.
  - `sparkamp_ml_album_count(ctx: *const SparkampCtx, sort: u32) -> c_int` (sort: 0=Artist,1=Album,2=Year).
  - `sparkamp_ml_albums(ctx, sort: u32, out: *mut SparkampAlbum, limit: c_int) -> c_int` (returns count written).
  - `sparkamp_ml_album_tracks(ctx, album: *const c_char, album_artist: *const c_char, out: *mut SparkampLibTrack, limit: c_int) -> c_int`.
- `sort` maps to `AlbumSort` via a small match (default Artist on unknown). `artist_as_album` read from `(*ctx).config` (verify the config accessor used by neighboring FFI, e.g. how `artist_as_album_artist` was read in phase 10).

**Design notes:** Follow the exact null-check / `limit` clamp / `from_lib_track` copy pattern of `sparkamp_ml_get_tracks`. Album-list caching so `_count` then `_albums` stay consistent: recompute in each call is acceptable (mac calls count then fetch back-to-back; the DB is single-threaded). String fields via the existing fixed-buffer copy helper (truncate + NUL-terminate like `SparkampLibTrack::from_lib_track`).

- [ ] **Step 1: Failing test** — build a temp ML with two albums, call `albums` through the core, assert an FFI-shaped roundtrip (or a thin test that constructs `SparkampAlbum::from_group` and checks buffer NUL-termination + `has_year`/`is_no_album` flags).
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement struct + three functions + `from_group`.**
- [ ] **Step 4: Mirror in `sparkamp_bridge.h`; append checklist item.**
- [ ] **Step 5: Run, verify pass; full gate; commit** `feat(ffi): album gallery FFI (count/list/tracks) + bridge`.

---

### Task 4: GTK gallery module — grid, cells, zoom, sort

**Files:**
- Create: `frontends/gtk/window/album_gallery.rs`
- Modify: `frontends/gtk/window/mod.rs` (add the `include!`, matching sibling modules like `now_playing.rs`)
- Modify: `src/config.rs` (add `WindowConfig.gallery_thumb_px: u32` default 160, `gallery_sort: String` default `"artist"`, both `#[serde(default)]` with a `default_gallery_thumb_px()` fn; update the `Default` impl)
- Modify: `src/skin.rs` (add `.album-cell` / `.album-cell-title` / `.album-cell-artist` selectors + extend a `render_gtk_css_covers_*` test)
- Test: config default test in `config.rs`; skin-cover test in `skin.rs`. (The GridView itself is user-verified; no unit test.)

**Interfaces:**
- Consumes: `MediaLibrary::albums`, `now_playing::thumb_path_for`, `AlbumGroup`, config `gallery_thumb_px`/`gallery_sort`/`artist_as_album_artist`, the existing no-art placeholder helper (find phase-2's 50%-logo placeholder — GTK `art_window.rs`/`now_playing.rs`; reuse it, don't reinvent).
- Produces (a builder fn callable from `media_library.rs`, exact signature to finalize against `AppState`):
  ```rust
  // Returns (page_widget, rebuild_closure). rebuild reloads albums from the
  // DB and repopulates the ListStore. on_album_activate is called with
  // (album, album_artist) when a cell is double-clicked/Enter.
  fn build_album_gallery(
      state: &Rc<RefCell<AppState>>,
      on_album_activate: Rc<dyn Fn(String, String)>,
  ) -> (gtk4::Widget, Rc<dyn Fn()>);
  ```

**Design notes:**
- `gtk4::GridView` + `gio::ListStore::new::<glib::BoxedAnyObject>()` holding `AlbumGroup`, wrapped in a `NoSelection` (activation via `GridView::connect_activate`). REQUIRED for perf — recycled cells, never a FlowBox of 3k widgets.
- `SignalListItemFactory`: `setup` builds a fixed cell (vertical box: `Image` + title `Label` + artist `Label` + optional year), `bind` sets labels from the bound `AlbumGroup`, requests the thumb lazily: if `thumb_path_for(art, px)` exists load it, else set placeholder and enqueue background generation (reuse A2's single background worker pattern — locate it in `media_library.rs` ~4586 thumb block / `now_playing.rs`; do NOT spin a thread per cell). `unbind` clears the `Image` to avoid stale recycled art.
- Cell/thumb size = `state.borrow().config.window.gallery_thumb_px`. A zoom control (a `Scale` or −/＋ buttons, range ~96–256 step 32) in a header row updates `px`, writes config, and rebuilds the factory sizing + store (re-request thumbs at the new `px`). Persisted on change (copy the adjacent per-widget save idiom) and on window close.
- Sort dropdown (`DropDown` with "Artist"/"Album"/"Year") → maps to `AlbumSort`, writes `config.window.gallery_sort`, rebuilds. Default from config.
- `rebuild_closure` opens the DB via the existing `ensure_media_lib_open`/`media_lib` access, calls `albums(sort, artist_as_album)`, and resets the store. Guard against no-DB (skip_db_load) — empty grid until opened.

- [ ] **Step 1: Config fields + defaults + test** (add fields, `Default`, a `config_defaults_gallery` test). Run `cargo test -p sparkamp config 2>&1 | tail`.
- [ ] **Step 2: Skin selectors + cover test.** Run the `render_gtk_css_covers_*` test.
- [ ] **Step 3: Create `album_gallery.rs`** with the builder, factory (setup/bind/unbind), zoom + sort controls, background-thumb reuse, and the `include!` wiring. Placeholder for no-art.
- [ ] **Step 4: Full gate** (`cargo build && cargo test`) — the module must compile in the bin target; zero warnings.
- [ ] **Step 5: Commit** `feat(gtk): album gallery grid module (zoom + sort, recycled cells)`.

---

### Task 5: GTK — sidebar entry, stack page, album→Files drill-down, play/enqueue

**Files:**
- Modify: `frontends/gtk/window/media_library.rs` (sidebar row ~302 pattern; stack `add_named` ~5580; sidebar→stack wiring `connect_row_selected` ~8573; `rebuild_files` closure ~near 5580; Files-view play/enqueue actions)
- Modify: `frontends/gtk/window/state.rs` (`album_filter: Rc<RefCell<Option<(String,String)>>>` on AppState, or a local `Rc` threaded through — prefer a local `Rc` shared into both `rebuild_files` and the gallery callback to avoid AppState churn; decide at implementation)
- Test: none new (interactive); rely on gate.

**Interfaces:**
- Consumes: `build_album_gallery` (Task 4), `MediaLibrary::album_tracks`, existing Files-view populate (`rebuild_files`) and existing ML play/enqueue actions.

**Design notes:**
- Add an "Albums" sidebar row (`widget_name("albums")`) right after "Files" (same `Label`+`ListBoxRow` idiom). Add gallery page: `stack.add_named(&gallery_page, Some("albums"))`. In `connect_row_selected`, `else if name == "albums" { stack_ref.set_visible_child_name("albums"); rebuild_gallery(); }`.
- Album drill-down via a shared `album_filter: Rc<RefCell<Option<(String,String)>>>`:
  - `rebuild_files` (Files populate): at the top, if `album_filter.borrow().is_some()`, populate the store from `lib.album_tracks(album, album_artist, artist_as_album)` and show the album name in the Files header/search placeholder; else keep the existing search/all path. Clearing the search entry or re-selecting the "Files" sidebar row sets `album_filter = None` and rebuilds (so the user escapes back to the full library). Selecting "Albums" leaves the filter as-is for the grid.
  - `on_album_activate = move |album, album_artist| { *album_filter.borrow_mut() = Some((album, album_artist)); select the "Files" sidebar row (which switches the stack) ; rebuild_files(); }`. Do the borrow-set BEFORE `select_row` and never hold the borrow across it (RefCell rule).
- "Play album" / "Enqueue album": add to the gallery cell's right-click menu (or header buttons operating on the activated album) — fetch `album_tracks` and route through the SAME functions the Files-view "Play"/"Enqueue" actions already call (locate them; reuse, don't duplicate append-or-replace logic).

- [ ] **Step 1: Add sidebar "Albums" row + gallery stack page + wire `connect_row_selected` "albums" branch.**
- [ ] **Step 2: Thread `album_filter` Rc into `rebuild_files`; honor it; clear on search-change / Files re-select.**
- [ ] **Step 3: Implement `on_album_activate` → set filter, select Files row, rebuild.**
- [ ] **Step 4: Play/Enqueue album actions reusing existing ML actions.**
- [ ] **Step 5: Full gate (`cargo build && cargo test`, zero warnings) + commit** `feat(gtk): album gallery navigation + play/enqueue album`.

---

### Task 6: macOS gallery (LazyVGrid) + navigation

**Files:**
- Create: `frontends/SparkampMac/Sources/MLAlbumGallery.swift`
- Modify: `frontends/SparkampMac/Sources/SparkampModelTypes.swift` (an `AlbumGroup` Swift struct), `SparkampModel+MediaLibrary.swift` (FFI bridge: `loadAlbums(sort:)`, `albumTracks(album:albumArtist:)`, album→files-filter state), `MediaLibraryWindow.swift` (add gallery to the ML view switcher + zoom/sort controls)
- Modify: `docs/mac-pass-checklist.md` (phase-11 gallery walk items, same commit)
- Test: none (blind); mirror-correctness only.

**Design notes (BLIND — read each whole file first):**
- Swift `AlbumGroup` mirrors `SparkampAlbum` fields; `loadAlbums(sort:)` calls `sparkamp_ml_album_count` then `sparkamp_ml_albums` into a `[SparkampAlbum]` buffer, decoding fixed `c_char` buffers to `String` (reuse the existing `SparkampLibTrack` decode helper in `SparkampModel+MediaLibrary.swift`).
- `LazyVGrid` with adaptive columns sized by a `@Published ggalleryThumbPx` (zoom `Slider`/stepper 96–256), matching GTK. Thumb: reuse the existing mac thumbnail-cache path (`thumb_path_for` is core; mac calls it via the same helper A2 used for the ML thumbnail column / ArtworkWindow — locate and reuse), placeholder = the mac 50%-logo no-art view.
- Sort `Picker` (Artist/Album/Year) → re-`loadAlbums`.
- Album tap → set a `selectedAlbum` filter that `MLFilesTable` honors by loading `albumTracks(...)` instead of the full list (mirror GTK's `album_filter`); a back affordance / re-selecting Files clears it. "Play album"/"Enqueue album" reuse existing mac ML play/enqueue.
- Persist zoom + sort in the mac settings store if that's where window prefs live (match neighbors); otherwise mirror GTK config field names in the checklist as a follow-up. Keep FFI structs byte-identical to the header.

- [ ] **Step 1: Swift `AlbumGroup` + `loadAlbums`/`albumTracks` bridge in `SparkampModel+MediaLibrary.swift`.**
- [ ] **Step 2: `MLAlbumGallery.swift` LazyVGrid + zoom + sort + placeholder + thumb reuse.**
- [ ] **Step 3: Wire into `MediaLibraryWindow.swift` view switcher; album tap → files filter; play/enqueue.**
- [ ] **Step 4: Mirror-check FFI usage vs `sparkamp_bridge.h`; append `docs/mac-pass-checklist.md` phase-11 items.**
- [ ] **Step 5: Full gate (Rust still builds — no Rust change expected; if any, zero warnings) + commit** `feat(mac): album gallery view + navigation (blind)`.

---

### Task 7: TUI Albums tab + drill-down

**Files:**
- Modify: `frontends/tui/media_library/mod.rs` (`MediaLibraryTab` enum + Tab-cycle at ~239; per-tab dispatch ~280; state for album list + selected album)
- Modify: `frontends/tui/ui/media_library.rs` (render the Albums tab list + drilled track list)
- Modify: `frontends/tui/mod.rs` if `MediaLibraryState` needs new fields (album list cache, selected index, drill-down `Option<(String,String)>`)
- Test: `frontends/tui/tests/views.rs` — a render smoke test for the Albums tab (follow existing view-test pattern).

**Design notes:**
- Add `MediaLibraryTab::Albums`; extend the `Tab` cycle to Files → Playlists → Discs → Albums → Files.
- Albums tab shows a text list: `Album — Album Artist (year)  ·  N tracks`, from `lib.albums(sort, artist_as_album)` (default Artist sort; no sort UI needed for TUI, or reuse the existing ML sort key if trivial). No art.
- `Enter` on an album → drill-down: replace the list with the album's tracks (`album_tracks`) rendered like the Files track list; `Esc` returns to the album list (not out of the ML). A `drill: Option<(String,String)>` field in `MediaLibraryState` tracks this.
- Cache the album list on tab entry (don't re-query per keystroke).

- [ ] **Step 1: Add `MediaLibraryTab::Albums` + Tab-cycle + state fields.**
- [ ] **Step 2: Album list load on tab entry + render.**
- [ ] **Step 3: Enter→drill / Esc→back with `album_tracks`.**
- [ ] **Step 4: Render smoke test.**
- [ ] **Step 5: Full gate + commit** `feat(tui): album gallery tab with track drill-down`.

---

## Automated test summary (must all pass in the final gate)

- Core: grouping (two-artist split, multi-disc fold, toggle on/off, blank bucket last, deterministic rep-art, min-year), each sort order, `album_tracks` ordering.
- Core: `delete_thumbs_for` multi-size deletion + no-op-when-absent.
- FFI: `SparkampAlbum` roundtrip / buffer NUL-termination + flags.
- Config: gallery defaults. Skin: `.album-cell` cover test. TUI: Albums tab render smoke.

## Manual test plan (→ user interactive GTK pass; mac → checklist)

1. Gallery renders the real ~36k-track library: scroll smooth (recycled cells, thumbs pop in lazily, no block), memory sane.
2. Grouping sanity: a known multi-disc album appears once; compilations split/merge correctly per the album-artist toggle.
3. Click album → correct tracks in disc/track order in the Files table; double-click plays; Play/Enqueue album behave like the Files-view equivalents.
4. Sort dropdown reorders (Artist/Album/Year).
5. Zoom control resizes cells; thumbs regenerate at the new size; setting persists across ML re-open.
6. No-art albums show the 50%-logo placeholder; after adding a cover + `refresh_artwork`, re-opening the gallery shows the new thumb (invalidation works).
7. Escape back to full library (clear search / re-select Files) restores all tracks.
8. mac gallery walk (checklist); TUI Albums tab: Tab reaches it, list renders, Enter drills, Esc returns.

## Performance notes

- One lean `album_rows` query per gallery open, folded in Rust; cache the result for the view session, refresh on scan-complete. Never group 36k rows on every keystroke.
- Thumb generation burst on first open: reuse A2's single background worker + placeholder-first; cap concurrency.
- Zoom change requests a new `px` set — thumbs regenerate lazily; old sizes stay cached (invalidation only on artwork change, Task 2).

## Open questions

Resolved 2026-07-29 with the user: track pane = **filtered Files table** (reuse existing Files view); cell size = **zoom control** (not fixed), multiple cache-size keys + zoom UI on GTK & mac.
