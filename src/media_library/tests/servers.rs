//! The server catalog cache.

use super::*;
use crate::media_library::servers::{PageOutcome, PullOutcome};
use crate::servers::api::ServerSong;

fn song(id: &str, path: &str, title: &str) -> ServerSong {
    ServerSong {
        id: id.into(),
        path: Some(path.into()),
        title: title.into(),
        artist: "Artist".into(),
        album: "Album".into(),
        duration_secs: Some(200),
        user_rating: 3,
        ..ServerSong::default()
    }
}

fn pull(lib: &MediaLibrary, server: &str, songs: &[ServerSong]) -> PullOutcome {
    let pull = lib.begin_server_pull(server).unwrap();
    lib.apply_server_songs(server, pull, songs).unwrap();
    lib.finish_server_pull(server, pull).unwrap()
}

fn titles(lib: &MediaLibrary, server: &str) -> Vec<String> {
    lib.server_songs(server).unwrap().into_iter().map(|r| r.song.title).collect()
}

#[test]
fn a_first_pull_caches_every_song_with_its_fields() {
    let (lib, _db) = temp_lib();
    let mut s = song("s1", "/music/A/01.mp3", "One");
    s.isrc = vec!["USSM15900113".into(), "GBAYE0601498".into()];
    s.replay_gain = Some(crate::servers::api::ReplayGain {
        track_gain: Some(-6.5),
        ..Default::default()
    });
    let pull_id = lib.begin_server_pull("oscar").unwrap();
    assert_eq!(
        lib.apply_server_songs("oscar", pull_id, &[s.clone(), song("s2", "/music/A/02.mp3", "Two")])
            .unwrap(),
        PageOutcome { added: 2, updated: 0 }
    );
    lib.finish_server_pull("oscar", pull_id).unwrap();
    let rows = lib.server_songs("oscar").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].song, s, "every field survives the round trip");
}

#[test]
fn a_retagged_song_keeps_its_row_and_takes_the_new_song_id() {
    let (lib, _db) = temp_lib();
    pull(&lib, "oscar", &[song("old-id", "/music/A/01.mp3", "test")]);
    let before = lib.server_songs("oscar").unwrap()[0].id;

    let p = lib.begin_server_pull("oscar").unwrap();
    let outcome =
        lib.apply_server_songs("oscar", p, &[song("new-id", "/music/A/01.mp3", "Test Song")]).unwrap();
    lib.finish_server_pull("oscar", p).unwrap();

    assert_eq!(outcome, PageOutcome { added: 0, updated: 1 });
    let rows = lib.server_songs("oscar").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, before);
    assert_eq!(rows[0].song.id, "new-id");
    assert_eq!(rows[0].song.title, "Test Song");
}

#[test]
fn an_unchanged_song_is_neither_added_nor_updated() {
    let (lib, _db) = temp_lib();
    pull(&lib, "oscar", &[song("s1", "/music/A/01.mp3", "One")]);
    let p = lib.begin_server_pull("oscar").unwrap();
    assert_eq!(
        lib.apply_server_songs("oscar", p, &[song("s1", "/music/A/01.mp3", "One")]).unwrap(),
        PageOutcome::default()
    );
}

#[test]
fn a_complete_pull_removes_songs_the_server_no_longer_has() {
    let (lib, _db) = temp_lib();
    pull(&lib, "oscar", &[song("s1", "/music/A/01.mp3", "One"), song("s2", "/music/A/02.mp3", "Two")]);
    match pull(&lib, "oscar", &[song("s1", "/music/A/01.mp3", "One")]) {
        PullOutcome::Removed(gone) => {
            assert_eq!(gone.iter().map(|r| r.song.title.as_str()).collect::<Vec<_>>(), vec!["Two"])
        }
        held => panic!("expected a removal, got {held:?}"),
    }
    assert_eq!(titles(&lib, "oscar"), vec!["One"]);
}

#[test]
fn a_pull_that_never_finished_removes_nothing() {
    let (lib, _db) = temp_lib();
    pull(&lib, "oscar", &[song("s1", "/music/A/01.mp3", "One"), song("s2", "/music/A/02.mp3", "Two")]);
    // The connection dropped after the first page.
    let p = lib.begin_server_pull("oscar").unwrap();
    lib.apply_server_songs("oscar", p, &[song("s1", "/music/A/01.mp3", "One")]).unwrap();
    assert_eq!(titles(&lib, "oscar"), vec!["One", "Two"]);
}

#[test]
fn a_mass_removal_is_held_until_confirmed() {
    let (lib, _db) = temp_lib();
    let all: Vec<ServerSong> = (0..600)
        .map(|i| song(&format!("s{i}"), &format!("/music/A/{i:03}.mp3"), &format!("T{i}")))
        .collect();
    pull(&lib, "oscar", &all);

    let p = lib.begin_server_pull("oscar").unwrap();
    lib.apply_server_songs("oscar", p, &all[..50]).unwrap();
    assert_eq!(
        lib.finish_server_pull("oscar", p).unwrap(),
        PullOutcome::Held { would_remove: 550, cached: 600 }
    );
    assert_eq!(lib.server_songs("oscar").unwrap().len(), 600, "nothing removed yet");

    assert_eq!(lib.confirm_server_removals("oscar", p).unwrap().len(), 550);
    assert_eq!(lib.server_songs("oscar").unwrap().len(), 50);
}

#[test]
fn removing_a_few_songs_from_a_small_library_is_not_held() {
    let (lib, _db) = temp_lib();
    let all: Vec<ServerSong> = (0..10)
        .map(|i| song(&format!("s{i}"), &format!("/music/A/{i}.mp3"), &format!("T{i}")))
        .collect();
    pull(&lib, "oscar", &all);
    assert!(matches!(pull(&lib, "oscar", &all[..8]), PullOutcome::Removed(r) if r.len() == 2));
}

#[test]
fn each_server_keeps_its_own_catalog() {
    let (lib, _db) = temp_lib();
    pull(&lib, "oscar", &[song("s1", "/music/A/01.mp3", "One")]);
    pull(&lib, "server2", &[song("x1", "/data/A/01.mp3", "Other")]);
    pull(&lib, "oscar", &[]);
    assert_eq!(titles(&lib, "oscar"), Vec::<String>::new());
    assert_eq!(titles(&lib, "server2"), vec!["Other"]);
}

// ── linking copies into songs ─────────────────────────────────────────────

use crate::media_library::servers::{Member, SongCopies};
use crate::servers::matcher::LinkReason;
use crate::servers::merge::Fields;

/// Two local files on disk, scanned, plus oscar's catalog of two songs.
fn linked_setup(lib: &MediaLibrary) -> (tempfile::TempDir, Vec<i64>, Vec<i64>) {
    let dir = temp_dir_with_files("mp3", 2);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    let locals: Vec<i64> = lib.all_tracks().unwrap().iter().map(|t| t.id).collect();
    pull(lib, "oscar", &[song("s1", "/music/A/01.mp3", "One"), song("s2", "/music/A/02.mp3", "Two")]);
    let servers: Vec<i64> = lib.server_songs("oscar").unwrap().iter().map(|r| r.id).collect();
    (dir, locals, servers)
}

fn members(copies: &SongCopies) -> Vec<(Member, LinkReason)> {
    copies.members.iter().map(|m| (m.member, m.how)).collect()
}

#[test]
fn a_linked_local_file_and_server_copy_are_one_song() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    lib.link_copies(Member::Local(locals[0]), Member::Server(servers[0]), LinkReason::Tags).unwrap();

    let copies = lib.song_copies(Member::Server(servers[0])).unwrap().expect("linked");
    assert_eq!(
        members(&copies),
        vec![(Member::Local(locals[0]), LinkReason::Tags), (Member::Server(servers[0]), LinkReason::Tags)]
    );
    assert_eq!(lib.song_copies(Member::Local(locals[0])).unwrap(), Some(copies));
    assert_eq!(lib.song_copies(Member::Server(servers[1])).unwrap(), None, "unlinked");
}

#[test]
fn a_linked_server_copy_is_no_longer_listed_as_its_own_row() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    assert_eq!(lib.shown_server_track_ids().unwrap(), servers);
    lib.link_copies(Member::Local(locals[0]), Member::Server(servers[0]), LinkReason::Path).unwrap();
    assert_eq!(lib.shown_server_track_ids().unwrap(), vec![servers[1]]);
}

#[test]
fn a_song_cannot_get_two_local_copies_or_two_copies_from_one_server() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    lib.link_copies(Member::Local(locals[0]), Member::Server(servers[0]), LinkReason::Manual).unwrap();
    assert!(lib.link_copies(Member::Local(locals[1]), Member::Server(servers[0]), LinkReason::Manual).is_err());
    assert!(lib.link_copies(Member::Local(locals[0]), Member::Server(servers[1]), LinkReason::Manual).is_err());
}

#[test]
fn unlinking_splits_the_song_and_the_matcher_never_proposes_the_pair_again() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    lib.link_copies(Member::Local(locals[0]), Member::Server(servers[0]), LinkReason::Filename).unwrap();
    lib.unlink_copy(Member::Server(servers[0])).unwrap();

    assert_eq!(lib.song_copies(Member::Local(locals[0])).unwrap(), None);
    assert!(lib.shown_server_track_ids().unwrap().contains(&servers[0]));
    assert_eq!(lib.never_link_pairs("oscar").unwrap(), vec![(locals[0], servers[0])]);
}

#[test]
fn a_never_link_pair_survives_the_song_id_changing() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    lib.link_copies(Member::Local(locals[0]), Member::Server(servers[0]), LinkReason::Tags).unwrap();
    lib.unlink_copy(Member::Local(locals[0])).unwrap();
    // oscar retags the file: new song ID, same path.
    pull(&lib, "oscar", &[song("s1-new", "/music/A/01.mp3", "One!"), song("s2", "/music/A/02.mp3", "Two")]);
    assert_eq!(lib.never_link_pairs("oscar").unwrap(), vec![(locals[0], servers[0])]);
}

#[test]
fn removing_the_local_file_leaves_the_server_copy_listed_on_its_own() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    lib.link_copies(Member::Local(locals[0]), Member::Server(servers[0]), LinkReason::Tags).unwrap();
    lib.remove_track(locals[0]).unwrap();
    assert_eq!(lib.song_copies(Member::Server(servers[0])).unwrap(), None);
    assert!(lib.shown_server_track_ids().unwrap().contains(&servers[0]));
}

#[test]
fn a_server_removing_its_copy_unlinks_it() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    lib.link_copies(Member::Local(locals[0]), Member::Server(servers[0]), LinkReason::Tags).unwrap();
    pull(&lib, "oscar", &[song("s2", "/music/A/02.mp3", "Two")]);
    assert_eq!(lib.song_copies(Member::Local(locals[0])).unwrap(), None);
}

#[test]
fn a_song_on_two_servers_and_no_local_copy_is_listed_once() {
    let (lib, _db) = temp_lib();
    pull(&lib, "oscar", &[song("s1", "/music/A/01.mp3", "One")]);
    pull(&lib, "server2", &[song("x1", "/data/A/01.mp3", "One")]);
    let a = lib.server_songs("oscar").unwrap()[0].id;
    let b = lib.server_songs("server2").unwrap()[0].id;
    lib.link_copies(Member::Server(a), Member::Server(b), LinkReason::Tags).unwrap();
    assert_eq!(lib.shown_server_track_ids().unwrap(), vec![a]);
}

#[test]
fn each_copy_keeps_its_own_last_agreed_values() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    lib.link_copies(Member::Local(locals[0]), Member::Server(servers[0]), LinkReason::Tags).unwrap();
    let copies = lib.song_copies(Member::Local(locals[0])).unwrap().unwrap();
    assert!(copies.members.iter().all(|m| m.baseline.is_none()), "no history when first linked");

    let local = Fields { title: "Test Song".into(), artist: "Artist".into(), ..Fields::default() };
    let server = Fields { title: "Test Song".into(), ..Fields::default() };
    lib.set_baseline(Member::Local(locals[0]), Some(&local)).unwrap();
    lib.set_baseline(Member::Server(servers[0]), Some(&server)).unwrap();

    let copies = lib.song_copies(Member::Local(locals[0])).unwrap().unwrap();
    assert_eq!(copies.members[0].baseline, Some(local));
    assert_eq!(copies.members[1].baseline, Some(server));
}

// ── differences between linked copies ────────────────────────────────────

use crate::servers::merge::{CopyId, Field, FieldOutcome, SongStatus, Value};

/// A local file tagged "Test Song" by "Artist", linked to an oscar copy.
fn tagged_pair(lib: &MediaLibrary, server_song: ServerSong) -> (tempfile::TempDir, i64, i64) {
    let dir = temp_dir_with_files("mp3", 1);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    let local = lib.all_tracks().unwrap()[0].id;
    lib.conn
        .execute(
            "UPDATE tracks SET title = 'Test Song', artist = 'Artist', album = 'Album' WHERE id = ?1",
            [local],
        )
        .unwrap();
    pull(lib, "oscar", &[server_song]);
    let server = lib.server_songs("oscar").unwrap()[0].id;
    lib.link_copies(Member::Local(local), Member::Server(server), LinkReason::Filename).unwrap();
    (dir, local, server)
}

fn untagged(path: &str) -> ServerSong {
    ServerSong {
        id: "s1".into(),
        path: Some(path.into()),
        title: "track_0".into(),
        artist: "[Unknown Artist]".into(),
        album: "[Unknown Album]".into(),
        ..ServerSong::default()
    }
}

fn status(lib: &MediaLibrary, local: i64) -> SongStatus {
    lib.song_merge(Member::Local(local)).unwrap().unwrap().status()
}

#[test]
fn a_tagged_local_copy_of_an_untagged_server_file_is_local_changed() {
    let (lib, _db) = temp_lib();
    let (_dir, local, _server) = tagged_pair(&lib, untagged("/music/incoming/track_0.mp3"));
    let merge = lib.song_merge(Member::Local(local)).unwrap().unwrap();
    assert_eq!(
        *merge.outcome(Field::Artist),
        FieldOutcome::Wins {
            value: Value::Text("Artist".into()),
            behind: vec![CopyId::Server("oscar".into())]
        }
    );
    assert_eq!(merge.status(), SongStatus::LocalChanged);
}

#[test]
fn an_unlinked_song_has_no_merge() {
    let (lib, _db) = temp_lib();
    pull(&lib, "oscar", &[song("s1", "/music/A/01.mp3", "One")]);
    let id = lib.server_songs("oscar").unwrap()[0].id;
    assert_eq!(lib.song_merge(Member::Server(id)).unwrap(), None);
}

#[test]
fn accepted_differences_stay_quiet_until_a_copy_changes_again() {
    let (lib, _db) = temp_lib();
    let (_dir, local, _server) = tagged_pair(&lib, untagged("/music/incoming/track_0.mp3"));
    lib.accept_differences(Member::Local(local)).unwrap();
    assert_eq!(status(&lib, local), SongStatus::InSync);

    // Someone tags the file on the server.
    let mut retagged = untagged("/music/incoming/track_0.mp3");
    retagged.genre = "Jazz".into();
    pull(&lib, "oscar", &[retagged]);
    assert_eq!(status(&lib, local), SongStatus::ServerChanged);
}

#[test]
fn a_chosen_value_keeps_the_other_copy_behind_until_it_catches_up() {
    let (lib, _db) = temp_lib();
    let mut server = untagged("/music/incoming/track_0.mp3");
    server.title = "test".into();
    server.artist = "Artist".into();
    server.album = "Album".into();
    let (_dir, local, _server) = tagged_pair(&lib, server.clone());
    assert_eq!(status(&lib, local), SongStatus::FirstLinkDiffers);

    lib.resolve_song(Member::Local(local), &[(Field::Title, Value::Text("Test Song".into()))])
        .unwrap();
    assert_eq!(status(&lib, local), SongStatus::LocalChanged, "oscar is behind on the title");
    assert_eq!(status(&lib, local), SongStatus::LocalChanged, "and stays behind");

    // The fixed file reaches oscar.
    server.title = "Test Song".into();
    pull(&lib, "oscar", &[server]);
    assert_eq!(status(&lib, local), SongStatus::InSync);
}

#[test]
fn copies_that_agree_completely_settle_their_history() {
    let (lib, _db) = temp_lib();
    let mut server = untagged("/music/incoming/track_0.mp3");
    server.title = "Test Song".into();
    server.artist = "Artist".into();
    server.album = "Album".into();
    let (_dir, local, _server) = tagged_pair(&lib, server);
    assert_eq!(status(&lib, local), SongStatus::InSync);
    lib.settle_song(Member::Local(local)).unwrap();
    let copies = lib.song_copies(Member::Local(local)).unwrap().unwrap();
    assert!(copies.members.iter().all(|m| m.baseline.is_some()), "agreement is now history");
}

#[test]
fn the_local_rating_is_part_of_the_comparison() {
    let (lib, _db) = temp_lib();
    let mut server = untagged("/music/incoming/track_0.mp3");
    server.title = "Test Song".into();
    server.artist = "Artist".into();
    server.album = "Album".into();
    server.user_rating = 4;
    let (_dir, local, _server) = tagged_pair(&lib, server);
    assert_eq!(status(&lib, local), SongStatus::ServerChanged, "unrated locally, rated on oscar");
    lib.set_local_rating(local, 4).unwrap();
    assert_eq!(status(&lib, local), SongStatus::InSync);
}

// ── server songs in local playlists ───────────────────────────────────────

/// A registered, empty `.m3u8` in a temp dir.
fn temp_playlist(lib: &MediaLibrary, dir: &std::path::Path) -> (i64, std::path::PathBuf) {
    let path = dir.join("Mix.m3u8");
    std::fs::write(&path, "#EXTM3U\n").unwrap();
    let id = lib.add_playlist_file(path.to_str().unwrap()).unwrap();
    (id, path)
}

#[test]
fn a_server_only_song_is_saved_as_a_sparkamp_line_and_loads_back_in_place() {
    let (lib, _db) = temp_lib();
    let (dir, locals, servers) = linked_setup(&lib);
    let (pl, file) = temp_playlist(&lib, dir.path());

    lib.save_playlist_tracks(pl, &[locals[0], -servers[1], locals[1]]).unwrap();

    let text = std::fs::read_to_string(&file).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.iter().any(|l| l.starts_with("#SPARKAMP-SONG:server=oscar;path=/music/A/02.mp3;title=Two")), "{text}");
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("#EXTINF") {
            assert!(!lines[i + 1].starts_with('#'), "an #EXTINF is always followed by its path:\n{text}");
        }
    }

    let loaded = lib.load_playlist_tracks(&lib.playlist_by_id(pl).unwrap()).unwrap();
    assert_eq!(loaded.iter().map(|t| t.id).collect::<Vec<_>>(), vec![locals[0], -servers[1], locals[1]]);
    assert_eq!(loaded[1].path, crate::servers::uri::song_uri("oscar", "/music/A/02.mp3"));
    assert_eq!(loaded[1].title.as_deref(), Some("Two"));
}

#[test]
fn a_server_song_with_a_local_copy_is_saved_as_the_local_path() {
    let (lib, _db) = temp_lib();
    let (dir, locals, servers) = linked_setup(&lib);
    lib.link_copies(Member::Local(locals[1]), Member::Server(servers[1]), LinkReason::Tags).unwrap();
    let (pl, file) = temp_playlist(&lib, dir.path());
    lib.save_playlist_tracks(pl, &[-servers[1]]).unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains("#SPARKAMP-SONG"), "{text}");
    let loaded = lib.load_playlist_tracks(&lib.playlist_by_id(pl).unwrap()).unwrap();
    assert_eq!(loaded[0].id, locals[1]);
}

#[test]
fn a_sparkamp_line_for_a_song_now_local_loads_as_the_local_file() {
    let (lib, _db) = temp_lib();
    let (dir, locals, servers) = linked_setup(&lib);
    let (pl, _file) = temp_playlist(&lib, dir.path());
    lib.save_playlist_tracks(pl, &[-servers[0]]).unwrap();
    // Later the song gets a local copy.
    lib.link_copies(Member::Local(locals[0]), Member::Server(servers[0]), LinkReason::Filename).unwrap();
    let loaded = lib.load_playlist_tracks(&lib.playlist_by_id(pl).unwrap()).unwrap();
    assert_eq!(loaded[0].id, locals[0]);
}

#[test]
fn a_sparkamp_line_for_a_song_the_server_removed_loads_as_missing() {
    let (lib, _db) = temp_lib();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Old.m3u8");
    std::fs::write(
        &path,
        "#EXTM3U\n#SPARKAMP-SONG:server=oscar;path=/music/Gone/01.mp3;title=Gone Song;artist=Someone\n",
    )
    .unwrap();
    let pl = lib.add_playlist_file(path.to_str().unwrap()).unwrap();
    let loaded = lib.load_playlist_tracks(&lib.playlist_by_id(pl).unwrap()).unwrap();
    assert_eq!(loaded.len(), 1, "kept, not dropped");
    assert_eq!(loaded[0].id, 0);
    assert_eq!(loaded[0].title.as_deref(), Some("Gone Song"));
    assert_eq!(loaded[0].path, crate::servers::uri::song_uri("oscar", "/music/Gone/01.mp3"));
}

#[test]
fn path_based_saves_write_song_uris_as_sparkamp_lines() {
    let (lib, _db) = temp_lib();
    let (dir, _locals, _servers) = linked_setup(&lib);
    let target = dir.path().join("Saved.m3u8");
    let uri = crate::servers::uri::song_uri("oscar", "/music/A/01.mp3");
    lib.save_playlist_tracks_to_path(&target, &[uri]).unwrap();
    let text = std::fs::read_to_string(&target).unwrap();
    assert!(text.contains("#SPARKAMP-SONG:server=oscar;path=/music/A/01.mp3;title=One"), "{text}");
    assert!(!text.contains("subsonic://"), "never a URI line other players would try to open");
}

// ── plays and the send queue ──────────────────────────────────────────────

fn play_count(lib: &MediaLibrary, id: i64) -> i64 {
    lib.tracks_by_ids(&[id]).unwrap()[&id].play_count
}

#[test]
fn playing_a_linked_local_file_counts_locally_and_queues_a_scrobble() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    lib.link_copies(Member::Local(locals[0]), Member::Server(servers[0]), LinkReason::Tags).unwrap();
    let path = lib.tracks_by_ids(&[locals[0]]).unwrap()[&locals[0]].path.clone();

    lib.record_play_at(&path, 1_759_000_000_000).unwrap();

    assert_eq!(play_count(&lib, locals[0]), 1);
    assert_eq!(lib.pending_scrobbles("oscar").unwrap(), vec![("s1".to_string(), 1_759_000_000_000)]);
}

#[test]
fn playing_a_server_only_song_queues_a_scrobble_and_adds_no_library_row() {
    let (lib, _db) = temp_lib();
    pull(&lib, "oscar", &[song("s1", "/music/A/01.mp3", "One")]);
    let uri = crate::servers::uri::song_uri("oscar", "/music/A/01.mp3");
    lib.record_play_at(&uri, 1_759_000_000_000).unwrap();
    assert_eq!(lib.pending_scrobbles("oscar").unwrap().len(), 1);
    assert!(lib.all_tracks().unwrap().is_empty());
}

#[test]
fn a_song_on_two_servers_scrobbles_to_both() {
    let (lib, _db) = temp_lib();
    pull(&lib, "oscar", &[song("s1", "/music/A/01.mp3", "One")]);
    pull(&lib, "server2", &[song("x1", "/data/A/01.mp3", "One")]);
    let a = lib.server_songs("oscar").unwrap()[0].id;
    let b = lib.server_songs("server2").unwrap()[0].id;
    lib.link_copies(Member::Server(a), Member::Server(b), LinkReason::Tags).unwrap();
    lib.record_play_at(&crate::servers::uri::song_uri("oscar", "/music/A/01.mp3"), 5).unwrap();
    assert_eq!(lib.pending_scrobbles("oscar").unwrap(), vec![("s1".to_string(), 5)]);
    assert_eq!(lib.pending_scrobbles("server2").unwrap(), vec![("x1".to_string(), 5)]);
}

#[test]
fn an_unlinked_local_file_queues_nothing() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, _servers) = linked_setup(&lib);
    let path = lib.tracks_by_ids(&[locals[0]]).unwrap()[&locals[0]].path.clone();
    lib.record_play_at(&path, 5).unwrap();
    assert!(lib.pending_scrobbles("oscar").unwrap().is_empty());
}

#[test]
fn a_queued_scrobble_follows_the_song_to_its_new_id() {
    let (lib, _db) = temp_lib();
    pull(&lib, "oscar", &[song("s1", "/music/A/01.mp3", "One")]);
    lib.record_play_at(&crate::servers::uri::song_uri("oscar", "/music/A/01.mp3"), 5).unwrap();
    pull(&lib, "oscar", &[song("s1-retagged", "/music/A/01.mp3", "One!")]);
    assert_eq!(lib.pending_scrobbles("oscar").unwrap(), vec![("s1-retagged".to_string(), 5)]);
}

#[test]
fn only_the_latest_queued_rating_per_song_is_kept() {
    let (lib, _db) = temp_lib();
    pull(&lib, "oscar", &[song("s1", "/music/A/01.mp3", "One")]);
    let id = lib.server_songs("oscar").unwrap()[0].id;
    lib.queue_rating(id, 2).unwrap();
    lib.queue_rating(id, 5).unwrap();
    assert_eq!(lib.pending_ratings("oscar").unwrap(), vec![(id, "s1".to_string(), 5)]);
}

#[test]
fn sent_items_leave_the_queue() {
    let (lib, _db) = temp_lib();
    pull(&lib, "oscar", &[song("s1", "/music/A/01.mp3", "One")]);
    let id = lib.server_songs("oscar").unwrap()[0].id;
    let uri = crate::servers::uri::song_uri("oscar", "/music/A/01.mp3");
    lib.record_play_at(&uri, 5).unwrap();
    lib.queue_rating(id, 4).unwrap();
    lib.clear_pending_scrobbles("oscar").unwrap();
    lib.clear_pending_rating(id).unwrap();
    assert!(lib.pending_scrobbles("oscar").unwrap().is_empty());
    assert!(lib.pending_ratings("oscar").unwrap().is_empty());
}

// ── the merged library list ───────────────────────────────────────────────

use crate::media_library::servers::{LibraryRow, SourceFilter};

/// Three songs: "A" local only, "B" local + oscar (linked), "C" oscar only.
fn three_kinds(lib: &MediaLibrary) -> (tempfile::TempDir, i64, i64, i64) {
    let dir = temp_dir_with_files("mp3", 2);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    let ids: Vec<i64> = lib.all_tracks().unwrap().iter().map(|t| t.id).collect();
    for (id, artist) in ids.iter().zip(["A", "B"]) {
        lib.conn
            .execute(
                "UPDATE tracks SET artist = ?1, title = ?1, last_scanned = '2026-09-30' WHERE id = ?2",
                rusqlite::params![artist, id],
            )
            .unwrap();
    }
    let mut b = song("sb", "/music/B/B.mp3", "B");
    b.artist = "B".into();
    let mut c = song("sc", "/music/C/C.mp3", "Two");
    c.artist = "C".into();
    pull(lib, "oscar", &[b, c]);
    let rows = lib.server_songs("oscar").unwrap();
    lib.link_copies(Member::Local(ids[1]), Member::Server(rows[0].id), LinkReason::Tags).unwrap();
    lib.accept_differences(Member::Local(ids[1])).unwrap();
    (dir, ids[0], ids[1], rows[1].id)
}

fn artists(rows: &[LibraryRow]) -> Vec<String> {
    rows.iter().map(|r| r.track.artist.clone().unwrap_or_default()).collect()
}

#[test]
fn the_full_list_merges_local_and_server_only_songs_in_sort_order() {
    let (lib, _db) = temp_lib();
    let (_dir, a, b, c) = three_kinds(&lib);
    let rows = lib.library_rows(&SourceFilter::All, None, "artist", false).unwrap();
    assert_eq!(artists(&rows), vec!["A", "B", "C"]);
    assert_eq!(rows.iter().map(|r| r.track.id).collect::<Vec<_>>(), vec![a, b, -c]);
    assert_eq!((rows[0].has_local, rows[0].servers.clone()), (true, vec![]));
    assert_eq!((rows[1].has_local, rows[1].servers.clone()), (true, vec!["oscar".to_string()]));
    assert_eq!((rows[2].has_local, rows[2].servers.clone()), (false, vec!["oscar".to_string()]));
}

#[test]
fn descending_sort_applies_to_the_merged_list() {
    let (lib, _db) = temp_lib();
    let _k = three_kinds(&lib);
    let rows = lib.library_rows(&SourceFilter::All, None, "artist", true).unwrap();
    assert_eq!(artists(&rows), vec!["C", "B", "A"]);
}

#[test]
fn the_local_filter_shows_every_song_with_a_local_copy() {
    let (lib, _db) = temp_lib();
    let _k = three_kinds(&lib);
    let rows = lib.library_rows(&SourceFilter::Local, None, "artist", false).unwrap();
    assert_eq!(artists(&rows), vec!["A", "B"]);
}

#[test]
fn a_server_filter_shows_every_song_with_a_copy_on_that_server() {
    let (lib, _db) = temp_lib();
    let _k = three_kinds(&lib);
    let rows = lib.library_rows(&SourceFilter::Server("oscar".into()), None, "artist", false).unwrap();
    assert_eq!(artists(&rows), vec!["B", "C"]);
    assert!(lib.library_rows(&SourceFilter::Server("server2".into()), None, "artist", false).unwrap().is_empty());
}

#[test]
fn local_changes_are_local_only_songs_and_songs_ahead_of_a_server() {
    let (lib, _db) = temp_lib();
    let (_dir, _a, b, _c) = three_kinds(&lib);
    assert_eq!(artists(&lib.library_rows(&SourceFilter::LocalChanges, None, "artist", false).unwrap()), vec!["A"]);
    lib.conn.execute("UPDATE tracks SET genre = 'Jazz' WHERE id = ?1", [b]).unwrap();
    let rows = lib.library_rows(&SourceFilter::LocalChanges, None, "artist", false).unwrap();
    assert_eq!(artists(&rows), vec!["A", "B"]);
    assert_eq!(rows[1].status, SongStatus::LocalChanged);
}

#[test]
fn needs_attention_shows_server_changes_and_possible_matches() {
    let (lib, _db) = temp_lib();
    let (_dir, a, _b, c) = three_kinds(&lib);
    assert!(lib.library_rows(&SourceFilter::NeedsAttention, None, "artist", false).unwrap().is_empty());
    let mut b = song("sb", "/music/B/B.mp3", "B (Live)");
    b.artist = "B".into();
    let mut cc = song("sc", "/music/C/C.mp3", "Two");
    cc.artist = "C".into();
    pull(&lib, "oscar", &[b, cc]);
    lib.record_possible_matches(
        "oscar",
        &[crate::servers::matcher::PossibleMatch { server: c, candidates: vec![a] }],
    )
    .unwrap();
    let rows = lib.library_rows(&SourceFilter::NeedsAttention, None, "artist", false).unwrap();
    assert_eq!(artists(&rows), vec!["B", "C"]);
    assert_eq!(rows[0].status, SongStatus::ServerChanged);
    assert!(rows[1].possible_match);
}

#[test]
fn search_finds_server_only_songs_too() {
    let (lib, _db) = temp_lib();
    let _k = three_kinds(&lib);
    let rows = lib.library_rows(&SourceFilter::All, Some("two"), "artist", false).unwrap();
    assert_eq!(artists(&rows), vec!["C"]);
}

#[test]
fn without_servers_the_list_is_the_plain_library() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 3);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    let plain: Vec<i64> = lib.all_tracks_sorted("title", false).unwrap().iter().map(|t| t.id).collect();
    let merged: Vec<i64> = lib
        .library_rows(&SourceFilter::All, None, "title", false)
        .unwrap()
        .iter()
        .map(|r| r.track.id)
        .collect();
    assert_eq!(merged, plain);
}

#[test]
fn playing_a_server_song_never_adds_it_to_the_library_as_a_file() {
    let (lib, _db) = temp_lib();
    let uri = crate::servers::uri::song_uri("oscar", "/music/A/01.mp3");
    assert!(!lib.add_played_track(&uri).unwrap());
    assert!(lib.all_tracks().unwrap().is_empty());
}

#[test]
fn adding_a_server_song_to_the_playlist_uses_its_server_metadata() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    let uri = crate::servers::uri::song_uri("oscar", "/music/A/02.mp3");
    let rows = crate::playlist_ingest::resolve(Some(&lib), &[std::path::PathBuf::from(&uri)]);
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].needs_tags, "nothing to read from disk");
    assert_eq!(rows[0].track.title, "Two");
    assert_eq!(rows[0].track.path, std::path::PathBuf::from(&uri));

    // With a local copy, the playlist gets the local file.
    lib.link_copies(Member::Local(locals[0]), Member::Server(servers[1]), LinkReason::Manual).unwrap();
    let rows = crate::playlist_ingest::resolve(Some(&lib), &[std::path::PathBuf::from(&uri)]);
    let local_path = lib.tracks_by_ids(&[locals[0]]).unwrap()[&locals[0]].path.clone();
    assert_eq!(rows[0].track.path, std::path::PathBuf::from(local_path));
}

#[test]
fn forgetting_a_server_drops_its_catalog_links_and_queue_but_no_files() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    lib.link_copies(Member::Local(locals[0]), Member::Server(servers[0]), LinkReason::Tags).unwrap();
    lib.record_play_at(&crate::servers::uri::song_uri("oscar", "/music/A/02.mp3"), 5).unwrap();
    pull(&lib, "server2", &[song("x1", "/data/A/01.mp3", "Other")]);

    lib.forget_server("oscar").unwrap();

    assert!(lib.server_songs("oscar").unwrap().is_empty());
    assert_eq!(lib.song_copies(Member::Local(locals[0])).unwrap(), None);
    assert!(lib.pending_scrobbles("oscar").unwrap().is_empty());
    assert_eq!(lib.server_last_scan("oscar").unwrap(), None);
    assert_eq!(lib.all_tracks().unwrap().len(), 2, "local files untouched");
    assert_eq!(lib.server_songs("server2").unwrap().len(), 1, "other servers untouched");
}

// ── applying server changes to local files ────────────────────────────────

/// A real WAV tagged "Old Title" in a watched folder, scanned, linked to an
/// oscar copy that agreed with it and has since been retagged "New Title".
fn server_retagged(lib: &MediaLibrary) -> (tempfile::TempDir, i64, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap().join("song.wav");
    write_test_wav(&path, 44100, 2, 1.0);
    let mut tags = crate::id3_editor::read_tag_fields(&path);
    tags.title = "Old Title".into();
    tags.artist = "Artist".into();
    crate::id3_editor::write_tag_fields(&path, &tags).unwrap();
    let root = dir.path().canonicalize().unwrap();
    let folder = lib.add_folder(root.to_str().unwrap()).unwrap().id();
    lib.rescan_folder_fast(folder, root.to_str().unwrap(), true).unwrap();
    let path_str = path.to_string_lossy().into_owned();
    lib.rescan_track(&path_str).unwrap();
    let local = lib.all_tracks().unwrap()[0].id;
    assert_eq!(lib.tracks_by_ids(&[local]).unwrap()[&local].title.as_deref(), Some("Old Title"));

    let mut s = ServerSong {
        id: "s1".into(),
        path: Some("/music/song.wav".into()),
        title: "Old Title".into(),
        artist: "Artist".into(),
        ..ServerSong::default()
    };
    pull(lib, "oscar", &[s.clone()]);
    let server = lib.server_songs("oscar").unwrap()[0].id;
    lib.link_copies(Member::Local(local), Member::Server(server), LinkReason::Tags).unwrap();
    lib.accept_differences(Member::Local(local)).unwrap();
    s.title = "New Title".into();
    s.user_rating = 4;
    pull(lib, "oscar", &[s]);
    (dir, local, path_str)
}

#[test]
fn applying_server_changes_writes_them_into_the_local_file() {
    let (lib, _db) = temp_lib();
    let (_dir, local, path) = server_retagged(&lib);
    assert_eq!(status(&lib, local), SongStatus::ServerChanged);

    let outcome = crate::servers::apply::apply_server_changes(&lib, Member::Local(local)).unwrap();

    assert_eq!(outcome.taken, vec![Field::Title]);
    assert_eq!(crate::id3_editor::read_tag_fields(std::path::Path::new(&path)).title, "New Title");
    assert_eq!(lib.tracks_by_ids(&[local]).unwrap()[&local].title.as_deref(), Some("New Title"));
}

/// A WAV has no rating tag, and a rating must reach the file first, so the
/// server's rating is refused and changes nothing; the title still applies.
#[test]
fn a_rating_the_file_cannot_hold_is_refused_and_left_unchanged() {
    let (lib, _db) = temp_lib();
    let (_dir, local, _path) = server_retagged(&lib);
    let outcome = crate::servers::apply::apply_server_changes(&lib, Member::Local(local)).unwrap();
    assert_eq!(outcome.refused.iter().map(|(f, _)| *f).collect::<Vec<_>>(), vec![Field::Rating]);
    assert_eq!(lib.local_rating(local).unwrap(), 0);
    assert_eq!(status(&lib, local), SongStatus::ServerChanged, "the rating still differs");
}

#[test]
fn a_server_rating_goes_into_the_local_file_then_the_library() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    lib.link_copies(Member::Local(locals[0]), Member::Server(servers[0]), LinkReason::Tags).unwrap();
    lib.accept_differences(Member::Local(locals[0])).unwrap();
    let mut rated = song("s1", "/music/A/01.mp3", "One");
    rated.user_rating = 4;
    pull(&lib, "oscar", &[rated, song("s2", "/music/A/02.mp3", "Two")]);

    let outcome = crate::servers::apply::apply_server_changes(&lib, Member::Local(locals[0])).unwrap();

    assert_eq!(outcome.taken, vec![Field::Rating]);
    let path = lib.tracks_by_ids(&[locals[0]]).unwrap()[&locals[0]].path.clone();
    assert_eq!(crate::rating::read_rating(std::path::Path::new(&path)), Some(4));
    assert_eq!(lib.local_rating(locals[0]).unwrap(), 4);
    assert_eq!(status(&lib, locals[0]), SongStatus::InSync);
}

#[test]
fn the_last_apply_can_be_undone() {
    let (lib, _db) = temp_lib();
    let (_dir, local, path) = server_retagged(&lib);
    crate::servers::apply::apply_server_changes(&lib, Member::Local(local)).unwrap();

    assert_eq!(crate::servers::apply::undo_last_apply(&lib).unwrap(), 1);

    assert_eq!(crate::id3_editor::read_tag_fields(std::path::Path::new(&path)).title, "Old Title");
    assert_eq!(lib.tracks_by_ids(&[local]).unwrap()[&local].title.as_deref(), Some("Old Title"));
    assert_eq!(status(&lib, local), SongStatus::ServerChanged, "back where it was");
    assert_eq!(crate::servers::apply::undo_last_apply(&lib).unwrap(), 0, "one level of undo");
}

#[test]
fn a_song_with_nothing_to_take_is_left_alone() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    lib.link_copies(Member::Local(locals[0]), Member::Server(servers[0]), LinkReason::Tags).unwrap();
    lib.accept_differences(Member::Local(locals[0])).unwrap();
    assert!(crate::servers::apply::apply_server_changes(&lib, Member::Local(locals[0])).unwrap().taken.is_empty());
}

// ── album gallery ─────────────────────────────────────────────────────────

#[test]
fn albums_count_linked_songs_once_and_include_server_only_songs() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 1);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    let local = lib.all_tracks().unwrap()[0].id;
    lib.conn
        .execute("UPDATE tracks SET album = 'Kind of Blue', artist = 'Miles', album_artist = 'Miles' WHERE id = ?1", [local])
        .unwrap();
    let tune = |id: &str, path: &str, album: &str| ServerSong {
        id: id.into(),
        path: Some(path.into()),
        title: id.into(),
        artist: "Miles".into(),
        album: album.into(),
        album_artist: "Miles".into(),
        ..ServerSong::default()
    };
    pull(&lib, "oscar", &[
        tune("so-what", "/m/1.mp3", "Kind of Blue"),
        tune("blue-in-green", "/m/2.mp3", "Kind of Blue"),
        tune("milestones", "/m/3.mp3", "Milestones"),
    ]);
    let linked = lib.server_songs("oscar").unwrap()[0].id;
    lib.link_copies(Member::Local(local), Member::Server(linked), LinkReason::Tags).unwrap();

    let albums = lib.albums(crate::media_library::AlbumSort::Album, false).unwrap();
    let counts: Vec<(String, i64)> = albums.iter().map(|g| (g.album.clone(), g.track_count)).collect();
    assert_eq!(counts, vec![("Kind of Blue".to_string(), 2), ("Milestones".to_string(), 1)]);

    let tracks = lib.album_tracks("Kind of Blue", "Miles", false).unwrap();
    assert_eq!(tracks.len(), 2);
    assert!(tracks.iter().any(|t| t.id == local));
    assert!(tracks.iter().any(|t| t.id < 0 && t.title.as_deref() == Some("blue-in-green")));
}

/// Two local files and oscar's three songs: "Kind of Blue" has a linked
/// song and a server-only one, "Milestones" is only on oscar, "Sketches"
/// only here.
fn jazz_library(lib: &MediaLibrary) -> (tempfile::TempDir, Vec<i64>) {
    let dir = temp_dir_with_files("mp3", 2);
    let path = dir.path().to_str().unwrap();
    let folder_id = lib.add_folder(path).unwrap().id();
    lib.rescan_folder_fast(folder_id, path, true).unwrap();
    let locals: Vec<i64> = lib.all_tracks().unwrap().iter().map(|t| t.id).collect();
    for (id, album) in locals.iter().zip(["Kind of Blue", "Sketches"]) {
        lib.conn
            .execute(
                "UPDATE tracks SET album = ?1, artist = 'Miles', album_artist = 'Miles' WHERE id = ?2",
                rusqlite::params![album, id],
            )
            .unwrap();
    }
    let tune = |id: &str, path: &str, album: &str| ServerSong {
        id: id.into(),
        path: Some(path.into()),
        title: id.into(),
        artist: "Miles".into(),
        album: album.into(),
        album_artist: "Miles".into(),
        ..ServerSong::default()
    };
    pull(lib, "oscar", &[
        tune("so-what", "/m/1.mp3", "Kind of Blue"),
        tune("blue-in-green", "/m/2.mp3", "Kind of Blue"),
        tune("milestones", "/m/3.mp3", "Milestones"),
    ]);
    let linked = lib.server_songs("oscar").unwrap()[0].id;
    lib.link_copies(Member::Local(locals[0]), Member::Server(linked), LinkReason::Tags).unwrap();
    (dir, locals)
}

/// (album, songs, songs here, songs on a server) for each album.
fn album_spread(albums: &[crate::media_library::AlbumGroup]) -> Vec<(String, i64, i64, i64)> {
    albums.iter().map(|g| (g.album.clone(), g.track_count, g.local_songs, g.server_songs)).collect()
}

#[test]
fn each_album_counts_its_songs_here_and_on_servers() {
    let (lib, _db) = temp_lib();
    let (_dir, _) = jazz_library(&lib);
    let albums = lib.albums(crate::media_library::AlbumSort::Album, false).unwrap();
    assert_eq!(
        album_spread(&albums),
        vec![
            ("Kind of Blue".to_string(), 2, 1, 2),
            ("Milestones".to_string(), 1, 0, 1),
            ("Sketches".to_string(), 1, 1, 0),
        ]
    );
}

#[test]
fn the_source_filters_narrow_the_albums_to_the_songs_they_list() {
    use crate::media_library::{servers::SourceFilter, AlbumSort};
    let (lib, _db) = temp_lib();
    let (_dir, _) = jazz_library(&lib);
    let spread = |filter: SourceFilter| album_spread(&lib.albums_in(AlbumSort::Album, false, &filter).unwrap());
    let row = |album: &str, songs, here, there| (album.to_string(), songs, here, there);

    assert_eq!(spread(SourceFilter::All), album_spread(&lib.albums(AlbumSort::Album, false).unwrap()));
    assert_eq!(spread(SourceFilter::Local), vec![row("Kind of Blue", 1, 1, 1), row("Sketches", 1, 1, 0)]);
    assert_eq!(
        spread(SourceFilter::Server("oscar".into())),
        vec![row("Kind of Blue", 2, 1, 2), row("Milestones", 1, 0, 1)]
    );
    assert_eq!(spread(SourceFilter::Server("elsewhere".into())), vec![]);
    assert_eq!(spread(SourceFilter::LocalChanges), vec![row("Sketches", 1, 1, 0)]);
    assert_eq!(
        spread(SourceFilter::NeedsAttention),
        vec![row("Kind of Blue", 1, 1, 1)],
        "the linked pair's tags differ on first link"
    );
}

#[test]
fn an_albums_songs_follow_the_filter_and_say_where_they_are() {
    use crate::media_library::servers::SourceFilter;
    let (lib, _db) = temp_lib();
    let (_dir, locals) = jazz_library(&lib);
    let songs = |filter: SourceFilter| -> Vec<(i64, bool, Vec<String>)> {
        lib.album_library_rows("kind of blue", "miles", false, &filter)
            .unwrap()
            .into_iter()
            .map(|r| (r.track.id.signum(), r.has_local, r.servers))
            .collect()
    };
    let mut all = songs(SourceFilter::All);
    all.sort();
    assert_eq!(all, vec![(-1, false, vec!["oscar".to_string()]), (1, true, vec!["oscar".to_string()])]);
    let local = lib.album_library_rows("Kind of Blue", "Miles", false, &SourceFilter::Local).unwrap();
    assert_eq!(local.iter().map(|r| r.track.id).collect::<Vec<_>>(), vec![locals[0]]);
}

#[test]
fn a_rows_indicator_says_where_its_song_is() {
    use crate::media_library::servers::SourceFilter;
    use crate::servers::indicator::icon_name;
    let (lib, _db) = temp_lib();
    let (_dir, locals) = jazz_library(&lib);
    let rows = lib.library_rows(&SourceFilter::All, None, "title", false).unwrap();
    let icon = |pred: &dyn Fn(&crate::media_library::servers::LibraryRow) -> bool| {
        icon_name(&rows.iter().find(|r| pred(r)).unwrap().indicator())
    };
    assert_eq!(icon(&|r| r.track.id == locals[1]), Some("local"));
    assert_eq!(icon(&|r| r.track.title.as_deref() == Some("milestones")), Some("server"));
    // The linked song: oscar has tags the local file lacks.
    assert_eq!(icon(&|r| r.track.id == locals[0]), Some("server-newer"));
}

#[test]
fn rows_by_id_include_server_only_songs() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    let found = lib.library_tracks_by_ids(&[locals[0], -servers[1], -999]).unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[&-servers[1]].title.as_deref(), Some("Two"));
    assert_eq!(found[&-servers[1]].path, crate::servers::uri::song_uri("oscar", "/music/A/02.mp3"));
}

// ── ratings live in the file ──────────────────────────────────────────────

#[test]
fn a_scan_reads_the_rating_from_the_file() {
    let (lib, _db) = temp_lib();
    let dir = temp_dir_with_files("mp3", 1);
    let root = dir.path().canonicalize().unwrap();
    let folder = lib.add_folder(root.to_str().unwrap()).unwrap().id();
    lib.rescan_folder_fast(folder, root.to_str().unwrap(), true).unwrap();
    let track = lib.all_tracks().unwrap()[0].clone();

    crate::rating::write_rating(std::path::Path::new(&track.path), 4).unwrap();
    lib.rescan_track(&track.path).unwrap();
    assert_eq!(lib.local_rating(track.id).unwrap(), 4);

    crate::rating::write_rating(std::path::Path::new(&track.path), 0).unwrap();
    lib.rescan_track(&track.path).unwrap();
    assert_eq!(lib.local_rating(track.id).unwrap(), 0, "the file is the source of truth");
}

/// A library made by an earlier build has the server tables without the
/// columns added since. Opening it upgrades them in place; it used to fail
/// every update with "no such column: album_id".
#[test]
fn server_tables_from_an_earlier_build_are_upgraded_in_place() {
    let db = tempfile::NamedTempFile::with_suffix(".db").unwrap();
    {
        let conn = rusqlite::Connection::open(db.path()).unwrap();
        conn.execute_batch(
            "CREATE TABLE server_state (
                 server_id TEXT PRIMARY KEY, pull_seq INTEGER NOT NULL DEFAULT 0, last_scan TEXT,
                 last_success_at TEXT, server_version TEXT, extensions TEXT);
             CREATE TABLE server_tracks (
                 id INTEGER PRIMARY KEY, server_id TEXT NOT NULL, path_key TEXT NOT NULL, path TEXT,
                 song_id TEXT NOT NULL, title TEXT NOT NULL DEFAULT '', artist TEXT NOT NULL DEFAULT '',
                 album TEXT NOT NULL DEFAULT '', album_artist TEXT NOT NULL DEFAULT '',
                 genre TEXT NOT NULL DEFAULT '', comment TEXT NOT NULL DEFAULT '', track_num INTEGER,
                 disc_num INTEGER, year INTEGER, bpm INTEGER, length_secs INTEGER, file_size INTEGER,
                 suffix TEXT, bitrate INTEGER, cover_art TEXT, rating INTEGER NOT NULL DEFAULT 0,
                 play_count INTEGER NOT NULL DEFAULT 0, played TEXT, musicbrainz_id TEXT,
                 isrc TEXT NOT NULL DEFAULT '', rg_track_gain REAL, rg_track_peak REAL,
                 rg_album_gain REAL, rg_album_peak REAL, UNIQUE (server_id, path_key));
             INSERT INTO server_tracks (server_id, path_key, song_id, title)
                 VALUES ('oscar', '/music/old.mp3', 's0', 'Old');",
        )
        .unwrap();
    }
    let lib = MediaLibrary::open_at(db.path()).expect("an older library opens");
    let pull = lib.begin_server_pull("oscar").unwrap();
    lib.apply_server_songs(
        "oscar",
        pull,
        &[crate::servers::api::ServerSong {
            id: "s1".into(),
            title: "New".into(),
            path: Some("/music/new.mp3".into()),
            album_id: Some("al-1".into()),
            ..Default::default()
        }],
    )
    .unwrap();
    let titles: Vec<String> = lib.server_songs("oscar").unwrap().into_iter().map(|r| r.song.title).collect();
    assert!(titles.contains(&"New".to_string()) && titles.contains(&"Old".to_string()), "{titles:?}");
    lib.record_server_update_success("oscar", None).unwrap();
    assert!(lib.server_last_success("oscar").unwrap().is_some());
}

/// The tag editor shows a server song from the catalog: there is no file to
/// read, and it should not claim the file is missing.
#[test]
fn a_server_songs_tags_come_from_the_catalog() {
    let (lib, _db) = temp_lib();
    let pull = lib.begin_server_pull("tags-srv").unwrap();
    lib.apply_server_songs(
        "tags-srv",
        pull,
        &[ServerSong {
            id: "s9".into(),
            title: "'Til the End of Time".into(),
            artist: "Delerium".into(),
            album: "Karma [Enhanced]".into(),
            album_artist: "Delerium".into(),
            genre: "Electronic".into(),
            year: Some(1997),
            track: Some(11),
            disc: Some(1),
            path: Some("/music/Delerium/Karma [Enhanced]/11 - 'Til the End of Time.mp3".into()),
            ..Default::default()
        }],
    )
    .unwrap();
    let uri = crate::servers::uri::song_uri("tags-srv", "/music/Delerium/Karma [Enhanced]/11 - 'Til the End of Time.mp3");
    let song = crate::id3_editor::server_song_tags(&lib, &uri).expect("found in the catalog");
    assert_eq!(song.server_id, "tags-srv");
    assert_eq!(song.path, "/music/Delerium/Karma [Enhanced]/11 - 'Til the End of Time.mp3");
    let f = &song.fields;
    assert_eq!((f.title.as_str(), f.artist.as_str(), f.album.as_str()), ("'Til the End of Time", "Delerium", "Karma [Enhanced]"));
    assert_eq!((f.album_artist.as_str(), f.genre.as_str(), f.year.as_str()), ("Delerium", "Electronic", "1997"));
    assert_eq!((f.track_number.as_str(), f.disc_number.as_str()), ("11", "1"));
    assert!(crate::id3_editor::server_song_tags(&lib, "subsonic://tags-srv//music/nope.mp3").is_none());
}

// ── server playlists ──────────────────────────────────────────────────────

use crate::media_library::PlaylistSource;
use std::collections::HashMap;

fn server_playlist(id: &str, name: &str, changed: &str) -> crate::servers::api::ServerPlaylist {
    crate::servers::api::ServerPlaylist {
        id: id.into(),
        name: name.into(),
        owner: "josef".into(),
        changed: Some(changed.into()),
        ..Default::default()
    }
}

fn listed(lib: &MediaLibrary) -> Vec<(String, PlaylistSource)> {
    lib.listed_playlists().unwrap().into_iter().map(|p| (p.name, p.source)).collect()
}

fn server_playlist_id(lib: &MediaLibrary, name: &str) -> i64 {
    lib.listed_playlists().unwrap().into_iter().find(|p| p.name == name).expect(name).id
}

#[test]
fn server_playlists_are_listed_with_the_local_ones_by_name() {
    let (lib, _db) = temp_lib();
    let (dir, _, _) = linked_setup(&lib);
    let (mix, _) = temp_playlist(&lib, dir.path());
    lib.store_server_playlists(
        "oscar",
        &[server_playlist("p1", "Road Trip", "t1"), server_playlist("p2", "chill", "t1")],
        &HashMap::new(),
    )
    .unwrap();

    let oscar = PlaylistSource::Server("oscar".into());
    assert_eq!(
        listed(&lib),
        vec![("chill".into(), oscar.clone()), ("Mix".into(), PlaylistSource::Local), ("Road Trip".into(), oscar)]
    );
    assert_eq!(server_playlist_id(&lib, "Mix"), mix);
    assert!(server_playlist_id(&lib, "chill") < 0, "a server playlist's id never meets a file's");
    assert_eq!(lib.all_playlists().unwrap().len(), 1, "playlist files alone, as devices and saving expect");
}

#[test]
fn a_server_playlist_plays_each_song_from_its_best_copy() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    lib.link_copies(Member::Local(locals[0]), Member::Server(servers[0]), LinkReason::Tags).unwrap();
    let mut gone = song("gone", "/music/Gone/03.mp3", "Gone Song");
    gone.artist = "Someone".into();
    let songs = HashMap::from([(
        "p1".to_string(),
        vec![song("s1", "/music/A/01.mp3", "One"), song("s2", "/music/A/02.mp3", "Two"), gone],
    )]);
    lib.store_server_playlists("oscar", &[server_playlist("p1", "Road Trip", "t1")], &songs).unwrap();

    let pl = lib.playlist_by_id(server_playlist_id(&lib, "Road Trip")).unwrap();
    assert_eq!(pl.name, "Road Trip");
    let loaded = lib.load_playlist_tracks(&pl).unwrap();
    assert_eq!(
        loaded.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![locals[0], -servers[1], 0],
        "the local copy, the server copy, and a song the catalog lacks kept as missing"
    );
    assert_eq!(loaded[2].title.as_deref(), Some("Gone Song"));
    assert_eq!(loaded[2].artist.as_deref(), Some("Someone"));
    assert_eq!(loaded[2].path, crate::servers::uri::song_uri("oscar", "/music/Gone/03.mp3"));
}

#[test]
fn an_update_keeps_unchanged_playlists_and_drops_the_ones_the_server_lost() {
    let (lib, _db) = temp_lib();
    let (_dir, _, servers) = linked_setup(&lib);
    let songs = HashMap::from([
        ("p1".to_string(), vec![song("s2", "/music/A/02.mp3", "Two")]),
        ("p2".to_string(), vec![song("s1", "/music/A/01.mp3", "One")]),
    ]);
    lib.store_server_playlists(
        "oscar",
        &[server_playlist("p1", "Road Trip", "t1"), server_playlist("p2", "Chill", "t1")],
        &songs,
    )
    .unwrap();
    assert_eq!(
        lib.server_playlist_stamps("oscar").unwrap(),
        HashMap::from([("p1".to_string(), Some("t1".to_string())), ("p2".to_string(), Some("t1".to_string()))])
    );

    // p1 is renamed but its songs are not sent again, p2 is gone, p3 is new.
    lib.store_server_playlists(
        "oscar",
        &[server_playlist("p1", "Road Trip 2", "t1"), server_playlist("p3", "New", "t2")],
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(listed(&lib).into_iter().map(|(n, _)| n).collect::<Vec<_>>(), vec!["New", "Road Trip 2"]);
    let kept = lib.playlist_by_id(server_playlist_id(&lib, "Road Trip 2")).unwrap();
    assert_eq!(lib.load_playlist_tracks(&kept).unwrap().iter().map(|t| t.id).collect::<Vec<_>>(), vec![-servers[1]]);
    assert_eq!(
        lib.server_playlist_stamps("oscar").unwrap().get("p3"),
        Some(&None),
        "no songs yet, so the next update asks for them"
    );
}

#[test]
fn a_server_playlist_cannot_be_changed_from_here() {
    let (lib, _db) = temp_lib();
    let (_dir, locals, servers) = linked_setup(&lib);
    let songs = HashMap::from([("p1".to_string(), vec![song("s2", "/music/A/02.mp3", "Two")])]);
    lib.store_server_playlists("oscar", &[server_playlist("p1", "Road Trip", "t1")], &songs).unwrap();
    let id = server_playlist_id(&lib, "Road Trip");

    assert!(lib.rename_playlist(id, "Other").is_err());
    assert!(lib.save_playlist_tracks(id, &[locals[0]]).is_err());
    assert!(lib.append_paths_to_playlist(id, &["/tmp/x.mp3".to_string()]).is_err());
    assert!(lib.remove_playlist(id).is_err());
    assert!(!lib.playlist_is_writable(id));
    assert!(!lib.playlist_is_managed(id));
    let pl = lib.playlist_by_id(id).unwrap();
    assert_eq!(pl.name, "Road Trip");
    assert_eq!(lib.load_playlist_tracks(&pl).unwrap().iter().map(|t| t.id).collect::<Vec<_>>(), vec![-servers[1]]);
}

#[test]
fn forgetting_a_server_forgets_its_playlists() {
    let (lib, _db) = temp_lib();
    lib.store_server_playlists("oscar", &[server_playlist("p1", "Road Trip", "t1")], &HashMap::new()).unwrap();
    lib.store_server_playlists("other", &[server_playlist("p1", "Theirs", "t1")], &HashMap::new()).unwrap();
    lib.forget_server("oscar").unwrap();
    assert_eq!(listed(&lib), vec![("Theirs".to_string(), PlaylistSource::Server("other".into()))]);
}
