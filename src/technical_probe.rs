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
}
