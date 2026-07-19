# Phase 6 — F9 Shortcuts + Dialog Sweep (design-level plan)

> Read `2026-07-19-opus-handoff.md` first. After phases 2 & 5 so the dialog
> sweep documents w/k/q too. Every binding: GTK + mac + TUI where surface
> exists; 3-file sync rule per key.

**Goal:** Close the approved shortcut gaps and make the shortcuts dialog the
single source of truth.

## Bindings (decided values)

| Key | Action | Notes |
|-----|--------|-------|
| `m` | Toggle Media Library window | GTK + mac (ML has no key today). Route through the ML button's click handler (btn pattern like `u`/EQ from phase 0). |
| `↑` / `↓` | Volume up/down 5% | GTK MAIN WINDOW only (mac already has it). CONTEXT CARE: playlist window keeps native ↑/↓ browse (dialog documents the split); implement in `handle_key` but gate on which window has focus, or attach to the main window's controller only — pick at expansion after reading how the shared handler attaches; the jump window's own capture controller already owns its arrows. |
| `Enter` | Play selected playlist row | GTK (mac has Return). Playlist-window capture controller (same vehicle as phase 5's `q`). |
| `n` | Add file(s) | Already GTK; ADD to mac + TUI. |
| `Shift+N` | Add folder | GTK + mac + TUI (today: none have it; GTK n = files only). |
| `t` | Stop after current track | USER-DECIDED KEY. Engine flag `stop_after_current: bool` (not persisted); checked at the advance seam BEFORE the queue (phase 5 precedence: stop-flag → queue → shuffle/linear); flag clears after firing or on manual stop/play. Visual: status-label text + mode-button-style feedback optional (propose status text "Stopping after current" toggle). |
| `Ctrl+S` | Save playlist | Accelerator on the existing Save button. |
| `Ctrl+.` | Open Settings | GTK (mac gets ⌘, free). |
| `Ctrl+I` | Invert playlist selection | GTK + mac; native select-all already exists. |

## Dialog sweep (source of truth)

Rebuild the `sections` array against reality: every binding above + w/k
(phase 2) + q's dual meaning (phase 5) + anything drifted. Then mirror the
COMPLETE list to `KeyboardShortcutsView.swift` and verify the mac handler
matches. TUI help screen (if it lists keys — check) gets the TUI-applicable
set. Acceptance: a person can read the dialog and every line works exactly
as written, on both platforms.

## Architecture notes

- Stop-after-current lives in core (engine/controller — beside the advance
  logic, `shuffle.rs`-adjacent), exposed to mac via
  `sparkamp_set/get_stop_after_current` FFI (bridge.h) so the mac key and
  any menu item share state.
- Invert selection: GTK ListBox multi-select — iterate rows, toggle
  selected state (selection mode is Multiple already for Delete-selection;
  verify). mac: NSTableView selection manipulation in the key handler.
- Volume ↑/↓: reuse the existing `-`/`=` arms' logic (extract shared fn if
  they're inline duplicates — small altitude fix while touching).

## Automated tests

- Stop-after-current: unit at the advance seam — flag set → advance
  returns stop (no next track), flag cleared after; precedence over a
  non-empty queue; cleared by manual play.
- Invert-selection model fn if extracted pure (indices in → complement
  out); otherwise UI-only, manual.
- Dialog accuracy: a test asserting the `sections` array contains entries
  for every key in a canonical list (keeps future drift honest —
  const KEYS list in the test, update deliberately).

## Manual test plan

1. Every table row above: press, observe, on GTK and mac.
2. `t` mid-song → song finishes, playback stops, flag indicator clears;
   pressing `t` twice = toggle off (song continues to next).
3. `t` with queued tracks → stops before the queue continues; next play
   resumes the queue (phase-5 interplay).
4. ↑/↓ on main window = volume; ↑/↓ in playlist = row browse; jump window
   arrows unaffected.
5. Ctrl+S saves; Ctrl+. opens settings; Ctrl+I inverts a partial selection.
6. Dialog read-through: every line true; mac help identical content.
7. TUI: n / Shift+N add file/folder.

## Open questions

1. `t` visual feedback: status-label text only (proposed) vs a lit mode
   button. Ask user.
2. Shift+N on TUI: the TUI add-flow may only have one entry path — confirm
   folder-picker capability exists or note capability gap.
