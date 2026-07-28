//! Discs tab: CD-TEXT overlay fallback. gnudb/user tags win entirely when
//! present; CD-TEXT read off the disc fills in only on a total miss
//! (Winamp precedence — whole-entry fallback, no per-field gap-fill, no
//! toggle). See `App::apply_disc_tags_to_entries`.

use super::*;

fn fake_audio_drive(id: &str, toc: crate::disc::DiscToc) -> crate::disc::OpticalDrive {
    crate::disc::OpticalDrive {
        id: id.to_string(),
        label: "Test Drive".to_string(),
        media: crate::disc::MediaInfo {
            present: true,
            is_audio_cd: true,
            is_blank: false,
            rewritable: false,
            kind: crate::disc::MediaKind::Unknown,
            free_bytes: 0,
            capacity_bytes: 0,
        },
        toc: Some(toc),
        mount_path: None,
    }
}

/// A data disc: it still reports a readable TOC (so `selected_disc_identity`
/// returns `Some`), but `media.is_audio_cd` is false and its tracks aren't
/// audio — mirrors what real drives report for e.g. a burned data CD-R.
fn fake_data_drive(id: &str, toc: crate::disc::DiscToc) -> crate::disc::OpticalDrive {
    crate::disc::OpticalDrive {
        id: id.to_string(),
        label: "Test Drive".to_string(),
        media: crate::disc::MediaInfo {
            present: true,
            is_audio_cd: false,
            is_blank: false,
            rewritable: false,
            kind: crate::disc::MediaKind::Unknown,
            free_bytes: 0,
            capacity_bytes: 0,
        },
        toc: Some(toc),
        mount_path: None,
    }
}

fn data_disc_toc() -> crate::disc::DiscToc {
    crate::disc::DiscToc {
        tracks: vec![crate::disc::TocTrack {
            number: 1,
            start_frame: 150,
            is_audio: false,
        }],
        leadout_frame: 30_000,
    }
}

fn two_track_toc() -> crate::disc::DiscToc {
    crate::disc::DiscToc {
        tracks: vec![
            crate::disc::TocTrack {
                number: 1,
                start_frame: 150,
                is_audio: true,
            },
            crate::disc::TocTrack {
                number: 2,
                start_frame: 15_000,
                is_audio: true,
            },
        ],
        leadout_frame: 30_000,
    }
}

fn xmcd_with_titles(titles: &[&str]) -> crate::disc::xmcd::XmcdEntry {
    crate::disc::xmcd::XmcdEntry {
        track_titles: titles.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    }
}

fn first_entry_title(app: &App) -> String {
    let Mode::MediaLibrary(s) = &app.mode else {
        panic!("expected MediaLibrary mode");
    };
    s.disc_entries[0].title.clone()
}

/// CD-TEXT overlays the "Track N" placeholders when there's no gnudb/user
/// entry for the disc; the moment a gnudb entry exists, it wins entirely and
/// CD-TEXT is ignored — no merging of the two.
#[test]
fn cdtext_overlays_only_when_gnudb_absent() {
    let mut app = make_app();
    app.open_media_library();
    let toc = two_track_toc();
    let discid = crate::disc::discid::freedb_discid(&toc);
    let Mode::MediaLibrary(s) = &mut app.mode else {
        panic!("expected MediaLibrary mode");
    };
    s.tab = MediaLibraryTab::Discs;
    s.drives = vec![fake_audio_drive("/dev/sr0", toc)];
    s.selected_drive = 0;
    // Builds disc_entries ("Track N" placeholders) and applies whatever tags
    // exist yet (none) — mirrors the real Discs-tab entry flow.
    app.reload_ml_disc_entries();
    assert_eq!(first_entry_title(&app), "Track 1");

    // CD-TEXT present, gnudb absent -> CD-TEXT titles overlay.
    app.disc_cdtext.insert(
        discid.clone(),
        xmcd_with_titles(&["From CDTEXT 1", "From CDTEXT 2"]),
    );
    app.apply_disc_tags_to_entries();
    assert_eq!(first_entry_title(&app), "From CDTEXT 1");

    // gnudb now present -> gnudb wins outright, CD-TEXT ignored.
    app.disc_tags.insert(
        discid.clone(),
        xmcd_with_titles(&["From gnudb 1", "From gnudb 2"]),
    );
    app.apply_disc_tags_to_entries();
    assert_eq!(first_entry_title(&app), "From gnudb 1");
}

/// A data disc has a readable TOC too (tracks just aren't audio), so
/// `selected_disc_identity` alone can't tell it apart from an audio CD.
/// CD-TEXT reads spin the drive for real, so the spawner must gate on
/// `media.is_audio_cd` and bail before ever recording the disc-id as
/// "tried" — otherwise switching to the Discs tab with a data disc loaded
/// pointlessly shells out to read CD-TEXT off a disc that can never have
/// any track titles to show.
#[test]
fn cdtext_read_skips_data_discs() {
    let mut app = make_app();
    app.open_media_library();
    let toc = data_disc_toc();
    let discid = crate::disc::discid::freedb_discid(&toc);
    let Mode::MediaLibrary(s) = &mut app.mode else {
        panic!("expected MediaLibrary mode");
    };
    s.tab = MediaLibraryTab::Discs;
    s.drives = vec![fake_data_drive("/dev/sr0", toc)];
    s.selected_drive = 0;

    app.reload_ml_disc_entries();

    assert!(
        !app.disc_cdtext_tried.contains(&discid),
        "CD-TEXT read must not be attempted for a data disc"
    );
    assert!(
        app.disc_cdtext_read.is_none(),
        "no background CD-TEXT read should be in flight for a data disc"
    );
}

/// The tag editor seeds from CD-TEXT when that's the only metadata source
/// (gnudb/user tags absent), and from `disc_tags` (gnudb wins) once that's
/// present too — same whole-entry precedence as `apply_disc_tags_to_entries`.
#[test]
fn tag_editor_seeds_from_cdtext_then_gnudb_wins() {
    let mut app = make_app();
    app.open_media_library();
    let toc = two_track_toc();
    let discid = crate::disc::discid::freedb_discid(&toc);
    let Mode::MediaLibrary(s) = &mut app.mode else {
        panic!("expected MediaLibrary mode");
    };
    s.tab = MediaLibraryTab::Discs;
    s.drives = vec![fake_audio_drive("/dev/sr0", toc)];
    s.selected_drive = 0;
    app.reload_ml_disc_entries();

    // CD-TEXT only, no gnudb/user entry -> editor seeds artist/album/titles
    // from CD-TEXT.
    app.disc_cdtext.insert(
        discid.clone(),
        crate::disc::xmcd::XmcdEntry {
            artist: "CDTEXT Artist".to_string(),
            album: "CDTEXT Album".to_string(),
            track_titles: vec!["From CDTEXT 1".to_string(), "From CDTEXT 2".to_string()],
            ..Default::default()
        },
    );
    app.open_disc_tag_editor();
    {
        let Mode::MediaLibrary(s) = &app.mode else {
            panic!("expected MediaLibrary mode");
        };
        let ed = s.tag_edit.as_ref().expect("tag editor should be open");
        assert_eq!(ed.artist, "CDTEXT Artist");
        assert_eq!(ed.album, "CDTEXT Album");
        assert_eq!(ed.titles, vec!["From CDTEXT 1", "From CDTEXT 2"]);
    }

    // A gnudb/user disc_tags entry now exists -> it wins outright, CD-TEXT
    // ignored.
    app.disc_tags.insert(
        discid.clone(),
        crate::disc::xmcd::XmcdEntry {
            artist: "Gnudb Artist".to_string(),
            album: "Gnudb Album".to_string(),
            track_titles: vec!["From gnudb 1".to_string(), "From gnudb 2".to_string()],
            ..Default::default()
        },
    );
    app.open_disc_tag_editor();
    let Mode::MediaLibrary(s) = &app.mode else {
        panic!("expected MediaLibrary mode");
    };
    let ed = s.tag_edit.as_ref().expect("tag editor should be open");
    assert_eq!(ed.artist, "Gnudb Artist");
    assert_eq!(ed.album, "Gnudb Album");
    assert_eq!(ed.titles, vec!["From gnudb 1", "From gnudb 2"]);
}
