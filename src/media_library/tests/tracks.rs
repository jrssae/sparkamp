//! Removing tracks, streaming removal, the soft-delete and purge pair, and
//! finding a row by path when the same file has two spellings.

use super::*;

// ── remove_track ──────────────────────────────────────────────────────

#[test]
fn remove_track_deletes_from_db() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 2);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let tracks = lib.all_tracks().unwrap();
    assert_eq!(tracks.len(), 2);
    let track_id = tracks[0].id;

    lib.remove_track(track_id).unwrap();

    let remaining = lib.all_tracks().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_ne!(remaining[0].id, track_id);
}

#[test]
fn remove_nonexistent_track_is_not_an_error() {
    let (lib, _db) = temp_lib();
    let result = lib.remove_track(99999);
    assert!(
        result.is_ok(),
        "removing nonexistent track should not error"
    );
}

// ── remove_tracks_streaming ───────────────────────────────────────────

#[test]
fn remove_tracks_streaming_sends_ids_and_returns_count() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 5);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let tracks = lib.all_tracks().unwrap();
    assert_eq!(tracks.len(), 5);
    let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();

    let (tx, rx) = std::sync::mpsc::channel();
    let count = lib.remove_tracks_streaming(&ids, tx).unwrap();

    assert_eq!(count, 5);
    let received: Vec<i64> = rx.try_iter().collect();
    assert_eq!(received.len(), 5);

    let remaining = lib.all_tracks().unwrap();
    assert_eq!(remaining.len(), 0);
}

#[test]
fn remove_tracks_streaming_empty_ids_returns_zero() {
    let (lib, _db) = temp_lib();
    let (tx, _rx) = std::sync::mpsc::channel();
    let count = lib.remove_tracks_streaming(&[], tx).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn remove_tracks_streaming_large_batch_chunks_correctly() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    const BATCH: usize = 1001;
    for i in 0..BATCH {
        let file_path = dir.path().join(format!("track_{}.mp3", i));
        fs::write(&file_path, b"fake audio").unwrap();
    }
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let ids: Vec<i64> = lib.all_tracks().unwrap().iter().map(|t| t.id).collect();
    assert_eq!(ids.len(), BATCH);

    let (tx, rx) = std::sync::mpsc::channel();
    let count = lib.remove_tracks_streaming(&ids, tx).unwrap();

    assert_eq!(count, BATCH);
    let received: Vec<i64> = rx.try_iter().collect();
    assert_eq!(
        received.len(),
        BATCH,
        "channel should receive every deleted ID"
    );
    assert_eq!(
        lib.all_tracks().unwrap().len(),
        0,
        "all tracks should be removed"
    );
}

// ── soft_delete and purge ──────────────────────────────────────────

#[test]
fn soft_delete_marks_tracks_with_timestamp() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let tracks = lib.all_tracks().unwrap();
    let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();

    // Soft delete 2 tracks
    lib.soft_delete_tracks(&ids[0..2]).unwrap();

    // Check count
    assert_eq!(lib.get_deleted_track_count().unwrap(), 2);

    // Tracks still exist but are marked as deleted
    assert_eq!(lib.all_tracks().unwrap().len(), 3);
}

#[test]
fn purge_deleted_removes_marked_tracks() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let tracks = lib.all_tracks().unwrap();
    let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();

    // Soft delete all tracks
    lib.soft_delete_tracks(&ids).unwrap();

    // Purge them
    let purged = lib.purge_deleted_tracks().unwrap();
    assert_eq!(purged, 3);

    // Tracks are now gone
    assert_eq!(lib.all_tracks().unwrap().len(), 0);
    assert_eq!(lib.get_deleted_track_count().unwrap(), 0);
}

#[test]
fn purge_keeps_active_tracks() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let tracks = lib.all_tracks().unwrap();
    let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();

    // Soft delete only first track
    lib.soft_delete_tracks(&ids[0..1]).unwrap();

    // Purge
    lib.purge_deleted_tracks().unwrap();

    // Only the non-deleted tracks remain
    assert_eq!(lib.all_tracks().unwrap().len(), 2);
}

#[test]
fn cleanup_on_startup_purges_deleted() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let tracks = lib.all_tracks().unwrap();
    let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();

    // Soft delete
    lib.soft_delete_tracks(&ids).unwrap();

    // Cleanup on startup (simulated)
    lib.cleanup_on_startup().unwrap();

    // All deleted
    assert_eq!(lib.all_tracks().unwrap().len(), 0);
}

#[test]
fn soft_delete_empty_ids_is_noop() {
    let (lib, _db) = temp_lib();
    let result = lib.soft_delete_tracks(&[]);
    assert!(result.is_ok());
    assert_eq!(lib.get_deleted_track_count().unwrap(), 0);
}

// ── track_by_path: the two-spellings case ──────────────────────────────

/// A file indexed under one spelling of its directory must still be found
/// when looked up through another that resolves to the same place.
///
/// This is not hypothetical tidiness. On an image-based system `/home` is a
/// symlink to `/var/home`, so a library scanned as `/home/u/x.mp3` and a file
/// dialog returning `/var/home/u/x.mp3` disagree about one file. The exact
/// match that `track_by_path` used to do rejected it, and the playlist editor
/// read that rejection as "not in the library" and silently declined to add
/// the track — a save that appeared to do nothing at all.
#[test]
fn track_by_path_resolves_a_symlinked_spelling_of_the_same_file() {
    #[cfg(not(target_os = "macos"))]
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();
    let root = tempfile::tempdir().unwrap();

    // real/track.mp3, plus link/ -> real/, so the file has two valid names.
    let real = root.path().join("real");
    fs::create_dir(&real).unwrap();
    let file_path = real.join("track.mp3");
    fs::write(&file_path, b"fake audio data").unwrap();

    let link = root.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    // Index it under the real spelling, the way a folder scan would.
    let indexed = file_path.to_str().unwrap();
    let folder_id = lib.add_folder(real.to_str().unwrap()).unwrap().id();
    lib.upsert_track(folder_id, indexed).unwrap();

    // The indexed spelling still works — the fast path is untouched.
    assert!(lib.track_by_path(indexed).is_ok(), "exact match must keep working");

    // The symlinked spelling names the same file and must resolve to it.
    let via_link = link.join("track.mp3");
    let found = lib
        .track_by_path(via_link.to_str().unwrap())
        .expect("a symlinked spelling of an indexed file must be found");
    assert_eq!(found.path, indexed, "must resolve to the indexed row, not a copy");

    // A genuinely absent file must still be an error, not a false positive
    // from the filename-narrowed fallback.
    let absent = root.path().join("nowhere").join("track.mp3");
    assert!(
        lib.track_by_path(absent.to_str().unwrap()).is_err(),
        "a path that does not exist must not match by filename alone"
    );
}

/// The same playlist file registered under two spellings must produce one row.
///
/// Saving a playlist registers the path the user picked; the filesystem
/// watcher then reports the same write under the path `notify` resolved it
/// to. On a system where `/home` is a symlink to `/var/home` those are
/// different strings for one file, and `INSERT OR IGNORE` — which
/// deduplicates on the string — let both in, so the sidebar listed the
/// playlist twice.
#[test]
fn add_playlist_file_does_not_duplicate_a_symlinked_spelling() {
    let (lib, _db) = temp_lib();
    let root = tempfile::tempdir().unwrap();

    let real = root.path().join("real");
    fs::create_dir(&real).unwrap();
    let pl = real.join("mix.m3u8");
    fs::write(&pl, b"#EXTM3U\n").unwrap();

    let link = root.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let first = lib.add_playlist_file(pl.to_str().unwrap()).unwrap();
    let again = lib
        .add_playlist_file(link.join("mix.m3u8").to_str().unwrap())
        .unwrap();

    assert_eq!(first, again, "both spellings must resolve to one playlist row");
    assert_eq!(
        lib.all_playlists().unwrap().len(),
        1,
        "the sidebar reads all_playlists, so a second row is a visible duplicate"
    );

    // A genuinely different playlist with the same filename elsewhere must
    // still register separately — the filename narrowing must not over-match.
    let other = root.path().join("other");
    fs::create_dir(&other).unwrap();
    let other_pl = other.join("mix.m3u8");
    fs::write(&other_pl, b"#EXTM3U\n").unwrap();
    let third = lib.add_playlist_file(other_pl.to_str().unwrap()).unwrap();
    assert_ne!(third, first, "a different file sharing a name is not the same playlist");
    assert_eq!(lib.all_playlists().unwrap().len(), 2);
}

/// A folder scan must not re-add a playlist under a second spelling.
///
/// The `folders` table can hold two names for one directory (`/home/u/f` and
/// `/var/home/u/f` where `/home` is a symlink). Scanning both walked the same
/// playlist twice, and the scan loops had their own INSERT keyed on the path
/// string — so guarding only `add_playlist_file` left this route wide open,
/// which is exactly how the duplicate survived the first fix.
#[test]
fn scanning_a_folder_twice_under_two_spellings_adds_one_playlist() {
    let (lib, _db) = temp_lib();
    let root = tempfile::tempdir().unwrap();

    let real = root.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("set.m3u8"), b"#EXTM3U\n").unwrap();

    let link = root.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    // Both spellings registered as folders, as a real library ends up.
    let a = lib.add_folder(real.to_str().unwrap()).unwrap().id();
    let b = lib.add_folder(link.to_str().unwrap()).unwrap().id();
    lib.rescan_folder_fast(a, real.to_str().unwrap(), false).unwrap();
    lib.rescan_folder_fast(b, link.to_str().unwrap(), false).unwrap();

    assert_eq!(
        lib.all_playlists().unwrap().len(),
        1,
        "one playlist file must yield one row however many spellings scan it"
    );
}

/// Two names for one file with no symlink anywhere must still be one row.
///
/// A hard link is the testable stand-in for the case that actually bit:
/// `/home` and `/var/home` bind-mounting the same content. Neither is a
/// symlink, so `fs::canonicalize` resolves each name to itself and a
/// canonical-path comparison reports two different files. Two fixes built on
/// that comparison shipped and changed nothing. `(device, inode)` is the
/// identity that survives it — this test fails under canonicalisation and
/// passes under stat.
#[test]
fn add_playlist_file_dedupes_a_hard_link_not_just_a_symlink() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();

    let a = dir.path().join("a.m3u8");
    fs::write(&a, b"#EXTM3U\n").unwrap();
    // Same inode, different path, and neither path is a link to the other as
    // far as path resolution is concerned.
    let b = dir.path().join("b.m3u8");
    fs::hard_link(&a, &b).unwrap();
    assert_eq!(
        fs::canonicalize(&a).unwrap().parent(),
        fs::canonicalize(&b).unwrap().parent(),
        "sanity: same directory",
    );
    assert_ne!(
        fs::canonicalize(&a).unwrap(),
        fs::canonicalize(&b).unwrap(),
        "canonicalize cannot see that these are one file — the whole point",
    );

    let first = lib.add_playlist_file(a.to_str().unwrap()).unwrap();
    let again = lib.add_playlist_file(b.to_str().unwrap()).unwrap();
    assert_eq!(first, again, "one inode must mean one playlist row");
    assert_eq!(lib.all_playlists().unwrap().len(), 1);
}
