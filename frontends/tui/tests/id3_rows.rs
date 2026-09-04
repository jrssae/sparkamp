//! Which rows the tag editor offers for a given file.
//!
//! The editor used to render a fixed 19 rows for every file, whatever the
//! container could actually store, and Tab stopped at 13 of them. Both are
//! decided here now, so both are checked here.

use super::*;
use crossterm::event::{KeyCode, KeyModifiers};

/// A FLAC header with a STREAMINFO block and no audio. Enough for lofty to
/// identify the container and answer for its tag type.
fn minimal_flac(dir: &std::path::Path) -> PathBuf {
    let mut f = b"fLaC".to_vec();
    f.push(0x80); // last-metadata-block flag, type 0 (STREAMINFO)
    f.extend_from_slice(&[0, 0, 34]);
    f.extend_from_slice(&[0u8; 34]);
    let p = dir.join("song.flac");
    std::fs::write(&p, f).unwrap();
    p
}

#[test]
fn an_mp3_offers_every_field_and_replaygain() {
    let dir = tempfile::tempdir().unwrap();
    let mp3 = dir.path().join("song.mp3");
    std::fs::write(&mp3, b"").unwrap();

    let rows = crate::tui::id3_rows_for(&mp3);
    // 18 tag fields plus the ReplayGain row.
    assert_eq!(rows.len(), 19);
    assert!(rows.contains(&Id3Row::ReplayGain));
    assert!(
        rows.contains(&Id3Row::Field(15)),
        "URL is ID3's own WXXX, so an MP3 keeps it"
    );
    assert_eq!(rows[0], Id3Row::Field(0), "Title leads, as it always did");
}

#[test]
fn a_flac_drops_the_url_row_and_keeps_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let flac = minimal_flac(dir.path());

    let rows = crate::tui::id3_rows_for(&flac);
    assert!(
        !rows.contains(&Id3Row::Field(15)),
        "a Vorbis comment has no WXXX, so the URL row is not offered"
    );
    assert!(rows.contains(&Id3Row::Field(0)), "Title survives");
    assert!(rows.contains(&Id3Row::Field(17)), "Lyric survives");
}

/// Every rendered row can be reached. Tab wrapped at a hardcoded 13 while 19
/// rows were drawn, which left Composer through Encoded By visible and
/// uneditable.
#[test]
fn every_offered_row_is_reachable_by_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mp3 = dir.path().join("song.mp3");
    std::fs::write(&mp3, b"").unwrap();
    let rows = crate::tui::id3_rows_for(&mp3);

    let mut app = make_app();
    app.mode = Mode::Id3Editor(Id3EditorState {
        path: mp3.clone(),
        taggable: true,
        rows: rows.clone(),
        tech_summary: String::new(),
        fields: Default::default(),
        rg_gain: String::new(),
        rg_seed: String::new(),
        focused: 0,
        cursor: 0,
        genre_sel: 0,
        show_extra: false,
        extra_frames: Vec::new(),
        extra_focused: 0,
        extra_editing: false,
        extra_input: String::new(),
        extra_cursor: 0,
        status: None,
    });

    // Tab through a full cycle and record where focus lands.
    let mut seen = std::collections::HashSet::new();
    for _ in 0..rows.len() {
        if let Mode::Id3Editor(ref s) = app.mode {
            seen.insert(s.focused);
        }
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    }
    assert_eq!(seen.len(), rows.len(), "Tab did not visit every row");
    if let Mode::Id3Editor(ref s) = app.mode {
        assert_eq!(s.focused, 0, "a full cycle returns to the first row");
    }
}
