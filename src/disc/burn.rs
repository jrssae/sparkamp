//! Burn audio CDs and data discs.
//!
//! The two platforms take different roads to the drive. Linux spawns
//! `cdrskin`/`xorriso`, whose arguments come from **pure functions** with
//! exact-args unit tests. macOS calls DiscRecording.framework in-process
//! ([`super::discrecording`]) — App Sandbox blocks spawning `/usr/bin/drutil`,
//! and this was the last place that did.
//!
//! Everything either road needs in common is shared and testable without a
//! disc:
//!
//! - Audio preparation (decode → Red Book WAV) runs the same GStreamer
//!   machinery as ripping and IS live-tested without media.
//! - [`wav_redbook_span`] locates and validates the PCM the mac backend feeds
//!   the drive, and is a pure function over bytes.
//! - Both burn paths are cancellable through the same global flag —
//!   [`request_cancel`] — and both report progress as a [`BurnProgress`].
//!
//! What must wait for blank media is the write itself. The `live_hw_*` tests
//! below are `#[ignore]`d because each run costs a disc.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
// Used only by the progress-channel plumbing, which is all cfg'd out on macOS
// (drutil owns the progress bar there). Gate the import to match, or macOS
// warns on an unused import while Linux fails to build without it.
#[cfg(not(target_os = "macos"))]
use std::sync::mpsc;

use super::burnlist::BurnItem;
use super::cdtext::CdTextSheet;
use super::{MediaKind, OpticalDrive};

/// Standard blank CD-R audio capacity in seconds (80-minute media). Used
/// when the platform can't report free blocks for audio (the UIs treat it as
/// the default guard; Opus verifies against real media).
pub const AUDIO_CD_CAPACITY_SECS: u32 = 80 * 60;

/// Audio capacity of the loaded media in seconds: free CD frames are 1/75 s
/// each. Falls back to the 80-minute standard when the probe reported no
/// free blocks (common for audio-blank probing).
pub fn audio_capacity_secs(drive: &OpticalDrive) -> u32 {
    let blocks = drive.media.free_bytes / 2048;
    if blocks == 0 {
        AUDIO_CD_CAPACITY_SECS
    } else {
        (blocks / 75) as u32
    }
}

/// What has to happen to the loaded media before a burn can start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EraseDecision {
    /// Blank media — burn straight away.
    None,
    /// Rewritable with content — erase first, but ONLY after the user
    /// explicitly confirms (never auto-blank).
    EraseAfterConfirm,
    /// Write-once with content — refuse the burn outright.
    Refuse,
}

/// Decide the erase handling for the loaded media. Pure — unit-tested
/// against the media matrix.
pub fn erase_decision(drive: &OpticalDrive) -> EraseDecision {
    if !drive.media.present {
        return EraseDecision::Refuse; // nothing to burn onto
    }
    if drive.media.is_blank {
        return EraseDecision::None;
    }
    if drive.media.rewritable || matches!(drive.media.kind, MediaKind::DvdRam) {
        return EraseDecision::EraseAfterConfirm;
    }
    EraseDecision::Refuse
}

// ---------------------------------------------------------------------------
// Audio preparation (shared GStreamer path — live-testable without media)
// ---------------------------------------------------------------------------


/// Transcode one burn-list entry to a Red Book WAV. Blocking (worker
/// threads loop per track for progress/cancel, same shape as ripping).
pub fn prepare_wav(src: &Path, out: &Path) -> Result<(), String> {
    prepare_wav_observed(src, out, |_| {})
}

/// [`prepare_wav`] with a position feed: `on_position` gets the source
/// position in seconds roughly twice a second while the transcode runs — the
/// within-track fraction for [`run_job`]'s "Preparing i/N" progress.
pub fn prepare_wav_observed(
    src: &Path,
    out: &Path,
    mut on_position: impl FnMut(f64),
) -> Result<(), String> {
    super::transcode::to_red_book_wav(src, out, &mut on_position)
}

/// The staged WAV name for burn-list position `index` (0-based): "01.wav",
/// "02.wav"… — numeric names keep both burn tools' track order identical to
/// the list order.
pub fn staged_wav_name(index: usize) -> String {
    format!("{:02}.wav", index + 1)
}

/// Where the PCM payload sits inside a staged WAV: `(byte offset, declared
/// length)`. The caller clamps the length against the real file size, since
/// only the file knows whether the writer finished.
///
/// macOS's burn feeds these bytes to the drive itself rather than handing a
/// file to a tool, so it needs the payload's exact span — and it must not
/// hand the drive anything that isn't Red Book, which is why the `fmt ` chunk
/// is checked rather than skipped. `header` is a prefix of the file; a `data`
/// chunk beyond it reads as absent.
///
/// Chunk-walking rather than a fixed 44-byte offset: `wavenc` is free to emit
/// `LIST`/`INFO` ahead of `data`, and it costs nothing to survive that.
pub fn wav_redbook_span(header: &[u8]) -> Result<(u64, u64), String> {
    if header.len() < 12 || &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".to_string());
    }
    let word = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]);
    let long = |at: usize| {
        u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
    };

    let mut seen_redbook_fmt = false;
    let mut at = 12usize;
    while at + 8 <= header.len() {
        let id = &header[at..at + 4];
        let size = long(at + 4) as usize;
        let body = at + 8;
        match id {
            b"fmt " if body + 16 <= header.len() => {
                // PCM, stereo, 44.1 kHz, 16-bit — Red Book, and the exact
                // shape `transcode::to_red_book_wav` produces.
                seen_redbook_fmt = word(body) == 1
                    && word(body + 2) == 2
                    && long(body + 4) == 44_100
                    && word(body + 14) == 16;
                if !seen_redbook_fmt {
                    return Err(format!(
                        "not Red Book audio: {} Hz, {} channels, {}-bit, format {}",
                        long(body + 4),
                        word(body + 2),
                        word(body + 14),
                        word(body)
                    ));
                }
            }
            b"data" if seen_redbook_fmt => return Ok((body as u64, size as u64)),
            b"data" => return Err("data chunk before a fmt chunk".to_string()),
            _ => {}
        }
        // RIFF chunks are word-aligned: an odd size carries one pad byte.
        let Some(next) = body.checked_add(size).and_then(|n| n.checked_add(size & 1)) else {
            return Err("chunk size overflows the file".to_string());
        };
        at = next;
    }
    Err(format!("no data chunk in the first {} bytes", header.len()))
}

// ---------------------------------------------------------------------------
// Command builders (pure, exact-args unit tests)
// ---------------------------------------------------------------------------

/// cdrskin: burn prepared WAVs as an audio CD, DAO, padding subframe gaps.
/// `sheet` is the staged CD-TEXT v07t definition sheet (`None` skips
/// CD-TEXT); when present it must precede `-dao` per cdrskin's docs for
/// `input_sheet_v07t=` (SAO/DAO-only option).
#[cfg_attr(target_os = "macos", allow(dead_code))] // Linux burn arm
pub fn cdrskin_audio_args(device: &str, wavs: &[PathBuf], sheet: Option<&Path>) -> Vec<String> {
    // -v: verbose progress ("Track NN: X of Y MB written" on stdout) —
    // `run_job`'s streamed burn parses these lines via
    // `parse_cdrskin_progress`; without -v cdrskin prints none of them.
    let mut args = vec![
        format!("dev={device}"),
        "blank=as_needed".to_string(),
        "-v".to_string(),
    ];
    if let Some(sheet) = sheet {
        args.push(format!("input_sheet_v07t={}", sheet.display()));
    }
    args.push("-dao".to_string());
    args.push("-audio".to_string());
    args.push("-pad".to_string());
    args.extend(wavs.iter().map(|w| w.display().to_string()));
    args
}

/// cdrskin: fast-blank a rewritable disc.
#[cfg_attr(target_os = "macos", allow(dead_code))] // Linux burn arm
pub fn cdrskin_erase_args(device: &str) -> Vec<String> {
    vec![format!("dev={device}"), "blank=fast".to_string()]
}

/// xorriso: burn a staged folder as an ISO9660+Joliet data disc.
#[cfg_attr(target_os = "macos", allow(dead_code))] // Linux burn arm
pub fn xorriso_data_args(device: &str, staged_dir: &Path) -> Vec<String> {
    vec![
        "-outdev".to_string(),
        device.to_string(),
        "-blank".to_string(),
        "as_needed".to_string(),
        "-joliet".to_string(),
        "on".to_string(),
        "-map".to_string(),
        staged_dir.display().to_string(),
        "/".to_string(),
        "-commit".to_string(),
    ]
}

// ---------------------------------------------------------------------------
// Data staging
// ---------------------------------------------------------------------------

/// Write a playlist file into the staged data-disc root listing the staged
/// audio files in burn order — the classic MP3-CD companion file most car
/// stereos and players read. `use_m3u` mirrors the app-wide playlist-format
/// setting (false = m3u8/UTF-8, the default).
///
/// `items` are the queue entries the staged files came from, in the same
/// order, and supply each entry's `#EXTINF` line: a player reading the disc
/// then shows "Artist - Title" and a running time instead of a file name.
/// The queue already carries both — [`BurnItem::display`] and
/// [`BurnItem::duration_secs`] — so nothing has to be re-read from the
/// library here. An entry with no matching item is written bare, which is
/// what every entry looked like before 2026-08-10.
///
/// The file name is deliberately generic rather than the source playlist's:
/// a burn queue can be filled from several playlists and from loose files,
/// so there is no one name to take, and `playlist.m3u8` at the disc root is
/// the convention players look for.
pub fn write_data_playlist(
    staged_dir: &Path,
    staged_files: &[PathBuf],
    items: &[BurnItem],
    use_m3u: bool,
) -> Result<PathBuf, String> {
    let name = if use_m3u { "playlist.m3u" } else { "playlist.m3u8" };
    let path = staged_dir.join(name);
    let mut body = String::from("#EXTM3U\n");
    for (i, f) in staged_files.iter().enumerate() {
        // Entries are relative to the disc root (players resolve them there).
        let Some(n) = f.file_name() else { continue };
        if let Some(item) = items.get(i) {
            // -1 for unknown length, matching the library's own writer.
            let secs = item.duration_secs.map(|s| s as i64).unwrap_or(-1);
            let display = if item.display.is_empty() {
                n.to_string_lossy().into_owned()
            } else {
                item.display.clone()
            };
            body.push_str(&format!("#EXTINF:{secs},{display}\n"));
        }
        body.push_str(&n.to_string_lossy());
        body.push('\n');
    }
    std::fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// Stage data-mode files into one flat directory (the future disc root).
/// Hard-links when possible (same filesystem, instant), copies otherwise.
/// Name collisions get " (2)", " (3)"… suffixes before the extension.
pub fn stage_data_files(files: &[PathBuf], staged_dir: &Path) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(staged_dir)
        .map_err(|e| format!("create {}: {e}", staged_dir.display()))?;
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        let base = f
            .file_name()
            .ok_or_else(|| format!("no file name: {}", f.display()))?
            .to_string_lossy()
            .into_owned();
        let mut target = staged_dir.join(&base);
        let mut n = 2;
        while target.exists() {
            let stem = Path::new(&base)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| base.clone());
            let ext = Path::new(&base)
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            target = staged_dir.join(format!("{stem} ({n}){ext}"));
            n += 1;
        }
        if std::fs::hard_link(f, &target).is_err() {
            std::fs::copy(f, &target)
                .map_err(|e| format!("copy {} → {}: {e}", f.display(), target.display()))?;
        }
        out.push(target);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Subprocess runner (cancellable, stderr-tail errors)
// ---------------------------------------------------------------------------
//
// The Linux burn arm. macOS burns through DiscRecording in-process, so nothing
// below it has a mac caller outside the tests — the `allow(dead_code)`s say so
// the same way the `cdrskin`/`xorriso` argument builders above do.

/// Cancel flag for the (single) in-flight burn/erase subprocess. Reset when a
/// new run starts; set by `request_cancel`. One concurrent burn is a product
/// assumption (one drive op at a time).
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Ask the running burn/erase child to be killed after the next poll.
pub fn request_cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}

/// Disambiguates the per-run log file (see `log_path` below) when more than
/// one `run_tool_streaming` call is in flight in this process at once — e.g.
/// under `cargo test`'s parallel test threads, where several tests may run
/// the same `program` (`sh`) concurrently. Without this, PID + program name
/// alone collide and two runs' output gets interleaved into the same file.
#[cfg_attr(target_os = "macos", allow(dead_code))] // Linux burn arm
static RUN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Run a burn/erase tool to completion. Polls every 200 ms for exit or a
/// cancel request (cancel kills the child and reports it). Output is judged by
/// [`interpret_exit`] — for most tools the exit status, but drutil needs its
/// text scanned too (it exits 0 even on a failed burn; see there).
///
/// stdout+stderr are captured to a temp file rather than a pipe: burn tools
/// emit a long, unbounded progress stream and an undrained pipe would fill and
/// deadlock the child mid-burn. A file has no such back-pressure.
///
/// A [`BURN_TIMEOUT`] wall-clock ceiling guards against a wedged tool (e.g. the
/// drive stops responding and the child never exits) so the app never hangs.
#[cfg_attr(target_os = "macos", allow(dead_code))] // Linux burn arm
pub fn run_tool(program: &str, args: &[String]) -> Result<(), String> {
    run_tool_streaming(program, args, |_: &str| {})
}

/// [`run_tool`], but every stdout line is teed to `on_line` as it arrives —
/// the live-progress feed for tools (cdrskin with `-v`) that report percent
/// complete on stdout. `on_line` runs on a dedicated reader thread (not the
/// caller's thread), which is why it must be `Send + 'static`: it's moved
/// into `std::thread::spawn`. stderr still goes straight to the log file,
/// same as [`run_tool`] — only stdout is split.
///
/// Cancel, the wall-clock watchdog, and the log-file error tail all behave
/// exactly as [`run_tool`]'s.
#[cfg_attr(target_os = "macos", allow(dead_code))] // Linux burn arm
pub fn run_tool_streaming(
    program: &str,
    args: &[String],
    on_line: impl FnMut(&str) + Send + 'static,
) -> Result<(), String> {
    run_tool_streaming_with_timeout(program, args, BURN_TIMEOUT, on_line)
}

/// Coarse wall-clock ceiling for one burn/erase subprocess. A full audio-CD
/// burn is minutes; 30 min without exit means the tool wedged — kill and report
/// rather than hang the burn UI forever.
#[cfg_attr(target_os = "macos", allow(dead_code))] // Linux burn arm
const BURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

#[cfg_attr(target_os = "macos", allow(dead_code))] // Linux burn arm
fn run_tool_streaming_with_timeout(
    program: &str,
    args: &[String],
    timeout: std::time::Duration,
    on_line: impl FnMut(&str) + Send + 'static,
) -> Result<(), String> {
    use std::io::{BufRead, BufReader, Write};

    CANCEL.store(false, Ordering::Relaxed);

    let seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
    let log_path = std::env::temp_dir().join(format!(
        "sparkamp-burn-{}-{}-{}.log",
        std::process::id(),
        program,
        seq
    ));
    let log = std::fs::File::create(&log_path).map_err(|e| format!("{program}: {e}"))?;
    let log_out = log.try_clone().map_err(|e| format!("{program}: {e}"))?;
    let log_err = log.try_clone().map_err(|e| format!("{program}: {e}"))?;

    let mut child = std::process::Command::new(program)
        .args(args)
        // BOTH streams are piped: cdrskin writes its write-progress to stdout,
        // but xorriso writes its UPDATE/progress to stderr — so tee both to
        // `on_line` (and to the log) or the data-burn bar never gets a
        // fraction (2026-07-17).
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;

    // One shared progress sink for both reader threads. Each line is teed to
    // the log (so `interpret_exit`'s error tail still sees everything) AND to
    // `on_line`. Reader threads keep the poll loop free to own the
    // cancel/watchdog checks; killing/exiting the child closes both pipes,
    // ending the reader loops on their own.
    let sink = std::sync::Arc::new(std::sync::Mutex::new(on_line));
    fn tee_reader<R, F>(
        stream: R,
        mut log: std::fs::File,
        sink: std::sync::Arc<std::sync::Mutex<F>>,
    ) -> std::thread::JoinHandle<()>
    where
        R: std::io::Read + Send + 'static,
        F: FnMut(&str) + Send + 'static,
    {
        std::thread::spawn(move || {
            let mut buf = BufReader::new(stream);
            let mut line = Vec::new();
            loop {
                line.clear();
                match buf.read_until(b'\n', &mut line) {
                    Ok(0) => break, // EOF: pipe closed
                    Ok(_) => {
                        // Byte-transparent tee (lossy UTF-8 decode) so the
                        // error tail keeps every line, even non-UTF-8 garbage.
                        let text = String::from_utf8_lossy(&line);
                        let text = text.trim_end_matches(['\n', '\r']);
                        let _ = writeln!(log, "{text}");
                        if let Ok(mut f) = sink.lock() {
                            f(text);
                        }
                    }
                    Err(_) => break, // real IO error — nothing more to read
                }
            }
        })
    }
    let out_reader = tee_reader(
        child.stdout.take().expect("piped stdout"),
        log_out,
        sink.clone(),
    );
    let err_reader = tee_reader(
        child.stderr.take().expect("piped stderr"),
        log_err,
        sink.clone(),
    );

    enum Outcome {
        Exited(std::process::ExitStatus),
        Errored(String),
    }

    let started = std::time::Instant::now();
    let outcome = loop {
        if CANCEL.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            break Outcome::Errored("cancelled".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Outcome::Exited(status),
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Outcome::Errored(format!(
                        "{program} timed out after {} min — the drive stopped responding",
                        timeout.as_secs() / 60
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => break Outcome::Errored(format!("wait {program}: {e}")),
        }
    };
    // Join both readers before reading the log: their tee-writes for the last
    // lines (e.g. a failure message) must land before `interpret_exit` reads
    // the file back.
    let _ = out_reader.join();
    let _ = err_reader.join();
    let result = match outcome {
        Outcome::Exited(status) => {
            let output = std::fs::read_to_string(&log_path).unwrap_or_default();
            interpret_exit(program, status, &output)
        }
        Outcome::Errored(e) => Err(e),
    };
    let _ = std::fs::remove_file(&log_path);
    result
}

/// Decide success/failure from a finished burn/erase tool given its exit status
/// and captured output.
///
/// Exit status is the primary signal, but macOS `drutil` is unreliable: a
/// failed burn (e.g. "Burn failed: The disc drive didn't respond properly…")
/// prints the failure to its output yet the process **still exits 0**. Trusting
/// the exit code alone reports a coaster as a success. So for drutil we also
/// scan the output for its failure marker. cdrskin/xorriso exit non-zero on
/// failure like well-behaved tools, so their exit code is trusted as-is.
#[cfg_attr(target_os = "macos", allow(dead_code))] // Linux burn arm
fn interpret_exit(
    program: &str,
    status: std::process::ExitStatus,
    output: &str,
) -> Result<(), String> {
    let failed_line = output
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("Burn failed") || l.starts_with("Burn Failed"));
    // drutil's exit code lies on failure; its failure text is the truth.
    let drutil_lied = program == "drutil" && failed_line.is_some();

    if status.success() && !drutil_lied {
        return Ok(());
    }
    if let Some(line) = failed_line {
        return Err(line.to_string());
    }
    let tail: String = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" · ");
    Err(if tail.is_empty() {
        format!("{program} exited with {status}")
    } else {
        tail
    })
}

/// Parse one line of `cdrskin -v`'s audio-write progress ("Track 01:   12 of
/// 34 MB written [buf  96%]   8.0x." — the `[buf …] …x.` suffix is optional
/// and ignored) into a `0.0..=1.0` fraction. `None` for any non-progress line
/// (banners, "Thank you for using cdrskin", etc.) or a zero denominator
/// (cdrskin prints "0 of 0" for a moment before it knows the track size).
/// Returns `(track_number, within_track_fraction)`. cdrskin reports progress
/// PER TRACK, so `within` resets to ~0 at each new track — the caller must
/// fold in the track number to get a monotonic overall fraction, else the
/// bar visibly runs backward every time a track boundary is crossed
/// (2026-07-17).
pub fn parse_cdrskin_progress(line: &str) -> Option<(u32, f32)> {
    let before = line.split("MB written").next()?;
    let tokens: Vec<&str> = before.split_whitespace().collect();
    // "Track NN:  x of  y" — the track number is the token after "Track".
    let track_pos = tokens.iter().position(|&t| t == "Track")?;
    let track: u32 = tokens.get(track_pos + 1)?.trim_end_matches(':').parse().ok()?;
    let of_idx = tokens.iter().position(|&t| t == "of")?;
    if of_idx == 0 {
        return None;
    }
    let numerator: f32 = tokens.get(of_idx - 1)?.parse().ok()?;
    let denominator: f32 = tokens.get(of_idx + 1)?.parse().ok()?;
    if denominator == 0.0 {
        return None;
    }
    Some((track, numerator / denominator))
}

/// xorriso writes its progress to stderr as UPDATE pacifier lines, e.g.
/// `xorriso : UPDATE : Writing:   45.2%  fifo 100%  buf  50%   8.0xB`. The
/// write percentage is the FIRST `%`-token on a "Writing" line (the fifo/buf
/// percentages follow it). Returns a `0.0..=1.0` fraction, or `None` for any
/// other line. Falls back to cdrskin's per-track format for xorriso builds
/// that mimic it.
pub fn parse_xorriso_progress(line: &str) -> Option<f32> {
    if line.contains("Writing") {
        for tok in line.split_whitespace() {
            if let Some(num) = tok.strip_suffix('%') {
                if let Ok(p) = num.parse::<f32>() {
                    return Some((p / 100.0).clamp(0.0, 1.0));
                }
            }
        }
    }
    parse_cdrskin_progress(line).map(|(_, within)| within)
}

// ---------------------------------------------------------------------------
// Whole-burn orchestration (platform split at the command level only)
// ---------------------------------------------------------------------------

/// Resolve a drive to the DiscRecording device the framework calls burn, erase
/// and status on, and arm the cancel flag for the run about to start.
///
/// `OpticalDrive::id` is still the enumeration index it was when `drutil
/// -drive N` consumed it, and the framework's device array is the same list in
/// the same order — so the id keeps meaning exactly what it meant.
#[cfg(target_os = "macos")]
fn with_drive<T>(
    drive: &OpticalDrive,
    run: impl FnOnce(&super::discrecording::Device, &dyn Fn() -> bool) -> Result<T, String>,
) -> Result<T, String> {
    let device = super::discrecording::device_at_id(&drive.id)
        .ok_or_else(|| format!("drive {} is no longer attached", drive.id))?;
    CANCEL.store(false, Ordering::Relaxed);
    run(&device, &|| CANCEL.load(Ordering::Relaxed))
}

/// Wait for the drive to report a blank disc again after an erase.
///
/// The erase finishes before the media does. Burning immediately afterwards
/// fails with "The disc drive doesn't contain a disc" — measured, on a CD-RW
/// through the erase-first path, which is the "erase and burn" button. The
/// framework has finished; the drive has not re-read the medium.
///
/// The subprocess path never hit this: `drutil erase` and `drutil burn` were
/// two processes, and spawning the second one took long enough for the drive
/// to catch up. Calling the framework directly removed that accidental pause,
/// so the wait has to be deliberate.
#[cfg(target_os = "macos")]
fn wait_for_blank_media(drive: &OpticalDrive) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Some(device) = super::discrecording::device_at_id(&drive.id) {
            let status = device.status();
            if status.present && status.is_blank {
                return Ok(());
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(
                "the drive did not report a blank disc after erasing — \
                 if it ejected, reload the disc and burn again"
                    .to_string(),
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// Linux erases through `cdrskin blank=fast`, a separate process whose exit
/// already means the drive is ready, so there is nothing to wait for.
#[cfg(not(target_os = "macos"))]
fn wait_for_blank_media(_drive: &OpticalDrive) -> Result<(), String> {
    Ok(())
}

/// Erase the loaded rewritable disc (caller has confirmed), reporting progress
/// as it goes. macOS's erase reports a percentage while it runs; `cdrskin
/// blank=fast` reports none, so on Linux the fraction stays `None` exactly as
/// it always has.
pub fn erase(
    drive: &OpticalDrive,
    #[allow(unused_mut)] mut progress: impl FnMut(BurnProgress),
) -> Result<(), String> {
    // The two things `run_job` does before it erases, which the standalone
    // erase used to skip. Without them an erase started from the Erase button
    // reported success and left the disc exactly as it was: detection kept
    // polling the device through a twenty-second operation, and the disc was
    // still mounted, so the framework never had it to itself.
    //
    // The guard is a depth counter, so the burn path holding it already is
    // fine. `unmount_for_burn` is likewise safe to repeat.
    crate::disc::detect::begin_exclusive_read();
    let result = erase_guarded(drive, &mut progress);
    crate::disc::detect::end_exclusive_read();
    result
}

/// Erase, then check that the disc is actually blank, and escalate if it is
/// not.
///
/// The framework reporting a finished erase is not the same as the disc being
/// empty. A quick erase rewrites the lead-in in a few seconds, and a drive
/// will report the result as blank straight afterwards. Measured on a CD-RW
/// that did exactly that and then handed its old 38 MB session back when the
/// same disc was read in another drive: the data had never gone anywhere.
///
/// So the result is verified rather than trusted, and a quick erase that left
/// content behind is followed by a complete one, which blanks the whole
/// recordable area. That takes minutes instead of seconds, which is why it is
/// the fallback and not the default.
#[cfg(target_os = "macos")]
fn erase_guarded(
    drive: &OpticalDrive,
    progress: &mut impl FnMut(BurnProgress),
) -> Result<(), String> {
    use super::discrecording::EraseDepth;
    crate::disc::mount::unmount_for_burn(drive)?;

    erase_media(drive, EraseDepth::Quick, progress)?;
    if wait_for_blank_media(drive).is_ok() {
        return Ok(());
    }

    progress(BurnProgress::new(
        "Erasing (full pass, this takes longer)…",
        None,
    ));
    crate::disc::mount::unmount_for_burn(drive)?;
    erase_media(drive, EraseDepth::Complete, progress)?;
    if wait_for_blank_media(drive).is_ok() {
        return Ok(());
    }
    Err(
        "the disc still has content after a full erase. It may be faulty, or the drive may          not have enough power to write to it"
            .to_string(),
    )
}

#[cfg(not(target_os = "macos"))]
fn erase_guarded(
    drive: &OpticalDrive,
    _progress: &mut impl FnMut(BurnProgress),
) -> Result<(), String> {
    crate::disc::mount::unmount_for_burn(drive)?;
    run_tool("cdrskin", &cdrskin_erase_args(&drive.id))
}

/// The erase itself, once the drive is ours and nothing is mounted from it.
#[cfg(target_os = "macos")]
fn erase_media(
    drive: &OpticalDrive,
    depth: super::discrecording::EraseDepth,
    progress: &mut impl FnMut(BurnProgress),
) -> Result<(), String> {
    with_drive(drive, |device, cancelled| {
        super::discrecording::erase(device, depth, cancelled, &mut |label, fraction| {
            progress(BurnProgress::new(label, fraction))
        })
    })
}

/// Burn already-prepared Red Book WAVs (in list order) as an audio CD.
/// `text` is the CD-TEXT to write (`None` skips it). `verify` = post-burn
/// verification where the backend supports it (DiscRecording; cdrskin has
/// none — a hardware-pass follow-up may add a readback check).

///
/// [`run_job`]'s streamed burn progress: a `label` (the same phase text the
/// UIs already string-match — "Erasing…", "Preparing i/N · …", "Burning…
/// (this takes a while)") plus an optional `fraction` in `0.0..=1.0` for the
/// phases that can report one (`None` means "show the label only, no bar" —
/// e.g. erasing, or a burn phase before the first progress line arrives).
#[derive(Debug, Clone, PartialEq)]
pub struct BurnProgress {
    pub label: String,
    pub fraction: Option<f32>,
}

impl BurnProgress {
    fn new(label: impl Into<String>, fraction: Option<f32>) -> Self {
        Self {
            label: label.into(),
            fraction,
        }
    }
}

/// Burn already-prepared Red Book WAVs (in list order) as an audio CD,
/// reporting progress as it goes.
///
/// The two backends reach the same `progress` contract from opposite
/// directions. macOS hands the layout to DiscRecording and polls
/// `DRBurnCopyStatus`, which reports a whole-disc percentage directly. Linux
/// streams `cdrskin -v`'s "Track NN: X of Y MB written" lines and folds the
/// per-track fraction into an overall one. Callers see the same
/// [`BurnProgress`] either way.
///
/// `text` is the CD-TEXT to write (`None` skips it), structured rather than
/// serialized: macOS builds a `DRCDTextBlock` from it and Linux renders it to
/// the v07t sheet `cdrskin` reads. Deriving both from one value is what keeps
/// the two platforms from disagreeing about what a track's artist is.
///
/// `verify` is post-burn verification where the backend supports it
/// (DiscRecording; cdrskin has none — a hardware-pass follow-up may add a
/// readback check).

#[cfg(target_os = "macos")]
pub fn burn_audio(
    drive: &OpticalDrive,
    wavs: &[PathBuf],
    text: Option<&CdTextSheet>,
    verify: bool,
    mut progress: impl FnMut(BurnProgress),
) -> Result<(), String> {
    with_drive(drive, |device, cancelled| {
        super::discrecording::burn_audio(
            device,
            wavs,
            text,
            verify,
            cancelled,
            &mut |label, fraction| progress(BurnProgress::new(label, fraction)),
        )
    })
}

/// See the macOS arm above for the shared contract.
#[cfg(not(target_os = "macos"))]
pub fn burn_audio(
    drive: &OpticalDrive,
    wavs: &[PathBuf],
    text: Option<&CdTextSheet>,
    verify: bool,
    mut progress: impl FnMut(BurnProgress),
) -> Result<(), String> {
    let _ = verify; // cdrskin has no verify option (see `burn_audio`'s doc)
    let label = "Burning… (this takes a while)";
    // `cdrskin` reads CD-TEXT from a file, so the sheet is rendered next to
    // the staged WAVs it is describing: that directory is the burn's own
    // staging area and is removed with it, so the sheet cannot outlive the
    // burn or collide with a concurrent one.
    let sheet = match (text, wavs.first().and_then(|w| w.parent())) {
        (Some(text), Some(dir)) => {
            let path = dir.join("cdtext.v07t");
            std::fs::write(&path, crate::disc::cdtext::render_v07t(text))
                .map_err(|e| format!("write {}: {e}", path.display()))?;
            Some(path)
        }
        _ => None,
    };
    let args = cdrskin_audio_args(&drive.id, wavs, sheet.as_deref());
    // Fold cdrskin's per-track fraction into an overall one across all N
    // tracks: (track-1 + within) / N. Monotonic, so the bar never reverses.
    let total = wavs.len().max(1) as f32;
    let (ftx, frx) = mpsc::channel::<f32>();
    let handle = std::thread::spawn(move || {
        run_tool_streaming("cdrskin", &args, move |line: &str| {
            if let Some((track, within)) = parse_cdrskin_progress(line) {
                let overall =
                    ((track.saturating_sub(1) as f32 + within) / total).clamp(0.0, 1.0);
                let _ = ftx.send(overall);
            }
        })
    });
    loop {
        match frx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(fraction) => progress(BurnProgress::new(label, Some(fraction))),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if handle.is_finished() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err("cdrskin: worker thread panicked".to_string()),
    }
}

/// Which kind of disc [`run_job`] writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurnMode {
    /// Red Book audio CD from the queue's tracks (prepared to WAV first).
    Audio,
    /// ISO9660/Joliet data disc of the queued files plus a companion
    /// playlist; `use_m3u` mirrors the app-wide playlist-format setting
    /// (false = m3u8/UTF-8, the default).
    Data { use_m3u: bool },
}

/// The whole burn job, shared by every frontend (mirrors [`crate::disc::rip::run_job`]):
/// staging-directory lifecycle, the optional erase step, per-track WAV
/// preparation (audio) or file staging + playlist (data), the burn itself,
/// detection-cache invalidation, and cleanup. The caller has already done the
/// pre-flight (capacity check, refuse/erase decision, erase confirmation) and
/// shows the phases this reports through `phase`.
///
/// `disc_meta` supplies the CD-TEXT album/track titles for audio burns
/// (`None` skips CD-TEXT entirely); data burns ignore it. `cancel` stops
/// between steps; a cancel *during* the burn subprocess needs
/// [`request_cancel`] as well (the UIs' cancel buttons do both). Returns the
/// one-line success status, or the failure/cancel reason.
///
/// `progress` reports each phase as a [`BurnProgress`] (`label` is the same
/// text prior versions passed as a bare `&str` — TUI/mac still string-match
/// it until their own fraction-consuming tasks land):
/// - Erasing: `fraction: None` (cdrskin/drutil's quick-blank has no useful
///   percent to show).
/// - Preparing i/N: `Some((i + within_track) / N)`, the within-track term
///   from `prepare_wav_observed`'s GStreamer position feed (`position /
///   item.duration_secs`) when the item's duration is known — every burn-list
///   item's duration is populated on add, so this is the common case; falls
///   back to the coarse `i/N` step otherwise.
/// - Burning: on Linux, cdrskin's own `-v` progress lines
///   (`parse_cdrskin_progress`) stream in via `burn_audio_streaming`; on
///   macOS (drutil) and for data discs (xorriso — untouched, no matching
///   progress format) it's `fraction: None` throughout, same as before this
///   task.
///
/// Threading shape for the streamed burn fraction: `run_job` itself already
/// runs on the caller's worker thread (GTK/TUI/FFI each spawn one), and
/// `progress` is that worker's own closure — it does NOT need to be `Send`.
/// But `run_tool_streaming`'s `on_line` fires on a *different* thread (the
/// stdout reader thread it spawns internally), so it DOES need to be `Send`.
/// `burn_audio_streaming` bridges the two: `on_line` parses each line and
/// sends the fraction over an `mpsc::Sender<f32>` (a `Send` end of a channel
/// created there, no `progress` involved); `run_job`'s own thread receives on
/// the matching `Receiver` in a loop and calls `progress` directly — so
/// `progress` only ever runs on the thread it was given on.
pub fn run_job(
    drive: &OpticalDrive,
    items: &[crate::disc::burnlist::BurnItem],
    mode: BurnMode,
    erase_first: bool,
    verify: bool,
    disc_meta: Option<&crate::disc::cdtext::DiscMeta>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(BurnProgress),
) -> Result<String, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".to_string());
    }
    let staged = std::env::temp_dir().join(format!("sparkamp-burn-{}", std::process::id()));
    std::fs::create_dir_all(&staged).map_err(|e| format!("create {}: {e}", staged.display()))?;

    // The burn subprocess owns the drive for the whole run — keep every
    // detection poll (even status ioctls) off the device.
    crate::disc::detect::begin_exclusive_read();
    let result = (|| -> Result<String, String> {
        // A mounted data disc (desktop auto-mount, or Sparkamp's own browse)
        // holds the raw device — cdrskin/xorriso then fail with "Cannot
        // access '/dev/srN' as SG_IO CDROM drive". Drop the mount first.
        crate::disc::mount::unmount_for_burn(drive)?;
        if erase_first {
            progress(BurnProgress::new("Erasing…", None));
            erase(drive, &mut progress)?;
            wait_for_blank_media(drive)?;
        }
        match mode {
            BurnMode::Audio => {
                let n = items.len().max(1) as f32;
                let mut wavs = Vec::with_capacity(items.len());
                for (i, item) in items.iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) {
                        return Err("cancelled".to_string());
                    }
                    let label =
                        format!("Preparing {}/{} · {}", i + 1, items.len(), item.display);
                    progress(BurnProgress::new(label.clone(), Some(i as f32 / n)));
                    let out = staged.join(staged_wav_name(i));
                    match item.duration_secs.filter(|&d| d > 0) {
                        Some(dur) => prepare_wav_observed(&item.path, &out, |pos_secs| {
                            let track_frac = (pos_secs / dur as f64).clamp(0.0, 1.0) as f32;
                            progress(BurnProgress::new(
                                label.clone(),
                                Some((i as f32 + track_frac) / n),
                            ));
                        })?,
                        None => prepare_wav(&item.path, &out)?,
                    }
                    wavs.push(out);
                }
                // CD-TEXT whenever the caller supplied disc metadata (audio
                // mode only — data burns pass None). Derived here and handed
                // to the backend structured; each backend serializes it its
                // own way.
                let text = disc_meta.map(|meta| CdTextSheet::from_queue(meta, items));
                progress(BurnProgress::new("Burning… (this takes a while)", None));
                burn_audio(drive, &wavs, text.as_ref(), verify, &mut progress)?;
                Ok(format!("Audio CD burned ({} tracks)", items.len()))
            }
            BurnMode::Data { use_m3u } => {
                // Start indeterminate; the streamed xorriso progress upgrades
                // it to a real percentage once the write begins.
                progress(BurnProgress::new("Burning… (this takes a while)", None));
                let files: Vec<PathBuf> = items.iter().map(|i| i.path.clone()).collect();
                let staged_files = stage_data_files(&files, &staged)?;
                // Staging is usually instant hard-links; re-check before the
                // irreversible part in case a cancel landed during copies.
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".to_string());
                }
                write_data_playlist(&staged, &staged_files, items, use_m3u)?;
                burn_data(drive, &staged, verify, &mut progress)?;
                Ok(format!("Data disc burned ({} files + playlist)", items.len()))
            }
        }
    })();
    crate::disc::detect::end_exclusive_read();
    if result.is_ok() {
        // Our own write doesn't raise the kernel's media-changed flag —
        // drop the shared snapshot so the next poll re-probes the disc.
        crate::disc::detect::invalidate_shared_cache();
    }
    let _ = std::fs::remove_dir_all(&staged);
    result
}

/// Burn a staged folder as a data disc, reporting progress as it goes.
///
/// macOS builds a `DRFilesystemTrack` over the staged folder and lets the
/// engine lay out the ISO 9660 / Joliet tree, polling `DRBurnCopyStatus` for
/// the percentage. Linux streams xorriso's cdrecord-pacifier progress lines
/// (single session, so the parsed within-track fraction IS the overall
/// fraction), falling back to an indeterminate bar for any line that doesn't
/// parse so a xorriso build with a different pacifier still burns.
#[cfg(target_os = "macos")]
pub fn burn_data(
    drive: &OpticalDrive,
    staged_dir: &Path,
    verify: bool,
    mut progress: impl FnMut(BurnProgress),
) -> Result<(), String> {
    with_drive(drive, |device, cancelled| {
        super::discrecording::burn_data(
            device,
            staged_dir,
            verify,
            cancelled,
            &mut |label, fraction| progress(BurnProgress::new(label, fraction)),
        )
    })
}

/// See the macOS arm above for the shared contract.
#[cfg(not(target_os = "macos"))]
pub fn burn_data(
    drive: &OpticalDrive,
    staged_dir: &Path,
    verify: bool,
    mut progress: impl FnMut(BurnProgress),
) -> Result<(), String> {
    let _ = verify; // xorriso has no verify option (see `burn_audio`'s doc)
    let label = "Burning… (this takes a while)";
    let args = xorriso_data_args(&drive.id, staged_dir);
    let (ftx, frx) = mpsc::channel::<f32>();
    let handle = std::thread::spawn(move || {
        run_tool_streaming("xorriso", &args, move |line: &str| {
            if let Some(f) = parse_xorriso_progress(line) {
                let _ = ftx.send(f.clamp(0.0, 1.0));
            }
        })
    });
    loop {
        match frx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(fraction) => progress(BurnProgress::new(label, Some(fraction))),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if handle.is_finished() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err("xorriso: worker thread panicked".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disc::MediaInfo;

    /// A drive holding burnable media for the live hardware tests, or
    /// `None` (skip): anything the erase-decision matrix wouldn't refuse —
    /// blank write-once included.
    fn live_rw_drive(want_blank: bool) -> Option<OpticalDrive> {
        crate::disc::detect::invalidate_shared_cache();
        let drives = crate::disc::detect::list_drives_shared();
        drives.into_iter().find(|d| {
            d.media.present
                && erase_decision(d) != EraseDecision::Refuse
                && (!want_blank || d.media.is_blank)
        })
    }

    /// The two smallest MP3s from the Testing folder (short burn).
    fn small_test_mp3s(n: usize) -> Vec<PathBuf> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("Testing");
        let mut mp3s: Vec<(u64, PathBuf)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().map(|x| x == "mp3").unwrap_or(false))
            .map(|e| (e.metadata().map(|m| m.len()).unwrap_or(u64::MAX), e.path()))
            .collect();
        mp3s.sort();
        let pool: Vec<PathBuf> = mp3s.into_iter().map(|(_, p)| p).collect();
        if pool.is_empty() || pool.len() >= n {
            return pool.into_iter().take(n).collect();
        }
        // `Testing/` ships two tones and `live_hw_burn_data` wants three. The
        // difference is load-bearing rather than incidental: `live_hw_rewrite_data`
        // burns two over the data test's three and tells the old set from the
        // new one by counting what is on the disc. Topping the pool up with
        // copies keeps that discriminator without committing a third fixture,
        // and asking for three when only two exist is why that test could
        // never run on this checkout.
        let extra = std::env::temp_dir().join(format!("sparkamp-fixtures-{}", std::process::id()));
        std::fs::create_dir_all(&extra).expect("create fixture dir");
        let mut out = pool.clone();
        while out.len() < n {
            let src = &pool[out.len() % pool.len()];
            let stem = src
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "track".to_string());
            let target = extra.join(format!("{stem}_copy{}.mp3", out.len()));
            std::fs::copy(src, &target).expect("copy fixture");
            out.push(target);
        }
        out
    }

    /// LIVE: burn 2 short tracks as an audio CD — WITH CD-TEXT — onto the
    /// blank rewritable disc, then re-probe and assert the disc reads back
    /// as a 2-track audio CD, and read the CD-TEXT back off the disc
    /// (`cdrskin cdtext_to_v07t=-`) asserting the album title survived the
    /// round trip. `cargo test --lib live_hw_burn_audio -- --ignored --nocapture`.
    /// WRITES THE LOADED DISC — run only on media you own for testing.
    #[test]
    #[ignore]
    fn live_hw_burn_audio() {
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().expect("gst init");
        let Some(drive) = live_rw_drive(true) else {
            println!("no blank rewritable disc — skipping");
            return;
        };
        let srcs = small_test_mp3s(2);
        assert_eq!(srcs.len(), 2, "need two Testing MP3s");
        let staged = std::env::temp_dir().join(format!("sparkamp-hwtest-{}", std::process::id()));
        std::fs::create_dir_all(&staged).unwrap();
        let mut wavs = Vec::new();
        for (i, s) in srcs.iter().enumerate() {
            println!("preparing {}…", s.display());
            let out = staged.join(staged_wav_name(i));
            prepare_wav(s, &out).expect("prepare wav");
            wavs.push(out);
        }
        // CD-TEXT: a fixed album title we can grep for in the readback, plus
        // per-track titles built the same way the app does.
        let meta = crate::disc::cdtext::DiscMeta {
            artist: "Sparkamp Test".to_string(),
            album: "Sparkamp CDTEXT Live".to_string(),
        };
        let items: Vec<crate::disc::burnlist::BurnItem> = srcs
            .iter()
            .map(|p| crate::disc::burnlist::BurnItem {
                path: p.clone(),
                display: p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                duration_secs: Some(60),
                bytes: 1,
            })
            .collect();
        let text = CdTextSheet::from_queue(&meta, &items);
        println!("burning… (audio, {} tracks, CD-TEXT)", wavs.len());
        let started = std::time::Instant::now();
        crate::disc::detect::begin_exclusive_read();
        let r = burn_audio(&drive, &wavs, Some(&text), false, |p| {
            println!("  {} {:?}", p.label, p.fraction)
        });
        crate::disc::detect::end_exclusive_read();
        let _ = std::fs::remove_dir_all(&staged);
        r.expect("burn_audio");
        println!("burned in {:.1?}", started.elapsed());

        let d = reload_burned_disc(&drive.id);
        println!("after burn: {}", d.media_summary());
        assert!(d.media.is_audio_cd, "disc must read back as an audio CD");
        assert_eq!(
            d.toc.as_ref().map(|t| t.tracks.len()),
            Some(2),
            "TOC must carry both tracks"
        );

        // Read the CD-TEXT back off the physical disc, through the app's own
        // reader on each platform — `cdrskin` on Linux, the DiscRecording
        // path on macOS. Reading it back with the same code the app uses is
        // the point: a burn that writes CD-TEXT only the burner can read is
        // not a feature.
        println!("reading CD-TEXT back…");
        crate::disc::detect::begin_exclusive_read();
        let back = crate::disc::cdtext::read_cdtext(&drive.id);
        crate::disc::detect::end_exclusive_read();
        let back = back.expect("read CD-TEXT back off the burned disc");
        println!("--- CD-TEXT readback ---\n{back:#?}\n------------------------");
        assert_eq!(
            back.album.as_deref(),
            Some("Sparkamp CDTEXT Live"),
            "the album title must survive the round trip"
        );
        assert_eq!(
            back.artist.as_deref(),
            Some("Sparkamp Test"),
            "the disc artist must survive the round trip"
        );
        let titles: Vec<&str> = back.track_titles.iter().map(|(_, t)| t.as_str()).collect();
        let want: Vec<&str> = text.tracks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, want, "every track title must survive the round trip");
    }

    /// LIVE, and the only burn test that costs no media: everything
    /// `burn_audio` and `burn_data` do up to the write, run against whatever
    /// blank disc is loaded.
    ///
    /// It builds the real layout from real staged WAVs, asks the framework how
    /// many blocks each track will take, times the producer callback through
    /// `DRTrackSpeedTest`, and round-trips the burn's properties — so a
    /// producer that cannot keep up, a track length that comes out wrong, or a
    /// property the engine silently drops all show up here rather than on a
    /// disc that cannot be rewritten.
    ///
    /// `cargo test --lib live_hw_burn_preflight -- --ignored --nocapture`.
    /// WRITES NOTHING.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn live_hw_burn_preflight() {
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().expect("gst init");
        let Some(drive) = live_rw_drive(true) else {
            println!("no blank disc — skipping");
            return;
        };
        println!("drive {}: {}", drive.id, drive.media_summary());
        let device = crate::disc::discrecording::device_at_id(&drive.id).expect("device");

        let srcs = small_test_mp3s(2);
        assert_eq!(srcs.len(), 2, "need two Testing MP3s");
        let staged = std::env::temp_dir().join(format!("sparkamp-preflight-{}", std::process::id()));
        std::fs::create_dir_all(&staged).unwrap();
        let mut wavs = Vec::new();
        for (i, s) in srcs.iter().enumerate() {
            let out = staged.join(staged_wav_name(i));
            prepare_wav(s, &out).expect("prepare wav");
            wavs.push(out);
        }

        // Each staged WAV must be locatable and Red Book before the framework
        // ever sees it.
        for wav in &wavs {
            let head = std::fs::read(wav).expect("read staged wav");
            let (offset, len) = wav_redbook_span(&head).expect("red book span");
            println!("{}: {len} PCM bytes at +{offset}", wav.display());
            assert!(len > 0);
        }

        let audio =
            crate::disc::discrecording::preflight_audio(&wavs, false).expect("audio preflight");
        assert_eq!(audio.len(), wavs.len(), "one track per staged WAV");
        for (wav, track) in wavs.iter().zip(&audio) {
            let pcm = std::fs::metadata(wav).unwrap().len() - 44;
            println!(
                "{}: {} blocks, producer sustained {:.0} kB/s",
                wav.display(),
                track.blocks,
                track.kilobytes_per_second
            );
            // 2352 bytes per Red Book block, rounded up — the last block is
            // padded with silence.
            assert_eq!(track.blocks, pcm.div_ceil(2352), "track length in blocks");
            assert!(
                track.kilobytes_per_second > 0.0,
                "the producer must sustain some throughput, or every burn underruns"
            );
        }
        let total: u64 = audio.iter().map(|t| t.blocks).sum();
        let free = drive.media.free_bytes / 2048;
        println!("layout is {total} blocks; the disc has {free} free");

        let data =
            crate::disc::discrecording::preflight_data(&staged, false).expect("data preflight");
        println!("data track: {} blocks", data[0].blocks);
        assert!(data[0].blocks > 0, "an ISO layout is never empty");

        // The drive must say it can write CD-TEXT, because `new_burn` drops
        // the block when it says otherwise — a burn that quietly carried no
        // CD-TEXT would otherwise look like a CD-TEXT bug.
        println!("can_write_cdtext: {}", device.can_write_cdtext());
        assert!(
            device.can_write_cdtext(),
            "this drive reports no CD-TEXT support, so the burn would drop it"
        );

        for verify in [false, true] {
            let burn = crate::disc::discrecording::rehearse_burn(&device, verify)
                .expect("rehearse burn");
            println!("verify={verify}: {burn:?}");
            assert_eq!(
                burn.verify_round_tripped,
                Some(verify),
                "kDRBurnVerifyDiscKey must survive the round trip, or `verify` does nothing"
            );
            // Recorded, not relied on. The property round-trips and the
            // engine ignores it: `DRBurnWriteLayout` returned noErr after
            // 210 ms while the drive went on writing for another 39 seconds.
            // The poll loop waits on the status state machine instead, so
            // this asserts only that the dictionary carried the value.
            assert_eq!(
                burn.synchronous_round_tripped,
                Some(true),
                "kDRSynchronousBehaviorKey must survive the round trip"
            );
            assert!(
                burn.percent.unwrap_or(0.0) == 0.0,
                "nothing has been written, so nothing is complete"
            );
        }
        let _ = std::fs::remove_dir_all(&staged);
    }

    /// Wait for the disc the burn just ejected to come back, and return the
    /// drive as it then probes.
    ///
    /// A burn ends with `kDRBurnCompletionActionEject`, which is what
    /// `drutil -eject` did and is the behaviour the app ships. So by the time
    /// there is anything to read back, the disc is out of the drive — every
    /// readback assertion below runs against media a human has to reload. On
    /// a tray drive `drutil tray close` is enough; on the USB drive these
    /// tests run against, the disc comes all the way out and a person has to
    /// put it back.
    ///
    /// Only a live `#[ignore]`d test does this, and only one already run by
    /// hand with `--nocapture`, which is what makes the prompt reachable.
    fn reload_burned_disc(drive_id: &str) -> crate::disc::OpticalDrive {
        reload_disc(drive_id, "the burned disc", |d| !d.media.is_blank)
    }

    /// The same wait after an erase, which also ends by ejecting.
    fn reload_erased_disc(drive_id: &str) -> crate::disc::OpticalDrive {
        reload_disc(drive_id, "the erased disc", |d| d.media.is_blank)
    }

    /// The same wait for a data disc, which is only checkable once the OS has
    /// mounted it — `mount_path` is what the file assertions read.
    fn reload_data_disc(drive_id: &str) -> crate::disc::OpticalDrive {
        reload_disc(drive_id, "the burned disc", |d| {
            !d.media.is_blank && d.mount_path.is_some()
        })
    }

    /// Linux burns through `cdrskin`, which leaves the disc in the drive, so
    /// there is nothing to wait for and a single probe is the whole answer.
    /// Kept as the same call the macOS arm makes so these tests stay one
    /// body: a helper that exists on one platform only is how this file's
    /// two arms drifted apart before.
    #[cfg(not(target_os = "macos"))]
    fn reload_disc(
        drive_id: &str,
        _what: &str,
        _ready: impl Fn(&crate::disc::OpticalDrive) -> bool,
    ) -> crate::disc::OpticalDrive {
        crate::disc::detect::invalidate_shared_cache();
        crate::disc::detect::list_drives_shared()
            .into_iter()
            .find(|d| d.id == drive_id)
            .expect("drive")
    }

    #[cfg(target_os = "macos")]
    fn reload_disc(
        drive_id: &str,
        what: &str,
        ready: impl Fn(&crate::disc::OpticalDrive) -> bool,
    ) -> crate::disc::OpticalDrive {
        // The fast path, for a drive whose tray can be closed in software.
        let _ = std::process::Command::new("drutil").args(["tray", "close"]).output();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        let mut asked = false;
        let mut settling: Option<String> = None;
        loop {
            crate::disc::detect::invalidate_shared_cache();
            let drives = crate::disc::detect::list_drives_shared();
            if let Some(d) = drives.iter().find(|d| d.id == drive_id) {
                if d.media.present && ready(d) {
                    // Present is not identified. A disc pushed back in reads
                    // as a data disc for a second or two before the TOC is
                    // available, and returning that probe made a correct
                    // audio CD assert as `Data disc`. Wait for two agreeing
                    // reads rather than one — and compare the summary, not
                    // the answer the caller is about to assert, so the wait
                    // cannot decide the test.
                    let now = d.media_summary();
                    if settling.as_deref() == Some(now.as_str()) {
                        return d.clone();
                    }
                    settling = Some(now);
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    continue;
                }
                settling = None;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{what} never came back — reload it within three minutes"
            );
            if !asked {
                println!();
                println!(">>> the drive ejected {what}. PUT IT BACK IN to check it. <<<");
                println!();
                asked = true;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }

    /// LIVE: read back a disc `live_hw_burn_audio` already wrote, and assert
    /// it is the two-track audio CD that test asked for.
    /// `cargo test --lib live_verify_burned_audio -- --ignored --nocapture`.
    /// Reads only — safe to re-run, and it spends no media.
    ///
    /// Split out from the burn because a burn ends by ejecting the disc, so
    /// the readback happens against media that has left the drive. Folding
    /// the two together means a missed reload reads as a failed burn, which
    /// is the opposite of what happened.
    #[test]
    #[ignore]
    fn live_verify_burned_audio() {
        crate::disc::detect::invalidate_shared_cache();
        let drives = crate::disc::detect::list_drives_shared();
        let Some(d) = drives.iter().find(|d| d.media.present) else {
            println!("no disc loaded — put the burned disc in and re-run");
            return;
        };
        println!("{}: {}", d.id, d.media_summary());
        if let Some(toc) = &d.toc {
            for (i, t) in toc.tracks.iter().enumerate() {
                println!("  track {}: {t:?}", i + 1);
            }
        }
        assert!(!d.media.is_blank, "the disc must not still be blank");
        assert!(d.media.is_audio_cd, "disc must read back as an audio CD");
        assert_eq!(
            d.toc.as_ref().map(|t| t.tracks.len()),
            Some(2),
            "TOC must carry both tracks"
        );

        // CD-TEXT, through the app's own reader — `cdrskin` on Linux, the
        // DiscRecording path on macOS. Reading it back with the same code the
        // app uses is the point: CD-TEXT only the burner can read is not a
        // feature. What `live_hw_burn_audio` wrote is fixed, so this can
        // assert the exact strings.
        let back = crate::disc::cdtext::read_cdtext(&d.id)
            .expect("read CD-TEXT back off the burned disc");
        println!("--- CD-TEXT readback ---\n{back:#?}\n------------------------");
        assert_eq!(
            back.album.as_deref(),
            Some("Sparkamp CDTEXT Live"),
            "the album title must survive the round trip"
        );
        assert_eq!(
            back.artist.as_deref(),
            Some("Sparkamp Test"),
            "the disc artist must survive the round trip"
        );
        assert_eq!(
            back.track_titles.len(),
            2,
            "both tracks must carry a CD-TEXT title"
        );
    }

    /// LIVE: read back a data disc `live_hw_burn_data` already wrote, and
    /// assert the staged payload is on it.
    /// `cargo test --lib live_verify_burned_data -- --ignored --nocapture`.
    /// Reads only — safe to re-run, and it spends no media.
    ///
    /// Split from the burn for the same reason as
    /// [`live_verify_burned_audio`]: the burn ends by ejecting, so a missed
    /// reload otherwise reads as a failed burn.
    #[test]
    #[ignore]
    fn live_verify_burned_data() {
        crate::disc::detect::invalidate_shared_cache();
        let drives = crate::disc::detect::list_drives_shared();
        let Some(d) = drives.iter().find(|d| d.media.present && !d.media.is_blank) else {
            println!("no burned disc loaded — put it in and re-run");
            return;
        };
        println!("{}: {}", d.id, d.media_summary());
        assert!(!d.media.is_audio_cd, "data disc must not read as an audio CD");
        let mount = d
            .mount_path
            .as_ref()
            .expect("a burned data disc must be mounted to be checked");
        let files = crate::disc::mount::list_disc_files(mount);
        for f in &files {
            println!("  {}  ({} bytes)", f.display, f.bytes);
        }
        // What `live_hw_burn_data` writes: three MP3s and a companion
        // playlist. Names are matched loosely because ISO 9660 is free to
        // rewrite them.
        let names: Vec<String> = files.iter().map(|f| f.display.to_lowercase()).collect();
        assert!(!files.is_empty(), "a burned data disc must hold files");
        assert!(
            files.iter().all(|f| f.bytes > 0),
            "no file on the disc may be empty: {names:?}"
        );
        assert_playlist_matches_disc(mount);
        report_iso9660();
    }

    /// Assert the companion playlist lists exactly the audio files on the
    /// disc — no more, no fewer.
    ///
    /// Deliberately an equality rather than a count, so this verifies any
    /// burn without being told which one it is looking at. It is also the
    /// check that catches the failure `live_hw_rewrite_data` exists for: an
    /// erase-first burn that appended instead of replacing leaves the old
    /// tracks on the disc, and the new playlist does not name them.
    fn assert_playlist_matches_disc(mount: &Path) {
        let entries: Vec<String> = std::fs::read_dir(mount)
            .expect("read the mounted disc")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let playlist = entries
            .iter()
            .find(|n| {
                n.eq_ignore_ascii_case("playlist.m3u8") || n.eq_ignore_ascii_case("playlist.m3u")
            })
            .unwrap_or_else(|| panic!("no companion playlist on the disc: {entries:?}"));
        let body = std::fs::read_to_string(mount.join(playlist)).expect("read the playlist");
        println!("--- {playlist} ---\n{body}------------------");

        let mut listed: Vec<String> = body
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_lowercase)
            .collect();
        let mut on_disc: Vec<String> = crate::disc::mount::list_disc_files(mount)
            .iter()
            .filter_map(|f| Path::new(&f.path).file_name().map(|n| n.to_string_lossy().to_lowercase()))
            .collect();
        listed.sort();
        on_disc.sort();
        assert_eq!(
            listed, on_disc,
            "the playlist must name exactly the tracks on the disc"
        );
        println!("{} track(s), playlist agrees", on_disc.len());
    }

    /// Report whether the burned data disc carries an ISO 9660 filesystem.
    ///
    /// Reported rather than asserted, because what it reports depends on the
    /// medium and the honest answer differs between them.
    ///
    /// **DVD: present.** A DVD+RW burned through this code reports a Primary
    /// Volume Descriptor at LBA 16 with the staged folder's name, measured
    /// 2026-09-02. The burn writes ISO 9660.
    ///
    /// **CD: not found at LBA 16**, which had looked like the burn producing a
    /// Mac-only disc. The DVD result says otherwise, and points at the reason:
    /// a burned CD carries an Apple partition scheme, and the whole-disc node
    /// reported 1.3 MB where the session used 1.85 MB. LBA 16 of that node is
    /// not LBA 16 of the ISO image. The same code wrote both discs, and one of
    /// them plainly has ISO 9660 on it.
    ///
    /// Left as a report rather than an assertion until someone finds where a
    /// CD's ISO image actually starts. Asserting it would fail on CD for a
    /// reason that has nothing to do with the burn.
    #[cfg(target_os = "macos")]
    fn report_iso9660(){
        use std::io::{Read, Seek, SeekFrom};
        let Some(node) = crate::disc::discrecording::devices()
            .iter()
            .find_map(|d| d.status().device_node.clone())
        else {
            println!("ISO 9660: no media node to read");
            return;
        };
        let Ok(mut f) = std::fs::File::open(&node) else {
            println!("ISO 9660: cannot open {node}");
            return;
        };
        if f.seek(SeekFrom::Start(16 * 2048)).is_err() {
            println!("ISO 9660: cannot seek to LBA 16 of {node}");
            return;
        }
        let mut pvd = [0u8; 2048];
        if f.read_exact(&mut pvd).is_err() {
            println!("ISO 9660: cannot read LBA 16 of {node}");
            return;
        }
        if &pvd[1..6] == b"CD001" {
            let volume = String::from_utf8_lossy(&pvd[40..72])
                .trim_matches(|c: char| c.is_whitespace() || c == '\0')
                .to_string();
            println!("ISO 9660: present, volume {volume:?}");
        } else {
            println!(
                "ISO 9660: NO volume descriptor at LBA 16 of {node} — \
                 this disc may read on a Mac and nowhere else"
            );
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn report_iso9660() {}

    /// LIVE: erase the loaded rewritable disc and assert it probes blank
    /// again. `cargo test --lib live_hw_erase -- --ignored --nocapture`.
    /// ERASES THE LOADED DISC.
    #[test]
    #[ignore]
    fn live_hw_erase() {
        let Some(drive) = live_rw_drive(false) else {
            println!("no rewritable disc — skipping");
            return;
        };
        if drive.media.is_blank {
            println!("already blank — nothing to erase");
            return;
        }
        println!("erasing…");
        let started = std::time::Instant::now();
        crate::disc::detect::begin_exclusive_read();
        let r = erase(&drive, |p| println!("  {} {:?}", p.label, p.fraction));
        crate::disc::detect::end_exclusive_read();
        r.expect("erase");
        println!("erased in {:.1?}", started.elapsed());

        let d = reload_erased_disc(&drive.id);
        println!("after erase: {}", d.media_summary());
        // Both kinds blank on this path, and that is a platform difference
        // worth stating. On Linux `cdrskin blank=fast` is a compatibility
        // no-op for DVD+RW: the old content stays readable until the next
        // burn writes over it, which is what was found live on 2026-07-17.
        // DiscRecording's erase genuinely blanks it, measured 2026-09-02 on a
        // DVD+RW that went from 4.70 GB used to 4.70 GB free and reported
        // `blank` afterwards.
        //
        // So the assertion is by platform rather than by media kind. The
        // invariant that holds everywhere is the weaker one below: whatever
        // erase did, the disc can still be burned again.
        if cfg!(target_os = "macos") || d.media.kind == MediaKind::CdRw {
            assert!(
                d.media.is_blank,
                "{:?} must probe blank after a DiscRecording erase",
                d.media.kind
            );
        }
        assert_ne!(
            erase_decision(&d),
            EraseDecision::Refuse,
            "an erased rewritable disc must remain burnable"
        );
    }

    /// LIVE: rewrite a NON-blank rewritable disc — the "burn a different set
    /// over existing content" flow: `run_job` with `erase_first = true`,
    /// exactly what the UIs run after the erase confirmation. Burns 2 files
    /// (vs the 3-file set the plain data test writes, so a mount check can
    /// tell the sets apart). Skips on blank or write-once media.
    /// `cargo test --lib live_hw_rewrite_data -- --ignored --nocapture`.
    /// OVERWRITES THE LOADED DISC.
    #[test]
    #[ignore]
    fn live_hw_rewrite_data() {
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().expect("gst init");
        let drives = crate::disc::detect::list_drives_shared();
        let Some(drive) = drives.iter().find(|d| {
            d.media.present
                && !d.media.is_blank
                && erase_decision(d) == EraseDecision::EraseAfterConfirm
        }) else {
            println!("no rewritable disc with content — skipping");
            return;
        };
        let files = small_test_mp3s(2);
        assert_eq!(files.len(), 2);
        let items: Vec<crate::disc::burnlist::BurnItem> = files
            .iter()
            .map(|p| crate::disc::burnlist::BurnItem {
                path: p.clone(),
                display: p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                duration_secs: Some(60),
                bytes: std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
            })
            .collect();
        println!("rewriting… (erase-first data burn, {} files)", items.len());
        let started = std::time::Instant::now();
        let cancel = AtomicBool::new(false);
        let r = run_job(
            drive,
            &items,
            BurnMode::Data { use_m3u: false },
            true, // erase_first — the post-confirmation path
            false,
            None,
            &cancel,
            |p| println!("  {}", p.label),
        );
        println!("rewrote in {:.1?}", started.elapsed());
        let summary = r.expect("rewrite run_job");
        println!("{summary}");

        let d = reload_data_disc(&drive.id);
        println!("after rewrite: {}", d.media_summary());
        assert!(d.media.present, "disc must probe present");
        assert!(!d.media.is_audio_cd, "rewritten disc must be a data disc");
        // The point of the erase-first path is that the *new* set replaces
        // the old one. This burns 2 files where `live_hw_burn_data` burns 3,
        // so a count that still says 3 means the erase did not take and the
        // session was appended instead.
        let mount = d.mount_path.as_ref().expect("a rewritten disc must be mounted");
        let on_disc = crate::disc::mount::list_disc_files(mount);
        let names: Vec<&str> = on_disc.iter().map(|f| f.display.as_str()).collect();
        println!("mounted at {}: {names:?}", mount.display());
        let audio = on_disc
            .iter()
            .filter(|f| !f.display.to_lowercase().contains("playlist"))
            .count();
        assert_eq!(
            audio,
            items.len(),
            "the erase-first burn must replace the old set, not append to it: {names:?}"
        );
    }

    /// LIVE: burn 3 MP3s + companion playlist as a data disc onto blank
    /// rewritable media, re-probe, and assert it reads back as a data disc.
    /// `cargo test --lib live_hw_burn_data -- --ignored --nocapture`.
    /// WRITES THE LOADED DISC.
    #[test]
    #[ignore]
    fn live_hw_burn_data() {
        let Some(drive) = live_rw_drive(true) else {
            println!("no blank rewritable disc — skipping");
            return;
        };
        let files = small_test_mp3s(3);
        assert_eq!(files.len(), 3);
        let staged = std::env::temp_dir().join(format!("sparkamp-hwdata-{}", std::process::id()));
        let staged_files = stage_data_files(&files, &staged).expect("stage");
        let pl = write_data_playlist(&staged, &staged_files, &[], false).expect("playlist");
        println!("staged {} files + {}", staged_files.len(), pl.display());
        println!("burning… (data)");
        let started = std::time::Instant::now();
        crate::disc::detect::begin_exclusive_read();
        let r = burn_data(&drive, &staged, false, |p| {
            println!("  {} {:?}", p.label, p.fraction)
        });
        crate::disc::detect::end_exclusive_read();
        let _ = std::fs::remove_dir_all(&staged);
        r.expect("burn_data");
        println!("burned in {:.1?}", started.elapsed());

        let d = reload_data_disc(&drive.id);
        println!("after burn: {}", d.media_summary());
        assert!(d.media.present, "disc must probe present");
        assert!(
            !d.media.is_audio_cd,
            "data disc must not read as an audio CD"
        );
        // "Not an audio CD" is true of a blank disc and of a coaster. What
        // makes this a data burn is that the staged files are readable off
        // the disc under the names they were staged with.
        assert_disc_holds(&d, &staged_names(&staged_files, &pl));
    }

    /// The file names a data burn should be able to read back: the staged
    /// payload plus its companion playlist.
    fn staged_names(staged_files: &[PathBuf], playlist: &Path) -> Vec<String> {
        staged_files
            .iter()
            .map(PathBuf::as_path)
            .chain(std::iter::once(playlist))
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect()
    }

    /// Mount the burned disc and assert every expected name is on it.
    ///
    /// Reads the mount directly rather than through
    /// [`crate::disc::mount::list_disc_files`], which filters to audio files
    /// and so would never see the companion playlist.
    ///
    /// The filesystem is free to rewrite names — case, length, which of
    /// several name trees a reader picks — so this matches case-insensitively
    /// on the stem rather than demanding the byte-for-byte name back.
    fn assert_disc_holds(d: &crate::disc::OpticalDrive, want: &[String]) {
        let mount = d
            .mount_path
            .as_ref()
            .expect("a burned data disc must be mounted to be checked");
        let on_disc: Vec<String> = std::fs::read_dir(mount)
            .expect("read the mounted disc")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_lowercase())
            .collect();
        println!("mounted at {}: {on_disc:?}", mount.display());
        for name in want {
            let stem = Path::new(name)
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            assert!(
                on_disc.iter().any(|f| f.contains(&stem)),
                "{name} is missing from the burned disc: {on_disc:?}"
            );
        }
    }

    fn drive(present: bool, blank: bool, rw: bool, kind: MediaKind) -> OpticalDrive {
        OpticalDrive {
            supports_writing: true,
            id: "1".into(),
            label: "TEST".into(),
            media: MediaInfo {
                present,
                is_audio_cd: false,
                is_blank: blank,
                rewritable: rw,
                kind,
                free_bytes: 0,
                capacity_bytes: 0,
                typing_unknown: false,
            },
            toc: None,
            mount_path: None,
        }
    }

    #[test]
    fn erase_matrix() {
        // Blank anything → burn straight away.
        assert_eq!(
            erase_decision(&drive(true, true, false, MediaKind::CdR)),
            EraseDecision::None
        );
        // RW with content → confirm-then-erase.
        assert_eq!(
            erase_decision(&drive(true, false, true, MediaKind::CdRw)),
            EraseDecision::EraseAfterConfirm
        );
        assert_eq!(
            erase_decision(&drive(true, false, false, MediaKind::DvdRam)),
            EraseDecision::EraseAfterConfirm
        );
        // Write-once with content → refuse.
        assert_eq!(
            erase_decision(&drive(true, false, false, MediaKind::CdR)),
            EraseDecision::Refuse
        );
        // Pressed CD-ROM (Unknown kind, not blank, not RW) → refuse.
        assert_eq!(
            erase_decision(&drive(true, false, false, MediaKind::Unknown)),
            EraseDecision::Refuse
        );
        // Empty tray → refuse.
        assert_eq!(
            erase_decision(&drive(false, false, false, MediaKind::Unknown)),
            EraseDecision::Refuse
        );
    }

    #[test]
    fn audio_capacity_math() {
        let mut d = drive(true, true, false, MediaKind::CdR);
        // 80-min blank: 359 999 free 2048-byte blocks ≈ 79:59.
        d.media.free_bytes = 359_999 * 2048;
        assert_eq!(audio_capacity_secs(&d), 4799);
        // Probe reported nothing → standard 80 min.
        d.media.free_bytes = 0;
        assert_eq!(audio_capacity_secs(&d), 4800);
    }

    #[test]
    fn command_builders_exact() {
        let wavs = vec![PathBuf::from("/t/01.wav"), PathBuf::from("/t/02.wav")];
        // None: args unchanged from the pre-CD-TEXT shape.
        assert_eq!(
            cdrskin_audio_args("/dev/sr0", &wavs, None),
            [
                "dev=/dev/sr0",
                "blank=as_needed",
                "-v",
                "-dao",
                "-audio",
                "-pad",
                "/t/01.wav",
                "/t/02.wav"
            ]
        );
        // Some: input_sheet_v07t= is inserted right before -dao (cdrskin
        // requires it ahead of the write-mode option).
        assert_eq!(
            cdrskin_audio_args("/dev/sr0", &wavs, Some(Path::new("/t/cdtext.v07t"))),
            [
                "dev=/dev/sr0",
                "blank=as_needed",
                "-v",
                "input_sheet_v07t=/t/cdtext.v07t",
                "-dao",
                "-audio",
                "-pad",
                "/t/01.wav",
                "/t/02.wav"
            ]
        );
        assert_eq!(
            cdrskin_erase_args("/dev/sr0"),
            ["dev=/dev/sr0", "blank=fast"]
        );
        assert_eq!(
            xorriso_data_args("/dev/sr0", Path::new("/t/stage")),
            [
                "-outdev", "/dev/sr0", "-blank", "as_needed",
                "-joliet", "on", "-map",
                "/t/stage", "/", "-commit"
            ]
        );
        assert_eq!(staged_wav_name(0), "01.wav");
        assert_eq!(staged_wav_name(11), "12.wav");
    }

    /// The macOS burn feeds the drive these bytes itself, so the span has to
    /// be exact: an offset off by one shifts every sample and a length off by
    /// one truncates or over-reads the last block.
    #[test]
    fn redbook_wav_span_is_located_past_the_header() {
        let wav = minimal_wav();
        // `minimal_wav` is the canonical 44-byte layout: RIFF/fmt /data.
        assert_eq!(wav_redbook_span(&wav), Ok((44, 1600)));
        // The payload really does start there.
        assert_eq!(wav.len() as u64, 44 + 1600);
    }

    /// `wavenc` may put a `LIST`/`INFO` chunk ahead of `data`, so the parser
    /// walks chunks rather than trusting the 44-byte offset — including the
    /// pad byte an odd-sized chunk carries.
    #[test]
    fn redbook_wav_span_walks_chunks_before_the_data() {
        let mut wav = minimal_wav();
        let payload = wav.split_off(44);
        let mut listed = wav[..44].to_vec();
        // A 5-byte LIST chunk: odd, so it is followed by one pad byte.
        let mut interposed = b"LIST".to_vec();
        interposed.extend_from_slice(&5u32.to_le_bytes());
        interposed.extend_from_slice(b"INFO\0");
        interposed.push(0); // word-alignment pad
        listed.splice(36..36, interposed);
        listed.extend_from_slice(&payload);
        assert_eq!(wav_redbook_span(&listed), Ok((44 + 14, 1600)));
    }

    /// Anything that is not 44.1 kHz / 16-bit / stereo PCM is refused rather
    /// than written: the drive would take the bytes and the disc would play
    /// as noise.
    #[test]
    fn redbook_wav_span_refuses_non_redbook_audio() {
        let mut mono = minimal_wav();
        mono[22] = 1; // channels
        let err = wav_redbook_span(&mono).unwrap_err();
        assert!(err.contains("not Red Book"), "{err}");

        let mut resampled = minimal_wav();
        resampled[24..28].copy_from_slice(&48_000u32.to_le_bytes());
        let err = wav_redbook_span(&resampled).unwrap_err();
        assert!(err.contains("48000"), "{err}");

        assert!(wav_redbook_span(b"not a wav at all").is_err());
        // A truncated header has no data chunk to find, and must not be
        // reported as a zero-length track.
        assert!(wav_redbook_span(&minimal_wav()[..40]).is_err());
    }

    #[test]
    fn data_playlist_written_in_order() {
        let dir = std::env::temp_dir().join(format!("sparkamp-m3u-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let staged = vec![dir.join("B Song.mp3"), dir.join("A Song.mp3")];

        let p = write_data_playlist(&dir, &staged, &[], false).unwrap();
        assert_eq!(p.file_name().unwrap(), "playlist.m3u8");
        let body = std::fs::read_to_string(&p).unwrap();
        // Burn order preserved, not alphabetized; entries disc-root relative.
        // No queue items supplied, so no #EXTINF — the pre-2026-08-10 shape.
        assert_eq!(body, "#EXTM3U\nB Song.mp3\nA Song.mp3\n");

        let p = write_data_playlist(&dir, &staged, &[], true).unwrap();
        assert_eq!(p.file_name().unwrap(), "playlist.m3u");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn data_playlist_carries_extinf_from_the_queue() {
        let dir = std::env::temp_dir().join(format!("sparkamp-m3ux-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let staged = vec![dir.join("B Song.mp3"), dir.join("A Song.mp3")];
        let items = vec![
            BurnItem {
                path: PathBuf::from("/src/B Song.mp3"),
                display: "Nina Simone - Sinnerman".to_string(),
                duration_secs: Some(602),
                bytes: 1,
            },
            // Unknown length is -1, the same convention the library's own
            // playlist writer uses.
            BurnItem {
                path: PathBuf::from("/src/A Song.mp3"),
                display: "Tuba Skinny - Jubilee Stomp".to_string(),
                duration_secs: None,
                bytes: 1,
            },
        ];

        let p = write_data_playlist(&dir, &staged, &items, false).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body,
            "#EXTM3U\n\
             #EXTINF:602,Nina Simone - Sinnerman\nB Song.mp3\n\
             #EXTINF:-1,Tuba Skinny - Jubilee Stomp\nA Song.mp3\n"
        );

        // Fewer items than staged files: the extras stay bare rather than
        // borrowing the wrong track's metadata.
        let p = write_data_playlist(&dir, &staged, &items[..1], false).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body,
            "#EXTM3U\n\
             #EXTINF:602,Nina Simone - Sinnerman\nB Song.mp3\n\
             A Song.mp3\n"
        );

        // An item with no display line falls back to the file name, so the
        // entry never renders as a bare comma.
        let blank = vec![BurnItem {
            path: PathBuf::from("/src/B Song.mp3"),
            display: String::new(),
            duration_secs: Some(5),
            bytes: 1,
        }];
        let p = write_data_playlist(&dir, &staged[..1], &blank, false).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(body, "#EXTM3U\n#EXTINF:5,B Song.mp3\nB Song.mp3\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staging_dedups_names() {
        let dir = std::env::temp_dir().join(format!("sparkamp-stage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let srcdir = dir.join("src");
        std::fs::create_dir_all(srcdir.join("a")).unwrap();
        std::fs::create_dir_all(srcdir.join("b")).unwrap();
        let f1 = srcdir.join("a/song.mp3");
        let f2 = srcdir.join("b/song.mp3");
        std::fs::write(&f1, b"one").unwrap();
        std::fs::write(&f2, b"two").unwrap();

        let staged = stage_data_files(&[f1, f2], &dir.join("stage")).unwrap();
        assert_eq!(staged[0].file_name().unwrap(), "song.mp3");
        assert_eq!(staged[1].file_name().unwrap(), "song (2).mp3");
        assert_eq!(std::fs::read(&staged[1]).unwrap(), b"two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `cdrskin -v`'s audio-write progress line (confirmed against the
    /// `cdrskin` 1.5.8 binary in the dev-box: `strings` on it shows the
    /// format string `"%s%sTrack %-2.2d: %s MB written %s[buf %3d%%]  %4.1fx.%s"`
    /// with the inner `%s` built from `"%4d of %4d"` — i.e. real output looks
    /// like `Track 01:   12 of   34 MB written [buf  96%]   8.0x.`; the
    /// parser only depends on the `Track NN: X of Y MB written` prefix, so it
    /// doesn't care about the trailing `[buf …] …x.` suffix.
    #[test]
    fn cdrskin_progress_lines_parse() {
        assert_eq!(
            parse_cdrskin_progress("Track 01:   12 of   34 MB written"),
            Some((1, 12.0 / 34.0))
        );
        assert_eq!(
            parse_cdrskin_progress("Track 12:  340 of  340 MB written"),
            Some((12, 1.0))
        );
        assert_eq!(parse_cdrskin_progress("Thank you for using cdrskin"), None);
        assert_eq!(parse_cdrskin_progress("Track 01: 0 of 0 MB written"), None);
        // Real lines carry a trailing buffer/speed suffix after "MB written".
        assert_eq!(
            parse_cdrskin_progress("Track 01:   12 of   34 MB written [buf  96%]   8.0x."),
            Some((1, 12.0 / 34.0))
        );
        // Overall folds in the track number: track 3 of 10 at half its bytes
        // → (2 + 0.5)/10 = 0.25 — always ≥ the previous track's max.
        let (t, w) = parse_cdrskin_progress("Track 03:  5 of 10 MB written").unwrap();
        assert_eq!((t, w), (3, 0.5));
    }

    #[test]
    fn xorriso_progress_lines_parse() {
        // The write percentage is the FIRST %-token on a "Writing" line;
        // the fifo/buf percentages that follow must NOT be picked up.
        let w = parse_xorriso_progress(
            "xorriso : UPDATE : Writing:   45.2%  fifo 100%  buf  50%   8.0xB",
        )
        .unwrap();
        assert!((w - 0.452).abs() < 1e-4, "got {w}, want ~0.452 (not fifo/buf %)");
        // Non-progress UPDATE lines (patient/files-added) yield nothing.
        assert_eq!(
            parse_xorriso_progress("xorriso : UPDATE : Thank you for being patient."),
            None
        );
        assert_eq!(
            parse_xorriso_progress("xorriso : UPDATE :  10 files added in 1 seconds"),
            None
        );
        // Falls back to a cdrecord-style line if a build emits one.
        assert_eq!(
            parse_xorriso_progress("Track 01:  5 of 10 MB written"),
            Some(0.5)
        );
    }

    #[test]
    fn run_tool_reports_failure_and_cancel() {
        // Non-zero exit surfaces stderr tail.
        let err = run_tool("sh", &["-c".into(), "echo boom >&2; exit 3".into()]).unwrap_err();
        assert!(err.contains("boom"), "{err}");
        // Success is Ok.
        assert!(run_tool("sh", &["-c".into(), "exit 0".into()]).is_ok());
        // Cancel kills a long-running child quickly.
        let started = std::time::Instant::now();
        let handle = std::thread::spawn(|| run_tool("sh", &["-c".into(), "sleep 30".into()]));
        std::thread::sleep(std::time::Duration::from_millis(400));
        request_cancel();
        let res = handle.join().unwrap();
        assert_eq!(res.unwrap_err(), "cancelled");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn run_tool_streaming_tees_lines_in_order() {
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let collected = lines.clone();
        let res = run_tool_streaming(
            "sh",
            &["-c".into(), "printf 'a\\nb\\n'".into()],
            move |line: &str| collected.lock().unwrap().push(line.to_string()),
        );
        assert!(res.is_ok(), "{res:?}");
        assert_eq!(
            *lines.lock().unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    /// Invalid UTF-8 mid-stream must not truncate the tee: the old
    /// `read_line`-based reader treated a decode error as EOF and silently
    /// dropped everything after it — including from the log file
    /// `interpret_exit` reads back for failure diagnostics. The reader now
    /// tees raw bytes lossily, so every line (valid or not) still reaches
    /// `on_line`, and lines after the bad one are never lost.
    #[test]
    fn run_tool_streaming_lossy_on_invalid_utf8() {
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let collected = lines.clone();
        let res = run_tool_streaming(
            "sh",
            &[
                "-c".into(),
                "printf 'ok\\n\\xff\\xfe bad\\nafter\\n'".into(),
            ],
            move |line: &str| collected.lock().unwrap().push(line.to_string()),
        );
        assert!(res.is_ok(), "{res:?}");
        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[0], "ok");
        assert_eq!(lines[2], "after");
    }

    /// drutil exits 0 even when a burn fails, printing "Burn failed: …" instead.
    /// `interpret_exit` must treat that as a failure and surface the reason —
    /// otherwise a coaster is reported to the user as a successful burn.
    #[cfg(unix)]
    #[test]
    fn interpret_exit_catches_drutil_zero_exit_lie() {
        use std::os::unix::process::ExitStatusExt;
        let zero = std::process::ExitStatus::from_raw(0);

        let failed = "Found: 01.wav\nBurning Audio Disc: /tmp/x\n\
                      Burn failed: The disc drive didn't respond properly and can't recover or retry.\n";
        let e = interpret_exit("drutil", zero, failed).unwrap_err();
        assert!(e.starts_with("Burn failed"), "{e}");

        // A clean drutil run at exit 0 stays a success.
        assert!(interpret_exit("drutil", zero, "Found: 01.wav\nBurn completed.\n").is_ok());

        // Other tools trust exit 0 even if the word "failed" appears in output
        // (they exit non-zero on real failure), so no false positive there.
        assert!(interpret_exit("cdrskin", zero, "cdrskin: no operation failed\n").is_ok());

        // A non-zero exit with no "Burn failed" line falls back to the tail.
        let three = std::process::ExitStatus::from_raw(3 << 8);
        let e = interpret_exit("cdrskin", three, "line one\nfatal: laser off\n").unwrap_err();
        assert!(e.contains("laser off"), "{e}");
    }

    /// A wedged burn tool that never exits is killed by the wall-clock watchdog
    /// and surfaces a timeout error, so the burn UI can't hang forever.
    #[test]
    fn run_tool_watchdog_kills_a_wedged_child() {
        let started = std::time::Instant::now();
        let err = run_tool_streaming_with_timeout(
            "sh",
            &["-c".into(), "sleep 30".into()],
            std::time::Duration::from_millis(300),
            |_: &str| {},
        )
        .unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        // A child that finishes within the ceiling is unaffected.
        assert!(run_tool_streaming_with_timeout(
            "sh",
            &["-c".into(), "exit 0".into()],
            std::time::Duration::from_secs(5),
            |_: &str| {},
        )
        .is_ok());
    }

    /// A cancel that's already set stops `run_job` before it touches the
    /// drive, the staging area, or GStreamer — no phases, no leftovers.
    #[test]
    fn run_job_cancelled_before_start_touches_nothing() {
        let items = vec![crate::disc::burnlist::BurnItem {
            path: PathBuf::from("/nonexistent.mp3"),
            display: "X - Y".into(),
            duration_secs: Some(60),
            bytes: 1,
        }];
        let d = drive(true, true, false, MediaKind::CdR);
        let cancel = AtomicBool::new(true);
        let mut phases: Vec<String> = Vec::new();
        for mode in [BurnMode::Audio, BurnMode::Data { use_m3u: false }] {
            let r = run_job(&d, &items, mode, false, true, None, &cancel, |p| {
                phases.push(p.label)
            });
            assert_eq!(r.unwrap_err(), "cancelled");
        }
        assert!(phases.is_empty(), "{phases:?}");
    }

    /// The audio prep loop re-checks the cancel flag per track: a cancel set
    /// after the run starts stops before the *second* track's (nonexistent →
    /// would-fail) prepare, reporting the cancel rather than a prep error.
    #[test]
    fn run_job_audio_cancel_between_tracks() {
        // `run_job` enters an exclusive-read scope for its whole run, so this
        // test must not overlap the ones asserting on that counter.
        let _guard = crate::disc::detect::exclusive_read_test_guard();
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().expect("gst init");
        let tmp = std::env::temp_dir().join(format!("sparkamp-runjob-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // Track 1: a real (tiny, silent) WAV so its prepare succeeds; the
        // progress callback then flips cancel before track 2 is reached.
        let src = tmp.join("t1.wav");
        std::fs::write(&src, minimal_wav()).unwrap();
        let items = vec![
            crate::disc::burnlist::BurnItem {
                path: src.clone(),
                display: "One".into(),
                duration_secs: Some(1),
                bytes: 1,
            },
            crate::disc::burnlist::BurnItem {
                path: tmp.join("missing.mp3"),
                display: "Two".into(),
                duration_secs: Some(1),
                bytes: 1,
            },
        ];
        let d = drive(true, true, false, MediaKind::CdR);
        let cancel = AtomicBool::new(false);
        let phases = std::cell::RefCell::new(Vec::<String>::new());
        let r = run_job(&d, &items, BurnMode::Audio, false, true, None, &cancel, |p| {
            phases.borrow_mut().push(p.label);
            // Cancel as soon as track 1 starts preparing.
            cancel.store(true, Ordering::Relaxed);
        });
        assert_eq!(r.unwrap_err(), "cancelled");
        let phases = phases.into_inner();
        // Track 1's real (if near-instant) WAV prepare may fire the
        // within-track observer zero or more times before EOS — every one of
        // those calls also re-flips (already-true) cancel, so the exact
        // count isn't the invariant under test. What matters: cancel is seen
        // before track 2 starts, so every phase text is still about track 1.
        assert!(!phases.is_empty(), "{phases:?}");
        assert!(
            phases.iter().all(|p| p.starts_with("Preparing 1/2")),
            "{phases:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A tiny valid Red Book-shaped WAV (PCM S16LE stereo 44.1 kHz, ~9 ms of
    /// silence) for tests that need a decodable source without fixtures.
    fn minimal_wav() -> Vec<u8> {
        let data_len: u32 = 1600; // 400 stereo S16 frames
        let mut w = Vec::with_capacity(44 + data_len as usize);
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data_len).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
        w.extend_from_slice(&1u16.to_le_bytes()); // PCM
        w.extend_from_slice(&2u16.to_le_bytes()); // stereo
        w.extend_from_slice(&44_100u32.to_le_bytes());
        w.extend_from_slice(&176_400u32.to_le_bytes()); // byte rate
        w.extend_from_slice(&4u16.to_le_bytes()); // block align
        w.extend_from_slice(&16u16.to_le_bytes()); // bits
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data_len.to_le_bytes());
        w.resize(44 + data_len as usize, 0);
        w
    }

    /// Live Red Book WAV preparation from any real audio file — run with
    /// `cargo test --lib live_prepare_wav -- --ignored --nocapture`.
    /// Needs no blank media: this is the pre-burn transcode step.
    #[test]
    #[ignore]
    fn live_prepare_wav() {
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().expect("gst init");
        // Prefer a mounted audio CD track; else any mp3 in ~/Music.
        let src = crate::disc::detect::list_drives()
            .iter()
            .find(|d| d.media.is_audio_cd)
            .map(crate::disc::toc::track_entries)
            .and_then(|e| e.into_iter().next().map(|t| PathBuf::from(t.path)))
            // Only usable when the track really is a file. `prepare_wav` is a
            // `filesrc ! decodebin` pipeline, so the Linux `cdda://` pseudo-URI
            // is not something it can open — there the local-file fallback
            // below is the right source, and the CD path exercises macOS.
            .filter(|p| !crate::model::is_disc_uri(p))
            .or_else(|| {
                let home = std::env::var("HOME").ok()?;
                walk_first_audio(Path::new(&home).join("Music"), 0)
            });
        let Some(src) = src else {
            println!("no audio source found — skipping");
            return;
        };
        let dir = std::env::temp_dir().join(format!("sparkamp-prep-{}", std::process::id()));
        let out = dir.join(staged_wav_name(0));
        prepare_wav(&src, &out).expect("prepare");
        let bytes = std::fs::read(&out).expect("wav");
        // RIFF/WAVE header with PCM(1), 2 channels, 44100 Hz, 16 bits.
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        let fmt = bytes
            .windows(4)
            .position(|w| w == b"fmt ")
            .expect("fmt chunk");
        let at = |off: usize| -> u16 { u16::from_le_bytes([bytes[fmt + off], bytes[fmt + off + 1]]) };
        let rate = u32::from_le_bytes([
            bytes[fmt + 12],
            bytes[fmt + 13],
            bytes[fmt + 14],
            bytes[fmt + 15],
        ]);
        assert_eq!(at(8), 1, "PCM");
        assert_eq!(at(10), 2, "stereo");
        assert_eq!(rate, 44_100, "44.1 kHz");
        assert_eq!(at(22), 16, "16-bit");
        println!(
            "prepared {} → {} ({} bytes, PCM 44.1 kHz 16-bit stereo)",
            src.display(),
            out.display(),
            bytes.len()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(test)]
    fn walk_first_audio(dir: PathBuf, depth: u8) -> Option<PathBuf> {
        if depth > 3 {
            return None;
        }
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Some(hit) = walk_first_audio(p, depth + 1) {
                    return Some(hit);
                }
            } else if matches!(
                p.extension().and_then(|x| x.to_str()),
                Some("mp3") | Some("m4a") | Some("flac") | Some("aiff")
            ) {
                return Some(p);
            }
        }
        None
    }
}
