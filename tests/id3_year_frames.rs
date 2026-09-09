//! The year survives a save on a file carrying ID3v2.3's `TYER`.
//!
//! Both halves of a real regression. A file tagged elsewhere arrived with
//! `TYER` holding a stray U+FEFF inside its text, and with `TYER` absent from
//! the editor's covered-frame set it also showed up in the Customize panel as
//! an editable "Year (legacy)" row. Saving wrote the fields first and replayed
//! that row second, so the old year went straight back over the new one and
//! the Year field read empty besides.

use id3::TagLike;
use std::path::PathBuf;

/// Copy the tone fixture and give it a v2.3 tag in the broken shape.
fn fixture_with_tyer(name: &str, tdrc: &str, tyer: &str) -> PathBuf {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone.mp3");
    let dst = std::env::temp_dir().join(name);
    std::fs::copy(&src, &dst).expect("fixture copy");

    let mut tag = id3::Tag::new();
    tag.set_text("TIT2", "Probe");
    if !tdrc.is_empty() {
        tag.set_text("TDRC", tdrc);
    }
    tag.set_text("TYER", tyer);
    tag.write_to_path(&dst, id3::Version::Id3v23).expect("tag write");
    dst
}

#[test]
fn a_stray_bom_in_tyer_does_not_swallow_the_year() {
    let path = fixture_with_tyer("sparkamp_tyer_bom.mp3", "", "\u{feff}2018");
    let fields = sparkamp::id3_editor::read_tag_fields(&path);
    assert_eq!(fields.year, "2018", "the BOM belongs to the encoding, not the number");
}

#[test]
fn tdrc_wins_when_the_two_year_frames_disagree() {
    let path = fixture_with_tyer("sparkamp_tyer_split.mp3", "2016", "\u{feff}2018");
    let fields = sparkamp::id3_editor::read_tag_fields(&path);
    assert_eq!(fields.year, "2016", "TDRC is the frame the editor writes");
}

#[test]
fn tyer_is_not_offered_as_a_second_year_control() {
    let path = fixture_with_tyer("sparkamp_tyer_extra.mp3", "2016", "2018");
    let ids: Vec<String> = sparkamp::id3_editor::read_extra_frames(&path)
        .into_iter()
        .map(|f| f.id)
        .collect();
    assert!(
        !ids.iter().any(|id| id == "TYER"),
        "TYER in the Customize panel replays over the saved year, got {ids:?}"
    );
}

#[test]
fn saving_a_new_year_sticks_on_a_v23_file() {
    let path = fixture_with_tyer("sparkamp_tyer_save.mp3", "2016", "\u{feff}2018");

    let mut fields = sparkamp::id3_editor::read_tag_fields(&path);
    fields.year = "2016".into();
    sparkamp::id3_editor::write_tag_fields(&path, &fields).expect("save");

    assert_eq!(sparkamp::id3_editor::read_tag_fields(&path).year, "2016");

    // Both spellings agree afterwards, so no reader disagrees with the editor.
    let tag = id3::Tag::read_from_path(&path).expect("re-read");
    assert_eq!(tag.get("TDRC").and_then(|f| f.content().text()), Some("2016"));
    assert_eq!(tag.get("TYER").and_then(|f| f.content().text()), Some("2016"));
}
