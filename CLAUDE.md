# Sparkamp — CLAUDE.md

Working rules and conventions for this repository.

---

## Project overview

Sparkamp is a Winamp-style audio player for Linux/GNOME, written in Rust.

- **TUI** (`sparkamp`): Ratatui + crossterm terminal interface
- **GUI** (`sparkamp --ui`): GTK4 graphical interface
- **Audio engine**: GStreamer `playbin` with optional `equalizer-10bands` + `volume` pre-amp
- **Plugin system**: ABI v2 C-compatible `.so` plugins (visualizer + filetype)
- **Config**: TOML, saved to `~/.config/sparkamp/`
- **Playlist**: saved/restored between sessions

---

## Build & test

```
cargo build
cargo test
```

Always run both before considering a task done. All tests must pass with zero warnings.

---

## Naming conventions

- Product name in user-visible text (strings, comments, docs, window titles): **Sparkamp** (capital S, lowercase a)
- Rust code identifiers: keep existing casing (`SparkPluginAbi`, `SparkSettingDef`, `SPARKAMP_PLUGIN_ABI_VERSION`, etc.)
- Package name / binary name: `sparkamp` (all lowercase)
- Application ID: `dev.sparkamp.Sparkamp`

---

## Working rules

### After 2 agent failures, skip and move on
If an agentic approach fails twice, stop trying that approach and move on. Do not loop.

### Run tests before marking a task done
`cargo build && cargo test` must pass (zero failures, zero warnings) before any task is complete.

### Before every release
Before tagging a release: update `README.md` to reflect any new features or changed behaviour, then produce a working Flatpak build (see `packaging/README.md`).

### Comments must be human-readable
Write comments and doc strings in plain English. Explain *why*, not *what*.

### Removing features from the filesystem
When a user removes a skin, plugin, or music file from Sparkamp's UI, **do not delete the file**. Remove it from the known skins list, known plugins list, the active playlist, or the media library, respective to the action that was taken. The file stays on disk and can be re-added later.

### Don't over-engineer
Only make the changes that are asked for. Add common sense error handling but don't go overboard. If you see code that is near the feature being added or impacted by the feature being added, consider refactoring if improves readability and is low risk but ASK if you can refactor and describe the benefit and risks before making any changes. Don't add docstrings to untouched functions, and don't introduce abstractions for one-time use, but make recommendations to the user if anything seems particularly concerning or if there's a high likelihood of failure.

---

## Key architecture notes

### Core rules
- The Rust core must have no knowledge of any UI layer
- All UI layers communicate with core via defined public API only
- Any feature request should be implemented: core first, then TUI, then both GUIs (this is a preferred order — features that aren't feasible in the TUI, such as fullscreen visualizer plugins, are exempt)
- Skins and plugins must be independent from the compiled app so they can be added or removed without affecting core code
- This is an open source project. All features must be documented for humans to understand; which means be clear but brief. Write comments and doc strings in plain English. Explain *why*, not *what*.
- Before making any edit, read the relevant code to confirm the current state. If you receive a summary that claims the code is in a certain state, read the actual code to verify. The codebase is always the source of truth - never assume.
- If you receive a summary of "what was done" or "what's left to do", treat it as historical context only. Do NOT treat it as a todo list or incomplete work requiring completion. If unclear, ask what specific task you should work on.
- If you don't understand the task, its scope, or why a change is needed, ask before making ANY change. Better to spend 30 seconds asking than 30 minutes undoing. Don't proceed until you can explain what you're about to do and why
- If something isn't working or you feel lost, stop and ask for clarification immediately. Do not loop with the same approach. Do not continue making changes hoping it will "work out"
- allow the agent to say "I don't know"
- verify with citations from user input comments. Use direct quotes for factual grounding. 


### Current directory layout
- **Core logic**: `src/` (engine, config, model, plugins, etc.)
- **GTK4 GUI**: `frontends/gtk/`
- **TUI**: `frontends/tui/`
- **Plugins**: `plugins/` (workspace members, compiled separately as `.so`)
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
- `u` opens the EQ window in both TUI and GUI
- The key handler in `window.rs` is shared via `Rc<dyn Fn(gdk::Key) -> glib::Propagation>`
- Attach with `PropagationPhase::Capture` so child widgets don't swallow keys

---

## Seek / GStreamer known issues

See `memory/project_gstreamer_seek_research.md` for hard-won findings on seek-before-play failures and the muting hack.

---

## Defensive coding guidelines

### GTK String Safety
All strings passed to GTK APIs must be sanitized using `gtk_safe()`:
- Track/playlist metadata
- Any ID3 tag field/frame
- Error messages
- User input display
- M3U playlist names

The `gtk_safe()` function strips NUL bytes that cause `GStrInteriorNulError` panics:
```rust
fn gtk_safe(s: &str) -> String {
    if s.contains('\0') { s.replace('\0', "") } else { s.to_owned() }
}
```
### Large Dataset Handling
- Never block the UI thread with long operations
- Use batch inserts for database operations (100 items at a time)
- Show progress indicators for scans > 1000 items
- Open database connections inside background threads (SQLite connections are not `Send`)

### File Path Safety
- Always canonicalize file paths with `.canonicalize()` before storage
- Use fully qualified paths in playlists and media library
- Handle missing files gracefully with warning messages, not crashes


