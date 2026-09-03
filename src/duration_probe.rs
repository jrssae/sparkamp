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
/// perfectly playable file is not misreported as unreadable.
pub fn probe_duration_full(path: &Path) -> Option<Duration> {
    probe_duration(path).or_else(|| discover_duration(path))
}

// ---------------------------------------------------------------------------
// discover_duration  (GStreamer Discoverer fallback)
// ---------------------------------------------------------------------------

/// Probe the duration of an audio file by decoding it.
///
/// The fallback for what a header cannot answer. Symphonia reads the container
/// and is done in microseconds, but a CBR MP3 with no Xing/Info header does not
/// say how long it is anywhere — the only way to know is to look at the audio.
///
/// Safe to call from any thread, including Rayon workers, on both platforms.
///
/// Which decoder does the looking is [`platform`]'s business. Callers ask for a
/// duration; nothing here is theirs to know.
///
/// A path holding an interior NUL is refused before either decoder sees it.
/// Neither can express one as a URI, and both answer a C string conversion
/// failure by panicking rather than by reporting it. This function returns an
/// `Option`, and a Rayon worker probing a library is no place to unwind.
pub fn discover_duration(path: &Path) -> Option<Duration> {
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return None;
    }
    platform::discover_duration(path)
}

/// The decoder behind [`discover_duration`].
mod platform {
    use super::Duration;
    use std::path::Path;

    /// AVFoundation. `AVAudioFile` reports the file's length in frames at its
    /// own processing rate, which for a headerless CBR MP3 means CoreAudio has
    /// parsed the whole stream — the same work GStreamer's Discoverer does,
    /// and the same answer.
    ///
    /// No GStreamer here is the point: it is what lets the App Store build,
    /// which bundles none, still report a duration for such a file.
    #[cfg(target_os = "macos")]
    pub fn discover_duration(path: &Path) -> Option<Duration> {
        use objc2::AllocAnyThread;
        use objc2_avf_audio::AVAudioFile;
        use objc2_foundation::{NSString, NSURL};

        let path = path.to_str()?;
        // A file URL cannot carry an interior NUL, and `+[NSURL
        // fileURLWithPath:]` answers nil for one — which objc2 turns into a
        // panic rather than a None.
        if path.contains('\0') {
            return None;
        }
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        // SAFETY: a live file URL; the call reports failure through its
        // `Result` rather than a null.
        let file =
            unsafe { AVAudioFile::initForReading_error(AVAudioFile::alloc(), &url) }.ok()?;
        // SAFETY: `file` is live for the length of these two reads.
        let (frames, rate) = unsafe { (file.length(), file.processingFormat().sampleRate()) };
        if frames <= 0 || rate <= 0.0 {
            return None;
        }
        Some(Duration::from_secs_f64(frames as f64 / rate))
    }

    /// GStreamer's `Discoverer`, which runs its own GMainContext and GMainLoop
    /// and so needs no main loop in the calling thread.
    ///
    /// Requires `gstreamer::init()` to have run; the app does that at startup.
    #[cfg(not(target_os = "macos"))]
    pub fn discover_duration(path: &Path) -> Option<Duration> {
        let path_str = path.to_str()?;
        let encoded = path_str
            .replace('%', "%25")
            .replace(' ', "%20")
            .replace('#', "%23")
            .replace('?', "%3F");
        let uri = format!("file://{encoded}");

        // 10 seconds per file is very generous for local storage.
        let timeout = gstreamer::ClockTime::from_seconds(10);
        let discoverer = gstreamer_pbutils::Discoverer::new(timeout).ok()?;
        let info = discoverer.discover_uri(&uri).ok()?;
        let dur = info.duration()?;
        Some(Duration::from_nanos(dur.nseconds()))
    }
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

    /// The decode fallback must measure every format this platform plays,
    /// including the one it exists for: a CBR MP3 whose header does not say
    /// how long it is.
    /// `SPARKAMP_FORMAT_DIR=<dir> cargo test --lib \
    ///   discover_duration_measures_real_files -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn discover_duration_measures_real_files() {
        let Some(dir) = std::env::var_os("SPARKAMP_FORMAT_DIR") else {
            println!("set SPARKAMP_FORMAT_DIR to a directory of samples");
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let mut measured = 0;
        for name in [
            "t.mp3", "t.flac", "t.ogg", "t.opus", "t.wav", "t.aac", "t.m4a", "t.aiff",
            "cbr-headerless.mp3",
        ] {
            let path = dir.join(name);
            if !path.exists() {
                continue;
            }
            match discover_duration(&path) {
                Some(d) => {
                    println!("  {name:20} {:.3} s", d.as_secs_f64());
                    // Every sample is the same ten-second tone. A decoder that
                    // guessed from file size would be wrong by more than this
                    // on the compressed ones.
                    assert!(
                        (d.as_secs_f64() - 10.0).abs() < 0.5,
                        "{name}: expected about 10 s, measured {:.3}",
                        d.as_secs_f64()
                    );
                    measured += 1;
                }
                None => println!("  {name:20} not measurable"),
            }
        }
        assert!(measured > 0, "nothing was measured at all");
    }

    /// A path a file URL cannot express must answer `None`, not panic.
    #[test]
    fn a_path_with_a_nul_byte_is_not_measurable() {
        assert!(discover_duration(Path::new("/tmp/we\0ird")).is_none());
    }
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

    /// probe_duration_full must fall through to the decode when Symphonia
    /// cannot measure the header — a real CBR MP3 without a Xing header
    /// returns None from `probe_duration` but a duration from the full probe.
    /// Regression guard: the burn-list add path was calling only
    /// `probe_duration` and rejecting such (perfectly playable) files as
    /// unreadable (2026-07-15).
    ///
    /// Not "via GStreamer" any more, which is the point of the rename: macOS
    /// falls through to AVFoundation instead, and the contract this guards is
    /// the fallback happening at all rather than which decoder does it.
    #[test]
    #[ignore] // needs a real headerless-CBR MP3; run with --ignored
    fn probe_duration_full_recovers_headerless_cbr() {
        #[cfg(not(target_os = "macos"))]
        #[cfg(not(target_os = "macos"))]
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
        #[cfg(not(target_os = "macos"))]
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
        #[cfg(not(target_os = "macos"))]
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

    /// A batch far larger than the pool must still get through all of it.
    ///
    /// The probes run in a bounded pool of their own now; wiring that up wrong
    /// — a pool that never starts, or one whose work is dropped past the first
    /// `PROBE_THREADS` items — would silently leave most rows unfinished.
    #[test]
    fn spawn_probes_reports_every_path_in_a_batch_larger_than_the_pool() {
        let n = super::PROBE_THREADS * 5;
        let paths: Vec<PathBuf> = (0..n)
            .map(|i| PathBuf::from(format!("/no/such/probe{i}.mp3")))
            .collect();
        let (result_tx, _result_rx) = std::sync::mpsc::channel();
        let (missing_tx, missing_rx) = std::sync::mpsc::channel();
        spawn_probes(paths.clone(), result_tx, missing_tx);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..n {
            let got = missing_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("every missing path is reported");
            seen.insert(got);
        }
        assert_eq!(seen, paths.into_iter().collect());
    }
}

/// How many files are read at once.
///
/// Deliberately small and fixed rather than one per core. Probing is dominated
/// by file I/O, not by arithmetic, so past a handful of concurrent readers
/// there is no throughput left to win — and on rotational media or a network
/// mount there is throughput to lose, because every extra reader is another
/// seek competing for the same head or the same link. A 36k folder add on a
/// 16-core machine would otherwise put sixteen readers on the disk at once.
const PROBE_THREADS: usize = 4;

/// The bounded pool every file-reading background job shares, built once.
///
/// Their own pool, not Rayon's global one. The global pool also serves every
/// `rayon::spawn` in the FFI — library scans, ReplayGain, dedupe — and a
/// `par_iter` over 36,000 paths occupies every worker in it until it finishes,
/// so those jobs would queue behind the whole probe run.
///
/// Deliberately small — see [`PROBE_THREADS`]. Reachable from outside this
/// module so the macOS bridge's row probes use it too rather than the global
/// pool, which would put an unbounded number of readers on the same disk.
///
/// `None` if the pool could not be built, which the caller answers by probing
/// sequentially rather than by failing.
pub(crate) fn shared_probe_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: std::sync::OnceLock<Option<rayon::ThreadPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(PROBE_THREADS)
            .thread_name(|i| format!("sparkamp-probe-{i}"))
            .build()
            .ok()
    })
    .as_ref()
}

pub fn spawn_probes(
    paths: Vec<PathBuf>,
    result_tx: std::sync::mpsc::Sender<(PathBuf, Duration)>,
    missing_tx: std::sync::mpsc::Sender<PathBuf>,
) {
    if paths.is_empty() {
        return;
    }
    // Spawn a single OS thread to drive the pool without blocking the caller's
    // main loop.
    std::thread::spawn(move || {
        let probe_one = |path: &PathBuf| {
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
        };
        match shared_probe_pool() {
            Some(pool) => pool.install(|| {
                use rayon::prelude::*;
                paths.par_iter().for_each(probe_one);
            }),
            None => paths.iter().for_each(probe_one),
        }
    });
}
