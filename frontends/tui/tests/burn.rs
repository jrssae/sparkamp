//! Discs tab: per-drive burn queues (`b` in Files tab queues onto the
//! currently shown drive; the burn overlay only opens for a non-empty
//! queue), driven through the public `handle_key` API.

use super::*;
use crossterm::event::{KeyCode, KeyModifiers};

fn fake_lib_track(path: &str, title: &str) -> crate::media_library::LibTrack {
    crate::media_library::LibTrack {
        id: 1,
        path: path.to_string(),
        artist: None,
        title: Some(title.to_string()),
        album: None,
        track_num: None,
        genre: None,
        year: None,
        bpm: None,
        length_secs: Some(120.0),
        bitrate: None,
        channels: None,
        filetype: None,
        filename: title.to_string(),
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
        sort_keys: Default::default(),
    }
}

/// A drive with a blank disc loaded — `erase_decision` lets `open_burn_setup`
/// proceed straight to the overlay (no erase confirmation, no "can't write"
/// refusal), which is what the isolation test needs.
fn fake_drive(id: &str, label: &str) -> crate::disc::OpticalDrive {
    crate::disc::OpticalDrive {
        id: id.to_string(),
        label: label.to_string(),
        media: crate::disc::MediaInfo {
            present: true,
            is_audio_cd: false,
            is_blank: true,
            rewritable: false,
            kind: crate::disc::MediaKind::CdR,
            free_bytes: 700_000_000,
            capacity_bytes: 700_000_000,
        },
        toc: None,
        mount_path: None,
    }
}

/// `b` in the Files tab queues onto whichever drive the Discs tab currently
/// shows; switching the shown drive switches which queue `b` (Discs tab)
/// sees, and each drive's queue is independent of the others.
#[test]
fn burn_queue_is_isolated_per_selected_drive() {
    let mut app = make_app();
    app.open_media_library();
    let Mode::MediaLibrary(s) = &mut app.mode else {
        panic!("expected MediaLibrary mode");
    };
    s.tracks = vec![fake_lib_track("/fake/a.mp3", "Track A")];
    s.drives = vec![fake_drive("/dev/sr0", "Drive A"), fake_drive("/dev/sr1", "Drive B")];
    s.selected_drive = 0;
    assert_eq!(s.tab, MediaLibraryTab::Files);

    // Files tab `b`: queue the highlighted track onto drive A (selected).
    app.handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
    assert_eq!(
        app.burn_queues.get("/dev/sr0").map(|l| l.len()),
        Some(1),
        "track should be queued on the selected drive (A)"
    );
    assert!(
        app.burn_queues.get("/dev/sr1").is_none(),
        "drive B's queue must not exist yet — isolation, not a shared list"
    );

    // Move to the Discs tab and switch the shown drive to B.
    let Mode::MediaLibrary(s) = &mut app.mode else {
        panic!("expected MediaLibrary mode");
    };
    s.tab = MediaLibraryTab::Discs;
    app.handle_key(KeyCode::Right, KeyModifiers::NONE); // selected_drive: 0 -> 1 (B)
    let Mode::MediaLibrary(s) = &app.mode else {
        panic!("expected MediaLibrary mode");
    };
    assert_eq!(s.selected_drive, 1);

    // Discs tab `b` on B: B's queue is empty, so the overlay refuses to
    // open — the "empty burn overlay" for the now-shown drive.
    app.handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
    let Mode::MediaLibrary(s) = &app.mode else {
        panic!("expected MediaLibrary mode");
    };
    assert!(
        s.burn.is_none(),
        "burn overlay must not open for drive B — its queue is empty"
    );

    // Switch back to drive A: its queued item is still there.
    app.handle_key(KeyCode::Left, KeyModifiers::NONE); // selected_drive: 1 -> 0 (A)
    let Mode::MediaLibrary(s) = &app.mode else {
        panic!("expected MediaLibrary mode");
    };
    assert_eq!(s.selected_drive, 0);
    app.handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
    let Mode::MediaLibrary(s) = &app.mode else {
        panic!("expected MediaLibrary mode");
    };
    assert!(
        s.burn.is_some(),
        "burn overlay should open for drive A — its item from before is still queued"
    );
    assert_eq!(app.burn_queues.get("/dev/sr0").map(|l| l.len()), Some(1));
}
