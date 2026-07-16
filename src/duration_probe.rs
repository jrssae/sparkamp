//! Background audio-duration probing.
//!
//! Two probers are tried in order, fastest first:
//!
//! ## 1. Symphonia (fast, no audio output)
//!
//! Reads only the container header — no decoding.  Works for:
//!
//! | Format | Source of duration |
//! |--------|-------------------|
//! | MP3 (with Xing/Info header) | Xing frame: total frame count × frame duration |
//! | FLAC | `STREAMINFO` block: exact sample count ÷ sample rate |
//! | OGG Vorbis / Opus | Stream info headers |
//! | WAV / AIFF | Data chunk size ÷ (sample rate × channels × bit depth) |
//! | M4A / AAC | MP4 `mvhd` box |
//!
//! Fails for raw CBR MP3 without a Xing header (returns `None`).
//!
//! ## 2. GStreamer Discoverer (fallback, handles CBR MP3)
//!
//! `gstreamer_pbutils::Discoverer` runs a full GStreamer pipeline internally,
//! creating its own GMainContext/GMainLoop so it is safe to call from any
//! thread.  For CBR MP3, GStreamer estimates duration from file size ÷ bitrate.
//!
//! ## Thread model
//!
//! [`spawn_probes`] hands all paths to Rayon's global thread pool.  Rayon
//! limits concurrency to one task per logical CPU.  Results are sent back to
//! the calling thread through a `std::sync::mpsc::Sender`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

// ---------------------------------------------------------------------------
// probe_duration
// ---------------------------------------------------------------------------

/// Read the duration of a single audio file from its container header.
///
/// Returns `None` if the file cannot be opened, the format is unrecognised by
/// Symphonia, or the container does not advertise a duration (e.g. raw CBR
/// MP3 without a Xing header).
pub fn probe_duration(path: &Path) -> Option<Duration> {
    let file = std::fs::File::open(path).ok()?;
    let mss  = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .ok()?;

    let track    = probed.format.default_track()?;
    let tb       = track.codec_params.time_base?;
    let n_frames = track.codec_params.n_frames?;
    let time     = tb.calc_time(n_frames);

    Some(Duration::from_secs_f64(time.seconds as f64 + time.frac))
}

// ---------------------------------------------------------------------------
// probe_duration_full  (Symphonia header read, then GStreamer fallback)
// ---------------------------------------------------------------------------

/// Full single-file duration probe: the fast Symphonia header read first, then
/// the GStreamer Discoverer fallback for CBR MP3 and other containers whose
/// header lacks an explicit frame count. This is the SAME two-step the
/// library's background [`spawn_probes`] uses — call it anywhere a single
/// file's duration is needed (e.g. the burn-list add path) so a headerless but
/// perfectly playable file is not misreported as unreadable. Requires
/// `gstreamer::init()` to have run (the app does this at startup).
pub fn probe_duration_full(path: &Path) -> Option<Duration> {
    probe_duration(path).or_else(|| discover_duration(path))
}

// ---------------------------------------------------------------------------
// discover_duration  (GStreamer Discoverer fallback)
// ---------------------------------------------------------------------------

/// Probe the duration of an audio file using `gstreamer_pbutils::Discoverer`.
///
/// The Discoverer runs its own internal GMainContext and GMainLoop, making it
/// safe to call from any thread — including Rayon worker threads — without a
/// running GLib main loop in the calling thread.
///
/// For CBR MP3 files without a Xing/Info header (which Symphonia cannot
/// measure), GStreamer's `mpegaudioparse` estimates duration from file size ÷
/// bitrate.  This estimate appears quickly and is accurate enough for display.
pub fn discover_duration(path: &Path) -> Option<Duration> {
    let path_str = path.to_str()?;
    let encoded = path_str
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace('#', "%23")
        .replace('?', "%3F");
    let uri = format!("file://{encoded}");

    // 10-second timeout per file is very generous for local storage.
    let timeout = gstreamer::ClockTime::from_seconds(10);
    let discoverer = gstreamer_pbutils::Discoverer::new(timeout).ok()?;
    let info = discoverer.discover_uri(&uri).ok()?;
    let dur  = info.duration()?;
    Some(Duration::from_nanos(dur.nseconds()))
}

// ---------------------------------------------------------------------------
// spawn_probes
// ---------------------------------------------------------------------------

/// Dispatch duration probes for all `paths` on the Rayon global thread pool.
///
/// For each path that yields a duration, `result_tx.send((path, duration))`
/// is called.  For each path that is confirmed missing from disk (not just
/// un-probeable), `missing_tx.send(path)` is called so the caller can mark
/// the track as broken without waiting for a playback error.
///
/// This function returns immediately; the probes run in the background.
// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// probe_duration must return None for a path that does not exist on disk.
    #[test]
    fn probe_duration_returns_none_for_nonexistent_file() {
        let result = probe_duration(Path::new("/no/such/file.mp3"));
        assert!(result.is_none());
    }

    /// probe_duration must return None for a path that exists but is not audio.
    #[test]
    fn probe_duration_returns_none_for_non_audio_file() {
        // /dev/null exists on Linux and is not a valid audio container.
        let result = probe_duration(Path::new("/dev/null"));
        assert!(result.is_none());
    }

    /// probe_duration_full must fall through to the GStreamer Discoverer when
    /// Symphonia can't measure the header — a real CBR MP3 without a Xing
    /// header returns None from `probe_duration` but a duration from the full
    /// probe. Regression guard: the burn-list add path was calling only
    /// `probe_duration` and rejecting such (perfectly playable) files as
    /// unreadable (2026-07-15).
    #[test]
    #[ignore] // needs a real headerless-CBR MP3 + gstreamer; run with --ignored
    fn probe_duration_full_recovers_headerless_cbr_via_gstreamer() {
        gstreamer::init().ok();
        // A CBR MP3 with no Xing/Info header (path supplied by the tester).
        let p = std::path::Path::new(
            "/var/mnt/Blackbeard/Music/Billboard Top 100 of 2014/\
             24. One Direction - Story Of My Life.mp3",
        );
        if !p.exists() {
            eprintln!("sample file absent — skipping");
            return;
        }
        // The bug: probe_duration alone may return None here …
        // … but the full probe must recover a duration via GStreamer.
        assert!(
            probe_duration_full(p).is_some(),
            "full probe must measure a playable CBR MP3 the header read misses"
        );
    }

    /// spawn_probes must send the path on missing_tx when the file does not exist.
    #[test]
    fn spawn_probes_reports_missing_file_on_missing_tx() {
        gstreamer::init().ok();
        let (result_tx, _result_rx) = std::sync::mpsc::channel();
        let (missing_tx, missing_rx) = std::sync::mpsc::channel();
        let path = PathBuf::from("/no/such/file.mp3");
        spawn_probes(vec![path.clone()], result_tx, missing_tx);
        let received = missing_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("missing_tx should fire for a nonexistent file");
        assert_eq!(received, path);
    }

    /// spawn_probes must NOT send on missing_tx for a path that exists (even if
    /// unprobeable), and must NOT crash.
    #[test]
    fn spawn_probes_does_not_report_existing_file_as_missing() {
        gstreamer::init().ok();
        let (result_tx, _result_rx) = std::sync::mpsc::channel();
        let (missing_tx, missing_rx) = std::sync::mpsc::channel();
        // /dev/null exists — should never appear on missing_tx.
        spawn_probes(vec![PathBuf::from("/dev/null")], result_tx, missing_tx);
        // Give the thread time to finish.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            missing_rx.try_recv().is_err(),
            "/dev/null exists and must not be reported as missing"
        );
    }
}

pub fn spawn_probes(
    paths: Vec<PathBuf>,
    result_tx: std::sync::mpsc::Sender<(PathBuf, Duration)>,
    missing_tx: std::sync::mpsc::Sender<PathBuf>,
) {
    // Spawn a single OS thread to drive Rayon without blocking the GTK loop.
    std::thread::spawn(move || {
        use rayon::prelude::*;
        paths.par_iter().for_each(|path| {
            // If the file is not on disk at all, notify the caller immediately
            // so it can mark the track broken without waiting for playback.
            if !path.exists() {
                let _ = missing_tx.send(path.clone());
                return;
            }
            // Header read (no decoding), then the GStreamer fallback for CBR
            // MP3 and other formats whose container header lacks a frame count.
            let dur = probe_duration_full(path);
            if let Some(dur) = dur {
                let _ = result_tx.send((path.clone(), dur));
            }
        });
    });
}
