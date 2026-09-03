//! The AVFoundation transcoder: `AVAudioFile` in, `AVAudioConverter` between,
//! `AVAudioFile` out.
//!
//! No pipeline, no plugins, and nothing that has to be bundled — which is what
//! lets the App Store build burn a CD at all.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

use block2::RcBlock;
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_avf_audio::{
    AVAudioConverter, AVAudioConverterInputStatus, AVAudioConverterOutputStatus, AVAudioFile,
    AVAudioPCMBuffer, AVFormatIDKey, AVLinearPCMBitDepthKey, AVNumberOfChannelsKey, AVSampleRateKey,
};
use objc2::runtime::AnyObject;
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

use super::{Encoder, RipFormat, Transcoder, RED_BOOK_BITS, RED_BOOK_CHANNELS, RED_BOOK_RATE};
use crate::disc::rip::RipSource;

pub struct AvTranscoder;

impl Transcoder for AvTranscoder {
    fn to_red_book_wav(
        src: &Path,
        out: &Path,
        on_position: &mut dyn FnMut(f64),
    ) -> Result<(), String> {
        convert(src, out, &*red_book_settings()?, on_position)
    }
}

impl Encoder for AvTranscoder {
    /// FLAC. CoreAudio decodes MP3 without being able to write it, and FLAC is
    /// lossless, so this is a different format rather than a lesser one.
    fn default_format() -> RipFormat {
        RipFormat::Flac
    }

    /// FLAC only. Saying so is what lets a caller fall back deliberately
    /// instead of writing an empty file.
    fn can_write(format: RipFormat) -> bool {
        matches!(format, RipFormat::Flac)
    }

    fn encode(
        source: &RipSource,
        out: &Path,
        format: RipFormat,
        on_position: &mut dyn FnMut(f64),
    ) -> Result<(), String> {
        if !Self::can_write(format) {
            return Err(format!("this platform cannot write {format:?}"));
        }
        match source {
            RipSource::File { path } => convert(path, out, &*flac_settings()?, on_position),
            // AVFoundation decodes files, not drives, so the track comes off
            // the disc first and is encoded from that. The disc read is the
            // slow half by a wide margin, so it is the half that reports
            // position; the encode that follows runs silent rather than
            // filling the same bar a second time.
            RipSource::Cdda { device, track } => {
                let staged = std::env::temp_dir().join(format!(
                    "sparkamp-cdda-{}-{track}.wav",
                    std::process::id()
                ));
                let result = crate::disc::discrecording::cdda_track_to_wav(
                    device,
                    *track,
                    &staged,
                    on_position,
                )
                .and_then(|()| convert(&staged, out, &*flac_settings()?, &mut |_| {}));
                let _ = std::fs::remove_file(&staged);
                result
            }
        }
    }
}

/// Decode `src` and write it back out under `settings`.
///
/// One body for both directions: the only thing that differs between staging
/// a burn and ripping a track is what the destination file is, and that is
/// entirely the settings dictionary.
fn convert(
    src: &Path,
    out: &Path,
    settings: &NSDictionary<NSString, AnyObject>,
    on_position: &mut dyn FnMut(f64),
) -> Result<(), String> {
        let src_str = src.to_str().ok_or("source path is not UTF-8")?;
        let out_str = out.to_str().ok_or("destination path is not UTF-8")?;
        let in_url = NSURL::fileURLWithPath(&NSString::from_str(src_str));
        let out_url = NSURL::fileURLWithPath(&NSString::from_str(out_str));

        // SAFETY: every call below is an ordinary Objective-C message to an
        // object this function owns. The unsafe is objc2's blanket marking of
        // generated methods, not a claim about aliasing.
        unsafe {
            let input = AVAudioFile::initForReading_error(AVAudioFile::alloc(), &in_url)
                .map_err(|e| format!("could not read {}: {e:?}", src.display()))?;
            let in_format = input.processingFormat();
            let in_rate = in_format.sampleRate();
            if in_rate <= 0.0 {
                return Err(format!(
                    "{} reports a sample rate of {in_rate}",
                    src.display()
                ));
            }

            let output =
                AVAudioFile::initForWriting_settings_error(AVAudioFile::alloc(), &out_url, settings)
                    .map_err(|e| format!("could not write {}: {e:?}", out.display()))?;
            let out_format = output.processingFormat();

            let converter = AVAudioConverter::initFromFormat_toFormat(
                AVAudioConverter::alloc(),
                &in_format,
                &out_format,
            )
            .ok_or_else(|| {
                format!(
                    "no conversion from {}'s format to Red Book",
                    src.display()
                )
            })?;

            // A second at a time: large enough that per-chunk overhead
            // disappears, small enough that `on_position` still moves at about
            // the cadence the GStreamer path reported.
            let in_frames = in_rate as u32;
            let out_frames = RED_BOOK_RATE as u32;
            // Shared with the input block rather than borrowed by it:
            // `RcBlock` requires `'static`, so the block owns refcounted
            // handles to the counters and to the file it reads.
            let consumed = Rc::new(Cell::new(0i64));

            loop {
                let out_buffer = AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
                    AVAudioPCMBuffer::alloc(),
                    &out_format,
                    out_frames,
                )
                .ok_or("could not allocate an output buffer")?;

                // The converter pulls input through this block rather than
                // being handed a buffer, and it has to: resampling means one
                // output chunk needs however much input it needs, not a fixed
                // amount.
                let ended = Rc::new(Cell::new(false));
                let block = RcBlock::new({
                    let consumed = Rc::clone(&consumed);
                    let ended = Rc::clone(&ended);
                    let input = input.clone();
                    let in_format = in_format.clone();
                    move |_wanted: u32, status: std::ptr::NonNull<AVAudioConverterInputStatus>| {
                        let buffer = AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
                            AVAudioPCMBuffer::alloc(),
                            &in_format,
                            in_frames,
                        );
                        let Some(buffer) = buffer else {
                            ended.set(true);
                            *status.as_ptr() = AVAudioConverterInputStatus::EndOfStream;
                            return std::ptr::null_mut();
                        };
                        // `readIntoBuffer` throws at the end of the file rather
                        // than returning an empty buffer, so an error here is
                        // "finished", not "broken".
                        match input.readIntoBuffer_error(&buffer) {
                            Ok(()) if buffer.frameLength() > 0 => {
                                consumed.set(consumed.get() + i64::from(buffer.frameLength()));
                                *status.as_ptr() = AVAudioConverterInputStatus::HaveData;
                                Retained::into_raw(buffer).cast()
                            }
                            _ => {
                                ended.set(true);
                                *status.as_ptr() = AVAudioConverterInputStatus::EndOfStream;
                                std::ptr::null_mut()
                            }
                        }
                    }
                });

                let mut error = None;
                let status = converter.convertToBuffer_error_withInputFromBlock(
                    &out_buffer,
                    Some(&mut error),
                    RcBlock::as_ptr(&block),
                );
                if let Some(error) = error {
                    return Err(format!("converting {}: {error:?}", src.display()));
                }
                if status == AVAudioConverterOutputStatus::Error {
                    return Err(format!("converting {} failed", src.display()));
                }
                if out_buffer.frameLength() > 0 {
                    output
                        .writeFromBuffer_error(&out_buffer)
                        .map_err(|e| format!("writing {}: {e:?}", out.display()))?;
                    on_position(consumed.get() as f64 / in_rate);
                }
                // Done when the converter says so, or when the source ran out
                // and the converter has nothing left to hand back.
                if status == AVAudioConverterOutputStatus::EndOfStream
                    || (ended.get() && out_buffer.frameLength() == 0)
                {
                    break;
                }
        }
    }
    Ok(())
}

/// One settings dictionary from a list of key/value pairs.
fn settings(
    pairs: &[(Option<&'static NSString>, &AnyObject)],
) -> Result<Retained<NSDictionary<NSString, AnyObject>>, String> {
    let mut keys: Vec<&NSString> = Vec::with_capacity(pairs.len());
    let mut values: Vec<&AnyObject> = Vec::with_capacity(pairs.len());
    for (key, value) in pairs {
        keys.push(key.ok_or("an AVAudioSettings key is missing")?);
        values.push(value);
    }
    Ok(NSDictionary::from_slices(&keys, &values))
}

/// A Red Book WAV: 16-bit LPCM, 44.1 kHz, stereo.
fn red_book_settings() -> Result<Retained<NSDictionary<NSString, AnyObject>>, String> {
    // SAFETY: framework string constants, read only.
    let (format_key, rate_key, channels_key, bits_key) = unsafe {
        (
            AVFormatIDKey,
            AVSampleRateKey,
            AVNumberOfChannelsKey,
            AVLinearPCMBitDepthKey,
        )
    };
    // `kAudioFormatLinearPCM`, spelled as the four-character code it is,
    // because CoreAudio's constant is not re-exported through these bindings.
    let format_id = NSNumber::new_u32(u32::from_be_bytes(*b"lpcm"));
    let rate = NSNumber::new_f64(RED_BOOK_RATE);
    let channels = NSNumber::new_u32(RED_BOOK_CHANNELS);
    let bits = NSNumber::new_u32(RED_BOOK_BITS);
    settings(&[
        (format_key, &format_id),
        (rate_key, &rate),
        (channels_key, &channels),
        (bits_key, &bits),
    ])
}

/// FLAC at the disc's own rate and channel count.
///
/// Rate and channels are named rather than left to the encoder because a rip
/// is a copy of the disc: 44.1 kHz stereo in, 44.1 kHz stereo out, and no
/// resampling anywhere in between to be lossless about.
fn flac_settings() -> Result<Retained<NSDictionary<NSString, AnyObject>>, String> {
    // SAFETY: framework string constants, read only.
    let (format_key, rate_key, channels_key) =
        unsafe { (AVFormatIDKey, AVSampleRateKey, AVNumberOfChannelsKey) };
    // `kAudioFormatFLAC`.
    let format_id = NSNumber::new_u32(u32::from_be_bytes(*b"flac"));
    let rate = NSNumber::new_f64(RED_BOOK_RATE);
    let channels = NSNumber::new_u32(RED_BOOK_CHANNELS);
    settings(&[
        (format_key, &format_id),
        (rate_key, &rate),
        (channels_key, &channels),
    ])
}
