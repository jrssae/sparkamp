//! Turning a burn-list entry into the only thing an audio CD holds.
//!
//! A burn list is whatever the user put in it — MP3, FLAC, AAC, anything the
//! player opens — and Red Book audio is 44.1 kHz, 16-bit, stereo PCM and
//! nothing else. Something has to convert, and this is where it happens.
//!
//! ## The seam, and why there is one
//!
//! Same reasoning as [`crate::engine::backend`], and deliberately the same
//! shape. Both platforms genuinely transcode; they just do it with different
//! machinery. So the vocabulary here is the job — a source file, a
//! destination, and progress — and neither GStreamer nor AVFoundation appears
//! in it.
//!
//! That is not decoration. Before this seam, `burn::prepare_pipeline_desc`
//! built a **GStreamer pipeline string in the middle of the burn module**, so
//! the core burn logic knew what a `decodebin` was. Moving it behind
//! [`Transcoder`] is what lets macOS answer the same question with
//! `AVAudioConverter` and lets `burn.rs` stop caring.
//!
//! ## Why macOS needs its own answer
//!
//! GStreamer was still a hard requirement for burning even after the audio
//! backend stopped using it, which meant the App Store build — which cannot
//! bundle GStreamer — could not burn at all. AVFoundation decodes every format
//! the player does (measured by `avf_decodes_the_shipped_formats`), so the
//! conversion is `AVAudioFile` in, `AVAudioConverter` between, `AVAudioFile`
//! out, with no pipeline anywhere.
//!
//! ## Ripping, and why the format differs by platform
//!
//! [`Encoder`] is the same seam for the other direction: a disc track out to a
//! file. It differs from burning in one way that cannot be hidden — the
//! **encoder** is not the same on both platforms, and pretending otherwise
//! would be a lie the type system helped tell.
//!
//! CoreAudio decodes MP3 without being able to write it. It writes FLAC. So
//! macOS rips to FLAC and Linux rips to MP3 by default, each says what it can
//! write through [`Encoder::can_write`], and the caller asks rather than
//! assumes. FLAC is lossless, so the macOS default is not a downgrade — but it
//! is a difference, and [`RipFormat::extension`] is what keeps it from leaking
//! into every path that builds a filename.

use std::path::Path;

/// Red Book: 44.1 kHz, 16-bit, stereo. Not configurable, because a CD player
/// is not.
pub const RED_BOOK_RATE: f64 = 44_100.0;
pub const RED_BOOK_CHANNELS: u32 = 2;
pub const RED_BOOK_BITS: u32 = 16;

/// Whatever turns a playable file into a Red Book WAV.
///
/// One method, in the vocabulary of the job. A caller states what it wants
/// converted and gets told how far along it is; nothing in this signature says
/// how, which is the point.
pub trait Transcoder {
    /// Convert `src` to a Red Book WAV at `out`.
    ///
    /// `on_position` receives the source position in seconds, roughly twice a
    /// second, and nothing after the end — the contract callers already turn
    /// into the within-track fraction of "Preparing i/N".
    ///
    /// A failed conversion leaves no file behind. A half-written WAV is worse
    /// than none: the burn would take it for a track and write a truncated
    /// one.
    fn to_red_book_wav(
        src: &Path,
        out: &Path,
        on_position: &mut dyn FnMut(f64),
    ) -> Result<(), String>;
}

/// What a ripped track is written as.
///
/// Not "the codec": the quality preset rides along, because for MP3 the two
/// are one decision and splitting them would let a caller ask for a bitrate
/// the format has no use for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RipFormat {
    /// MPEG-1 Layer III at one of the presets in [`crate::disc::rip::Mp3Quality`].
    Mp3(crate::disc::rip::Mp3Quality),
    /// Free Lossless Audio Codec. No quality knob, because there is nothing to
    /// trade — the output is the disc.
    Flac,
}

impl RipFormat {
    /// The filename extension, without the dot.
    ///
    /// One place, so nothing else in the tree hardcodes `.mp3` — which
    /// `dest_path` did, and which is exactly how a format choice leaks.
    pub fn extension(self) -> &'static str {
        match self {
            RipFormat::Mp3(_) => "mp3",
            RipFormat::Flac => "flac",
        }
    }

    /// Whether tags go in an ID3 tag or a Vorbis comment block. The two
    /// containers are not interchangeable and nothing else decides this.
    pub fn tags_are_id3(self) -> bool {
        matches!(self, RipFormat::Mp3(_))
    }
}

/// Whatever turns a disc track into a file.
///
/// Separate from [`Transcoder`] because the answer genuinely differs: burning
/// needs no encoder and every platform can produce PCM, while ripping needs
/// one and they do not have the same ones.
pub trait Encoder {
    /// What this platform rips to when nothing else is specified.
    fn default_format() -> RipFormat;

    /// Whether this platform can write `format`. Asked rather than assumed:
    /// a caller that hardcodes MP3 produces an empty file on macOS.
    fn can_write(format: RipFormat) -> bool;

    /// Encode `source` to `out`, reporting the source position in seconds.
    ///
    /// Tags are not written here. They are a property of the file that comes
    /// out, not of the encoding, and they differ by container — see
    /// [`RipFormat::tags_are_id3`].
    fn encode(
        source: &crate::disc::rip::RipSource,
        out: &Path,
        format: RipFormat,
        on_position: &mut dyn FnMut(f64),
    ) -> Result<(), String>;
}

#[cfg(target_os = "macos")]
pub mod avf;
#[cfg(not(target_os = "macos"))]
pub mod gst;

/// The transcoder a bare call gets. `#[cfg]` picks it; nothing above this line
/// names either implementation.
#[cfg(target_os = "macos")]
pub type DefaultTranscoder = avf::AvTranscoder;
#[cfg(not(target_os = "macos"))]
pub type DefaultTranscoder = gst::GstTranscoder;

/// The encoder a bare call gets.
#[cfg(target_os = "macos")]
pub type DefaultEncoder = avf::AvTranscoder;
#[cfg(not(target_os = "macos"))]
pub type DefaultEncoder = gst::GstTranscoder;

/// What this build rips to unless told otherwise.
pub fn default_rip_format() -> RipFormat {
    DefaultEncoder::default_format()
}

/// Whether this build can write `format`.
pub fn can_write(format: RipFormat) -> bool {
    DefaultEncoder::can_write(format)
}

/// Encode one disc track through the platform's encoder.
///
/// Falls back to [`default_rip_format`] when the requested format is one this
/// platform cannot write, rather than failing or writing an empty file — a
/// user who asked for MP3 on macOS gets a FLAC, which is the format they can
/// actually have, and the caller is told which it was.
pub fn encode(
    source: &crate::disc::rip::RipSource,
    out: &Path,
    format: RipFormat,
    on_position: &mut dyn FnMut(f64),
) -> Result<(RipFormat, ()), String> {
    let format = if can_write(format) {
        format
    } else {
        default_rip_format()
    };
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let result = DefaultEncoder::encode(source, out, format, on_position);
    if result.is_err() {
        let _ = std::fs::remove_file(out);
    }
    result.map(|()| (format, ()))
}

/// Convert `src` to a Red Book WAV through the platform's transcoder.
///
/// The one entry point `burn` calls, so the choice of implementation is made
/// here and nowhere else.
pub fn to_red_book_wav(
    src: &Path,
    out: &Path,
    on_position: &mut dyn FnMut(f64),
) -> Result<(), String> {
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let result = DefaultTranscoder::to_red_book_wav(src, out, on_position);
    if result.is_err() {
        let _ = std::fs::remove_file(out);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every format the burn list may hold must come out as Red Book audio.
    /// `SPARKAMP_FORMAT_DIR=<dir of t.<ext> samples> cargo test --lib \
    ///   transcodes_to_red_book -- --ignored --nocapture`
    ///
    /// `#[ignore]` because it needs sample files the repository does not
    /// carry; the same directory `avf_decodes_the_shipped_formats` uses.
    ///
    /// The bar is the WAV header, not a successful return: a transcode that
    /// wrote the source's own rate would burn a disc that plays at the wrong
    /// speed, and would pass any check that only asked whether it finished.
    #[test]
    #[ignore]
    fn transcodes_to_red_book() {
        let Some(dir) = std::env::var_os("SPARKAMP_FORMAT_DIR") else {
            println!("set SPARKAMP_FORMAT_DIR to a directory of t.<ext> samples");
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let out_dir = std::env::temp_dir().join(format!("sparkamp-tc-{}", std::process::id()));
        std::fs::create_dir_all(&out_dir).unwrap();
        let mut converted = 0;
        for ext in crate::model::AUDIO_EXTENSIONS {
            let src = dir.join(format!("t.{ext}"));
            if !src.exists() {
                continue;
            }
            let out = out_dir.join(format!("{ext}.wav"));
            let mut last = 0.0f64;
            let mut ticks = 0usize;
            match to_red_book_wav(&src, &out, &mut |p| {
                assert!(p >= last, "position must not go backwards: {p} after {last}");
                last = p;
                ticks += 1;
            }) {
                Ok(()) => {
                    let (rate, channels, bits, frames) = wav_shape(&out);
                    println!(
                        "  {ext:5} -> {rate} Hz, {channels} ch, {bits}-bit, {frames} frames, \
                         {ticks} progress tick(s)"
                    );
                    assert_eq!(rate, RED_BOOK_RATE as u32, "{ext}: sample rate");
                    assert_eq!(channels, RED_BOOK_CHANNELS as u16, "{ext}: channels");
                    assert_eq!(bits, RED_BOOK_BITS as u16, "{ext}: bit depth");
                    assert!(frames > 0, "{ext}: wrote no audio");
                    assert!(ticks > 0, "{ext}: reported no progress");
                    // Shape is not content. A transcode that wrote the right
                    // header over silence, or over noise, passes every
                    // assertion above — so this asks what the audio actually
                    // is. Every sample is the same 440 Hz tone.
                    let hz = dominant_hz(&out);
                    println!("        dominant {hz:.1} Hz");
                    assert!(
                        (hz - 440.0).abs() < 15.0,
                        "{ext}: expected the 440 Hz tone, measured {hz:.1} Hz"
                    );
                    converted += 1;
                }
                Err(e) => {
                    println!("  {ext:5} -> refused: {e}");
                    assert!(!out.exists(), "{ext}: a failed transcode left a file behind");
                }
            }
        }
        let _ = std::fs::remove_dir_all(&out_dir);
        assert!(converted > 0, "no sample converted at all");
    }

    /// A source that is not audio must fail and leave nothing behind — the
    /// burn would take a stray file for a track.
    #[test]
    fn a_refused_source_leaves_no_file() {
        let dir = std::env::temp_dir().join(format!("sparkamp-tcbad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("not-audio.mp3");
        std::fs::write(&src, b"this is not an audio file").unwrap();
        let out = dir.join("out.wav");
        let result = to_red_book_wav(&src, &out, &mut |_| {});
        let existed = out.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(result.is_err(), "garbage must not transcode");
        assert!(!existed, "a failed transcode must leave no file");
    }

    /// The strongest frequency in the left channel of a 16-bit stereo WAV.
    ///
    /// A coarse DFT over one 8192-sample window, which resolves to about
    /// 5.4 Hz at 44.1 kHz — far finer than needed to tell a correct transcode
    /// from silence, noise, or one written at the wrong rate.
    fn dominant_hz(path: &std::path::Path) -> f64 {
        let b = std::fs::read(path).expect("read the transcoded wav");
        // The samples start after the `data` header, wherever the chunk walk
        // finds it.
        let mut at = 12usize;
        let mut start = None;
        while at + 8 <= b.len() {
            let id = &b[at..at + 4];
            let len = u32::from_le_bytes([b[at + 4], b[at + 5], b[at + 6], b[at + 7]]) as usize;
            if id == b"data" {
                start = Some(at + 8);
                break;
            }
            at = at + 8 + len + (len & 1);
        }
        let start = start.expect("no data chunk");
        // A quarter second in, past any encoder priming silence.
        let skip = start + 11_025 * 4;
        const N: usize = 8192;
        let left: Vec<f64> = (0..N)
            .map(|i| {
                let at = skip + i * 4;
                if at + 1 < b.len() {
                    f64::from(i16::from_le_bytes([b[at], b[at + 1]]))
                } else {
                    0.0
                }
            })
            .collect();
        let mut best = (0.0f64, 0usize);
        for k in 1..N / 2 {
            let w = 2.0 * std::f64::consts::PI * k as f64 / N as f64;
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (j, v) in left.iter().enumerate() {
                re += v * (w * j as f64).cos();
                im += v * (w * j as f64).sin();
            }
            let mag = re * re + im * im;
            if mag > best.0 {
                best = (mag, k);
            }
        }
        best.1 as f64 * RED_BOOK_RATE / N as f64
    }

    /// (rate, channels, bits, frames), by walking the RIFF chunks.
    ///
    /// Walked rather than read at fixed offsets: a WAV is a chunk container,
    /// and CoreAudio writes a padding chunk ahead of `fmt `, so the canonical
    /// 44-byte layout is a convention rather than the format. Assuming it read
    /// every field as zero.
    fn wav_shape(path: &std::path::Path) -> (u32, u16, u16, u32) {
        let b = std::fs::read(path).expect("read the transcoded wav");
        assert!(b.len() >= 12, "too short to be a WAV");
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(&b[8..12], b"WAVE");
        let (mut rate, mut channels, mut bits, mut data) = (0u32, 0u16, 0u16, 0u32);
        let mut at = 12usize;
        while at + 8 <= b.len() {
            let id = &b[at..at + 4];
            let len = u32::from_le_bytes([b[at + 4], b[at + 5], b[at + 6], b[at + 7]]) as usize;
            let body = at + 8;
            if id == b"fmt " && body + 16 <= b.len() {
                channels = u16::from_le_bytes([b[body + 2], b[body + 3]]);
                rate = u32::from_le_bytes([
                    b[body + 4],
                    b[body + 5],
                    b[body + 6],
                    b[body + 7],
                ]);
                bits = u16::from_le_bytes([b[body + 14], b[body + 15]]);
            } else if id == b"data" {
                data = len as u32;
            }
            // Chunks are word-aligned: an odd length is followed by a pad byte.
            at = body + len + (len & 1);
        }
        let frame = u32::from(channels) * u32::from(bits / 8);
        (rate, channels, bits, if frame > 0 { data / frame } else { 0 })
    }
}
