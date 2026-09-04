//! Play counts and history: recording a play, the snapshot the UI reads, and the
//! auto-add-played path.

use super::*;

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

/// The no-probe variant must leave the file alone, even though the probing
/// one directly above proves the same fixture has everything to offer.
///
/// This is the now-playing contract. That panel rebuilds on every track
/// change, on the UI thread; a macOS audio CD track is a real ~40 MB AIFF on
/// the drive, so probing there read the whole file twice per track change and
/// starved the playback it was reading against. Asserting the fields are
/// *empty* is the only way to see the absence of I/O from a test.
#[test]
fn read_only_fields_no_probe_never_reads_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("outside.wav");
    write_test_wav(&p, 48000, 2, 2.0);

    let ro = read_only_track_fields_no_probe(&p, None);
    // Free facts — these come from the path, not its contents.
    assert_eq!(ro.filetype, "wav", "the extension costs nothing to read");
    assert_eq!(ro.filename, "outside.wav");
    // Everything that would need the file opened.
    assert_eq!(ro.sample_rate, "", "sample rate needs the codec probe");
    assert_eq!(ro.channels, "", "channels needs the codec probe");
    assert_eq!(ro.duration, "-:--", "duration needs the file");
    assert_eq!(ro.bitrate, "", "bitrate needs size and duration");
    assert_eq!(ro.artwork_path, "", "artwork needs the tag reader");
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
    #[cfg(not(target_os = "macos"))]
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
    #[cfg(not(target_os = "macos"))]
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
    #[cfg(not(target_os = "macos"))]
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
