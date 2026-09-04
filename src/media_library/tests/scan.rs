//! Scanning: upserts, the fast and metadata rescans, the production scan flow,
//! and the filesystem-watch seam that drives them.

use super::*;

// ── upsert_track: technical columns + added_at stability ───────────────


/// Like `temp_dir_with_files`, but writes real (parseable) minimal WAV
/// fixtures instead of garbage bytes, so `technical_probe` gets a real
/// `sample_rate` on the first scan. Needed for tests that scan a folder
/// twice and assert the second pass skips on mtime alone: with garbage
/// bytes `sample_rate` never resolves, and the scan_folder backfill net
/// (re-read while `sample_rate IS NULL`) would keep re-scanning every
/// pass — exactly the mtime-only behavior these tests are not testing.
fn temp_dir_with_wav_files(count: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..count {
        let file_path = dir.path().join(format!("track_{}.wav", i));
        write_test_wav(&file_path, 44100, 2, 0.1);
    }
    dir
}

// ── upsert_track: ReplayGain harvested from existing file tags ─────────

/// Write a minimal MP3-shaped file carrying REPLAYGAIN_* TXXX frames.
fn write_mp3_with_rg(path: &std::path::Path, track_gain: &str, track_peak: &str) {
    use id3::TagLike;
    fs::write(path, [0xFFu8, 0xFB, 0x90, 0x00]).unwrap();
    let mut tag = id3::Tag::new();
    tag.set_title("T");
    for (desc, value) in [
        ("REPLAYGAIN_TRACK_GAIN", track_gain),
        ("REPLAYGAIN_TRACK_PEAK", track_peak),
    ] {
        tag.add_frame(id3::frame::ExtendedText {
            description: desc.to_string(),
            value: value.to_string(),
        });
    }
    tag.write_to_path(path, id3::Version::Id3v23).unwrap();
}

#[test]
fn scan_harvests_replaygain_tags_already_in_the_file() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("pretagged.mp3");
    write_mp3_with_rg(&file_path, "-11.00 dB", "0.988123");
    let path = file_path.to_str().unwrap();
    let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();

    lib.upsert_track(folder_id, path).unwrap();

    let track = lib.track_by_path(path).unwrap();
    // A file that arrived pre-normalized should need no `rganalysis` pass.
    assert_eq!(track.rg_track_gain, Some(-11.0));
    assert_eq!(track.rg_track_peak, Some(0.988123));
    assert!(
        !crate::replaygain::needs_analysis(&track),
        "harvested gain must satisfy needs_analysis so we don't re-measure it"
    );
}

#[test]
fn rescan_of_untagged_file_keeps_a_gain_sparkamp_measured() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("untagged.wav");
    write_test_wav(&file_path, 44100, 2, 1.0);
    let path = file_path.to_str().unwrap();
    let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();
    lib.upsert_track(folder_id, path).unwrap();

    // Analysis result stored in the DB only (write-tags off, and this is a
    // WAV, which Sparkamp cannot tag at all).
    let id = lib.track_by_path(path).unwrap().id;
    lib.set_replaygain(id, -6.20, 0.988123, -7.10, 0.995).unwrap();

    // A later rescan reads no ReplayGain from the file — it must not wipe it.
    lib.upsert_track(folder_id, path).unwrap();

    let track = lib.track_by_path(path).unwrap();
    assert_eq!(track.rg_track_gain, Some(-6.20));
    assert_eq!(track.rg_album_gain, Some(-7.10));
}

#[test]
fn upsert_captures_technical_columns_and_preserves_added_at() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("song.wav");
    write_test_wav(&file_path, 44100, 2, 1.0);
    let path = file_path.to_str().unwrap();
    let expected_size = fs::metadata(&file_path).unwrap().len() as i64;

    let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();

    lib.upsert_track(folder_id, path).unwrap();
    let track = lib.track_by_path(path).unwrap();

    assert_eq!(track.sample_rate, Some(44100));
    assert_eq!(track.channels, Some(2));
    assert_eq!(track.file_size, Some(expected_size));
    assert!(track.file_mtime.is_some(), "file_mtime should be populated");
    assert!(track.added_at.is_some(), "added_at should be populated");
    assert!(
        track.length_secs.is_some(),
        "duration must be known for this fixture (header-derived)"
    );
    assert!(
        track.bitrate.is_some(),
        "bitrate must be non-NULL when duration is known"
    );

    let added_at_first = track.added_at.clone();
    let file_mtime_first = track.file_mtime.clone();

    // Re-upsert the same path (as a rescan would). added_at must be stable
    // (INSERT-only), while last_scanned/file_mtime refresh.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    lib.upsert_track(folder_id, path).unwrap();
    let track2 = lib.track_by_path(path).unwrap();

    assert_eq!(
        track2.added_at, added_at_first,
        "added_at must not change on re-upsert"
    );
    assert!(
        track2.last_scanned.is_some(),
        "last_scanned should be refreshed by upsert_track"
    );
    assert!(track2.file_mtime.is_some());
    assert_eq!(
        track2.file_mtime, file_mtime_first,
        "file_mtime unchanged since the file itself was not modified"
    );
}

// ── bitrate: computed value with tag fallback ───────────────────────────
// Mirrors `channels`, which already does `tech.channels.or(tags.channels)`.
// `resolve_bitrate` isolates the same combination so it's testable without
// a real tag reader that can produce Some(bitrate) (none currently do).

#[test]
fn resolve_bitrate_prefers_computed_value_over_tag_value() {
    assert_eq!(MediaLibrary::resolve_bitrate(Some(128), Some(320)), Some(128));
}

#[test]
fn resolve_bitrate_falls_back_to_tag_value_when_computed_is_none() {
    assert_eq!(MediaLibrary::resolve_bitrate(None, Some(320)), Some(320));
}

#[test]
fn resolve_bitrate_is_none_when_neither_source_has_a_value() {
    assert_eq!(MediaLibrary::resolve_bitrate(None, None), None);
}

// ── rescan_folder_fast ────────────────────────────────────────────────

#[test]
fn rescan_folder_fast_inserts_audio_files() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    let (added, _) = lib.rescan_folder_fast(folder_id, path, true).unwrap();

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
    let (added, _) = lib.rescan_folder_fast(folder_id, path, true).unwrap();

    assert_eq!(added, 4);
}

#[test]
fn rescan_folder_fast_skips_nonexistent_paths() {
    let (lib, _db) = temp_lib();
    let folder_id = lib.add_folder("/nonexistent/path/xyz").unwrap().id();
    let result = lib.rescan_folder_fast(folder_id, "/nonexistent/path/xyz", true);
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
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    assert_eq!(lib.all_tracks().unwrap().len(), 3);

    // Delete one file and rescan.
    fs::remove_file(dir.path().join("b.mp3")).unwrap();
    let (_, removed) = lib.rescan_folder_fast(folder_id, path, true).unwrap();

    assert_eq!(removed, 1);
    assert_eq!(lib.all_tracks().unwrap().len(), 2);
}

// ── remove_missing gating (Phase 8 Task 6) ──────────────────────────────
// USER-DECIDED 2026-07-27: remove_missing=false is the new production
// default and KEEPS rows for files that vanished from disk (Winamp
// offline-media parity); remove_missing=true reproduces today's
// unconditional hard-delete. Both rescan_folder and rescan_folder_fast
// share the identical gated loop; rescan_folder_fast is exercised here
// since the rest of this test module already does.

#[test]
fn rescan_remove_missing_off_keeps_row() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 1);
    let path = dir.path().to_str().unwrap();
    let file_path = dir.path().join("track_0.mp3");

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    assert_eq!(lib.all_tracks().unwrap().len(), 1);

    fs::remove_file(&file_path).unwrap();
    let (_, removed) = lib.rescan_folder_fast(folder_id, path, false).unwrap();

    assert_eq!(removed, 0, "removed count must be 0 when remove_missing is off");
    assert_eq!(
        lib.all_tracks().unwrap().len(),
        1,
        "row for the missing file must be kept (offline-media parity)"
    );
}

#[test]
fn rescan_remove_missing_on_deletes_row() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 1);
    let path = dir.path().to_str().unwrap();
    let file_path = dir.path().join("track_0.mp3");

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    assert_eq!(lib.all_tracks().unwrap().len(), 1);

    fs::remove_file(&file_path).unwrap();
    let (_, removed) = lib.rescan_folder_fast(folder_id, path, true).unwrap();

    assert_eq!(removed, 1);
    assert_eq!(
        lib.all_tracks().unwrap().len(),
        0,
        "row for the missing file must be deleted when remove_missing is on"
    );
}

#[test]
fn compact_runs_without_error() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    lib.remove_folder(folder_id).unwrap();

    assert!(lib.compact().is_ok());
}

#[test]
fn rescan_folder_fast_upserts_m3u_playlists() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("My Playlist.m3u"), b"#EXTM3U\n").unwrap();
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let playlists = lib.all_playlists().unwrap();
    assert_eq!(playlists.len(), 1);
    assert_eq!(playlists[0].name, "My Playlist");
}

// ── production scan flow: rescan_folder_fast + scan_all_folders ────────
// GTK/mac "Rescan" runs the fast path-only insert first, then the mtime
// smart-skip pass (scan_all_folders → scan_folder → needs_metadata_scan).
// rescan_folder_metadata is test-only — it is never called by either
// frontend — so correctness has to be proven through this real pipeline,
// not through rescan_folder_metadata directly.

#[test]
fn production_scan_flow_stamps_added_at_and_keeps_it_stable() {
    #[cfg(not(target_os = "macos"))]
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 2);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();

    // Step 1: fast path-only insert (what "Add folder" / "Rescan" do first).
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    // Step 2: the real background metadata pass (mtime smart-skip).
    let cancel = std::sync::atomic::AtomicBool::new(false);
    lib.scan_all_folders(&cancel, |_, _| {}).unwrap();

    let tracks_first = lib.all_tracks().unwrap();
    assert_eq!(tracks_first.len(), 2);
    assert!(
        tracks_first.iter().all(|t| t.added_at.is_some()),
        "added_at must be populated by the production fast-insert + scan flow"
    );
    let added_at_first: std::collections::HashMap<String, Option<String>> = tracks_first
        .iter()
        .map(|t| (t.path.clone(), t.added_at.clone()))
        .collect();

    // A second scan pass (nothing changed on disk) must not disturb the
    // already-set added_at — it is first-sighting-only, never overwritten.
    std::thread::sleep(std::time::Duration::from_millis(10));
    let cancel2 = std::sync::atomic::AtomicBool::new(false);
    lib.scan_all_folders(&cancel2, |_, _| {}).unwrap();

    let tracks_second = lib.all_tracks().unwrap();
    for t in &tracks_second {
        assert_eq!(
            t.added_at,
            added_at_first[&t.path],
            "added_at must stay stable across a second scan"
        );
    }
}

#[test]
fn scan_all_folders_backfills_null_sample_rate_for_previously_scanned_row() {
    #[cfg(not(target_os = "macos"))]
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("song.wav");
    write_test_wav(&file_path, 44100, 2, 1.0);
    let path = file_path.to_str().unwrap();
    let folder_path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(folder_path).unwrap().id();
    lib.rescan_folder_fast(folder_id, folder_path, true).unwrap();

    // Simulate a row scanned before this phase shipped: last_scanned is set
    // (mtime-skip would normally leave it alone) but sample_rate — a column
    // this phase added — was never backfilled onto it.
    lib.conn
        .execute(
            "UPDATE tracks SET last_scanned = ?1, sample_rate = NULL WHERE path = ?2",
            rusqlite::params![crate::timeutil::format_current_timestamp(), path],
        )
        .unwrap();

    // The production rescan entry point (what the GTK/mac "Rescan" button
    // calls) must still pick this row up despite the unchanged mtime.
    let cancel = std::sync::atomic::AtomicBool::new(false);
    lib.scan_all_folders(&cancel, |_, _| {}).unwrap();

    let track = lib.track_by_path(path).unwrap();
    assert!(
        track.sample_rate.is_some(),
        "pre-phase row with NULL sample_rate must be backfilled by scan_all_folders \
         despite an unchanged file mtime"
    );
    assert!(track.file_size.is_some());
}

// ── rescan_folder_metadata ─────────────────────────────────────────────

#[test]
fn rescan_folder_metadata_reports_progress() {
    #[cfg(not(target_os = "macos"))]
    gstreamer::init().ok();

    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 5);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

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
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);

    // Even with cancel set, it should return Ok (not an error).
    let result = lib.rescan_folder_metadata(folder_id, &cancel, |_, _| {}, None);
    assert!(result.is_ok());
}

#[test]
fn rescan_folder_metadata_sets_last_scanned() {
    #[cfg(not(target_os = "macos"))]
    gstreamer::init().ok();

    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

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
    #[cfg(not(target_os = "macos"))]
    gstreamer::init().ok();

    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 2);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

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

#[test]
fn rescan_track_refreshes_null_folder_row() {
    #[cfg(not(target_os = "macos"))]
    gstreamer::init().ok();

    // No folders registered at all — mirrors
    // add_played_outside_library_creates_null_folder_row: a played file
    // outside every watched folder lands in the NULL-folder_id bucket.
    // Editing its tags and rescanning must not error out just because
    // there's no owning folder to look up (Phase 8 review Fix 1).
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().canonicalize().unwrap().join("track.mp3");
    fs::write(&file_path, b"fake audio data").unwrap();
    let path = file_path.to_str().unwrap();

    lib.add_played_track(path).unwrap();
    assert!(track_row_exists(&lib, path));

    let result = lib.rescan_track(path);

    assert!(
        result.is_ok(),
        "rescan_track on a NULL-folder row must not error: {result:?}"
    );
    assert!(
        track_row_exists(&lib, path),
        "the NULL-bucket row must still exist after rescan"
    );
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
    assert!(MediaLibrary::needs_metadata_scan(path, None, None));
}

#[test]
fn needs_metadata_scan_file_missing() {
    // File doesn't exist - should need scan
    assert!(MediaLibrary::needs_metadata_scan(
        "/nonexistent/file.mp3",
        Some("2024-01-15T10:30:00Z"),
        None
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
    assert!(MediaLibrary::needs_metadata_scan(path, Some(old_timestamp), None));
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
    assert!(!MediaLibrary::needs_metadata_scan(path, Some(&current_ts), None));
}

/// The bug the `file_mtime` comparison exists to fix: an mtime that moves
/// BACKWARDS. Restoring from a backup, `rsync -t`, unzipping with preserved
/// timestamps, or a tag editor that puts mtime back all leave a file that
/// genuinely changed but now looks older than the scan that read it. The old
/// `mtime > last_scanned + 2` rule returned false here, forever.
#[test]
fn needs_metadata_scan_catches_mtime_moving_backwards() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test.mp3");
    fs::write(&file_path, b"fake").unwrap();
    let path = file_path.to_str().unwrap();

    // The file's mtime right now, and a scan that ran well after it.
    let actual_mtime = crate::timeutil::format_system_time(
        fs::metadata(&file_path).unwrap().modified().unwrap(),
    );
    let scanned_later = "2099-01-01T00:00:00Z";

    // Recorded mtime matches the file: unchanged, even though last_scanned is
    // in the future relative to it.
    assert!(!MediaLibrary::needs_metadata_scan(
        path,
        Some(scanned_later),
        Some(&actual_mtime)
    ));

    // Now the row remembers a DIFFERENT (newer) mtime than the file has —
    // exactly the backwards case. It must rescan.
    assert!(
        MediaLibrary::needs_metadata_scan(path, Some(scanned_later), Some("2098-06-01T00:00:00Z")),
        "an mtime older than the recorded one still means the file changed"
    );
}

/// The other half: the old rule's 2-second buffer meant a file touched within
/// 2 s of the scan that read it was never rescanned. An exact comparison has
/// no window to fall through.
#[test]
fn needs_metadata_scan_has_no_two_second_blind_spot() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test.mp3");
    fs::write(&file_path, b"fake").unwrap();
    let path = file_path.to_str().unwrap();

    let actual_mtime = crate::timeutil::format_system_time(
        fs::metadata(&file_path).unwrap().modified().unwrap(),
    );
    // A scan stamped one second after the file's mtime — inside the old
    // buffer, so the legacy rule skipped it.
    let scanned = crate::timeutil::parse_iso_timestamp(&actual_mtime).unwrap() + 1;
    let scanned_ts = crate::timeutil::format_system_time(
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(scanned),
    );

    // Recorded mtime disagrees with the file -> rescan, buffer or not.
    assert!(MediaLibrary::needs_metadata_scan(
        path,
        Some(&scanned_ts),
        Some("2000-01-01T00:00:00Z")
    ));
}

/// A row from before `file_mtime` existed passes `None` and must behave
/// exactly as it did, so upgrading does not rescan an entire library.
#[test]
fn needs_metadata_scan_legacy_rows_keep_the_old_rule() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test.mp3");
    fs::write(&file_path, b"fake").unwrap();
    let path = file_path.to_str().unwrap();

    let current_ts = crate::timeutil::format_current_timestamp();
    assert!(!MediaLibrary::needs_metadata_scan(path, Some(&current_ts), None));
    assert!(MediaLibrary::needs_metadata_scan(path, Some("2020-01-01T00:00:00Z"), None));
}

// ── scan_folder ─────────────────────────────────────────────────────────

#[test]
fn scan_folder_scans_never_scanned() {
    #[cfg(not(target_os = "macos"))]
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap(); // Add tracks

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
    let dir = temp_dir_with_wav_files(2);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

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
    let dir = temp_dir_with_wav_files(2);
    let path = dir.path().to_str().unwrap();

    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

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
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);

    let result = lib.scan_folder(folder_id, &cancel, |_, _| {});
    assert!(result.is_ok()); // Should not error on cancel
}

// ── scan_all_folders ───────────────────────────────────────────────────

#[test]
fn scan_all_folders_processes_all_folders() {
    #[cfg(not(target_os = "macos"))]
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();

    let dir1 = temp_dir_with_files("mp3", 2);
    let dir2 = temp_dir_with_files("flac", 3);

    let folder_id1 = lib.add_folder(dir1.path().to_str().unwrap()).unwrap().id();
    let folder_id2 = lib.add_folder(dir2.path().to_str().unwrap()).unwrap().id();

    lib.rescan_folder_fast(folder_id1, dir1.path().to_str().unwrap(), true)
        .unwrap();
    lib.rescan_folder_fast(folder_id2, dir2.path().to_str().unwrap(), true)
        .unwrap();

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (scanned, skipped, _) = lib.scan_all_folders(&cancel, |_, _| {}).unwrap();

    assert_eq!(scanned, 5); // 2 + 3
    assert_eq!(skipped, 0);
}

#[test]
fn scan_all_folders_cumulative_progress() {
    #[cfg(not(target_os = "macos"))]
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();

    let dir1 = temp_dir_with_files("mp3", 2);
    let dir2 = temp_dir_with_files("flac", 3);

    let folder_id1 = lib.add_folder(dir1.path().to_str().unwrap()).unwrap().id();
    let folder_id2 = lib.add_folder(dir2.path().to_str().unwrap()).unwrap().id();

    lib.rescan_folder_fast(folder_id1, dir1.path().to_str().unwrap(), true)
        .unwrap();
    lib.rescan_folder_fast(folder_id2, dir2.path().to_str().unwrap(), true)
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

// ── apply_watch_action: routes fs watch events through the scan seam ───


#[test]
fn apply_upsert_inserts_row() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 1);
    // Canonicalize the folder path up front and derive the file path from
    // it, so the registered folder path is a literal string prefix of the
    // watch-event path regardless of tempdir symlinking (e.g. /tmp on some
    // platforms) — the deepest-prefix folder match is a plain string
    // comparison, not a filesystem-aware one.
    let folder_path = dir.path().canonicalize().unwrap();
    lib.add_folder(folder_path.to_str().unwrap()).unwrap();
    let file_path = folder_path.join("track_0.mp3");

    let action = crate::watch::WatchAction::Upsert(file_path.clone());
    lib.apply_watch_action(&action, false).unwrap();

    // The fixture is dummy bytes (not a parseable audio file), so tags stay
    // empty — assert the row exists, not that metadata was populated.
    assert!(track_row_exists(&lib, file_path.to_str().unwrap()));
}

#[test]
fn apply_upsert_outside_folders_gets_null_folder_id() {
    let (lib, _db) = temp_lib();
    // No folder registered at all — the path lives outside every watched
    // folder, exercising the NULL-folder_id bucket apply_watch_action must
    // support (Task 7 relies on the same bucket).
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().canonicalize().unwrap().join("track.mp3");
    fs::write(&file_path, b"fake audio data").unwrap();

    let action = crate::watch::WatchAction::Upsert(file_path.clone());
    lib.apply_watch_action(&action, false).unwrap();

    let path = file_path.to_str().unwrap();
    assert!(track_row_exists(&lib, path));
    let folder_id: Option<i64> = lib
        .conn
        .query_row(
            "SELECT folder_id FROM tracks WHERE path = ?1",
            rusqlite::params![path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        folder_id, None,
        "path outside every watched folder should get a NULL folder_id"
    );
}

#[test]
fn apply_remove_keeps_row_when_flag_off() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 1);
    let folder_path = dir.path().canonicalize().unwrap();
    let folder_id = lib.add_folder(folder_path.to_str().unwrap()).unwrap().id();
    let file_path = folder_path.join("track_0.mp3");
    let path = file_path.to_str().unwrap();
    lib.upsert_track(folder_id, path).unwrap();
    assert!(track_row_exists(&lib, path));

    let action = crate::watch::WatchAction::Remove(file_path.clone());
    lib.apply_watch_action(&action, false).unwrap();

    assert!(
        track_row_exists(&lib, path),
        "row must be kept (Winamp parity: offline media persists) when remove_missing is false"
    );
}

#[test]
fn apply_remove_deletes_row_when_flag_on() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 1);
    let folder_path = dir.path().canonicalize().unwrap();
    let folder_id = lib.add_folder(folder_path.to_str().unwrap()).unwrap().id();
    let file_path = folder_path.join("track_0.mp3");
    let path = file_path.to_str().unwrap();
    lib.upsert_track(folder_id, path).unwrap();
    assert!(track_row_exists(&lib, path));

    let action = crate::watch::WatchAction::Remove(file_path.clone());
    lib.apply_watch_action(&action, true).unwrap();

    assert!(
        !track_row_exists(&lib, path),
        "row must be hard-deleted when remove_missing is true"
    );
}

// ── scan_folder: no redundant writes, and one transaction ──────────────

/// Count the UPDATEs a scan runs against `tracks`, using a temp trigger.
///
/// rusqlite 0.31 exposes no statement counter without the `hooks` feature,
/// and a trigger is cheaper than adding one. SQLite allows a TEMP trigger on
/// a table in another database, so this leaves the schema untouched.
fn count_track_updates(lib: &MediaLibrary) -> impl Fn() -> i64 + '_ {
    lib.conn
        .execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS update_tally (n INTEGER);
             DELETE FROM update_tally;
             INSERT INTO update_tally VALUES (0);
             CREATE TEMP TRIGGER IF NOT EXISTS tally_track_updates
                 AFTER UPDATE ON main.tracks
             BEGIN
                 UPDATE update_tally SET n = n + 1;
             END;",
        )
        .unwrap();
    move || {
        lib.conn
            .query_row("SELECT n FROM update_tally", [], |r| r.get::<_, i64>(0))
            .unwrap()
    }
}

/// `upsert_track` stamps `last_scanned` itself as its last act, so the scan
/// loop stamping it a second time is a pure extra write per track.
///
/// Per track the scan should run two UPDATEs: the ON CONFLICT arm of the
/// upsert (the row already exists, inserted by the fast pass), and the stamp
/// inside `upsert_track`. A third means the caller is stamping again.
#[test]
fn scan_folder_writes_each_row_once() {
    #[cfg(not(target_os = "macos"))]
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    for i in 0..2 {
        write_test_wav(&dir.path().join(format!("track_{i}.wav")), 44_100, 2, 1.0);
    }
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let updates = count_track_updates(&lib);
    let before = updates();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let (scanned, _skipped, failed) = lib.scan_folder(folder_id, &cancel, |_, _| {}).unwrap();
    let written = updates() - before;

    assert_eq!(scanned, 2, "both tracks should have been scanned");
    assert_eq!(failed, 0, "neither track should have failed");
    assert_eq!(
        written, 4,
        "two UPDATEs per track — the upsert's ON CONFLICT arm and the stamp \
         inside upsert_track. A third per track means the scan loop is \
         stamping last_scanned again after upsert_track already did"
    );
}

/// The stamp must still land. Removing the duplicate write is only correct if
/// the surviving write inside `upsert_track` actually sets `last_scanned` —
/// otherwise every scanned row keeps the "not yet scanned" clock icon.
#[test]
fn scan_folder_still_stamps_last_scanned() {
    #[cfg(not(target_os = "macos"))]
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    write_test_wav(&dir.path().join("track_0.wav"), 44_100, 2, 1.0);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let cancel = std::sync::atomic::AtomicBool::new(false);
    lib.scan_folder(folder_id, &cancel, |_, _| {}).unwrap();

    let tracks = lib.all_tracks().unwrap();
    assert_eq!(tracks.len(), 1);
    assert!(
        tracks[0].last_scanned.is_some(),
        "the surviving write must still stamp last_scanned"
    );
}

/// Work done before a cancel must survive. The loop now runs inside one
/// transaction, so a scan stopped part-way has to commit what it read rather
/// than rolling the whole folder back.
#[test]
fn scan_folder_commits_work_done_before_a_cancel() {
    #[cfg(not(target_os = "macos"))]
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    for i in 0..6 {
        write_test_wav(&dir.path().join(format!("track_{i}.wav")), 44_100, 2, 1.0);
    }
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    // Cancel after the third file: the progress callback is the only hook
    // that runs between tracks.
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let (scanned, _skipped, _failed) = lib
        .scan_folder(folder_id, &cancel, |done, _| {
            if done >= 3 {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        })
        .unwrap();

    assert_eq!(scanned, 3, "the scan should stop right after the third file");
    let stamped = lib
        .all_tracks()
        .unwrap()
        .iter()
        .filter(|t| t.last_scanned.is_some())
        .count();
    assert_eq!(
        stamped, 3,
        "the three tracks read before the cancel must be committed, not rolled back"
    );
}

/// A running scan must leave gaps for other writers.
///
/// The scan competes for SQLite's single write lock with `record_play` —
/// which the GTK tick calls from the main loop — plus the folder watcher and
/// the ID3 editor. Holding one transaction for a whole folder made that a
/// hard failure: the other connection blocked for the full 5 s `busy_timeout`
/// and then got "database is locked", freezing its caller for those 5 s. On a
/// real 36k library, where reading each file costs ~48 ms, the lock would have
/// been held for around half an hour.
///
/// The observer sets `busy_timeout=0` so an attempt either finds the lock free
/// right now or fails immediately. It polls only while the scan is running, so
/// the question is not "does it eventually succeed" — after the final commit
/// anything succeeds — but "does a gap ever appear mid-scan". A whole-folder
/// transaction never opens one; a time-budgeted one opens several.
#[test]
fn a_running_scan_leaves_gaps_for_other_writers() {
    #[cfg(not(target_os = "macos"))]
    gstreamer::init().ok();
    let db = NamedTempFile::with_suffix(".db").unwrap();
    let lib = MediaLibrary::open_at(db.path()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Tiny and numerous, so the scan comfortably outlives several budgets.
    let n = 4000usize;
    for i in 0..n {
        write_test_wav(&dir.path().join(format!("t_{i}.wav")), 8_000, 1, 0.01);
    }
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let observer = MediaLibrary::open_at(db.path()).unwrap();
    // Fail instantly rather than waiting: we are sampling for a free moment,
    // not trying to get the write done.
    observer.conn.execute_batch("PRAGMA busy_timeout=0;").unwrap();
    let target = dir.path().join("t_0.wav").to_str().unwrap().to_string();

    let scanning = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let found_gap = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let poller = {
        let scanning = scanning.clone();
        let found_gap = found_gap.clone();
        let attempts = attempts.clone();
        std::thread::spawn(move || {
            use std::sync::atomic::Ordering as O;
            while !scanning.load(O::Relaxed) {
                std::thread::yield_now();
            }
            // Keep sampling for the whole scan rather than stopping at the
            // first success, so `attempts` measures the window that was
            // actually available to sample.
            while scanning.load(O::Relaxed) {
                attempts.fetch_add(1, O::Relaxed);
                if observer.record_play(&target).is_ok() {
                    found_gap.store(true, O::Relaxed);
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        })
    };

    let cancel = std::sync::atomic::AtomicBool::new(false);
    lib.scan_folder(folder_id, &cancel, |done, _| {
        if done == 1 {
            scanning.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    })
    .unwrap();
    scanning.store(false, std::sync::atomic::Ordering::Relaxed);
    poller.join().unwrap();

    let tries = attempts.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        tries > 5,
        "the poller needs a real window to sample; it only tried {tries} times \
         — the scan finished too fast for this test to mean anything"
    );
    assert!(
        found_gap.load(std::sync::atomic::Ordering::Relaxed),
        "in {tries} attempts spread across the whole scan, the write lock was \
         never free — the scan is holding one transaction for the entire folder"
    );
}
