//! Reading the library: search, sort keys, playlist resolution, targeted lookups
//! that must not read the whole table, and the change token they invalidate on.

use super::*;

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

// ── targeted lookups: don't read the whole table to pick a few rows ─────

/// Picking a handful of rows must not read the whole table. The macOS FFI
/// add path did: `all_tracks()` measures 370–390 ms against this machine's
/// 36,329-track library, where the equivalent `WHERE id IN (...)` measures
/// 116 us — and it ran synchronously on every add.
#[test]
fn tracks_by_ids_returns_only_what_was_asked_for() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 5);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let all = lib.all_tracks().unwrap();
    assert_eq!(all.len(), 5);
    let want: Vec<i64> = all.iter().take(2).map(|t| t.id).collect();

    let got = lib.tracks_by_ids(&want).unwrap();
    assert_eq!(got.len(), 2, "exactly the rows asked for");
    for id in &want {
        assert!(got.contains_key(id), "id {id} should be present");
        assert_eq!(got[id].id, *id, "keyed by its own id");
    }
}

/// An empty request must be empty, not "SELECT everything" — the exact
/// failure this replaces.
#[test]
fn tracks_by_ids_of_nothing_is_nothing() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    assert!(lib.tracks_by_ids(&[]).unwrap().is_empty());
}

/// More ids than SQLite's variable limit must still work — the same chunking
/// `tracks_by_exact_paths` already does for paths. Unknown ids are simply
/// absent rather than an error.
#[test]
fn tracks_by_ids_handles_more_ids_than_the_sqlite_variable_limit() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    let real: Vec<i64> = lib.all_tracks().unwrap().iter().map(|t| t.id).collect();

    // 1200 ids, only 3 of which exist: well past the 999-variable limit.
    let mut ids: Vec<i64> = (100_000..101_200).collect();
    ids.extend(&real);
    let got = lib.tracks_by_ids(&ids).unwrap();
    assert_eq!(got.len(), real.len(), "only the real rows come back");
}

/// The prefix lookup must match on a path boundary. The `starts_with` it
/// replaces did not, so adding `/music/rock` to a playlist also swept in
/// everything under `/music/rockabilly`.
#[test]
fn tracks_under_path_prefix_does_not_match_a_sibling_folder() {
    let (lib, _db) = temp_lib();
    let root = tempfile::tempdir().unwrap();
    let rock = root.path().join("rock");
    let rockabilly = root.path().join("rockabilly");
    fs::create_dir_all(&rock).unwrap();
    fs::create_dir_all(&rockabilly).unwrap();
    fs::write(rock.join("a.mp3"), b"x").unwrap();
    fs::write(rockabilly.join("b.mp3"), b"x").unwrap();
    let root_str = root.path().to_str().unwrap();
    let folder_id = lib.add_folder(root_str).unwrap().id();
    lib.rescan_folder_fast(folder_id, root_str, true).unwrap();
    assert_eq!(lib.all_tracks().unwrap().len(), 2, "both files indexed");

    // Canonicalized, because the scan stores canonical paths and the lookup
    // matches on the string. On macOS the temp dir is reached through a
    // symlink (`/var` -> `/private/var`), so the raw path matches nothing.
    let rock = rock.canonicalize().unwrap();
    let got = lib
        .tracks_under_path_prefix(rock.to_str().unwrap())
        .unwrap();
    assert_eq!(got.len(), 1, "only the rock/ track, not rockabilly/");
    assert!(got[0].path.ends_with("a.mp3"));
}

/// A folder name containing `%` or `_` must not turn into a LIKE wildcard —
/// `_` matches any single character, so an unescaped "Rock_Pop" would also
/// match "RockXPop".
#[test]
fn tracks_under_path_prefix_escapes_like_wildcards() {
    let (lib, _db) = temp_lib();
    let root = tempfile::tempdir().unwrap();
    let literal = root.path().join("Rock_Pop");
    let decoy = root.path().join("RockXPop");
    fs::create_dir_all(&literal).unwrap();
    fs::create_dir_all(&decoy).unwrap();
    fs::write(literal.join("a.mp3"), b"x").unwrap();
    fs::write(decoy.join("b.mp3"), b"x").unwrap();
    let root_str = root.path().to_str().unwrap();
    let folder_id = lib.add_folder(root_str).unwrap().id();
    lib.rescan_folder_fast(folder_id, root_str, true).unwrap();

    // Canonicalized for the same reason as the sibling-folder test above.
    let literal = literal.canonicalize().unwrap();
    let got = lib
        .tracks_under_path_prefix(literal.to_str().unwrap())
        .unwrap();
    assert_eq!(got.len(), 1, "'_' must be a literal underscore, not a wildcard");
    assert!(got[0].path.ends_with("a.mp3"));
}

/// The device sync planner wants every row but only two of the 37 columns.
#[test]
fn filename_path_index_maps_every_track() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 4);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let idx = lib.filename_path_index().unwrap();
    assert_eq!(idx.len(), 4);
    for t in lib.all_tracks().unwrap() {
        assert_eq!(idx.get(&t.filename).map(String::as_str), Some(t.path.as_str()));
    }
}

/// Basenames repeat across albums, so the index's row order decides which
/// library file a device file gets paired with. It must resolve a repeated
/// name to the same path `all_tracks()` did, or a device sync would silently
/// re-pair files after this change.
#[test]
fn filename_path_index_resolves_duplicates_the_same_way_all_tracks_did() {
    let (lib, _db) = temp_lib();
    let root = tempfile::tempdir().unwrap();
    // Two albums, same track filename in each.
    for album in ["Album A", "Album B"] {
        let d = root.path().join(album);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("01 - Intro.mp3"), b"x").unwrap();
    }
    let root_str = root.path().to_str().unwrap();
    let folder_id = lib.add_folder(root_str).unwrap().id();
    lib.rescan_folder_fast(folder_id, root_str, true).unwrap();
    assert_eq!(lib.all_tracks().unwrap().len(), 2);

    let expected: std::collections::HashMap<String, String> = lib
        .all_tracks()
        .unwrap()
        .into_iter()
        .map(|t| (t.filename, t.path))
        .collect();
    let got = lib.filename_path_index().unwrap();

    assert_eq!(got, expected, "the index must agree with what all_tracks produced");
    assert_eq!(got.len(), 1, "one entry survives for the repeated basename");
}

// ── change_token: O(1) cache-invalidation proxy ─────────────────────────
//
// The album gallery (frontends/gtk/window/album_gallery.rs) caches the
// whole-library `albums()` fold and only re-runs it when `change_token()`
// no longer matches the token it cached the fold under. These three cases
// are the whole contract: it must move on a write, and must NOT move
// between two reads, or the cache would either lie (never moves) or never
// pay off (moves on every check).

#[test]
fn change_token_changes_after_an_insert() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 1);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();

    let before = lib.change_token();
    // The scan's track upsert is a real INSERT.
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    let after = lib.change_token();

    assert_ne!(before, after, "an INSERT must move the token");
}

#[test]
fn change_token_changes_after_a_delete() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 1);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    let track_id = lib.all_tracks().unwrap()[0].id;

    let before = lib.change_token();
    // soft_delete_tracks alone only flips a flag; purge_deleted_tracks is
    // the real `DELETE FROM tracks` — the review's finding #1 was that this
    // is the statement the album fold's answer actually depends on.
    lib.soft_delete_tracks(&[track_id]).unwrap();
    lib.purge_deleted_tracks().unwrap();
    let after = lib.change_token();

    assert_ne!(before, after, "a DELETE (purge_deleted_tracks) must move the token");
}

#[test]
fn change_token_is_unchanged_across_two_reads_with_no_write_between() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 2);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();

    let before = lib.change_token();
    // Two ordinary reads — exactly what the gallery's `rebuild()` does on
    // every call, including the ones that end up skipping the fold.
    let _ = lib.all_tracks().unwrap();
    let _ = lib.albums(AlbumSort::Artist, false).unwrap();
    let after = lib.change_token();

    assert_eq!(before, after, "reads alone must not move the token");
}

/// Fix round 2 review: the three tests above all read/write through one
/// `MediaLibrary`, so they only exercise the `total_changes` half of the
/// token. Track removal (`files.rs`'s `btn_rm_from_ml`,
/// `files_menu.rs`'s `ml.remove`) runs `soft_delete_tracks` +
/// `purge_deleted_tracks` on its OWN `Connection`, opened via
/// `MediaLibrary::open_at` on a background thread — never the long-lived
/// connection the gallery reads through. `total_changes` alone cannot see
/// that write; only `PRAGMA data_version` can, and nothing above exercises
/// it. This is the test that actually covers Critical finding #1.
///
/// A second `MediaLibrary::open_at` on the SAME database file, on this same
/// test thread, reproduces the "separate `Connection`, same file" shape
/// those write paths use. A second OS thread is not needed to make this
/// faithful: SQLite's `data_version`/`sqlite3_total_changes` accounting is
/// keyed on the `sqlite3*` connection handle, not the thread that holds it
/// (confirmed against the same `sqlite3.h` comments `change_token`'s doc
/// cites — both describe "database connections", never threads) — the
/// production code only uses a thread because `rusqlite::Connection` is not
/// `Send`, a constraint that doesn't apply to two connections opened
/// side by side in one test function.
#[test]
fn change_token_reacts_to_a_delete_made_through_a_second_connection() {
    let (lib, db_file) = temp_lib();
    let dir = temp_dir_with_files("mp3", 1);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    let track_id = lib.all_tracks().unwrap()[0].id;

    // Second connection to the same database file — exactly what
    // `MediaLibrary::open_at(&db_path)` gives the background thread in
    // files.rs.
    let lib2 = MediaLibrary::open_at(db_file.path()).unwrap();

    // A read through either connection must be inert first: the drill-down
    // fix (Critical finding #2) depends on reads never moving the token,
    // and that has to hold across connections too, not just on the one the
    // other tests use.
    let before = lib.change_token();
    let _ = lib.all_tracks().unwrap();
    let _ = lib2.all_tracks().unwrap();
    assert_eq!(
        before,
        lib.change_token(),
        "a read on either connection must not move the first connection's token"
    );

    // Delete through the SECOND connection — the shape the real removal
    // paths use.
    lib2.soft_delete_tracks(&[track_id]).unwrap();
    lib2.purge_deleted_tracks().unwrap();

    let after = lib.change_token();
    assert_ne!(
        before, after,
        "a DELETE made through a different connection must move the FIRST \
         connection's token via PRAGMA data_version — total_changes alone \
         cannot see a write made on another connection"
    );
}
