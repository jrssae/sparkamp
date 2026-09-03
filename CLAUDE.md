# Sparkamp — CLAUDE.md

Working rules and conventions for this repository.

---

## Project overview

Winamp-style audio player for Linux/GNOME and macOS (Rust core, per-platform UI).

- TUI: Ratatui/crossterm (sparkamp --tui)

- GUI: GTK4 (sparkamp) on Linux, SwiftUI on macOS

- Engine: chosen behind the `AudioBackend` seam. GStreamer (playbin +
  equalizer-10bands + volume) on Linux and the TUI; AVFoundation
  (AVAudioEngine + AVAudioUnitEQ) on macOS, where no GStreamer is built or
  shipped at all. Core code calls the trait and never names either.

- Storage: TOML in ~/.config/sparkamp/; Playlists and settings restored between sessions.
---

## Mandatory Workflow
- Fail Fast: After 2 agent failures, stop and ask. Do not loop.

- Verification: Run cargo build && cargo test before completion. Zero warnings/failures allowed.

- Release: Confirm the intended version number with the user first (never assume a bump). Update Cargo.toml, README.md, and a metainfo `<release>` entry, then run `scripts/pre-release-check.sh <version>` before tagging — it refuses a forgotten bump and syncs the macOS `MARKETING_VERSION`. Verify Flatpak build ( packaging/ ).

- Deletion Rule: Permanently deleting a music file from disk is allowed ONLY from the Media Library file view or the Media Library external-device view, and ONLY after explicit user confirmation. Removing a track from the active playlist or any saved playlist must only remove it from that list — never delete the file from disk. Removing skins from the UI must not delete their files from disk.

- Refactoring: Ask before refactoring. Focus on requested changes; avoid over-engineering.

---

## Naming & Style

- User-facing: "Sparkamp" (Capital S, lowercase a).

- App ID (Linux/Flatpak): dev.sparkamp.Sparkamp. Names the `.desktop` file, the
  metainfo, the icons and the MPRIS identity.

- Bundle ID (macOS): com.sparkamp.sparkampmac. **Deliberately different**, and
  not a mistake to tidy up. Every released build has used it, an App ID and App
  Store record already exist against it, and an App Store record that has never
  been approved can never be deleted — so the identifier is permanent whatever
  we do. A bundle ID is invisible to users, so aligning the two would cost
  existing users their saved UI state to buy nothing they can see.

- Code: Keep existing casing (e.g., SparkampCtx).

- Docs: Plain English. Explain why, not what. Assume human reviewers and contributors.

---

## Architecture

- Core: UI-agnostic. UI communicates via public API only.

- Order: Core first -> TUI -> GUI.

- State: Always read code to verify state; do not trust summaries.

- macOS (Future): Keep Core ready for C FFI extraction into core/.

- Files: Core (src/), GTK (frontends/gtk/), TUI (frontends/tui/), macOS (frontends/SparkampMac/).

---

## Technical Specs

### Audio engine & EQ

- Limits (both backends): EQ bands ±12 dB (Max +12 to avoid panic); Pre-amp
  0.5–1.5×.

- Linux/TUI pipeline: playbin → volume (pre-amp) → equalizer-10bands. Silently
  no-ops if the GStreamer plugins are missing.

- macOS: AVAudioEngine → AVAudioUnitEQ → mixer. The EQ is part of the OS, so
  there is no "plugin missing" state. `clip_protection` and `album_mode` are
  inert on this backend; both are recorded in `set_normalization`.

- Do not add GStreamer to a macOS code path. It is declared under
  `cfg(not(target_os = "macos"))` and the shipped app links none of it.

### UI Specifics

- TUI EQ: Col 0-9 (Bands), Col 10 (Pre-amp). Nav: arrows/PgUp/PgDn.

- GTK Keys: u for EQ. Use PropagationPhase::Capture.

- Config: Use #[serde(default)] and Default impl for new fields.

- Skins: Built-in (skin.rs) vs User (~/.config/sparkamp/skins/ — shared verbatim by the macOS frontend; do not "fix" code to XDG data dir, skins are deliberately in config).

--- 

## Safety Guidelines

- GTK Strings: Use gtk_safe() to strip NUL bytes (\0) from metadata/errors.

- Performance: Batch DB inserts (100 items); background threads for long ops; SQLite is not Send.

- Paths: Always use .canonicalize(). Handle missing files gracefully.

---

## Agent skills

### Issue tracker

Issues live as GitHub issues in `jrssae/sparkamp`, driven by the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Domain docs

Single-context: one `CONTEXT.md` at the repo root plus `docs/adr/`. See `docs/agents/domain.md`.


