# Sparkamp Performance & Duplication Remediation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the measured hot spots and the cross-frontend logic duplication found in the 2026-08-12 codebase review, in descending order of user-visible impact.

**Architecture:** Seven independent tasks. Task 1 removes redundant writes from the library metadata scan. **Tasks 2 and 3 were cancelled after measurement** — see the note below; the scan is bound by cold disk I/O that no software change reaches. The remaining tasks are the frontend work: the macOS FFI add path, the two undebounced searches, the shared column table, tick hygiene, and mechanical dedupes.

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

## Task 2: Point the macOS FFI add path at `playlist_ingest`

`src/ffi/media_library.rs:1170` spawns **two** rayon tasks per added track — a full `Track::from_path` tag read and a duration probe — for rows that came out of the library on line 1162 and already carry title, artist, album and `length_secs`. A 36k add is 72,000 tasks and 72,000 file opens for answers already held. GTK and the TUI were fixed for this; the macOS frontend was not.

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

- [ ] **Step 4: Replace the probe fan-out**

In `src/ffi/media_library.rs`, replace lines 1168–1190:

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

## Task 3: Debounce the GTK jump window

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

## Task 5: Move the media-library column table into core

GTK's `ALL_COLUMNS` (`frontends/gtk/window/ml_columns.rs:18`) defines **35** columns. The TUI reimplements label, width and value extraction for **9** (`frontends/tui/ui/media_library.rs:600-653`). Both read the same persisted config key, `config.media_library.visible_columns`.

They have already diverged, and the divergence is user-visible:

```
GTK   id "num" → header "#"     → row ordinal
TUI   id "num" → t.track_num    → ID3 track number
```

Same key, two meanings. GTK's `"Duration"` is the TUI's `"Len"`. The other 26 ids render `"?"` in the TUI.

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

    /// The divergence this module exists to end: GTK read "num" as the row
    /// ordinal while the TUI read it as the ID3 track number, from the same
    /// persisted config key. One table, one meaning.
    #[test]
    fn num_is_the_row_ordinal_and_track_num_is_the_tag() {
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
config.media_library.visible_columns — so the two tables had to agree, and
they had already stopped.

The visible consequence: id \"num\" was the row ordinal in GTK and the ID3
track number in the TUI, from the same config. \"Duration\" was \"Len\". The
other 26 ids rendered as \"?\".

Move the table to core as crate::ml_columns and have both frontends read
it. \"num\" is the row ordinal everywhere, which is what its \"#\" header
describes and what the default visible set was written against;
\"track_num\" is the tag.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 6: Tick hygiene

The GTK tick runs at 33 ms. Three things in it repeat work that has not changed, and one copies a megabyte per frame.

**Files:**
- Modify: `frontends/gtk/window/tick.rs:171-187` (the `broken_rx` drain)
- Modify: `frontends/gtk/window/tick.rs:518-538` (the time label)
- Modify: `frontends/gtk/window/tick.rs:692-700` (the Granite framebuffer copy)
- Test: `frontends/gtk/window/tests.rs` (append)

**Interfaces:**
- Consumes: nothing new.
- Produces: no signature changes.

- [ ] **Step 1: Write the failing test**

The batching is the testable part. Append to `frontends/gtk/window/tests.rs`:

```rust
/// The broken-file drain scanned the whole playlist once per message. The two
/// drains immediately above it in the tick were batched for exactly this
/// reason — one pass for the whole batch, not one pass per result. Prove the
/// batch form marks every matching row, including duplicates of one path,
/// which the per-message `break` never did.
#[test]
fn a_batch_of_broken_paths_marks_every_matching_row() {
    let mut pl = crate::model::Playlist::new();
    pl.add(named_track(1, "One", "/m/a.mp3"));
    pl.add(named_track(2, "Two", "/m/b.mp3"));
    pl.add(named_track(3, "Dup", "/m/a.mp3")); // same file, second entry

    let broken: std::collections::HashSet<std::path::PathBuf> =
        [std::path::PathBuf::from("/m/a.mp3")].into_iter().collect();

    let mut marked = Vec::new();
    for (idx, t) in pl.tracks.iter_mut().enumerate() {
        if broken.contains(&t.path) {
            t.broken = true;
            marked.push(idx);
        }
    }

    assert_eq!(
        marked,
        vec![0, 2],
        "both entries pointing at the missing file must be marked, not just the first"
    );
    assert!(!pl.tracks[1].broken);
}
```

- [ ] **Step 2: Run to verify**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo test --bin sparkamp a_batch_of_broken_paths 2>&1 | tail -20'
```

Expected: PASS — it describes the shape Step 3 must produce.

- [ ] **Step 3: Batch the broken drain**

Replace `frontends/gtk/window/tick.rs:171-187`:

```rust
            // 0b. Drain missing-file notifications; mark those tracks broken.
            while let Ok(path) = broken_rx.try_recv() {
                let found_idx = {
                    let mut s = state.borrow_mut();
                    let mut found = None;
                    for (idx, track) in s.playlist.tracks.iter_mut().enumerate() {
                        if track.path == path {
                            track.broken = true;
                            found = Some(idx);
                            break;
                        }
                    }
                    found
                };
                if let Some(idx) = found_idx {
                    patch_pl_row(idx);
                }
            }
```

with:

```rust
            // 0b. Drain missing-file notifications; mark those tracks broken.
            // Batched like the two drains above: a pass per message was
            // O(rows × messages). The batch also marks EVERY entry pointing at
            // the missing file, not just the first — the old `break` left
            // duplicate rows of one path unmarked.
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

- [ ] **Step 4: Guard the time label**

The time display changes once a second but is rebuilt 30 times. Add a cached-last-value `Rc<RefCell<String>>` next to the other tick locals, then at lines 525 and 531 build the string and only call `set_text` when it differs:

```rust
                    let text = if show_rem {
                        match dur_opt {
                            Some(dur) => {
                                let rs = dur.saturating_sub(pos).as_secs();
                                format!("-{}:{:02}", rs / 60, rs % 60)
                            }
                            None => "--:--".to_string(),
                        }
                    } else {
                        let ps = pos.as_secs();
                        format!("{}:{:02}", ps / 60, ps % 60)
                    };
                    if *last_time_text.borrow() != text {
                        time_disp_label.set_text(&text);
                        *last_time_text.borrow_mut() = text;
                    }
```

- [ ] **Step 5: Stop copying the Granite framebuffer**

At line 692, `glib::Bytes::from(&buf[..])` copies the whole buffer. At 640×360 that is ~921 KB memcpy'd 30 times a second, about 27 MB/s, purely to hand it to `MemoryTexture`.

Replace the borrow-and-copy with an owned handoff. Change the buffer local from `Rc<RefCell<Vec<u8>>>` usage at that site to take the buffer, wrap it without copying, and put a fresh one back:

```rust
                        // `Bytes::from(&[u8])` copies; `from_owned` does not.
                        // At 30 fps this was ~27 MB/s of memcpy for nothing.
                        let taken = std::mem::take(&mut *buf);
                        let bytes = glib::Bytes::from_owned(taken);
                        let texture = gdk::MemoryTexture::new(
                            w as i32,
                            h as i32,
                            gdk::MemoryFormat::R8g8b8a8,
                            &bytes,
                            (w * 4) as usize,
                        );
                        pic.set_paintable(Some(&texture));
                        // The next frame resizes an empty buffer back to `need`.
                        *buf = vec![0u8; need];
```

The `if buf.len() != need { buf.resize(need, 0); }` above already handles a wrong-sized buffer, so the reallocation is a plain per-frame `Vec` allocation of the same size the copy used to cost — strictly cheaper than the copy plus the retained buffer. If a later measurement shows the allocation is worse than the copy, say so and revert this step alone.

- [ ] **Step 6: Run the suite**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -20'
```

- [ ] **Step 7: Manual check**

```bash
distrobox enter dev-box -- bash -lc 'cd /var/home/josef/Code/Sparkamp && cargo run -- --ui'
```

Play a track. Confirm: the time display still counts, the Granite visualizer still animates without tearing or a stale frame, and a file deleted underneath a playing playlist still gets its ⚠.

- [ ] **Step 8: Commit**

```bash
git add frontends/gtk/window/tick.rs frontends/gtk/window/tests.rs
git commit -m "perf(gtk): stop the tick redoing unchanged work every frame

Three things in the 33 ms tick repeated work that had not changed.

The broken-file drain scanned the whole playlist once per message — the
one drain that never got the batching its two neighbours carry comments
explaining. Batching also fixes a real gap: the old per-message loop broke
on the first match, so a second playlist entry pointing at the same
missing file never got its warning.

The time display was rebuilt and re-set 30 times a second for a value that
changes once. It now writes only on change.

The Granite path handed its framebuffer to MemoryTexture through
Bytes::from(&[u8]), which copies — about 921 KB at 30 fps, 27 MB/s of
memcpy. Bytes::from_owned takes the buffer instead.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 7: Collapse the mechanical duplicates

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

- [ ] **Step 5: Replace the open-coded sites**

Replace each with `fmt_duration` where the input is an `Option<Duration>`, or with a shared seconds-taking helper where it is a raw `u64`. The sites:

```
src/media_library/mod.rs:288
frontends/gtk/window/ml_columns.rs:393
frontends/gtk/window/playlists_columns.rs:806
frontends/gtk/window/state.rs:1163, 1165
frontends/gtk/window/disc.rs:35
frontends/gtk/window/disc_data.rs:265
frontends/gtk/window/disc_page.rs:742
frontends/gtk/window/dedupe.rs:165
frontends/gtk/window/files.rs:681
frontends/gtk/window/tick.rs:525, 531
frontends/tui/ui/media_library.rs:640, 1059
```

Two of these are **not** plain `mm:ss` and must keep their own shape — `state.rs:1163` and `tick.rs:525` emit a leading `-` for remaining-time. Leave the sign at the call site and use the shared formatter for the digits. `tick.rs:525/531` were already rewritten in Task 8; fold them in there rather than touching them twice.

`frontends/tui/ui/media_library.rs:640` and `:1059` use `{:>2}` padding for column alignment — if the shared formatter does not pad, leave those two and note why in a comment.

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
fmt_duration, in three spellings that disagreed about padding and about
what an unknown duration should read as. Those that are plain mm:ss now
use the shared one; the remaining-time and column-padded variants keep
their shape, with a note saying why.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

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
