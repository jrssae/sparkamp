//! Path grants: which grants cover which paths, the two lifetimes, and the
//! startup prune.
//!
//! Real bookmarks exist only inside a sandbox, so these store placeholder
//! bytes through `store_path_grant` and test what the table decides.

use super::*;
use std::path::PathBuf;

fn grant_count(lib: &MediaLibrary) -> i64 {
    lib.conn
        .query_row("SELECT COUNT(*) FROM path_grants", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn a_folder_grant_covers_the_files_under_it_and_nothing_beside_it() {
    let (lib, _db) = temp_lib();
    lib.store_path_grant(Path::new("/Users/me/Music"), b"bm", false)
        .unwrap();

    assert!(
        lib.path_is_granted(Path::new("/Users/me/Music"), false)
            .unwrap()
    );
    assert!(
        lib.path_is_granted(Path::new("/Users/me/Music/Album/a.flac"), false)
            .unwrap()
    );
    // A sibling whose name starts the same is not under it.
    assert!(
        !lib.path_is_granted(Path::new("/Users/me/Music Backup/a.flac"), false)
            .unwrap()
    );
}

#[test]
fn a_library_folder_covers_its_songs() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    lib.add_folder(dir.path().to_str().unwrap()).unwrap();
    let song = dir.path().canonicalize().unwrap().join("a.mp3");

    assert!(lib.path_is_granted(&song, false).unwrap());
    assert!(lib.path_is_granted(&song, true).unwrap());
}

#[test]
fn a_kept_request_is_not_covered_by_a_while_used_grant() {
    // The rip folder must outlive the songs that happened to be dragged in
    // from it, so only another kept grant counts.
    let (lib, _db) = temp_lib();
    lib.store_path_grant(Path::new("/Users/me/Music"), b"bm", false)
        .unwrap();

    assert!(
        lib.path_is_granted(Path::new("/Users/me/Music/Rips"), false)
            .unwrap()
    );
    assert!(
        !lib.path_is_granted(Path::new("/Users/me/Music/Rips"), true)
            .unwrap()
    );
}

#[test]
fn a_kept_grant_stays_kept_when_the_same_path_is_added_again() {
    let (lib, _db) = temp_lib();
    let path = Path::new("/Users/me/Music");
    lib.store_path_grant(path, b"one", true).unwrap();
    lib.store_path_grant(path, b"two", false).unwrap();

    assert_eq!(grant_count(&lib), 1);
    assert!(lib.path_is_granted(path, true).unwrap());
    // Pruning with nothing in use leaves it, because it is kept.
    assert_eq!(lib.prune_path_grants(&[]).unwrap(), 0);
    assert_eq!(grant_count(&lib), 1);
}

#[test]
fn prune_drops_only_while_used_grants_nothing_lies_under() {
    let (lib, _db) = temp_lib();
    lib.store_path_grant(Path::new("/Users/me/Downloads/a.mp3"), b"a", false)
        .unwrap();
    lib.store_path_grant(Path::new("/Users/me/Desktop/Mix"), b"b", false)
        .unwrap();
    lib.store_path_grant(Path::new("/Users/me/Music/Rips"), b"c", true)
        .unwrap();

    let in_use = [PathBuf::from("/Users/me/Desktop/Mix/01.flac")];
    assert_eq!(lib.prune_path_grants(&in_use).unwrap(), 1);

    assert!(
        !lib.path_is_granted(Path::new("/Users/me/Downloads/a.mp3"), false)
            .unwrap(),
        "nothing in the playlist uses the Downloads file any more"
    );
    assert!(
        lib.path_is_granted(Path::new("/Users/me/Desktop/Mix/02.flac"), false)
            .unwrap(),
        "a song in the playlist is under the Mix folder"
    );
    assert!(
        lib.path_is_granted(Path::new("/Users/me/Music/Rips"), true)
            .unwrap(),
        "kept grants are never pruned"
    );
}

#[test]
fn a_grant_that_does_not_resolve_is_reported_and_kept() {
    // A folder on an unplugged drive resolves again when the drive returns,
    // so a failed restore must not delete the row.
    let (lib, _db) = temp_lib();
    lib.store_path_grant(Path::new("/Volumes/Ext/Rips"), b"not a bookmark", true)
        .unwrap();

    let unreachable = lib.restore_path_access().unwrap();
    assert_eq!(unreachable, vec!["/Volumes/Ext/Rips".to_string()]);
    assert_eq!(grant_count(&lib), 1);
}

#[test]
fn grant_path_stores_a_row_exactly_when_it_says_it_did() {
    // Outside a sandbox there is usually no bookmark to take, and none is
    // needed. Either way the answer and the table must agree, and a second
    // grant for the same path is covered by the first.
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let stored = lib.grant_path(dir.path(), false).unwrap();
    assert_eq!(grant_count(&lib), i64::from(stored));
    assert!(!lib.grant_path(dir.path(), false).unwrap() || !stored);
}
