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
        sample_rate: None,
        file_size: None,
        file_mtime: None,
        added_at: None,
        bitrate_mode: None,
        rg_track_gain: None,
        rg_track_peak: None,
        rg_album_gain: None,
        rg_album_peak: None,
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

// ---------------------------------------------------------------------------
// Task 10: burn progress bar rendering
// ---------------------------------------------------------------------------

/// A determinate `BurnProgress` renders as `label [bar] pct%` — a 20-column
/// text bar with the filled portion proportional to `fraction`.
#[test]
fn render_progress_line_determinate_shows_bar_and_percent() {
    let p = crate::disc::burn::BurnProgress {
        label: "Burning".to_string(),
        fraction: Some(0.5),
    };
    let line = render_progress_line(&p, 0);
    assert!(
        line.starts_with("Burning ["),
        "label leads, then the bracketed bar: {line}"
    );
    assert!(line.contains("50%"), "50% fraction should read as 50%: {line}");
    assert!(
        line.contains("##########----------"),
        "half of a 20-wide bar filled at 50%: {line}"
    );
}

/// Fractions outside `0.0..=1.0` (shouldn't happen, but a streamed parse
/// could round past either end) clamp instead of producing a garbled bar.
#[test]
fn render_progress_line_clamps_out_of_range_fractions() {
    let over = crate::disc::burn::BurnProgress { label: "X".into(), fraction: Some(1.5) };
    assert!(render_progress_line(&over, 0).contains("100%"));
    let under = crate::disc::burn::BurnProgress { label: "X".into(), fraction: Some(-0.5) };
    assert!(render_progress_line(&under, 0).contains("0%"));
}

/// An indeterminate phase (`fraction: None`) renders a spinner glyph that
/// advances with `tick` and cycles back once every frame is used.
#[test]
fn render_progress_line_indeterminate_shows_advancing_spinner() {
    let p = crate::disc::burn::BurnProgress { label: "Erasing…".to_string(), fraction: None };
    let frame0 = render_progress_line(&p, 0);
    let frame1 = render_progress_line(&p, 1);
    assert!(frame0.starts_with("Erasing… "));
    assert_ne!(frame0, frame1, "the spinner glyph should differ as tick advances");
    assert_eq!(
        frame0,
        render_progress_line(&p, 4),
        "the 4-glyph spinner should cycle back after 4 ticks"
    );
}

// ---------------------------------------------------------------------------
// Task 10: disc artist/album fields (CD-TEXT) on the burn overlay
// ---------------------------------------------------------------------------

/// The burn overlay's disc artist/album start out showing the queue's
/// computed default (no override yet) and switch to a user override — the
/// edited field plus the untouched field carried through unchanged — the
/// moment either is typed into, mirroring the GTK burn panel's behavior.
#[test]
fn burn_setup_meta_fields_default_then_override_on_edit() {
    let mut app = make_app();
    app.open_media_library();
    let Mode::MediaLibrary(s) = &mut app.mode else {
        panic!("expected MediaLibrary mode");
    };
    s.tracks = vec![fake_lib_track("/fake/a.mp3", "Track A")];
    s.drives = vec![fake_drive("/dev/sr0", "Drive A")];
    s.selected_drive = 0;

    // Files tab `b`: queue the track, then switch to Discs and open `b`.
    app.handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
    let Mode::MediaLibrary(s) = &mut app.mode else {
        panic!("expected MediaLibrary mode");
    };
    s.tab = MediaLibraryTab::Discs;
    app.handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
    let Mode::MediaLibrary(s) = &app.mode else {
        panic!("expected MediaLibrary mode");
    };
    assert!(s.burn.is_some(), "burn overlay should open for a non-empty queue");

    // No override yet: meta_override is None and effective_meta reads the
    // computed default (a single track with no " - " artist prefix falls
    // back to "Various Artists" — see disc::cdtext::default_disc_meta).
    let list = app.burn_queues.get("/dev/sr0").unwrap();
    assert!(list.meta_override.is_none());
    let default_meta = list.effective_meta();
    assert!(!default_meta.artist.is_empty());
    assert!(!default_meta.album.is_empty());

    // `a` starts editing the disc artist. Same append-at-end convention as
    // every other text-input overlay in this app (rip's `editing_dest`,
    // add-path, search) — the field starts from its current (default) text
    // rather than clearing, so the typed char lands after it. The override
    // carries the untouched album straight through from the default.
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('X'), KeyModifiers::NONE);
    let overridden = app
        .burn_queues
        .get("/dev/sr0")
        .unwrap()
        .meta_override
        .clone()
        .expect("typing into the artist field should write an override");
    let expected_artist = format!("{}X", default_meta.artist);
    assert_eq!(overridden.artist, expected_artist);
    assert_eq!(
        overridden.album, default_meta.album,
        "the untouched album should carry through from the default, not go blank"
    );

    // Backspacing the whole override clears it back to an empty (still
    // Some, i.e. still overridden — not a reversion to the default).
    for _ in 0..expected_artist.chars().count() {
        app.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
    }
    let after_backspace = app
        .burn_queues
        .get("/dev/sr0")
        .unwrap()
        .meta_override
        .clone()
        .unwrap();
    assert_eq!(after_backspace.artist, "");

    // Esc leaves field-edit mode without closing the whole overlay.
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    let Mode::MediaLibrary(s) = &app.mode else {
        panic!("expected MediaLibrary mode");
    };
    let burn = s.burn.as_ref().expect("Esc from field-edit must not close the overlay");
    assert!(burn.editing_meta.is_none());
}
