//! Artwork refresh, and the guard that stops it overwriting good data.

use super::*;

// ── refresh_artwork: data-safety guard ──────────────────────────────────
// Insurance against a regression re-widening the cache-only delete guard
// added in F2 (feat(art): folder-image fallback with cache-guarded
// refresh). Not tied to a bug fix in this wave — locks in current, correct
// behavior so a future edit here can't silently start deleting user files.

#[test]
fn refresh_artwork_deletes_only_cache_dir_files_not_user_images() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("song.mp3");
    fs::write(&file_path, b"fake audio data").unwrap();
    let path = file_path.to_str().unwrap();

    let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();
    lib.upsert_track(folder_id, path).unwrap();
    let track_id = lib.track_by_path(path).unwrap().id;

    // Case 1: artwork_path points at the user's own folder image (F2
    // fallback) — refresh must not delete it, only our cache extractions.
    let user_image = dir.path().join("folder.jpg");
    fs::write(&user_image, b"fake jpg").unwrap();
    lib.conn
        .execute(
            "UPDATE tracks SET artwork_path = ?1 WHERE id = ?2",
            rusqlite::params![user_image.to_string_lossy().as_ref(), track_id],
        )
        .unwrap();
    lib.refresh_artwork(track_id, path).unwrap();
    assert!(
        user_image.exists(),
        "refresh_artwork must not delete a user's own folder image"
    );

    // Case 2: artwork_path points at a file inside our cache dir (a
    // previous APIC extraction) — refresh must remove the stale cache file.
    let cache_root = dirs::cache_dir().unwrap().join("sparkamp");
    fs::create_dir_all(&cache_root).unwrap();
    let cached_art = cache_root.join(format!(
        "refresh_artwork_test_{}_{}.jpg",
        std::process::id(),
        line!()
    ));
    fs::write(&cached_art, b"stale cached art").unwrap();
    lib.conn
        .execute(
            "UPDATE tracks SET artwork_path = ?1 WHERE id = ?2",
            rusqlite::params![cached_art.to_string_lossy().as_ref(), track_id],
        )
        .unwrap();
    lib.refresh_artwork(track_id, path).unwrap();
    assert!(
        !cached_art.exists(),
        "refresh_artwork must delete stale cache-dir extractions"
    );
}
