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
/// shown at insert comes from the media library's record of the file. The two
/// things a database cannot answer — is it still there, is it writable, and
/// for a file the library has never seen, what are its tags and how long is it
/// — are handed to the background pass in `crate::file_status`, which walks
/// them one at a time and reports each row as it goes.
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

    let mut checks = Vec::new();
    let start;
    let tx;
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
            let path = track.path.clone();
            s.playlist.add(track);
            checks.push(crate::file_status::RowCheck {
                path,
                needs_tags,
                id: s.playlist.tracks.last().map(|t| t.id).unwrap_or(0),
            });
        }
        tx = s.row_facts_tx.clone();
    }
    let count = checks.len();
    schedule(checks, tx);
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
/// Safe to call in a loop: scheduling coalesces, so a thousand calls in one
/// main-loop turn become one background pass rather than a thousand threads.
pub(super) fn add_track(
    state: &Rc<RefCell<AppState>>,
    track: crate::model::Track,
    needs_tags: bool,
) {
    let check = crate::file_status::RowCheck {
        path: track.path.clone(),
        needs_tags,
        id: 0,
    };
    let (tx, id) = {
        let mut s = state.borrow_mut();
        s.playlist.add(track);
        // `add` stamps the entry id; take it back so the answer can be applied
        // to this exact row without searching for it.
        let id = s.playlist.tracks.last().map(|t| t.id).unwrap_or(0);
        (s.row_facts_tx.clone(), id)
    };
    let mut check = check;
    check.id = id;
    schedule(vec![check], tx);
}

/// Schedule the background pass for rows the caller added itself.
///
/// For the sites that hold a `borrow_mut` across their whole loop and so
/// cannot call [`add_track`] per row: add as before, drop the borrow, then
/// hand over the index the batch started at.
pub(super) fn schedule_from(state: &Rc<RefCell<AppState>>, start: usize, needs_tags: bool) {
    let (checks, tx) = {
        let s = state.borrow();
        let checks: Vec<crate::file_status::RowCheck> = s.playlist.tracks[start.min(s.playlist.tracks.len())..]
            .iter()
            .map(|t| crate::file_status::RowCheck {
                path: t.path.clone(),
                needs_tags,
                id: t.id,
            })
            .collect();
        (checks, s.row_facts_tx.clone())
    };
    schedule(checks, tx);
}

thread_local! {
    /// Rows waiting to be checked, accumulated across every add in this
    /// main-loop turn.
    static PENDING: RefCell<Vec<crate::file_status::RowCheck>> =
        const { RefCell::new(Vec::new()) };
    /// Whether a flush is already booked, so N adds book one.
    static FLUSH_QUEUED: Cell<bool> = const { Cell::new(false) };
    /// Where to send the answers. Re-stamped on every schedule; it is the same
    /// sender every time within a session.
    static SENDER: RefCell<Option<std::sync::mpsc::Sender<crate::file_status::RowFacts>>> =
        const { RefCell::new(None) };
}

/// Queue rows for the background pass, flushing once on the next idle.
///
/// The coalescing is what lets [`add_track`] sit inside a `for` loop without
/// the caller having to know it should batch. Fifteen of the call sites this
/// module replaced were loops; spawning a thread per iteration would have
/// swapped a main-thread freeze for a thread explosion.
fn schedule(
    checks: Vec<crate::file_status::RowCheck>,
    tx: Option<std::sync::mpsc::Sender<crate::file_status::RowFacts>>,
) {
    // No sender means no GTK main loop to deliver to — the FFI and test paths.
    // The rows are still correct, they just never get their markers.
    let Some(tx) = tx else { return };
    SENDER.with(|s| *s.borrow_mut() = Some(tx));
    PENDING.with(|p| p.borrow_mut().extend(checks));
    if !FLUSH_QUEUED.with(|f| f.replace(true)) {
        glib::idle_add_local_once(flush_pending);
    }
}

fn flush_pending() {
    FLUSH_QUEUED.with(|f| f.set(false));
    let batch: Vec<crate::file_status::RowCheck> =
        PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
    let tx = SENDER.with(|s| s.borrow().clone());
    if let (Some(tx), false) = (tx, batch.is_empty()) {
        crate::file_status::spawn_row_checks(batch, tx);
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
