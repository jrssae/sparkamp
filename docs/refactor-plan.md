# Large-file refactor plan (branch: cleanup-large-files)

## Status (2026-06-12)

- Phase 1 (a–d): DONE. 642 tests green, zero warnings.
- Phase 3 (a–c): DONE. Xcode build green.
- Phase 2 (GTK window.rs) and Phase 4 (ML columns): NOT STARTED — blocked
  on this machine. `frontends/gtk` is `#[cfg(target_os = "linux")]`, so the
  GTK code is never compiled on macOS (no syntax/name-resolution checking
  possible), and no docker/podman is available for a Linux toolchain.
  Run these phases on a Linux host, or set up a GitHub Actions
  `cargo check` workflow (ubuntu + libgtk-4-dev) and iterate through CI.
  Phase 4 goes with Phase 2: its GTK consumer is the larger half.

Goal: no source file over ~800 lines, no function over ~300, so smaller
models (Opus/Sonnet) can work on any file without losing context. Zero
functional change — every step is a mechanical move or a named-function
extraction, verified by `cargo build && cargo test` (zero warnings) and,
for Swift, an Xcode build.

This document is written so any phase can be executed in a fresh session
with no prior context. Line numbers are from the branch point
(`main` @ a07c66a) and will drift as phases land — re-locate by symbol
name, not line number.

## Ground rules

- One extraction per commit. Never combine a move with a behavior change.
- Rust: inherent `impl` blocks may be split across files in the same
  crate — `impl App` / `impl MediaLibrary` methods move freely. Visibility
  inside a module dir is `pub(super)` unless already `pub`.
- FFI: `#[no_mangle] extern "C"` symbol names are unaffected by module
  location. `sparkamp_bridge.h` must not change in Phase 1a.
- GTK changes are compile-checked only on this machine (no Linux host);
  runtime verification happens later on Linux. Note this in merge messages.
- Swift: the Xcode project uses manual file references (no
  fileSystemSynchronizedGroups). Every new .swift file needs
  project.pbxproj entries (PBXBuildFile, PBXFileReference, group child,
  Sources build phase). Pattern-match the GraniteView.swift entries.
- Swift extensions cannot hold stored properties — all `@Published` /
  stored vars stay in the class's main file; only methods move.
- CLAUDE.md rules apply: zero warnings, deletion rule, ask before
  expanding scope.

## File inventory at branch point

| File | Lines | Action |
|---|---|---|
| frontends/gtk/window.rs | 14,835 | Phase 2: split into window/ dir |
| frontends/tui/mod.rs | 4,690 | Phase 1d: split impl App |
| src/media_library.rs | 3,670 | Phase 1c: split into dir |
| src/ffi.rs | 3,275 | Phase 1a: split into dir |
| frontends/tui/ui.rs | 2,371 | Phase 1d: split draw fns |
| MediaLibraryWindow.swift | 2,078 | Phase 3b: 5 files |
| src/model.rs | 1,761 | leave; Phase 1b pulls dup helpers only |
| SparkampModel.swift | 1,518 | Phase 3c: extension files |
| src/skin.rs | 1,403 | leave (cohesive) |
| src/config.rs | 1,121 | leave (cohesive) |

Inside window.rs the real monsters are single functions:
`build()` ≈ 4,000 lines, `open_media_library_window()` ≈ 5,000,
`open_settings_window()` ≈ 1,500.

## Known duplication (Phase 1b / Phase 4 targets)

1. `sanitize()` — identical fn + tests in src/model.rs and
   src/media_library.rs → single copy in new `src/textutil.rs`.
2. Two symphonia tag readers — `read_symphonia_metadata` (model.rs,
   returns 4-tuple) and `read_symphonia_tags` (media_library.rs, returns
   `TrackTags`) → one reader in new `src/tags.rs` returning the rich
   struct; model.rs caller adapts the tuple from it.
3. Date helpers duplicated *within* media_library.rs — `days_to_ymd`
   vs `year_month_day_from_days` are the same algorithm; plus
   `parse_iso_timestamp`, `days_since_1970`, `days_in_month`,
   `format_current_timestamp` → new `src/timeutil.rs`.
4. ML column metadata exists three times: GTK `MlColumnDef`/`ALL_COLUMNS`,
   TUI `ml_col_width/label/value`, Swift `MLFilesTable.ColumnSpec`.
   Phase 4 single-sources GTK + TUI from core `src/ml_columns.rs`.
   Swift keeps its own copy (FFI exposure not worth the surface).
5. Swift `SparkampTableView` + style helpers live in PlaylistView.swift
   but serve three tables → Phase 3a moves them to TableSupport.swift.

## Phase 1 — core Rust (safest first)

### 1a. src/ffi.rs → src/ffi/ dir
Existing section comments define the split:
- `mod.rs` — SparkampContext, lifecycle, main tick, callbacks, string
  utilities; `mod` declarations for the rest; shared helpers `pub(super)`.
- `playback.rs` — playback, navigation, repeat/shuffle, duration probing,
  config persistence.
- `playlist.rs` — playlist ops, playlist path accessor, background
  metadata scanning.
- `viz.rs` — visualizer data/mode, waveform style, bars/waveform zones.
- `granite.rs` — granite plasma settings.
- `eq.rs` — equalizer + EQ limit constants.
- `settings.rs` — behavior/settings, audio extensions.
- `id3.rs` — ID3 tag editor.
- `media_library.rs` — ML struct/lifecycle/folders/queries/playlists.
- `dedupe.rs` — dedup structs + FFI.

### 1b. Consolidate duplicated helpers
New `src/textutil.rs` (sanitize + tests), `src/tags.rs` (merged
symphonia reader), `src/timeutil.rs` (date/time helpers + tests).
Update model.rs and media_library.rs callers. Register mods in both
main.rs and lib.rs.

### 1c. src/media_library.rs → src/media_library/ dir
- `mod.rs` — MediaLibrary struct, open/open_at, schema, LibTrack,
  LibPlaylist, SortKeys, ReadOnlyTrackFields, AddFolderResult.
- `scan.rs` — folder management, rescan_* family, walk_dir, upsert_track,
  needs_metadata_scan, read_track_tags/TrackTags (now thin wrappers over
  src/tags.rs).
- `queries.rs` — all_tracks*, scanned_tracks, search_tracks*,
  sort_order_clause, track lookups, remove/soft-delete/purge.
- `playlists.rs` — playlist CRUD, m3u read/write, extinf, record_play.
Tests move with their subjects.

### 1d. TUI split
- `frontends/tui/mod.rs` keeps Mode, run(), App struct + small core impl
  (new/tick/marquee/transport).
- `keys.rs` — handle_key, handle_normal, handle_jump, handle_add_file,
  handle_move_track, handle_remove_track, drain_add_file_scan.
- `media_library.rs` — MediaLibraryState, MediaLibraryTab,
  handle_media_library, commit_ml_add_path, refresh_ml_sort/search,
  open_media_library.
- `id3.rs` — Id3EditorState, handle_id3_editor, handle_id3_extra,
  id3_save_and_close, id3_field_value_mut, id3_genre_matches.
- `settings_eq.rs` — SettingsState, EqState, handle_settings,
  handle_equalizer.
- `frontends/tui/ui.rs` → `ui/` dir mirroring: mod.rs (draw, header,
  progress, playlist, bars/waveform render), overlays.rs (jump, add-file,
  move/remove, help), media_library.rs, id3.rs, settings_eq.rs, plus
  shared small helpers (centered_popup, hint, sep, tail_chars) in mod.rs.

## Phase 2 — GTK window.rs

### 2a. window.rs → window/ dir (move whole functions, no internal splits yet)
- `mod.rs` — AppState (+ScanState) with `pub(super)` fields, impl
  AppState, scan helpers, module declarations.
- `helpers.rs` — gtk_safe, sanitize_id3_text/numeric, format_last_played,
  parse_hex_color, make_genre_combo, find_row_by_name, show_error_alert,
  load_logo_pixbuf, repeat_btn_icon/text, playlist save dialog helpers,
  notify_* fns, editor_cell_positions.
- `main_window.rs` — build() (still huge; split in 2c).
- `settings.rs` — open_settings_window.
- `equalizer.rs` — open_eq_window.
- `dedupe.rs` — open_dedupe_window.
- `id3_editor.rs` — open_id3_editor_window, open_id3_field_customizer,
  open_customize_columns_dialog, get_id3_field_value.
- `media_library.rs` — open_media_library_window (split in 2b).
- `fullscreen_viz.rs` — open_waveform_fullscreen, open_image_viewer.
- `ml_columns.rs` — MlColumnDef, ALL_COLUMNS, ml_sort_key (moves to core
  in Phase 4).
One commit per extracted file.

### 2b. Split open_media_library_window internals (~5,000 lines)
Extract named fns with explicit params: sidebar build, files page,
playlists-manage page, playlist-editor page, track context menus,
scan polling/tick. The function currently communicates through dozens of
captured Rc clones — each extraction takes the Rcs it needs as params.

### 2c. Split build() internals (~4,000 lines)
Extract: css/theme setup, main-window layout, playlist window, key
handler (the big EventControllerKey closure), drag-and-drop handlers,
tick loop.

## Phase 3 — Swift (each step: pbxproj entries + xcodebuild verify)

### 3a. TableSupport.swift
Move SparkampTableView, row-style helpers from PlaylistView.swift.

### 3b. MediaLibraryWindow.swift → 5 files
MediaLibraryWindow.swift (MediaLibraryView: nav, sidebar, toolbar, files
tab, helpers), MLPlaylistManagement.swift, MLPlaylistEditor.swift,
MLEditorTable.swift (incl. MLEditingRow), MLFilesTable.swift (incl.
ColumnSpec, MLTableEvent).

### 3c. SparkampModel.swift core + extensions
Stored props + init/deinit/tick/callbacks stay. Move methods:
SparkampModel+MediaLibrary.swift (ML + ML playlist CRUD),
SparkampModel+Dedupe.swift, SparkampModel+Transport.swift (transport +
playlist actions + persistence), SparkampModel+Keys.swift
(handleRawKey + shortcut routing). C-helper types (MLTrack etc.) can
move to SparkampModelTypes.swift if the core file is still over budget.

## Phase 4 — ML column single-source
Core `src/ml_columns.rs`: id, label, width hint, SQL sort key,
id3-editable flag. GTK ml_columns.rs and TUI ml_col_* become thin
consumers. Swift untouched.

## Verification checklist (every commit)
1. `cargo build` — zero warnings.
2. `cargo test` — all pass (baseline 644).
3. Phase 2: `cargo build` covers GTK (gtk feature is default on this
   tree); no runtime test available — flag in commit message.
4. Phase 3: xcodebuild (or user builds in Xcode); app launch smoke test
   by user.
5. `git diff --stat` sanity: moves should be ~1:1 adds/deletes.
