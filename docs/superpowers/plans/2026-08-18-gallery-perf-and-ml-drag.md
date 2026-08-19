# Album Gallery Performance & Unified ML Drag-and-Drop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the album gallery open near-instantly, and make every Media Library view draggable onto the active playlist with one shared rule for append-vs-replace.

**Architecture:** Two independent tracks. Track A removes wasted work in the gallery's load path (a doubled rebuild, a per-item store fill, a re-query on re-entry) and moves the album fold from a 36k-row Rust loop into a SQL `GROUP BY`. Track B introduces one core rule for `playlist_add_behavior`, converges the five GTK sites that each re-implement it, and adds a shared GTK drag-source helper applied to every ML view — carrying a Sparkamp-specific payload alongside `FileList` so `cdda://` disc tracks can travel too.

**Tech Stack:** Rust 2024, GTK4 (gtk4-rs 0.9), rusqlite 0.31 (SQLite, WAL), Ratatui 0.29, Swift/SwiftUI (macOS frontend).

**Spec:** No separate spec file. This plan implements decisions taken in conversation on 2026-08-18; they are reproduced verbatim under "Decisions" below so the plan is self-contained.

## Global Constraints

- Build and test **only** inside the `dev-box` distrobox: `distrobox enter dev-box -- sh -c 'cargo build && cargo test'`. The host lacks `gstreamer-video-1.0` dev packages.
- `cargo build && cargo test` must finish with **zero warnings and zero failures** before any task is considered done.
- Do not add new `clippy` warnings in files you touch. Pre-existing ones elsewhere are out of scope.
- Ask before refactoring beyond a task's stated scope. Focus on the requested change; avoid over-engineering.
- GTK strings from metadata/errors go through `gtk_safe()` to strip NUL bytes.
- Paths use `.canonicalize()`. `MediaLibrary` (SQLite) is **not** `Send`.
- **Deletion Rule:** permanently deleting a music file from disk is allowed ONLY from the Media Library file view or the Media Library external-device view, and ONLY after explicit user confirmation. Removing a track from the active playlist or any saved playlist must only remove it from that list. Nothing in this plan deletes anything — if a task appears to, stop and ask.
- Never `git push`. Commit only.
- `~/Music` is a symlink to `/mnt/Blackbeard/Music`, which distrobox mounts **read-only**. A write failing there with `EROFS` is the mount, not a bug.

## Decisions (from the 2026-08-18 conversation)

1. **Replace clears first, then adds.** A Replace-configured drop discards the drop position: clear the playlist, then add the dropped tracks.
2. **File-manager drops respect the flag too.** There is no expectation that the file manager knows anything; the active playlist window applies the rule to any incoming file list. This means the drop handler needs **no** ML-vs-external marker.
3. **Containers expand to all their files.** Dragging an album, a saved playlist, a disc, or a device adds every track it contains.
4. Issue 3 takes **both** option A (remove waste) and option C (fold in SQL).
5. Issue 2 takes **option 1** (core funnel + all missing drag sources).

## Measured baseline (real library, 2026-08-18)

```
album_rows(): 106.717ms for 36,330 rows
albums():     131.593ms for 5,158 groups
groups with artwork_path: 4,753
```

`show_gallery_overview()` runs **twice** per sidebar click, so a nav click costs ~263 ms of query alone, plus 2 × 5,158 individual `ListStore::append` calls.

## File Structure

**Track A**
- Modify `src/media_library/queries.rs` — replace the `album_rows` + Rust fold with a SQL `GROUP BY`; keep `effective_album_artist` resolution in Rust. Holds the `gallery_cost_probe` measurement harness.
- Modify `frontends/gtk/window/albums.rs` — stop the doubled `show_gallery_overview()`.
- Modify `frontends/gtk/window/album_gallery.rs` — `splice` the store; cache the folded list across re-entry.

**Track B**
- Create `src/playlist_add.rs` — the shared append-vs-replace rule (`AddMode`, `should_replace`). Core, no state, unit-tested.
- Create `frontends/gtk/window/ml_drag.rs` — `SPARKAMP_TRACKS_MIME`, `attach_track_drag`, `payload_uris`. One helper every ML view calls.
- Modify `frontends/gtk/window/playlist_add.rs` — `add_uris_with_mode`, the single GTK entry point that stops, clears, adds and autoplays.
- Modify `frontends/gtk/window/dnd.rs` — drop handler applies the rule; accepts the Sparkamp payload as well as `FileList`.
- Modify `frontends/gtk/window/{files,disc_page,disc_data,playlists_menu,tick}.rs` — converge the five hand-rolled behavior checks.
- Modify `frontends/gtk/window/{album_gallery,disc_page,devices_page,playlists}.rs` — attach drag sources.
- Modify `src/ffi/playlist.rs` — expose the rule so macOS stops keeping its own copy.
- Modify `frontends/SparkampMac/Sources/SparkampModel+Transport.swift` — route `addFiles` through the FFI rule.

---

# Track A — Album gallery performance

### Task 1 (A1): Pin the current album fold with a characterization test

The fold is about to be rewritten in SQL. Before touching it, lock in what it currently returns so the rewrite can be proven equivalent rather than assumed so.

**Files:**
- Modify: `src/media_library/queries.rs` (test module at the end)

**Interfaces:**
- Consumes: `MediaLibrary::albums(AlbumSort, bool)`, `AlbumGroup`, the existing private `insert_track` test helper.
- Produces: `fold_fixture(&MediaLibrary)` — inserts a fixed 12-track fixture exercising blank albums, case variance, the album-artist toggle, missing years and artwork. Task 2 (A2) reuses it unchanged.

- [ ] **Step 1: Write the characterization test**

Add to the `mod tests` block in `src/media_library/queries.rs`:

```rust
    /// A fixture that exercises every branch of the album fold: two real
    /// albums, one of them spelled with different case across its tracks, a
    /// track whose album_artist is blank (so the F12.2 toggle decides its
    /// group), two blank-album tracks for the bucket, a track with no year,
    /// and artwork on only the second track of an album.
    fn fold_fixture(lib: &MediaLibrary) {
        // (path, filename, artist, album, album_artist, track, disc, year, art)
        let rows: &[(&str, &str, &str, &str, &str, i64, i64, Option<i64>, Option<&str>)] = &[
            ("/m/a1.mp3", "a1.mp3", "Ward Thomas", "Liberation", "Ward Thomas", 1, 1, Some(2017), None),
            ("/m/a2.mp3", "a2.mp3", "Ward Thomas", "liberation", "Ward Thomas", 2, 1, Some(2017), Some("/art/lib.jpg")),
            ("/m/a3.mp3", "a3.mp3", "Ward Thomas", "LIBERATION", "Ward Thomas", 3, 1, Some(2016), None),
            ("/m/b1.mp3", "b1.mp3", "Pink Floyd", "Animals", "Pink Floyd", 1, 1, Some(1977), Some("/art/an.jpg")),
            ("/m/b2.mp3", "b2.mp3", "Pink Floyd", "Animals", "Pink Floyd", 2, 1, None, None),
            ("/m/c1.mp3", "c1.mp3", "Solo Artist", "Only Album", "", 1, 1, Some(2001), None),
            ("/m/c2.mp3", "c2.mp3", "Solo Artist", "Only Album", "", 2, 1, Some(2001), None),
            ("/m/d1.mp3", "d1.mp3", "Nobody", "", "", 1, 1, Some(1999), None),
            ("/m/d2.mp3", "d2.mp3", "Someone Else", "", "", 1, 1, None, None),
            ("/m/d3.mp3", "d3.mp3", "Third", "   ", "", 1, 1, Some(2020), None),
            ("/m/e1.mp3", "e1.mp3", "VA Artist One", "Sampler", "Various Artists", 1, 1, Some(2010), None),
            ("/m/e2.mp3", "e2.mp3", "VA Artist Two", "Sampler", "Various Artists", 2, 1, Some(2010), None),
        ];
        for (p, f, ar, al, aa, tn, dn, y, art) in rows {
            insert_track(lib, p, f, ar, al, aa, Some(*tn), Some(*dn), *y, *art);
        }
    }

    /// Every field of every group the fold produces, pinned exactly.
    ///
    /// Written before the SQL rewrite so the rewrite can be proved equivalent
    /// rather than eyeballed. Covers: case-insensitive grouping, the earliest
    /// year winning, the first non-null artwork winning, the blank/whitespace
    /// album bucket collapsing to one group sorted last, and the F12.2
    /// album-artist fallback under both settings.
    #[test]
    fn the_album_fold_groups_exactly_as_specified() {
        let (lib, _db) = temp_lib();
        fold_fixture(&lib);

        let got = lib.albums(AlbumSort::Artist, false).unwrap();
        let summary: Vec<(String, String, Option<i64>, i64, Option<String>, bool)> = got
            .iter()
            .map(|g| {
                (
                    g.album.clone(),
                    g.album_artist.clone(),
                    g.year,
                    g.track_count,
                    g.artwork_path.clone(),
                    g.is_no_album,
                )
            })
            .collect();

        assert_eq!(
            summary,
            vec![
                ("Animals".into(), "Pink Floyd".into(), Some(1977), 2, Some("/art/an.jpg".into()), false),
                ("Only Album".into(), "Solo Artist".into(), Some(2001), 2, None, false),
                ("Sampler".into(), "Various Artists".into(), Some(2010), 2, None, false),
                ("Liberation".into(), "Ward Thomas".into(), Some(2016), 3, Some("/art/lib.jpg".into()), false),
                (String::new(), String::new(), None, 3, None, true),
            ],
            "album fold changed shape"
        );
    }

    /// The blank-album bucket is always last, whichever sort is asked for.
    #[test]
    fn the_no_album_bucket_sorts_last_under_every_sort() {
        let (lib, _db) = temp_lib();
        fold_fixture(&lib);
        for sort in [AlbumSort::Artist, AlbumSort::Album, AlbumSort::Year] {
            let got = lib.albums(sort, false).unwrap();
            assert!(
                got.last().unwrap().is_no_album,
                "bucket not last under {sort:?}"
            );
            assert_eq!(
                got.iter().filter(|g| g.is_no_album).count(),
                1,
                "bucket split under {sort:?}"
            );
        }
    }

    /// With the F12.2 toggle on, a track with no album_artist groups under its
    /// artist instead. The Sampler's two tracks have different artists, so the
    /// toggle must NOT split it — its album_artist is set.
    #[test]
    fn the_album_artist_toggle_only_moves_tracks_that_lack_one() {
        let (lib, _db) = temp_lib();
        fold_fixture(&lib);
        let on = lib.albums(AlbumSort::Artist, true).unwrap();
        let sampler = on.iter().find(|g| g.album == "Sampler").unwrap();
        assert_eq!(sampler.track_count, 2, "Sampler must not split");
        assert_eq!(sampler.album_artist, "Various Artists");
        let only = on.iter().find(|g| g.album == "Only Album").unwrap();
        assert_eq!(only.album_artist, "Solo Artist");
    }
```

- [ ] **Step 2: Run the tests and verify they pass against today's code**

Run: `distrobox enter dev-box -- sh -c 'cargo test --lib media_library::queries::tests -- --nocapture'`
Expected: PASS. These describe current behaviour — if any fails, the assertion is wrong, not the code. Fix the assertion to match what today's fold actually returns, and note the correction in the commit message.

- [ ] **Step 3: Commit**

```bash
git add src/media_library/queries.rs
git commit -m "test(library): pin the album fold before rewriting it in SQL"
```

---

### Task 2 (A2): Fold albums in SQL instead of reading 36k rows

`album_rows` currently `SELECT`s all 36,330 rows with an unindexable four-expression `ORDER BY LOWER(...)`, then folds them in a Rust `HashMap`. Group in SQLite instead and return ~5,158 rows.

The F12.2 toggle stays in Rust. `effective_album_artist` is the single source of truth (`queries.rs` documents this) and SQL must not re-derive it — so SQL groups on the raw `(album, album_artist, artist)` triple and Rust folds that much smaller set into final groups.

**Files:**
- Modify: `src/media_library/queries.rs:675-693` (`album_rows`) and `:703-…` (`albums`)
- Test: `src/media_library/queries.rs` test module (Task 1 (A1)'s tests, unchanged)

**Interfaces:**
- Consumes: `fold_fixture`, the three tests from Task 1 (A1).
- Produces: `album_rows` returning pre-aggregated rows; `AlbumRow` gains `track_count: i64`. `albums()` keeps its exact signature `albums(&self, sort: AlbumSort, artist_as_album: bool) -> Result<Vec<AlbumGroup>>`.

- [ ] **Step 1: Widen `AlbumRow` to carry a count**

Replace the `AlbumRow` struct in `src/media_library/queries.rs`:

```rust
/// Lean per-*group* projection used only to fold rows into [`AlbumGroup`]s.
///
/// One row per distinct `(album, album_artist, artist)` triple, not per track:
/// SQLite does the counting and the year/artwork picking, and Rust only has to
/// merge the triples that the F12.2 toggle collapses together. Over the
/// 36,330-track library this is ~5,200 rows instead of 36,330.
struct AlbumRow {
    artist: String,
    album: String,
    album_artist: String,
    year: Option<i64>,
    artwork_path: Option<String>,
    track_count: i64,
}
```

- [ ] **Step 2: Replace `album_rows` with the grouped query**

```rust
    /// Pre-aggregate the tracks table into one row per
    /// `(album, album_artist, artist)` triple.
    ///
    /// Grouping is case-insensitive on album and album_artist, matching the
    /// Rust fold this replaced. `artist` is in the key only because the F12.2
    /// toggle may promote it to the group's album-artist; when the toggle is
    /// off, [`MediaLibrary::albums`] merges those rows back together.
    ///
    /// `MIN(year)` and the artwork pick happen here rather than in Rust.
    /// Artwork uses `MIN(artwork_path)` — a deterministic choice among the
    /// group's non-null paths, replacing "first in a four-expression
    /// ORDER BY", which cost a full unindexable sort of every track row to
    /// decide. The chosen path can differ from the old one for an album whose
    /// tracks carry different artwork; both are valid cover art for that
    /// album, and no ordering guarantee was ever documented.
    ///
    /// NULLs are folded to '' by COALESCE so the GROUP BY key matches the
    /// Rust-side `trim()` handling of blank albums.
    fn album_rows(&self) -> Result<Vec<AlbumRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(artist,''), COALESCE(album,''), COALESCE(album_artist,''),
                    MIN(year), MIN(artwork_path), COUNT(*)
             FROM tracks
             GROUP BY LOWER(TRIM(COALESCE(album,''))),
                      LOWER(TRIM(COALESCE(album_artist,''))),
                      LOWER(TRIM(COALESCE(artist,'')))",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(AlbumRow {
                artist: r.get(0)?,
                album: r.get(1)?,
                album_artist: r.get(2)?,
                year: r.get(3)?,
                artwork_path: r.get(4)?,
                track_count: r.get(5)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }
```

- [ ] **Step 3: Teach the fold to add counts rather than increment**

In `MediaLibrary::albums`, replace the single line

```rust
            acc.track_count += 1;
```

with

```rust
            // `+=` not `+= 1`: each row now stands for a whole
            // (album, album_artist, artist) triple, and several triples merge
            // into one group when the F12.2 toggle promotes an artist.
            acc.track_count += row.track_count;
```

- [ ] **Step 4: Run the characterization tests**

Run: `distrobox enter dev-box -- sh -c 'cargo test --lib media_library::queries::tests'`
Expected: PASS — all three Task 1 (A1) tests, unchanged.

If `the_album_fold_groups_exactly_as_specified` fails on `artwork_path` only, that is the documented `MIN(artwork_path)` change. Confirm the new value is one of the group's real artwork paths, then update that one field in the fixture's expectation and say so in the commit message. Any other difference is a bug — fix the code, not the test.

- [ ] **Step 5: Re-measure against the real library**

Run: `distrobox enter dev-box -- sh -c 'cargo test --lib gallery_cost_probe -- --ignored --nocapture'`
Expected: `albums()` well under the 131.6 ms baseline, and the group count still 5,158. A different group count means the SQL grouping key disagrees with the old Rust key — stop and investigate.

- [ ] **Step 6: Add the covering index**

Add to the migration/schema setup in `src/media_library/mod.rs`, alongside the existing `CREATE INDEX` statements:

```sql
CREATE INDEX IF NOT EXISTS idx_tracks_album_group
    ON tracks(album, album_artist, artist);
```

- [ ] **Step 7: Re-measure and verify the index is used**

Run: `distrobox enter dev-box -- sh -c 'cargo test --lib gallery_cost_probe -- --ignored --nocapture'`
Expected: same group count, timing no worse than Step 5. Record both numbers for the commit message.

- [ ] **Step 8: Full suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test'`
Expected: zero warnings, zero failures.

- [ ] **Step 9: Commit**

```bash
git add src/media_library/queries.rs src/media_library/mod.rs
git commit -m "perf(library): fold albums in SQL instead of reading every track"
```

---

### Task 3 (A3): Stop rebuilding the gallery twice per click

`albums.rs` hooks both `connect_row_selected` and `connect_row_activated` on the "albums" sidebar row. Both fire for one click on a row that was not already selected, so `show_gallery_overview()` runs twice — two full `albums()` queries and two full store repopulations. The existing comment calls this "idempotent", which it is, and also not free.

**Files:**
- Modify: `frontends/gtk/window/albums.rs:143-171`

**Interfaces:**
- Consumes: `show_gallery_overview: Rc<dyn Fn()>`.
- Produces: nothing new. Behaviour is unchanged; only the duplicate call is removed.

- [ ] **Step 1: Split navigation from the rebuild**

`show_gallery_overview` (declared just above the sidebar-routing block, ~`:122-135`) does four things: clear `album_filter`, hide `btn_album_back`, switch the stack to "albums", and call `rebuild_gallery()`. Only the fourth is expensive. Split it so the cheap three can run on every signal while the expensive one is coalesced.

Keep `show_gallery_overview` exactly as it is — the back button at `:138` calls it and fires once, so it needs no coalescing — and add a navigation-only sibling next to it:

```rust
    // The cheap half of `show_gallery_overview`: everything except the
    // rebuild. Split out because the two sidebar signals below both fire for
    // one click and only the rebuild is worth collapsing — deferring the
    // stack switch as well would leave the previous page on screen until the
    // idle callback ran, turning a doubled query into a visible lag.
    let navigate_to_gallery: Rc<dyn Fn()> = {
        let album_filter_nav = ctx.album_filter.clone();
        let btn_album_back_nav = ctx.btn_album_back.clone();
        let stack_nav = ctx.stack.clone();
        Rc::new(move || {
            {
                *album_filter_nav.borrow_mut() = None;
            }
            btn_album_back_nav.set_visible(false);
            stack_nav.set_visible_child_name("albums");
        })
    };
```

- [ ] **Step 2: Coalesce the two signals**

Replace the two handler blocks in `frontends/gtk/window/albums.rs` with:

```rust
    // Both signals are needed and both can fire for one click.
    //
    // `row-selected` is the normal arrival from another sidebar row.
    // `row-activated` is the only one that fires when "Albums" is clicked
    // while already selected — which happens on the way back from a
    // drill-down, since the highlight never left "Albums" (see
    // `on_album_activate` above).
    //
    // When the user arrives from another row they BOTH fire, and
    // `show_gallery_overview` re-queries the whole library and repopulates
    // 5,000-odd tiles.
    //
    // Only the REBUILD is coalesced, not the whole of
    // `show_gallery_overview`. Clearing the filter, hiding the back button and
    // switching the stack are cheap, idempotent, and what makes the click feel
    // instant — deferring those to idle would leave the previous page on
    // screen until the callback ran. So the navigation stays synchronous on
    // every signal and only the expensive half collapses to one call.
    {
        let navigate = navigate_to_gallery.clone();
        let rebuild_coalesced = rebuild_gallery.clone();
        let queued: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let show_once: Rc<dyn Fn()> = Rc::new(move || {
            // Synchronous every time — cheap, idempotent, and what makes the
            // click feel immediate.
            navigate();
            // Coalesced — the second signal of the same click finds the flag
            // already set and books no second rebuild.
            if queued.replace(true) {
                return;
            }
            let rebuild = rebuild_coalesced.clone();
            let queued = queued.clone();
            glib::idle_add_local_once(move || {
                queued.set(false);
                rebuild();
            });
        });

        {
            let show_once = show_once.clone();
            sb.list.connect_row_selected(move |_, opt_row| {
                let Some(row) = opt_row else { return };
                if row.widget_name() == "albums" {
                    show_once();
                }
            });
        }
        {
            let show_once = show_once.clone();
            sb.list.connect_row_activated(move |_, row| {
                if row.widget_name() == "albums" {
                    show_once();
                }
            });
        }
    }
```

- [ ] **Step 3: Verify `Cell` is in scope**

Run: `distrobox enter dev-box -- sh -c 'cargo build 2>&1 | head -20'`
Expected: clean. If `Cell` is unresolved, add `use std::cell::Cell;` at the top of `frontends/gtk/window/albums.rs`.

- [ ] **Step 4: Instrument to prove the doubling is gone**

Temporarily add as the first line of the `rebuild` closure in `frontends/gtk/window/album_gallery.rs`:

```rust
            eprintln!("[gallery] rebuild at {:?}", std::time::Instant::now());
```

- [ ] **Step 5: Run the app and count rebuilds**

Run: `distrobox enter dev-box -- sh -c 'cargo run 2>&1 | grep "\[gallery\] rebuild"'`

Open the Media Library, click "Files", then click "Albums". Expected: **exactly one** `[gallery] rebuild` line per Albums click. Before this task it was two. Then drill into an album and click "Albums" again — expected one more line.

- [ ] **Step 6: Remove the instrumentation**

Delete the `eprintln!` added in Step 4.

- [ ] **Step 7: Full suite and commit**

```bash
distrobox enter dev-box -- sh -c 'cargo build && cargo test'
git add frontends/gtk/window/albums.rs
git commit -m "perf(gtk): rebuild the album gallery once per click, not twice"
```

---

### Task 4 (A4): Splice the gallery store and cache the fold across re-entry

Two remaining costs: the store is filled with one `append` per album (5,158 `items-changed` emissions, each invalidating the `GridView`), and every return to the gallery re-runs `albums()` even when nothing changed.

**Files:**
- Modify: `frontends/gtk/window/album_gallery.rs` (the `refilter` and `rebuild` closures added on 2026-08-17)

**Interfaces:**
- Consumes: `all_albums: Rc<RefCell<Vec<AlbumGroup>>>`, `query: Rc<RefCell<String>>`, `store: gio::ListStore`.
- Produces: `rebuild` keeps its `Rc<dyn Fn()>` type and its meaning to callers. A new `invalidate_albums: Rc<dyn Fn()>` is returned as the third tuple element so scan/watch paths can force a re-query.

- [ ] **Step 1: Replace the append loop with one splice**

In `frontends/gtk/window/album_gallery.rs`, replace the body of the `refilter` closure:

```rust
        Rc::new(move || {
            // Collected before touching the store: appending drives the
            // GridView's bind closure, and no RefCell borrow may be live
            // across a UI call (see the `bind` handler above).
            let matching: Vec<glib::BoxedAnyObject> = {
                let q = query.borrow();
                all_albums
                    .borrow()
                    .iter()
                    .filter(|a| a.matches(&q))
                    .map(|a| glib::BoxedAnyObject::new(a.clone()))
                    .collect()
            };
            // One `items-changed` for the whole set. Appending album by album
            // emitted one per tile — 5,158 of them on this library — and the
            // GridView re-examined its model after each. `files.rs` splices
            // its track store for the same reason.
            store.splice(0, store.n_items(), &matching);
        })
```

- [ ] **Step 2: Cache the fold across re-entry**

Change the `rebuild` closure so it only re-queries when the cache is empty or has been invalidated:

```rust
    // Set when something outside the gallery changed the library, so the next
    // `rebuild()` re-queries instead of trusting `all_albums`.
    let albums_stale: Rc<Cell<bool>> = Rc::new(Cell::new(true));

    let rebuild: Rc<dyn Fn()> = {
        let state = state.clone();
        let sort_dd = sort_dd.clone();
        let all_albums = all_albums.clone();
        let refilter = refilter.clone();
        let albums_stale = albums_stale.clone();
        let last_sort: Rc<Cell<u32>> = Rc::new(Cell::new(u32::MAX));
        Rc::new(move || {
            ensure_media_lib_open(&state);
            let sort_idx = sort_dd.selected();
            // The fold is the expensive half — 36,330 rows into ~5,158 groups.
            // Returning to the gallery from a drill-down changes neither the
            // library nor the sort, so the previous answer still stands and
            // only the filter needs reapplying.
            let must_query = albums_stale.get() || last_sort.get() != sort_idx;
            if must_query {
                let sort = gallery_sort_from_idx(sort_idx);
                let albums: Vec<crate::media_library::AlbumGroup> = {
                    let s = state.borrow();
                    let artist_as_album = s.config.media_library.artist_as_album_artist;
                    s.media_lib
                        .as_ref()
                        .and_then(|lib| lib.albums(sort, artist_as_album).ok())
                        .unwrap_or_default()
                };
                *all_albums.borrow_mut() = albums;
                albums_stale.set(false);
                last_sort.set(sort_idx);
            }
            refilter();
        })
    };

    // Handed to the caller so a scan, a watch-folder event or a tag edit can
    // drop the cache. Without it the gallery would keep showing a fold taken
    // before the library changed.
    let invalidate_albums: Rc<dyn Fn()> = {
        let albums_stale = albums_stale.clone();
        Rc::new(move || albums_stale.set(true))
    };
```

- [ ] **Step 3: Return the invalidator**

Change the function's return type and final expression:

```rust
) -> (gtk4::Widget, Rc<dyn Fn()>, Rc<dyn Fn()>) {
```

```rust
    (page.upcast::<gtk4::Widget>(), rebuild, invalidate_albums)
```

Update the doc comment's `Returns` line to name the third element.

- [ ] **Step 4: Update the one caller**

In `frontends/gtk/window/albums.rs`, change the destructuring at the `build_album_gallery` call:

```rust
    let (gallery_page, rebuild_gallery, invalidate_gallery): (
        gtk4::Widget,
        Rc<dyn Fn()>,
        Rc<dyn Fn()>,
    ) = {
```

- [ ] **Step 5: Wire the invalidator to the rebuild-ML seam**

The scan and watch paths already go through `state.borrow().rebuild_ml_callback`. In `frontends/gtk/window/albums.rs`, after `ctx.stack.add_named(&gallery_page, Some("albums"));`, chain the invalidator onto that callback:

```rust
    // A scan, a watch-folder event or a tag edit fires `rebuild_ml_callback`.
    // The gallery caches its fold, so it has to be told the fold is stale —
    // otherwise the grid keeps showing albums as they were before the scan.
    {
        let prev = ctx.host.state.borrow().rebuild_ml_callback.clone();
        let invalidate = invalidate_gallery.clone();
        let chained: Rc<dyn Fn()> = Rc::new(move || {
            invalidate();
            if let Some(prev) = prev.as_ref() {
                prev();
            }
        });
        ctx.host.state.borrow_mut().rebuild_ml_callback = Some(chained);
    }
```

- [ ] **Step 6: Build**

Run: `distrobox enter dev-box -- sh -c 'cargo build 2>&1 | head -20'`
Expected: clean. If `rebuild_ml_callback` has a different type than `Option<Rc<dyn Fn()>>`, read its declaration in `frontends/gtk/window/state.rs` and match it.

- [ ] **Step 7: Verify by hand**

Run: `distrobox enter dev-box -- sh -c 'cargo run'`

1. Open the Media Library, click Albums. Note how long the grid takes.
2. Click Files, click Albums again. Expected: **immediate** — no re-query.
3. Drill into an album, click Albums. Expected: immediate.
4. Change the sort dropdown. Expected: re-queries, grid reorders.
5. Rescan a folder, then click Albums. Expected: re-queries, and any newly scanned album appears.

Step 5 is the one that catches a broken invalidator. If a new album does not appear, the chaining in Step 6 is wrong.

- [ ] **Step 8: Full suite and commit**

```bash
distrobox enter dev-box -- sh -c 'cargo build && cargo test'
git add frontends/gtk/window/album_gallery.rs frontends/gtk/window/albums.rs
git commit -m "perf(gtk): splice the gallery store, and keep its fold across re-entry"
```

---

# Track B — Unified ML drag-and-drop

### Task 5 (B1): One core rule for append-vs-replace

Five GTK sites each read `config.behavior.playlist_add_behavior` and decide independently; macOS has a sixth copy in Swift. Put the rule in core with tests, then converge the callers.

**Files:**
- Create: `src/playlist_add.rs`
- Modify: `src/lib.rs` (add `pub mod playlist_add;`)

**Interfaces:**
- Consumes: `crate::config::PlaylistAddBehavior`.
- Produces:
  - `pub enum AddMode { Behavior, Enqueue, Replace }`
  - `pub fn should_replace(behavior: &PlaylistAddBehavior, mode: AddMode) -> bool`

  Task B2 (GTK), Task B6 (FFI) and Task B7 (macOS) all call `should_replace`.

- [ ] **Step 1: Write the failing test**

Create `src/playlist_add.rs`:

```rust
//! The one rule for whether adding tracks to the active playlist replaces
//! what is there or appends to it.
//!
//! Lives in core because every frontend needs the same answer and each had
//! been deciding for itself: five GTK sites, one Swift copy in
//! `SparkampModel+Transport.swift`, and the drag-and-drop drop handler which
//! did not consult the setting at all.

use crate::config::PlaylistAddBehavior;

/// Why tracks are being added, which decides whether the configured
/// preference applies at all.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AddMode {
    /// Honour the user's `playlist_add_behavior` setting. Drag-and-drop,
    /// double-click, and the plain "Add" buttons all use this.
    Behavior,
    /// Always append, whatever the setting says — an explicit "Enqueue"
    /// action has already told us what the user wants.
    Enqueue,
    /// Always replace — an explicit "Play now" action.
    Replace,
}

/// Whether this add should clear the playlist before adding.
///
/// A Replace discards any drop position: the playlist is cleared and the new
/// tracks become the whole of it. That is the decision recorded on
/// 2026-08-18 — "replace clears first then adds".
pub fn should_replace(behavior: &PlaylistAddBehavior, mode: AddMode) -> bool {
    match mode {
        AddMode::Replace => true,
        AddMode::Enqueue => false,
        AddMode::Behavior => *behavior == PlaylistAddBehavior::Replace,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_enqueue_appends_even_when_the_setting_says_replace() {
        assert!(!should_replace(&PlaylistAddBehavior::Replace, AddMode::Enqueue));
    }

    #[test]
    fn an_explicit_play_now_replaces_even_when_the_setting_says_append() {
        assert!(should_replace(&PlaylistAddBehavior::Append, AddMode::Replace));
    }

    #[test]
    fn the_default_mode_follows_the_setting() {
        assert!(should_replace(&PlaylistAddBehavior::Replace, AddMode::Behavior));
        assert!(!should_replace(&PlaylistAddBehavior::Append, AddMode::Behavior));
    }
}
```

- [ ] **Step 2: Register the module**

Add to `src/lib.rs`, in alphabetical position among the existing `pub mod` lines:

```rust
pub mod playlist_add;
```

- [ ] **Step 3: Run the tests**

Run: `distrobox enter dev-box -- sh -c 'cargo test --lib playlist_add'`
Expected: 3 passed.

- [ ] **Step 4: Commit**

```bash
git add src/playlist_add.rs src/lib.rs
git commit -m "feat(core): one rule for whether an add replaces the playlist"
```

---

### Task 6 (B2): One GTK entry point that applies the rule

**Files:**
- Modify: `frontends/gtk/window/playlist_add.rs`

**Interfaces:**
- Consumes: `crate::playlist_add::{AddMode, should_replace}`, the existing `add_paths(state, &[PathBuf]) -> Added`.
- Produces: `pub(super) fn add_with_mode(state: &Rc<RefCell<AppState>>, paths: &[PathBuf], mode: AddMode) -> Added`. Tasks 7 (B3) and B5 call it.

- [ ] **Step 1: Write the entry point**

Append to `frontends/gtk/window/playlist_add.rs`:

```rust
/// Add `paths` to the active playlist, applying the append-vs-replace rule.
///
/// The single place in the GTK frontend that turns
/// `config.behavior.playlist_add_behavior` into an action. Five call sites
/// each used to read the setting, stop the player and clear the playlist
/// themselves; the drop handler in `dnd.rs` did none of it, so a
/// Replace-configured drag appended.
///
/// A replace stops the player first. Clearing the playlist out from under a
/// playing track leaves the engine holding a file the playlist no longer
/// lists, and the tick loop then reports a position in a track the user
/// cannot see.
pub(super) fn add_with_mode(
    state: &Rc<RefCell<AppState>>,
    paths: &[std::path::PathBuf],
    mode: crate::playlist_add::AddMode,
) -> Added {
    let replace = {
        let s = state.borrow();
        crate::playlist_add::should_replace(&s.config.behavior.playlist_add_behavior, mode)
    };
    if replace {
        // Fresh borrow per line — never one held across a player call.
        let _ = state.borrow_mut().player.stop();
        state.borrow_mut().playlist.clear();
    }
    add_paths(state, paths)
}
```

- [ ] **Step 2: Build**

Run: `distrobox enter dev-box -- sh -c 'cargo build 2>&1 | head -20'`
Expected: clean, apart from a dead-code warning for `add_with_mode` until Task 7 (B3) calls it. If the warning appears, leave it — Task 7 (B3) removes it in the same session. If you must commit before then, add `#[allow(dead_code)]` and delete it in B3.

- [ ] **Step 3: Commit**

```bash
git add frontends/gtk/window/playlist_add.rs
git commit -m "feat(gtk): one entry point for adding tracks to the active playlist"
```

---

### Task 7 (B3): The drop handler honours the setting

**Files:**
- Modify: `frontends/gtk/window/dnd.rs:353`

**Interfaces:**
- Consumes: `playlist_add::add_with_mode`, `crate::playlist_add::AddMode`.
- Produces: nothing new.

- [ ] **Step 1: Route the add through the rule**

In `frontends/gtk/window/dnd.rs`, replace:

```rust
            let did_add = playlist_add::add_paths(&state_dnd, &new_paths).any();
```

with:

```rust
            // Honour the append-vs-replace setting — for any file list,
            // whatever produced it. A file manager knows nothing about
            // Sparkamp's preferences, so the playlist window applies them on
            // arrival (decision, 2026-08-18).
            //
            // Only the add half is subject to the rule. `did_move` above is an
            // internal reorder of rows that are already in the playlist, and
            // clearing the playlist to "replace" it with its own rows would
            // delete the very tracks being dragged. A drop that both reorders
            // and adds keeps the reorder and appends, for the same reason.
            let did_add = if did_move {
                playlist_add::add_paths(&state_dnd, &new_paths).any()
            } else {
                playlist_add::add_with_mode(
                    &state_dnd,
                    &new_paths,
                    crate::playlist_add::AddMode::Behavior,
                )
                .any()
            };
```

- [ ] **Step 2: Build and run the suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test'`
Expected: zero warnings, zero failures.

- [ ] **Step 3: Verify by hand — the reorder guard is the risky half**

Run: `distrobox enter dev-box -- sh -c 'cargo run'`

Set Settings → Behavior → "Replace playlist". Then:

1. Add several tracks. Drag one row within the playlist to reorder it. **Expected: the playlist is NOT cleared** — the row moves. This is the case the `did_move` guard protects; if the playlist empties, the guard is wrong.
2. Drag a file from Nautilus onto the playlist. Expected: playlist clears, that file is the only entry.
3. Drag several tracks from the ML Files view. Expected: playlist clears, those tracks are the whole playlist.
4. Switch to "Append". Repeat 2 and 3. Expected: appended, nothing cleared.

- [ ] **Step 4: Commit**

```bash
git add frontends/gtk/window/dnd.rs
git commit -m "fix(gtk): a drop onto the playlist honours the add-behavior setting"
```

---

### Task 8 (B4): A shared drag source for every ML view

Five ML views have no `DragSource`. Linux disc tracks are `cdda://N?device=/dev/srX` pseudo-URIs, which a `gdk::FileList` cannot carry — `gio::File::path()` returns `None` for them and the drop handler filters on `.path()`. So the payload is a Sparkamp-specific string type carrying URIs, offered alongside `FileList` for file-manager interop. macOS already does exactly this with its own UTI (`TrackDragPayload` in `PlaylistView.swift`).

**Files:**
- Create: `frontends/gtk/window/ml_drag.rs`
- Modify: `frontends/gtk/window/mod.rs` (add `mod ml_drag;`)

**Interfaces:**
- Consumes: `gtk4::{DragSource, gdk}`.
- Produces:
  - `pub(super) const SPARKAMP_URIS_MIME: &str = "application/x-sparkamp-uris"`
  - `pub(super) fn attach_uri_drag<W, F>(widget: &W, uris: F)` where `W: IsA<gtk4::Widget>`, `F: Fn() -> Vec<String> + 'static`
  - `pub(super) fn uris_from_value(value: &glib::Value) -> Vec<String>`

  Task 9 (B5) consumes `uris_from_value`; Task 12 (B8) consumes `attach_uri_drag`.

- [ ] **Step 1: Write the helper**

Create `frontends/gtk/window/ml_drag.rs`:

```rust
//! One drag source for every Media Library view.
//!
//! Before this, only the Files table and the playlist editor were draggable;
//! the album gallery, the disc views and the device views were not. Each new
//! source needs the same three things — collect what is selected, publish it,
//! and let the active playlist's drop target read it back — so they share one
//! helper rather than five near-copies.
//!
//! ## Why URIs and not `gdk::FileList`
//!
//! A CD track on Linux is `cdda://5?device=/dev/sr0`, not a file. `FileList`
//! holds `gio::File`s and `dnd.rs` filters them through `.path()`, which is
//! `None` for a `cdda://` URI, so a disc drag carried in a `FileList` would
//! arrive empty. Strings carry both. The provider still offers `FileList` too,
//! so dragging library tracks out to a file manager keeps working.

use super::*;

/// Content type for a Sparkamp drag: URIs, one per line.
///
/// Newline-separated because `gdk::ContentProvider::for_value` takes a single
/// `Value` and a `Vec<String>` has no `ToValue`. URIs cannot contain a raw
/// newline, so the join is unambiguous.
pub(super) const SPARKAMP_URIS_MIME: &str = "application/x-sparkamp-uris";

/// Make `widget` draggable, publishing whatever `uris` returns at drag time.
///
/// `uris` is called on every drag, not once at setup, so it reads the
/// selection as it is when the drag starts.
pub(super) fn attach_uri_drag<W, F>(widget: &W, uris: F)
where
    W: IsA<gtk4::Widget>,
    F: Fn() -> Vec<String> + 'static,
{
    let ds = gtk4::DragSource::new();
    ds.set_actions(gdk::DragAction::COPY);
    ds.connect_prepare(move |_, _, _| {
        let list = uris();
        if list.is_empty() {
            // No selection: refuse the drag rather than starting an empty one.
            return None;
        }
        let joined = list.join("\n");
        let text_provider = gdk::ContentProvider::for_value(&joined.to_value());

        // Offer a FileList as well, for the paths that are real files, so a
        // drag out to a file manager still works. Skipped entirely when the
        // selection is all pseudo-URIs (a CD), since an empty FileList would
        // advertise a type that yields nothing.
        let files: Vec<gio::File> = list
            .iter()
            .filter(|u| !u.contains("://"))
            .map(|p| gio::File::for_path(p))
            .collect();
        if files.is_empty() {
            return Some(text_provider);
        }
        let file_provider =
            gdk::ContentProvider::for_value(&gdk::FileList::from_files(files).to_value());
        Some(gdk::ContentProvider::new_union(&[
            text_provider,
            file_provider,
        ]))
    });
    widget.as_ref().add_controller(ds);
}

/// Read a dropped value back into URIs, accepting either content type.
///
/// Returns an empty Vec for a value that is neither, which callers treat as
/// "not for us" and decline.
pub(super) fn uris_from_value(value: &glib::Value) -> Vec<String> {
    if let Ok(joined) = value.get::<String>() {
        return joined
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
    }
    if let Ok(fl) = value.get::<gdk::FileList>() {
        return fl
            .files()
            .iter()
            .filter_map(|f| f.path())
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_joined_payload_splits_back_into_its_uris() {
        let v = "/m/a.mp3\ncdda://5?device=/dev/sr0\n/m/b.mp3".to_string().to_value();
        assert_eq!(
            uris_from_value(&v),
            vec![
                "/m/a.mp3".to_string(),
                "cdda://5?device=/dev/sr0".to_string(),
                "/m/b.mp3".to_string()
            ]
        );
    }

    #[test]
    fn blank_lines_in_a_payload_are_dropped() {
        let v = "/m/a.mp3\n\n\n/m/b.mp3\n".to_string().to_value();
        assert_eq!(
            uris_from_value(&v),
            vec!["/m/a.mp3".to_string(), "/m/b.mp3".to_string()]
        );
    }

    #[test]
    fn a_value_of_neither_type_yields_nothing() {
        let v = 42i32.to_value();
        assert!(uris_from_value(&v).is_empty());
    }
}
```

- [ ] **Step 2: Register the module**

Add to `frontends/gtk/window/mod.rs`, alongside the other `mod` declarations:

```rust
mod ml_drag;
```

- [ ] **Step 3: Run the tests**

Run: `distrobox enter dev-box -- sh -c 'cargo test --bin sparkamp ml_drag'`
Expected: 3 passed.

If `gdk::FileList::from_files` does not exist in gtk4-rs 0.9, check the actual constructor with
`distrobox enter dev-box -- sh -c 'cargo doc --open -p gdk4'` and adjust; the rest of the helper is unaffected.

- [ ] **Step 4: Commit**

```bash
git add frontends/gtk/window/ml_drag.rs frontends/gtk/window/mod.rs
git commit -m "feat(gtk): one drag source helper for the Media Library's views"
```

---

### Task 9 (B5): The drop target accepts the Sparkamp payload

**Files:**
- Modify: `frontends/gtk/window/dnd.rs:246` (the `pl_view` drop target) and `:404` (the external-file drop target)

**Interfaces:**
- Consumes: `ml_drag::{SPARKAMP_URIS_MIME, uris_from_value}`.
- Produces: nothing new.

- [ ] **Step 0: Add the shape check to `ml_drag.rs`, and delete the dead constant**

```rust
/// Whether `uri` looks like something the playlist can actually hold.
///
/// An absolute filesystem path, or a pseudo-URI whose scheme the engine
/// understands (`cdda://` for a CD track — see `parse_cdda_uri`). Everything
/// a Sparkamp drag produces passes; dropped prose does not.
///
/// Needed because the drop target accepts a bare `glib::Type::STRING` and
/// GTK negotiates by GType, not by mime — `text/plain` from any application
/// deserializes to a string as well, so the type alone cannot tell a track
/// list from a paragraph.
pub(super) fn is_playable_uri(uri: &str) -> bool {
    uri.starts_with('/') || uri.starts_with("cdda://")
}
```

Delete `SPARKAMP_URIS_MIME` and its `#[allow(dead_code)]`. Nothing publishes under it, and a custom mime could not have made the target exclusive anyway.

Tests, alongside the existing three in that module: an absolute path passes; a `cdda://5?device=/dev/sr0` URI passes; a bare word, a relative path, and an `https://` URL each fail.

- [ ] **Step 1: Widen the drop target's accepted types**

In `frontends/gtk/window/dnd.rs`, replace the `DropTarget::new` call at the `pl_view` target:

```rust
        // Accepts the Sparkamp URI payload (any Media Library view, including
        // a CD's `cdda://` tracks) and a plain FileList (file managers, and
        // the playlist's own rows). `DropTarget::new` takes one type, so the
        // target is constructed with one and given the full set after.
        let drop_tgt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        drop_tgt.set_types(&[gdk::FileList::static_type(), glib::Type::STRING]);
```

`DropTarget::new` requires a concrete type in gtk4-rs 0.9 — `glib::Type::INVALID` is not accepted. Constructing with `FileList` and then calling `set_types` with both is the supported shape. If `set_types` does not exist on this binding version, check the generated docs (`cargo doc -p gtk4 --open`, search `DropTarget`) and use whichever of `set_types` / `set_gtypes` the version exposes; report which one you used.

- [ ] **Step 2: Read the payload through the shared reader, and validate it**

The drop target accepts a bare `glib::Type::STRING`, which is not exclusive to Sparkamp: `GtkDropTarget` negotiates by GType, and `text/plain` from any application deserializes to a string too. Without a shape check, dragging a browser selection or an editor snippet onto the playlist would be split on newlines and each line turned into a placeholder row by `playlist_ingest::resolve` — visible junk the user has to remove by hand.

A custom MIME does **not** fix this on its own; only a private non-`String` GType with registered serializers would, and that is its own task. A cheap shape check closes the actual failure mode regardless of transport, so do that instead. `SPARKAMP_URIS_MIME` is deleted as dead, misleading scaffolding — it was never published under, and could not have been made exclusive as written.

Every line of a genuine payload comes from `attach_uri_drag`, so a real drop is uniformly valid and arbitrary text is uniformly invalid; reject the whole drop rather than filtering, which gives GTK an honest rejection instead of a partial, confusing accept.

Replace the opening of the same handler's `connect_drop`:

```rust
        drop_tgt.connect_drop(move |_, value, x, y| {
            let uris = ml_drag::uris_from_value(value);
            // Every entry must look like something the playlist can hold: an
            // absolute path, or a pseudo-URI scheme the engine understands.
            // A drag Sparkamp produced always satisfies this; dropped prose
            // never does.
            if uris.is_empty() || !uris.iter().all(|u| ml_drag::is_playable_uri(u)) {
                return false;
            }
            // Everything the playlist can hold is addressed by a string:
            // local files by path, CD tracks by `cdda://` pseudo-URI. The
            // reorder lookup below compares against `Track::path`, which
            // stores exactly these strings.
            let dropped: Vec<std::path::PathBuf> =
                uris.iter().map(std::path::PathBuf::from).collect();
```

Delete the old `let file_list = match value.get::<gdk::FileList>() { … }` block and the `let dropped = file_list.files()…` that followed it.

- [ ] **Step 3: Do the same for the external-file target**

Apply the same two changes to the `DropTarget` at `frontends/gtk/window/dnd.rs:404`, and route its add through the rule as in Task 7 (B3):

```rust
            let added = playlist_add::add_with_mode(
                &state_files,
                &dropped,
                crate::playlist_add::AddMode::Behavior,
            );
```

- [ ] **Step 4: Build and run the suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test'`
Expected: zero warnings, zero failures.

- [ ] **Step 5: Verify the existing sources still work**

Run: `distrobox enter dev-box -- sh -c 'cargo run'`

1. Drag rows from the ML Files view onto the playlist. Expected: added.
2. Drag a file from Nautilus. Expected: added.
3. Reorder within the playlist. Expected: reorders, does not clear.

All three worked before this task and must still work — this step is a regression check, not a new feature.

- [ ] **Step 6: Commit**

```bash
git add frontends/gtk/window/dnd.rs
git commit -m "feat(gtk): the playlist accepts drags from any Media Library view"
```

---

### Task 10 (B6): Converge the five hand-rolled behavior checks

**Files:**
- Modify: `frontends/gtk/window/files.rs:1231` and `:1314`
- Modify: `frontends/gtk/window/disc_page.rs:461`
- Modify: `frontends/gtk/window/disc_data.rs:561`
- Modify: `frontends/gtk/window/playlists_menu.rs:403`
- Modify: `frontends/gtk/window/tick.rs:236`

**Interfaces:**
- Consumes: `crate::playlist_add::{AddMode, should_replace}`.
- Produces: nothing new.

- [ ] **Step 1: Replace each check with the shared rule**

At each of the six sites, replace the local comparison

```rust
let should_replace = state_rc.borrow().config.behavior.playlist_add_behavior
    == crate::config::PlaylistAddBehavior::Replace;
```

with

```rust
let should_replace = crate::playlist_add::should_replace(
    &state_rc.borrow().config.behavior.playlist_add_behavior,
    crate::playlist_add::AddMode::Behavior,
);
```

adjusting the state binding's name per site (`state_rc`, `state`, …).

`disc_page.rs:461` already has a three-way `DiscAdd` enum — map it rather than replacing it:

```rust
            let replace = crate::playlist_add::should_replace(
                &behavior,
                match mode {
                    DiscAdd::Behavior => crate::playlist_add::AddMode::Behavior,
                    DiscAdd::PlayNow => crate::playlist_add::AddMode::Replace,
                    DiscAdd::Enqueue => crate::playlist_add::AddMode::Enqueue,
                },
            );
```

- [ ] **Step 2: Confirm nothing else reads the setting directly**

Run: `grep -rn "playlist_add_behavior" --include=*.rs frontends/gtk/ | grep -v settings.rs`
Expected: every remaining hit is either inside `crate::playlist_add::should_replace(...)` call arguments or a comment. A bare `== PlaylistAddBehavior::Replace` anywhere else is a site you missed.

- [ ] **Step 3: Build and run the suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test'`
Expected: zero warnings, zero failures.

- [ ] **Step 4: Verify each converged path by hand**

Run: `distrobox enter dev-box -- sh -c 'cargo run'` with "Replace playlist" set.

1. Double-click a track in ML Files → playlist clears, that track plays.
2. ML Files "Send to → Active playlist" → clears, adds selection.
3. Disc view "Play" → clears. Disc "Enqueue" → appends (this is the `DiscAdd` mapping; if Enqueue clears, the mapping is inverted).
4. Right-click a saved playlist → "Add to active playlist" → clears, adds its tracks.

- [ ] **Step 5: Commit**

```bash
git add frontends/gtk/window/files.rs frontends/gtk/window/disc_page.rs \
        frontends/gtk/window/disc_data.rs frontends/gtk/window/playlists_menu.rs \
        frontends/gtk/window/tick.rs
git commit -m "refactor(gtk): one rule decides append-vs-replace everywhere"
```

---

### Task 11 (B7): Expose the rule over FFI and drop the Swift copy

**Files:**
- Modify: `src/ffi/playlist.rs`
- Modify: `frontends/SparkampMac/Sources/SparkampModel+Transport.swift:255-262`
- Modify: `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`

**Interfaces:**
- Consumes: `crate::playlist_add::{AddMode, should_replace}`.
- Produces: `sparkamp_should_replace_on_add(ctx: *const SparkampCtx, mode: c_int) -> c_int` — `mode` 0 = Behavior, 1 = Enqueue, 2 = Replace; returns 1 to clear first, 0 otherwise.

- [ ] **Step 1: Add the FFI getter**

Append to `src/ffi/playlist.rs`:

```rust
/// Whether an add in `mode` should clear the playlist first.
///
/// `mode`: 0 = honour the user's setting, 1 = always append, 2 = always
/// replace. An unknown value is treated as 0.
///
/// Exists so the macOS frontend stops deciding for itself. `addFiles` used to
/// compare `sparkamp_get_playlist_add_behavior` against 1 inline, which was
/// correct but was a second copy of a rule that GTK also held five copies of.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_should_replace_on_add(
    ctx: *const SparkampCtx,
    mode: c_int,
) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    let mode = match mode {
        1 => crate::playlist_add::AddMode::Enqueue,
        2 => crate::playlist_add::AddMode::Replace,
        _ => crate::playlist_add::AddMode::Behavior,
    };
    crate::playlist_add::should_replace(&ctx.config.behavior.playlist_add_behavior, mode) as c_int
}
```

- [ ] **Step 2: Declare it in the bridging header**

Add to `frontends/SparkampMac/SparkampCore/sparkamp_bridge.h`, near the other playlist declarations:

```c
/* 0 = honour setting, 1 = always append, 2 = always replace.
   Returns 1 when the playlist should be cleared before adding. */
int sparkamp_should_replace_on_add(const void *ctx, int mode);
```

- [ ] **Step 3: Route the Swift call through it**

In `frontends/SparkampMac/Sources/SparkampModel+Transport.swift`, replace:

```swift
        // If "Replace playlist" is the configured behavior, clear before adding.
        let shouldReplace = Int(sparkamp_get_playlist_add_behavior(ctx)) == 1
        if shouldReplace {
            sparkamp_playlist_clear(ctx)
        }
```

with:

```swift
        // Core decides. `sparkamp_should_replace_on_add` is the same rule GTK
        // and the TUI use, so the three frontends cannot drift on what
        // "Replace playlist" means. 0 = honour the configured setting.
        let shouldReplace = sparkamp_should_replace_on_add(ctx, 0) == 1
        if shouldReplace {
            sparkamp_playlist_clear(ctx)
        }
```

- [ ] **Step 4: Build and run the suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test'`
Expected: zero warnings, zero failures.

**The Swift change cannot be built here** — there is no Mac in this environment. Say so plainly in the commit message rather than implying it was verified.

- [ ] **Step 5: Commit**

```bash
git add src/ffi/playlist.rs frontends/SparkampMac/
git commit -m "refactor(ffi): the add-behavior rule crosses to macOS instead of being copied"
```

---

### Task 12 (B8): Attach drag sources to the five views that lack them

Containers expand to all their files (decision 3): an album drags its tracks, a saved playlist its tracks, a disc its tracks, a device its files.

**Files:**
- Modify: `frontends/gtk/window/album_gallery.rs` (gallery cells)
- Modify: `frontends/gtk/window/disc_page.rs` (track rows and the drive row)
- Modify: `frontends/gtk/window/devices_page.rs` (track rows and the device row)
- Modify: `frontends/gtk/window/playlists.rs` (playlist rows in the overview)

**Interfaces:**
- Consumes: `ml_drag::attach_uri_drag`, `MediaLibrary::album_tracks`, `MediaLibrary::load_playlist_tracks`.
- Produces: nothing new.

- [ ] **Step 1: Album gallery cell — drags the album's tracks**

In `frontends/gtk/window/album_gallery.rs`, inside the factory's `connect_setup` where the cell is built, after `cell.append(&artist);`:

```rust
            // Dragging a tile drags the album: every track in it, in the order
            // `album_tracks` returns them (disc then track number). A gallery
            // tile is a container, so the drag carries its contents rather
            // than the tile itself (decision, 2026-08-18).
            {
                let li_drag = li.clone();
                let state_drag = state.clone();
                super::ml_drag::attach_uri_drag(&cell, move || {
                    let Some(obj) = li_drag
                        .item()
                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                    else {
                        return Vec::new();
                    };
                    let (album, album_artist) = {
                        let a = obj.borrow::<crate::media_library::AlbumGroup>();
                        (a.album.clone(), a.album_artist.clone())
                    };
                    let s = state_drag.borrow();
                    let artist_as_album = s.config.media_library.artist_as_album_artist;
                    s.media_lib
                        .as_ref()
                        .and_then(|lib| {
                            lib.album_tracks(&album, &album_artist, artist_as_album).ok()
                        })
                        .unwrap_or_default()
                        .into_iter()
                        .map(|t| t.path)
                        .collect()
                });
            }
```

- [ ] **Step 2: Build and verify the album drag**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo run'`

Drag an album tile onto the playlist. Expected: every track of that album is added, in disc/track order. With "Replace playlist" set, the playlist clears first.

- [ ] **Step 3: Playlist overview row — drags the playlist's tracks**

`frontends/gtk/window/util.rs:182` already attaches a `pl:<id>` string drag aimed at device rows. Leave it; add the URI drag alongside so the same row can also be dropped on the playlist. In `frontends/gtk/window/playlists.rs`, where each overview row is built:

```rust
        // A playlist row already carries `pl:<id>` for device drops
        // (`attach_pl_row_drag`). Dropping one on the active playlist means
        // "add its tracks", so it also publishes those track paths.
        {
            let state_drag = state.clone();
            let pl_id = pl.id;
            super::ml_drag::attach_uri_drag(&row, move || {
                let s = state_drag.borrow();
                let Some(lib) = s.media_lib.as_ref() else {
                    return Vec::new();
                };
                let Ok(all) = lib.all_playlists() else {
                    return Vec::new();
                };
                let Some(pl) = all.into_iter().find(|p| p.id == pl_id) else {
                    return Vec::new();
                };
                lib.load_playlist_tracks(&pl)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|t| t.path)
                    .collect()
            });
        }
```

- [ ] **Step 4: Device track rows and the device row**

In `frontends/gtk/window/devices_page.rs`, in the cell factory's setup for the track table, mirroring the Files table's per-cell source at `files.rs:484` — collect every selected row, falling back to the row under the pointer:

```rust
                    {
                        let ds_sel = dev_selection.clone();
                        let ds_li = li.clone();
                        super::ml_drag::attach_uri_drag(&cell, move || {
                            let mut paths: Vec<String> = Vec::new();
                            for i in 0..ds_sel.n_items() {
                                if ds_sel.is_selected(i) {
                                    if let Some(obj) = ds_sel
                                        .item(i)
                                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                    {
                                        let t = obj.borrow::<crate::media_library::LibTrack>();
                                        paths.push(t.path.clone());
                                    }
                                }
                            }
                            if paths.is_empty() {
                                if let Some(obj) = ds_li
                                    .item()
                                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                {
                                    let t = obj.borrow::<crate::media_library::LibTrack>();
                                    paths.push(t.path.clone());
                                }
                            }
                            paths
                        });
                    }
```

For the device row in the sidebar, drag every file the device scan cached — `all_tracks` in `devices_page.rs` (see `*all_tracks2.borrow_mut() = tracks.clone();` around line 613):

```rust
        // Dragging the device drags everything on it, per the container rule.
        {
            let all = all_tracks.clone();
            super::ml_drag::attach_uri_drag(&dev_row, move || {
                all.borrow().iter().map(|t| t.path.clone()).collect()
            });
        }
```

- [ ] **Step 5: Disc track rows and the drive row**

**Do not build the URI by hand.** `DiscTrackEntry.path` (`src/disc/mod.rs:190-192`) already *is* the playlist address — `cdda://N?device=/dev/srX` on Linux, the mounted AIFF path on macOS — and it is produced in one place, `toc::track_entries`. Reconstructing it would fork that format, and `engine.rs:608` builds a bare `cdda://{track}` for its own purposes, so copying *that* shape would drop the device and load the wrong drive.

In `frontends/gtk/window/disc_page.rs`, inside the `for e in &entries` loop that builds each track row (the `let row = ListBoxRow::new();` at ~`:758`), immediately after `row.set_child(Some(&row_lbl));`:

```rust
                    // A CD track is addressed by pseudo-URI, not by path —
                    // the payload `ml_drag` exists to carry, since a
                    // `gdk::FileList` cannot hold one. `e.path` is the same
                    // string the disc's own add buttons put in `Track.path`
                    // (see the `DiscAdd` closure at ~:498), so a dragged
                    // track and an enqueued one are identical.
                    {
                        let uri = e.path.clone();
                        super::ml_drag::attach_uri_drag(&row, move || vec![uri.clone()]);
                    }
```

For the drive row, every track on the disc. `current_disc_entries` is the `Rc<RefCell<Vec<DiscTrackEntry>>>` the file already clones at `:263`, `:545`, `:831` and `:988` — clone it once more rather than introducing new state, and attach to the drive's row widget in the disc **overview** list:

```rust
        // Dragging the drive drags the disc: every track on it, per the
        // container rule.
        {
            let entries_drag = current_disc_entries.clone();
            super::ml_drag::attach_uri_drag(&drive_row, move || {
                entries_drag.borrow().iter().map(|e| e.path.clone()).collect()
            });
        }
```

Read the overview's row construction and bind `drive_row` to whatever that row widget is actually called there; if the overview builds no per-drive row widget, attach to the row's container and say so in the report rather than inventing one.

- [ ] **Step 6: Build and run the suite**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test'`
Expected: zero warnings, zero failures.

- [ ] **Step 7: Verify all nine sources**

Run: `distrobox enter dev-box -- sh -c 'cargo run'`, with "Append" set. Drag each of these onto the active playlist:

1. Files view — a multi-row selection
2. Album details (Files, drilled in) — a selection
3. Album overview — a tile → the whole album
4. Specific playlist view — a selection
5. Playlists overview — a playlist row → all its tracks
6. Disc view — a track row
7. Disc overview — a drive row → all tracks on the disc
8. Device view — a selection
9. Device overview — a device row → every file on it

Then set "Replace playlist" and re-check 3, 5, 7 and 9 — the container drags — expecting the playlist to clear first each time.

A CD track that adds but will not play means the `cdda://` URI is malformed; compare it against what `engine.rs:608` builds.

- [ ] **Step 8: Commit**

```bash
git add frontends/gtk/window/
git commit -m "feat(gtk): every Media Library view can be dragged to the playlist"
```

---

## Self-Review

**Spec coverage.** Decision 1 (replace clears first) → Task 5 (B1)'s `should_replace` plus B2's clear-then-add. Decision 2 (file-manager drops respect the flag) → Task 7 (B3) and B5 Step 3, and it is why no ML-vs-external marker exists anywhere in the plan. Decision 3 (containers expand) → Task 12 (B8) Steps 1, 3, 4, 5. Decision 4 (issue 3 = A + C) → A3/A4 are option A, A2 is option C. Decision 5 (issue 2 = option 1) → Track B in full: core rule (B1), GTK funnel (B2), converged callers (B6), FFI + macOS (B7), missing sources (B4, B8).

**Known gaps, deliberately left.** The TUI is untouched: it already honours the setting (`media_library/mod.rs:826`, `detection.rs:126`) and has no drag-and-drop to fix. Those two sites could converge on `crate::playlist_add::should_replace` for symmetry, but that is a refactor of working code and is not requested here.

**Type consistency.** `AddMode` and `should_replace` are defined in B1 and used unchanged in B2, B6 and B7. `add_with_mode` is defined in B2 and called in B3 and B5. `attach_uri_drag` and `uris_from_value` are defined in B4 and used in B5 and B8. `build_album_gallery` gains a third return element in A4 Step 4 and its single caller is updated in A4 Step 5. `AlbumRow` gains `track_count` in A2 Step 1 and is read in A2 Steps 2 and 3.

**Verification that is not automated.** GTK widget layout, drag-and-drop and the gallery's load feel have no test coverage in this repo, so Tasks 3 (A3), A4, B3, B5, B6 and B8 all carry explicit by-hand steps. The riskiest is B3 Step 3 case 1 — an internal reorder under "Replace playlist" must not empty the playlist — because getting it wrong destroys the user's playlist rather than merely misbehaving.

**Not verifiable here.** Task 11 (B7)'s Swift edit cannot be compiled in this environment. Its commit message must say so.
