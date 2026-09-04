//! Technical audio properties read from codec headers.
//!
//! The scanner never captured sample rate or a reliable bitrate/channel
//! count (the DB columns existed but stayed NULL). This module is the one
//! place that derives them: codec parameters via Symphonia's format probe
//! (header-only — no decode), and average bitrate from file size over
//! duration, which is exact for CBR and the honest average for VBR.

use std::path::Path;

#[derive(Debug, Default, Clone, Copy)]
pub struct TechProbe {
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
}

/// Read sample rate and channel count for a file.
///
/// Symphonia's header read answers first, and the platform's decoder fills in
/// whatever it could not. Returns an empty probe when neither can say, so a
/// scan row degrades to NULL and the column reads blank rather than wrong.
pub fn probe_technical(path: &Path) -> TechProbe {
    let symphonia = symphonia_technical(path);
    if symphonia.sample_rate.is_some() && symphonia.channels.is_some() {
        return symphonia;
    }
    // Symphonia answered partly or not at all. Ask the platform's decoder for
    // the rest, the same way `duration_probe` already recovers a length it
    // could not read from a header. Without this, an MP4 shows no channel
    // count (its reader fills in the rate and not the channels, while the same
    // AAC in a raw stream fills in both), and TrueAudio, WavPack and WMA show
    // neither, because Symphonia has no reader for any of them.
    let fallback = platform::probe_technical(path);
    TechProbe {
        sample_rate: symphonia.sample_rate.or(fallback.sample_rate),
        channels: symphonia.channels.or(fallback.channels),
    }
}

/// Codec parameters from Symphonia's header read. Empty on any failure, which
/// includes every container it has no reader for.
fn symphonia_technical(path: &Path) -> TechProbe {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let Ok(file) = std::fs::File::open(path) else {
        return TechProbe::default();
    };
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let Ok(probed) = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) else {
        return TechProbe::default();
    };
    let params = probed.format.tracks().first().map(|t| &t.codec_params);
    TechProbe {
        sample_rate: params.and_then(|p| p.sample_rate).map(|s| s as i64),
        channels: params.and_then(|p| p.channels).map(|c| c.count() as i64),
    }
}

/// The decoder behind [`probe_technical`]'s fallback.
mod platform {
    use super::TechProbe;
    use std::path::Path;

    /// `AVAudioFile` reports the file's processing format, which carries both
    /// values. It describes what CoreAudio can decode and nothing else, so
    /// TrueAudio, WavPack and WMA stay unanswered here rather than wrong.
    #[cfg(target_os = "macos")]
    pub fn probe_technical(path: &Path) -> TechProbe {
        use objc2::AllocAnyThread;
        use objc2_avf_audio::AVAudioFile;
        use objc2_foundation::{NSString, NSURL};

        let Some(path) = path.to_str() else {
            return TechProbe::default();
        };
        // A file URL cannot carry an interior NUL, and `+[NSURL
        // fileURLWithPath:]` answers nil for one, which objc2 turns into a
        // panic rather than a None.
        if path.contains('\0') {
            return TechProbe::default();
        }
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        // SAFETY: a live file URL; the call reports failure through its
        // `Result` rather than a null.
        let Ok(file) = (unsafe { AVAudioFile::initForReading_error(AVAudioFile::alloc(), &url) })
        else {
            return TechProbe::default();
        };
        // SAFETY: `file` is live for the length of these reads.
        let format = unsafe { file.processingFormat() };
        let (rate, channels) = unsafe { (format.sampleRate(), format.channelCount()) };
        TechProbe {
            sample_rate: (rate > 0.0).then_some(rate.round() as i64),
            channels: (channels > 0).then_some(channels as i64),
        }
    }

    /// GStreamer's `Discoverer`, which runs its own GMainContext and GMainLoop
    /// and so needs no main loop in the calling thread.
    #[cfg(not(target_os = "macos"))]
    pub fn probe_technical(path: &Path) -> TechProbe {
        // `Discoverer::new` asserts initialisation rather than reporting it,
        // so a caller that has not initialised would panic out of a function
        // that returns a probe. The app initialises at startup; this makes the
        // guarantee local, and `init` is idempotent and cheap.
        if gstreamer::init().is_err() {
            return TechProbe::default();
        }
        let Some(path_str) = path.to_str() else {
            return TechProbe::default();
        };
        let encoded = path_str
            .replace('%', "%25")
            .replace(' ', "%20")
            .replace('#', "%23")
            .replace('?', "%3F");
        let uri = format!("file://{encoded}");

        let timeout = gstreamer::ClockTime::from_seconds(10);
        let Ok(discoverer) = gstreamer_pbutils::Discoverer::new(timeout) else {
            return TechProbe::default();
        };
        let Ok(info) = discoverer.discover_uri(&uri) else {
            return TechProbe::default();
        };
        // The first audio stream is the one being played; a container with
        // several is not something the library indexes separately.
        let Some(audio) = info.audio_streams().into_iter().next() else {
            return TechProbe::default();
        };
        let rate = audio.sample_rate();
        let channels = audio.channels();
        TechProbe {
            sample_rate: (rate > 0).then_some(rate as i64),
            channels: (channels > 0).then_some(channels as i64),
        }
    }
}

/// Average bitrate in kbps from container size and duration. Exact for
/// CBR; for VBR it is the true average, which is what players display.
pub fn avg_bitrate_kbps(file_size_bytes: u64, length_secs: f64) -> Option<i64> {
    if length_secs <= 0.5 {
        return None;
    }
    Some(((file_size_bytes as f64 * 8.0) / length_secs / 1000.0).round() as i64)
}

/// Read a stored bitrate mode in today's words.
///
/// Rows scanned before this was generalised hold "VBR" and "CBR". Rewriting
/// the database to change a display string would be a migration for nothing,
/// so the old spellings are translated on the way out and a rescan quietly
/// replaces them.
pub fn normalize_bitrate_mode(stored: &str) -> &str {
    match stored {
        "VBR" => "Variable",
        "CBR" => "Constant",
        other => other,
    }
}

/// Whether this file's bitrate is variable or constant.
///
/// "Variable" and "Constant" rather than VBR and CBR: this reaches a listener,
/// in the now-playing panel and a library column, not a codec forum.
///
/// Most containers answer by what they are. Everything compressed here is
/// variable by construction, losslessly (FLAC, TrueAudio, WavPack) or lossily
/// (Vorbis, Opus), and PCM is constant because every second is the same size.
/// MP3 is the one that has to be read, and AAC, MP4 and WMA are the ones that
/// cannot be answered cheaply, so they stay unanswered and their column reads
/// blank rather than guessed.
pub fn bitrate_mode(path: &Path) -> Option<&'static str> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "flac" | "ogg" | "opus" | "ape" | "mpc" | "tta" | "wv" => return Some("Variable"),
        "wav" | "aiff" | "aif" => return Some("Constant"),
        "mp3" => {}
        // AAC, MP4 and WMA are encoded either way and neither container says
        // so in a place worth reading a file to reach.
        _ => return None,
    }
    let data = read_prefix(path, 10)?;
    // Skip a leading ID3v2 tag: 10-byte header, syncsafe 28-bit size.
    let audio_start = if data.starts_with(b"ID3") && data.len() >= 10 {
        10 + (((data[6] as u64 & 0x7f) << 21)
            | ((data[7] as u64 & 0x7f) << 14)
            | ((data[8] as u64 & 0x7f) << 7)
            | (data[9] as u64 & 0x7f))
    } else {
        0
    };
    // The Xing/Info block sits inside the first MPEG frame; 4 KiB past the
    // tag comfortably covers every version/channel-mode offset.
    let window = read_range(path, audio_start, 4096)?;
    // LAME and friends write "Xing" into the first frame for a variable-rate
    // encode and "Info" for a constant one. Neither present means unknown.
    if window.windows(4).any(|w| w == b"Xing") {
        Some("Variable")
    } else if window.windows(4).any(|w| w == b"Info") {
        Some("Constant")
    } else {
        None
    }
}

fn read_prefix(path: &Path, n: usize) -> Option<Vec<u8>> {
    read_range(path, 0, n)
}

fn read_range(path: &Path, start: u64, n: usize) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf).ok()?;
    buf.truncate(read);
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal valid PCM WAV: 44-byte header + one frame. Symphonia parses
    // this from the header alone — no fixtures needed, fully deterministic.
    fn write_test_wav(path: &std::path::Path, sample_rate: u32, channels: u16) {
        let data_len = (channels as u32) * 2; // one 16-bit frame
        let byte_rate = sample_rate * channels as u32 * 2;
        let block_align = channels * 2;
        let mut buf = Vec::new();
        buf.extend(b"RIFF");
        buf.extend(&(36 + data_len).to_le_bytes());
        buf.extend(b"WAVE");
        buf.extend(b"fmt ");
        buf.extend(&16u32.to_le_bytes());
        buf.extend(&1u16.to_le_bytes()); // PCM
        buf.extend(&channels.to_le_bytes());
        buf.extend(&sample_rate.to_le_bytes());
        buf.extend(&byte_rate.to_le_bytes());
        buf.extend(&block_align.to_le_bytes());
        buf.extend(&16u16.to_le_bytes()); // bits per sample
        buf.extend(b"data");
        buf.extend(&data_len.to_le_bytes());
        buf.extend(std::iter::repeat(0u8).take(data_len as usize));
        std::fs::write(path, buf).unwrap();
    }

    #[test]
    fn probe_reads_sample_rate_and_channels_from_wav_header() {
        let p = std::env::temp_dir().join("sparkamp_techprobe_test.wav");
        write_test_wav(&p, 44100, 2);
        let t = probe_technical(&p);
        assert_eq!(t.sample_rate, Some(44100));
        assert_eq!(t.channels, Some(2));
        std::fs::remove_file(&p).ok();
    }

    // Full valid MPEG1 Layer3 frames (128 kbps, 44.1 kHz, stereo, silent):
    // 417-byte frames so symphonia's mpa reader accepts the stream. Guards
    // the `mp3` cargo feature — without it the probe rejects every MP3 and
    // the library's technical columns stay NULL (phase-1 user-pass bug).
    fn write_probeable_mp3(path: &std::path::Path) {
        let mut buf = Vec::new();
        for _ in 0..4 {
            buf.extend(&[0xFF, 0xFB, 0x90, 0x00]);
            buf.extend(std::iter::repeat(0u8).take(413));
        }
        std::fs::write(path, buf).unwrap();
    }

    #[test]
    fn probe_reads_sample_rate_from_mp3() {
        let p = std::env::temp_dir().join("sparkamp_techprobe_test_probe.mp3");
        write_probeable_mp3(&p);
        let t = probe_technical(&p);
        assert_eq!(t.sample_rate, Some(44100));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn probe_survives_unreadable_file() {
        let t = probe_technical(std::path::Path::new("/nonexistent/x.mp3"));
        assert_eq!(t.sample_rate, None);
        assert_eq!(t.channels, None);
    }

    #[test]
    fn avg_bitrate_math() {
        // 1 MB over 25 s ≈ 320 kbps; degenerate durations yield None.
        assert_eq!(avg_bitrate_kbps(1_000_000, 25.0), Some(320));
        assert_eq!(avg_bitrate_kbps(1_000_000, 0.0), None);
        assert_eq!(avg_bitrate_kbps(0, 25.0), Some(0));
    }

    // Build a fake MP3: optional ID3v2 header (10-byte header + payload),
    // then bytes that contain (or don't) a Xing/Info marker.
    fn write_fake_mp3(path: &std::path::Path, id3_payload_len: u32, marker: Option<&[u8]>) {
        let mut buf = Vec::new();
        if id3_payload_len > 0 {
            buf.extend(b"ID3");
            buf.extend(&[3u8, 0, 0]); // version 2.3, no flags
            // Syncsafe 28-bit size, 7 bits per byte.
            let s = id3_payload_len;
            buf.extend(&[
                ((s >> 21) & 0x7f) as u8,
                ((s >> 14) & 0x7f) as u8,
                ((s >> 7) & 0x7f) as u8,
                (s & 0x7f) as u8,
            ]);
            buf.extend(std::iter::repeat(0u8).take(id3_payload_len as usize));
        }
        buf.extend(&[0xFF, 0xFB, 0x90, 0x00]); // MPEG1 Layer3 frame sync
        buf.extend(std::iter::repeat(0u8).take(32));
        if let Some(m) = marker {
            buf.extend(m);
        }
        buf.extend(std::iter::repeat(0u8).take(64));
        std::fs::write(path, buf).unwrap();
    }

    #[test]
    fn a_xing_marker_means_a_variable_rate() {
        let p = std::env::temp_dir().join("sparkamp_vbr_test.mp3");
        write_fake_mp3(&p, 0, Some(b"Xing"));
        assert_eq!(bitrate_mode(&p), Some("Variable"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn an_info_marker_means_a_constant_rate_and_id3_is_skipped() {
        let p = std::env::temp_dir().join("sparkamp_cbr_test.mp3");
        // 5000-byte ID3 tag: marker sits beyond a naive fixed-window scan,
        // so this fails unless the ID3 header size is actually honored.
        write_fake_mp3(&p, 5000, Some(b"Info"));
        assert_eq!(bitrate_mode(&p), Some("Constant"));
        std::fs::remove_file(&p).ok();
    }

    /// An MP3 with neither marker says nothing, and so does a file that is not
    /// there. PCM is answered by what it is rather than by reading it.
    #[test]
    fn an_unmarked_mp3_yields_none_while_pcm_is_constant() {
        let p = std::env::temp_dir().join("sparkamp_nomode_test.mp3");
        write_fake_mp3(&p, 0, None);
        assert_eq!(bitrate_mode(&p), None);
        std::fs::remove_file(&p).ok();
        assert_eq!(bitrate_mode(std::path::Path::new("/nonexistent.mp3")), None);
        let w = std::env::temp_dir().join("sparkamp_nomode_test.wav");
        write_test_wav(&w, 44100, 2);
        assert_eq!(bitrate_mode(&w), Some("Constant"));
        std::fs::remove_file(&w).ok();
    }

    /// Rows written before the mode was generalised are read in today's words.
    #[test]
    fn stored_abbreviations_read_as_words() {
        assert_eq!(normalize_bitrate_mode("VBR"), "Variable");
        assert_eq!(normalize_bitrate_mode("CBR"), "Constant");
        assert_eq!(normalize_bitrate_mode("Variable"), "Variable");
        assert_eq!(normalize_bitrate_mode(""), "");
    }
}
