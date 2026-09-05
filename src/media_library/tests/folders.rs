//! Watched folders: adding, removing, recursion, and the path canonicalisation
//! that decides whether two spellings are one folder.

use super::*;

// ── add_folder / remove_folder ─────────────────────────────────────────

#[test]
fn add_folder_inserts_and_returns_id() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();

    let r1 = lib.add_folder(path).unwrap();
    let r2 = lib.add_folder(path).unwrap();
    assert!(
        matches!(r1, AddFolderResult::New(_)),
        "first add should return New"
    );
    assert!(
        matches!(r2, AddFolderResult::AlreadyExists(_)),
        "second add should return AlreadyExists"
    );
    assert_eq!(r1.id(), r2.id(), "both calls return the same folder ID");
}

#[test]
fn add_folder_duplicate_does_not_insert_row() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();

    let r1 = lib.add_folder(path).unwrap();
    assert!(matches!(r1, AddFolderResult::New(_)));
    assert_eq!(lib.list_folders().unwrap().len(), 1);

    // Re-adding must return AlreadyExists and NOT insert a second row.
    let r2 = lib.add_folder(path).unwrap();
    assert!(matches!(r2, AddFolderResult::AlreadyExists(_)));
    assert_eq!(
        lib.list_folders().unwrap().len(),
        1,
        "duplicate add must not create a second row"
    );
    assert_eq!(r1.id(), r2.id());
}

#[test]
fn folder_exists_returns_correct_result() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();

    assert!(
        lib.folder_exists(path).unwrap().is_none(),
        "nonexistent folder returns None"
    );

    let folder_id = lib.add_folder(path).unwrap().id();

    assert_eq!(
        lib.folder_exists(path).unwrap(),
        Some(folder_id),
        "existing folder returns its ID"
    );

    assert!(
        lib.folder_exists("/nonexistent/path/xyz")
            .unwrap()
            .is_none(),
        "different path returns None"
    );
}

#[test]
fn remove_folder_deletes_tracks() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    let (added, _) = lib.rescan_folder_fast(folder_id, path, true).unwrap();

    assert_eq!(added, 3, "fast scan should have added 3 files");
    assert_eq!(lib.all_tracks().unwrap().len(), 3);

    lib.remove_folder(folder_id).unwrap();

    assert_eq!(
        lib.all_tracks().unwrap().len(),
        0,
        "all tracks should be removed after remove_folder"
    );
}

// ── add_folder with NUL bytes in path ─────────────────────────────────

#[test]
fn add_folder_path_with_nul_byte_is_handled() {
    let (lib, _db) = temp_lib();
    // A path with embedded NUL bytes should not crash.
    // The path won't exist so add_folder will still work (it's just an insert).
    let result = lib.add_folder("/tmp/test\x00dir");
    // May succeed or fail depending on path resolution, but should not panic.
    assert!(result.is_ok() || result.is_err());
}

// ── folders.recurse column + per-folder recursive walk ─────────────────

#[test]
fn folders_recurse_column_added_once_default_1() {
    let db_file = NamedTempFile::with_suffix(".db").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let folder_id;
    {
        let lib = MediaLibrary::open_at(db_file.path()).unwrap();
        let cols: std::collections::HashSet<String> = {
            let mut stmt = lib
                .conn
                .prepare("SELECT name FROM pragma_table_info('folders')")
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        assert!(
            cols.contains("recurse"),
            "folders table must gain a recurse column via the additive migration"
        );

        folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();
        assert!(
            lib.folder_recurse(folder_id).unwrap(),
            "new folders default to recurse = true (column DEFAULT 1)"
        );

        lib.set_folder_recurse(folder_id, false).unwrap();
        assert!(!lib.folder_recurse(folder_id).unwrap());
    } // `lib` dropped here, closing the connection.

    // Re-opening the same DB file must not error: the ALTER TABLE guard
    // has to be idempotent once the column already exists.
    let lib2 = MediaLibrary::open_at(db_file.path()).unwrap();
    assert!(
        !lib2.folder_recurse(folder_id).unwrap(),
        "recurse flag must persist across reopen"
    );
}

#[test]
fn walk_dir_non_recursive_skips_subdir() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.mp3"), b"fake audio data").unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("b.mp3"), b"fake audio data").unwrap();

    let mut audio: Vec<std::path::PathBuf> = Vec::new();
    let mut m3u: Vec<std::path::PathBuf> = Vec::new();
    MediaLibrary::walk_dir(
        dir.path(),
        crate::model::AUDIO_EXTENSIONS,
        &mut audio,
        &mut m3u,
        false,
    );
    assert_eq!(audio.len(), 1, "non-recursive walk must not descend into sub/");
    assert_eq!(audio[0].file_name().unwrap(), "a.mp3");

    let mut audio_rec: Vec<std::path::PathBuf> = Vec::new();
    let mut m3u_rec: Vec<std::path::PathBuf> = Vec::new();
    MediaLibrary::walk_dir(
        dir.path(),
        crate::model::AUDIO_EXTENSIONS,
        &mut audio_rec,
        &mut m3u_rec,
        true,
    );
    assert_eq!(audio_rec.len(), 2, "recursive walk must find files in sub/");
}

/// Adding a folder must not fill the library with rows that can never play.
///
/// This is the behaviour a user sees: point Sparkamp at a mixed folder and the
/// formats this platform's decoder cannot open are simply not added, rather
/// than appearing as rows that fail the moment they are clicked. The predicate
/// is tested in `model`; this proves the folder walk actually applies it.
#[test]
fn a_folder_scan_skips_containers_this_platform_cannot_decode() {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "keep.mp3", "keep.flac", "keep.ogg", "keep.opus", "keep.m4a", "keep.aiff",
        "maybe.wma", "maybe.tta", "maybe.wv", "maybe.ape",
        "skip.jpg", "skip.txt",
    ] {
        fs::write(dir.path().join(name), b"not really audio").unwrap();
    }

    let mut audio: Vec<std::path::PathBuf> = Vec::new();
    let mut m3u: Vec<std::path::PathBuf> = Vec::new();
    MediaLibrary::walk_dir(
        dir.path(),
        crate::model::AUDIO_EXTENSIONS,
        &mut audio,
        &mut m3u,
        false,
    );
    let names: Vec<String> = audio
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();

    for n in ["keep.mp3", "keep.flac", "keep.ogg", "keep.opus", "keep.m4a", "keep.aiff"] {
        assert!(names.contains(&n.to_string()), "{n} must be scanned: {names:?}");
    }
    assert!(!names.iter().any(|n| n.ends_with(".jpg") || n.ends_with(".txt")));

    // The four CoreAudio refuses. On Linux GStreamer decodes all four, so the
    // same walk must keep them there: this asserts the split, not a deletion.
    let undecodable = ["maybe.wma", "maybe.tta", "maybe.wv", "maybe.ape"];
    #[cfg(target_os = "macos")]
    for n in undecodable {
        assert!(!names.contains(&n.to_string()), "{n} must be skipped on macOS: {names:?}");
    }
    #[cfg(not(target_os = "macos"))]
    for n in undecodable {
        assert!(names.contains(&n.to_string()), "{n} must be kept off macOS: {names:?}");
    }
}

#[test]
fn set_replaygain_roundtrips() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("song.wav");
    write_test_wav(&file_path, 44100, 2, 1.0);
    let path = file_path.to_str().unwrap();
    let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();
    lib.upsert_track(folder_id, path).unwrap();

    // A freshly scanned track carries no ReplayGain values.
    let t = lib.track_by_path(path).unwrap();
    assert_eq!(t.rg_track_gain, None);
    assert_eq!(t.rg_track_peak, None);
    assert_eq!(t.rg_album_gain, None);
    assert_eq!(t.rg_album_peak, None);

    // Analysis results round-trip through the DB.
    lib.set_replaygain(t.id, -6.20, 0.988123, -7.10, 0.995).unwrap();
    let t2 = lib.track_by_path(path).unwrap();
    assert_eq!(t2.rg_track_gain, Some(-6.20));
    assert_eq!(t2.rg_track_peak, Some(0.988123));
    assert_eq!(t2.rg_album_gain, Some(-7.10));
    assert_eq!(t2.rg_album_peak, Some(0.995));
}

// ── path canonicalization (2026-08-11) ─────────────────────────────────
//
// A symlinked folder must not make the library hold every file twice. The
// live case was `/mnt` -> `var/mnt` on Fedora ostree, which grew 8,417
// duplicate rows before it was noticed.

/// `dir` plus a sibling symlink pointing at it, so the same files have two
/// valid spellings.
#[cfg(unix)]
fn dir_with_symlink_alias() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let parent = tempfile::tempdir().unwrap();
    let real = parent.path().join("real_music");
    fs::create_dir(&real).unwrap();
    for i in 0..3 {
        fs::write(real.join(format!("t{i}.mp3")), b"fake audio data").unwrap();
    }
    let alias = parent.path().join("alias_music");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    // `real` has to be the spelling the library will store, because callers
    // compare it against `path` columns directly. On macOS the tempdir itself
    // sits under `/var` -> `/private/var`, so the unresolved form is already an
    // alias and every such comparison missed.
    let real = real.canonicalize().unwrap();
    (parent, real, alias)
}

#[cfg(unix)]
#[test]
fn scanning_through_a_symlink_does_not_duplicate_rows() {
    let (_parent, real, alias) = dir_with_symlink_alias();
    let (lib, _db) = temp_lib();

    // Add via the symlink first — the spelling a file chooser would hand us.
    let id = lib.add_folder(alias.to_str().unwrap()).unwrap().id();
    lib.rescan_folder_fast(id, alias.to_str().unwrap(), false)
        .unwrap();
    let after_alias: i64 = lib
        .conn
        .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after_alias, 3, "three files, three rows");

    // Now the real path. Same files, so still three rows and one folder.
    let id2 = lib.add_folder(real.to_str().unwrap()).unwrap().id();
    lib.rescan_folder_fast(id2, real.to_str().unwrap(), false)
        .unwrap();
    let after_real: i64 = lib
        .conn
        .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after_real, 3, "the same files must not be added twice");
    assert_eq!(id, id2, "both spellings resolve to one folder row");
}

#[cfg(unix)]
#[test]
fn normalize_track_paths_merges_an_alias_row_and_keeps_its_plays() {
    let (_parent, real, alias) = dir_with_symlink_alias();
    let (lib, _db) = temp_lib();
    let fid = lib.add_folder(real.to_str().unwrap()).unwrap().id();
    lib.rescan_folder_fast(fid, real.to_str().unwrap(), false)
        .unwrap();

    // Forge the pre-fix state: a second row for one file under the alias
    // spelling, carrying the play history the canonical row lacks.
    let canonical = real.join("t0.mp3").to_string_lossy().into_owned();
    let aliased = alias.join("t0.mp3").to_string_lossy().into_owned();
    lib.conn
        .execute(
            "INSERT INTO tracks (path, folder_id, filename, play_count, last_played)
             VALUES (?1, ?2, 't0.mp3', 7, '2026-08-01T00:00:00Z')",
            rusqlite::params![aliased, fid],
        )
        .unwrap();
    assert!(lib.needs_path_normalization(), "the alias row is detectable");

    let (moved, merged) = lib.normalize_track_paths().unwrap();
    assert_eq!((moved, merged), (0, 1), "one alias merged, none relocated");

    let total: i64 = lib
        .conn
        .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, 3, "the duplicate is gone");

    let (plays, last): (i64, Option<String>) = lib
        .conn
        .query_row(
            "SELECT play_count, last_played FROM tracks WHERE path = ?1",
            rusqlite::params![canonical],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(plays, 7, "play history survives the merge");
    assert_eq!(last.as_deref(), Some("2026-08-01T00:00:00Z"));
    assert!(!lib.needs_path_normalization(), "and it stays repaired");
}
