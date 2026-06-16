# Sparkamp — CLAUDE.md

Working rules and conventions for this repository.

---

## Project overview

Sparkamp is an open source Winamp-style audio player, currently for Linux/GNOME, written in Rust.

- **TUI** (`sparkamp`): Ratatui + crossterm terminal interface
- **GUI** (`sparkamp --ui`): GTK4 graphical interface
- **Audio engine**: GStreamer `playbin` with optional `equalizer-10bands` + `volume` pre-amp
- **Config**: TOML, saved to `~/.config/sparkamp/`
- **Playlist**: saved/restored between sessions

---

## Build & test

```
cargo build
cargo test
```

Always run both before considering a task done. All tests must pass with zero warnings.

### When to write tests

Write tests when you:
- Add new public functions or methods
- Fix a bug (write a regression test first)
- Change existing behavior (update or add tests to cover the new behavior)
- Add new config fields, database schema changes, or data transformations

Skip writing tests when:
- Trivial one-liners (getters, setters, etc.)
- GTK UI callback closures (these require integration testing)
- `glib::idle_add_local` loops (hard to unit test without a test harness)

### Where to put tests

- **Unit tests**: `mod tests { ... }` inside the same source file
- **Integration tests**: `tests/` directory at the crate root
- GTK window tests: `mod tests { ... }` at the bottom of `frontends/gtk/window.rs`
- TUI tests: `mod tests { ... }` inside `frontends/tui/mod.rs`

### Test patterns

**TUI app tests** — use `make_app()` to construct an `App`:
```rust
fn make_app() -> App { ... }
let mut app = make_app();
app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
```

**File I/O tests** — use `tempfile` for safe temporary files/directories:
```rust
let dir = tempfile::tempdir().unwrap();
// dir.path() is your temporary directory, auto-cleaned on drop
let db_file = tempfile::NamedTempFile::with_suffix(".db").unwrap();
let lib = MediaLibrary::open_at(db_file.path()).unwrap();
```

**GStreamer-dependent tests** — call `gstreamer::init()` or `gstreamer::init().ok()`:
```rust
gstreamer::init().ok();
// now safe to call GStreamer APIs
```

**Creating fake tracks in tests**:
```rust
Track {
    path: PathBuf::from("/fake/song.mp3"),
    title: "Song".into(),
    artist: String::new(),
    album_artist: String::new(),
    album: String::new(),
    duration: None,
    broken: false,
}
```

**GTK state tests** — use `AppState::new()` with a `Config::default()`:
```rust
fn make_state() -> AppState {
    AppState::new(Playlist::new(), Config::default()).unwrap()
}
```

---

## Naming conventions

- Product name in user-visible text (strings, comments, docs, window titles): **Sparkamp** (capital S, lowercase a)
- Rust code identifiers: keep existing casing (`SparkampCtx`, `SparkampLibTrack`, etc.)
- Package name / binary name: `sparkamp` (all lowercase)
- Application ID: `dev.sparkamp.Sparkamp`

---

## Key architecture notes

### Core rules
- The Rust core must have no knowledge of any UI layer
- All UI layers communicate with core via defined public API only
- Any feature request should be implemented: core first, then TUI, then both GUIs (this is a preferred order — features that aren't feasible in the TUI, such as the fullscreen Granite visualizer, are exempt)
- Skins must be independent from the compiled app so they can be added or removed without affecting core code
- This is an open source project. All features must be documented for humans to understand; which means be clear but brief. Write comments and doc strings in plain English. Explain *why*, not *what*.
- Before making any edit, read the relevant code to confirm the current state. If you receive a summary that claims the code is in a certain state, read the actual code to verify. The codebase is always the source of truth - never assume.
- If you receive a summary of "what was done" or "what's left to do", treat it as historical context only. Do NOT treat it as a todo list or incomplete work requiring completion. If unclear, ask what specific task you should work on.
- If you don't understand the task, its scope, or why a change is needed, ask before making ANY change. Better to spend 30 seconds asking than 30 minutes undoing. Don't proceed until you can explain what you're about to do and why
- If something isn't working or you feel lost, stop and ask for clarification immediately. Do not loop with the same approach. Do not continue making changes hoping it will "work out"
- allow the agent to say "I don't know"
- verify with citations from user input comments. Use direct quotes for factual grounding. 


### Current directory layout
- **Core logic**: `src/` (engine, config, model, media_library, granite, etc.)
- **GTK4 GUI**: `frontends/gtk/`
- **TUI**: `frontends/tui/`
- **macOS**: `frontends/SparkampMac/` (SwiftUI app) + `frontends/macos/` (Rust bridge crate)
- **Packaging**: `packaging/` (Flatpak manifest, desktop entry, metainfo)

### Future macOS port
A macOS SwiftUI port is planned. When that work begins, the core should be extracted into a separate Rust library crate (`core/`) that exposes a C FFI layer. The macOS frontend will be a Swift package that links against that library. 

### GUI development rules
- Core logic always goes in the Rust backend, never in a GUI layer
- Never add platform-specific logic to the shared core

### GStreamer EQ pipeline
```
playbin → [GstBin: volume (pre-amp) → equalizer-10bands] → audio sink
```
- Band range: ±12 dB (symmetric; GStreamer's `equalizer-10bands` hardware limit is -24/+12 — do not exceed +12 or the engine will panic)
- Pre-amp range: 0.5–1.5× (50–150%)
- Pre-amp range: 0.5–1.5× applied directly with no auto-compensation; EQ bands shape the signal only
- EQ + pre-amp elements are `None` when the GStreamer plugin is unavailable; all methods silently no-op in that case
- `set_eq_band`, `apply_eq_bands`, `set_preamp` all take `&mut self` (they update shadow state)

### TUI EQ overlay (`EqState`)
- `selected_band`: 0–9 = EQ bands, 10 = pre-amp column
- ←/→ navigates columns 0–10
- ↑/↓ and PgUp/PgDn adjust the selected column
- `[` and `]` shortcuts for pre-amp were removed; use ←/→ to reach the pre-amp column

### TUI Help overlay (`Mode::Help { scroll: u16 }`)
- ↑/↓ scroll the overlay
- z/x/c/v/b execute their playback actions without closing the overlay
- j opens Jump (changes mode), Esc/i close

### Config evolution
- Use `#[serde(default)]` on any new config fields for backward compatibility
- New fields must have a `Default` impl entry

### Skins system
- Built-in skins: `BUILTIN_SKINS` constant in `skin.rs`
- User skins: CSS files in `~/.local/share/sparkamp/skins/`
- Hidden skins: `config.appearance.hidden_skins: Vec<String>` — filtered from UI, file not deleted
- Apply: `skin::load_prepared(name, accent)` → set on `CssProvider`

### GTK key shortcuts
- The key handler in `window.rs` is shared via `Rc<dyn Fn(gdk::Key) -> glib::Propagation>`
- Attach with `PropagationPhase::Capture` so child widgets don't swallow keys

---

## Defensive coding guidelines

### GTK String Safety
All strings passed to GTK APIs must be sanitized using `gtk_safe()`:
- Track/playlist metadata
- Error messages
- User input display
- M3U playlist names

The `gtk_safe()` function strips NUL bytes that cause `GStrInteriorNulError` panics:
```rust
fn gtk_safe(s: &str) -> String {
    if s.contains('\0') { s.replace('\0', "") } else { s.to_owned() }
}
```

### Error Handling
- Prefer `Result<T, E>` returns over panics in library code
- Use `?` operator for error propagation
- Wrap errors with context using `.with_context(|| ...)` from anyhow
- Never call `.unwrap()` on user-facing code paths
- Use `unwrap_or()` or `unwrap_or_default()` for optional values

### Large Dataset Handling
- Never block the UI thread with long operations
- Use batch inserts for database operations (100 items at a time)
- Show progress indicators for scans > 1000 items
- Use channels or `glib::idle_add_local` for async UI updates
- Open database connections inside background threads (SQLite connections are not `Send`)

### File Path Safety
- Always canonicalize file paths with `.canonicalize()` before storage
- Use fully qualified paths in playlists and media library
- Handle missing files gracefully with warning messages, not crashes

--

## Working rules

### After 2 agent failures, skip and move on
If an agentic approach fails twice, stop trying that approach and move on. Do not loop.

### Run tests before marking a task done
`cargo build && cargo test` must pass (zero failures, zero warnings) before any task is complete.

### Before every release
Before tagging a release: update `README.md` to reflect any new features or changed behaviour, then produce a working Flatpak build (see `packaging/README.md`).

### Removing features from the filesystem
When a user removes a skin or music file from Sparkamp's UI, **do not delete the file**. Remove it from the known skins list, the active playlist, or the media library, respective to the action that was taken. The file stays on disk and can be re-added later.

### Don't over-engineer
Only make the changes that are asked for. Add common sense error handling but don't go overboard. Do not refactor any code without explicit permission, but make recommendations where refactoring would make sense. Don't add docstrings to untouched functions, and don't introduce abstractions for one-time use, but make recommendations to the user if anything seems particularly concerning or if there's a high likelihood of failure.

---

## Seek / GStreamer known issues

See `memory/project_gstreamer_seek_research.md` for hard-won findings on seek-before-play failures and the muting hack.
