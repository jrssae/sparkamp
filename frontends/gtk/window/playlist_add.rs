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
    let mut wanted: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let mut known = resolve(state, &wanted);

    // A dropped directory means everything under it. Only paths the library
    // could not place are candidates — a library row is a file by definition,
    // so a 36k Media Library drop never asks the filesystem anything, while a
    // folder dropped from a file manager still expands the way it always has.
    let unresolved: Vec<std::path::PathBuf> = paths
        .iter()
        .zip(wanted.iter())
        .filter(|(_, key)| !known.contains_key(*key))
        .map(|(p, _)| p.clone())
        .collect();
    let mut paths: Vec<std::path::PathBuf> = paths.to_vec();
    if unresolved.iter().any(|p| p.is_dir()) {
        let mut expanded = Vec::with_capacity(paths.len());
        for p in paths {
            if p.is_dir() {
                expanded.extend(crate::model::Playlist::collect_audio_files(&p));
            } else {
                expanded.push(p);
            }
        }
        paths = expanded;
        wanted = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        // The files inside a dropped folder may well be indexed even though the
        // folder itself is not a row, so resolve again now they are named.
        known = resolve(state, &wanted);
    }
    let paths = &paths[..];

    let start;
    let count;
    {
        let mut s = state.borrow_mut();
        start = s.playlist.tracks.len();
        for (path, key) in paths.iter().zip(wanted.iter()) {
            let (track, needs_tags) = match known.get(key) {
                Some(lt) => (crate::model::Track::from(lt), false),
                // Placeholder: the filename stem, so the row is recognisable
                // straight away. Everything real about it arrives from the pass.
                None => (placeholder(path), true),
            };
            s.playlist.add(track);
            // `add` stamps the entry id; note the row as unfinished against it.
            let id = s.playlist.tracks.last().map(|t| t.id).unwrap_or(0);
            s.pending_rows.insert(id, needs_tags);
        }
        count = s.playlist.tracks.len() - start;
    }
    Added { start, count }
}

/// One batched library lookup. An unopenable or absent library is not an
/// error here — every path simply comes back unknown and gets read from disk
/// by the background pass, which is the correct answer for a machine with no
/// library at all.
fn resolve(
    state: &Rc<RefCell<AppState>>,
    wanted: &[String],
) -> std::collections::HashMap<String, crate::media_library::LibTrack> {
    state
        .borrow()
        .media_lib
        .as_ref()
        .and_then(|lib| lib.tracks_by_exact_paths(wanted).ok())
        .unwrap_or_default()
}

/// A row for a file nothing has told us about yet. Built without touching the
/// filesystem — even `Track::from_path_fast` canonicalises and tests
/// writability, which is two syscalls we are trying not to spend here.
fn placeholder(path: &std::path::Path) -> crate::model::Track {
    crate::model::Track {
        path: path.to_path_buf(),
        title: path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string(),
        artist: String::new(),
        album_artist: String::new(),
        album: String::new(),
        duration: None,
        broken: false,
        read_only: false,
        id: 0,
    }
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

/// Ask the background worker about the rows in `first..=last` that have not
/// been asked about yet — the viewport pass.
///
/// This is the only thing in the module that causes a file to be read, and it
/// is driven by what is on screen. Rows are taken out of `pending_rows` as they
/// are handed over, so scrolling back and forth over the same rows costs one
/// read each, not one per pass.
///
/// A batch that finds nothing pending sends nothing, which is the common case
/// once a screenful has settled — scrolling a resolved playlist is free.
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
        if s.pending_rows.len() > n + 4096 {
            let live: std::collections::HashSet<u64> =
                s.playlist.tracks.iter().map(|t| t.id).collect();
            s.pending_rows.retain(|id, _| live.contains(id));
        }
        if s.pending_rows.is_empty() || n == 0 || first >= n {
            return;
        }
        let end = last.min(n - 1);
        // Ids are `Copy`, so the range can be read without cloning any paths —
        // most scans find nothing pending and should cost almost nothing.
        let ids: Vec<u64> = s.playlist.tracks[first..=end].iter().map(|t| t.id).collect();
        let wanted: Vec<(usize, u64, bool)> = ids
            .iter()
            .enumerate()
            .filter_map(|(k, id)| s.pending_rows.get(id).map(|nt| (first + k, *id, *nt)))
            .collect();
        let mut batch = Vec::with_capacity(wanted.len());
        for (idx, id, needs_tags) in wanted {
            s.pending_rows.remove(&id);
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
    // Remember any measured durations so the next session starts with them.
    for facts in batch {
        if let Some(d) = facts.tags.as_ref().and_then(|t| t.duration) {
            s.duration_cache.insert(&facts.path, d);
        }
    }
    changed
}
