# Phase 8 — F10 Watch Folders & Scan Behaviors Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> Read `docs/superpowers/plans/2026-07-19-opus-handoff.md` and the design doc
> `docs/superpowers/plans/2026-07-19-phase8-watch-folders.md` first. This file
> is the execution expansion of that design doc, with re-verified anchors as of
> 2026-07-27.

**Goal:** Make the library live — a filesystem watcher picks up new/changed/removed audio files under watched folders; plus rescan-on-startup, auto-add-played, remove-missing toggle, per-folder recurse, and compact-on-rescan.

**Architecture:** New core module `src/watch.rs` splits into (a) a **pure** event classifier + self-write suppression registry (fully unit-tested, no OS) and (b) a thin debounced `notify` OS-watcher wrapper that emits `WatchAction`s on a channel. The DB apply layer routes actions through the existing production scan seam (`rescan_folder_fast` fast-insert → `upsert_track`). Frontends drain watcher events on their existing main-loop/tick and reuse scan-completion refresh callbacks. Watching runs in core so TUI benefits (gio FileMonitor rejected — ties core to glib).

**Tech Stack:** Rust, `notify` v6 + `notify-debouncer-mini`, rusqlite, GTK4, Ratatui, SwiftUI (blind).

## Global Constraints

- Build/test ONLY inside distrobox: `distrobox enter dev-box -- sh -c 'cd ~/Code/Sparkamp && cargo build && cargo test'`. Host builds fail (no gstreamer/gtk dev libs). NEVER gate on `cargo build --lib` — GTK code only compiles in the bin target.
- Zero warnings, zero failures before any "done" claim. Quote BOTH `test result:` lines (lib + bin). Current floor to beat: **505 lib + 711 bin** passed.
- New `src/` modules need `mod x;` in BOTH `src/lib.rs` AND `src/main.rs`.
- Known pre-existing flaky test (NOT a regression): `disc::detect::exclusive_read_tests::refcount_nesting_and_underflow` — confirm green with `cargo test -- --test-threads=1`.
- All work lands on branch `album-art-improvements` (no per-phase branch, no merge). NEVER push without a fresh explicit user instruction.
- GTK strings via `gtk_safe()`. Config fields use `#[serde(default)]` + `Default`. Paths `.canonicalize()`; handle missing files gracefully.
- macOS Swift is BLIND (no compiler): read whole files before editing; every new/changed C-visible FFI symbol hand-mirrored byte-for-byte in `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`; verification items appended to `docs/mac-pass-checklist.md` in the SAME commit.
- Keyboard/settings parity: GTK + mac full capability parity on every item; TUI wherever its surface reaches. GTK formatting is the parity reference.
- Comments: plain English, why-not-what. Casing "Sparkamp". Commits: conventional prefix + why + a verification line + `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- SQLite is not Send — all DB work stays on its thread; frontends get results via existing channel/callback patterns. User library ≈ 36k tracks: per-track×N work must be background, batched (100-row batching exists).
- Production scan seam (load-bearing): `rescan_folder_fast` (path-only rows) then `scan_all_folders → scan_folder → needs_metadata_scan`. `upsert_track` is the single metadata write seam. `rescan_folder_metadata` is TEST-ONLY — do not wire it into production.

## Pre-Flight Notes (resolved before execution)

1. **User decisions (2026-07-27):** watch-folders master toggle **defaults ON** (supersedes Winamp interval polling); auto-add-played tracks land in a **folder_id NULL bucket** — verified safe: `queries.rs::all_tracks_sorted` is `FROM tracks` with no folder JOIN, so NULL-folder rows display in the Files view. No view fix needed.
2. **Vestigial periodic-rescan UI:** `config::MediaLibraryConfig` already has `periodic_rescan` + `rescan_interval_mins` with a TUI settings row and FFI get/set for the interval, but **no timer runs it anywhere** — it is dead. Per the settled roadmap decision (true watching, not interval polling), Task 11 (TUI) **replaces** the periodic-rescan + interval rows with the new watch-folders toggle. The config fields are **retained** (kept `#[serde(default)]`, doc-commented as deprecated) for TOML back-compat — do NOT delete them (removal would break existing user configs' round-trip and the FFI symbols). `rescan_on_startup` already exists and is wired in TUI (`frontends/tui/mod.rs:585`); this plan adds its GTK + mac startup triggers and settings rows.
3. `folders` table today: `id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE` (`src/media_library/mod.rs:448`). `tracks.folder_id` is nullable already (`REFERENCES folders(id)`, no NOT NULL) — the NULL bucket needs no schema change to `tracks`.

---

## File Structure

- **Create** `src/watch.rs` — core watcher. Two halves: pure classifier + `SelfWriteGuard`; and the `FolderWatcher` OS wrapper (notify + debouncer). Soft-cap ~800 lines; if the OS wrapper grows, its integration test can live in `src/watch/tests.rs` via `#[path]`.
- **Modify** `Cargo.toml` — add `notify` + `notify-debouncer-mini`.
- **Modify** `src/lib.rs`, `src/main.rs` — `mod watch;`.
- **Modify** `src/media_library/mod.rs` — `folders.recurse` migration (mirror the `pragma_table_info('tracks')` guard for `folders`); apply-action entry point.
- **Modify** `src/media_library/scan.rs` — `walk_dir` honors recurse; `add_folder` default recurse; `folder_recurse`/`set_folder_recurse`; VACUUM after full rescan; remove-missing branch; `apply_watch_action`; `add_played_track`.
- **Modify** `src/config.rs` — new fields `watch_folders` (default true), `auto_add_played`, `remove_missing_on_rescan`, `compact_on_rescan`; deprecate periodic fields in comments only.
- **Modify** `src/id3_editor.rs`, `src/replaygain.rs`, and the artwork/folder-image write site — call `crate::watch::register_self_write(path)` at each write.
- **Modify** `src/ffi/settings.rs`, `src/ffi/media_library.rs`, `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h` — FFI get/set for 4 toggles + per-folder recurse + a watcher-event poll mirroring the scan-progress poll.
- **Modify** `frontends/gtk/window/settings.rs`, `frontends/gtk/window/media_library.rs`, GTK app init — settings rows, recurse checkbox, event drain, startup rescan.
- **Modify** `frontends/tui/settings_eq.rs`, `frontends/tui/ui/settings_eq.rs`, `frontends/tui/mod.rs` — replace periodic rows with watch toggle + others; ML view refresh.
- **Modify** mac `SparkampModel+MediaLibrary.swift`, settings view, folder list view — toggles, recurse checkbox, event polling, startup rescan.
- **Modify** `docs/mac-pass-checklist.md`, `docs/superpowers/specs/2026-07-17-winamp-parity-roadmap-design.md` — checklist + known-limitations.

---

## Task 1: Pure event classifier + self-write suppression registry

**Files:**
- Create: `src/watch.rs`
- Modify: `src/lib.rs`, `src/main.rs` (add `mod watch;`)
- Test: inline `#[cfg(test)] mod tests` in `src/watch.rs`

**Interfaces:**
- Produces:
  - `pub enum WatchAction { Upsert(std::path::PathBuf), Remove(std::path::PathBuf) }` (derive `Debug, Clone, PartialEq, Eq`)
  - `pub fn classify_paths(paths: &[std::path::PathBuf], audio_exts: &[&str], cache_prefix: &std::path::Path, guard: &SelfWriteGuard) -> Vec<WatchAction>` — pure. A path is dropped if it lives under `cache_prefix`, if `guard.is_suppressed(path)` is true, or (for upsert candidates) its extension is not in `audio_exts`. Existing paths → `Upsert`; non-existing paths → `Remove`. Non-audio, non-suppressed removals are dropped too (only classify removals whose extension is audio OR whose path is not on disk with an audio-ish name — keep it simple: a path that no longer exists and had an audio extension → `Remove`).
  - `pub struct SelfWriteGuard` with `pub fn new(window: std::time::Duration) -> Self`, `pub fn register(&self, path: &std::path::Path)`, `pub fn is_suppressed(&self, path: &std::path::Path) -> bool` (also prunes expired entries), backed by `Mutex<HashMap<PathBuf, Instant>>`.
  - `pub fn register_self_write(path: &std::path::Path)` — module-level convenience writing to a global `static GUARD: OnceLock<SelfWriteGuard>` (default window 5 s), used by write sites in Task 8 and read by the watcher in Task 3.
- Consumes: nothing (pure + std only).

- [ ] **Step 1: Write failing tests** in `src/watch.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn exts() -> Vec<&'static str> { vec!["mp3", "flac", "ogg"] }

    #[test]
    fn classify_existing_audio_is_upsert_missing_is_remove() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("a.mp3");
        std::fs::write(&present, b"x").unwrap();
        let missing = dir.path().join("gone.mp3");
        let guard = SelfWriteGuard::new(Duration::from_secs(5));
        let cache = Path::new("/nonexistent-cache");
        let actions = classify_paths(&[present.clone(), missing.clone()], &exts(), cache, &guard);
        assert!(actions.contains(&WatchAction::Upsert(present)));
        assert!(actions.contains(&WatchAction::Remove(missing)));
    }

    #[test]
    fn non_audio_extension_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("notes.txt");
        std::fs::write(&txt, b"x").unwrap();
        let guard = SelfWriteGuard::new(Duration::from_secs(5));
        let actions = classify_paths(&[txt], &exts(), Path::new("/no-cache"), &guard);
        assert!(actions.is_empty());
    }

    #[test]
    fn cache_prefix_paths_dropped() {
        let cache = PathBuf::from("/home/u/.cache/sparkamp");
        let inside = cache.join("deadbeef.jpg");
        let guard = SelfWriteGuard::new(Duration::from_secs(5));
        let actions = classify_paths(&[inside], &exts(), &cache, &guard);
        assert!(actions.is_empty());
    }

    #[test]
    fn suppressed_path_dropped_then_processed_after_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("s.mp3");
        std::fs::write(&f, b"x").unwrap();
        let guard = SelfWriteGuard::new(Duration::from_millis(50));
        guard.register(&f);
        assert!(classify_paths(&[f.clone()], &exts(), Path::new("/no-cache"), &guard).is_empty());
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(
            classify_paths(&[f.clone()], &exts(), Path::new("/no-cache"), &guard),
            vec![WatchAction::Upsert(f)]
        );
    }
}
```

- [ ] **Step 2: Run tests, verify they fail** (module/function not defined). `cargo test --lib watch::`.
- [ ] **Step 3: Implement** the enum, `SelfWriteGuard`, `classify_paths`, `register_self_write` + global `OnceLock`. Extension match is case-insensitive. `is_suppressed` prunes entries older than `window` on each call.
- [ ] **Step 4: Add `mod watch;` to BOTH `src/lib.rs` and `src/main.rs`.**
- [ ] **Step 5: Run** `cargo test --lib watch::` → PASS. Full `cargo build && cargo test` → zero warnings.
- [ ] **Step 6: Commit** `feat(watch): add pure fs-event classifier + self-write suppression guard`.

---

## Task 2: `folders.recurse` migration + per-folder recurse in walk_dir

**Files:**
- Modify: `src/media_library/mod.rs:445-571` (`init_schema`), folder-management section
- Modify: `src/media_library/scan.rs:526` (`walk_dir`), `add_folder`, scan callers
- Test: `src/media_library/tests.rs`

**Interfaces:**
- Produces: `pub fn folder_recurse(&self, folder_id: i64) -> Result<bool>`; `pub fn set_folder_recurse(&self, folder_id: i64, recurse: bool) -> Result<()>`. `walk_dir` gains a `recurse: bool` parameter (recurse into subdirs only when true).
- Consumes: nothing new.

- [ ] **Step 1: Write failing tests** in `src/media_library/tests.rs`: (a) `folders_recurse_column_added_once_default_1` — open a DB, assert `pragma_table_info('folders')` contains `recurse`, and a folder added via `add_folder` reads `folder_recurse == true`; re-opening the same DB does not error (idempotent ALTER). (b) `walk_dir_non_recursive_skips_subdir` — fixture tree `root/a.mp3` + `root/sub/b.mp3`; `walk_dir(root, exts, &mut audio, &mut m3u, /*recurse*/ false)` yields only `a.mp3`; with `true` yields both.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement.** In `init_schema`, after the tracks-column loop, add a `folders` guard mirroring the tracks one:

```rust
let folder_cols: std::collections::HashSet<String> = {
    let mut stmt = self.conn.prepare("SELECT name FROM pragma_table_info('folders')")?;
    stmt.query_map([], |row| row.get::<_, String>(0))?.filter_map(|r| r.ok()).collect()
};
if !folder_cols.contains("recurse") {
    self.conn.execute("ALTER TABLE folders ADD COLUMN recurse INTEGER NOT NULL DEFAULT 1", [])?;
}
```

Add the `recurse` param to `walk_dir` and thread `false`/`true` through its recursive self-call (only recurse when the flag is set — note the flag governs descent, so pass it through unchanged). Update every existing `walk_dir` caller in `scan.rs` to pass the folder's `folder_recurse(folder_id)?`. `add_folder` inserts default (column default handles it). Add `folder_recurse`/`set_folder_recurse`.
- [ ] **Step 4: Run** the two tests → PASS, then full suite.
- [ ] **Step 5: Commit** `feat(library): per-folder recurse column + walk_dir honors it`.

---

## Task 3: Debounced OS watcher wrapper (`FolderWatcher`)

**Files:**
- Modify: `Cargo.toml` (add deps), `src/watch.rs`
- Test: one integration smoke test in `src/watch.rs` tests, timing-generous

**Interfaces:**
- Produces:
  - `pub struct FolderWatcher` with `pub fn start(folders: Vec<(std::path::PathBuf, bool /*recurse*/)>, audio_exts: Vec<String>, cache_prefix: std::path::PathBuf) -> std::io::Result<(FolderWatcher, std::sync::mpsc::Receiver<WatchAction>)>` and `pub fn stop(self)`.
  - Internally: 2 s debounce via `notify-debouncer-mini`; on each debounced batch, collect paths → `classify_paths(..., register_self_write's global GUARD)` → send each `WatchAction` on the channel. On `notify` init error (e.g. inotify `max_user_watches` exhausted) return `Err` so the caller degrades to manual rescan (never panics).
- Consumes: `classify_paths`, `SelfWriteGuard` global (Task 1); recurse flags (Task 2).

- [ ] **Step 1:** Add to `Cargo.toml` under `[dependencies]`:

```toml
# Filesystem watching (F10 watch folders)
notify = "6"
notify-debouncer-mini = "0.4"
```

- [ ] **Step 2: Write failing smoke test** (generous timing; if the suite proves flaky, gate with `#[ignore]` and document — the pure classifier already carries the logic coverage):

```rust
#[test]
fn watcher_emits_upsert_on_new_file() {
    use std::sync::mpsc::RecvTimeoutError;
    let dir = tempfile::tempdir().unwrap();
    let (watcher, rx) = FolderWatcher::start(
        vec![(dir.path().to_path_buf(), true)],
        vec!["mp3".into()],
        std::path::PathBuf::from("/no-cache"),
    ).expect("watcher start");
    std::thread::sleep(std::time::Duration::from_millis(300));
    std::fs::write(dir.path().join("new.mp3"), b"x").unwrap();
    let mut got = false;
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(8)) {
            Ok(WatchAction::Upsert(p)) if p.ends_with("new.mp3") => { got = true; break; }
            Ok(_) => continue,
            Err(RecvTimeoutError::Timeout) => break,
            Err(_) => break,
        }
    }
    watcher.stop();
    assert!(got, "expected an Upsert for the new file");
}
```

- [ ] **Step 3: Run, verify fail** (type missing). `cargo test --lib watch::watcher_emits`.
- [ ] **Step 4: Implement** `FolderWatcher`. Spawn the debouncer; per watched folder call `watcher.watch(path, if recurse { Recursive } else { NonRecursive })`. Map debounced events → paths → `classify_paths` (reading the global `GUARD`, audio_exts as `&str` slice, cache_prefix). Store the debouncer + join handle so `stop` drops cleanly.
- [ ] **Step 5: Run** the smoke test → PASS. Full `cargo build && cargo test`; also confirm `cargo test -- --test-threads=1` green for the flaky disc test.
- [ ] **Step 6: Commit** `feat(watch): debounced notify-based FolderWatcher emitting WatchActions`.

---

## Task 4: DB apply layer for watch actions

**Files:**
- Modify: `src/media_library/scan.rs` (or `mod.rs`), `src/watch.rs` (only if a shared type is needed)
- Test: `src/media_library/tests.rs`

**Interfaces:**
- Produces: `pub fn apply_watch_action(&self, action: &crate::watch::WatchAction, remove_missing: bool) -> Result<()>` on the ML handle. `Upsert(path)`: resolve the owning folder (nearest ancestor of the path among rows in `folders` — reuse the deepest-prefix `best` logic already in `add_files_to_library`, scan.rs:214-226; if none, `folder_id = NULL`), fast-insert the path row if absent (mirror `rescan_folder_fast`'s single-row INSERT ... ON CONFLICT(path) DO NOTHING with `added_at`), then `upsert_track(folder_id, path)`. `Remove(path)`: **USER-DECIDED SEMANTIC (2026-07-27)** — if `remove_missing` is true, hard-delete the row (`DELETE FROM tracks WHERE path = ?1`); if false, **KEEP the row (no-op)** so entries persist for temporarily-offline media (Winamp parity). There is NO "mark-broken" state in this codebase (the design doc's premise was wrong — `deleted_at` is a purge-staging flag not filtered from any display query, and today's rescan hard-deletes unconditionally).
- Consumes: `WatchAction` (Task 1), `upsert_track`, folder lookup.

- [ ] **Step 1: Write failing tests:** (a) `apply_upsert_inserts_and_fills_metadata` — tempdir DB + a folder + a real audio fixture path under it; `apply_watch_action(Upsert(path), false)` yields a row with metadata. (b) `apply_remove_marks_broken_when_flag_off` and (c) `apply_remove_deletes_when_flag_on`.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement.** Reuse the ancestor-folder resolution already present for `scan.rs:204` (`best` match logic) — factor a small `owning_folder_id(&self, path) -> Result<Option<i64>>` helper if none exists. Match the existing mark-broken column semantics exactly.
- [ ] **Step 4: Run tests → PASS**, full suite.
- [ ] **Step 5: Commit** `feat(library): apply_watch_action routes fs events through the scan seam`.

---

## Task 5: New config fields

**Files:**
- Modify: `src/config.rs:648-761` (`MediaLibraryConfig` + `Default`)
- Test: `src/config.rs` tests

**Interfaces:**
- Produces: `pub watch_folders: bool` (default **true**), `pub auto_add_played: bool` (false), `pub remove_missing_on_rescan: bool` (false), `pub compact_on_rescan: bool` (false). All `#[serde(default = "...")]` where the default is non-`Default`-derivable (i.e. `watch_folders` needs `#[serde(default = "MediaLibraryConfig::default_watch_folders")]` returning `true`; the three false ones can use plain `#[serde(default)]`).
- Consumes: nothing.

- [ ] **Step 1: Write failing tests:** `watch_folders_defaults_true` (`MediaLibraryConfig::default().watch_folders == true`); a round-trip test that a TOML omitting all four fields deserializes to (true, false, false, false); a round-trip that explicit values survive. Add a comment-line to the existing periodic fields marking them deprecated (superseded by `watch_folders`).
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** the four fields + `default_watch_folders() -> bool { true }`, update the `Default` impl.
- [ ] **Step 4: Run tests → PASS**, full suite.
- [ ] **Step 5: Commit** `feat(config): watch_folders/auto_add_played/remove_missing/compact toggles`.

---

## Task 6: Rescan behaviors — compact-on-rescan (VACUUM) + remove-missing-on-rescan

**Files:**
- Modify: `src/media_library/scan.rs` (`scan_all_folders` / `scan_folder` completion)
- Test: `src/media_library/tests.rs`

**Interfaces:**
- Produces: `scan_all_folders` gains awareness of the two flags. Signature options: either read flags from a passed-in `&MediaLibraryConfig`/booleans, OR add `pub fn compact(&self) -> Result<()>` (runs `VACUUM`) and `pub fn rescan_all_with(&self, remove_missing: bool, compact: bool, cancel, progress)` wrapper. Prefer minimal: add `pub fn compact(&self) -> Result<()>` and have the remove-missing branch keyed by a bool param already flowing into `scan_folder`. Match the design: remove-missing ON → the "file gone" branch deletes the row instead of marking broken.
- Consumes: existing scan flow.

- [ ] **Step 1: Write failing tests:** (a) `compact_runs_without_error` — build a DB, delete some rows, call `compact()`, assert `Ok`. (b) `rescan_remove_missing_off_marks_broken` vs `..._on_deletes` — scan a folder, delete the file on disk, rescan with flag off (row marked broken) vs on (row gone). Reuse whatever fixture helper the existing scan tests use.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement.** `compact()` = `self.conn.execute("VACUUM", [])?`. **USER-DECIDED (2026-07-27):** the missing-file removal loop in `rescan_folder` (scan.rs:357-363, the `DELETE FROM tracks WHERE id` for files where `!Path::exists`) must become **gated on `remove_missing`** — this CHANGES current behavior, which prunes unconditionally. Add a `remove_missing: bool` parameter to `rescan_folder`/`rescan_all` (and thread through `rescan_folder_fast` if its removal path exists); when false, SKIP the deletion loop entirely (keep rows for offline media). Update ALL callers (FFI `sparkamp_ml_rescan_*`, GTK `settings.rs`/`media_library.rs`, TUI) to pass `config.media_library.remove_missing_on_rescan`. Test the OFF-keeps / ON-deletes pair. VACUUM runs after a full `scan_all_folders`/`rescan_all` completes only when `compact_on_rescan` is on (caller-driven, so `compact()` stays a plain method).
- [ ] **Step 4: Run tests → PASS**, full suite.
- [ ] **Step 5: Commit** `feat(library): compact-on-rescan VACUUM + remove-missing rescan branch`.

---

## Task 7: auto-add-played hook

**Files:**
- Modify: `src/media_library/scan.rs` (or `queries.rs`), and the play-start seam (`src/controller.rs` play_current, mirrored by the GTK/mac play paths)
- Test: `src/media_library/tests.rs`

**Interfaces:**
- Produces: `pub fn add_played_track(&self, path: &str) -> Result<bool>` — if a row for `path` already exists, return `Ok(false)` (no-op). Else fast-insert with `owning_folder_id(path)` (NULL when outside all folders) + `upsert_track`, return `Ok(true)`. The *gating on the setting* lives at the call site, not inside this fn (keeps the fn testable without config).
- Consumes: `owning_folder_id` (Task 4), `upsert_track`.

- [ ] **Step 1: Write failing tests:** (a) `add_played_outside_library_creates_null_folder_row` — no folders registered, add a played path, assert a row exists with `folder_id IS NULL` and it appears in `all_tracks_sorted`. (b) `add_played_existing_is_noop` returns `false`. (c) inside-library path attaches to the owning folder.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** `add_played_track`. Wire the call at the play-start seam guarded by `config.media_library.auto_add_played` (core `Controller` for TUI; FFI-exposed for mac; GTK play path). The wiring lines are small; the core fn carries the tested logic.
- [ ] **Step 4: Run tests → PASS**, full suite.
- [ ] **Step 5: Commit** `feat(library): auto-add-played hook (folder_id NULL bucket for outside tracks)`.

---

## Task 8: Self-write suppression wiring at write sites

**Files:**
- Modify: `src/id3_editor.rs:447` (`write_tag_fields`), `src/id3_editor.rs:598` (`write_extra_frame`), `src/replaygain.rs:385` (RG write-back), the folder-image/artwork write site (grep for where Sparkamp writes a cover/folder image, and `queries.rs:384 refresh_artwork` if it writes files)
- Test: `src/watch.rs` or `src/id3_editor.rs`

**Interfaces:**
- Consumes: `crate::watch::register_self_write` (Task 1).
- Produces: nothing new (side-effect registration).

- [ ] **Step 1: Write a failing test** proving a write site registers: e.g. in `id3_editor.rs` tests, after `write_tag_fields(path, ..)`, assert `crate::watch::is_path_suppressed(path)` is true (add a tiny pub `is_path_suppressed(path) -> bool` reading the global GUARD to `watch.rs` for testability). Keep the assertion tolerant of the 5 s window.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement.** Call `crate::watch::register_self_write(path)` at the top of each write fn, right before/after the actual write completes (register AFTER the write returns Ok so the mtime is already bumped). Confirm every write path: `write_tag_fields`, `write_extra_frame`, RG write-back in `replaygain.rs`, and any folder-image writer. Cache-dir writes are already excluded by `cache_prefix` in the classifier — no registration needed there.
- [ ] **Step 4: Run test → PASS**, full suite.
- [ ] **Step 5: Commit** `feat(watch): register Sparkamp's own writes to suppress self-triggered rescans`.

---

## Task 9: FFI surface

**Files:**
- Modify: `src/ffi/settings.rs` (get/set 4 toggles + per-folder recurse), `src/ffi/media_library.rs` (watcher lifecycle + event poll), `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`
- Test: FFI smoke tests where feasible (`src/ffi/settings.rs` already has the pattern)

**Interfaces (mirror the existing `sparkamp_get/set_*` + scan-progress-poll idioms exactly):**
- `sparkamp_get_watch_folders(ctx)->bool` / `sparkamp_set_watch_folders(ctx, bool)` (setter also starts/stops the watcher).
- `sparkamp_get_auto_add_played` / `_set_`, `sparkamp_get_remove_missing_on_rescan` / `_set_`, `sparkamp_get_compact_on_rescan` / `_set_`, `sparkamp_get_rescan_on_startup` / `_set_` (the last already has a config field; add FFI if missing).
- `sparkamp_ml_folder_recurse(ctx, path)->bool` / `sparkamp_ml_set_folder_recurse(ctx, path, bool)`.
- `sparkamp_ml_poll_watch_event(ctx, out_kind: *mut c_int, out_path: *mut c_char, cap)->bool` — drains one pending `WatchAction` for the mac tick (mirror the scan-progress poll shape in `ffi/media_library.rs`). Returns false when the queue is empty.

- [ ] **Step 1:** Read the whole existing FFI files first (`src/ffi/settings.rs`, `src/ffi/media_library.rs`) and the header, matching the exact `#[no_mangle] pub unsafe extern "C"` idiom and null-safety guards.
- [ ] **Step 2: Write failing smoke tests** for the pure get/set pairs (set then get round-trips config), following the existing `settings.rs` test pattern.
- [ ] **Step 3: Implement** all symbols. Watcher lifecycle: store an `Option<FolderWatcher>` + `Receiver<WatchAction>` in `SparkampCtx` (or a side struct); `set_watch_folders(true)` (re)builds it from the current folders + recurse flags; folder add/remove rebuilds it; `poll_watch_event` drains and applies via `apply_watch_action` OR returns the event for the frontend to refresh — pick: apply in core (DB write) and return the path so mac refreshes that row. On watcher start `Err`, log + leave watcher `None` (degraded) — never panic.
- [ ] **Step 4: Mirror every new symbol in `sparkamp_bridge.h`** byte-for-byte in the SAME commit.
- [ ] **Step 5: Run** `cargo build && cargo test` → PASS, zero warnings.
- [ ] **Step 6: Commit** `feat(ffi): watch-folder toggles, per-folder recurse, watch-event poll`.

---

## Task 10: GTK wiring

**Files:**
- Modify: `frontends/gtk/window/settings.rs` (Tab 4 Media Library, ~1215+), `frontends/gtk/window/media_library.rs` (folder list rows; watcher event drain), GTK app init (startup rescan + watcher start)

**Interfaces:** Consumes core `MediaLibrary::apply_watch_action`, `FolderWatcher`, config toggles. GTK compiles only in the bin target; the user verifies interactively.

- [ ] **Step 1:** Read the settings Tab 4 block and the folder-list construction in `media_library.rs` fully before editing (grep the "Watched folders" list at `settings.rs:1226`).
- [ ] **Step 2:** Add four `Switch`/`CheckButton` rows to Tab 4 Media Library: Watch folders (live), Auto-add played, Remove missing on rescan, Compact on rescan, plus a Rescan-on-startup row. Persist via the tab's existing save-on-close idiom (copy the adjacent row's save call — it varies by tab). Add a per-folder **Recurse** checkbox to each watched-folder row, wired to `sparkamp`-less core `set_folder_recurse` (GTK calls the core `MediaLibrary` directly, not FFI).
- [ ] **Step 3:** Start a `FolderWatcher` when the library is loaded (and `watch_folders` on); drain its `Receiver` on the GTK main loop via a `glib::timeout_add_local` or the existing ML poll tick; for each `WatchAction`, call `apply_watch_action` off the DB thread and refresh the affected ML rows reusing the scan-completion refresh callback. Rebuild the watcher on folder add/remove and on the toggle. Stop cleanly on shutdown.
- [ ] **Step 4:** At app start, if `rescan_on_startup`, kick the existing background `scan_all_folders` path (`settings.rs:1686` / `media_library.rs:5257` show the call).
- [ ] **Step 5: RefCell discipline** — no borrow held across a UI call or `select_row`. `gtk_safe()` any path/error strings shown.
- [ ] **Step 6: Run** `cargo build && cargo test` (bin target must compile) → zero warnings.
- [ ] **Step 7: Commit** `feat(gtk): watch-folder settings, per-folder recurse, live event drain, startup rescan`.

---

## Task 11: TUI wiring

**Files:**
- Modify: `frontends/tui/settings_eq.rs`, `frontends/tui/ui/settings_eq.rs`, `frontends/tui/mod.rs`

**Interfaces:** Consumes core config + `MediaLibrary` methods + watcher. TUI already triggers `rescan_on_startup` at `mod.rs:585`.

- [ ] **Step 1:** Read the Media Library settings block in `settings_eq.rs` (the `periodic_rescan`/`rescan_interval_mins`/`rescan_on_startup` rows at lines ~186-216) and the item-count comment at `mod.rs:415`.
- [ ] **Step 2: Replace** the `periodic_rescan` + `rescan_interval_mins` rows with a **Watch folders** toggle row; add **Auto-add played**, **Remove missing on rescan**, **Compact on rescan** rows. Keep the `rescan_on_startup` row. Update the item-count comment/handler index math at `mod.rs:415` accordingly.
- [ ] **Step 3:** Start a `FolderWatcher` when the ML view is active and `watch_folders` on; drain the `Receiver` on the TUI tick; `apply_watch_action` + refresh the ML view list. Per-folder recurse: expose a toggle in the TUI folder list if it has one; else document as GTK/mac-only in the known-limitations (TUI folder management surface may be thinner — verify what exists before promising it).
- [ ] **Step 4: Run** full build+suite → zero warnings.
- [ ] **Step 5: Commit** `feat(tui): replace dead periodic rows with watch toggle + scan-behavior toggles + live refresh`.

---

## Task 12: macOS wiring (blind)

**Files:**
- Modify: mac settings view (find the Media Library settings surface), `SparkampModel+MediaLibrary.swift`, the watched-folder list view, `docs/mac-pass-checklist.md`

**Interfaces:** Consumes the Task 9 FFI symbols only. BLIND — read whole files, mechanical changes, mirror nothing that isn't in the header.

- [ ] **Step 1:** Read the whole of `SparkampModel+MediaLibrary.swift` and the mac Media Library settings + folder-list views before editing. Identify the existing `tick()` (`SparkampModel+MediaLibrary.swift:123` mentions periodic refresh) to hang the `sparkamp_ml_poll_watch_event` drain on.
- [ ] **Step 2:** Add four `Toggle`s (Watch folders, Auto-add played, Remove missing on rescan, Compact on rescan) + Rescan-on-startup, each bound to a `@Published` mirrored via the FFI get/set (follow the `stopAfterCurrent`/`rgWriteTags` pattern). Add a per-folder Recurse toggle to each watched-folder row via `sparkamp_ml_set_folder_recurse`.
- [ ] **Step 3:** In `tick()`, drain `sparkamp_ml_poll_watch_event`; on a returned path, refresh that ML row (reuse the scan-refresh path). At app start, if `rescanOnStartup`, call the existing rescan-all entry.
- [ ] **Step 4:** Append a dated **Phase 8** section to `docs/mac-pass-checklist.md` (checkbox items: toggles persist; new file appears live; delete marks broken / removes with flag; external tag edit refreshes; Sparkamp's own tag save causes no storm; non-recursive folder ignores subdir; startup rescan; auto-add-played row appears; compact shrinks db). Same commit.
- [ ] **Step 5:** No Swift compiler — verify by re-reading against real property names + the header. Do NOT run cargo for Swift, but DO run the full Rust build+suite to confirm the header/FFI still compile.
- [ ] **Step 6: Commit** `feat(mac): watch-folder toggles, per-folder recurse, live event polling, startup rescan (blind)`.

---

## Task 13: Docs — known limitations + checklist close-out

**Files:**
- Modify: `docs/superpowers/specs/2026-07-17-winamp-parity-roadmap-design.md` (known-limitations register), `docs/mac-pass-checklist.md` (if not fully covered by Task 12)

- [ ] **Step 1:** Add known-limitations entries: (a) inotify `max_user_watches` exhaustion on very large trees → watcher degrades to manual rescan with a surfaced status line (never crashes); (b) ~2 s debounce latency before a new file appears; (c) debounce-batch smoke test may be timing-sensitive (classification covered by pure unit tests); (d) `periodic_rescan`/`rescan_interval_mins` config fields retained deprecated for back-compat but no longer surfaced in UI.
- [ ] **Step 2:** Verify the GTK interactive pass list (design doc "Manual test plan" 1-9) and the mac checklist are both complete.
- [ ] **Step 3: Commit** `docs(phase8): known-limitations + verification checklist`.

---

## Automated test summary (must all pass, zero warnings)

Classifier (upsert/remove/cache/suppression), suppression expiry, folder recurse migration + walk_dir, watcher smoke (1), apply_watch_action (upsert/remove×2), config defaults + round-trip, compact + remove-missing rescan, auto-add-played (3), self-write registration, FFI get/set round-trips. Quote lib + bin `test result:` lines; confirm the disc flaky test green single-threaded.

## Manual verification (delivered to user at close-out)

The design doc's "Manual test plan" 1-9 becomes the GTK interactive pass list. mac items live in `docs/mac-pass-checklist.md` Phase 8 section. TUI: keyboard walk of the ML settings + a live add/remove.
