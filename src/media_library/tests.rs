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

// ── upsert_track: technical columns + added_at stability ───────────────

/// Minimal valid PCM WAV with exactly `secs` seconds of silence. Symphonia
/// derives duration straight from the header (data chunk size ÷ byte rate),
/// so this is enough to make `avg_bitrate_kbps`'s >0.5s threshold trip
/// without needing a real audio fixture.
fn write_test_wav(path: &std::path::Path, sample_rate: u32, channels: u16, secs: f64) {
    let bytes_per_frame = channels as u32 * 2;
    let data_len = (sample_rate as f64 * secs) as u32 * bytes_per_frame;
    let byte_rate = sample_rate * bytes_per_frame;
    let block_align = channels * 2;
    let mut buf = Vec::new();
    buf.extend(b"RIFF");
    buf.extend(&(36 + data_len).to_le_bytes());
    buf.extend(b"WAVE");
    buf.extend(b"fmt ");
    buf.extend(&16u32.to_le_bytes());
    buf.extend(&1u16.to_le_bytes()); // PCM
    buf.extend(&channels.to_le_bytes());
    buf.extend(&sample_rate.to_le_bytes());
    buf.extend(&byte_rate.to_le_bytes());
    buf.extend(&block_align.to_le_bytes());
    buf.extend(&16u16.to_le_bytes()); // bits per sample
    buf.extend(b"data");
    buf.extend(&data_len.to_le_bytes());
    buf.extend(std::iter::repeat(0u8).take(data_len as usize));
    fs::write(path, buf).unwrap();
}

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
        sample_rate: None,
        file_size: None,
        file_mtime: None,
        added_at: None,
        bitrate_mode: None,
        rg_track_gain: None,
        rg_track_peak: None,
        rg_album_gain: None,
        rg_album_peak: None,
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
        sample_rate: None,
        file_size: None,
        file_mtime: None,
        added_at: None,
        bitrate_mode: None,
        rg_track_gain: None,
        rg_track_peak: None,
        rg_album_gain: None,
        rg_album_peak: None,
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
    lib.rescan_folder_fast(folder_id, dir.path().to_str().unwrap(), true)
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
    lib.rescan_folder_fast(folder_id, dir.path().to_str().unwrap(), true)
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
    lib.rescan_folder_fast(folder_id, dir.path().to_str().unwrap(), true)
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

// ── play_snapshot ──────────────────────────────────────────────────────

#[test]
fn play_snapshot_reads_preplay_values() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("song.mp3");
    let path = file_path.to_str().unwrap();
    fs::write(&file_path, b"fake").unwrap();

    let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();
    lib.rescan_folder_fast(folder_id, dir.path().to_str().unwrap(), true)
        .unwrap();

    // Track present, never played.
    let snap0 = lib.play_snapshot(path);
    assert_eq!(snap0.play_count, Some(0));
    assert_eq!(snap0.last_played, None);

    // After a recorded play, the ROW advances but a snapshot taken earlier is stale.
    lib.record_play(path).unwrap();
    let snap1 = lib.play_snapshot(path);
    assert_eq!(snap1.play_count, Some(1));
    assert!(snap1.last_played.is_some());
}

#[test]
fn play_snapshot_none_for_unknown_path() {
    let (lib, _db) = temp_lib();
    let snap = lib.play_snapshot("/nonexistent/x.mp3");
    assert_eq!(snap.play_count, None);
    assert_eq!(snap.last_played, None);
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
        sample_rate: None,
        file_size: None,
        file_mtime: None,
        added_at: None,
        bitrate_mode: None,
        rg_track_gain: None,
        rg_track_peak: None,
        rg_album_gain: None,
        rg_album_peak: None,
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
    // Non-library files now probe the file directly for technical fields
    // (phase-1 user-pass fix). A NONEXISTENT path still degrades cleanly:
    // filetype from the extension, everything probe-derived empty.
    let path = std::path::Path::new("/unknown/file.mp3");
    let ro = read_only_track_fields(path, None);

    assert_eq!(ro.filename, "file.mp3");
    assert_eq!(ro.path, "/unknown/file.mp3");
    assert_eq!(ro.filetype, "mp3");
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
        sample_rate: None,
        file_size: None,
        file_mtime: None,
        added_at: None,
        bitrate_mode: None,
        rg_track_gain: None,
        rg_track_peak: None,
        rg_album_gain: None,
        rg_album_peak: None,
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
        sample_rate: None,
        file_size: None,
        file_mtime: None,
        added_at: None,
        bitrate_mode: None,
        rg_track_gain: None,
        rg_track_peak: None,
        rg_album_gain: None,
        rg_album_peak: None,
        sort_keys: SortKeys::default(),
    };
    let path = std::path::Path::new("/test.mp3");
    let ro = read_only_track_fields(path, Some(&track));
    assert_eq!(ro.channels, "6ch");
}

#[test]
fn tech_summary_joins_populated_parts_only() {
    let ro = ReadOnlyTrackFields {
        filetype: "mp3".into(),
        bitrate: "320k".into(),
        sample_rate: "44.1 kHz".into(),
        channels: "stereo".into(),
        duration: "3:45".into(),
        // fill the remaining fields with Default/empty per the struct
        ..Default::default()
    };
    assert_eq!(tech_summary(&ro), "MP3 · 320k · 44.1 kHz · stereo · 3:45");

    let sparse = ReadOnlyTrackFields {
        duration: "3:45".into(),
        ..Default::default()
    };
    assert_eq!(tech_summary(&sparse), "3:45");
}


#[test]
fn read_only_fields_probe_fallback_for_non_library_files() {
    // A file with no LibTrack row (played from outside the library) must
    // still get a tech line: filetype from the extension, sample rate and
    // channels from the codec probe, duration/bitrate from the file itself.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("outside.wav");
    write_test_wav(&p, 48000, 2, 2.0);

    let ro = read_only_track_fields(&p, None);
    assert_eq!(ro.filetype, "wav");
    assert_eq!(ro.sample_rate, "48.0 kHz");
    assert_eq!(ro.channels, "stereo");
    assert_ne!(ro.duration, "-:--", "duration must come from the probe");
    assert!(!ro.bitrate.is_empty(), "bitrate must be computed from size/duration");
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
    lib.rescan_folder_fast(fid, stale_dir.path().to_str().unwrap(), true).unwrap();
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

// ── search_tracks / search_tracks_sorted ────────────────────────────────

#[test]
fn search_tracks_matches_case_insensitive_substrings() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    assert_eq!(lib.search_tracks("TRACK_").unwrap().len(), 3);
    let one = lib.search_tracks("track_1").unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].filename, "track_1.mp3");
    assert!(lib.search_tracks("zzz-no-match").unwrap().is_empty());
}

#[test]
fn search_tracks_with_empty_query_returns_nothing() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 2);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    // Consistent with the jump window: empty (or whitespace) query = empty
    // result, not "everything".
    assert!(lib.search_tracks("").unwrap().is_empty());
    assert!(lib.search_tracks("   ").unwrap().is_empty());
}

#[test]
fn search_words_all_have_to_match_and_sort_is_honored() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    // Two words AND together: "track" hits all, "_2" narrows to one.
    let hits = lib.search_tracks("track _2").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].filename, "track_2.mp3");

    // Descending filename sort puts track_2 first.
    let sorted = lib.search_tracks_sorted("track", "filename", true).unwrap();
    assert_eq!(sorted.len(), 3);
    assert_eq!(sorted[0].filename, "track_2.mp3");
    assert_eq!(sorted[2].filename, "track_0.mp3");
}

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

// ── apply_watch_action: routes fs watch events through the scan seam ───

fn track_row_exists(lib: &MediaLibrary, path: &str) -> bool {
    lib.conn
        .query_row(
            "SELECT 1 FROM tracks WHERE path = ?1",
            rusqlite::params![path],
            |_| Ok(()),
        )
        .is_ok()
}

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

// ── add_played_track: auto-add-played core method (Phase 8 Task 7) ─────
//
// Frontend call-site wiring (playback hook, `auto_add_played` config gate)
// is deliberately out of scope — later GTK/TUI/mac tasks own that. This
// method only needs to get a played file into the `tracks` table using the
// exact same folder-resolution rules as a fs watch event, and to be a
// true no-op for a file the library already knows about.

#[test]
fn add_played_outside_library_creates_null_folder_row() {
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();
    // No folders registered at all, mirroring
    // apply_upsert_outside_folders_gets_null_folder_id: a played file with
    // no watched folder above it must still land in the library, in the
    // NULL-folder_id bucket the Files view already knows how to show.
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().canonicalize().unwrap().join("track.mp3");
    fs::write(&file_path, b"fake audio data").unwrap();
    let path = file_path.to_str().unwrap();

    let created = lib.add_played_track(path).unwrap();

    assert!(created, "first play of an unknown file must return Ok(true)");
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
    assert!(
        lib.all_tracks_sorted("filename", false)
            .unwrap()
            .iter()
            .any(|t| t.path == path),
        "the NULL-bucket row must be visible in the Files view (all_tracks_sorted)"
    );
}

#[test]
fn add_played_existing_is_noop() {
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 1);
    let folder_path = dir.path().canonicalize().unwrap();
    let folder_id = lib.add_folder(folder_path.to_str().unwrap()).unwrap().id();
    let file_path = folder_path.join("track_0.mp3");
    let path = file_path.to_str().unwrap();
    lib.upsert_track(folder_id, path).unwrap();
    assert!(track_row_exists(&lib, path));

    let created = lib.add_played_track(path).unwrap();

    assert!(
        !created,
        "playing a file already in the library must return Ok(false)"
    );
    let count: i64 = lib
        .conn
        .query_row(
            "SELECT COUNT(*) FROM tracks WHERE path = ?1",
            rusqlite::params![path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "must not duplicate the row");
}

#[test]
fn add_played_inside_library_attaches_to_folder() {
    gstreamer::init().ok();
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 1);
    let folder_path = dir.path().canonicalize().unwrap();
    let folder_id = lib.add_folder(folder_path.to_str().unwrap()).unwrap().id();
    let file_path = folder_path.join("track_0.mp3");
    let path = file_path.to_str().unwrap();

    let created = lib.add_played_track(path).unwrap();

    assert!(created);
    let got_folder_id: Option<i64> = lib
        .conn
        .query_row(
            "SELECT folder_id FROM tracks WHERE path = ?1",
            rusqlite::params![path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        got_folder_id,
        Some(folder_id),
        "a played file under a watched folder must be attached to it, not NULL"
    );
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
