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

use super::{Transcoder, RED_BOOK_BITS, RED_BOOK_CHANNELS, RED_BOOK_RATE};

pub struct AvTranscoder;

impl Transcoder for AvTranscoder {
    fn to_red_book_wav(
        src: &Path,
        out: &Path,
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
                AVAudioFile::initForWriting_settings_error(AVAudioFile::alloc(), &out_url, &*settings()?)
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
}

/// What the file on disk holds: 16-bit LPCM at Red Book rate and channels.
fn settings() -> Result<Retained<NSDictionary<NSString, AnyObject>>, String> {
    // SAFETY: these are framework string constants, read only.
    let keys: Vec<&NSString> = unsafe {
        vec![
            AVFormatIDKey.ok_or("AVFormatIDKey missing")?,
            AVSampleRateKey.ok_or("AVSampleRateKey missing")?,
            AVNumberOfChannelsKey.ok_or("AVNumberOfChannelsKey missing")?,
            AVLinearPCMBitDepthKey.ok_or("AVLinearPCMBitDepthKey missing")?,
        ]
    };
    // `kAudioFormatLinearPCM`, spelled as the four-character code it is,
    // because CoreAudio's constant is not re-exported through these bindings.
    let format_id = NSNumber::new_u32(u32::from_be_bytes(*b"lpcm"));
    let rate = NSNumber::new_f64(RED_BOOK_RATE);
    let channels = NSNumber::new_u32(RED_BOOK_CHANNELS);
    let bits = NSNumber::new_u32(RED_BOOK_BITS);
    let values: Vec<&AnyObject> = vec![&format_id, &rate, &channels, &bits];
    Ok(NSDictionary::from_slices(&keys, &values))
}
