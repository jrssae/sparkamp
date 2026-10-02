//! Three servers and a local library together, over real HTTP: what each
//! server feature does when more than one server is set up.
//!
//! The world, built fresh for each test:
//!
//! - Local files `Artist/Album/01 Intro.mp3`, `02 Shared.mp3`,
//!   `05 Local Only.mp3`, tagged as the servers tag them.
//! - alpha (priority 0): Intro and Shared (linked to the local files by
//!   path), Alpha Only, and Everywhere. Playlists "Road Trip" and
//!   "Shared Name".
//! - beta (priority 1): Shared (linked by path too), Beta Only, Everywhere.
//!   Playlist "Shared Name".
//! - gamma (priority 2): Everywhere, Gamma Only. Playlist "Gamma Mix".
//!
//! Everywhere has no local copy and is on all three servers under different
//! roots. Each server has its own user and password.

use super::client::ServerClient;
use super::fake_http::{song, FakeSubsonic};
use super::manager::{MemorySecrets, SecretStore, ServerManager};
use super::playback::{Readiness, SongSource};
use super::request::Credentials;
use super::status::Health;
use super::sync::{rate_song, UpdateError};
use super::transport::MinreqTransport;
use crate::config::{ServerConfig, ServerSyncConfig};
use crate::media_library::servers::{LibraryRow, Member, SourceFilter};
use crate::media_library::{AlbumSort, MediaLibrary, PlaylistSource};
use std::collections::HashMap;
use std::path::PathBuf;

struct World {
    dir: tempfile::TempDir,
    db: PathBuf,
    alpha: FakeSubsonic,
    beta: FakeSubsonic,
    gamma: FakeSubsonic,
    configs: Vec<ServerConfig>,
    manager: ServerManager<MinreqTransport>,
}

const SERVERS: [(&str, &str, &str); 3] = [("alpha", "ann", "a-pw"), ("beta", "bob", "b-pw"), ("gamma", "cat", "c-pw")];

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("music/Artist/Album");
        std::fs::create_dir_all(&album).unwrap();
        for (file, title) in [("01 Intro.mp3", "Intro"), ("02 Shared.mp3", "Shared"), ("05 Local Only.mp3", "Local Only")] {
            tagged_mp3(&album.join(file), title, "Artist", "Album");
        }
        let db = dir.path().join("library.db");
        let lib = MediaLibrary::open_at(&db).unwrap();
        let root = dir.path().join("music").canonicalize().unwrap();
        let root = root.to_str().unwrap();
        let folder = lib.add_folder(root).unwrap().id();
        lib.rescan_folder(folder, root, true).unwrap();

        let [alpha, beta, gamma] = SERVERS.map(|(_, user, pw)| FakeSubsonic::start(user, pw));
        alpha.set_songs(vec![
            song("a1", "Intro", "Artist", "Album", "/music/Artist/Album/01 Intro.mp3"),
            song("a2", "Shared", "Artist", "Album", "/music/Artist/Album/02 Shared.mp3"),
            song("a3", "Alpha Only", "Artist", "Album", "/music/Artist/Album/03 Alpha Only.mp3"),
            song("a4", "Everywhere", "Trio", "Live", "/music/Trio/Live/01 Everywhere.mp3"),
        ]);
        alpha.add_playlist("ap1", "Road Trip", "2026-09-01T00:00:00Z", &["a1", "a3", "a4"]);
        alpha.add_playlist("ap2", "Shared Name", "2026-09-01T00:00:00Z", &["a2"]);
        beta.set_songs(vec![
            song("b1", "Shared", "Artist", "Album", "/srv/Artist/Album/02 Shared.mp3"),
            song("b2", "Beta Only", "Beta", "Beta Album", "/srv/Beta/Beta Album/01 Beta Only.mp3"),
            song("b3", "Everywhere", "Trio", "Live", "/srv/Trio/Live/01 Everywhere.mp3"),
        ]);
        beta.add_playlist("bp1", "Shared Name", "2026-09-02T00:00:00Z", &["b2", "b1"]);
        gamma.set_songs(vec![
            song("g1", "Everywhere", "Trio", "Live", "/g/Trio/Live/01 Everywhere.mp3"),
            song("g2", "Gamma Only", "Gamma", "G", "/g/Gamma/G/01 Gamma Only.mp3"),
        ]);
        gamma.add_playlist("gp1", "Gamma Mix", "2026-09-03T00:00:00Z", &["g2", "g1"]);

        let secrets = MemorySecrets::default();
        let configs: Vec<ServerConfig> = [&alpha, &beta, &gamma]
            .iter()
            .zip(SERVERS)
            .enumerate()
            .map(|(i, (fake, (id, user, pw)))| {
                secrets.set(id, pw).unwrap();
                ServerConfig {
                    id: id.into(),
                    name: id.to_uppercase(),
                    lan_url: Some(fake.base.clone()),
                    username: user.into(),
                    enabled: true,
                    priority: i as u32,
                    ..ServerConfig::default()
                }
            })
            .collect();
        let manager = ServerManager::new(
            db.clone(),
            &configs,
            ServerSyncConfig::default(),
            &secrets,
            |_| MinreqTransport,
        );
        World { dir, db, alpha, beta, gamma, configs, manager }
    }

    /// All three servers up and updated.
    fn updated() -> Self {
        let w = World::new();
        for (id, result) in w.manager.refresh(None) {
            result.unwrap_or_else(|e| panic!("{id}: {e}"));
        }
        w
    }

    fn lib(&self) -> MediaLibrary {
        MediaLibrary::open_at(&self.db).unwrap()
    }

    fn local(&self, filename: &str) -> i64 {
        self.lib().all_tracks().unwrap().into_iter().find(|t| t.filename == filename).expect(filename).id
    }

    fn server_row(&self, server: &str, song_id: &str) -> i64 {
        self.lib().server_songs(server).unwrap().into_iter().find(|r| r.song.id == song_id).expect(song_id).id
    }

    /// Clients of their own, as a frontend keeps for ratings.
    fn clients(&self) -> HashMap<String, ServerClient<MinreqTransport>> {
        self.configs
            .iter()
            .zip(SERVERS)
            .map(|(c, (_, user, pw))| {
                let creds = Credentials::Password { username: user.into(), password: pw.into() };
                (c.id.clone(), ServerClient::new(c.lan_url.clone(), None, creds, MinreqTransport))
            })
            .collect()
    }
}

/// One silent MPEG frame behind an ID3v2 tag: enough for the scan to read
/// the tags.
fn tagged_mp3(path: &std::path::Path, title: &str, artist: &str, album: &str) {
    use id3::TagLike;
    let mut frame = vec![0xFFu8, 0xFB, 0x90, 0x00];
    frame.resize(417, 0);
    std::fs::write(path, frame).unwrap();
    let mut tag = id3::Tag::new();
    tag.set_title(title);
    tag.set_artist(artist);
    tag.set_album(album);
    tag.set_album_artist(artist);
    tag.write_to_path(path, id3::Version::Id3v24).unwrap();
}

/// `(title or file name, has a local copy, servers)` of each listed row, by
/// title.
fn rows(lib: &MediaLibrary, filter: SourceFilter) -> Vec<(String, bool, Vec<String>)> {
    let mut out: Vec<_> = lib
        .library_rows(&filter, None, "title", false)
        .unwrap()
        .into_iter()
        .map(|r: LibraryRow| {
            let name = r.track.title.clone().unwrap_or_else(|| r.track.filename.clone());
            (name, r.has_local, r.servers)
        })
        .collect();
    out.sort();
    out
}

fn ids(servers: &[&str]) -> Vec<String> {
    servers.iter().map(|s| s.to_string()).collect()
}

#[test]
fn each_server_updates_with_its_own_password_and_one_down_stops_none_of_the_others() {
    let w = World::new();
    w.gamma.set_down(true);

    let results: HashMap<String, Result<_, UpdateError>> = w.manager.refresh(None).into_iter().collect();
    assert!(results["alpha"].is_ok() && results["beta"].is_ok(), "{results:?}");
    assert!(matches!(&results["gamma"], Err(UpdateError::Server(e)) if e.is_offline()), "{results:?}");
    assert_eq!(w.manager.health("alpha"), Health::Online);
    assert!(matches!(w.manager.health("gamma"), Health::Offline { .. }));
    for (fake, user) in [(&w.alpha, "ann"), (&w.beta, "bob")] {
        let pulls = fake.calls("search3");
        assert!(!pulls.is_empty() && pulls.iter().all(|q| q.contains(&format!("u={user}"))), "{pulls:?}");
    }
    let lines = w.manager.status_lines();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].starts_with("ALPHA: updated"), "{lines:?}");
    assert!(lines[1].starts_with("BETA: updated"), "{lines:?}");
    assert!(lines[2].starts_with("GAMMA: not responding"), "{lines:?}");
    assert!(w.lib().server_songs("gamma").unwrap().is_empty());

    w.gamma.set_down(false);
    w.manager.refresh(Some("gamma"))[0].1.as_ref().unwrap();
    assert_eq!(w.lib().server_songs("gamma").unwrap().len(), 2);
    assert_eq!(w.lib().server_songs("alpha").unwrap().len(), 4, "the others keep their catalogs");
}

#[test]
fn a_song_is_one_row_however_many_servers_hold_it_and_each_filter_lists_its_own() {
    let w = World::updated();
    let lib = w.lib();
    assert_eq!(
        rows(&lib, SourceFilter::All),
        vec![
            ("Alpha Only".into(), false, ids(&["alpha"])),
            ("Beta Only".into(), false, ids(&["beta"])),
            ("Everywhere".into(), false, ids(&["alpha", "beta", "gamma"])),
            ("Gamma Only".into(), false, ids(&["gamma"])),
            ("Intro".into(), true, ids(&["alpha"])),
            ("Local Only".into(), true, ids(&[])),
            ("Shared".into(), true, ids(&["alpha", "beta"])),
        ]
    );
    let titles = |filter| rows(&lib, filter).into_iter().map(|r| r.0).collect::<Vec<_>>();
    assert_eq!(titles(SourceFilter::Local), vec!["Intro", "Local Only", "Shared"]);
    assert_eq!(titles(SourceFilter::Server("beta".into())), vec!["Beta Only", "Everywhere", "Shared"]);
    assert_eq!(titles(SourceFilter::Server("gamma".into())), vec!["Everywhere", "Gamma Only"]);
    assert_eq!(titles(SourceFilter::LocalChanges), vec!["Local Only"]);
}

#[test]
fn albums_count_each_song_once_and_split_by_server() {
    let w = World::updated();
    let lib = w.lib();
    let spread = |filter: SourceFilter| -> Vec<(String, i64, i64, i64)> {
        lib.albums_in(AlbumSort::Album, false, &filter)
            .unwrap()
            .into_iter()
            .map(|g| (g.album, g.track_count, g.local_songs, g.server_songs))
            .collect()
    };
    let album = |name: &str, songs, here, there| (name.to_string(), songs, here, there);
    // "Album": three local files (two linked) and Alpha Only.
    assert_eq!(
        spread(SourceFilter::All),
        vec![album("Album", 4, 3, 3), album("Beta Album", 1, 0, 1), album("G", 1, 0, 1), album("Live", 1, 0, 1)]
    );
    assert_eq!(spread(SourceFilter::Server("beta".into())), vec![
        album("Album", 1, 1, 1),
        album("Beta Album", 1, 0, 1),
        album("Live", 1, 0, 1),
    ]);
    assert_eq!(spread(SourceFilter::Server("gamma".into())), vec![album("G", 1, 0, 1), album("Live", 1, 0, 1)]);
}

#[test]
fn every_servers_playlists_are_listed_and_play_from_the_best_copy() {
    let w = World::updated();
    let lib = w.lib();
    let listed: Vec<(String, PlaylistSource)> =
        lib.listed_playlists().unwrap().into_iter().map(|p| (p.name, p.source)).collect();
    let on = |s: &str| PlaylistSource::Server(s.into());
    assert_eq!(listed.len(), 4, "{listed:?}");
    assert_eq!(listed[0], ("Gamma Mix".into(), on("gamma")));
    assert_eq!(listed[1], ("Road Trip".into(), on("alpha")));
    let shared: Vec<&PlaylistSource> = listed.iter().filter(|p| p.0 == "Shared Name").map(|p| &p.1).collect();
    assert!(shared.contains(&&on("alpha")) && shared.contains(&&on("beta")), "same name on two servers: both listed");

    let id = |name: &str, source: PlaylistSource| {
        lib.listed_playlists().unwrap().into_iter().find(|p| p.name == name && p.source == source).unwrap().id
    };
    let tracks = |id| lib.load_playlist_tracks(&lib.playlist_by_id(id).unwrap()).unwrap();
    let road_trip = tracks(id("Road Trip", on("alpha")));
    assert_eq!(road_trip[0].id, w.local("01 Intro.mp3"), "a linked song plays its local copy");
    assert_eq!(road_trip[1].id, -w.server_row("alpha", "a3"));
    assert_eq!(road_trip[2].title.as_deref(), Some("Everywhere"));
    let beta_shared = tracks(id("Shared Name", on("beta")));
    assert_eq!(beta_shared.iter().map(|t| t.id).collect::<Vec<_>>(), vec![
        -w.server_row("beta", "b2"),
        w.local("02 Shared.mp3")
    ]);
}

#[test]
fn a_rating_and_a_play_reach_every_server_holding_the_song_and_wait_for_one_that_is_down() {
    let w = World::updated();
    let lib = w.lib();
    let shared = w.local("02 Shared.mp3");

    rate_song(&w.clients(), &lib, Member::Local(shared), 4).unwrap();
    let sent = |fake: &FakeSubsonic, id: &str| {
        let calls = fake.calls("setRating");
        calls.len() == 1 && calls[0].contains(&format!("id={id}&")) && calls[0].contains("rating=4")
    };
    assert!(sent(&w.alpha, "a2"), "{:?}", w.alpha.calls("setRating"));
    assert!(sent(&w.beta, "b1"), "{:?}", w.beta.calls("setRating"));
    assert!(w.gamma.calls("setRating").is_empty(), "gamma does not hold it");

    // Everywhere is on all three; gamma is down when it plays.
    w.gamma.set_down(true);
    let everywhere = lib
        .library_rows(&SourceFilter::All, Some("Everywhere"), "title", false)
        .unwrap()
        .remove(0)
        .track;
    lib.record_play_at(&everywhere.path, 1_759_400_000_000).unwrap();
    w.manager.refresh(None);
    assert!(w.alpha.calls("scrobble").iter().any(|q| q.contains("id=a4")), "{:?}", w.alpha.calls("scrobble"));
    assert!(w.beta.calls("scrobble").iter().any(|q| q.contains("id=b3")), "{:?}", w.beta.calls("scrobble"));
    assert_eq!(lib.pending_scrobbles("alpha").unwrap(), vec![]);
    assert_eq!(lib.pending_scrobbles("gamma").unwrap().len(), 1, "kept for when gamma is back");

    w.gamma.set_down(false);
    w.manager.refresh(Some("gamma"));
    assert!(w.gamma.calls("scrobble").iter().any(|q| q.contains("id=g1")));
    assert_eq!(lib.pending_scrobbles("gamma").unwrap(), vec![]);
}

#[test]
fn playback_takes_the_local_copy_else_the_first_server_that_answers() {
    let w = World::updated();
    let lib = w.lib();
    let source = w.manager.song_source(super::cache::PlaybackCache::new(w.dir.path().join("cache"), 1 << 24));
    let wait = |uri: &str| -> Readiness {
        for _ in 0..100 {
            match source.prepare(uri) {
                Readiness::Downloading | Readiness::Streaming(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(50))
                }
                done => return done,
            }
        }
        panic!("{uri} never finished downloading");
    };
    let uri_of = |title: &str| {
        lib.library_rows(&SourceFilter::All, Some(title), "title", false).unwrap().remove(0).track.path
    };
    let shared_on_beta = crate::servers::uri::song_uri("beta", "/srv/Artist/Album/02 Shared.mp3");

    match wait(&shared_on_beta) {
        Readiness::Ready(path) => assert!(path.ends_with("02 Shared.mp3"), "{path:?}"),
        other => panic!("{other:?}"),
    }
    assert!(w.beta.calls("stream").is_empty(), "a local copy needs no download");

    assert!(matches!(wait(&uri_of("Beta Only")), Readiness::Ready(_)));
    assert_eq!(w.beta.calls("stream").len(), 1);
    assert!(w.alpha.calls("stream").is_empty());

    // Everywhere: alpha comes first, but it is down, so beta serves it.
    w.alpha.set_down(true);
    assert!(matches!(wait(&uri_of("Everywhere")), Readiness::Ready(_)));
    assert!(w.beta.calls("stream").iter().any(|q| q.contains("id=b3")), "{:?}", w.beta.calls("stream"));
}

#[test]
fn covers_come_from_the_server_that_has_the_album() {
    let w = World::updated();
    assert!(w.beta.calls("getCoverArt").iter().any(|q| q.contains("Beta Album")), "{:?}", w.beta.calls("getCoverArt"));
    assert!(w.gamma.calls("getCoverArt").iter().any(|q| q.contains("al-G")), "{:?}", w.gamma.calls("getCoverArt"));
}

#[test]
fn forgetting_one_server_leaves_the_others_and_the_local_files_alone() {
    let w = World::updated();
    let lib = w.lib();
    lib.forget_server("beta").unwrap();

    let all = rows(&lib, SourceFilter::All);
    assert!(all.contains(&("Shared".into(), true, ids(&["alpha"]))), "{all:?}");
    assert!(all.contains(&("Everywhere".into(), false, ids(&["alpha", "gamma"]))), "{all:?}");
    assert!(!all.iter().any(|r| r.0 == "Beta Only"));
    let names: Vec<String> = lib.listed_playlists().unwrap().into_iter().map(|p| p.name).collect();
    assert_eq!(names, vec!["Gamma Mix", "Road Trip", "Shared Name"]);
    assert!(w.dir.path().join("music/Artist/Album/02 Shared.mp3").exists());
    assert_eq!(lib.all_tracks().unwrap().len(), 3);
}

#[test]
fn copies_unlinked_by_hand_stay_apart_through_the_next_update() {
    let w = World::updated();
    let lib = w.lib();
    let on_beta = w.server_row("beta", "b3");
    lib.unlink_copy(Member::Server(on_beta)).unwrap();
    for (id, result) in w.manager.refresh(None) {
        result.unwrap_or_else(|e| panic!("{id}: {e}"));
    }
    let everywhere: Vec<_> = rows(&lib, SourceFilter::All).into_iter().filter(|r| r.0 == "Everywhere").collect();
    assert_eq!(
        everywhere,
        vec![("Everywhere".into(), false, ids(&["alpha", "gamma"])), ("Everywhere".into(), false, ids(&["beta"]))]
    );
}
