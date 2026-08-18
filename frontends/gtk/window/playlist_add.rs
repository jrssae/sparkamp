use super::*;

/// The one way rows enter the active playlist.
///
/// Before this existed there were 27 places that pushed onto
/// `state.playlist`, each deciding for itself how to turn a path or a library
/// row into a `Track`. They did not agree. Some read every file from disk on
/// the GTK main thread — at 27.974 ms per file, measured, which is roughly
/// seventeen minutes for a 36k library and exactly the freeze that was
/// reported. Three of them forgot to probe durations at all, which is why a
/// file dropped from the file manager showed a blank duration until it played.
///
/// The rule here is: **adding a row never touches the filesystem.** Everything
/// shown at insert comes from the media library's record of the file. The
/// things a database cannot answer — is it still there, is it writable, and
/// for a file the library has never seen, what are its tags and how long is it
/// — are only *noted* here, in `AppState::pending_rows`.
///
/// Nothing reads a file until [`request_range`] is called with rows that are
/// actually on screen. Adding 36,000 rows therefore costs 36,000 map inserts
/// and no I/O at all; the reading is paid for one screenful at a time, by
/// whoever scrolls. See `crate::file_status` for why that is the whole design
/// rather than an optimisation.
///
/// Callers get back an [`Added`] describing where the rows landed, and are
/// responsible for the UI rebuild; this module deliberately knows nothing
/// about widgets.

/// Where a batch of newly added rows landed in the playlist.
pub(super) struct Added {
    /// Index of the first row added.
    pub(super) start: usize,
    /// How many rows were added. Zero when nothing resolved.
    pub(super) count: usize,
}

impl Added {
    /// True when anything was actually added — the usual guard before a
    /// rebuild or an autoplay decision.
    pub(super) fn any(&self) -> bool {
        self.count > 0
    }
}

/// Add rows by path, resolving them against the media library first.
///
/// One batched query covers the whole batch, so a path the library knows costs
/// no filesystem access whatever. A path it has never seen gets a placeholder
/// row immediately — the filename, no duration — and the background pass reads
/// its tags and measures it, so the row fills in a moment later rather than
/// holding up the other 35,999.
///
/// A library file reached under a different spelling of its path (a bind mount
/// spelling `/mnt` where the scan recorded `/var/mnt`) misses the exact match
/// and is treated as unknown: correct, since the pass reads the real file, just
/// slower for that one row.
pub(super) fn add_paths(state: &Rc<RefCell<AppState>>, paths: &[std::path::PathBuf]) -> Added {
    if paths.is_empty() {
        return Added { start: 0, count: 0 };
    }
    // Resolving is core work shared with the TUI and the CLI; the borrow is
    // taken and dropped around it rather than held, because the library query
    // is the slow part and nothing here needs the state while it runs.
    let rows = {
        let s = state.borrow();
        crate::playlist_ingest::resolve(s.media_lib.as_ref(), paths)
    };
    let start;
    let count;
    {
        let mut s = state.borrow_mut();
        start = s.playlist.tracks.len();
        for row in rows {
            s.playlist.add(row.track);
            // `add` stamps the entry id; note the row as unfinished against it.
            let id = s.playlist.tracks.last().map(|t| t.id).unwrap_or(0);
            s.pending_rows.insert(id, row.needs_tags);
        }
        count = s.playlist.tracks.len() - start;
    }
    Added { start, count }
}

/// Add one already-built track — the single-row entry point, for sites that
/// construct their own `Track` (a disc rip, a device file, a dedupe result).
///
/// Safe to call in a loop: noting a row is a map insert, so a thousand calls in
/// one main-loop turn cost a thousand map inserts and no I/O.
pub(super) fn add_track(
    state: &Rc<RefCell<AppState>>,
    track: crate::model::Track,
    needs_tags: bool,
) {
    let mut s = state.borrow_mut();
    s.playlist.add(track);
    // `add` stamps the entry id; note the row as unfinished against it, so the
    // answer can later be applied to this exact row without searching for it.
    let id = s.playlist.tracks.last().map(|t| t.id).unwrap_or(0);
    s.pending_rows.insert(id, needs_tags);
}

/// Note rows the caller added itself as still needing the background pass.
///
/// For the sites that hold a `borrow_mut` across their whole loop and so
/// cannot call [`add_track`] per row: add as before, drop the borrow, then
/// hand over the index the batch started at.
pub(super) fn schedule_from(state: &Rc<RefCell<AppState>>, start: usize, needs_tags: bool) {
    let mut s = state.borrow_mut();
    // Collected first so the playlist read is finished before `pending_rows`
    // is borrowed mutably.
    let from = start.min(s.playlist.tracks.len());
    let ids: Vec<u64> = s.playlist.tracks[from..].iter().map(|t| t.id).collect();
    for id in ids {
        s.pending_rows.insert(id, needs_tags);
    }
}

/// How long a row's file status is trusted before the viewport pass asks
/// again.
///
/// Matches the Media Library's own status cache. A status check is two cheap
/// syscalls — `stat` plus `access` — with no file read, so re-confirming the
/// forty-odd rows on screen every ten seconds is not measurable, while leaving
/// them decided forever means a permission change made outside Sparkamp never
/// shows up.
// `pub(super)` only so the window's test module can age a row past it rather
// than sleeping ten seconds.
pub(super) const ROW_RECHECK_TTL: std::time::Duration = std::time::Duration::from_secs(10);

/// Ask the background worker about one specific row, whatever its last answer
/// was.
///
/// Used when the user acts on a row — playing it — where waiting for the
/// viewport pass to come round would read as the marker being stuck.
pub(super) fn request_row(state: &Rc<RefCell<AppState>>, idx: usize) {
    let (check, tx) = {
        let mut s = state.borrow_mut();
        let Some(t) = s.playlist.tracks.get(idx) else {
            return;
        };
        let (id, path) = (t.id, t.path.clone());
        // Usually a status-only check: the library or an earlier pass already
        // answered for the tags, and this is about the markers. A row that
        // still owes tags stays pending until they arrive, as in
        // [`request_range`].
        let needs_tags = s.pending_rows.get(&id).copied().unwrap_or(false);
        if !needs_tags {
            s.pending_rows.remove(&id);
        }
        s.row_checked_at.insert(id, std::time::Instant::now());
        (
            crate::file_status::RowCheck { path, needs_tags, id },
            s.row_check_tx.clone(),
        )
    };
    if let Some(tx) = tx {
        let _ = tx.send(vec![check]);
    }
}

/// Ask the background worker about the rows in `first..=last` that are due a
/// look — the viewport pass.
///
/// This is the only thing in the module that causes a file to be read, and it
/// is driven by what is on screen. A row is due when it has never been checked
/// or when its last answer is older than [`ROW_RECHECK_TTL`], so scrolling back
/// and forth over the same rows costs one read each rather than one per pass.
///
/// A batch that finds nothing due sends nothing, which is the common case once
/// a screenful has settled — scrolling a resolved playlist is free.
pub(super) fn request_range(state: &Rc<RefCell<AppState>>, first: usize, last: usize) {
    let (batch, tx) = {
        let mut s = state.borrow_mut();
        let n = s.playlist.tracks.len();
        // Clearing the playlist leaves the cleared rows' ids behind. Ids are
        // never reused, so a stale one is only ever dead weight — but a
        // clear-and-refill loop would grow the map without bound. Pruned here
        // rather than at the fourteen sites that clear the playlist, because a
        // rule every call site has to remember is the exact failure this module
        // was written to end.
        if s.pending_rows.len() + s.row_checked_at.len() > 2 * n + 4096 {
            let live: std::collections::HashSet<u64> =
                s.playlist.tracks.iter().map(|t| t.id).collect();
            s.pending_rows.retain(|id, _| live.contains(id));
            s.row_checked_at.retain(|id, _| live.contains(id));
        }
        if n == 0 || first >= n {
            return;
        }
        let end = last.min(n - 1);
        // Ids are `Copy`, so the range can be read without cloning any paths —
        // most scans find nothing pending and should cost almost nothing.
        let ids: Vec<u64> = s.playlist.tracks[first..=end].iter().map(|t| t.id).collect();
        // A row is worth asking about when it has never been looked at, or when
        // the last answer has aged out. The second case is what keeps the ⚠ and
        // 🔒 markers honest after the file changes underneath us.
        //
        // `needs_tags` is read from `pending_rows` in both cases, not forced to
        // false on the aged-out one. A row whose file was missing when it was
        // first checked never had its tags read, and hard-coding false meant
        // that once the file came back the ⚠ cleared while the row kept its
        // placeholder filename-stem title and blank duration for good.
        let wanted: Vec<(usize, u64, bool)> = ids
            .iter()
            .enumerate()
            .filter_map(|(k, id)| {
                let fresh = s
                    .row_checked_at
                    .get(id)
                    .is_some_and(|at| at.elapsed() < ROW_RECHECK_TTL);
                if fresh {
                    return None;
                }
                Some((first + k, *id, s.pending_rows.get(id).copied().unwrap_or(false)))
            })
            .collect();
        let mut batch = Vec::with_capacity(wanted.len());
        for (idx, id, needs_tags) in wanted {
            // Only a row with nothing left to read is retired here. One still
            // owing tags stays pending until they actually arrive — see
            // `apply_facts` — with the timestamp above holding it off for the
            // TTL so a missing file is retried, not hammered.
            if !needs_tags {
                s.pending_rows.remove(&id);
            }
            s.row_checked_at.insert(id, std::time::Instant::now());
            if let Some(t) = s.playlist.tracks.get(idx) {
                batch.push(crate::file_status::RowCheck {
                    path: t.path.clone(),
                    needs_tags,
                    id,
                });
            }
        }
        (batch, s.row_check_tx.clone())
    };
    if batch.is_empty() {
        return;
    }
    // No sender means no GTK main loop to deliver answers to — the FFI and test
    // paths. The rows are still correct, they just never get their markers.
    if let Some(tx) = tx {
        let _ = tx.send(batch);
    }
}

/// Apply a whole batch of background answers in ONE pass over the playlist.
///
/// The batch matters. The first version of this applied a single result at a
/// time and scanned the playlist for a matching path on each one — O(rows ×
/// results). With 36,000 rows and 500 results a tick that is eighteen million
/// path comparisons every 33 ms, which did not merely stall the UI, it took
/// the machine down with it. The probe drain immediately above the call site
/// carries a comment warning about exactly this shape, added when the same
/// mistake was fixed there.
///
/// Now results are keyed by the entry id `Playlist::add` stamps, so the batch
/// becomes a map and the playlist is walked once, O(rows + results), with an
/// O(1) lookup per row.
pub(super) fn apply_facts(
    state: &Rc<RefCell<AppState>>,
    batch: &[crate::file_status::RowFacts],
) -> Vec<usize> {
    if batch.is_empty() {
        return Vec::new();
    }
    let by_id: std::collections::HashMap<u64, &crate::file_status::RowFacts> =
        batch.iter().map(|f| (f.id, f)).collect();
    let mut changed = Vec::new();
    let mut s = state.borrow_mut();
    for (i, t) in s.playlist.tracks.iter_mut().enumerate() {
        let Some(facts) = by_id.get(&t.id) else {
            continue;
        };
        let mut touched = false;
        if t.broken != !facts.exists {
            t.broken = !facts.exists;
            touched = true;
        }
        if t.read_only != facts.read_only {
            t.read_only = facts.read_only;
            touched = true;
        }
        if let Some(read) = &facts.tags {
            // Only for rows the library could not answer for. Keep whatever the
            // user is already looking at if the read came back empty.
            if !read.title.is_empty() && t.title != read.title {
                t.title = read.title.clone();
                touched = true;
            }
            if t.artist != read.artist {
                t.artist = read.artist.clone();
                touched = true;
            }
            if t.album_artist != read.album_artist {
                t.album_artist = read.album_artist.clone();
                touched = true;
            }
            if t.album != read.album {
                t.album = read.album.clone();
                touched = true;
            }
            if t.duration.is_none() && read.duration.is_some() {
                t.duration = read.duration;
                touched = true;
            }
        }
        if touched {
            changed.push(i);
        }
    }
    for facts in batch {
        // The row stays pending only while there is still something to find.
        // `request_range` leaves a tag-owing row in the map precisely so that a
        // read which could not happen — the file was missing at the time — is
        // tried again once it comes back, rather than written off and left
        // showing its placeholder filename stem for good.
        //
        // `exists` is the condition, not `tags.is_some()`: a file that is there
        // but could not be parsed has already given its answer, and retrying it
        // every TTL would re-read a corrupt file forever.
        if facts.exists {
            s.pending_rows.remove(&facts.id);
        }
        // Remember any measured duration so the next session starts with it.
        if let Some(d) = facts.tags.as_ref().and_then(|t| t.duration) {
            s.duration_cache.insert(&facts.path, d);
        }
    }
    changed
}

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
// The first caller lands in Task 7 (B3); until then this is unreferenced.
#[allow(dead_code)]
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
