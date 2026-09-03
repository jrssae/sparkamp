//! Tag writes into containers that are not MP3.
//!
//! Until this branch, `write_tag_fields` handed everything to the `id3` crate,
//! so a FLAC or an Ogg came back with an ID3v2 header bolted to the front and
//! no longer began with its own magic. Routing by container fixed that, and
//! `src/id3_editor.rs` has a thorough round-trip covering eleven formats, but
//! it is `#[ignore]`d behind a `SPARKAMP_FIXTURES` directory the caller has to
//! build with ffmpeg. Nothing about the routing runs in a plain `cargo test`.
//!
//! These do. Two real files, a tenth of a second of a sine tone each, small
//! enough to live in the repository and complete enough to have audio frames
//! after the tag block, which is what a bad write damages.
//!
//! Each check reads back through something other than the code that wrote:
//! `metaflac` for the FLAC comment, and Symphonia for whether the audio still
//! parses. A write that produced a plausible tag while corrupting the stream
//! would satisfy our own reader and fail these.

use std::path::{Path, PathBuf};

use sparkamp::id3_editor::{read_tag_fields, write_tag_fields};

/// Copy a fixture into a temporary directory, because these tests write.
fn scratch_copy(name: &str) -> (tempfile::TempDir, PathBuf) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let dir = tempfile::tempdir().expect("temp dir");
    let dst = dir.path().join(name);
    std::fs::copy(&src, &dst).unwrap_or_else(|e| panic!("copying {}: {e}", src.display()));
    (dir, dst)
}

/// The first four bytes, which is where a prepended foreign tag shows up.
fn magic(path: &Path) -> [u8; 4] {
    let bytes = std::fs::read(path).expect("readable fixture");
    bytes[..4].try_into().expect("fixture shorter than 4 bytes")
}

#[test]
fn writing_a_flac_tag_keeps_the_container_and_the_audio() {
    let (_dir, flac) = scratch_copy("tone.flac");
    assert_eq!(&magic(&flac), b"fLaC", "fixture is not a FLAC to begin with");
    let duration_before = sparkamp::duration_probe::probe_duration(&flac);
    assert!(
        duration_before.is_some(),
        "fixture has no readable duration, so a later loss would prove nothing"
    );

    let mut fields = read_tag_fields(&flac);
    fields.title = "Tone Title".to_string();
    fields.artist = "Tone Artist".to_string();
    write_tag_fields(&flac, &fields).expect("writing a FLAC tag");

    assert_eq!(
        &magic(&flac),
        b"fLaC",
        "an ID3 tag was prepended; the write went through the MPEG path"
    );

    // metaflac, not our reader: the value has to be a real Vorbis comment.
    let tag = metaflac::Tag::read_from_path(&flac).expect("still a readable FLAC");
    let title: Vec<&str> = tag.get_vorbis("TITLE").into_iter().flatten().collect();
    assert_eq!(
        title,
        vec!["Tone Title"],
        "TITLE is not a Vorbis comment in the FLAC's own metadata block"
    );

    assert_eq!(
        sparkamp::duration_probe::probe_duration(&flac),
        duration_before,
        "the audio frames did not survive the tag write"
    );
}

#[test]
fn writing_an_ogg_tag_keeps_the_container_and_the_audio() {
    let (_dir, ogg) = scratch_copy("tone.ogg");
    assert_eq!(&magic(&ogg), b"OggS", "fixture is not an Ogg to begin with");
    let duration_before = sparkamp::duration_probe::probe_duration(&ogg);
    assert!(
        duration_before.is_some(),
        "fixture has no readable duration, so a later loss would prove nothing"
    );

    let mut fields = read_tag_fields(&ogg);
    fields.title = "Tone Title".to_string();
    fields.artist = "Tone Artist".to_string();
    write_tag_fields(&ogg, &fields).expect("writing an Ogg tag");

    assert_eq!(
        &magic(&ogg),
        b"OggS",
        "an ID3 tag was prepended; the write went through the MPEG path"
    );

    // Symphonia demuxes the Ogg pages independently of lofty. A write that
    // broke the page structure fails here even if a tag reads back.
    assert_eq!(
        sparkamp::duration_probe::probe_duration(&ogg),
        duration_before,
        "the Ogg stream did not survive the tag write"
    );

    let back = read_tag_fields(&ogg);
    assert_eq!(back.title, "Tone Title");
    assert_eq!(back.artist, "Tone Artist");
}
