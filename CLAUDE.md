# Sparkamp — CLAUDE.md

Working rules and conventions for this repository.

---

## Project overview

Winamp-style audio player for Linux/GNOME (Rust).

- TUI: Ratatui/crossterm (sparkamp)

- GUI: GTK4 (sparkamp --ui)

- Engine: GStreamer playbin + equalizer-10bands + volume

- Plugins: C-compatible .so (ABI v2)

- Storage: TOML in ~/.config/sparkamp/; Playlists and settings restored between sessions.
---

## Mandatory Workflow
- Fail Fast: After 2 agent failures, stop and ask. Do not loop.

- Verification: Run cargo build && cargo test before completion. Zero warnings/failures allowed.

- Release: Update README.md and verify Flatpak build ( packaging/ ).

- Deletion Rule: Removing skins/plugins/music from UI must not delete files from disk.

- Refactoring: Ask before refactoring. Focus on requested changes; avoid over-engineering.

---

## Naming & Style

- User-facing: "Sparkamp" (Capital S, lowercase a).

- App ID: dev.sparkamp.Sparkamp.

- Code: Keep existing casing (e.g., SparkPluginAbi).

- Docs: Plain English. Explain why, not what. Assume human reviewers and contributors.

---

## Architecture

- Core: UI-agnostic. UI communicates via public API only.

- Order: Core first -> TUI -> GUI.

- State: Always read code to verify state; do not trust summaries.

- macOS (Future): Keep Core ready for C FFI extraction into core/.

- Files: Core (src/), GTK (frontends/gtk/), TUI (frontends/tui/), Plugins (plugins/).

---

## Technical Specs

### GStreamer & EQ

- Pipeline: playbin → volume (pre-amp) → equalizer-10bands.

- Limits: EQ bands ±12 dB (Max +12 to avoid panic); Pre-amp 0.5–1.5×.

- Behavior: Silently no-op if GStreamer plugins are missing.

### UI Specifics

- TUI EQ: Col 0-9 (Bands), Col 10 (Pre-amp). Nav: arrows/PgUp/PgDn.

- GTK Keys: u for EQ. Use PropagationPhase::Capture.

- Config: Use #[serde(default)] and Default impl for new fields.

- Skins: Built-in (skin.rs) vs User (~/.local/share/sparkamp/skins/).

--- 

## Safety Guidelines

- GTK Strings: Use gtk_safe() to strip NUL bytes (\0) from metadata/errors.

- Performance: Batch DB inserts (100 items); background threads for long ops; SQLite is not Send.

- Paths: Always use .canonicalize(). Handle missing files gracefully.


