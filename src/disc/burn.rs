//! Burn audio CDs and data discs.
//!
//! Written BLIND (no blank media on the dev machine) — so the shape is
//! aggressively testable without a disc:
//!
//! - Every external command is built by a **pure function** with exact-args
//!   unit tests (`cdrskin`/`xorriso` on Linux, Apple's `drutil` on macOS —
//!   see the plan's deviation note: drutil is a CLI over DiscRecording, which
//!   keeps one subprocess path for both OSes instead of blind ObjC).
//! - Audio preparation (decode → Red Book WAV) runs the same GStreamer
//!   machinery as ripping and IS live-tested without media.
//! - The subprocess runner is cancellable (global flag polled while the
//!   child runs) and reports the stderr tail on failure.
//!
//! What must wait for blank media is enumerated in the plan's
//! "Hardware tests (Opus)" sections.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

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

/// Pipeline description turning any audio file into a Red Book WAV
/// (44.1 kHz / 16-bit / stereo) — what both cdrskin and drutil accept as an
/// audio-CD track source.
pub fn prepare_pipeline_desc(src: &Path, out: &Path) -> String {
    format!(
        "filesrc location=\"{}\" ! decodebin ! audioconvert ! audioresample \
         ! audio/x-raw,format=S16LE,rate=44100,channels=2 ! wavenc \
         ! filesink location=\"{}\"",
        src.display().to_string().replace('"', "\\\""),
        out.display().to_string().replace('"', "\\\"")
    )
}

/// Transcode one burn-list entry to a Red Book WAV. Blocking (worker
/// threads loop per track for progress/cancel, same shape as ripping).
pub fn prepare_wav(src: &Path, out: &Path) -> Result<(), String> {
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let desc = prepare_pipeline_desc(src, out);
    super::rip::run_pipeline_to_eos(&desc).inspect_err(|_| {
        let _ = std::fs::remove_file(out);
    })
}

/// The staged WAV name for burn-list position `index` (0-based): "01.wav",
/// "02.wav"… — numeric names keep both burn tools' track order identical to
/// the list order.
pub fn staged_wav_name(index: usize) -> String {
    format!("{:02}.wav", index + 1)
}

// ---------------------------------------------------------------------------
// Command builders (pure, exact-args unit tests)
// ---------------------------------------------------------------------------

/// cdrskin: burn prepared WAVs as an audio CD, DAO, padding subframe gaps.
#[cfg_attr(target_os = "macos", allow(dead_code))] // Linux burn arm
pub fn cdrskin_audio_args(device: &str, wavs: &[PathBuf]) -> Vec<String> {
    let mut args = vec![
        format!("dev={device}"),
        "blank=as_needed".to_string(),
        "-dao".to_string(),
        "-audio".to_string(),
        "-pad".to_string(),
    ];
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

/// drutil (macOS): burn a folder of Red Book WAVs as an audio CD.
/// `drive_index` is the drutil enumeration index (`OpticalDrive::id`).
/// `verify` keeps drutil's default post-burn verification; false adds
/// `-noverify` (faster, less safe).
// macOS-only (`drutil` call sites are cfg'd to macOS); exercised by the
// cross-platform tests below, so compiled everywhere but allowed-dead off macOS.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn drutil_audio_args(drive_index: &str, staged_dir: &Path, verify: bool) -> Vec<String> {
    let mut args = vec![
        "burn".to_string(),
        "-drive".to_string(),
        drive_index.to_string(),
        "-audio".to_string(),
    ];
    if !verify {
        args.push("-noverify".to_string());
    }
    args.push("-eject".to_string());
    args.push(staged_dir.display().to_string());
    args
}

/// drutil (macOS): burn a folder as a data disc (ISO9660/Joliet layout).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn drutil_data_args(drive_index: &str, staged_dir: &Path, verify: bool) -> Vec<String> {
    let mut args = vec![
        "burn".to_string(),
        "-drive".to_string(),
        drive_index.to_string(),
    ];
    if !verify {
        args.push("-noverify".to_string());
    }
    args.push("-eject".to_string());
    args.push(staged_dir.display().to_string());
    args
}

/// drutil (macOS): quick-erase a rewritable disc.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn drutil_erase_args(drive_index: &str) -> Vec<String> {
    vec![
        "erase".to_string(),
        "quick".to_string(),
        "-drive".to_string(),
        drive_index.to_string(),
    ]
}

// ---------------------------------------------------------------------------
// Data staging
// ---------------------------------------------------------------------------

/// Write a playlist file into the staged data-disc root listing the staged
/// audio files in burn order — the classic MP3-CD companion file most car
/// stereos and players read. `use_m3u` mirrors the app-wide playlist-format
/// setting (false = m3u8/UTF-8, the default).
pub fn write_data_playlist(
    staged_dir: &Path,
    staged_files: &[PathBuf],
    use_m3u: bool,
) -> Result<PathBuf, String> {
    let name = if use_m3u { "playlist.m3u" } else { "playlist.m3u8" };
    let path = staged_dir.join(name);
    let mut body = String::from("#EXTM3U\n");
    for f in staged_files {
        // Entries are relative to the disc root (players resolve them there).
        if let Some(n) = f.file_name() {
            body.push_str(&n.to_string_lossy());
            body.push('\n');
        }
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

/// Cancel flag for the (single) in-flight burn/erase subprocess. Reset when a
/// new run starts; set by `request_cancel`. One concurrent burn is a product
/// assumption (one drive op at a time).
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Ask the running burn/erase child to be killed after the next poll.
pub fn request_cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}

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
pub fn run_tool(program: &str, args: &[String]) -> Result<(), String> {
    run_tool_with_timeout(program, args, BURN_TIMEOUT)
}

/// Coarse wall-clock ceiling for one burn/erase subprocess. A full audio-CD
/// burn is minutes; 30 min without exit means the tool wedged — kill and report
/// rather than hang the burn UI forever.
const BURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

fn run_tool_with_timeout(
    program: &str,
    args: &[String],
    timeout: std::time::Duration,
) -> Result<(), String> {
    CANCEL.store(false, Ordering::Relaxed);

    let log_path = std::env::temp_dir().join(format!(
        "sparkamp-burn-{}-{}.log",
        std::process::id(),
        program
    ));
    let log = std::fs::File::create(&log_path).map_err(|e| format!("{program}: {e}"))?;
    let log_err = log.try_clone().map_err(|e| format!("{program}: {e}"))?;

    let mut child = std::process::Command::new(program)
        .args(args)
        .stdout(log)
        .stderr(log_err)
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;

    let started = std::time::Instant::now();
    let result = loop {
        if CANCEL.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            break Err("cancelled".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = std::fs::read_to_string(&log_path).unwrap_or_default();
                break interpret_exit(program, status, &output);
            }
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(format!(
                        "{program} timed out after {} min — the drive stopped responding",
                        timeout.as_secs() / 60
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => break Err(format!("wait {program}: {e}")),
        }
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

// ---------------------------------------------------------------------------
// Whole-burn orchestration (platform split at the command level only)
// ---------------------------------------------------------------------------

/// Erase the loaded rewritable disc (caller has confirmed).
pub fn erase(drive: &OpticalDrive) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return run_tool("drutil", &drutil_erase_args(&drive.id));
    #[cfg(not(target_os = "macos"))]
    return run_tool("cdrskin", &cdrskin_erase_args(&drive.id));
}

/// Burn already-prepared Red Book WAVs (in list order) as an audio CD.
/// `verify` = post-burn verification where the tool supports it (drutil;
/// cdrskin has none — a hardware-pass follow-up may add a readback check).
pub fn burn_audio(
    drive: &OpticalDrive,
    staged_dir: &Path,
    wavs: &[PathBuf],
    verify: bool,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let _ = wavs; // drutil takes the folder; order comes from the 01.wav names
        run_tool("drutil", &drutil_audio_args(&drive.id, staged_dir, verify))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (staged_dir, verify);
        run_tool("cdrskin", &cdrskin_audio_args(&drive.id, wavs))
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
/// `cancel` stops between steps; a cancel *during* the burn subprocess needs
/// [`request_cancel`] as well (the UIs' cancel buttons do both). Returns the
/// one-line success status, or the failure/cancel reason.
pub fn run_job(
    drive: &OpticalDrive,
    items: &[crate::disc::burnlist::BurnItem],
    mode: BurnMode,
    erase_first: bool,
    verify: bool,
    cancel: &AtomicBool,
    mut phase: impl FnMut(&str),
) -> Result<String, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".to_string());
    }
    let staged = std::env::temp_dir().join(format!("sparkamp-burn-{}", std::process::id()));
    std::fs::create_dir_all(&staged).map_err(|e| format!("create {}: {e}", staged.display()))?;

    // The burn subprocess owns the drive for the whole run — keep every
    // detection poll (even status ioctls) off the device.
    crate::disc::detect::set_exclusive_read(true);
    let result = (|| -> Result<String, String> {
        if erase_first {
            phase("Erasing…");
            erase(drive)?;
        }
        match mode {
            BurnMode::Audio => {
                let mut wavs = Vec::with_capacity(items.len());
                for (i, item) in items.iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) {
                        return Err("cancelled".to_string());
                    }
                    phase(&format!(
                        "Preparing {}/{} · {}",
                        i + 1,
                        items.len(),
                        item.display
                    ));
                    let out = staged.join(staged_wav_name(i));
                    prepare_wav(&item.path, &out)?;
                    wavs.push(out);
                }
                phase("Burning… (this takes a while)");
                burn_audio(drive, &staged, &wavs, verify)?;
                Ok(format!("Audio CD burned ({} tracks)", items.len()))
            }
            BurnMode::Data { use_m3u } => {
                phase("Burning… (this takes a while)");
                let files: Vec<PathBuf> = items.iter().map(|i| i.path.clone()).collect();
                let staged_files = stage_data_files(&files, &staged)?;
                // Staging is usually instant hard-links; re-check before the
                // irreversible part in case a cancel landed during copies.
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".to_string());
                }
                write_data_playlist(&staged, &staged_files, use_m3u)?;
                burn_data(drive, &staged, verify)?;
                Ok(format!("Data disc burned ({} files + playlist)", items.len()))
            }
        }
    })();
    crate::disc::detect::set_exclusive_read(false);
    if result.is_ok() {
        // Our own write doesn't raise the kernel's media-changed flag —
        // drop the shared snapshot so the next poll re-probes the disc.
        crate::disc::detect::invalidate_shared_cache();
    }
    let _ = std::fs::remove_dir_all(&staged);
    result
}

/// Burn a staged folder as a data disc.
pub fn burn_data(drive: &OpticalDrive, staged_dir: &Path, verify: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return run_tool("drutil", &drutil_data_args(&drive.id, staged_dir, verify));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = verify;
        return run_tool("xorriso", &xorriso_data_args(&drive.id, staged_dir));
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
        mp3s.into_iter().take(n).map(|(_, p)| p).collect()
    }

    /// LIVE: burn 2 short tracks as an audio CD onto the blank rewritable
    /// disc, then re-probe and assert the disc reads back as a 2-track
    /// audio CD. `cargo test --lib live_hw_burn_audio -- --ignored --nocapture`.
    /// WRITES THE LOADED DISC — run only on media you own for testing.
    #[test]
    #[ignore]
    fn live_hw_burn_audio() {
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
        println!("burning… (audio, {} tracks)", wavs.len());
        let started = std::time::Instant::now();
        crate::disc::detect::set_exclusive_read(true);
        let r = burn_audio(&drive, &staged, &wavs, false);
        crate::disc::detect::set_exclusive_read(false);
        let _ = std::fs::remove_dir_all(&staged);
        r.expect("burn_audio");
        println!("burned in {:.1?}", started.elapsed());

        crate::disc::detect::invalidate_shared_cache();
        let after = crate::disc::detect::list_drives_shared();
        let d = after.iter().find(|d| d.id == drive.id).expect("drive");
        println!("after burn: {}", d.media_summary());
        assert!(d.media.is_audio_cd, "disc must read back as an audio CD");
        assert_eq!(
            d.toc.as_ref().map(|t| t.tracks.len()),
            Some(2),
            "TOC must carry both tracks"
        );
    }

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
        crate::disc::detect::set_exclusive_read(true);
        let r = erase(&drive);
        crate::disc::detect::set_exclusive_read(false);
        r.expect("erase");
        println!("erased in {:.1?}", started.elapsed());

        crate::disc::detect::invalidate_shared_cache();
        let after = crate::disc::detect::list_drives_shared();
        let d = after.iter().find(|d| d.id == drive.id).expect("drive");
        println!("after erase: {}", d.media_summary());
        assert!(d.media.is_blank, "disc must probe blank after the erase");
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
        let pl = write_data_playlist(&staged, &staged_files, false).expect("playlist");
        println!("staged {} files + {}", staged_files.len(), pl.display());
        println!("burning… (data)");
        let started = std::time::Instant::now();
        crate::disc::detect::set_exclusive_read(true);
        let r = burn_data(&drive, &staged, false);
        crate::disc::detect::set_exclusive_read(false);
        let _ = std::fs::remove_dir_all(&staged);
        r.expect("burn_data");
        println!("burned in {:.1?}", started.elapsed());

        crate::disc::detect::invalidate_shared_cache();
        let after = crate::disc::detect::list_drives_shared();
        let d = after.iter().find(|d| d.id == drive.id).expect("drive");
        println!("after burn: {}", d.media_summary());
        assert!(d.media.present, "disc must probe present");
        assert!(
            !d.media.is_audio_cd,
            "data disc must not read as an audio CD"
        );
    }

    fn drive(present: bool, blank: bool, rw: bool, kind: MediaKind) -> OpticalDrive {
        OpticalDrive {
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
        assert_eq!(
            cdrskin_audio_args("/dev/sr0", &wavs),
            [
                "dev=/dev/sr0",
                "blank=as_needed",
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
                "-outdev", "/dev/sr0", "-blank", "as_needed", "-joliet", "on", "-map",
                "/t/stage", "/", "-commit"
            ]
        );
        // verify=true keeps drutil's default post-burn verification.
        assert_eq!(
            drutil_audio_args("1", Path::new("/t/stage"), true),
            ["burn", "-drive", "1", "-audio", "-eject", "/t/stage"]
        );
        assert_eq!(
            drutil_audio_args("1", Path::new("/t/stage"), false),
            ["burn", "-drive", "1", "-audio", "-noverify", "-eject", "/t/stage"]
        );
        assert_eq!(
            drutil_data_args("1", Path::new("/t/stage"), true),
            ["burn", "-drive", "1", "-eject", "/t/stage"]
        );
        assert_eq!(
            drutil_data_args("1", Path::new("/t/stage"), false),
            ["burn", "-drive", "1", "-noverify", "-eject", "/t/stage"]
        );
        assert_eq!(
            drutil_erase_args("1"),
            ["erase", "quick", "-drive", "1"]
        );
        assert_eq!(staged_wav_name(0), "01.wav");
        assert_eq!(staged_wav_name(11), "12.wav");
    }

    #[test]
    fn data_playlist_written_in_order() {
        let dir = std::env::temp_dir().join(format!("sparkamp-m3u-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let staged = vec![dir.join("B Song.mp3"), dir.join("A Song.mp3")];

        let p = write_data_playlist(&dir, &staged, false).unwrap();
        assert_eq!(p.file_name().unwrap(), "playlist.m3u8");
        let body = std::fs::read_to_string(&p).unwrap();
        // Burn order preserved, not alphabetized; entries disc-root relative.
        assert_eq!(body, "#EXTM3U\nB Song.mp3\nA Song.mp3\n");

        let p = write_data_playlist(&dir, &staged, true).unwrap();
        assert_eq!(p.file_name().unwrap(), "playlist.m3u");
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
        let err = run_tool_with_timeout(
            "sh",
            &["-c".into(), "sleep 30".into()],
            std::time::Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        // A child that finishes within the ceiling is unaffected.
        assert!(run_tool_with_timeout(
            "sh",
            &["-c".into(), "exit 0".into()],
            std::time::Duration::from_secs(5)
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
            let r = run_job(&d, &items, mode, false, true, &cancel, |p| {
                phases.push(p.to_string())
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
        gstreamer::init().expect("gst init");
        let tmp = std::env::temp_dir().join(format!("sparkamp-runjob-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // Track 1: a real (tiny, silent) WAV so its prepare succeeds; the
        // phase callback then flips cancel before track 2 is reached.
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
        let r = run_job(&d, &items, BurnMode::Audio, false, true, &cancel, |p| {
            phases.borrow_mut().push(p.to_string());
            // Cancel as soon as track 1 starts preparing.
            cancel.store(true, Ordering::Relaxed);
        });
        assert_eq!(r.unwrap_err(), "cancelled");
        let phases = phases.into_inner();
        assert_eq!(phases.len(), 1, "{phases:?}");
        assert!(phases[0].starts_with("Preparing 1/2"), "{phases:?}");
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
        gstreamer::init().expect("gst init");
        // Prefer a mounted audio CD track; else any mp3 in ~/Music.
        let src = crate::disc::detect::list_drives()
            .iter()
            .find(|d| d.media.is_audio_cd)
            .map(crate::disc::toc::track_entries)
            .and_then(|e| e.into_iter().next().map(|t| PathBuf::from(t.path)))
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
