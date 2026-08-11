//! The background pass that finishes a playlist row.
//!
//! Adding a row to the active playlist is a data copy. For a file the media
//! library knows, everything worth showing — title, artist, album, duration —
//! is already in its record, so the row can appear complete and instantly.
//!
//! Two things never come from the database:
//!
//! * whether the file is still on disk, and whether it can be written; and
//! * for a file the library has *never seen*, its tags and duration, which
//!   only exist inside the file itself.
//!
//! Both need the filesystem, and asking inline is what made a large add
//! freeze the UI — `Track::from_path` measured 27.974 ms per file, so a 36k
//! add cost about seventeen minutes on the GTK main thread. So they are asked
//! for here instead, after the rows are already on screen.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

/// One row to finish: its path, and whether the library had a record for it.
///
/// `needs_tags` is false for a library row — its tags came from the database
/// and re-reading them off disk would be work for an answer already held.
pub struct RowCheck {
    pub path: PathBuf,
    pub needs_tags: bool,
}

/// What the pass learned about one row.
pub struct RowFacts {
    /// Identifies the row. Matched by path rather than index, so results stay
    /// correct across a reorder, a removal, or a second add landing first.
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

/// Finish `rows` on a background thread, one at a time, reporting each as it
/// is measured.
///
/// **Sequential on purpose.** These are `stat`-class calls, and for unknown
/// files a full tag read, against whatever the library lives on. Fanning
/// 36,000 of them across every Rayon worker at once would keep the UI
/// responsive while making the machine unusable — on a spinning disk it is a
/// seek storm. One at a time keeps the whole pass invisible, and nothing
/// downstream cares how long it takes: each result patches exactly one row
/// whenever it happens to arrive.
///
/// Sending stops as soon as the receiver is gone, so a closed window ends the
/// walk rather than checking tens of thousands of files nobody is waiting for.
pub fn spawn_row_checks(rows: Vec<RowCheck>, tx: Sender<RowFacts>) {
    if rows.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for row in rows {
            let exists = row.path.exists();
            let read_only = exists && crate::media_library::is_read_only(&row.path);
            // Only read the file when nothing else can answer for it, and only
            // when it is actually there.
            let tags = if row.needs_tags && exists {
                crate::model::Track::from_path(&row.path).ok().map(|mut t| {
                    // `from_path` reads tags, not length. The library supplies
                    // a duration for any row it knows; measuring it here for
                    // one it does not means the duration and the 🔒 marker
                    // reach the row in the same update instead of as two
                    // separate flickers.
                    t.duration = crate::duration_probe::probe_duration_full(&row.path);
                    t
                })
            } else {
                None
            };
            let facts = RowFacts {
                path: row.path,
                exists,
                read_only,
                tags,
            };
            if tx.send(facts).is_err() {
                return;
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
        }
    }

    #[test]
    fn reports_a_missing_file_as_absent_and_not_read_only() {
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_row_checks(vec![check("/no/such/file.mp3", false)], tx);
        let got = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a result for every row handed in");
        assert_eq!(got.path, PathBuf::from("/no/such/file.mp3"));
        assert!(!got.exists);
        assert!(!got.read_only, "a file that is not there is not read-only");
    }

    #[test]
    fn reports_an_existing_file_as_present() {
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_row_checks(vec![check("/dev/null", false)], tx);
        let got = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a result for every row handed in");
        assert!(got.exists, "/dev/null exists");
    }

    /// A library row must never pay for a tag read — that is the whole point
    /// of keeping `needs_tags` on the request rather than inferring it.
    #[test]
    fn does_not_read_tags_when_the_library_already_has_them() {
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_row_checks(vec![check("/dev/null", false)], tx);
        let got = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(got.tags.is_none());
    }

    /// A missing file is never opened, however unknown it is.
    #[test]
    fn does_not_read_tags_for_a_file_that_is_not_there() {
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_row_checks(vec![check("/no/such/file.mp3", true)], tx);
        let got = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(got.tags.is_none());
    }

    #[test]
    fn reports_every_row_in_the_order_given() {
        let (tx, rx) = std::sync::mpsc::channel();
        let rows: Vec<RowCheck> = (0..5)
            .map(|i| check(&format!("/no/such/file{i}.mp3"), false))
            .collect();
        let want: Vec<PathBuf> = rows.iter().map(|r| r.path.clone()).collect();
        spawn_row_checks(rows, tx);
        for w in &want {
            let got = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("one result per row");
            assert_eq!(&got.path, w);
        }
    }

    #[test]
    fn an_empty_batch_spawns_nothing_and_does_not_hang() {
        let (tx, rx) = std::sync::mpsc::channel::<RowFacts>();
        spawn_row_checks(Vec::new(), tx);
        // The sender is dropped without a thread ever taking it, so the
        // receiver disconnects immediately rather than blocking.
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_err());
    }
}
