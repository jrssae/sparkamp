//! Integration-style tests against a temp SQLite DB, split by topic.
//!
//! One file per area of `media_library`. This was a single 2,982-line file;
//! the split is by the section headings it already carried, so each test sits
//! where it did relative to its neighbours. The shared helpers stay here and
//! reach every submodule through `use super::*`.

use super::*;
use std::fs;
use tempfile::NamedTempFile;

fn temp_lib() -> (MediaLibrary, NamedTempFile) {
    let db_file = NamedTempFile::with_suffix(".db").unwrap();
    let lib = MediaLibrary::open_at(db_file.path()).unwrap();
    (lib, db_file)
}

fn temp_dir_with_files(extension: &str, count: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..count {
        let file_path = dir.path().join(format!("track_{}.{}", i, extension));
        fs::write(&file_path, b"fake audio data").unwrap();
    }
    dir
}

/// A real WAV, for the tests that need the scanner to read technical
/// properties off disk rather than take them from a row.
/// Minimal valid PCM WAV with exactly `secs` seconds of silence. Symphonia
/// derives duration straight from the header (data chunk size ÷ byte rate),
/// so this is enough to make `avg_bitrate_kbps`'s >0.5s threshold trip
/// without needing a real audio fixture.
fn write_test_wav(path: &std::path::Path, sample_rate: u32, channels: u16, secs: f64) {
    let bytes_per_frame = channels as u32 * 2;
    let data_len = (sample_rate as f64 * secs) as u32 * bytes_per_frame;
    let byte_rate = sample_rate * bytes_per_frame;
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
    fs::write(path, buf).unwrap();
}

fn track_row_exists(lib: &MediaLibrary, path: &str) -> bool {
    lib.conn
        .query_row(
            "SELECT 1 FROM tracks WHERE path = ?1",
            rusqlite::params![path],
            |_| Ok(()),
        )
        .is_ok()
}

mod artwork;
mod devices;
mod folders;
mod play_stats;
mod queries;
mod scan;
mod tracks;
