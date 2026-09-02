//! The GStreamer transcoder: `decodebin ! audioconvert ! audioresample ! wavenc`.
//!
//! Still the right answer wherever GStreamer is the audio stack anyway. The
//! pipeline description lives here rather than in `burn`, which is the whole
//! reason [`super::Transcoder`] exists — the burn module has no business
//! knowing what a `decodebin` is.

use std::path::Path;

use super::{Transcoder, RED_BOOK_CHANNELS, RED_BOOK_RATE};

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
