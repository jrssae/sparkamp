//! Turning paths into playlist rows, without reading the files.
//!
//! Every frontend has to answer the same question: the user pointed at some
//! paths, what rows should the playlist show? The obvious implementation reads
//! each file for its tags — and that is what the GTK, TUI and CLI paths each
//! did separately, at a measured 27.974 ms per file. For a 36k media library
//! that is about seventeen minutes of blocking work.
//!
//! Almost none of it is necessary. A file the media library has scanned already
//! has its title, artist, album and duration in the database, and one batched
//! query answers for thousands of paths at once. Only a path the library has
//! never seen needs the file itself, and even then the row can appear
//! immediately with a placeholder while something else reads it.
//!
//! So this module resolves paths to rows and touches the filesystem only to
//! expand directories. Each row comes back with a [`Row::needs_tags`] flag
//! saying whether anything still has to be read for it; what to do about that
//! is the frontend's business — GTK asks only about rows on screen, the TUI
//! hands them to `crate::file_status`.
//!
//! It lives in core because the three frontends drifting apart is not
//! hypothetical: before the GTK side was unified there were 27 add sites, and
//! three of them had quietly lost their duration probe.

use crate::media_library::{LibTrack, MediaLibrary};
use crate::model::{Playlist, Track};
use std::path::{Path, PathBuf};

/// One resolved playlist row.
pub struct Row {
    pub track: Track,
    /// True when nothing but the file itself can answer for its tags and
    /// duration, because the library had no record of this path.
    ///
    /// False for a library row: re-reading its tags off disk would be work for
    /// an answer already held.
    pub needs_tags: bool,
}

/// Resolve `paths` into playlist rows.
///
/// One batched query covers the whole batch, so a path the library knows costs
/// no filesystem access whatever. A path it has never seen gets a placeholder
/// row — the filename stem, no duration — flagged `needs_tags`, so it can be
/// shown at once and finished later rather than holding up the other 35,999.
///
/// Directories are expanded recursively, but only when the library could not
/// place them: a library row is a file by definition, so adding a scanned
/// library never asks the filesystem anything, while a folder dropped from a
/// file manager still expands the way it always has.
///
/// A library file reached under a different spelling of its path (a bind mount
/// spelling `/mnt` where the scan recorded `/var/mnt`) misses the exact match
/// and is treated as unknown: correct, since it will be read from disk, just
/// slower for that one row.
///
/// `lib` is `None` on a machine with no library, or before one is opened. Every
/// path then comes back needing tags, which is the right answer.
pub fn resolve(lib: Option<&MediaLibrary>, paths: &[PathBuf]) -> Vec<Row> {
    if paths.is_empty() {
        return Vec::new();
    }
    let mut wanted: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let mut known = lookup(lib, &wanted);

    // Only paths the library could not place are candidates for expansion.
    let needs_expanding = paths
        .iter()
        .zip(wanted.iter())
        .any(|(p, key)| !known.contains_key(key) && p.is_dir());
    let paths: Vec<PathBuf> = if needs_expanding {
        let mut expanded = Vec::with_capacity(paths.len());
        for p in paths {
            if p.is_dir() {
                expanded.extend(Playlist::collect_audio_files(p));
            } else {
                expanded.push(p.clone());
            }
        }
        wanted = expanded.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        // The files inside a dropped folder may well be indexed even though the
        // folder itself is not a row, so resolve again now they are named.
        known = lookup(lib, &wanted);
        expanded
    } else {
        paths.to_vec()
    };

    paths
        .iter()
        .zip(wanted.iter())
        .map(|(path, key)| match known.get(key) {
            Some(lt) => Row {
                track: Track::from(lt),
                needs_tags: false,
            },
            None => Row {
                track: placeholder(path),
                needs_tags: true,
            },
        })
        .collect()
}

/// One batched library lookup. An unopenable or absent library is not an error
/// here — every path simply comes back unknown.
fn lookup(
    lib: Option<&MediaLibrary>,
    wanted: &[String],
) -> std::collections::HashMap<String, LibTrack> {
    lib.and_then(|lib| lib.tracks_by_exact_paths(wanted).ok())
        .unwrap_or_default()
}

/// A row for a file nothing has told us about yet.
///
/// Built without touching the filesystem — even `Track::from_path_fast`
/// canonicalises and tests writability, which is two syscalls we are trying not
/// to spend here.
pub fn placeholder(path: &Path) -> Track {
    Track {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_batch_resolves_to_nothing() {
        assert!(resolve(None, &[]).is_empty());
    }

    /// Without a library every path is unknown, and none of them are read.
    #[test]
    fn without_a_library_every_path_needs_tags() {
        let paths = vec![
            PathBuf::from("/music/a.mp3"),
            PathBuf::from("/music/b.flac"),
        ];
        let rows = resolve(None, &paths);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.needs_tags));
        assert!(rows.iter().all(|r| r.duration_is_none()));
    }

    /// The placeholder has to be recognisable straight away — the filename
    /// stem is what the user pointed at.
    #[test]
    fn placeholder_titles_a_row_with_its_filename_stem() {
        let t = placeholder(Path::new("/music/Pearl Jam - Black.mp3"));
        assert_eq!(t.title, "Pearl Jam - Black");
        assert_eq!(t.artist, "");
        assert!(t.duration.is_none());
        assert!(!t.broken, "nothing has been checked yet, so nothing is known bad");
        assert!(!t.read_only);
    }

    #[test]
    fn placeholder_survives_a_path_with_no_stem() {
        assert_eq!(placeholder(Path::new("/")).title, "Unknown");
    }

    /// Rows come back in the order asked for, so callers can map a result back
    /// to the path that produced it by position.
    #[test]
    fn rows_keep_the_order_of_the_paths_given() {
        let paths: Vec<PathBuf> = (0..5)
            .map(|i| PathBuf::from(format!("/music/{i}.mp3")))
            .collect();
        let rows = resolve(None, &paths);
        let got: Vec<PathBuf> = rows.into_iter().map(|r| r.track.path).collect();
        assert_eq!(got, paths);
    }

    /// A path that is not a directory is never expanded, so resolving cannot
    /// invent rows the caller did not ask for.
    #[test]
    fn a_missing_path_yields_exactly_one_row() {
        let rows = resolve(None, &[PathBuf::from("/no/such/file.mp3")]);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].needs_tags);
    }

    impl Row {
        fn duration_is_none(&self) -> bool {
            self.track.duration.is_none()
        }
    }

    /// Live measurement against this machine's real library: resolve every
    /// track it knows, and compare against what reading the same files costs.
    ///
    /// This is the claim the module exists for, so it is worth being able to
    /// check rather than assert. Needs a populated library.
    ///
    /// `cargo test --lib live_resolve_whole_library -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_resolve_whole_library() {
        let Some(lib) = MediaLibrary::open().ok() else {
            eprintln!("no media library on this machine — skipping");
            return;
        };
        let Ok(tracks) = lib.all_tracks_sorted("path", false) else {
            eprintln!("could not read the library — skipping");
            return;
        };
        if tracks.is_empty() {
            eprintln!("library is empty — skipping");
            return;
        }
        let paths: Vec<PathBuf> = tracks.iter().map(|t| PathBuf::from(&t.path)).collect();
        let n = paths.len();

        let started = std::time::Instant::now();
        let rows = resolve(Some(&lib), &paths);
        let resolved_in = started.elapsed();

        let unknown = rows.iter().filter(|r| r.needs_tags).count();
        eprintln!("resolve: {n} paths in {resolved_in:?}");
        eprintln!("         {} from the library, {unknown} needing a file read", n - unknown);

        // What the old add path cost, sampled rather than run 36,000 times.
        let sample = paths.iter().take(50).collect::<Vec<_>>();
        let started = std::time::Instant::now();
        let read = sample
            .iter()
            .filter(|p| crate::model::Track::from_path(p).is_ok())
            .count();
        let per_file = started.elapsed() / sample.len().max(1) as u32;
        eprintln!(
            "Track::from_path: {:?} per file over {read}/{} readable",
            per_file,
            sample.len()
        );
        eprintln!(
            "         extrapolated over {n} paths: {:?}",
            per_file * n as u32
        );

        assert_eq!(rows.len(), n, "one row per path");
    }
}
