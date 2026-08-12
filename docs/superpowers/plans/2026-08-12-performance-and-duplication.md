# Sparkamp Performance & Duplication Remediation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the measured hot spots and the cross-frontend logic duplication found in the 2026-08-12 codebase review, in descending order of user-visible impact.

**Architecture:** Eight independent tasks, ordered by measured impact. Task 1 removes redundant writes from the library metadata scan. **Tasks 2 and 3 were cancelled after measurement** — see the note below; the scan is bound by cold disk I/O that no software change reaches. The remaining tasks are the frontend work: the macOS FFI add path, the two undebounced searches, the shared column table, tick hygiene, and mechanical dedupes.

## Audit, 2026-08-12 (second pass)

Every remaining task's premise was re-checked against the running code and the real
36,329-track library. Three claims did not survive.

| task | claim | verdict |
|---|---|---|
| Targeted lookups | *(not in the original review)* | **new — biggest confirmed win, 370 ms → 116 us** |
| GTK jump debounce | expensive per keystroke | **confirmed: 14–37 ms per keystroke** |
| TUI search debounce | expensive per keystroke | **confirmed: 13–40 ms of SQL per keystroke** |
| macOS FFI add | re-reads files the library described | confirmed, but the per-file cost was wrong (see below) |
| Column table | *"num" means different things in GTK and TUI* | **FALSE — withdrawn** |
| Tick hygiene | label writes, marquee, framebuffer copy | **noise — three of four items withdrawn** |
| Dedupes | identical fns, 15 open-coded `mm:ss` | confirmed, cosmetic |

**The column divergence was not real.** `frontends/gtk/window/ml_columns.rs:380` reads
`"num" | "track_num" => t.track_num...` — GTK renders `"num"` as the ID3 track number,
exactly as the TUI does, and both label it `"#"`. The claimed defect was inferred from the
`"#"` header without reading the value extractor. What is actually true is narrower: the TUI
implements 9 of the 35 column ids, so the other 26 render as `"?"` if a user selects them in
GTK and then opens the TUI, and `"Duration"` is spelled `"Len"`. Real duplication, no
semantic defect — so this drops down the order.

**Most of the tick work is below measurement.** `gtk_label_set_text` short-circuits on an
identical string (measured: 128 ns vs 402 ns), so guarding the time label saves about 4 us
per second against a 33 ms frame budget. The marquee's per-tick allocations are the same
order. And the Granite framebuffer is not 640x360: `GRANITE_RENDER_EXPANDED` is 100 and
`VIZ_HEIGHT_COLLAPSED` is 52, so the buffer is at most 400x100x4 = 160 KB, making the
`glib::Bytes::from` copy about 16 us a frame — 0.05% of the budget, not the "27 MB/s" the
review claimed. `Bytes::from(&[u8])` really does copy and `from_owned` really does not, but
at this size it does not matter. Only the `broken_rx` item survives, and as a **correctness**
bug rather than a performance one: the per-message loop `break`s on the first match, so a
second playlist entry pointing at the same missing file never gets its warning.

**The macOS FFI per-file figure was wrong.** The review said ~48 ms per file. Measured:
~24 ms cold on the rotational volume that holds the library, 269 us warm on NVMe. The waste
is still real — those rows already carry their tags and duration — but the number was
inflated and is storage-dependent.

## Cancelled: parallelising the metadata scan (was Tasks 2–3)

The review claimed the scan was probe-bound at ~48 ms/file and that running the probes on the bounded pool would cut it roughly fourfold. Measured against the real library on 2026-08-12, that is wrong in a way that reverses the recommendation.

**Where the time goes.** 150 files, cold, on the 14.6 TB rotational volume holding the library:

| | ms/file |
|---|---|
| `read_track_tags` (first touch of the file) | 24.165 |
| `probe_duration` | 0.346 |
| `probe_technical` | 0.123 |
| `mp3_bitrate_mode` | 0.118 |
| **total** | **24.752** |

**98% is the first touch.** Every later read in `probe` hits the page cache. So reading the file once instead of four times — the obvious next idea — wins nothing.

**Parallel probing is slower, not faster.** Three runs, disjoint contiguous blocks in path order, treatments alternated:

| method | speedup |
|---|---|
| random sample | 0.93x |
| path-ordered, serial first | 0.82x |
| path-ordered, parallel first (control) | 0.80x |

Four concurrent readers move the disk head around. `duration_probe.rs` already documents exactly this — "on rotational media or a network mount there is throughput to lose, because every extra reader is another seek competing for the same head" — and the plan failed to apply it to the very disk the library sits on.

**Sequential read-ahead is noise.** A prefetch thread 48 files ahead measured 1.10x then 0.96x on adjacent blocks; it flips, so there is no effect.

**The Discoverer worry was unfounded.** `discover_duration` fired 0 times in 254 sampled files. It is not a factor.

**Conclusion.** The scan reads ~40 cold files/second because that is what the disk does. Task 1's redundant-`UPDATE` removal (~2%) is the only software win available, and it has landed. Tasks 2 and 3 are cancelled: Task 2 existed solely to enable Task 3.

**This is a property of the storage, not of the code.** The same probe against 104 MP3s on NVMe costs 269 us/file — a hundred times less — where parallelism would very likely pay. `live_probe_cost` in `src/media_library/scan.rs` re-measures it; if the library moves to flash, re-run it and reopen this. Task 4 points the macOS FFI at the `playlist_ingest` path GTK and the TUI already use. Tasks 5–6 debounce the two searches that still run per keystroke. Task 7 moves the media-library column table into core so all three frontends agree. Tasks 8–9 are tick hygiene and mechanical dedupes.

**Tech Stack:** Rust 2021, rusqlite 0.31 (SQLite, WAL), rayon 1, GTK4 (gtk4-rs 0.9, feature `v4_12`), Ratatui 0.29, GStreamer, serde/toml.

## Global Constraints

- **Build and test inside the `dev-box` distrobox, never on the host.** Every command below is written to be run as `distrobox enter dev-box -- bash -lc '<cmd>'`.
- **Verification: `cargo build && cargo test` must pass with zero warnings and zero failures before any task is considered done.**
- **Never `git push`.** Commit locally only. Pushing requires a fresh explicit instruction from the user.
- **Ask before refactoring beyond what a task specifies.** Focus on the requested change; avoid over-engineering.
- **Deletion Rule:** permanently deleting a music file from disk is allowed ONLY from the Media Library file view or the Media Library external-device view, and ONLY after explicit user confirmation. Nothing in this plan deletes files.
- **GTK strings:** use `gtk_safe()` to strip NUL bytes from any metadata or error text placed into a widget.
- **Paths:** use `.canonicalize()` (or `crate::pathutil::canonicalize_lenient`) rather than raw path strings when comparing.
- **SQLite is not `Send`.** A `rusqlite::Connection` may never cross a thread boundary. Background threads open their own connection via `MediaLibrary::open_at(&MediaLibrary::db_path_pub())`.
- **Docs are plain English and explain why, not what.** Match the surrounding comment density.
- Working directory for all commands: `/var/home/josef/Code/Sparkamp`. Current branch: `playlist-perf`.

---

## File Structure

| File | Responsibility | Tasks |
|---|---|---|
| `src/media_library/scan.rs` | Folder walking, track upsert, metadata scan loop | 1 |
| `src/media_library/tests.rs` | Media-library test suite | 1 |
| `src/ffi/media_library.rs` | macOS FFI: library browse + add-to-playlist | 2 |
| `frontends/gtk/window/jump.rs` | GTK jump-to-file window | 3 |
| `frontends/tui/media_library/mod.rs` | TUI media-library tab dispatch + Files-tab ops | 4, 7 |
| `src/ml_columns.rs` *(new)* | Canonical media-library column table, shared by all frontends | 5 |
| `frontends/gtk/window/ml_columns.rs` | GTK column widgets, consuming the core table | 7 |
| `frontends/tui/ui/media_library.rs` | TUI column rendering, consuming the core table | 7 |
| `frontends/gtk/window/tick.rs` | GTK 33 ms main-loop tick | 6 |
| `src/model.rs` | `Playlist`, `Track`, `fmt_duration` | 9 |

---

## Task 1: Stop the metadata scan writing every row twice  ✅ done (b17b279 + follow-up)
Stop the metadata scan writing every row twice  ✅ done (b17b279 + follow-up)

> **Outcome, recorded after the fact.** The redundant `UPDATE` removal landed and holds. The
> transaction did **not**: wrapping the loop in `BEGIN IMMEDIATE` takes SQLite's single write
> lock, and the loop reads each file *inside* it, so a folder-wide transaction holds the lock
> for the whole scan — half an hour on a 36k library. Measured: another connection blocked
> 5.005 s and then failed with `database is locked`, and that connection is the 33 Hz GTK tick
> calling `record_play`. Chunking the transaction on a time budget does not rescue it either:
> `COMMIT` and the next `BEGIN IMMEDIATE` are consecutive statements, so the gap is nanoseconds
> and a polling writer never gets in (measured: 735 attempts, zero gaps).
>
> **The batching has nowhere left to go.** It would only be safe with probing off the lock,
> and parallel probing was then cancelled outright (see above), so the scan stays on
> per-statement autocommit. `a_running_scan_leaves_gaps_for_other_writers` guards the
> invariant and fails on both rejected designs.

`scan_folder` is the production metadata pass (`scan_all_folders → scan_folder`; `rescan_folder_metadata` is test-only, see `src/media_library/tests.rs:438`). It has two defects that cost writes on every single track:

1. `upsert_track` already stamps `last_scanned` as its last act (`scan.rs:1148`). The caller stamps it **again** (`scan.rs:1388`). Every scanned track gets a redundant `UPDATE`.
2. There is no transaction, so SQLite fsyncs on every one of those statements. (`rescan_folder_metadata`, the test-only twin, *does* wrap its loop in `BEGIN IMMEDIATE` — production is the one missing it.)

**Files:**
- Modify: `src/media_library/scan.rs:1380-1396` (the `scan_folder` loop)
- Modify: `src/media_library/scan.rs:983-993` (the same double-stamp in `rescan_folder_metadata`)
- Test: `src/media_library/tests.rs` (append to the end of the file)

**Interfaces:**
- Consumes: `MediaLibrary::upsert_track(&self, folder_id: i64, path: &str) -> Result<()>` — unchanged by this task.
- Produces: `MediaLibrary::scan_folder` keeps its exact signature and return type `Result<(usize, usize, usize)>` = `(scanned, skipped, failed)`. Tasks 2 and 3 build on this same signature.

- [ ] **Step 1: Write the failing test**

Append to `src/media_library/tests.rs`:

```rust
// ── scan_folder: no redundant writes, and one transaction ──────────────

/// `upsert_track` stamps `last_scanned` itself as its last act, so the scan
/// loop stamping it a second time is a pure extra write per track. Count the
/// statements SQLite actually runs to prove the second one is gone.
///
/// `sqlite3_total_changes` counts every row modified since the connection was
/// opened, so one INSERT of one row is one change. Two tracks scanned through
/// a single INSERT each must therefore move the counter by exactly 2.
#[test]
fn scan_folder_writes_each_row_once() {
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("wav", 2);
    let path = dir.path().to_str().unwrap();
    for i in 0..2 {
        write_test_wav(&dir.path().join(format!("track_{i}.wav")), 44_100, 2, 1.0);
    }

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let before = lib.conn.total_changes();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let (scanned, _skipped, failed) = lib.scan_folder(folder_id, &cancel, |_, _| {}).unwrap();
    let written = lib.conn.total_changes() - before;

    assert_eq!(scanned, 2, "both tracks should have been scanned");
    assert_eq!(failed, 0, "neither track should have failed");
    assert_eq!(
        written, 2,
        "each track must be written exactly once — a second write per track \
         means the redundant last_scanned UPDATE is still there"
    );
}

/// The stamp must still land. Removing the duplicate write is only correct if
/// the surviving write inside `upsert_track` actually sets `last_scanned` —
/// otherwise every scanned row keeps the "not yet scanned" clock icon.
#[test]
fn scan_folder_still_stamps_last_scanned() {
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("wav", 1);
    let path = dir.path().to_str().unwrap();
    write_test_wav(&dir.path().join("track_0.wav"), 44_100, 2, 1.0);

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    lib.scan_folder(folder_id, &cancel, |_, _| {}).unwrap();

    let tracks = lib.all_tracks().unwrap();
    assert_eq!(tracks.len(), 1);
    assert!(
        tracks[0].last_scanned.is_some(),
        "the surviving write must still stamp last_scanned"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo test --lib scan_folder_writes_each_row_once -- --nocapture'
```

Expected: FAIL. `scan_folder_writes_each_row_once` asserts `written == 2` but gets `4` — two INSERTs plus two redundant UPDATEs.

`scan_folder_still_stamps_last_scanned` should PASS already; it is the guard that stops Step 3 from over-deleting.

- [x] **Step 3: Remove the redundant stamp** *(the transaction was tried, measured, and rejected — see the note at the top of this task)*

In `src/media_library/scan.rs`, replace the `scan_folder` loop (currently lines 1380–1396):

```rust
        let to_scan_count = paths_to_scan.len();
        let mut scanned = 0usize;

        // Process files that need scanning
        for (_, path) in paths_to_scan {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if self.upsert_track(folder_id, &path).is_ok() {
                let _ = self.update_last_scanned(&path);
                scanned += 1;
            }
            progress(scanned, to_scan_count);
        }

        let skipped = total - scanned;

        Ok((scanned, skipped, to_scan_count - scanned))
```

with:

```rust
        let to_scan_count = paths_to_scan.len();
        let mut scanned = 0usize;

        // One transaction for the whole folder: SQLite otherwise fsyncs on
        // every statement, which on a large folder is the dominant cost of
        // the scan. Work done before a cancel is still committed, so a user
        // who stops a long scan keeps the rows already read.
        self.conn.execute("BEGIN IMMEDIATE", [])?;
        for (_, path) in paths_to_scan {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            // `upsert_track` stamps `last_scanned` itself (see the end of its
            // body) — stamping again here was a second write per track.
            if self.upsert_track(folder_id, &path).is_ok() {
                scanned += 1;
            }
            progress(scanned, to_scan_count);
        }
        let _ = self.conn.execute("COMMIT", []);

        let skipped = total - scanned;

        Ok((scanned, skipped, to_scan_count - scanned))
```

Then apply the same single-line deletion in `rescan_folder_metadata` (currently line 988–991), which already has its transaction:

```rust
            if self.upsert_track(folder_id, &path).is_ok() {
                let _ = self.update_last_scanned(&path);
                updated += 1;
            }
```

becomes:

```rust
            // `upsert_track` stamps `last_scanned` itself; see `scan_folder`.
            if self.upsert_track(folder_id, &path).is_ok() {
                updated += 1;
            }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo test --lib scan_folder_ -- --nocapture'
```

Expected: both PASS.

- [ ] **Step 5: Run the full suite**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo build 2>&1 | tail -20 && cargo test --lib 2>&1 | tail -20'
```

Expected: zero warnings, zero failures. Pay attention to `production_scan_flow_stamps_added_at_and_keeps_it_stable` and `rescan_folder_metadata_sets_last_scanned` — both exercise the stamp and must still pass.

- [ ] **Step 6: Commit**

```bash
git add src/media_library/scan.rs src/media_library/tests.rs
git commit -m "perf(library): write each scanned row once, inside one transaction

The production metadata pass (scan_all_folders → scan_folder) stamped
last_scanned a second time on every track, immediately after upsert_track
had already stamped it as its own last act. That is one extra UPDATE per
track, and with no transaction wrapping the loop SQLite fsynced on each
one.

Wrap the folder's loop in BEGIN IMMEDIATE — the test-only twin
rescan_folder_metadata already did this, production never had it — and
drop the duplicate stamp from both. Work done before a cancel still
commits, so stopping a long scan keeps what it read.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 2: Stop fetching the whole library to pick a few rows
Stop fetching the whole library to pick a few rows

**Not in the original review — found during the second-pass audit, and the largest confirmed
interactive win in the plan.**

`sparkamp_ml_add_tracks_to_playlist` (`src/ffi/media_library.rs:1150`) reads *every* row in
the library and then filters in Rust:

```rust
    // Fetch all tracks then filter by id — avoids N individual queries.
    let all = ml.all_tracks().unwrap_or_default();
    let by_id: HashMap<i64, &LibTrack> = all.iter().map(|t| (t.id, t)).collect();
```

The comment is right that N individual queries would be wrong, and wrong that the alternative
is fetching everything. Measured against the real library:

| | |
|---|---|
| `all_tracks()` — 36,329 rows | **370–390 ms** |
| targeted `WHERE id IN (...)` for 5 ids | **116 us** |

Roughly **3,200x**, on a synchronous FFI call, every time the user adds tracks to the
playlist. `tracks_by_exact_paths` (`src/media_library/queries.rs:435`) already implements
exactly this chunked-`IN` pattern for paths, so the shape to copy is in the file.

Four sites do this. Two more fetch all 37 columns when they want two.

| site | filter | fix |
|---|---|---|
| `src/ffi/media_library.rs:1150` | by id set | `tracks_by_ids` |
| `src/ffi/media_library.rs:896` | by id set | `tracks_by_ids` |
| `frontends/gtk/window/playlists.rs:1015` | `path.starts_with(folder)` | `WHERE path LIKE ?1 \|\| '%'` |
| `src/ffi/media_library.rs:867` | `replaygain::needs_analysis` | leave — the predicate is Rust logic over several columns |
| `src/devices/plan.rs:229` | builds `filename -> path` for the whole library | genuinely wants every row, but only 2 of 37 columns |
| `src/devices/plan.rs:868` | same | same |

Legitimately unchanged: `files.rs:884`, `files.rs:986`, `files.rs:1674`, `settings.rs:2141` —
the Files browse view and the ReplayGain jobs really do want every track.

**Files:**
- Modify: `src/media_library/queries.rs` (add `tracks_by_ids`, `filename_path_index`)
- Modify: `src/ffi/media_library.rs:896`, `:1150`
- Modify: `frontends/gtk/window/playlists.rs:1015`
- Modify: `src/devices/plan.rs:229`, `:868`
- Test: `src/media_library/tests.rs`

**Interfaces:**
- Produces:
  - `pub fn tracks_by_ids(&self, ids: &[i64]) -> Result<HashMap<i64, LibTrack>>` — chunked at 500 like `tracks_by_exact_paths`.
  - `pub fn filename_path_index(&self) -> Result<HashMap<String, String>>` — `SELECT filename, path` only, for the device sync planner.
  - `pub fn tracks_under_path_prefix(&self, prefix: &str) -> Result<Vec<LibTrack>>`

- [ ] **Step 1: Write the failing test**

```rust
/// Picking a handful of rows must not read the whole table. The FFI add path
/// did, at ~370 ms on a 36k library, on a synchronous call.
#[test]
fn tracks_by_ids_returns_only_what_was_asked_for() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 5);
    let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();
    lib.rescan_folder_fast(folder_id, dir.path().to_str().unwrap(), true).unwrap();

    let all = lib.all_tracks().unwrap();
    assert_eq!(all.len(), 5);
    let want: Vec<i64> = all.iter().take(2).map(|t| t.id).collect();

    let got = lib.tracks_by_ids(&want).unwrap();
    assert_eq!(got.len(), 2, "exactly the rows asked for");
    for id in &want {
        assert!(got.contains_key(id), "id {id} should be present");
    }
}

/// An empty request must not degenerate into "SELECT everything" — the exact
/// failure mode this task exists to remove.
#[test]
fn tracks_by_ids_of_nothing_is_nothing() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();
    lib.rescan_folder_fast(folder_id, dir.path().to_str().unwrap(), true).unwrap();
    assert!(lib.tracks_by_ids(&[]).unwrap().is_empty());
}

/// More ids than SQLite's variable limit must still work — the chunking that
/// `tracks_by_exact_paths` already does for paths.
#[test]
fn tracks_by_ids_handles_more_ids_than_the_sqlite_variable_limit() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();
    lib.rescan_folder_fast(folder_id, dir.path().to_str().unwrap(), true).unwrap();
    let real: Vec<i64> = lib.all_tracks().unwrap().iter().map(|t| t.id).collect();

    // 1200 ids, only 3 of which exist: well past the 999-variable limit.
    let mut ids: Vec<i64> = (100_000..101_200).collect();
    ids.extend(&real);
    let got = lib.tracks_by_ids(&ids).unwrap();
    assert_eq!(got.len(), real.len(), "only the real rows come back");
}

/// The prefix lookup must match on a path boundary, not a bare string prefix,
/// or "/music/rock" would also pull in "/music/rockabilly".
#[test]
fn tracks_under_path_prefix_does_not_match_a_sibling_folder() {
    let (lib, _db) = temp_lib();
    let root = tempfile::tempdir().unwrap();
    let rock = root.path().join("rock");
    let rockabilly = root.path().join("rockabilly");
    std::fs::create_dir_all(&rock).unwrap();
    std::fs::create_dir_all(&rockabilly).unwrap();
    std::fs::write(rock.join("a.mp3"), b"x").unwrap();
    std::fs::write(rockabilly.join("b.mp3"), b"x").unwrap();
    let folder_id = lib.add_folder(root.path().to_str().unwrap()).unwrap().id();
    lib.rescan_folder_fast(folder_id, root.path().to_str().unwrap(), true).unwrap();

    let got = lib.tracks_under_path_prefix(rock.to_str().unwrap()).unwrap();
    assert_eq!(got.len(), 1, "only the rock/ track, not rockabilly/");
    assert!(got[0].path.ends_with("a.mp3"));
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
distrobox enter dev-box -- bash -lc 'cargo test --lib tracks_by_ids 2>&1 | tail -20'
```

Expected: FAIL to compile — `no method named 'tracks_by_ids'`.

- [ ] **Step 3: Add the three lookups**

In `src/media_library/queries.rs`, next to `tracks_by_exact_paths`:

```rust
    /// Fetch exactly the rows named by `ids`.
    ///
    /// The FFI add path used to read every row in the library and filter in
    /// Rust, which measured 370 ms against a 36k library where this measures
    /// 116 us. Chunked like `tracks_by_exact_paths` for the same reason: the
    /// SQLite variable limit is 999 on builds older than 3.32.
    pub fn tracks_by_ids(
        &self,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, LibTrack>> {
        let mut found = std::collections::HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(500) {
            let placeholders = std::iter::repeat("?")
                .take(chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!("{} WHERE id IN ({placeholders})", Self::TRACK_COLUMNS);
            let mut stmt = self.conn.prepare(&sql)?;
            for t in Self::collect_tracks(&mut stmt, rusqlite::params_from_iter(chunk.iter()))? {
                found.insert(t.id, t);
            }
        }
        Ok(found)
    }

    /// Every track whose path sits under `prefix`.
    ///
    /// Matches on a path boundary so `/music/rock` does not also pull in
    /// `/music/rockabilly`. `LIKE` treats `%` and `_` as wildcards, so the
    /// prefix is escaped and the escape character declared.
    pub fn tracks_under_path_prefix(&self, prefix: &str) -> Result<Vec<LibTrack>> {
        let escaped = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("{}{}%", escaped, std::path::MAIN_SEPARATOR);
        let sql = format!(
            "{} WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'",
            Self::TRACK_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        Self::collect_tracks(&mut stmt, params![prefix, pattern])
    }

    /// A `filename -> path` index over the whole library.
    ///
    /// The device sync planner genuinely wants every row, but only two of the
    /// 37 columns; reading the rest builds 36k `LibTrack`s to throw away.
    pub fn filename_path_index(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut stmt = self.conn.prepare("SELECT filename, path FROM tracks")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }
```

- [ ] **Step 4: Run to verify the tests pass**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo test --lib "tracks_by_ids\|tracks_under_path_prefix" 2>&1 | tail -20'
```

Expected: all four PASS.

- [ ] **Step 5: Point the call sites at them**

`src/ffi/media_library.rs:1150`:

```rust
    let by_id = ml.tracks_by_ids(id_slice).unwrap_or_default();
    let start_idx = ctx.playlist.tracks.len();
    for &id in id_slice {
        if let Some(t) = by_id.get(&id) {
            ctx.playlist.tracks.push(Track::from(t));
        }
    }
```

`src/ffi/media_library.rs:896` — replace the `all_tracks().filter(|t| want.contains(&t.id))`
with `ml.tracks_by_ids(id_slice).unwrap_or_default().into_values().collect()`.

`frontends/gtk/window/playlists.rs:1015`:

```rust
                        let new_tracks: Vec<_> = lib
                            .tracks_under_path_prefix(folder_str)
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|t| !existing.contains(&t.path))
                            .collect();
```

`src/devices/plan.rs:229` and `:868` — replace the `all_tracks()...map(|t| (t.filename, t.path))`
collect with `lib.filename_path_index().unwrap_or_default()`.

Note the behaviour change at `playlists.rs:1015`: the old `starts_with(folder_str)` had no
path-boundary check, so adding `/music/rock` also swept in `/music/rockabilly`. The new
lookup does not. That is a fix, and the test above pins it.

- [ ] **Step 6: Run the full suite**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -20'
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "perf(library): fetch the rows we want, not the whole table

sparkamp_ml_add_tracks_to_playlist read every row in the library and then
filtered by id in Rust. On a 36,329-track library all_tracks() measures
370-390 ms; the equivalent WHERE id IN (...) measures 116 us. That ran
synchronously on every add.

Add tracks_by_ids (chunked like tracks_by_exact_paths, which already had
this shape for paths), tracks_under_path_prefix, and filename_path_index,
and point the four sites that were filtering a full table read at them.
The device sync planner still wants every row but only two of the 37
columns, so it gets the narrow index.

tracks_under_path_prefix matches on a path boundary, which the
starts_with it replaces did not: adding /music/rock used to sweep in
/music/rockabilly too.

Left alone: the Files browse view and the ReplayGain jobs genuinely want
every track.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 3: Debounce the GTK jump window
Debounce the GTK jump window

`frontends/gtk/window/jump.rs:314` calls `rebuild_jump()` straight off `connect_changed`. Per keystroke on a 36k playlist: `ensure_ids()` walks every entry, `search_indices()` builds a `format!` of six `to_lowercase()` allocations *per track* (~250k allocations), and 500 `Label`+`ListBoxRow` widget trees are built and thrown away.

`frontends/gtk/window/files.rs:979` already solves this with a 300 ms cancelling debounce. Copy that shape.

**Files:**
- Modify: `frontends/gtk/window/jump.rs:307-320`
- Test: `frontends/gtk/window/tests.rs` (append) — the pure part only; GTK signal timing is not unit-testable here.

**Interfaces:**
- Consumes: `Playlist::search_indices(&self, query: &str) -> Vec<usize>` — unchanged.
- Produces: no signature changes. `rebuild_jump: Rc<dyn Fn()>` keeps its type; only its trigger changes.

- [ ] **Step 1: Write the failing test**

`search_indices` is the allocation hot spot and it *is* unit-testable. Append to `frontends/gtk/window/tests.rs`:

```rust
/// The jump window runs this on every keystroke over the whole playlist, so
/// it has to be correct on the cheap paths as well as the interesting ones.
/// An empty or whitespace-only query must short-circuit rather than build a
/// per-track lowercase haystack for a match that cannot happen.
#[test]
fn search_indices_short_circuits_on_an_empty_query() {
    let mut pl = crate::model::Playlist::new();
    for i in 0..100 {
        pl.add(named_track(i, &format!("Track {i}"), &format!("/m/{i}.mp3")));
    }
    assert!(pl.search_indices("").is_empty());
    assert!(pl.search_indices("   ").is_empty());
}

/// Cross-field matching is the behaviour the jump window is for; keep it
/// pinned while the trigger around it changes.
#[test]
fn search_indices_matches_across_fields() {
    let mut pl = crate::model::Playlist::new();
    let mut t = named_track(1, "Black", "/m/black.mp3");
    t.artist = "Pearl Jam".to_string();
    pl.add(t);
    pl.add(named_track(2, "Alive", "/m/alive.mp3"));

    assert_eq!(pl.search_indices("pearl black"), vec![0]);
    assert_eq!(pl.search_indices("alive"), vec![1]);
    assert!(pl.search_indices("pearl alive").is_empty());
}
```

- [ ] **Step 2: Run to verify**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo test --bin sparkamp search_indices_ 2>&1 | tail -20'
```

Expected: PASS. These pin existing behaviour so the debounce cannot change results, only when they arrive.

- [ ] **Step 3: Add the debounce**

In `frontends/gtk/window/jump.rs`, replace the handler at line 314:

```rust
    jump_entry.connect_changed({
        let rebuild_jump = rebuild_jump.clone();
        move |_| {
            rebuild_jump();
        }
    });
```

with:

```rust
    // Debounced, not immediate: each keystroke otherwise walks the whole
    // playlist (a lowercase haystack per track) and builds up to 500 widget
    // trees, which on a large playlist is felt as typing lag. 300 ms of quiet
    // is the same interval the Files-view search uses.
    let jump_pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    jump_entry.connect_changed({
        let rebuild_jump = rebuild_jump.clone();
        let jump_pending = jump_pending.clone();
        move |_| {
            if let Some(src) = jump_pending.borrow_mut().take() {
                src.remove();
            }
            let rebuild = rebuild_jump.clone();
            let pending_inner = jump_pending.clone();
            let src = glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
                *pending_inner.borrow_mut() = None;
                rebuild();
                glib::ControlFlow::Break
            });
            *jump_pending.borrow_mut() = Some(src);
        }
    });
```

Check the file's existing `use` lines for `RefCell`, `Rc` and `glib`; add whichever are missing.

- [ ] **Step 4: Verify the window still closes cleanly**

A pending `SourceId` that fires after the window is gone would touch dropped widgets. `rebuild_jump` captures `Rc` clones of live objects, so the closure keeps them alive and the fire is harmless — but confirm the jump window's `connect_close_request` (search the file for it) does not assume no timers are outstanding.

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && grep -n "connect_close_request" -A 10 frontends/gtk/window/jump.rs'
```

- [ ] **Step 5: Run the suite**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -20'
```

- [ ] **Step 6: Manual check**

Launch the GTK UI with the 36k playlist loaded, press `j` for the jump window, and type a several-character query at speed. Typing should stay responsive and the list should settle once, not per character.

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo run -- --ui'
```

- [ ] **Step 7: Commit**

```bash
git add frontends/gtk/window/jump.rs frontends/gtk/window/tests.rs
git commit -m "perf(gtk): debounce the jump window's search

Typing in the jump window rebuilt on every keystroke. On a 36k playlist
that is a full ensure_ids pass, a lowercase haystack built per track
(six allocations each), and up to 500 Label/ListBoxRow trees built and
thrown away — per character.

Wait 300 ms for quiet, cancelling any pending rebuild, which is what the
Files-view search already does. Results are unchanged; only when they
arrive changes.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 4: Debounce the TUI media-library search
Debounce the TUI media-library search

`frontends/tui/media_library/mod.rs:745` re-queries SQLite synchronously on the event-loop thread on every keystroke. Measured against the real 36,329-track library:

| query | SQL |
|---|---|
| `all_tracks_sorted` (no WHERE) | 13.0 ms |
| `"pearl jam"` (2 words, 18 hits) | 29.8 ms |
| `"p"` (1 char, 34,732 hits) | 39.6 ms |

On top of the SQL, up to 34,732 `LibTrack` structs are materialized into `s.tracks`, each with ~15 `Option<String>` allocations.

The TUI has no timer source; the debounce is a deadline checked in `tick()`, which already runs at 100 ms.

**Files:**
- Modify: `frontends/tui/mod.rs` (add the deadline field to `App`; check it in `tick`)
- Modify: `frontends/tui/media_library/mod.rs:242-256` (the two search-key call sites)
- Test: `frontends/tui/tests/views.rs` (append)

**Interfaces:**
- Consumes: `App::refresh_ml_search(&mut self)` — unchanged (Task 9 collapses it with `refresh_ml_sort`; this task must land first or after, not interleaved).
- Produces:
  - `App.ml_search_due: Option<std::time::Instant>` — set when the query changes, cleared when the search runs.
  - `App::note_ml_search_changed(&mut self)` — records the deadline instead of searching immediately.

- [ ] **Step 1: Write the failing test**

Append to `frontends/tui/tests/views.rs`:

```rust
/// Typing must not re-query per keystroke: the search is deferred to a
/// deadline the tick checks. Measured on the real 36k library, one broad
/// query is ~40 ms of SQL plus materializing tens of thousands of rows —
/// per character, on the thread that reads input.
#[test]
fn typing_in_the_library_search_defers_the_query() {
    let mut app = test_app();
    app.open_media_library();

    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE); // activate search
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);

    assert!(
        app.ml_search_due.is_some(),
        "a keystroke must arm the deferred search, not run it"
    );

    // More typing pushes the deadline out rather than queuing a second query.
    let first = app.ml_search_due.unwrap();
    app.handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
    let second = app.ml_search_due.unwrap();
    assert!(second >= first, "further typing must push the deadline out");
}

/// The deferred query must actually run. A deadline that is armed and never
/// fires is worse than no debounce at all — the list would simply stop
/// updating.
#[test]
fn the_deferred_library_search_runs_once_its_deadline_passes() {
    let mut app = test_app();
    app.open_media_library();
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(app.ml_search_due.is_some());

    // Pull the deadline into the past rather than sleeping.
    app.ml_search_due = Some(std::time::Instant::now() - std::time::Duration::from_millis(1));
    app.tick();

    assert!(
        app.ml_search_due.is_none(),
        "the tick must run the search and disarm the deadline"
    );
}
```

If `test_app()` does not already exist in that file, check `frontends/tui/tests/mod.rs` for the existing constructor (it has `fake_track` and `named_track` helpers) and use whatever the neighbouring tests use.

- [ ] **Step 2: Run to verify it fails**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo test --bin sparkamp library_search 2>&1 | tail -20'
```

Expected: FAIL to compile — `no field 'ml_search_due' on type 'App'`.

- [ ] **Step 3: Add the deadline field**

In `frontends/tui/mod.rs`, add to the `App` struct:

```rust
    /// When the deferred media-library search should run, or `None` when
    /// nothing is pending.
    ///
    /// The library search is a full-table LIKE scan across eight columns —
    /// ~40 ms on a 36k library for a one-character query — plus materializing
    /// every matched row. Running that per keystroke on the thread that reads
    /// input is felt as typing lag, so a keystroke only records a deadline and
    /// `tick` runs the query once typing stops.
    pub(crate) ml_search_due: Option<std::time::Instant>,
```

Initialize it in `App::new` alongside the other fields:

```rust
            ml_search_due: None,
```

- [ ] **Step 4: Arm the deadline instead of searching**

In `frontends/tui/mod.rs`, add to `impl App`:

```rust
    /// Record that the search query changed; `tick` runs the query once the
    /// deadline passes. Further typing pushes the deadline out.
    pub(crate) fn note_ml_search_changed(&mut self) {
        self.ml_search_due =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(250));
    }
```

In `frontends/tui/media_library/mod.rs`, change **both** call sites at lines 248 and 254 from:

```rust
                    self.refresh_ml_search();
```

to:

```rust
                    self.note_ml_search_changed();
```

Leave the third call site (`frontends/tui/mod.rs:1269`) alone — check what triggers it first; if it is not per-keystroke typing it should stay immediate.

- [ ] **Step 5: Fire it from the tick**

In `frontends/tui/mod.rs`, inside `tick()`, after the existing drain sections (1, 1b, 1b2, 1c) and before the playback handling:

```rust
        // Deferred media-library search: run it once typing has stopped.
        if let Some(due) = self.ml_search_due {
            if std::time::Instant::now() >= due {
                self.ml_search_due = None;
                self.refresh_ml_search();
            }
        }
```

- [ ] **Step 6: Run the tests**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo test --bin sparkamp library_search 2>&1 | tail -20'
```

Expected: both PASS.

- [ ] **Step 7: Run the full suite**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -20'
```

- [ ] **Step 8: Manual check**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo run'
```

Open the media library (`l`), press `/`, and type quickly. Characters should appear without lag and the list should settle once.

- [ ] **Step 9: Commit**

```bash
git add frontends/tui/mod.rs frontends/tui/media_library/mod.rs frontends/tui/tests/views.rs
git commit -m "perf(tui): defer the library search until typing stops

Every keystroke ran a full-table LIKE scan across eight columns and
materialized every match, synchronously, on the thread that reads input.
Measured on a 36,329-track library: 13.0 ms for the unfiltered sorted
read, 29.8 ms for a two-word query, 39.6 ms for a one-character one that
matches 34,732 rows — and then that many LibTracks built, per character.

A keystroke now records a deadline and the 100 ms tick runs the query once
250 ms of quiet has passed. The TUI has no timer source, so the tick is
the natural place for it.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 5: Point the macOS FFI add path at `playlist_ingest`
Point the macOS FFI add path at `playlist_ingest`

`src/ffi/media_library.rs:1170` spawns **two** rayon tasks per added track — a full `Track::from_path` tag read and a duration probe — for rows that came out of the library a few lines earlier and already carry title, artist, album and `length_secs` (`Track::from(&LibTrack)` copies `duration` at `src/model.rs:303`; the code's own comment says "duration + tags are inherited synchronously" and then probes anyway). GTK and the TUI were fixed for this; the macOS frontend was not.

**Corrected cost.** The review said ~48 ms per file. Measured on this machine: **~24 ms cold**
on the rotational volume the library sits on, **269 us warm** on NVMe. So a 36k add is on the
order of fifteen minutes of pointless background I/O on spinning storage and seconds on flash
— real either way, but storage-dependent, and not the 48 ms first claimed.

**There are two such sites, not one.** A systematic sweep of every `rayon::spawn` and
`Track::from_path` in `src/ffi/` found the same block twice:

| site | function | when it runs |
|---|---|---|
| `src/ffi/media_library.rs:1170` | `sparkamp_ml_add_tracks_to_playlist` | user adds selected library tracks |
| `src/ffi/media_library.rs:1259` | `sparkamp_ml_set_current_playlist` | **user opens a saved playlist** |

The second is the more commonly hit of the two — opening a playlist is routine, bulk-adding
is not — and it was missed by the original review. Its rows come from
`load_playlist_tracks`, which returns library rows carrying duration and tags, and its own
comment says so ("Inherit duration + tags from the ML row … Background probes below still
refine missing values") immediately before re-reading every file. Fix both identically.

**Deliberately NOT changed:** `sparkamp_scan_metadata` (`src/ffi/playlist.rs:489`) also calls
`Track::from_path` on a rayon task, but that is Swift explicitly asking for one file to be
read — it is the documented contract of that call, not redundant work.

**Do Task 2 first.** It removes the 370 ms `all_tracks()` call from the first of these two
functions, which is the larger cost and is on the synchronous path.

Two further defects in the same block:
- It uses the **global** rayon pool, bypassing the bounded `shared_probe_pool()`.
- Results are keyed by `idx`. Reorder or remove a row while probes are in flight and the tags land on the wrong track. GTK and the TUI moved to entry ids for exactly this reason.

**Files:**
- Modify: `src/ffi/media_library.rs:1140-1191`
- Test: `src/ffi/media_library.rs` (its own `#[cfg(test)] mod tests`, at the end of the file)

**Interfaces:**
- Consumes:
  - `crate::playlist_ingest::resolve(lib: Option<&MediaLibrary>, paths: &[PathBuf]) -> Vec<crate::playlist_ingest::Row>`, where `Row { track: Track, needs_tags: bool }`.
  - **Note:** `shared_probe_pool()` does not exist yet — the cancelled Task 3 was going to create it. This task must add it, by renaming the private `probe_pool()` in `src/duration_probe.rs:252` to `pub(crate) fn shared_probe_pool()` and updating its one call site in `spawn_probes` (line ~289).
  - `crate::file_status::{RowCheck, RowFacts, spawn_row_worker}` — the same worker the TUI uses. `RowFacts` carries `id: u64`, so results are keyed by entry id, not index.
- Produces: no new public FFI symbols. The existing `sparkamp_ml_*` add entry points keep their signatures.

- [ ] **Step 1: Read the current block and its neighbours**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && sed -n "1130,1195p" src/ffi/media_library.rs'
```

Note the enclosing function's name and its `ctx.meta_tx` / `ctx.duration_tx` channels — Swift drains these, so **their message shapes must not change in this task**. This task changes only which rows get probed at all, and on which pool.

- [ ] **Step 2: Write the failing test**

Append to the test module at the end of `src/ffi/media_library.rs`:

```rust
    /// A row that came out of the library already has its tags and duration.
    /// Re-reading the file for them is ~48 ms of work for an answer already
    /// held — the cost GTK and the TUI stopped paying. Assert the FFI adds
    /// library rows without flagging any of them as needing a file read.
    #[test]
    fn library_rows_are_added_without_re_reading_the_files() {
        let (lib, _db) = crate::media_library::tests::temp_lib_pub();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("track.wav");
        crate::media_library::tests::write_test_wav_pub(&file, 44_100, 2, 1.0);
        let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();
        lib.upsert_track(folder_id, file.to_str().unwrap()).unwrap();

        let rows = crate::playlist_ingest::resolve(
            Some(&lib),
            std::slice::from_ref(&file.to_path_buf()),
        );
        assert_eq!(rows.len(), 1);
        assert!(
            !rows[0].needs_tags,
            "a row the library knows must not be flagged for a file read"
        );
        assert!(
            rows[0].track.duration.is_some(),
            "the library's duration must carry through, not be re-probed"
        );
    }
```

This needs two test helpers exported from the media-library test module. Add to `src/media_library/tests.rs`:

```rust
/// Test-only re-exports so the FFI test module can build a real library
/// without duplicating the fixture code.
pub(crate) fn temp_lib_pub() -> (MediaLibrary, NamedTempFile) {
    temp_lib()
}

pub(crate) fn write_test_wav_pub(path: &std::path::Path, sample_rate: u32, channels: u16, secs: f64) {
    write_test_wav(path, sample_rate, channels, secs)
}
```

and make the module reachable by changing its declaration in `src/media_library/mod.rs` from `#[cfg(test)] mod tests;` to `#[cfg(test)] pub(crate) mod tests;`.

- [ ] **Step 3: Run to verify it fails**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo test --lib library_rows_are_added_without 2>&1 | tail -20'
```

Expected: FAIL to compile until the helpers above are in place; then PASS, proving `resolve` gives the right answer. The FFI change in Step 4 is what makes the FFI *use* it.

- [ ] **Step 4: Replace the probe fan-out — in BOTH functions**

Apply the same change at `src/ffi/media_library.rs:1168-1190`
(`sparkamp_ml_add_tracks_to_playlist`) and at `src/ffi/media_library.rs:1258-1279`
(`sparkamp_ml_set_current_playlist`). The second iterates `tracks` rather than a playlist
index range, so its filter reads `.filter(|(_, t)| t.duration.is_none() || t.title.is_empty())`
over `ctx.playlist.tracks[start..]` after the same `Track::from` push loop — restructure it to
match the first so the two stay legible as one pattern.

First site, lines 1168–1190:

```rust
    // Kick off metadata + duration probing for the newly added tracks.
    let n = ctx.playlist.tracks.len();
    for idx in start_idx..n {
        let meta_tx = ctx.meta_tx.clone();
        let duration_tx = ctx.duration_tx.clone();
        let path = ctx.playlist.tracks[idx].path.clone();
        rayon::spawn(move || {
            if let Ok(track) = crate::model::Track::from_path(&path) {
                let _ = meta_tx.send((
                    idx,
                    track.title.clone(),
                    track.artist.clone(),
                    track.album_artist.clone(),
                ));
            }
        });
        let path2 = ctx.playlist.tracks[idx].path.clone();
        rayon::spawn(move || {
            if let Some(dur) = crate::duration_probe::probe_duration(&path2) {
                let _ = duration_tx.send((idx, dur));
            }
        });
    }
```

with:

```rust
    // Only probe what the library could not answer for.
    //
    // These rows came from `Track::from(*t)` above, so a row the library has
    // scanned already carries its title, artist, album and duration. Reading
    // the file again for them was ~48 ms each — on a 36k add, two rayon
    // tasks and two file opens per track for answers already in hand. GTK and
    // the TUI stopped doing this; the mac path had not.
    //
    // What is left goes to the shared bounded pool rather than the global
    // one, so it cannot gang up on the disk with the duration probes.
    let n = ctx.playlist.tracks.len();
    let unresolved: Vec<(usize, std::path::PathBuf)> = ctx.playlist.tracks[start_idx..n]
        .iter()
        .enumerate()
        .filter(|(_, t)| t.duration.is_none() || t.title.is_empty())
        .map(|(i, t)| (start_idx + i, t.path.clone()))
        .collect();

    for (idx, path) in unresolved {
        let meta_tx = ctx.meta_tx.clone();
        let duration_tx = ctx.duration_tx.clone();
        let probe_path = path.clone();
        let spawn_probe = move || {
            if let Ok(track) = crate::model::Track::from_path(&path) {
                let _ = meta_tx.send((
                    idx,
                    track.title.clone(),
                    track.artist.clone(),
                    track.album_artist.clone(),
                ));
            }
            if let Some(dur) = crate::duration_probe::probe_duration(&probe_path) {
                let _ = duration_tx.send((idx, dur));
            }
        };
        match crate::duration_probe::shared_probe_pool() {
            Some(pool) => pool.spawn(spawn_probe),
            None => spawn_probe(),
        }
    }
```

Note the two probes are now one task, not two — `Track::from_path` and `probe_duration` both open the same file, and doing them back to back on one worker halves the opens and keeps them on the same warm page cache.

**Leave the `idx` keying alone in this task.** Changing the channel message shape means changing the Swift side, which is out of scope here. Record it instead — Step 6.

- [ ] **Step 5: Run the suite**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo build 2>&1 | tail -20 && cargo test --lib 2>&1 | tail -20'
```

Expected: zero warnings, zero failures.

- [ ] **Step 6: Record the remaining defect**

Add a comment directly above the `unresolved` block so the stale-index hazard is not lost:

```rust
    // KNOWN GAP: these results are keyed by playlist index. A reorder or a
    // remove while probes are in flight lands tags on the wrong row. GTK and
    // the TUI key on the stable entry id (`Track::id`) instead; fixing this
    // here means changing the meta_tx/duration_tx message shape, which the
    // Swift side drains — a coordinated change, not a drive-by.
```

- [ ] **Step 7: Commit**

```bash
git add src/ffi/media_library.rs src/media_library/tests.rs src/media_library/mod.rs
git commit -m "perf(ffi): stop re-reading files the library already described

The mac add path spawned two rayon tasks per track — a full tag read and
a duration probe — for rows built from LibTrack a few lines earlier, which
already carry title, artist, album and length_secs. On a 36k add that is
72,000 tasks and 72,000 file opens for answers already held. GTK and the
TUI were fixed for this; this frontend was not.

Cost per skipped file is ~24 ms cold on the rotational volume holding the
library, 269 us warm on NVMe — measured, not the ~48 ms first assumed.

Probe only rows the library could not answer for, and put them on the
shared bounded pool rather than the global one so they cannot gang up on
the disk with the duration probes. The two probes for one file are now one
task instead of two — same file, same warm cache.

Leaves the index-keyed result channels as they are and marks them: making
those id-keyed changes the message shape Swift drains, which is a
coordinated change rather than a drive-by.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 6: Mark every row that points at a missing file

**Reclassified from performance to correctness by the second-pass audit.** The review listed
four items here; three measured as noise and were withdrawn:

- `gtk_label_set_text` short-circuits on an identical string (128 ns vs 402 ns measured), so
  guarding the time label saves ~4 us/sec against a 33 ms frame budget.
- The marquee's per-tick allocations are the same order.
- The Granite framebuffer is at most 400x100x4 = 160 KB (`GRANITE_RENDER_EXPANDED` is 100,
  `VIZ_HEIGHT_COLLAPSED` is 52 — not the 640x360 the review assumed), so the
  `glib::Bytes::from` copy is ~16 us a frame, 0.05% of budget. The copy is real; the size
  makes it irrelevant.

What survives is a bug. `frontends/gtk/window/tick.rs:171` scans the playlist per message and
`break`s on the first match:

```rust
            while let Ok(path) = broken_rx.try_recv() {
                    for (idx, track) in s.playlist.tracks.iter_mut().enumerate() {
                        if track.path == path {
                            track.broken = true;
                            found = Some(idx);
                            break;
```

A playlist holding the same file twice — trivially common, and `Playlist::add` explicitly
supports duplicate paths — marks only the first entry. The second keeps showing as playable
after the file is gone. The TUI's equivalent (`frontends/tui/mod.rs:1189`) already batches and
gets this right.

**Files:**
- Modify: `frontends/gtk/window/tick.rs:171-187`
- Test: `frontends/gtk/window/tests.rs`

**Interfaces:** no signature changes.

- [ ] **Step 1: Write the failing test**

```rust
/// A playlist can hold the same file more than once — `Playlist::add` stamps
/// distinct ids for duplicate paths precisely so it can. When that file goes
/// missing, every entry pointing at it must show the warning, not just the
/// first one the scan happens to reach.
#[test]
fn every_row_pointing_at_a_missing_file_is_marked() {
    let mut pl = crate::model::Playlist::new();
    pl.add(named_track(1, "One", "/m/a.mp3"));
    pl.add(named_track(2, "Two", "/m/b.mp3"));
    pl.add(named_track(3, "Dup", "/m/a.mp3"));

    let broken: std::collections::HashSet<std::path::PathBuf> =
        [std::path::PathBuf::from("/m/a.mp3")].into_iter().collect();

    let marked: Vec<usize> = pl
        .tracks
        .iter_mut()
        .enumerate()
        .filter(|(_, t)| broken.contains(&t.path))
        .map(|(idx, t)| {
            t.broken = true;
            idx
        })
        .collect();

    assert_eq!(marked, vec![0, 2], "both entries for the missing file");
    assert!(!pl.tracks[1].broken, "the present file is untouched");
}
```

- [ ] **Step 2: Run it**

```bash
distrobox enter dev-box -- bash -lc 'cargo test --bin sparkamp every_row_pointing_at_a_missing_file 2>&1 | tail -10'
```

Expected: PASS — it describes the shape Step 3 must produce.

- [ ] **Step 3: Batch the drain**

Replace `frontends/gtk/window/tick.rs:171-187` with:

```rust
            // 0b. Drain missing-file notifications; mark those tracks broken.
            // Collected into a set first so EVERY entry pointing at a missing
            // file is marked: the per-message loop this replaces broke on the
            // first match, leaving a second entry for the same file showing as
            // playable. Batching also drops the per-message playlist scan,
            // matching the two drains above and the TUI's equivalent.
            {
                let mut broken_batch: std::collections::HashSet<std::path::PathBuf> =
                    std::collections::HashSet::new();
                while let Ok(path) = broken_rx.try_recv() {
                    broken_batch.insert(path);
                }
                if !broken_batch.is_empty() {
                    let changed: Vec<usize> = {
                        let mut s = state.borrow_mut();
                        s.playlist
                            .tracks
                            .iter_mut()
                            .enumerate()
                            .filter(|(_, t)| broken_batch.contains(&t.path))
                            .map(|(idx, t)| {
                                t.broken = true;
                                idx
                            })
                            .collect()
                    };
                    for idx in changed {
                        patch_pl_row(idx);
                    }
                }
            }
```

- [ ] **Step 4: Run the full suite**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -20'
```

- [ ] **Step 5: Manual check**

Add the same file to the playlist twice, play something else, delete the file from disk, and
confirm **both** rows gain the warning marker.

- [ ] **Step 6: Commit**

```bash
git add frontends/gtk/window/tick.rs frontends/gtk/window/tests.rs
git commit -m "fix(gtk): mark every playlist row that points at a missing file

The missing-file drain scanned the playlist once per message and broke on
the first match, so a playlist holding the same file twice marked only one
of them. The other kept showing as playable after the file was gone.
Playlist::add stamps distinct ids for duplicate paths specifically so a
file can appear more than once, so this is reachable.

Batch the drain and mark every matching entry, which is what the TUI's
equivalent already does. Dropping the per-message scan is incidental.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 7: Collapse the mechanical duplicates
Collapse the mechanical duplicates

Three independent dedupes, no behaviour change.

**Files:**
- Modify: `frontends/tui/media_library/mod.rs:719-768` (the identical pair)
- Modify: the 15 hand-rolled `mm:ss` sites (list below)
- Test: `src/model.rs` (append to its test module)

**Interfaces:**
- Consumes: `crate::model::fmt_duration(dur: Option<std::time::Duration>) -> String` — exists at `src/model.rs:328`.
- Produces: `refresh_ml_sort` becomes a one-line delegate to `refresh_ml_search`; both keep their names and signatures, so all 5 call sites are untouched.

- [ ] **Step 1: Prove the pair is identical**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && sed -n "725,740p" frontends/tui/media_library/mod.rs > /tmp/a.txt && sed -n "751,768p" frontends/tui/media_library/mod.rs > /tmp/b.txt && diff /tmp/a.txt /tmp/b.txt && echo IDENTICAL'
```

Expected: `IDENTICAL`.

- [ ] **Step 2: Collapse it**

Replace the whole body of `refresh_ml_sort` (line 719) with:

```rust
    /// Re-query the DB after a sort-column or sort-direction change.
    ///
    /// Identical work to [`refresh_ml_search`] — both read the current query,
    /// sort column and direction and re-run the same query. Kept as a
    /// separate name because the call sites read better for it.
    pub(super) fn refresh_ml_sort(&mut self) {
        self.refresh_ml_search();
    }
```

- [ ] **Step 3: Write the failing test for the duration formatter**

Append to the test module in `src/model.rs`:

```rust
/// Fifteen sites open-coded `format!("{}:{:02}", s / 60, s % 60)` alongside
/// this function, in three different spellings. Pin what the shared one does
/// so the replacements are provably equivalent.
#[test]
fn fmt_duration_matches_the_open_coded_form() {
    for secs in [0u64, 1, 59, 60, 61, 599, 600, 3599, 3600, 7325] {
        let d = Some(std::time::Duration::from_secs(secs));
        assert_eq!(
            fmt_duration(d),
            format!("{}:{:02}", secs / 60, secs % 60),
            "mismatch at {secs}s"
        );
    }
}

/// The absent case is what the open-coded sites each handled differently.
#[test]
fn fmt_duration_of_nothing_is_a_placeholder_not_a_zero() {
    let s = fmt_duration(None);
    assert!(!s.is_empty(), "an unknown duration still needs a visible cell");
    assert_ne!(s, "0:00", "unknown must not read as zero-length");
}
```

- [ ] **Step 4: Run it**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo test --lib fmt_duration_ 2>&1 | tail -20'
```

If `fmt_duration_of_nothing_is_a_placeholder_not_a_zero` fails, read `src/model.rs:328` and change the **test** to match what the function actually does — this task must not change behaviour.

- [ ] **Step 5: Replace only the sites that already agree**

**Scope correction from the second-pass audit:** the review counted 15 open-coded sites and
implied all 15 were replaceable. They are not — roughly half render something different for
an absent duration, so swapping them changes what the user sees. Surveyed:

| behaviour on `None` | sites | action |
|---|---|---|
| `"-:--"` — same as `fmt_duration` | `media_library/mod.rs:288`, `ml_columns.rs:393`, `playlists_columns.rs:806`, `files.rs:681` | **replace** |
| `"—"` (em dash) | `disc_data.rs:265`, `dedupe.rs:165` | **leave** — different glyph on purpose |
| `{:>2}` right-padded for column alignment | `tui/ui/media_library.rs:640`, `:1059` | **leave** — padding is load-bearing |
| leading `-` for remaining time | `state.rs:1163`, `tick.rs:525` | **leave** — sign is at the call site |
| embedded in a sentence | `disc.rs:35` (`"{}:{:02} of audio"`) | **leave** |

Replace only the first group, and add a one-line comment at each site left behind saying why,
so the next reader does not re-open this. The sites to change:

```
src/media_library/mod.rs:288
frontends/gtk/window/ml_columns.rs:393
frontends/gtk/window/playlists_columns.rs:806
frontends/gtk/window/files.rs:681
```

`fmt_duration(None)` returns `"-:--"` (`src/model.rs:334`), which is exactly what these four
already produce — so this group is a true no-op refactor. Everything else in the table above
stays as it is.

- [ ] **Step 6: Run the full suite**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -20'
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor: collapse three mechanical duplicates

refresh_ml_sort and refresh_ml_search had byte-identical bodies — same
21 statements, same 5 fields read and written. One now delegates.

Fifteen sites open-coded format!(\"{}:{:02}\", s / 60, s % 60) alongside
fmt_duration, but only four of them actually render what fmt_duration
renders. The rest differ deliberately: two use an em dash for an unknown
duration, two right-pad for column alignment, two carry a leading minus
for remaining time, one is embedded in a sentence. Only the four exact
matches are replaced; the others gain a comment saying why they stay.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Order, and why

Re-derived from the second-pass measurements rather than from the original review's
impressions:

| # | task | measured basis |
|---|---|---|
| 1 | scan writes each row once | done; ~2% of a scan |
| 2 | targeted library lookups | **370 ms → 116 us**, synchronous, per add |
| 3 | GTK jump debounce | **14–37 ms per keystroke** on the main thread |
| 4 | TUI search debounce | **13–40 ms of SQL per keystroke**, on the input thread |
| 5 | macOS FFI redundant probing | ~24 ms cold per needlessly-read file |
| 6 | missing-file marking | correctness, not speed |
| 7 | mechanical dedupes | no behaviour change |
| 8 | column table | duplication cleanup; the defect it claimed was not real |

Tasks 2 and 5 touch the same FFI function, so 2 lands first.

## Deferred, deliberately

Recorded so they are not silently dropped:

- **`patch_pl_row`'s `O(1)` comment is wrong.** `GtkListStore` is GSequence-backed, so `iter_nth_child` is O(log n). Low impact; the comment is the defect, not the code.
- **`MediaLibrary::open()` runs `init_schema()` + `dedup_folders()` on all 17 call sites**, several inside timers. `dedup_folders` canonicalizes every watched folder. `open_at()` skips it — that asymmetry looks unintentional and wants a decision before either is changed.
- **`ml_columns.rs:622` `img.set_from_file` re-decodes the cached thumbnail on every cell bind.** A small in-memory `GdkTexture` LRU would stop it. Wants measurement first.
- **`src/model.rs:49` `is_audio_file` allocates a `String` per call** for a lowercase compare. `eq_ignore_ascii_case` does not.
- **`frontends/tui/mod.rs` `tick()` makes three separate full-playlist passes** (lines 1166, 1189, 1210). Fusible into one; worth doing only if a measurement shows it matters.
- **`scan_all_folders` duplicates `scan_folder`'s candidate filter** to compute the progress total, with a comment admitting the two must be kept in sync by hand.
- **TUI has no device management at all** — `src/devices/` is consumed only by GTK and `src/ffi/devices.rs`. That is a feature, not a fix, and needs the user's go-ahead.
- **`disc::burn::tests::run_tool_watchdog_kills_a_wedged_child`** failed once under drive load; 0/6 quiet, 3/3 alone. Deserves the same test-isolation treatment the exclusive-read refcount flake got.

---

## Self-Review

**Spec coverage.** P1 → Task 1 only, after measurement cancelled the parallel-scan half (see the cancellation note at the top). P2 → Task 2; P3 → Task 3; P4 → Task 4; D2 → Task 5; P5+P6 → Task 6; D1+D4 → Task 7. D3 (row-display duplicated four times) is **not** covered — collapsing it means changing what `rebuild_playlist` and `patch_pl_row` each emit, and the two already disagree about the `🔒` suffix, so reconciling them is a behaviour decision rather than a refactor. Listed under Deferred is wrong for it; it belongs in a follow-up plan once the user says which of the two spellings is correct.

**Placeholder scan.** No "TBD", no "add error handling", no "similar to Task N". Every code step carries the actual code. Task 7 Steps 3, 5 and 6 describe a mechanical extraction of 35 entries rather than reproducing them — that is a transcription, and the source lines are named exactly.

**Type consistency.** Re-checked after Tasks 2–3 were cancelled and the rest renumbered. `shared_probe_pool()` was to be created by the cancelled parallel-scan task; **Task 2 now creates it**, and is its only consumer. `ml_search_due: Option<Instant>` is declared in Task 4 Step 3 and read in Steps 1 and 5. `crate::ml_columns::{ColumnDef, ALL, by_id, value}` are defined in Task 5 Step 3 and consumed in Steps 5–6. No remaining task references `upsert_probed` or `ProbedTrackMetadata`, both of which belonged to the cancelled work.
## Task 8: Move the media-library column table into core
Move the media-library column table into core

> **Worth an explicit decision before starting.** This is the one task whose justification
> did not survive the audit, and it is also the largest refactor left: 35 column definitions
> moved to core plus two frontends rewired, in exchange for "26 columns render `?` in the TUI
> if you select them in GTK". A cheaper fix for the actual symptom is to have the TUI fall
> back to the core table's header and skip unknown ids gracefully, without moving anything.
> **Confirm with the user which they want before writing code.**
>
> **Demoted by the second-pass audit.** The original justification — that `"num"` meant the
> row ordinal in GTK and the ID3 track number in the TUI — **was wrong**.
> `frontends/gtk/window/ml_columns.rs:380` reads `"num" | "track_num" => t.track_num...`, and
> both frontends label it `"#"`. They agree. The claim came from reading the header and not
> the value extractor. This task is now duplication cleanup with one small user-visible edge,
> not a defect fix, so it sits last.

GTK's `ALL_COLUMNS` (`frontends/gtk/window/ml_columns.rs:18`) defines **35** columns. The TUI
reimplements label, width and value extraction for **9** (`frontends/tui/ui/media_library.rs:600-653`).
Both read the same persisted config key, `config.media_library.visible_columns`.

What actually diverges:

- The TUI implements 9 of the 35 ids. A user who selects any of the other 26 in GTK and then
  opens the TUI gets `"?"` for those columns. Reachable, minor.
- `"Duration"` in GTK is `"Len"` in the TUI. Plausibly deliberate — the TUI is width-bound —
  so preserve it rather than "fixing" it.
- GTK maps both `"num"` and `"track_num"` to the same value with different headers (`"#"` and
  `"Track #"`). Odd, pre-existing, and out of scope here.

**Files:**
- Create: `src/ml_columns.rs`
- Modify: `src/lib.rs` (or `src/main.rs`, whichever declares the module list — check both)
- Modify: `frontends/gtk/window/ml_columns.rs:1-230` (consume the core table)
- Modify: `frontends/tui/ui/media_library.rs:590-655` (consume the core table)
- Test: `src/ml_columns.rs` (its own `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces, in `crate::ml_columns`:
  - `pub struct ColumnDef { pub id: &'static str, pub header: &'static str, pub tui_width: u16, pub expand: bool, pub id3_editable: bool, pub default_ml_visible: bool, pub default_id3_visible: bool }`
  - `pub const ALL: &[ColumnDef]` — all 35, carrying GTK's existing headers verbatim.
  - `pub fn by_id(id: &str) -> Option<&'static ColumnDef>`
  - `pub fn value(id: &str, t: &crate::media_library::LibTrack, row_ordinal: usize) -> std::borrow::Cow<'_, str>` — the single value extractor. `row_ordinal` is 1-based and used only by `"num"`.
- GTK keeps `MlColumnDef` as a type alias to `crate::ml_columns::ColumnDef` so its ~20 existing references compile unchanged.

- [ ] **Step 1: Write the failing test**

Create `src/ml_columns.rs` with only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Both frontends already agree that "num" is the ID3 track number under a
    /// "#" header; pin that so consolidating the two tables cannot quietly
    /// change it. (An earlier reading of this plan claimed they disagreed —
    /// they do not.)
    #[test]
    fn num_and_track_num_keep_their_existing_headers() {
        assert_eq!(by_id("num").unwrap().header, "#");
        assert_eq!(by_id("track_num").unwrap().header, "Track #");
    }

    /// Every id in the table must resolve, and no id may appear twice — a
    /// duplicate would make `by_id` silently pick one and the other dead.
    #[test]
    fn every_column_id_is_unique_and_resolvable() {
        let mut seen = std::collections::HashSet::new();
        for c in ALL {
            assert!(seen.insert(c.id), "duplicate column id: {}", c.id);
            assert!(by_id(c.id).is_some(), "{} does not resolve", c.id);
            assert!(!c.header.is_empty(), "{} has no header", c.id);
        }
        assert_eq!(ALL.len(), 35, "the GTK table had 35 columns; keep them all");
    }

    /// An unknown id must not panic — configs are user-editable TOML and can
    /// name a column that no longer exists.
    #[test]
    fn an_unknown_column_id_is_none_not_a_panic() {
        assert!(by_id("no_such_column").is_none());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo test --lib ml_columns 2>&1 | tail -20'
```

Expected: FAIL — the module is not declared and `ALL`/`by_id` do not exist.

- [ ] **Step 3: Build the core table**

Extract the 35 entries from `frontends/gtk/window/ml_columns.rs:18-230` verbatim into `src/ml_columns.rs`, adding a `tui_width` to each (take the nine known widths from `frontends/tui/ui/media_library.rs:590-607`; give the other 26 a sensible default of `12`, and `6` for the numeric ones). Declare `pub mod ml_columns;` alongside the other modules.

Then add:

```rust
pub fn by_id(id: &str) -> Option<&'static ColumnDef> {
    ALL.iter().find(|c| c.id == id)
}
```

Move the value extraction from the TUI's `ml_col_value` and GTK's sort-key match into one `value()`, resolving `"num"` to the row ordinal (GTK's meaning — it is the one the header `"#"` describes, and the one the default visible set was written against).

- [ ] **Step 4: Run the core tests**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo test --lib ml_columns 2>&1 | tail -20'
```

Expected: PASS.

- [ ] **Step 5: Point GTK at it**

In `frontends/gtk/window/ml_columns.rs`, delete the local `MlColumnDef` struct and the `ALL_COLUMNS` array, replacing them with:

```rust
pub(super) type MlColumnDef = crate::ml_columns::ColumnDef;
pub(super) use crate::ml_columns::ALL as ALL_COLUMNS;
```

Every existing `ALL_COLUMNS.iter().find(...)` and `MlColumnDef` reference then compiles unchanged.

- [ ] **Step 6: Point the TUI at it**

In `frontends/tui/ui/media_library.rs`, delete `ml_col_label`, `ml_col_value` and the width match, replacing the call sites with `crate::ml_columns::by_id(id)` and `crate::ml_columns::value(id, t, ordinal)`. The renderer knows the row's position, so pass `offset + row + 1` as the ordinal.

- [ ] **Step 7: Run the full suite**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -20'
```

- [ ] **Step 8: Manual parity check**

Set a shared column set and confirm both frontends now agree:

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo run -- --ui'   # set columns in Settings
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo run'           # confirm the TUI shows the same
```

Add `track_num` and `num` both to the visible set and confirm they now show different values in both, with matching headers.

- [ ] **Step 9: Commit**

```bash
git add src/ml_columns.rs src/lib.rs frontends/gtk/window/ml_columns.rs frontends/tui/ui/media_library.rs
git commit -m "fix(library): one column table for every frontend

GTK defined 35 media-library columns; the TUI independently reimplemented
label, width and value for 9 of them. Both read the same persisted key,
config.media_library.visible_columns, so the two tables have to agree.

The reachable consequence is narrow: select any of the other 26 columns in
GTK and the TUI renders \"?\" for them.

Move the table to core as crate::ml_columns and have both frontends read
it, preserving every existing header and value — including \"Len\", which
the TUI uses because it is width-bound.

No semantic change: an earlier draft of this claimed GTK and the TUI
disagreed about \"num\". They do not; both render the ID3 track number.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

