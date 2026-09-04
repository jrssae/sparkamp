//! The device tables' schema.

use super::*;

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
