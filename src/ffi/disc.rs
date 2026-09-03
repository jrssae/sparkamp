//! JSON-over-FFI optical-disc API for the macOS frontend.
//!
//! Mirrors the device-sync FFI conventions: UTF-8 JSON through `*mut c_char`
//! (freed with [`super::sparkamp_free_string`]), ctx-free so Swift can call
//! from a background queue (detection runs `drutil`/`plutil` subprocesses —
//! never block the UI thread on them).
//!
//! All disc logic lives in `crate::disc`; this file only drives it. Phase 1
//! exposes drive enumeration + per-track playlist entries; later phases add
//! gnudb, rip, and burn entry points here.
#![allow(unsafe_op_in_unsafe_fn)]

use std::os::raw::{c_char, c_int};

use crate::disc::{detect, toc, OpticalDrive};

use super::SparkampCtx;

// Reuse the JSON helpers' conventions rather than the helpers themselves —
// they're private to `devices.rs`; the pair below is identical in behaviour.

fn json_out<T: serde::Serialize>(v: &T) -> *mut c_char {
    match serde_json::to_string(v) {
        Ok(s) => std::ffi::CString::new(s)
            .map(|c| c.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Decode a JSON payload from the frontend.
///
/// A failure is reported rather than swallowed. This used to return `None` for
/// a null pointer, a non-UTF-8 payload and a shape mismatch alike, so every
/// rejected job cost a round trip with the user to find out which. The
/// frontend still only learns "rejected"; the log says why.
unsafe fn json_in<T: for<'de> serde::Deserialize<'de>>(p: *const c_char) -> Option<T> {
    if p.is_null() {
        return None;
    }
    let s = match std::ffi::CStr::from_ptr(p).to_str() {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "sparkamp: {} payload is not UTF-8: {e}",
                std::any::type_name::<T>()
            );
            return None;
        }
    };
    match serde_json::from_str(s) {
        Ok(v) => Some(v),
        Err(e) => {
            // The prefix, not the whole payload: a burn job carries every
            // queued path and the interesting part is always near the front.
            let head: String = s.chars().take(400).collect();
            eprintln!(
                "sparkamp: could not read {} from JSON: {e}\n  payload starts: {head}",
                std::any::type_name::<T>()
            );
            None
        }
    }
}

/// One drive as the FFI reports it: the `OpticalDrive` fields plus the
/// core-computed `media_summary` line ("Audio CD (8 tracks)", "Blank CD-R",
/// …) so Swift renders the same wording as the GTK/TUI frontends instead of
/// rebuilding it.
#[derive(serde::Serialize)]
struct DriveOut {
    #[serde(flatten)]
    drive: OpticalDrive,
    media_summary: String,
    /// Whether the tray is open right now.
    ///
    /// Reported per drive rather than stored on `OpticalDrive`, because it is
    /// the only consumer and it changes on its own between polls. The Eject
    /// control needs it for the empty-drive case: with no disc in, Eject means
    /// "open the tray", which is worth offering and pointless to offer twice.
    tray_open: bool,
}

/// Whether this drive's tray is open, asked of the drive itself.
///
/// False anywhere the framework will not say, including off macOS, which
/// leaves Eject enabled: offering it and having it do nothing is better than
/// hiding the only control that opens a tray.
fn tray_is_open(_id: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        crate::disc::discrecording::device_at_id(_id)
            .map(|device| device.status().tray_open)
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// Force the next drive enumeration to probe rather than answer from cache.
///
/// The cached path is right for a timer, and wrong for a person: opening the
/// Media Library is someone asking "what is in the drive now", and the answer
/// has to be current even though the kernel's device list has not changed
/// since the last poll. Without this the window could show a drive list taken
/// before the disc finished mounting and keep showing it until the next
/// devfs change.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_invalidate_cache() {
    detect::invalidate_shared_cache();
}

/// Enumerate every optical drive with its loaded-media state and (for an
/// audio CD) the TOC. Returns a JSON array of `OpticalDrive` (+ a
/// `media_summary` field per drive). Runs subprocesses — call on a
/// background queue and throttle polling.
///
/// Goes through `list_drives_shared` rather than probing fresh. This is the
/// call the macOS app polls every ten seconds, and the unshared version
/// neither respects the exclusive-read guard nor records where the discs are
/// mounted — so it probed the drive during playback, and nothing could tell
/// that a playing file was on a disc.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_list_drives(_ctx: *mut SparkampCtx) -> *mut c_char {
    let drives: Vec<DriveOut> = detect::list_drives_shared()
        .into_iter()
        .map(|drive| DriveOut {
            media_summary: drive.media_summary(),
            tray_open: tray_is_open(&drive.id),
            drive,
        })
        .collect();
    json_out(&drives)
}

/// Whether an email is acceptable for a gnudb submission: deliverable shape
/// (x@y.z — `gnudb::is_valid_email`) AND not the unset/retired app-wide
/// default (`gnudb::is_unset_email`). The frontends' prompts gate on this.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_gnudb_email_valid(
    _ctx: *mut SparkampCtx,
    email: *const c_char,
) -> bool {
    cstr(email)
        .map(|e| {
            !crate::disc::gnudb::is_unset_email(&e) && crate::disc::gnudb::is_valid_email(&e)
        })
        .unwrap_or(false)
}

/// Best-effort map from a free-text genre to a fixed CDDB category (the same
/// `gnudb::suggest_category` the GTK/TUI frontends call), for prefilling the
/// submit sheet's category picker. Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_gnudb_suggest_category(
    _ctx: *mut SparkampCtx,
    genre: *const c_char,
) -> *mut c_char {
    let genre = cstr(genre).unwrap_or_default();
    std::ffi::CString::new(crate::disc::gnudb::suggest_category(&genre))
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Playlist-ready entries (path/URI + "Track N" title + duration) for every
/// audio track on the given drive's disc. Takes the `OpticalDrive` JSON as
/// returned by `sparkamp_disc_list_drives`; returns a JSON array of
/// `DiscTrackEntry` (empty array when the drive has no audio disc).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_track_entries(
    _ctx: *mut SparkampCtx,
    drive_json: *const c_char,
) -> *mut c_char {
    let Some(drive): Option<OpticalDrive> = json_in(drive_json) else {
        return json_out(&Vec::<crate::disc::DiscTrackEntry>::new());
    };
    json_out(&toc::track_entries(&drive))
}

/// Result wrapper for the gnudb calls: exactly one of `ok`/`error` is set, so
/// Swift can branch without exceptions. `ok` carries call-specific JSON.
#[derive(serde::Serialize)]
struct GnudbResult<T: serde::Serialize> {
    ok: Option<T>,
    error: Option<String>,
}

fn gnudb_out<T: serde::Serialize>(r: Result<T, crate::disc::gnudb::GnudbError>) -> *mut c_char {
    match r {
        Ok(v) => json_out(&GnudbResult {
            ok: Some(v),
            error: None,
        }),
        Err(e) => json_out(&GnudbResult::<T> {
            ok: None,
            error: Some(e.to_string()),
        }),
    }
}

unsafe fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    std::ffi::CStr::from_ptr(p)
        .to_str()
        .ok()
        .map(|s| s.to_string())
}

/// The freedb disc ID (8 hex chars) for a `DiscToc` JSON — the stable key the
/// frontends use for per-disc tag overrides. Pure math; safe anywhere. Free
/// with `sparkamp_free_string`; null on bad input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_id(
    _ctx: *mut SparkampCtx,
    toc_json: *const c_char,
) -> *mut c_char {
    let Some(disc_toc): Option<crate::disc::DiscToc> = json_in(toc_json) else {
        return std::ptr::null_mut();
    };
    std::ffi::CString::new(crate::disc::discid::freedb_discid(&disc_toc))
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Ask gnudb which discs match a TOC. Takes the `DiscToc` JSON (from an
/// `OpticalDrive.toc`) and the configured email; returns
/// `{"ok":[DiscMatch…]}` or `{"error":"…"}`. Blocking network call (10 s
/// timeout) — background queue only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_gnudb_query(
    _ctx: *mut SparkampCtx,
    toc_json: *const c_char,
    email: *const c_char,
) -> *mut c_char {
    let (Some(disc_toc), Some(email)): (Option<crate::disc::DiscToc>, _) =
        (json_in(toc_json), cstr(email))
    else {
        return gnudb_out::<Vec<crate::disc::gnudb::DiscMatch>>(Err(
            crate::disc::gnudb::GnudbError::Protocol("bad arguments".into()),
        ));
    };
    gnudb_out(crate::disc::gnudb::query(&disc_toc, &email))
}

/// Fetch one matched entry and parse it: returns `{"ok":XmcdEntry}` or
/// `{"error":"…"}`. Blocking network call — background queue only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_gnudb_read(
    _ctx: *mut SparkampCtx,
    category: *const c_char,
    discid: *const c_char,
    email: *const c_char,
) -> *mut c_char {
    let (Some(category), Some(discid), Some(email)) = (cstr(category), cstr(discid), cstr(email))
    else {
        return gnudb_out::<crate::disc::xmcd::XmcdEntry>(Err(
            crate::disc::gnudb::GnudbError::Protocol("bad arguments".into()),
        ));
    };
    let entry = crate::disc::gnudb::read(&category, &discid, &email).and_then(|text| {
        crate::disc::xmcd::parse(&text).ok_or_else(|| {
            crate::disc::gnudb::GnudbError::Protocol("unparseable xmcd entry".into())
        })
    });
    gnudb_out(entry)
}

// ───────────────────── rip job (whole selection, core loop) ─────────────────
//
// One rip at a time: `start` spawns a worker running `rip::run_job` (the same
// loop the GTK/TUI frontends use — destination pre-flight, per-track tags,
// within-track progress, cancel between tracks); the frontend polls `poll`
// from its UI timer and shows `frac`; `cancel` stops after the current track.

/// A whole rip job (JSON in): the selected entries plus the disc's tag set.
#[derive(serde::Deserialize)]
struct RipRunIn {
    entries: Vec<crate::disc::DiscTrackEntry>,
    dest_root: String,
    /// 0 = VBR V0, 1 = VBR V2, 2 = 320 CBR (config preset ids).
    quality: u8,
    tags: crate::disc::xmcd::XmcdEntry,
    total_on_disc: u8,
}

/// What a finished job produced (mirrors `rip::RipOutcome`).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct RipJobDone {
    ripped: Vec<String>,
    failures: Vec<String>,
    cancelled: bool,
}

/// Snapshot returned by `sparkamp_disc_rip_job_poll`.
#[derive(Clone, Default, serde::Serialize)]
struct RipJobStatus {
    running: bool,
    /// 0-based index of the track being ripped.
    track_index: usize,
    track_count: usize,
    title: String,
    /// 0.0–1.0 within the current track (pipeline position / TOC duration).
    frac: f64,
    /// Set once the job finished (successfully, with failures, or cancelled).
    done: Option<RipJobDone>,
}

struct RipJobSlot {
    status: RipJobStatus,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

static RIP_JOB: std::sync::LazyLock<std::sync::Mutex<RipJobSlot>> =
    std::sync::LazyLock::new(|| {
        std::sync::Mutex::new(RipJobSlot {
            status: RipJobStatus::default(),
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    });

/// Start ripping a selection on a core worker thread. Returns 0 on start,
/// -1 for bad JSON, -2 when a rip is already running. Progress via
/// `sparkamp_disc_rip_job_poll`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_rip_job_start(
    _ctx: *mut SparkampCtx,
    job_json: *const c_char,
) -> c_int {
    let Some(job): Option<RipRunIn> = json_in(job_json) else {
        return -1;
    };
    let cancel = {
        let mut slot = RIP_JOB.lock().unwrap();
        if slot.status.running {
            return -2;
        }
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        slot.cancel = cancel.clone();
        slot.status = RipJobStatus {
            running: true,
            track_count: job.entries.len(),
            title: job.entries.first().map(|e| e.title.clone()).unwrap_or_default(),
            ..RipJobStatus::default()
        };
        cancel
    };
    std::thread::spawn(move || {
        use crate::disc::rip;
        let outcome = rip::run_job(
            &job.entries,
            std::path::Path::new(&job.dest_root),
            rip::Mp3Quality::from_config(job.quality),
            &job.tags,
            job.total_on_disc,
            &cancel,
            |i, n, title, frac| {
                let mut slot = RIP_JOB.lock().unwrap();
                slot.status.track_index = i;
                slot.status.track_count = n;
                slot.status.title = title.to_string();
                slot.status.frac = frac;
            },
        );
        let mut slot = RIP_JOB.lock().unwrap();
        slot.status.running = false;
        slot.status.done = Some(RipJobDone {
            ripped: outcome.ripped,
            failures: outcome.failures,
            cancelled: outcome.cancelled,
        });
    });
    0
}

/// Poll the running (or just-finished) rip job: JSON `RipJobStatus`. Once
/// `done` is non-null the job is over and a new one may start. Free with
/// `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_rip_job_poll(_ctx: *mut SparkampCtx) -> *mut c_char {
    json_out(&RIP_JOB.lock().unwrap().status)
}

/// Ask the running rip job to stop after the current track.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_rip_job_cancel(_ctx: *mut SparkampCtx) {
    RIP_JOB
        .lock()
        .unwrap()
        .cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The format a rip writes here ("FLAC" or "MP3"). Free with
/// `sparkamp_free_string`.
///
/// The frontends ask rather than assume. macOS writes FLAC through
/// AVFoundation and Linux writes MP3, and the rip window's heading, its
/// description, the extension it promises and whether it offers a quality
/// control all follow from this one answer, so the answer lives with the
/// encoder instead of being restated in each UI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_rip_format_name(_ctx: *mut SparkampCtx) -> *mut c_char {
    std::ffi::CString::new(crate::disc::transcode::default_rip_format().name())
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Whether the rip format takes a quality setting. False for a lossless
/// format, where a quality picker would be offering a choice that does not
/// exist.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_rip_has_quality(_ctx: *mut SparkampCtx) -> bool {
    crate::disc::transcode::default_rip_format().has_quality()
}

/// The one status line every frontend shows for a finished rip, given the
/// job's `done` JSON and how many files the library import registered.
/// Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_rip_result_message(
    _ctx: *mut SparkampCtx,
    done_json: *const c_char,
    imported: c_int,
) -> *mut c_char {
    let Some(done): Option<RipJobDone> = json_in(done_json) else {
        return std::ptr::null_mut();
    };
    let outcome = crate::disc::rip::RipOutcome {
        ripped: done.ripped,
        failures: done.failures,
        cancelled: done.cancelled,
    };
    std::ffi::CString::new(outcome.status_message(imported.max(0) as usize))
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

// ───────────────────────── shared lists (UI pickers) ────────────────────────

/// The fixed CDDB category set, as a JSON string array — the submit sheet's
/// picker items. Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_gnudb_categories(_ctx: *mut SparkampCtx) -> *mut c_char {
    json_out(&crate::disc::gnudb::CATEGORIES.to_vec())
}

/// Every ID3v1 genre string, alphabetically sorted, as a JSON string array —
/// the ID3 editor's genre typeahead items (same order the GTK dropdown
/// shows). Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_id3_genres(_ctx: *mut SparkampCtx) -> *mut c_char {
    let mut genres: Vec<&str> = crate::id3_editor::ID3V1_GENRES.to_vec();
    genres.sort_unstable_by_key(|g| g.to_ascii_lowercase());
    json_out(&genres)
}

// ─────────────────────────── burning (Phases 5–6) ───────────────────────────
//
// Blind-implemented (no blank media on the dev box) — the command builders
// and the WAV preparation are unit/live-tested; the disc-write itself is
// verified by the follow-up hardware pass (see the plan's Opus test matrix).
// All blocking; worker threads only. Cancel via sparkamp_disc_burn_cancel.

/// What must happen before burning onto the loaded media:
/// 0 = blank, burn now · 1 = rewritable with content, erase after an explicit
/// user confirmation · 2 = refuse (write-once with content / no media).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_erase_decision(
    _ctx: *mut SparkampCtx,
    drive_json: *const c_char,
) -> c_int {
    let Some(drive): Option<OpticalDrive> = json_in(drive_json) else {
        return 2;
    };
    match crate::disc::burn::erase_decision(&drive) {
        crate::disc::burn::EraseDecision::None => 0,
        crate::disc::burn::EraseDecision::EraseAfterConfirm => 1,
        crate::disc::burn::EraseDecision::Refuse => 2,
    }
}

/// Audio capacity of the loaded media in seconds (free frames / 75; falls
/// back to the 80-minute standard when free blocks are unreported).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_audio_capacity_secs(
    _ctx: *mut SparkampCtx,
    drive_json: *const c_char,
) -> c_int {
    let Some(drive): Option<OpticalDrive> = json_in(drive_json) else {
        return crate::disc::burn::AUDIO_CD_CAPACITY_SECS as c_int;
    };
    crate::disc::burn::audio_capacity_secs(&drive) as c_int
}

// One burn at a time: `start` spawns a worker running `burn::run_job` (the
// same orchestration the GTK/TUI frontends call directly — staging, optional
// erase, per-track WAV preparation or data staging + playlist, the burn,
// detection-cache invalidation, cleanup); the frontend polls `poll` from its
// UI timer and shows `phase`; `cancel` stops between steps and kills the
// burn/erase subprocess.

/// A whole burn job (JSON in). The caller has already done the pre-flight
/// (capacity check, erase_decision, the explicit erase confirmation).
#[derive(serde::Deserialize)]
struct BurnRunIn {
    drive: OpticalDrive,
    items: Vec<BurnItemIn>,
    audio: bool,
    /// Data mode: companion playlist as .m3u instead of .m3u8 (app setting).
    use_m3u: bool,
    erase_first: bool,
    verify: bool,
    /// Disc-level CD-TEXT artist/album for an audio burn (Task 11) — the mac
    /// burn panel's editable "Disc artist"/"Disc album" fields, defaulted
    /// client-side via `sparkamp_disc_default_meta`. `None`/absent skips
    /// CD-TEXT (matches pre-Task-11 behavior); data burns ignore both fields
    /// regardless of what's sent.
    ///
    /// MAC GAP: `drutil` (macOS's burn backend, see `burn::burn_audio`'s doc
    /// comment) has no CD-TEXT/v07t input — a mac burn stays untitled no
    /// matter what's sent here. These fields exist only so the UX (the
    /// editable fields themselves, kept in sync with the queue) matches the
    /// GTK/Linux burn panel; the v07t sheet they build only takes effect on
    /// the Linux `cdrskin` backend.
    #[serde(default)]
    disc_artist: Option<String>,
    #[serde(default)]
    disc_album: Option<String>,
}

/// One queued file: the path plus its "Artist - Title" display line for the
/// "Preparing i/N · …" phases.
#[derive(serde::Deserialize)]
struct BurnItemIn {
    path: String,
    display: String,
}

/// Snapshot returned by `sparkamp_disc_burn_job_poll`.
#[derive(Clone, Default, serde::Serialize)]
struct BurnJobStatus {
    running: bool,
    /// Current phase line ("Erasing…", "Preparing 2/8 · …", "Burning…").
    phase: String,
    /// `0.0..=1.0` progress within the current phase when one is known
    /// (`BurnProgress::fraction`); additive field — older mac clients that
    /// don't read it just keep showing `phase` as before.
    fraction: Option<f32>,
    /// Set once the job finished: `ok` + the success/failure message.
    done: Option<BurnJobDone>,
}

#[derive(Clone, serde::Serialize)]
struct BurnJobDone {
    ok: bool,
    message: String,
}

struct BurnJobSlot {
    status: BurnJobStatus,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

static BURN_JOB: std::sync::LazyLock<std::sync::Mutex<BurnJobSlot>> =
    std::sync::LazyLock::new(|| {
        std::sync::Mutex::new(BurnJobSlot {
            status: BurnJobStatus::default(),
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    });

#[cfg(test)]
mod burn_wire_tests {
    use super::*;

    /// The exact field names a frontend has to send for a burn job.
    ///
    /// Written out rather than round-tripped through `serde_json::to_string`,
    /// because a round trip agrees with itself whatever the names are. This
    /// is the wire contract, and it is the one that broke: the mac app's
    /// encoder turned `useM3u` into `use_m_3u`, so every data burn was
    /// refused with no reason given.
    #[test]
    fn burn_job_parses_from_the_documented_field_names() {
        let payload = r#"{
            "drive": {
                "id": "2",
                "label": "Test drive",
                "media": {
                    "present": true, "is_audio_cd": false, "is_blank": true,
                    "rewritable": true, "kind": "CdRw",
                    "free_bytes": 700000000, "capacity_bytes": 700000000
                },
                "toc": null
            },
            "items": [{"path": "/tmp/a.mp3", "display": "A"}],
            "audio": false,
            "use_m3u": true,
            "erase_first": false,
            "verify": true
        }"#;
        let job: BurnRunIn = serde_json::from_str(payload).expect("documented shape must parse");
        assert_eq!(job.drive.id, "2");
        assert_eq!(job.items.len(), 1);
        assert!(job.use_m3u);
        assert!(!job.erase_first);
        assert!(job.verify);
        // Absent disc metadata is a data burn, not a parse failure.
        assert!(job.disc_artist.is_none());
        assert!(job.disc_album.is_none());
    }
}

/// The job slot, recovering from a poisoned lock rather than panicking on it.
///
/// A worker that panics poisons the mutex, and `lock().unwrap()` would then
/// panic in every later caller, across the C FFI, for the life of the process.
/// The data behind the lock is a status record: a half-written one is worth
/// having back, and is certainly worth more than taking the app down.
fn job_slot() -> std::sync::MutexGuard<'static, BurnJobSlot> {
    BURN_JOB.lock().unwrap_or_else(|e| e.into_inner())
}

/// Clears `running` when a worker ends, however it ends.
///
/// Without this a worker that panics or returns early leaves the slot marked
/// running for the life of the process, and every later burn is refused with
/// "another burn is already running" when none is. Recovering needed a
/// restart, and nothing in the UI said so.
struct JobGuard;

impl Drop for JobGuard {
    fn drop(&mut self) {
        let mut slot = job_slot();
        if slot.status.running {
            slot.status.running = false;
            if slot.status.done.is_none() {
                slot.status.done = Some(BurnJobDone {
                    ok: false,
                    message: "the job ended unexpectedly".to_string(),
                });
            }
        }
    }
}

/// Start a burn on a core worker thread. Returns 0 on start, -1 for bad
/// JSON, -2 when a burn is already running. Progress via
/// `sparkamp_disc_burn_job_poll`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_burn_job_start(
    _ctx: *mut SparkampCtx,
    job_json: *const c_char,
) -> c_int {
    let Some(job): Option<BurnRunIn> = json_in(job_json) else {
        return -1;
    };
    let cancel = {
        let mut slot = job_slot();
        if slot.status.running {
            return -2;
        }
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        slot.cancel = cancel.clone();
        slot.status = BurnJobStatus {
            running: true,
            phase: "Starting…".to_string(),
            fraction: None,
            done: None,
        };
        cancel
    };
    std::thread::spawn(move || {
        // Releases the slot even if the work below panics.
        let _guard = JobGuard;
        use crate::disc::burn;
        let items: Vec<crate::disc::burnlist::BurnItem> = job
            .items
            .into_iter()
            .map(|i| crate::disc::burnlist::BurnItem {
                path: std::path::PathBuf::from(i.path),
                display: i.display,
                duration_secs: None, // capacity pre-flight already happened caller-side
                bytes: 0,
            })
            .collect();
        let mode = if job.audio {
            burn::BurnMode::Audio
        } else {
            burn::BurnMode::Data {
                use_m3u: job.use_m3u,
            }
        };
        // Audio mode only, and only when the caller sent both fields — data
        // burns ignore disc-level metadata entirely (see BurnRunIn's doc).
        let disc_meta = if job.audio {
            job.disc_artist
                .zip(job.disc_album)
                .map(|(artist, album)| crate::disc::cdtext::DiscMeta { artist, album })
        } else {
            None
        };
        let result = burn::run_job(
            &job.drive,
            &items,
            mode,
            job.erase_first,
            job.verify,
            disc_meta.as_ref(),
            &cancel,
            |p: burn::BurnProgress| {
                let mut slot = job_slot();
                slot.status.phase = p.label;
                slot.status.fraction = p.fraction;
            },
        );
        let mut slot = job_slot();
        slot.status.running = false;
        slot.status.done = Some(match result {
            Ok(message) => BurnJobDone { ok: true, message },
            Err(message) => BurnJobDone { ok: false, message },
        });
    });
    0
}

/// Erase the loaded rewritable disc, with no burn afterwards.
///
/// Shares the burn job slot, its poll and its cancel, because a drive does
/// one thing at a time and the UI already knows how to follow that job. The
/// caller is expected to have confirmed with the user and to have checked
/// `sparkamp_disc_erase_decision` first: this does not ask.
///
/// Returns 0 when the job started, -1 on bad input, -2 when a job is already
/// running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_erase_job_start(
    _ctx: *mut SparkampCtx,
    drive_json: *const c_char,
) -> c_int {
    let Some(drive): Option<crate::disc::OpticalDrive> = json_in(drive_json) else {
        return -1;
    };
    {
        let mut slot = job_slot();
        if slot.status.running {
            return -2;
        }
        slot.cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        slot.status = BurnJobStatus {
            running: true,
            phase: "Erasing…".to_string(),
            fraction: None,
            done: None,
        };
    }
    std::thread::spawn(move || {
        // Releases the slot even if the erase below panics.
        let _guard = JobGuard;
        // Complete, not quick. This is the Erase button, where the user
        // wants the disc empty rather than merely re-writable, and a quick
        // erase leaves the old session readable in another drive.
        let result = crate::disc::burn::erase(
            &drive,
            crate::disc::burn::EraseDepth::Complete,
            |p: crate::disc::burn::BurnProgress| {
                let mut slot = job_slot();
                slot.status.phase = p.label;
                slot.status.fraction = p.fraction;
            },
        );
        let mut slot = job_slot();
        slot.status.running = false;
        slot.status.done = Some(match result {
            Ok(()) => BurnJobDone {
                ok: true,
                message: "Disc erased.".to_string(),
            },
            Err(message) => BurnJobDone {
                ok: false,
                message,
            },
        });
    });
    0
}

/// Poll the running (or just-finished) burn job: JSON `BurnJobStatus`. Once
/// `done` is non-null the job is over and a new one may start. Free with
/// `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_burn_job_poll(_ctx: *mut SparkampCtx) -> *mut c_char {
    json_out(&job_slot().status)
}

/// Cancel the in-flight burn: stops the job between steps AND kills the
/// burn/erase subprocess if one is mid-write (one burn runs at a time).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_burn_cancel(_ctx: *mut SparkampCtx) {
    BURN_JOB
        .lock()
        .unwrap()
        .cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
    crate::disc::burn::request_cancel();
}

/// One queued item's display line, for [`sparkamp_disc_default_meta`] — the
/// only field `cdtext::default_disc_meta` reads (path/duration/bytes don't
/// factor into the artist/album defaults).
#[derive(serde::Deserialize)]
struct DefaultMetaItemIn {
    display: String,
}

/// Compute the disc-level CD-TEXT defaults the core burn job would use right
/// now (`cdtext::default_disc_meta`: the common track artist when every
/// queued item agrees, else "Various Artists"; album = "Sparkamp Disc
/// YYYY-MM-DD") for a JSON array of queue display lines:
/// `[{"display":"Artist - Title"}, …]`. Exposed so the mac burn panel's
/// artist/album fields can prefill/refresh without reimplementing the
/// consensus rule or the date format in Swift (Task 11 — see BurnRunIn's
/// `disc_artist`/`disc_album` doc for how the result flows back into a burn).
/// Returns `{"artist":"…","album":"…"}`. Pure — safe on any thread. Free
/// with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_default_meta(
    _ctx: *mut SparkampCtx,
    items_json: *const c_char,
) -> *mut c_char {
    let items: Vec<DefaultMetaItemIn> = json_in(items_json).unwrap_or_default();
    let burn_items: Vec<crate::disc::burnlist::BurnItem> = items
        .into_iter()
        .map(|i| crate::disc::burnlist::BurnItem {
            path: std::path::PathBuf::new(),
            display: i.display,
            duration_secs: None,
            bytes: 0,
        })
        .collect();
    json_out(&crate::disc::cdtext::default_disc_meta(&burn_items))
}

/// List the audio files on a mounted data disc: JSON `[DiscFile]`
/// (`{"path","display","duration_secs":u32|null,"bytes":u64}`), empty when
/// unmounted/unreadable/not a data disc. Takes an `OpticalDrive` JSON from
/// `sparkamp_disc_list_drives`.
///
/// On Linux this mounts the disc via udisks2 first (`mount::ensure_mounted`,
/// under the same exclusive-read guard a TOC probe/rip uses — the whole call
/// may spin the drive). On macOS the OS already auto-mounted the disc by the
/// time it shows up in `sparkamp_disc_list_drives`'s `mount_path` (Task 11's
/// `drutil status` + `mount`(8) resolution in `disc::detect`), so this just
/// trusts that path — no extra mount step, and no zbus/udisks2 involved.
///
/// Runs on the calling thread and may block (disc IO + a tag read per file,
/// like every `devices::browse` file walk) — background queue only, same as
/// every other `sparkamp_disc_*` call. Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_mount_list(
    _ctx: *mut SparkampCtx,
    drive_json: *const c_char,
) -> *mut c_char {
    let Some(drive): Option<OpticalDrive> = json_in(drive_json) else {
        return json_out(&Vec::<crate::disc::mount::DiscFile>::new());
    };

    #[cfg(target_os = "linux")]
    let mount_path = {
        crate::disc::detect::begin_exclusive_read();
        let result = crate::disc::mount::ensure_mounted(&drive);
        crate::disc::detect::end_exclusive_read();
        result.ok()
    };
    #[cfg(not(target_os = "linux"))]
    let mount_path = drive.mount_path.clone();

    let files = mount_path
        .map(|p| crate::disc::mount::list_disc_files(&p))
        .unwrap_or_default();
    json_out(&files)
}

/// The stored tag record for a disc: `{"user":XmcdEntry|null,
/// "official":XmcdEntry|null}` from the on-disk per-disc cache
/// (`disc_tags.toml`). File IO — background queue preferred. Free with
/// `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_tags_get(
    _ctx: *mut SparkampCtx,
    discid: *const c_char,
) -> *mut c_char {
    #[derive(serde::Serialize)]
    struct Out<'a> {
        user: Option<&'a crate::disc::xmcd::XmcdEntry>,
        official: Option<&'a crate::disc::xmcd::XmcdEntry>,
    }
    let Some(discid) = cstr(discid) else {
        return json_out(&Out {
            user: None,
            official: None,
        });
    };
    let store = crate::disc::tagstore::DiscTagStore::load();
    let rec = store.get(&discid);
    json_out(&Out {
        user: rec.map(|r| &r.user),
        official: rec.and_then(|r| r.official.as_ref()),
    })
}

/// Persist a disc's tag record (user tags + optional official baseline) to
/// the on-disk cache so it survives restarts. `official_json` may be null.
/// File IO — background queue preferred. Returns false on bad input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_tags_set(
    _ctx: *mut SparkampCtx,
    discid: *const c_char,
    user_json: *const c_char,
    official_json: *const c_char,
) -> bool {
    let (Some(discid), Some(user)): (_, Option<crate::disc::xmcd::XmcdEntry>) =
        (cstr(discid), json_in(user_json))
    else {
        return false;
    };
    let official: Option<crate::disc::xmcd::XmcdEntry> = json_in(official_json);
    let mut store = crate::disc::tagstore::DiscTagStore::load();
    store.set(&discid, user, official);
    true
}

/// Validate + build + POST a disc entry to gnudb. Takes the `DiscToc` JSON,
/// the `XmcdEntry` JSON (its `revision` field is written into the xmcd — pass
/// the matched entry's revision + 1 for an update, 0 for a new disc), the
/// chosen CDDB category, the hello email, and the test-mode flag. Returns
/// `{"ok":"<server message>"}` or `{"error":"…"}` (validation failures
/// included). Blocking network — background queue only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_gnudb_submit(
    _ctx: *mut SparkampCtx,
    toc_json: *const c_char,
    entry_json: *const c_char,
    category: *const c_char,
    email: *const c_char,
    test_mode: bool,
) -> *mut c_char {
    use crate::disc::{discid, gnudb, xmcd};
    let (Some(disc_toc), Some(entry), Some(category), Some(email)): (
        Option<crate::disc::DiscToc>,
        Option<xmcd::XmcdEntry>,
        _,
        _,
    ) = (
        json_in(toc_json),
        json_in(entry_json),
        cstr(category),
        cstr(email),
    )
    else {
        return gnudb_out::<String>(Err(gnudb::GnudbError::Protocol("bad arguments".into())));
    };
    // Submissions require the user's real address (howto: never a default).
    if gnudb::is_unset_email(&email) {
        return gnudb_out::<String>(Err(gnudb::GnudbError::Protocol(
            "Set your email before submitting (gnudb requires a personal address)".into(),
        )));
    }
    if let Err(reason) = xmcd::validate_for_submit(&entry, &disc_toc) {
        return gnudb_out::<String>(Err(gnudb::GnudbError::Protocol(reason)));
    }
    let body = xmcd::build(&entry, &disc_toc, entry.revision);
    let id = discid::freedb_discid(&disc_toc);
    gnudb_out(gnudb::submit(&body, &category, &id, &email, test_mode))
}

/// One probed path result: `secs` is `None` (JSON `null`) when the file is
/// unreadable — the caller must skip it rather than queue an unknown length.
#[derive(serde::Serialize)]
struct DurationProbeOut {
    path: String,
    secs: Option<u32>,
}

/// Probe durations for a JSON array of absolute paths. Returns a JSON array
/// `[{"path":"…","secs":123|null}]` — null ⇒ unreadable; the caller must
/// skip that file (never queue unknown lengths). Runs GStreamer discovery
/// per file: call from a background queue, not the UI thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_probe_durations(
    _ctx: *mut SparkampCtx,
    paths_json: *const c_char,
) -> *mut c_char {
    let paths: Vec<String> = json_in(paths_json).unwrap_or_default();
    let results: Vec<DurationProbeOut> = paths
        .into_iter()
        .map(|p| {
            let secs = crate::duration_probe::probe_duration(std::path::Path::new(&p))
                .map(|d| d.as_secs() as u32);
            DurationProbeOut { path: p, secs }
        })
        .collect();
    json_out(&results)
}

/// Read CD-TEXT off the drive's loaded audio disc and return it as an
/// `XmcdEntry` JSON (same shape as a gnudb match), so the frontend can
/// overlay it exactly like a database entry when gnudb has no match. Takes
/// the `OpticalDrive` JSON from `sparkamp_disc_list_drives`. Returns `null`
/// when the disc has no CD-TEXT, the read fails, or the input is bad. Holds
/// the exclusive-read guard for the duration of the read (drive contention).
/// Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_read_cdtext(
    _ctx: *mut SparkampCtx,
    drive_json: *const c_char,
) -> *mut c_char {
    let Some(drive): Option<OpticalDrive> = json_in(drive_json) else {
        return std::ptr::null_mut();
    };
    let Some(toc) = drive.toc.as_ref() else {
        return std::ptr::null_mut();
    };
    let discid = crate::disc::discid::freedb_discid(toc);
    crate::disc::detect::begin_exclusive_read();
    let cd = crate::disc::cdtext::read_cdtext(&drive.id);
    crate::disc::detect::end_exclusive_read();
    match cd {
        Ok(cd) => json_out(&cd.to_xmcd(&discid)),
        // The Swift side has no channel for a reason here, so every miss is
        // still a null. `CdTextMiss` exists for the UIs that can say why.
        Err(_) => std::ptr::null_mut(),
    }
}

/// Combine two descriptions of the same disc into what a rip window should
/// start from.
///
/// `primary_json` wins field by field and `secondary_json` fills the gaps, so
/// the caller states the precedence by which argument goes where — gnudb
/// first, CD-TEXT second, for the rip window. Either may be null, which is how
/// "that source had nothing" is said.
///
/// The rule lives here rather than in each frontend because there is one rule.
/// Two copies of it drift, and the symptom is a disc that tags differently
/// depending on which UI ripped it.
///
/// Returns null only when both arguments are absent or unparseable.
///
/// # Safety
/// Both pointers must be null or valid NUL-terminated UTF-8 JSON. The returned
/// string is owned by the caller and freed with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_merge_metadata(
    _ctx: *mut SparkampCtx,
    primary_json: *const c_char,
    secondary_json: *const c_char,
) -> *mut c_char {
    let primary: Option<crate::disc::xmcd::XmcdEntry> = json_in(primary_json);
    let secondary: Option<crate::disc::xmcd::XmcdEntry> = json_in(secondary_json);
    match (primary, secondary) {
        (Some(a), Some(b)) => json_out(&a.merged_with(&b)),
        (Some(a), None) => json_out(&a),
        (None, Some(b)) => json_out(&b),
        (None, None) => std::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CStr, CString};

    #[test]
    fn track_entries_round_trip() {
        let drive = OpticalDrive {
            supports_writing: true,
            id: "/dev/sr0".into(),
            label: "TEST".into(),
            media: crate::disc::MediaInfo {
                present: true,
                is_audio_cd: true,
                ..crate::disc::MediaInfo::none()
            },
            toc: Some(crate::disc::DiscToc {
                tracks: vec![
                    crate::disc::TocTrack {
                        number: 1,
                        start_frame: 150,
                        is_audio: true,
                    },
                    crate::disc::TocTrack {
                        number: 2,
                        start_frame: 7650,
                        is_audio: true,
                    },
                ],
                leadout_frame: 15150,
            }),
            mount_path: None,
        };
        let arg = CString::new(serde_json::to_string(&drive).unwrap()).unwrap();
        let out = unsafe { sparkamp_disc_track_entries(std::ptr::null_mut(), arg.as_ptr()) };
        assert!(!out.is_null());
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        unsafe { super::super::sparkamp_free_string(out) };
        let entries: Vec<crate::disc::DiscTrackEntry> = serde_json::from_str(&s).unwrap();
        // Entries come straight from the TOC on every platform now. macOS
        // used to need a mounted volume to resolve AIFF paths and yielded
        // nothing without one, which is exactly what the sandbox produced.
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "cdda://1?device=/dev/sr0");
        assert_eq!(entries[0].duration_secs, 100);
    }

    #[test]
    fn bad_drive_json_yields_empty_array() {
        let arg = CString::new("not json").unwrap();
        let out = unsafe { sparkamp_disc_track_entries(std::ptr::null_mut(), arg.as_ptr()) };
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        unsafe { super::super::sparkamp_free_string(out) };
        assert_eq!(s, "[]");
    }

    #[test]
    fn drive_out_flattens_and_adds_summary() {
        let out = DriveOut {
            media_summary: "Blank CD-R".into(),
            tray_open: false,
            drive: OpticalDrive {
                supports_writing: true,
                id: "/dev/sr0".into(),
                label: "TEST".into(),
                media: crate::disc::MediaInfo {
                    present: true,
                    is_blank: true,
                    kind: crate::disc::MediaKind::CdR,
                    ..crate::disc::MediaInfo::none()
                },
                toc: None,
                mount_path: None,
            },
        };
        let v: serde_json::Value = serde_json::to_value(&out).unwrap();
        // The OpticalDrive fields stay top-level (Swift's existing decoder
        // keeps working) with the summary alongside them.
        assert_eq!(v["id"], "/dev/sr0");
        assert_eq!(v["media"]["is_blank"], true);
        assert_eq!(v["media_summary"], "Blank CD-R");
        // And the payload still parses as a plain OpticalDrive.
        let round: OpticalDrive = serde_json::from_value(v).unwrap();
        assert_eq!(round.id, "/dev/sr0");
    }

    #[test]
    fn rip_result_message_round_trip() {
        let done = RipJobDone {
            ripped: vec!["a.mp3".into(), "b.mp3".into()],
            failures: vec!["3: stalled".into()],
            cancelled: false,
        };
        let arg = CString::new(serde_json::to_string(&done).unwrap()).unwrap();
        let out = unsafe {
            sparkamp_disc_rip_result_message(std::ptr::null_mut(), arg.as_ptr(), 2)
        };
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        unsafe { super::super::sparkamp_free_string(out) };
        assert_eq!(s, "Ripped 2 tracks · 1 failed — 3: stalled");
    }

    #[test]
    fn category_and_genre_lists() {
        let out = unsafe { sparkamp_gnudb_categories(std::ptr::null_mut()) };
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        unsafe { super::super::sparkamp_free_string(out) };
        let cats: Vec<String> = serde_json::from_str(&s).unwrap();
        assert_eq!(cats.len(), crate::disc::gnudb::CATEGORIES.len());
        assert!(cats.iter().any(|c| c == "rock"));

        let out = unsafe { sparkamp_id3_genres(std::ptr::null_mut()) };
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        unsafe { super::super::sparkamp_free_string(out) };
        let genres: Vec<String> = serde_json::from_str(&s).unwrap();
        assert_eq!(genres.len(), crate::id3_editor::ID3V1_GENRES.len());
        let mut sorted = genres.clone();
        sorted.sort_unstable_by_key(|g| g.to_ascii_lowercase());
        assert_eq!(genres, sorted, "genres must arrive alphabetically");
    }

    #[test]
    fn rip_job_rejects_bad_json_and_double_start() {
        let bad = CString::new("nope").unwrap();
        assert_eq!(
            unsafe { sparkamp_disc_rip_job_start(std::ptr::null_mut(), bad.as_ptr()) },
            -1
        );
        // Simulate a running job, then check the busy answer + poll shape.
        RIP_JOB.lock().unwrap().status.running = true;
        let job = RipRunIn {
            entries: vec![],
            dest_root: "/tmp".into(),
            quality: 1,
            tags: crate::disc::xmcd::XmcdEntry::default(),
            total_on_disc: 0,
        };
        let arg = CString::new(
            serde_json::to_string(&serde_json::json!({
                "entries": job.entries,
                "dest_root": job.dest_root,
                "quality": job.quality,
                "tags": job.tags,
                "total_on_disc": job.total_on_disc,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            unsafe { sparkamp_disc_rip_job_start(std::ptr::null_mut(), arg.as_ptr()) },
            -2
        );
        let out = unsafe { sparkamp_disc_rip_job_poll(std::ptr::null_mut()) };
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        unsafe { super::super::sparkamp_free_string(out) };
        assert!(s.contains("\"running\":true"));
        RIP_JOB.lock().unwrap().status.running = false;
    }

    #[test]
    fn probe_durations_empty_and_missing_path() {
        // Empty array in -> empty array out.
        let arg = CString::new("[]").unwrap();
        let out = unsafe { sparkamp_disc_probe_durations(std::ptr::null_mut(), arg.as_ptr()) };
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        unsafe { super::super::sparkamp_free_string(out) };
        assert_eq!(s, "[]");

        // Nonexistent path -> secs null, path echoed back.
        let arg =
            CString::new(serde_json::to_string(&["/no/such/file.mp3"]).unwrap()).unwrap();
        let out = unsafe { sparkamp_disc_probe_durations(std::ptr::null_mut(), arg.as_ptr()) };
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        unsafe { super::super::sparkamp_free_string(out) };
        let results: Vec<serde_json::Value> = serde_json::from_str(&s).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["path"], "/no/such/file.mp3");
        assert!(results[0]["secs"].is_null());
    }

    #[test]
    fn read_cdtext_null_on_bad_drive_json() {
        // Bad JSON in → null out, no panic.
        let bad = std::ffi::CString::new("not json").unwrap();
        let p = unsafe { sparkamp_disc_read_cdtext(std::ptr::null_mut(), bad.as_ptr()) };
        assert!(p.is_null());
    }

    /// The whole read path a frontend actually walks, against a real disc:
    /// enumerate drives, hand one back in as JSON, and get an `XmcdEntry`
    /// out. The unit tests above cover the parser with captured output; this
    /// is the one that proves detection populates the TOC, the disc-id is
    /// computed, and the JSON the frontends decode is well-formed — the
    /// pieces that only exist when a drive and a CD-TEXT disc are present.
    /// Human-run: `cargo test --lib live_read_cdtext_ffi -- --ignored --nocapture`.
    #[test]
    #[ignore = "requires a real audio disc with CD-TEXT in the drive; human-run"]
    fn live_read_cdtext_ffi() {
        let drives_json = {
            let p = unsafe { sparkamp_disc_list_drives(std::ptr::null_mut()) };
            assert!(!p.is_null(), "list_drives returned null");
            let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string();
            unsafe { super::super::sparkamp_free_string(p) };
            s
        };
        let drives: Vec<crate::disc::OpticalDrive> =
            serde_json::from_str(&drives_json).expect("drive list should parse");
        println!("drives: {drives:#?}");
        let drive = drives
            .iter()
            .find(|d| d.media.is_audio_cd)
            .expect("no audio CD found — load one and retry");
        assert!(drive.toc.is_some(), "audio CD reported without a TOC");

        let arg = CString::new(serde_json::to_string(drive).unwrap()).unwrap();
        let out = unsafe { sparkamp_disc_read_cdtext(std::ptr::null_mut(), arg.as_ptr()) };
        if out.is_null() {
            // Not every audio CD carries CD-TEXT, and a disc that does not is
            // exactly what this entry point must answer null for. Skipping
            // says "wrong disc for this test"; failing used to say "the
            // reader is broken", which sent the last run chasing a bug that
            // was not there.
            println!("the loaded disc carries no CD-TEXT — load one that does and retry");
            return;
        }
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        unsafe { super::super::sparkamp_free_string(out) };
        println!("XmcdEntry JSON: {s}");

        let entry: crate::disc::xmcd::XmcdEntry = serde_json::from_str(&s).unwrap();
        assert!(!entry.discid.is_empty(), "entry carries no disc id");
        assert!(
            !entry.album.is_empty() || !entry.artist.is_empty(),
            "entry has neither album nor artist"
        );
        assert_eq!(
            entry.track_titles.len(),
            drive.toc.as_ref().unwrap().tracks.iter().filter(|t| t.is_audio).count(),
            "one title per audio track (empty where CD-TEXT names none)"
        );
    }

    #[test]
    fn suggest_category_ffi_round_trip() {
        let arg = CString::new("Progressive Rock").unwrap();
        let out =
            unsafe { sparkamp_gnudb_suggest_category(std::ptr::null_mut(), arg.as_ptr()) };
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        unsafe { super::super::sparkamp_free_string(out) };
        assert_eq!(s, "rock");
        let out = unsafe { sparkamp_gnudb_suggest_category(std::ptr::null_mut(), std::ptr::null()) };
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        unsafe { super::super::sparkamp_free_string(out) };
        assert_eq!(s, "misc");
    }
}
