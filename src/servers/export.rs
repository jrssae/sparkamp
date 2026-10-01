//! Exporting local changes so they can be copied onto a server.
//!
//! The API cannot write tags or upload files, so local work reaches a server
//! by hand: export to a USB stick (or any folder), then copy the export onto
//! the server's music folder. Songs whose local copy is ahead go to the
//! server's own relative path, so the copy replaces the old file instead of
//! adding a second one the matcher could not tell apart. Local-only songs go
//! to their path relative to their watched folder. Export never deletes.
//!
//! SMB later is this same export with the mounted server folder as the
//! destination.

use crate::media_library::MediaLibrary;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Why a file is in the export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    /// The local copy is ahead of the server's copy, which it replaces.
    ReplacesServerCopy,
    /// The server does not have this song yet.
    NewOnServer,
}

/// One file to export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportItem {
    pub source: PathBuf,
    /// Where it goes, relative to the export folder.
    pub dest_rel: PathBuf,
    pub kind: ExportKind,
    /// Something the user should know before copying, e.g. a path that
    /// already holds a different song on the server.
    pub warning: Option<String>,
}

/// What would be exported for `server_id`.
pub fn plan_export(lib: &MediaLibrary, server_id: &str) -> Result<Vec<ExportItem>> {
    use crate::media_library::servers::{Member, SourceFilter};
    use crate::servers::merge::{CopyId, FieldOutcome};

    let server_rows = lib.server_songs(server_id)?;
    let root = common_root(server_rows.iter().filter_map(|r| r.song.path.as_deref()));
    let server_rel = |path: &str| -> String {
        match &root {
            Some(root) => path.strip_prefix(root.as_str()).unwrap_or(path).trim_start_matches('/').to_string(),
            None => path.trim_start_matches('/').to_string(),
        }
    };
    // Paths the server already uses, lowercased, to catch a new song that
    // would land on top of a different one.
    let taken: std::collections::HashMap<String, i64> = server_rows
        .iter()
        .filter_map(|r| r.song.path.as_deref().map(|p| (server_rel(p).to_lowercase(), r.id)))
        .collect();
    let rel_paths = lib.local_rel_paths()?;

    let mut items = Vec::new();
    for row in lib.library_rows(&SourceFilter::Local, None, "filename", false)? {
        let id = row.track.id;
        let source = PathBuf::from(&row.track.path);
        if !row.servers.iter().any(|s| s == server_id) {
            let rel = rel_paths.get(&id).cloned().unwrap_or_else(|| row.track.filename.clone());
            let warning = taken.get(&rel.to_lowercase()).map(|_| {
                format!("{server_id} already has a different song at {rel}; copying would replace it")
            });
            items.push(ExportItem {
                source,
                dest_rel: PathBuf::from(rel),
                kind: ExportKind::NewOnServer,
                warning,
            });
            continue;
        }
        let Some(merged) = lib.song_merge(Member::Local(id))? else { continue };
        let behind = merged.fields.iter().any(|(_, o)| {
            matches!(o, FieldOutcome::Wins { behind, .. }
                if behind.contains(&CopyId::Server(server_id.to_string())))
        });
        if !behind {
            continue;
        }
        let server_copy = lib.song_copies(Member::Local(id))?.and_then(|c| {
            c.members.iter().find_map(|m| match m.member {
                Member::Server(sid) => lib
                    .server_row(sid)
                    .ok()
                    .flatten()
                    .filter(|r| r.server_id == server_id),
                Member::Local(_) => None,
            })
        });
        let Some(copy) = server_copy else { continue };
        let Some(path) = copy.song.path.as_deref() else { continue };
        items.push(ExportItem {
            source,
            dest_rel: PathBuf::from(server_rel(path)),
            kind: ExportKind::ReplacesServerCopy,
            warning: (!path.starts_with('/')).then(|| {
                format!("{server_id} does not report real paths; turn on Report Real Path so this lands on the right file")
            }),
        });
    }
    Ok(items)
}

/// The deepest directory every absolute path shares: the server's library
/// root as far as its paths reveal it. `None` when no path is absolute.
fn common_root<'a>(paths: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut root: Option<Vec<&str>> = None;
    for p in paths.filter(|p| p.starts_with('/')) {
        let dirs: Vec<&str> = p.split('/').collect();
        let dirs = &dirs[..dirs.len() - 1];
        root = Some(match root {
            None => dirs.to_vec(),
            Some(r) => r.iter().zip(dirs).take_while(|(a, b)| a == b).map(|(a, _)| *a).collect(),
        });
    }
    root.map(|parts| parts.join("/"))
}

/// What writing an export did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExportReport {
    pub copied: usize,
    pub bytes: u64,
}

/// Copy `items` under `dest_root` and write `sparkamp-export.txt` beside
/// them. Existing files at a destination are replaced; nothing else is
/// touched.
pub fn write_export(items: &[ExportItem], dest_root: &Path, server_name: &str) -> Result<ExportReport> {
    let mut report = ExportReport::default();
    for item in items {
        let dest = dest_root.join(&item.dest_rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        report.bytes += std::fs::copy(&item.source, &dest)?;
        report.copied += 1;
    }
    let mut note = format!(
        "Sparkamp export for {server_name}\n\n\
         Copy everything in this folder (except this file) into {server_name}'s music folder,\n\
         merging folders and replacing files that already exist. Then let the server rescan.\n\
         Sparkamp notices the new files at its next update and marks the songs as in sync.\n\n\
         Files:\n"
    );
    for item in items {
        let what = match item.kind {
            ExportKind::ReplacesServerCopy => "replaces",
            ExportKind::NewOnServer => "new",
        };
        note.push_str(&format!("  [{what}] {}\n", item.dest_rel.display()));
        if let Some(w) = &item.warning {
            note.push_str(&format!("         note: {w}\n"));
        }
    }
    std::fs::write(dest_root.join("sparkamp-export.txt"), note)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_library::servers::Member;
    use crate::servers::api::ServerSong;
    use crate::servers::matcher::LinkReason;

    struct Lib {
        lib: MediaLibrary,
        _db: tempfile::NamedTempFile,
        dir: tempfile::TempDir,
    }

    /// Local files `Artist/Album/01.mp3` and `Artist/Album/02.mp3`, oscar
    /// holding `/music/Artist/Album/01.mp3` (linked to the first) and
    /// `/music/Other/X/02.mp3`.
    fn setup() -> Lib {
        let db = tempfile::NamedTempFile::with_suffix(".db").unwrap();
        let lib = MediaLibrary::open_at(db.path()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("Artist").join("Album");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(album.join("01.mp3"), b"one").unwrap();
        std::fs::write(album.join("02.mp3"), b"two").unwrap();
        let root = dir.path().canonicalize().unwrap();
        let folder = lib.add_folder(root.to_str().unwrap()).unwrap().id();
        lib.rescan_folder_fast(folder, root.to_str().unwrap(), true).unwrap();

        let pull = lib.begin_server_pull("oscar").unwrap();
        let song = |id: &str, path: &str| ServerSong {
            id: id.into(),
            path: Some(path.into()),
            title: "Song".into(),
            artist: "Artist".into(),
            ..ServerSong::default()
        };
        lib.apply_server_songs(
            "oscar",
            pull,
            &[song("s1", "/music/Artist/Album/01.mp3"), song("s2", "/music/Other/X/02.mp3")],
        )
        .unwrap();
        lib.finish_server_pull("oscar", pull).unwrap();
        let (one, _) = local_ids(&lib);
        let s1 = lib.server_songs("oscar").unwrap()[0].id;
        lib.link_copies(Member::Local(one), Member::Server(s1), LinkReason::Path).unwrap();
        lib.conn_for_tests()
            .execute("UPDATE tracks SET title = 'Song', artist = 'Artist'", [])
            .unwrap();
        lib.accept_differences(Member::Local(one)).unwrap();
        Lib { lib, _db: db, dir }
    }

    fn local_ids(lib: &MediaLibrary) -> (i64, i64) {
        let t = lib.all_tracks_sorted("filename", false).unwrap();
        (t[0].id, t[1].id)
    }

    #[test]
    fn a_local_only_song_goes_to_its_library_relative_path() {
        let s = setup();
        let items = plan_export(&s.lib, "oscar").unwrap();
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].dest_rel, PathBuf::from("Artist/Album/02.mp3"));
        assert_eq!(items[0].kind, ExportKind::NewOnServer);
        assert_eq!(items[0].warning, None);
    }

    #[test]
    fn a_song_ahead_of_the_server_goes_to_the_servers_own_path() {
        let s = setup();
        let (one, _) = local_ids(&s.lib);
        s.lib.conn_for_tests().execute("UPDATE tracks SET genre = 'Jazz' WHERE id = ?1", [one]).unwrap();
        let items = plan_export(&s.lib, "oscar").unwrap();
        let replace: Vec<_> = items.iter().filter(|i| i.kind == ExportKind::ReplacesServerCopy).collect();
        assert_eq!(replace.len(), 1);
        assert_eq!(replace[0].dest_rel, PathBuf::from("Artist/Album/01.mp3"), "below oscar's /music root");
    }

    #[test]
    fn a_song_only_another_server_lacks_is_not_exported_for_this_one() {
        let s = setup();
        assert!(plan_export(&s.lib, "oscar").unwrap().iter().all(|i| i.kind == ExportKind::NewOnServer));
        assert!(plan_export(&s.lib, "server2").unwrap().len() == 2, "server2 has neither song");
    }

    #[test]
    fn a_new_song_landing_on_a_path_the_server_uses_for_another_song_is_flagged() {
        let s = setup();
        // oscar's other song sits at Other/X/02.mp3; put a local file there.
        let other = s.dir.path().join("Other").join("X");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("02.mp3"), b"different").unwrap();
        let root = s.dir.path().canonicalize().unwrap();
        let folder = s.lib.list_folders().unwrap()[0].0;
        s.lib.rescan_folder_fast(folder, root.to_str().unwrap(), true).unwrap();
        let items = plan_export(&s.lib, "oscar").unwrap();
        let clash = items.iter().find(|i| i.dest_rel == PathBuf::from("Other/X/02.mp3")).unwrap();
        assert!(clash.warning.as_deref().unwrap_or("").contains("already"), "{clash:?}");
    }

    #[test]
    fn writing_an_export_copies_files_and_explains_where_they_go() {
        let s = setup();
        let items = plan_export(&s.lib, "oscar").unwrap();
        let out = tempfile::tempdir().unwrap();
        let report = write_export(&items, out.path(), "oscar").unwrap();
        assert_eq!(report, ExportReport { copied: 1, bytes: 3 });
        assert_eq!(std::fs::read(out.path().join("Artist/Album/02.mp3")).unwrap(), b"two");
        let note = std::fs::read_to_string(out.path().join("sparkamp-export.txt")).unwrap();
        assert!(note.contains("oscar") && note.contains("Artist/Album/02.mp3"), "{note}");
        // The sources are untouched.
        assert!(s.dir.path().join("Artist/Album/02.mp3").exists());
    }
}
