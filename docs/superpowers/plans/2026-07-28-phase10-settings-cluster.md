# Phase 10 — F11 Play-Count Threshold + F12 Niceties Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the "played" threshold configurable (F11) and add three Media-Library niceties (F12): remember-search-per-view, treat-artist-as-album-artist, and skip-DB-load-at-startup.

**Architecture:** F11 moves the currently-hardcoded 20 s gate out of each frontend into a pure core decision function (`play_counted_at`), exposed to frontends via a single FFI deadline helper plus config get/set pairs. F12 adds three config flags with core helpers where shared logic exists, then wires GTK + mac (blind) + TUI-where-its-settings-reach. Every flag persists to TOML via the existing `#[serde(default)]` idiom.

**Tech Stack:** Rust core (`src/`), GTK4 (`frontends/gtk/`), Ratatui TUI (`frontends/tui/`), macOS SwiftUI (`frontends/SparkampMac/`) via C FFI.

## Global Constraints

- **Build/test ONLY in distrobox `dev-box`:** `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`. Host builds fail (no gstreamer/gtk). NEVER gate on `cargo build --lib` — GTK/TUI compile only in the bin target; use full `cargo build`.
- **Zero warnings, zero test failures** before any task is "done".
- **KNOWN FLAKY TESTS** (parallel races, not regressions): `disc::burn::tests::run_tool_watchdog_kills_a_wedged_child` and `disc::detect::exclusive_read_tests::refcount_nesting_and_underflow`. If tripped, re-run the gate single-threaded (`-- --test-threads=1`).
- **New top-level `src/` modules** need `mod x;` in BOTH `src/lib.rs` AND `src/main.rs`.
- **RefCell borrows** never held across UI calls/callbacks (double-borrow = runtime panic). Extract Rc, drop the borrow, then invoke callbacks (see GTK player.rs:3729 idiom).
- **macOS is BLIND** (no Swift compiler): read whole files for real property names, mirror FFI byte-for-byte in `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`, and append verify items to `docs/mac-pass-checklist.md` in the SAME commit as the Swift/header change.
- **GTK Strings:** use `gtk_safe()` on any metadata/error string that reaches a widget.
- **Config:** new fields use `#[serde(default)]` + a `Default` impl so old TOML files still load.
- Commits end with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- Deletion rule unchanged: nothing here deletes files from disk.

## User Decisions (2026-07-28)

1. **F11 short files:** seconds-mode deadline = `min(seconds, length × 0.9)` so a track shorter than the threshold still counts near its end.
2. **F11 percent mode, unknown duration:** fall back to seconds mode for that track.
3. **skip_db_load + watcher:** watcher stays dormant until the DB is first opened by something else, then starts.

## File Structure

- `src/play_stats.rs` (NEW) — pure `play_counted_at` + `effective_album_artist` helpers, table-tested.
- `src/config.rs` (MODIFY) — `PlayStatsConfig` + `PlayStatsMode` under `PlaybackConfig`; three new `MediaLibraryConfig` fields.
- `src/ffi/settings.rs` (MODIFY) — get/set pairs + `sparkamp_play_deadline_secs`.
- `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` (MODIFY) — FFI mirror.
- `frontends/gtk/window/player.rs`, `settings.rs`, `media_library.rs` (MODIFY) — wiring + UI.
- `frontends/SparkampMac/Sources/SparkampModel.swift`, `SettingsWindow.swift` (MODIFY) — wiring + UI.
- `docs/mac-pass-checklist.md` (MODIFY) — phase-10 verify items.

---

### Task 1: F11 core — config + `play_counted_at`

**Files:**
- Modify: `src/config.rs` (add `PlayStatsMode`, `PlayStatsConfig`; add `play_stats` field to `PlaybackConfig` near line 131)
- Create: `src/play_stats.rs`
- Modify: `src/lib.rs`, `src/main.rs` (add `mod play_stats;` / `pub mod play_stats;` matching the existing module-declaration style in each)

**Interfaces:**
- Produces: `config::PlayStatsConfig { enabled: bool, mode: PlayStatsMode, seconds: u32, percent: u8 }`; `config::PlayStatsMode { Seconds, Percent }`; `play_stats::play_counted_at(length_secs: Option<f64>, cfg: &PlayStatsConfig) -> Option<f64>`.

- [ ] **Step 1: Add the config types to `src/config.rs`**

Place `PlayStatsMode` + `PlayStatsConfig` near the other Playback types, and add the field to `PlaybackConfig` (after `replaygain`, ~line 133):

```rust
/// How the "played" threshold is measured (F11). The user picks ONE active
/// mode; Winamp exposes both but only one applies at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PlayStatsMode {
    /// Count a play after N seconds of listening.
    #[default]
    Seconds,
    /// Count a play after a percentage of the track length.
    Percent,
}

/// Play-count threshold settings (F11). Lives under `[playback.play_stats]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayStatsConfig {
    /// When false, no play is ever recorded (snapshot stats still read fine).
    pub enabled: bool,
    /// Active measurement mode.
    pub mode: PlayStatsMode,
    /// Threshold in seconds (Seconds mode).
    pub seconds: u32,
    /// Threshold as a percent of track length, 1..=100 (Percent mode).
    pub percent: u8,
}

impl Default for PlayStatsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: PlayStatsMode::Seconds,
            seconds: 20,
            percent: 50,
        }
    }
}
```

Add to `PlaybackConfig`:

```rust
    /// Play-count threshold settings (F11).
    #[serde(default)]
    pub play_stats: PlayStatsConfig,
```

If `PlaybackConfig` has a hand-written `Default` impl (check — it does not use `#[serde(default)]` at the struct level for every field), add `play_stats: PlayStatsConfig::default(),` to it. If `PlaybackConfig` derives its default from `#[serde(default)]` only, no impl edit is needed — verify by reading the struct's current default source before editing.

- [ ] **Step 2: Write the failing test file `src/play_stats.rs`**

```rust
//! Pure decision helpers for play-count stats (F11) and album-artist
//! fallback (F12). No I/O — table-tested so every frontend agrees.

use crate::config::{PlayStatsConfig, PlayStatsMode};

/// Playback position (seconds) at which the current track counts as "played".
///
/// Returns `None` when stats are disabled — the caller must then never call
/// `record_play`.
///
/// Seconds mode: `min(cfg.seconds, length * 0.9)` when the length is known, so
/// a track shorter than the threshold still counts near its end (Winamp "reach
/// the threshold OR the file end"). Unknown length → `cfg.seconds` (no clamp).
///
/// Percent mode: `length * cfg.percent/100` when known; unknown length falls
/// back to seconds mode (`cfg.seconds`), per the 2026-07-28 user decision.
pub fn play_counted_at(length_secs: Option<f64>, cfg: &PlayStatsConfig) -> Option<f64> {
    if !cfg.enabled {
        return None;
    }
    let seconds = f64::from(cfg.seconds);
    match cfg.mode {
        PlayStatsMode::Seconds => Some(match length_secs {
            Some(len) if len > 0.0 => seconds.min(len * 0.9),
            _ => seconds,
        }),
        PlayStatsMode::Percent => match length_secs {
            Some(len) if len > 0.0 => Some(len * (f64::from(cfg.percent) / 100.0)),
            _ => Some(seconds),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PlayStatsConfig, PlayStatsMode};

    fn cfg(mode: PlayStatsMode, seconds: u32, percent: u8) -> PlayStatsConfig {
        PlayStatsConfig { enabled: true, mode, seconds, percent }
    }

    #[test]
    fn disabled_never_counts() {
        let mut c = cfg(PlayStatsMode::Seconds, 20, 50);
        c.enabled = false;
        assert_eq!(play_counted_at(Some(200.0), &c), None);
        assert_eq!(play_counted_at(None, &c), None);
    }

    #[test]
    fn seconds_mode_normal_track() {
        let c = cfg(PlayStatsMode::Seconds, 20, 50);
        assert_eq!(play_counted_at(Some(200.0), &c), Some(20.0));
    }

    #[test]
    fn seconds_mode_clamps_short_track_to_90pct() {
        let c = cfg(PlayStatsMode::Seconds, 20, 50);
        // 15 s jingle: 20 > 15*0.9 = 13.5 → count at 13.5 s.
        assert_eq!(play_counted_at(Some(15.0), &c), Some(13.5));
    }

    #[test]
    fn seconds_mode_unknown_length_uses_raw_seconds() {
        let c = cfg(PlayStatsMode::Seconds, 20, 50);
        assert_eq!(play_counted_at(None, &c), Some(20.0));
    }

    #[test]
    fn percent_mode_half_of_200() {
        let c = cfg(PlayStatsMode::Percent, 20, 50);
        assert_eq!(play_counted_at(Some(200.0), &c), Some(100.0));
    }

    #[test]
    fn percent_mode_unknown_length_falls_back_to_seconds() {
        let c = cfg(PlayStatsMode::Percent, 20, 50);
        assert_eq!(play_counted_at(None, &c), Some(20.0));
    }
}
```

- [ ] **Step 3: Register the module** — add `pub mod play_stats;` to `src/lib.rs` and `mod play_stats;` to `src/main.rs`, matching each file's existing declaration style/placement.

- [ ] **Step 4: Run tests**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo test play_stats'`
Expected: 6 tests pass, 0 warnings.

- [ ] **Step 5: Full gate + commit**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: green, 0 warnings.

```bash
git add src/config.rs src/play_stats.rs src/lib.rs src/main.rs
git commit -m "feat(phase10): configurable play-count threshold core (F11)"
```

---

### Task 2: F11 FFI — deadline helper + config get/set

**Files:**
- Modify: `src/ffi/settings.rs` (add after the existing watch-folder get/set block, ~line 369)
- Modify: `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` (mirror, in the settings section)
- Modify: `docs/mac-pass-checklist.md` (phase-10 section)

**Interfaces:**
- Consumes: `play_stats::play_counted_at`, `config::PlayStatsConfig/PlayStatsMode`.
- Produces (C ABI): `double sparkamp_play_deadline_secs(const SparkampCtx *ctx, double length_secs)` (`length_secs <= 0` = unknown; returns `-1.0` when disabled/null); `bool sparkamp_get_play_stats_enabled/set`; `uint32_t sparkamp_get_play_stats_mode` (0=seconds,1=percent) `/set`; `uint32_t sparkamp_get_play_stats_seconds/set`; `uint32_t sparkamp_get_play_stats_percent/set`.

- [ ] **Step 1: Add the FFI functions to `src/ffi/settings.rs`**

Mirror the exact idiom of the existing `sparkamp_get_auto_add_played`/`sparkamp_set_auto_add_played` pair (read those first for the `ctx.as_ref()`/`ctx.as_mut()` + `save_config` idiom — the setters in this file persist config; confirm whether the neighbor setters call a save helper and match it).

```rust
/// Position (seconds) at which the current track should be counted as played,
/// given its length (`length_secs <= 0` means unknown). Returns `-1.0` when
/// play-stats are disabled or `ctx` is null — the caller then never records.
#[no_mangle]
pub unsafe extern "C" fn sparkamp_play_deadline_secs(
    ctx: *const SparkampCtx,
    length_secs: f64,
) -> f64 {
    let ctx = match ctx.as_ref() {
        Some(c) => c,
        None => return -1.0,
    };
    let len = if length_secs > 0.0 { Some(length_secs) } else { None };
    crate::play_stats::play_counted_at(len, &ctx.config.playback.play_stats).unwrap_or(-1.0)
}

#[no_mangle]
pub unsafe extern "C" fn sparkamp_get_play_stats_enabled(ctx: *const SparkampCtx) -> bool {
    let ctx = match ctx.as_ref() { Some(c) => c, None => return true };
    ctx.config.playback.play_stats.enabled
}

#[no_mangle]
pub unsafe extern "C" fn sparkamp_set_play_stats_enabled(ctx: *mut SparkampCtx, value: bool) {
    let ctx = match ctx.as_mut() { Some(c) => c, None => return };
    ctx.config.playback.play_stats.enabled = value;
    // match the neighbor setters' persistence call here
}

#[no_mangle]
pub unsafe extern "C" fn sparkamp_get_play_stats_mode(ctx: *const SparkampCtx) -> u32 {
    let ctx = match ctx.as_ref() { Some(c) => c, None => return 0 };
    match ctx.config.playback.play_stats.mode {
        crate::config::PlayStatsMode::Seconds => 0,
        crate::config::PlayStatsMode::Percent => 1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn sparkamp_set_play_stats_mode(ctx: *mut SparkampCtx, value: u32) {
    let ctx = match ctx.as_mut() { Some(c) => c, None => return };
    ctx.config.playback.play_stats.mode = if value == 1 {
        crate::config::PlayStatsMode::Percent
    } else {
        crate::config::PlayStatsMode::Seconds
    };
    // persist
}

#[no_mangle]
pub unsafe extern "C" fn sparkamp_get_play_stats_seconds(ctx: *const SparkampCtx) -> u32 {
    let ctx = match ctx.as_ref() { Some(c) => c, None => return 20 };
    ctx.config.playback.play_stats.seconds
}

#[no_mangle]
pub unsafe extern "C" fn sparkamp_set_play_stats_seconds(ctx: *mut SparkampCtx, value: u32) {
    let ctx = match ctx.as_mut() { Some(c) => c, None => return };
    ctx.config.playback.play_stats.seconds = value.max(1);
    // persist
}

#[no_mangle]
pub unsafe extern "C" fn sparkamp_get_play_stats_percent(ctx: *const SparkampCtx) -> u32 {
    let ctx = match ctx.as_ref() { Some(c) => c, None => return 50 };
    u32::from(ctx.config.playback.play_stats.percent)
}

#[no_mangle]
pub unsafe extern "C" fn sparkamp_set_play_stats_percent(ctx: *mut SparkampCtx, value: u32) {
    let ctx = match ctx.as_mut() { Some(c) => c, None => return };
    ctx.config.playback.play_stats.percent = value.clamp(1, 100) as u8;
    // persist
}
```

Replace the `// persist` comments with whatever persistence call the neighboring setters in this file use (read them first — do not invent a new save path).

- [ ] **Step 2: Write a unit test in `src/ffi/settings.rs`** covering the deadline helper against a null ctx and (if the file's tests construct a `SparkampCtx`) a disabled-config case. If no ctx test harness exists in this file, add only the null case:

```rust
#[test]
fn play_deadline_null_ctx_is_negative() {
    unsafe {
        assert_eq!(sparkamp_play_deadline_secs(std::ptr::null(), 200.0), -1.0);
    }
}
```

- [ ] **Step 3: Mirror the 9 signatures in `sparkamp_bridge.h`** in the settings section, matching the existing formatting:

```c
double   sparkamp_play_deadline_secs(const SparkampCtx *ctx, double length_secs);
bool     sparkamp_get_play_stats_enabled(const SparkampCtx *ctx);
void     sparkamp_set_play_stats_enabled(SparkampCtx *ctx, bool value);
uint32_t sparkamp_get_play_stats_mode(const SparkampCtx *ctx);
void     sparkamp_set_play_stats_mode(SparkampCtx *ctx, uint32_t value);
uint32_t sparkamp_get_play_stats_seconds(const SparkampCtx *ctx);
void     sparkamp_set_play_stats_seconds(SparkampCtx *ctx, uint32_t value);
uint32_t sparkamp_get_play_stats_percent(const SparkampCtx *ctx);
void     sparkamp_set_play_stats_percent(SparkampCtx *ctx, uint32_t value);
```

- [ ] **Step 4: Append phase-10 F11 items to `docs/mac-pass-checklist.md`** (deadline helper reachable; settings controls wire to get/set — the actual mac UI verify lands in Task 4, but note the FFI surface here).

- [ ] **Step 5: Gate + commit**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: green, 0 warnings.

```bash
git add src/ffi/settings.rs frontends/SparkampMac/SparkampCore/sparkamp_bridge.h docs/mac-pass-checklist.md
git commit -m "feat(phase10): F11 FFI deadline + play-stats config get/set"
```

---

### Task 3: F11 GTK — wire deadline + settings UI

**Files:**
- Modify: `frontends/gtk/window/player.rs` (replace the hardcoded gate at ~3725–3754)
- Modify: `frontends/gtk/window/settings.rs` (add a Playback-tab group near the existing watch/auto-add checkbuttons, ~1996–2020)

**Interfaces:**
- Consumes: `sparkamp::play_stats::play_counted_at`, `config.playback.play_stats`.

- [ ] **Step 1: Replace the hardcoded 20 s gate in `player.rs`**

Read the full tick block (3690–3760) first to find the current track's length source (the tick computes a gst `dur`/`duration` — use it; convert to `Option<f64>` seconds, `None` if the duration is zero/unknown). Replace `if pos >= Duration::from_secs(20)` with a config-driven deadline:

```rust
                let track_len = /* dur as Option<f64> seconds, None if unknown */;
                let deadline = sparkamp::play_stats::play_counted_at(
                    track_len,
                    &s.config.playback.play_stats,
                );
                let crossed = deadline
                    .map(|dl| pos.as_secs_f64() >= dl)
                    .unwrap_or(false);
                if crossed {
                    // ... existing record_play + counted_play_path guard ...
```

Keep the existing `counted_play_path` de-dupe guard and the RefCell-safe `rebuild_ml_callback` extraction exactly as-is. When `deadline` is `None` (stats disabled), `crossed` is false → no record_play, as specified.

- [ ] **Step 2: Add the settings UI group in `settings.rs`**

Mirror the existing `chk_watch` / `chk_auto_add` idiom (read 1990–2025). Add to the Playback tab:
- `chk_play_stats` checkbutton "Count plays" bound to `config.playback.play_stats.enabled`.
- A mode chooser (two radio buttons "After N seconds" / "After N% of track") bound to `mode`.
- A seconds `SpinButton` (range 1–3600) bound to `seconds`.
- A percent `SpinButton` (range 1–100) bound to `percent`.

Each `connect_*` handler mutates `s.config.playback.play_stats.*` and saves config the same way the neighbors do. Grey out / desensitize the seconds vs percent spin to match the active mode (optional polish — follow existing sensitivity patterns if present, else leave both active).

- [ ] **Step 3: Gate + commit**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`
Expected: green, 0 warnings. (No new automated test — this is UI wiring over the Task-1 pure fn already covered. The reviewer verifies the deadline replaces the hardcode and the guard is intact.)

```bash
git add frontends/gtk/window/player.rs frontends/gtk/window/settings.rs
git commit -m "feat(phase10): F11 GTK deadline wiring + settings UI"
```

---

### Task 4: F11 mac (blind) — wire deadline + settings UI

**Files:**
- Modify: `frontends/SparkampMac/Sources/SparkampModel.swift` (~330 constant, ~488 gate)
- Modify: `frontends/SparkampMac/Sources/SettingsWindow.swift`
- Modify: `docs/mac-pass-checklist.md`

**Interfaces:**
- Consumes: `sparkamp_play_deadline_secs`, the four play-stats get/set FFI pairs.

- [ ] **Step 1: Replace `playCountThresholdSecs` in `SparkampModel.swift`**

Delete the `private let playCountThresholdSecs: Double = 20.0` (line ~330). At the gate (~488) compute the deadline per-track from the FFI:

```swift
let deadline = sparkamp_play_deadline_secs(ctx, duration)  // duration in secs, <=0 if unknown
if isPlaying, idx >= 0, deadline >= 0, pos >= deadline {
    // ... existing record_play + countedPlayPath guard unchanged ...
}
```

`deadline < 0` means disabled → never record. Keep the `countedPlayPath` gate and the `mlReloadTrigger` nudge exactly as-is. Read the surrounding tick to confirm `duration` is the current track length in seconds (it is published at ~444).

- [ ] **Step 2: Add settings controls in `SettingsWindow.swift`**

Mirror an existing bool/stepper settings row (read the file for the established Toggle/Picker/Stepper idiom and the get/set call pattern). Add a "Count plays" toggle (`sparkamp_get/set_play_stats_enabled`), a mode Picker (Seconds/Percent → `..._mode` 0/1), a seconds stepper (`..._seconds`, 1–3600), and a percent stepper (`..._percent`, 1–100). Use the file's real property/binding names — do not invent SwiftUI state you haven't confirmed exists.

- [ ] **Step 3: Append phase-10 F11 mac verify items to `docs/mac-pass-checklist.md`** (toggle off → counts freeze; seconds=5 counts at 5 s; percent=50 on 4-min track counts past 2:00; short track counts near end).

- [ ] **Step 4: Commit** (mac is blind — no build; verify by reading. Run the Rust gate to confirm nothing else regressed.)

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`

```bash
git add frontends/SparkampMac/Sources/SparkampModel.swift frontends/SparkampMac/Sources/SettingsWindow.swift docs/mac-pass-checklist.md
git commit -m "feat(phase10): F11 mac deadline wiring + settings UI (blind)"
```

---

### Task 5: F12.1 — remember search query per view

**Files:**
- Modify: `src/config.rs` (add two `MediaLibraryConfig` fields near line 716)
- Modify: `src/ffi/settings.rs` (flag get/set + per-view last-search get/set)
- Modify: `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`
- Modify: `frontends/gtk/window/media_library.rs` (restore on view open, persist on change)
- Modify: `frontends/SparkampMac/Sources/SparkampModel.swift` + the ML search view (blind mirror)
- Modify: `docs/mac-pass-checklist.md`

**Interfaces:**
- Produces: `config.media_library.remember_search: bool` (default false); `config.media_library.last_search: HashMap<String,String>` (view-id → query); FFI `bool sparkamp_get_remember_search/set`; `char *sparkamp_get_last_search(ctx, view_id)` (empty string if none; caller frees); `void sparkamp_set_last_search(ctx, view_id, query)`.

- [ ] **Step 1: Add config fields** to `MediaLibraryConfig`:

```rust
    /// When true, restore each Media-Library view's search query when the view
    /// is next opened (F12). When false, the search box clears each open.
    #[serde(default)]
    pub remember_search: bool,

    /// Last search query per view id ("files"/"playlists"/"devices"/"discs").
    /// Only consulted when `remember_search` is true.
    #[serde(default)]
    pub last_search: std::collections::HashMap<String, String>,
```

- [ ] **Step 2: FFI in `src/ffi/settings.rs`** — bool get/set for `remember_search` (mirror `auto_add_played`), plus:

```rust
#[no_mangle]
pub unsafe extern "C" fn sparkamp_get_last_search(
    ctx: *const SparkampCtx,
    view_id: *const c_char,
) -> *mut c_char {
    // as_ref guard; read view_id CStr; look up config.media_library.last_search;
    // return CString::new(query).into_raw() or an empty-string CString.
    // Follow the existing string-returning FFI idiom in this crate for null
    // handling + allocation (find one that returns *mut c_char and copy it).
}

#[no_mangle]
pub unsafe extern "C" fn sparkamp_set_last_search(
    ctx: *mut SparkampCtx,
    view_id: *const c_char,
    query: *const c_char,
) {
    // as_mut guard; read both CStrs; insert into the map; persist.
}
```

Read an existing `*mut c_char`-returning FFI (e.g. in `src/ffi/disc.rs` `json_out`) to copy the exact allocation + null idiom, and confirm the string is freed via the crate's existing `sparkamp_free_string`.

- [ ] **Step 3: Add a config-roundtrip test** in `src/config.rs` tests (or `src/ffi/settings.rs`): set `remember_search=true`, insert `"files" -> "beatles"`, serialize→deserialize→assert the map survives.

- [ ] **Step 4: Mirror the 4 signatures in `sparkamp_bridge.h`.**

- [ ] **Step 5: GTK wiring in `media_library.rs`** — when a view's search entry is built/shown, if `remember_search` is on, prefill it from `config.media_library.last_search[view_id]` and apply the filter. On search-text change, write back to the map + save config (debounced or on-change per the file's existing save idiom). When `remember_search` is off, clear as today. Add a "Remember search per view" toggle to the ML settings/config surface where the other ML toggles live.

- [ ] **Step 6: mac blind mirror** — `SparkampModel` reads/writes via the FFI on ML-view appear/search-change; add the toggle to `SettingsWindow.swift`. Append checklist items.

- [ ] **Step 7: Gate + commit**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`

```bash
git add -A
git commit -m "feat(phase10): F12 remember search query per view"
```

---

### Task 6: F12.2 — treat artist as album artist

**Files:**
- Modify: `src/config.rs` (`MediaLibraryConfig.artist_as_album_artist: bool`)
- Modify: `src/play_stats.rs` (add `effective_album_artist` + tests — the file is the shared pure-helper home)
- Modify: `src/ffi/settings.rs` (bool get/set)
- Modify: `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`
- Modify: GTK ML column render + mac ML column render (where album_artist is consulted)
- Modify: `docs/mac-pass-checklist.md`

**Interfaces:**
- Produces: `config.media_library.artist_as_album_artist: bool` (default false); `play_stats::effective_album_artist(artist: &str, album_artist: &str, artist_as_album: bool) -> String`; FFI `bool sparkamp_get_artist_as_album_artist/set`.

- [ ] **Step 1: Add config field** (`#[serde(default)]`, default false) to `MediaLibraryConfig`.

- [ ] **Step 2: Add the helper + tests to `src/play_stats.rs`**

```rust
/// The album-artist to display/group by. When `artist_as_album` is true and
/// the track has no album-artist tag, fall back to the artist (F12). Trims so
/// whitespace-only tags count as empty.
pub fn effective_album_artist(artist: &str, album_artist: &str, artist_as_album: bool) -> String {
    if !album_artist.trim().is_empty() {
        album_artist.to_string()
    } else if artist_as_album {
        artist.to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod album_artist_tests {
    use super::effective_album_artist;

    #[test]
    fn prefers_album_artist_when_present() {
        assert_eq!(effective_album_artist("A", "AA", true), "AA");
        assert_eq!(effective_album_artist("A", "AA", false), "AA");
    }

    #[test]
    fn falls_back_to_artist_only_when_enabled() {
        assert_eq!(effective_album_artist("A", "", true), "A");
        assert_eq!(effective_album_artist("A", "   ", true), "A");
        assert_eq!(effective_album_artist("A", "", false), "");
    }

    #[test]
    fn neither_present() {
        assert_eq!(effective_album_artist("", "", true), "");
    }
}
```

- [ ] **Step 3: FFI bool get/set** for `artist_as_album_artist` (mirror `auto_add_played`).

- [ ] **Step 4: Mirror the 2 signatures in `sparkamp_bridge.h`.**

- [ ] **Step 5: Apply at the ML album-artist surfaces** — find where the album-artist column / grouping reads `album_artist` (GTK `media_library.rs`, mac ML column view) and route it through the fallback (GTK via the core helper; mac via the FFI flag + inline equivalent). Add the toggle to the ML settings surface (GTK + mac `SettingsWindow.swift`). Note in a comment that A4 (phase 11) MUST also use this helper.

- [ ] **Step 6: Append checklist items + Gate + commit**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`

```bash
git add -A
git commit -m "feat(phase10): F12 treat artist as album artist"
```

---

### Task 7: F12.3 — skip database load at startup

**Files:**
- Modify: `src/config.rs` (`MediaLibraryConfig.skip_db_load: bool`)
- Modify: `src/ffi/settings.rs` (bool get/set)
- Modify: `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`
- Modify: GTK startup (`state.rs` / app init) + `watch.rs`; mac `SparkampModel.swift` init
- Modify: `docs/mac-pass-checklist.md`

**Interfaces:**
- Produces: `config.media_library.skip_db_load: bool` (default false); FFI `bool sparkamp_get_skip_db_load/set`.

- [ ] **Step 1: Add config field** (`#[serde(default)]`, default false).

- [ ] **Step 2: FFI bool get/set** (mirror `auto_add_played`) + mirror the 2 signatures in `sparkamp_bridge.h`.

- [ ] **Step 3: Defer DB open at startup**

Read how each frontend opens the ML DB today (GTK `state.rs` app init constructs `media_lib`; mac `SparkampCtx` init). When `skip_db_load` is on:
- Do NOT open/load the ML DB at startup.
- Open it lazily on first demand: ML window open, device sync, or the watcher's first need.
- Play paths that opportunistically read the DB (the snapshot-stats read, `record_play`, `note_played`/auto-add) must tolerate a not-yet-open DB by treating it as `None`/skip — no crash. Verify each `media_lib` access site is already `Option`-guarded (GTK `s.media_lib` is `Option`; mac guards on `ctx`), and that a skipped open leaves them `None` rather than panicking.

- [ ] **Step 4: Watcher-on-first-open (user decision 3)**

Under `skip_db_load`, the folder watcher (phase 8) must NOT force the DB open at startup. Wire it so the watcher starts when the DB is first opened by another path. Read `watch.rs` (GTK) + the mac watcher init: gate `watch_rebuild`/watcher-start behind "DB is open", and trigger a watcher start at the lazy-open site.

- [ ] **Step 5: Add the toggle** to the ML settings surface (GTK + mac `SettingsWindow.swift`).

- [ ] **Step 6: Test** — add a unit test proving a pre-open DB read path returns gracefully (unit the accessor's `None` branch if one exists; otherwise a config-roundtrip test for the flag). Append checklist items (cold start faster; first ML open loads normally; play-count works after ML opens; A1 stats show em-dashes before ML ever opens — no crash).

- [ ] **Step 7: Gate + commit**

Run: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`

```bash
git add -A
git commit -m "feat(phase10): F12 skip database load at startup"
```

---

## Automated test coverage (summary)

- `play_counted_at`: disabled→None; seconds normal/clamped-short/unknown-length; percent 50%-of-200=100; percent unknown→seconds fallback. (Task 1)
- `sparkamp_play_deadline_secs`: null ctx → -1. (Task 2)
- `effective_album_artist`: album-artist present (both flag states); artist fallback only when enabled; whitespace-only tag = empty; neither present. (Task 6)
- Config roundtrips: `last_search` map survives serialize/deserialize (Task 5); flags default correctly (all tasks).

## Manual test plan (human, post-merge)

1. Seconds=5: skip at 3 s → count unchanged; at 6 s → +1 once per play. Percent=50 on a 4-min track → counts only past 2:00. Short (15 s) track → counts near its end.
2. Stats toggle OFF → counts/last-played frozen; A1 panel still shows old values.
3. Remember-search ON: type in Files view, close/reopen ML → query + filter restored, per-view independent. OFF → clean each open.
4. Album-artist fallback ON: compilation-free library → ML album-artist column/grouping shows artist where blank.
5. skip_db_load ON: cold start visibly faster with a big library; first ML open loads normally; play-count still works after ML opens; A1 stats show em-dashes before ML ever opened (acceptable — verify no crash).
6. mac + TUI settings walk.

## TUI note

TUI has NO play-count seam today (no `record_play` call) and no per-view search-persistence surface, so F11 timer wiring and F12.1 are GTK + mac only. If the TUI settings screen surfaces these configs, plumb the toggles there; otherwise TUI inherits the config values passively (they persist in TOML). Do not invent a TUI play-count timer in this phase.
