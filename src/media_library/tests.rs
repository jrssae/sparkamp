//! Integration-style tests against a temp SQLite DB.

use super::*;
use std::fs;
use tempfile::NamedTempFile;

fn temp_lib() -> (MediaLibrary, NamedTempFile) {
    let db_file = NamedTempFile::with_suffix(".db").unwrap();
    let lib = MediaLibrary::open_at(db_file.path()).unwrap();
    (lib, db_file)
}

fn temp_dir_with_files(extension: &str, count: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..count {
        let file_path = dir.path().join(format!("track_{}.{}", i, extension));
        fs::write(&file_path, b"fake audio data").unwrap();
    }
    dir
}

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
    let (added, _) = lib.rescan_folder_fast(folder_id, path).unwrap();

    assert_eq!(added, 3, "fast scan should have added 3 files");
    assert_eq!(lib.all_tracks().unwrap().len(), 3);

    lib.remove_folder(folder_id).unwrap();

    assert_eq!(
        lib.all_tracks().unwrap().len(),
        0,
        "all tracks should be removed after remove_folder"
    );
}

// ── rescan_folder_fast ────────────────────────────────────────────────

#[test]
fn rescan_folder_fast_inserts_audio_files() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    let (added, _) = lib.rescan_folder_fast(folder_id, path).unwrap();

    assert_eq!(added, 3);
    let tracks = lib.all_tracks().unwrap();
    assert_eq!(tracks.len(), 3);
}

#[test]
fn rescan_folder_fast_handles_multiple_extensions() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    for ext in &["mp3", "flac", "ogg", "m4a"] {
        fs::write(dir.path().join(format!("song.{}", ext)), b"x").unwrap();
    }
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    let (added, _) = lib.rescan_folder_fast(folder_id, path).unwrap();

    assert_eq!(added, 4);
}

#[test]
fn rescan_folder_fast_skips_nonexistent_paths() {
    let (lib, _db) = temp_lib();
    let folder_id = lib.add_folder("/nonexistent/path/xyz").unwrap().id();
    let result = lib.rescan_folder_fast(folder_id, "/nonexistent/path/xyz");
    assert!(result.is_ok());
}

#[test]
fn rescan_folder_fast_removes_deleted_files() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();

    // Create and scan 3 files.
    fs::write(dir.path().join("a.mp3"), b"x").unwrap();
    fs::write(dir.path().join("b.mp3"), b"x").unwrap();
    fs::write(dir.path().join("c.mp3"), b"x").unwrap();
    lib.rescan_folder_fast(folder_id, path).unwrap();
    assert_eq!(lib.all_tracks().unwrap().len(), 3);

    // Delete one file and rescan.
    fs::remove_file(dir.path().join("b.mp3")).unwrap();
    let (_, removed) = lib.rescan_folder_fast(folder_id, path).unwrap();

    assert_eq!(removed, 1);
    assert_eq!(lib.all_tracks().unwrap().len(), 2);
}

#[test]
fn rescan_folder_fast_upserts_m3u_playlists() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("My Playlist.m3u"), b"#EXTM3U\n").unwrap();
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path).unwrap();

    let playlists = lib.all_playlists().unwrap();
    assert_eq!(playlists.len(), 1);
    assert_eq!(playlists[0].name, "My Playlist");
}

// ── rescan_folder_metadata ─────────────────────────────────────────────

#[test]
fn rescan_folder_metadata_reports_progress() {
    gstreamer::init().ok();

    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 5);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path).unwrap();

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let progress_count = std::sync::Arc::new(std::sync::Mutex::new(0usize));
    let progress_count_clone = progress_count.clone();

    lib.rescan_folder_metadata(
        folder_id,
        &cancel,
        |done, total| {
            assert!(done <= total);
            *progress_count_clone.lock().unwrap() += 1;
        },
        None,
    )
    .unwrap();

    // Progress callback should have been called.
    assert!(
        *progress_count.lock().unwrap() > 0,
        "progress callback should have been called"
    );
}

#[test]
fn rescan_folder_metadata_respects_cancel() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 10);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path).unwrap();

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);

    // Even with cancel set, it should return Ok (not an error).
    let result = lib.rescan_folder_metadata(folder_id, &cancel, |_, _| {}, None);
    assert!(result.is_ok());
}

#[test]
fn rescan_folder_metadata_sets_last_scanned() {
    gstreamer::init().ok();

    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path).unwrap();

    // Verify tracks have no last_scanned yet
    let tracks_before = lib.all_tracks().unwrap();
    assert!(tracks_before.iter().all(|t| t.last_scanned.is_none()));

    // Run metadata scan
    let cancel = std::sync::atomic::AtomicBool::new(false);
    lib.rescan_folder_metadata(folder_id, &cancel, |_, _| {}, None)
        .unwrap();

    // Verify tracks now have last_scanned set
    let tracks_after = lib.all_tracks().unwrap();
    assert!(tracks_after.iter().all(|t| t.last_scanned.is_some()));
}

#[test]
fn rescan_track_updates_metadata() {
    gstreamer::init().ok();

    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 2);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path).unwrap();

    // Get first track path
    let tracks = lib.all_tracks().unwrap();
    assert!(!tracks.is_empty());
    let track_path = &tracks[0].path;

    // Verify no last_scanned initially
    assert!(tracks[0].last_scanned.is_none());

    // Rescan the track
    lib.rescan_track(track_path).unwrap();

    // Verify last_scanned is now set
    let tracks_after = lib.all_tracks().unwrap();
    let rescanned = tracks_after.iter().find(|t| t.path == *track_path).unwrap();
    assert!(rescanned.last_scanned.is_some());
}

// ── Smart scan helpers ─────────────────────────────────────────────────
// (parse/format timestamp tests live in `crate::timeutil`.)

#[test]
fn needs_metadata_scan_never_scanned() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test.mp3");
    fs::write(&file_path, b"fake").unwrap();
    let path = file_path.to_str().unwrap();

    // Never scanned - should need scan
    assert!(MediaLibrary::needs_metadata_scan(path, None));
}

#[test]
fn needs_metadata_scan_file_missing() {
    // File doesn't exist - should need scan
    assert!(MediaLibrary::needs_metadata_scan(
        "/nonexistent/file.mp3",
        Some("2024-01-15T10:30:00Z")
    ));
}

#[test]
fn needs_metadata_scan_file_changed_after_scan() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test.mp3");
    fs::write(&file_path, b"fake").unwrap();

    // Wait a moment so mtime is definitely after old timestamp
    std::thread::sleep(std::time::Duration::from_millis(10));

    let path = file_path.to_str().unwrap();
    let old_timestamp = "2020-01-01T00:00:00Z";

    // File was modified after scan - should need scan
    assert!(MediaLibrary::needs_metadata_scan(path, Some(old_timestamp)));
}

#[test]
fn needs_metadata_scan_file_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test.mp3");
    fs::write(&file_path, b"fake").unwrap();

    let path = file_path.to_str().unwrap();

    // Get current mtime as a string (this is what we'd store after scanning)
    let current_ts = crate::timeutil::format_current_timestamp();

    // File hasn't changed since scan - should NOT need scan
    assert!(!MediaLibrary::needs_metadata_scan(path, Some(&current_ts)));
}

// ── scan_folder ─────────────────────────────────────────────────────────

#[test]
fn scan_folder_scans_never_scanned() {
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path).unwrap(); // Add tracks

    // Verify tracks have no last_scanned yet
    let tracks_before = lib.all_tracks().unwrap();
    assert!(tracks_before.iter().all(|t| t.last_scanned.is_none()));

    // Scan folder
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut progress_calls = Vec::new();
    let (scanned, skipped, _) = lib
        .scan_folder(folder_id, &cancel, |curr, total| {
            progress_calls.push((curr, total));
        })
        .unwrap();

    assert_eq!(scanned, 3);
    assert_eq!(skipped, 0);
    assert!(!progress_calls.is_empty());

    // Verify tracks now have last_scanned set
    let tracks_after = lib.all_tracks().unwrap();
    assert!(tracks_after.iter().all(|t| t.last_scanned.is_some()));
}

#[test]
fn scan_folder_skips_unchanged_files() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 2);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path).unwrap();

    // Scan once
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (scanned1, _, _) = lib.scan_folder(folder_id, &cancel, |_, _| {}).unwrap();
    assert_eq!(scanned1, 2);

    // Scan again - should skip all (nothing changed)
    let cancel2 = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (scanned2, skipped2, _) = lib.scan_folder(folder_id, &cancel2, |_, _| {}).unwrap();
    assert_eq!(scanned2, 0);
    assert_eq!(skipped2, 2);
}

#[test]
fn scan_folder_rescans_changed_files() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 2);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path).unwrap();

    // Scan once
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    lib.scan_folder(folder_id, &cancel, |_, _| {}).unwrap();

    // Wait and modify one file (3 seconds to ensure mtime differs after 2-second buffer)
    std::thread::sleep(std::time::Duration::from_secs(3));
    let files: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
    fs::write(files[0].as_ref().unwrap().path(), b"modified data").unwrap();

    // Scan again - should rescan the modified file
    let cancel2 = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (scanned, skipped, _) = lib.scan_folder(folder_id, &cancel2, |_, _| {}).unwrap();
    assert_eq!(scanned, 1); // Only the modified file
    assert_eq!(skipped, 1); // The unchanged file
}

#[test]
fn scan_folder_respects_cancel() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 5);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path).unwrap();

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);

    let result = lib.scan_folder(folder_id, &cancel, |_, _| {});
    assert!(result.is_ok()); // Should not error on cancel
}

// ── scan_all_folders ───────────────────────────────────────────────────

#[test]
fn scan_all_folders_processes_all_folders() {
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();

    let dir1 = temp_dir_with_files("mp3", 2);
    let dir2 = temp_dir_with_files("flac", 3);

    let folder_id1 = lib.add_folder(dir1.path().to_str().unwrap()).unwrap().id();
    let folder_id2 = lib.add_folder(dir2.path().to_str().unwrap()).unwrap().id();

    lib.rescan_folder_fast(folder_id1, dir1.path().to_str().unwrap())
        .unwrap();
    lib.rescan_folder_fast(folder_id2, dir2.path().to_str().unwrap())
        .unwrap();

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (scanned, skipped, _) = lib.scan_all_folders(&cancel, |_, _| {}).unwrap();

    assert_eq!(scanned, 5); // 2 + 3
    assert_eq!(skipped, 0);
}

#[test]
fn scan_all_folders_cumulative_progress() {
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();

    let dir1 = temp_dir_with_files("mp3", 2);
    let dir2 = temp_dir_with_files("flac", 3);

    let folder_id1 = lib.add_folder(dir1.path().to_str().unwrap()).unwrap().id();
    let folder_id2 = lib.add_folder(dir2.path().to_str().unwrap()).unwrap().id();

    lib.rescan_folder_fast(folder_id1, dir1.path().to_str().unwrap())
        .unwrap();
    lib.rescan_folder_fast(folder_id2, dir2.path().to_str().unwrap())
        .unwrap();

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut last_total = 0usize;
    let result = lib
        .scan_all_folders(&cancel, |current, total| {
            // Total should be consistent (all files to scan)
            assert_eq!(total, 5);
            // Current should increase monotonically
            assert!(current >= last_total);
            last_total = current;
        })
        .unwrap();

    assert_eq!(result.0, 5); // All scanned
}

#[test]
fn scan_all_folders_empty_library() {
    let (lib, _db) = temp_lib();

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (scanned, skipped, _) = lib.scan_all_folders(&cancel, |_, _| {}).unwrap();

    assert_eq!(scanned, 0);
    assert_eq!(skipped, 0);
}

// ── remove_track ──────────────────────────────────────────────────────

#[test]
fn remove_track_deletes_from_db() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 2);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path).unwrap();

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
    lib.rescan_folder_fast(folder_id, path).unwrap();

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
    lib.rescan_folder_fast(folder_id, path).unwrap();

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
    lib.rescan_folder_fast(folder_id, path).unwrap();

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
    lib.rescan_folder_fast(folder_id, path).unwrap();

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
    lib.rescan_folder_fast(folder_id, path).unwrap();

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
    lib.rescan_folder_fast(folder_id, path).unwrap();

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

// ── SortKeys pre-computation ───────────────────────────────────────────

#[test]
fn sort_keys_are_precomputed_from_libtrack() {
    let track = LibTrack {
        id: 1,
        path: "/music/Test Song.mp3".into(),
        artist: Some("The ARTIST".into()),
        title: Some("My TITLE".into()),
        album: Some("The ALBUM".into()),
        track_num: Some(7),
        genre: Some("Rock".into()),
        year: Some(2024),
        bpm: None,
        length_secs: Some(180.5),
        bitrate: Some(320),
        channels: None,
        filetype: Some("mp3".into()),
        filename: "Test Song.mp3".into(),
        play_count: 0,
        last_played: None,
        comment: Some("Great track!".into()),
        album_artist: Some("Various Artists".into()),
        disc_num: None,
        disc_total: None,
        composer: None,
        original_artist: None,
        copyright: None,
        url: None,
        encoded_by: None,
        lyric: None,
        artwork_path: None,
        last_scanned: None,
        sort_keys: SortKeys::default(),
    };
    let keys = SortKeys::from_track(&track);

    assert_eq!(keys.num, "0000000007");
    assert_eq!(keys.title, "my title");
    assert_eq!(keys.artist, "the artist");
    assert_eq!(keys.album, "the album");
    assert_eq!(keys.duration, "00000000180.500");
    assert_eq!(keys.filename, "test song.mp3");
    assert_eq!(keys.year, "0000002024");
    assert_eq!(keys.genre, "rock");
    assert_eq!(keys.bitrate, "0000000320");
    assert_eq!(keys.album_artist, "various artists");
    assert_eq!(keys.composer, "");
    assert_eq!(keys.comment, "great track!");
}

#[test]
fn sort_keys_fallback_to_filename_for_title() {
    let track = LibTrack {
        id: 1,
        path: "/music/No Title.mp3".into(),
        artist: None,
        title: None,
        album: None,
        track_num: None,
        genre: None,
        year: None,
        bpm: None,
        length_secs: None,
        bitrate: None,
        channels: None,
        filetype: None,
        filename: "No Title.mp3".into(),
        play_count: 0,
        last_played: None,
        comment: None,
        album_artist: None,
        disc_num: None,
        disc_total: None,
        composer: None,
        original_artist: None,
        copyright: None,
        url: None,
        encoded_by: None,
        lyric: None,
        artwork_path: None,
        last_scanned: None,
        sort_keys: SortKeys::default(),
    };
    let keys = SortKeys::from_track(&track);

    assert_eq!(keys.title, "no title.mp3");
}

// ── record_play ────────────────────────────────────────────────────────

#[test]
fn record_play_increments_play_count() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("song.mp3");
    let path = file_path.to_str().unwrap();
    fs::write(&file_path, b"fake").unwrap();

    let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();
    lib.rescan_folder_fast(folder_id, dir.path().to_str().unwrap())
        .unwrap();

    // play_count starts at 0.
    let track = lib.track_by_path(path).unwrap();
    assert_eq!(track.play_count, 0);

    lib.record_play(path).unwrap();

    let track = lib.track_by_path(path).unwrap();
    assert_eq!(track.play_count, 1);
    assert!(track.last_played.is_some());
}

#[test]
fn record_play_accumulates_multiple_calls() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("song.mp3");
    let path = file_path.to_str().unwrap();
    fs::write(&file_path, b"fake").unwrap();

    let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();
    lib.rescan_folder_fast(folder_id, dir.path().to_str().unwrap())
        .unwrap();

    for i in 1..=5 {
        lib.record_play(path).unwrap();
        let track = lib.track_by_path(path).unwrap();
        assert_eq!(track.play_count, i);
    }
}

#[test]
fn record_play_updates_last_played_timestamp() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("song.mp3");
    let path = file_path.to_str().unwrap();
    fs::write(&file_path, b"fake").unwrap();

    let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();
    lib.rescan_folder_fast(folder_id, dir.path().to_str().unwrap())
        .unwrap();

    lib.record_play(path).unwrap();
    let first = lib.track_by_path(path).unwrap().last_played.clone();
    assert!(first.is_some(), "first play should set last_played");

    // Wait 1.1 seconds so the second play gets a different timestamp
    // (timestamps are stored as seconds, not milliseconds).
    std::thread::sleep(std::time::Duration::from_millis(1100));
    lib.record_play(path).unwrap();
    let second = lib.track_by_path(path).unwrap().last_played;

    assert!(second.is_some(), "second play should update last_played");
    assert_ne!(first, second, "second play should have a newer timestamp");
}

#[test]
fn record_play_noop_for_unknown_path() {
    let (lib, _db) = temp_lib();
    // No track added — record_play should succeed without error.
    let result = lib.record_play("/nonexistent/path.mp3");
    assert!(result.is_ok());
}

// -----------------------------------------------------------------------
// read_only_track_fields
// -----------------------------------------------------------------------

#[test]
fn read_only_track_fields_all_values_formatted() {
    let track = LibTrack {
        id: 1,
        path: "/music/song.mp3".into(),
        artist: Some("The Artist".into()),
        title: Some("My Song".into()),
        album: Some("The Album".into()),
        track_num: Some(5),
        genre: Some("Rock".into()),
        year: Some(2020),
        bpm: Some("120".into()),
        length_secs: Some(185.0),
        bitrate: Some(320),
        channels: Some(2),
        filetype: Some("MP3".into()),
        filename: "song.mp3".into(),
        play_count: 42,
        last_played: Some("2024-01-15T10:30:00Z".into()),
        comment: None,
        album_artist: None,
        disc_num: None,
        disc_total: None,
        composer: None,
        original_artist: None,
        copyright: None,
        url: None,
        encoded_by: None,
        lyric: None,
        artwork_path: Some("/music/cover.jpg".into()),
        last_scanned: None,
        sort_keys: SortKeys::default(),
    };
    let path = std::path::Path::new("/music/song.mp3");
    let ro = read_only_track_fields(path, Some(&track));

    assert_eq!(ro.filename, "song.mp3");
    assert_eq!(ro.path, "/music/song.mp3");
    assert_eq!(ro.filetype, "MP3");
    assert_eq!(ro.bitrate, "320k");
    assert_eq!(ro.channels, "stereo");
    assert_eq!(ro.duration, "3:05");
    assert_eq!(ro.play_count, "42");
    assert_eq!(ro.last_played, "2024-01-15T10:30:00Z");
    assert_eq!(ro.num, "5");
    assert_eq!(ro.artwork_path, "/music/cover.jpg");
}

#[test]
fn read_only_track_fields_fallback_when_no_track() {
    let path = std::path::Path::new("/unknown/file.mp3");
    let ro = read_only_track_fields(path, None);

    assert_eq!(ro.filename, "file.mp3");
    assert_eq!(ro.path, "/unknown/file.mp3");
    assert_eq!(ro.filetype, "");
    assert_eq!(ro.bitrate, "");
    assert_eq!(ro.channels, "");
    assert_eq!(ro.duration, "-:--");
    assert_eq!(ro.play_count, "");
    assert_eq!(ro.last_played, "");
    assert_eq!(ro.num, "");
    assert_eq!(ro.artwork_path, "");
}

#[test]
fn read_only_track_fields_channels_mono() {
    let track = LibTrack {
        id: 0,
        path: String::new(),
        artist: None,
        title: None,
        album: None,
        track_num: None,
        genre: None,
        year: None,
        bpm: None,
        length_secs: None,
        bitrate: None,
        channels: Some(1),
        filetype: None,
        filename: String::new(),
        play_count: 0,
        last_played: None,
        comment: None,
        album_artist: None,
        disc_num: None,
        disc_total: None,
        composer: None,
        original_artist: None,
        copyright: None,
        url: None,
        encoded_by: None,
        lyric: None,
        artwork_path: None,
        last_scanned: None,
        sort_keys: SortKeys::default(),
    };
    let path = std::path::Path::new("/test.mp3");
    let ro = read_only_track_fields(path, Some(&track));
    assert_eq!(ro.channels, "mono");
}

#[test]
fn read_only_track_fields_channels_multi() {
    let track = LibTrack {
        id: 0,
        path: String::new(),
        artist: None,
        title: None,
        album: None,
        track_num: None,
        genre: None,
        year: None,
        bpm: None,
        length_secs: None,
        bitrate: None,
        channels: Some(6),
        filetype: None,
        filename: String::new(),
        play_count: 0,
        last_played: None,
        comment: None,
        album_artist: None,
        disc_num: None,
        disc_total: None,
        composer: None,
        original_artist: None,
        copyright: None,
        url: None,
        encoded_by: None,
        lyric: None,
        artwork_path: None,
        last_scanned: None,
        sort_keys: SortKeys::default(),
    };
    let path = std::path::Path::new("/test.mp3");
    let ro = read_only_track_fields(path, Some(&track));
    assert_eq!(ro.channels, "6ch");
}


// ── load_playlist_tracks path resolution ──────────────────────────────

#[test]
fn load_playlist_prefers_accessible_path_over_stale_catalogue_row() {
    // A playlist line pointing at a file that exists must round-trip as a
    // playable track on that accessible path — even when the catalogue only
    // knows a same-named file under a now-inaccessible (stale) path. This
    // guards against the filename fallback substituting the dead path and
    // making an accessible track appear missing.
    let (lib, _db) = temp_lib();

    // Catalogue a "song.mp3" then delete it on disk, leaving a stale row
    // whose recorded path no longer exists.
    let stale_dir = tempfile::tempdir().unwrap();
    fs::write(stale_dir.path().join("song.mp3"), b"x").unwrap();
    let fid = lib.add_folder(stale_dir.path().to_str().unwrap()).unwrap().id();
    lib.rescan_folder_fast(fid, stale_dir.path().to_str().unwrap()).unwrap();
    fs::remove_file(stale_dir.path().join("song.mp3")).unwrap();

    // A different, accessible "song.mp3" referenced by the playlist file.
    let live_dir = tempfile::tempdir().unwrap();
    let live_path = live_dir.path().join("song.mp3");
    fs::write(&live_path, b"x").unwrap();

    let m3u_path = live_dir.path().join("list.m3u8");
    fs::write(&m3u_path, format!("#EXTM3U\n{}\n", live_path.display())).unwrap();

    let pl = LibPlaylist {
        id: 0,
        path: m3u_path.to_string_lossy().into_owned(),
        name: "list".into(),
        tracks: Vec::new(),
    };
    let tracks = lib.load_playlist_tracks(&pl).unwrap();
    assert_eq!(tracks.len(), 1);
    let canon = live_path.canonicalize().unwrap();
    assert_eq!(tracks[0].path, canon.to_string_lossy());
    assert!(std::path::Path::new(&tracks[0].path).exists());
}

#[test]
fn load_playlist_marks_genuinely_missing_entry_as_stub() {
    // A playlist line whose file does not exist anywhere stays a stub on the
    // raw path so the UI can show it in the unavailable color.
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let m3u_path = dir.path().join("list.m3u8");
    fs::write(&m3u_path, "#EXTM3U\n/no/such/file/ghost.mp3\n").unwrap();

    let pl = LibPlaylist {
        id: 0,
        path: m3u_path.to_string_lossy().into_owned(),
        name: "list".into(),
        tracks: Vec::new(),
    };
    let tracks = lib.load_playlist_tracks(&pl).unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].id, 0);
    assert!(!std::path::Path::new(&tracks[0].path).exists());
}

// ── device schema ──────────────────────────────────────────────────────

fn table_exists(lib: &MediaLibrary, name: &str) -> bool {
    lib.conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |_| Ok(()),
        )
        .is_ok()
}

fn column_exists(lib: &MediaLibrary, table: &str, col: &str) -> bool {
    let mut stmt = lib
        .conn
        .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    cols.iter().any(|c| c == col)
}

#[test]
fn schema_has_device_tables_and_rating_column() {
    let (lib, _db) = temp_lib();
    assert!(table_exists(&lib, "devices"));
    assert!(table_exists(&lib, "device_sync_pairs"));
    assert!(column_exists(&lib, "tracks", "rating"));
}

#[test]
fn device_upsert_and_get_roundtrip() {
    let (lib, _db) = temp_lib();
    let dev = crate::media_library::DeviceRecord {
        id: "UUID-1234".into(),
        label: "MY STICK".into(),
        last_seen: Some("2026-06-13T00:00:00Z".into()),
        smart_rules: None,
    };
    lib.upsert_device(&dev).unwrap();
    assert_eq!(lib.get_device("UUID-1234").unwrap(), Some(dev.clone()));

    // Upsert updates rather than duplicating.
    let dev2 = crate::media_library::DeviceRecord { label: "RENAMED".into(), ..dev };
    lib.upsert_device(&dev2).unwrap();
    assert_eq!(lib.get_device("UUID-1234").unwrap().unwrap().label, "RENAMED");

    assert_eq!(lib.get_device("nope").unwrap(), None);
}

#[test]
fn sync_pair_crud_and_lookups() {
    let (lib, _db) = temp_lib();
    let pair = crate::media_library::SyncPair {
        device_id: "UUID-1234".into(),
        device_relpath: "Music/A/B/song.mp3".into(),
        library_path: "/home/u/Music/song.mp3".into(),
        baseline_tag_hash: "abc".into(),
        baseline_rating: 4,
        baseline_playcount: 7,
        last_sync_at: None,
    };
    lib.upsert_sync_pair(&pair).unwrap();

    assert_eq!(lib.sync_pairs_for_device("UUID-1234").unwrap(), vec![pair.clone()]);
    assert_eq!(
        lib.sync_pairs_for_library_path("/home/u/Music/song.mp3").unwrap(),
        vec![pair.clone()]
    );

    // Upsert on the same key replaces (baseline refresh after a sync).
    let refreshed = crate::media_library::SyncPair {
        baseline_tag_hash: "def".into(),
        baseline_playcount: 8,
        ..pair.clone()
    };
    lib.upsert_sync_pair(&refreshed).unwrap();
    let got = lib.sync_pairs_for_device("UUID-1234").unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].baseline_tag_hash, "def");
    assert_eq!(got[0].baseline_playcount, 8);

    lib.delete_sync_pair("UUID-1234", "Music/A/B/song.mp3").unwrap();
    assert!(lib.sync_pairs_for_device("UUID-1234").unwrap().is_empty());
}

#[test]
fn playlist_baseline_crud() {
    let (lib, _db) = temp_lib();
    let base = crate::media_library::PlaylistBaseline {
        device_id: "UUID-1234".into(),
        library_playlist_id: 42,
        device_filename: "Roadtrip.m3u8".into(),
        entries_hash: "h1".into(),
        last_sync_at: None,
    };
    lib.upsert_playlist_baseline(&base).unwrap();
    assert_eq!(
        lib.playlist_baselines_for_device("UUID-1234").unwrap(),
        vec![base.clone()]
    );

    // Upsert on (device_id, playlist_id) replaces (rename + content change).
    let refreshed = crate::media_library::PlaylistBaseline {
        device_filename: "Road Trip.m3u8".into(),
        entries_hash: "h2".into(),
        ..base.clone()
    };
    lib.upsert_playlist_baseline(&refreshed).unwrap();
    let got = lib.playlist_baselines_for_device("UUID-1234").unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].device_filename, "Road Trip.m3u8");
    assert_eq!(got[0].entries_hash, "h2");

    lib.delete_playlist_baseline("UUID-1234", 42).unwrap();
    assert!(lib.playlist_baselines_for_device("UUID-1234").unwrap().is_empty());
}
