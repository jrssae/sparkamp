//! The GStreamer transcoder: `decodebin ! audioconvert ! audioresample ! wavenc`.
//!
//! Still the right answer wherever GStreamer is the audio stack anyway. The
//! pipeline description lives here rather than in `burn`, which is the whole
//! reason [`super::Transcoder`] exists — the burn module has no business
//! knowing what a `decodebin` is.

use std::path::Path;

use super::{Encoder, RipFormat, Transcoder, RED_BOOK_CHANNELS, RED_BOOK_RATE};
use crate::disc::rip::RipSource;

pub struct GstTranscoder;

impl Transcoder for GstTranscoder {
    fn to_red_book_wav(
        src: &Path,
        out: &Path,
        on_position: &mut dyn FnMut(f64),
    ) -> Result<(), String> {
        crate::disc::rip::run_pipeline_observed(&pipeline_desc(src, out), on_position)
    }
}

/// The pipeline that decodes anything and writes Red Book PCM.
///
/// Quoting is deliberate: a path with a space or a quote in it is ordinary,
/// and `gst_parse_launch` takes this as a string.
pub fn pipeline_desc(src: &Path, out: &Path) -> String {
    format!(
        "filesrc location=\"{}\" ! decodebin ! audioconvert ! audioresample \
         ! audio/x-raw,format=S16LE,rate={},channels={} ! wavenc \
         ! filesink location=\"{}\"",
        src.display().to_string().replace('"', "\\\""),
        RED_BOOK_RATE as u32,
        RED_BOOK_CHANNELS,
        out.display().to_string().replace('"', "\\\"")
    )
}

impl Encoder for GstTranscoder {
    /// MP3, which is what this platform has always ripped to and what its
    /// encoder set can write.
    fn default_format() -> RipFormat {
        RipFormat::Mp3(crate::disc::rip::Mp3Quality::VbrV2)
    }

    /// Both. `lamemp3enc` and `flacenc` are ordinary GStreamer elements.
    fn can_write(_format: RipFormat) -> bool {
        true
    }

    fn encode(
        source: &RipSource,
        out: &Path,
        format: RipFormat,
        on_position: &mut dyn FnMut(f64),
    ) -> Result<(), String> {
        crate::disc::rip::run_pipeline_observed(&encode_desc(source, format, out), on_position)
    }
}

/// The pipeline that reads one track and encodes it.
pub fn encode_desc(source: &RipSource, format: RipFormat, out: &Path) -> String {
    let src = match source {
        RipSource::File { path } => format!(
            "filesrc location=\"{}\" ! decodebin",
            path.display().to_string().replace('"', "\\\"")
        ),
        RipSource::Cdda { device, track } => {
            // cdparanoiasrc, not cdiocddasrc: it does read error correction,
            // and libcdio's source fails partway through a track with
            // "cdio_read_audio_sector … No such device" once the drive-typing
            // probe has touched the drive — which the detection poll does
            // routinely.
            format!("cdparanoiasrc track={track} device=\"{device}\"")
        }
    };
    let encoder = match format {
        RipFormat::Mp3(quality) => format!("lamemp3enc {}", quality.encoder_props()),
        // Level 5 is flacenc's own default: the middle of the compression
        // range, and the one every other FLAC tool means by "default".
        RipFormat::Flac => "flacenc".to_string(),
    };
    format!(
        "{src} ! audioconvert ! {encoder} ! filesink location=\"{}\"",
        out.display().to_string().replace('"', "\\\"")
    )
}
