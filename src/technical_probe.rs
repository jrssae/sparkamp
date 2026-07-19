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

/// Read sample rate and channel count from the file's codec parameters.
/// Returns an empty probe on any error — scan rows degrade to NULL rather
/// than failing the scan.
pub fn probe_technical(path: &Path) -> TechProbe {
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

/// Average bitrate in kbps from container size and duration. Exact for
/// CBR; for VBR it is the true average, which is what players display.
pub fn avg_bitrate_kbps(file_size_bytes: u64, length_secs: f64) -> Option<i64> {
    if length_secs <= 0.5 {
        return None;
    }
    Some(((file_size_bytes as f64 * 8.0) / length_secs / 1000.0).round() as i64)
}

/// Detect VBR vs CBR for MP3 files by the Xing/Info header convention:
/// LAME and friends write "Xing" into the first frame for VBR files and
/// "Info" for CBR. Absence of both means unknown — display blank rather
/// than guessing.
pub fn mp3_bitrate_mode(path: &Path) -> Option<&'static str> {
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("mp3"))
        .unwrap_or(false)
    {
        return None;
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
    if window.windows(4).any(|w| w == b"Xing") {
        Some("VBR")
    } else if window.windows(4).any(|w| w == b"Info") {
        Some("CBR")
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
    fn xing_marker_means_vbr() {
        let p = std::env::temp_dir().join("sparkamp_vbr_test.mp3");
        write_fake_mp3(&p, 0, Some(b"Xing"));
        assert_eq!(mp3_bitrate_mode(&p), Some("VBR"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn info_marker_means_cbr_and_id3_is_skipped() {
        let p = std::env::temp_dir().join("sparkamp_cbr_test.mp3");
        // 5000-byte ID3 tag: marker sits beyond a naive fixed-window scan,
        // so this fails unless the ID3 header size is actually honored.
        write_fake_mp3(&p, 5000, Some(b"Info"));
        assert_eq!(mp3_bitrate_mode(&p), Some("CBR"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn no_marker_and_non_mp3_yield_none() {
        let p = std::env::temp_dir().join("sparkamp_nomode_test.mp3");
        write_fake_mp3(&p, 0, None);
        assert_eq!(mp3_bitrate_mode(&p), None);
        std::fs::remove_file(&p).ok();
        assert_eq!(mp3_bitrate_mode(std::path::Path::new("/nonexistent.mp3")), None);
        let w = std::env::temp_dir().join("sparkamp_nomode_test.wav");
        write_test_wav(&w, 44100, 2);
        assert_eq!(mp3_bitrate_mode(&w), None);
        std::fs::remove_file(&w).ok();
    }
}
