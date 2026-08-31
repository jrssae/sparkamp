//! The background pass that finishes a playlist row.
//!
//! Adding a row to the active playlist is a data copy. For a file the media
//! library knows, title, artist and album are already in its record, so the
//! row can appear complete and instantly.
//!
//! Three things never come from the database:
//!
//! * whether the file is still on disk, and whether it can be written; and
//! * for a file the library has *never seen*, its tags and duration, which
//!   only exist inside the file itself.
//!
//! All of them need the filesystem, and asking inline is what made a large add
//! freeze the UI — `Track::from_path` measured 27.974 ms per file, so a 36k
//! add cost about seventeen minutes on the GTK main thread.
//!
//! # Only what is on screen
//!
//! The rows handed here are the ones the user is currently looking at, not
//! every row that was added. This is Winamp's design, and it is worth stating
//! why, because the obvious alternative — walk all 36,000 in the background —
//! reads as harmless and is not.
//!
//! Winamp's classic playlist editor keeps a `cached` bit on each entry and a
//! 100 ms timer that walks *only the visible rows*, reads the first unresolved
//! one it finds, repaints that single row, and stops (`Src/Winamp/Pledit.cpp`).
//! Rows nobody has scrolled to are never touched at all. The cost of a 36k add
//! is therefore the cost of one screenful, whatever the playlist size.
//!
//! Draining the whole playlist instead would cost 36,000 × 27.974 ms ≈ 17
//! minutes of continuous disk I/O for a large folder drop, nearly all of it
//! for rows that will never be looked at — a fan, a battery and a busy disk
//! spent on nothing. What the user does see fills at exactly the same speed
//! either way, because a screenful is ~35 rows in both designs.
//!
//! The visible trade is that a file deleted from disk at row 20,000 shows no
//! ⚠ until that row is scrolled to. That is the deal Winamp makes too.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

/// One row to finish: its path, and whether the library had a record for it.
///
/// `needs_tags` is false for a library row — its tags came from the database
/// and re-reading them off disk would be work for an answer already held.
pub struct RowCheck {
    pub path: PathBuf,
    pub needs_tags: bool,
    /// The playlist entry's stable session id, so the answer can be applied to
    /// the right row in O(1) even after a reorder. Matching on the path instead
    /// meant scanning the whole playlist per result, which on a 36k add is
    /// millions of comparisons per tick — the difference between a background
    /// pass and a locked machine.
    pub id: u64,
}

/// What the pass learned about one row.
pub struct RowFacts {
    /// The entry this answers for. Stable across reorders and removals, and
    /// unique even when the same file sits in the playlist twice.
    pub id: u64,
    /// Kept for the duration cache, which is keyed by path.
    pub path: PathBuf,
    /// False when the file is gone — the row's ⚠ marker.
    pub exists: bool,
    /// The row's 🔒 marker. Always false for a file that is not there.
    pub read_only: bool,
    /// Only for rows whose file the library had never seen: the tags and
    /// duration read from disk. `None` for library rows, and for a file that
    /// could not be read at all.
    pub tags: Option<crate::model::Track>,
}

/// Answer for one row. The only function here that touches the filesystem.
pub fn check_row(row: &RowCheck) -> RowFacts {
    // A disc track is a `cdda://` pseudo-URI, not a file: `exists()` is false
    // for it however good the disc is, which marked every audio-CD row broken.
    // The same rule `Playlist::load` already applies (`model.rs`) — a disc
    // track is present and read-only — just applied by this worker too.
    let is_disc = crate::model::is_disc_uri(&row.path);
    let exists = is_disc || row.path.exists();
    let read_only = is_disc || (exists && crate::media_library::is_read_only(&row.path));
    // Only read the file when nothing else can answer for it, and only when it
    // is actually there.
    // `from_path` opens the file; on a pseudo-URI it fails, so there is
    // nothing to gain by trying.
    let tags = if row.needs_tags && exists && !is_disc {
        crate::model::Track::from_path(&row.path).ok().map(|mut t| {
            // `from_path` reads tags, not length. The library supplies a
            // duration for any row it knows; measuring it here for one it does
            // not means the duration and the 🔒 marker reach the row in the
            // same update instead of as two separate flickers.
            t.duration = crate::duration_probe::probe_duration_full(&row.path);
            t
        })
    } else {
        None
    };
    RowFacts {
        id: row.id,
        path: row.path.clone(),
        exists,
        read_only,
        tags,
    }
}

/// Start the single background worker that finishes rows.
///
/// **One thread for the session, and one row at a time.** These are
/// `stat`-class calls, and for unknown files a full tag read, against whatever
/// the library lives on. Fanning them across every Rayon worker would keep the
/// UI responsive while making the machine unusable — on a spinning disk it is a
/// seek storm. Sequential keeps the whole pass invisible, and nothing
/// downstream cares how long it takes: each result patches exactly one row
/// whenever it happens to arrive.
///
/// A persistent worker rather than a thread per batch, because the producer is
/// now the scroll handler: dragging the scrollbar through a long playlist would
/// otherwise spawn a thread per stop along the way.
///
/// Both channels ending stops the walk: a closed window drops the receiver, and
/// the next send fails rather than checking files nobody is waiting for.
pub fn spawn_row_worker(rx: Receiver<Vec<RowCheck>>, tx: Sender<RowFacts>) {
    std::thread::spawn(move || {
        for batch in rx {
            for row in batch {
                if tx.send(check_row(&row)).is_err() {
                    return;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn check(path: &str, needs_tags: bool) -> RowCheck {
        RowCheck {
            path: PathBuf::from(path),
            needs_tags,
            id: 1,
        }
    }

    /// Drive the worker with one batch and collect that many answers.
    fn run(rows: Vec<RowCheck>) -> Vec<RowFacts> {
        let want = rows.len();
        let (check_tx, check_rx) = std::sync::mpsc::channel();
        let (facts_tx, facts_rx) = std::sync::mpsc::channel();
        spawn_row_worker(check_rx, facts_tx);
        check_tx.send(rows).expect("worker is listening");
        (0..want)
            .map(|_| {
                facts_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("a result for every row handed in")
            })
            .collect()
    }

    #[test]
    fn reports_a_missing_file_as_absent_and_not_read_only() {
        let got = run(vec![check("/no/such/file.mp3", false)]);
        assert_eq!(got[0].path, PathBuf::from("/no/such/file.mp3"));
        assert!(!got[0].exists);
        assert!(!got[0].read_only, "a file that is not there is not read-only");
    }

    #[test]
    fn reports_an_existing_file_as_present() {
        let got = run(vec![check("/dev/null", false)]);
        assert!(got[0].exists, "/dev/null exists");
    }

    /// A library row must never pay for a tag read — that is the whole point
    /// of keeping `needs_tags` on the request rather than inferring it.
    #[test]
    fn does_not_read_tags_when_the_library_already_has_them() {
        let got = run(vec![check("/dev/null", false)]);
        assert!(got[0].tags.is_none());
    }

    /// A missing file is never opened, however unknown it is.
    #[test]
    fn does_not_read_tags_for_a_file_that_is_not_there() {
        let got = run(vec![check("/no/such/file.mp3", true)]);
        assert!(got[0].tags.is_none());
    }

    #[test]
    fn reports_every_row_in_the_order_given() {
        let rows: Vec<RowCheck> = (0..5)
            .map(|i| check(&format!("/no/such/file{i}.mp3"), false))
            .collect();
        let want: Vec<PathBuf> = rows.iter().map(|r| r.path.clone()).collect();
        let got = run(rows);
        let saw: Vec<PathBuf> = got.into_iter().map(|f| f.path).collect();
        assert_eq!(saw, want);
    }

    /// The worker outlives one batch — this is the whole reason it is not a
    /// thread per request.
    #[test]
    fn keeps_serving_batches_until_the_sender_is_dropped() {
        let (check_tx, check_rx) = std::sync::mpsc::channel();
        let (facts_tx, facts_rx) = std::sync::mpsc::channel();
        spawn_row_worker(check_rx, facts_tx);
        for i in 0..3 {
            check_tx
                .send(vec![check(&format!("/no/such/file{i}.mp3"), false)])
                .expect("worker is still listening");
            let got = facts_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("one result per batch");
            assert_eq!(got.path, PathBuf::from(format!("/no/such/file{i}.mp3")));
        }
        drop(check_tx);
        assert!(
            facts_rx.recv_timeout(Duration::from_secs(5)).is_err(),
            "worker ends when no more batches can arrive"
        );
    }

    #[test]
    fn an_empty_batch_is_harmless() {
        let (check_tx, check_rx) = std::sync::mpsc::channel();
        let (facts_tx, facts_rx) = std::sync::mpsc::channel();
        spawn_row_worker(check_rx, facts_tx);
        check_tx.send(Vec::new()).expect("worker is listening");
        drop(check_tx);
        assert!(facts_rx.recv_timeout(Duration::from_secs(5)).is_err());
    }
}

#[cfg(test)]
mod disc_row_tests {
    use super::*;
    use std::path::PathBuf;

    fn row(path: &str) -> RowCheck {
        RowCheck {
            id: 1,
            path: PathBuf::from(path),
            needs_tags: true,
        }
    }

    #[test]
    fn a_disc_track_is_present_and_read_only_without_being_stated() {
        // `cdda://1?device=/dev/sr0` is not a file. Before this, `exists()`
        // answered false for every audio-CD row and the playlist marked them
        // all broken while the disc played perfectly well.
        let facts = check_row(&row("cdda://1?device=/dev/sr0"));
        assert!(facts.exists, "a disc track is present by definition");
        assert!(facts.read_only, "optical media is never writable in place");
        assert!(
            facts.tags.is_none(),
            "from_path cannot open a pseudo-URI, so it must not be attempted"
        );
    }

    #[test]
    fn a_missing_file_is_still_reported_missing() {
        // The disc guard must not become a blanket "everything is fine".
        let facts = check_row(&row("/nonexistent/definitely-not-here.mp3"));
        assert!(!facts.exists);
        assert!(!facts.read_only);
    }
}
