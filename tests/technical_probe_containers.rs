//! Technical properties across every container, against the committed tones.
//!
//! Sample rate and channel count came from Symphonia alone, which reads some
//! containers and not others, so the columns silently went blank for the rest.
//! Duration already recovered through a platform decoder when Symphonia could
//! not answer; these do the same, so the three properties agree about which
//! files they can describe.

use std::path::{Path, PathBuf};

/// Containers Symphonia reads unaided, plus the ones only a platform decoder
/// can describe. Split because the second group is answerable on Linux, where
/// GStreamer decodes them, and not on macOS, where CoreAudio does not.
const SYMPHONIA_READS: &[&str] = &["mp3", "flac", "ogg", "opus", "wav", "aiff", "aac"];
// Gated like its only user below: on macOS CoreAudio decodes none of the
// three, so there is no fallback to assert and the list has no reader.
#[cfg(not(target_os = "macos"))]
const PLATFORM_ONLY: &[&str] = &["tta", "wv", "wma"];
/// MP4 is the odd one: Symphonia reads its sample rate but reports no channel
/// count, while the same AAC in a raw ADTS stream reports one.
const MP4: &str = "m4a";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn probe(ext: &str) -> sparkamp::technical_probe::TechProbe {
    sparkamp::technical_probe::probe_technical(&fixture(&format!("tone.{ext}")))
}

#[test]
fn every_container_symphonia_reads_reports_rate_and_channels() {
    for ext in SYMPHONIA_READS {
        let t = probe(ext);
        assert!(t.sample_rate.is_some(), "{ext} has no sample rate");
        assert!(t.channels.is_some(), "{ext} has no channel count");
    }
}

/// The committed tones are 8 kHz mono, which is what makes them small enough
/// to live in the repository. Opus is the exception: it always resamples to
/// 48 kHz, so it is not asserted here.
#[test]
fn an_mp4_reports_its_channel_count() {
    let t = probe(MP4);
    assert_eq!(t.sample_rate, Some(8_000), "m4a sample rate");
    assert_eq!(
        t.channels,
        Some(1),
        "m4a channel count: Symphonia's MP4 reader does not fill this in, so \
         it has to come from the platform decoder like duration already does"
    );
}

/// Containers with no Symphonia reader at all. On Linux GStreamer describes
/// them; on macOS CoreAudio does not decode any of the three, so they stay
/// blank there and the columns are empty rather than wrong.
#[cfg(not(target_os = "macos"))]
#[test]
fn containers_without_a_symphonia_reader_fall_back_to_the_platform() {
    for ext in PLATFORM_ONLY {
        let t = probe(ext);
        assert!(t.sample_rate.is_some(), "{ext} has no sample rate");
        assert!(t.channels.is_some(), "{ext} has no channel count");
    }
}

/// Bitrate mode, per container.
///
/// It used to be MP3-only: the function returned `None` for anything else
/// before doing any work, so FLAC, Ogg and Opus showed blank despite every one
/// of them being variable by construction. The words are "Variable" and
/// "Constant" rather than the codec-forum abbreviations, because this is a
/// value a listener reads.
#[test]
fn bitrate_mode_is_known_for_the_containers_that_only_have_one() {
    use sparkamp::technical_probe::bitrate_mode;

    // Lossless and lossy formats whose frames are inherently variable.
    for ext in ["flac", "ogg", "opus", "tta", "wv"] {
        assert_eq!(
            bitrate_mode(&fixture(&format!("tone.{ext}"))),
            Some("Variable"),
            "{ext} has no constant-bitrate form"
        );
    }

    // Uncompressed PCM: every second is the same size.
    for ext in ["wav", "aiff"] {
        assert_eq!(
            bitrate_mode(&fixture(&format!("tone.{ext}"))),
            Some("Constant"),
            "{ext} is PCM"
        );
    }

    // MP3 carries the answer in its first frame: LAME writes "Xing" for
    // variable and "Info" for constant. The tone is encoded constant.
    assert_eq!(
        bitrate_mode(&fixture("tone.mp3")),
        Some("Constant"),
        "the mp3 tone is a constant-bitrate encode"
    );

    // These can be either and say nothing cheaply, so they stay unanswered
    // rather than guessed, and the column reads blank.
    for ext in ["m4a", "aac", "wma"] {
        assert_eq!(
            bitrate_mode(&fixture(&format!("tone.{ext}"))),
            None,
            "{ext} can be encoded either way"
        );
    }
}
