//! Optical-drive detection.
//!
//! Public entry: [`list_drives`] — one [`OpticalDrive`] per physical drive.
//!
//! Platform glue is thin and cfg-gated; every output parser is a plain
//! `&str → struct` function compiled on all platforms so the whole module is
//! unit-testable anywhere (the Linux `cd-info` parser is tested on macOS and
//! vice versa).
//!
//! - **macOS:** `drutil list` enumerates drives, `drutil status -drive N`
//!   probes the loaded media, and an audio CD's TOC comes from the mounted
//!   volume's `.TOC.plist` (converted with `plutil -convert xml1`). The
//!   plist's "Start Block" values are already CDDB-absolute (track 1 = 150).
//! - **Linux:** `/sys/block/sr*` enumerates drives (vendor+model from sysfs),
//!   `cd-info` (libcdio) reads the TOC. cd-info reports post-pregap LSNs, so
//!   **+150** is added here to make them CDDB-absolute.

use std::path::{Path, PathBuf};

use super::{DiscToc, MediaInfo, MediaKind, OpticalDrive, TocTrack};

/// Enumerate every optical drive with its loaded-media state.
///
/// Runs small subprocesses (`drutil`/`plutil` on macOS, `cd-info` on Linux) —
/// call it off the UI thread and throttle polling (a few seconds is plenty).
#[allow(dead_code)] // the in-process frontends poll via list_drives_shared; the FFI (lib only) probes fresh
pub fn list_drives() -> Vec<OpticalDrive> {
    platform::list_drives()
}

/// [`list_drives`] for repeated polling: pass the previous poll's result and
/// an unchanged loaded disc is NOT re-probed. On Linux the full probe
/// physically touches the drive, so a periodic poll must go through here —
/// the cheap kernel status ioctl answers "same disc still loaded?" without
/// any medium access.
///
/// macOS answers the same question from devfs. This used to be documented as
/// safe on the grounds that "drutil's status query doesn't spin the disc",
/// which is not true: `drutil status` reports track count and used blocks, and
/// getting those means reading the medium. Because `prev` was then discarded,
/// a poll every ten seconds re-read the disc for the life of the process — the
/// drive could never finish spinning down before the next one arrived, so an
/// idle app kept a disc turning indefinitely. Firing it into the middle of CD
/// playback is worse still, and is the hazard [`begin_exclusive_read`] exists
/// to prevent.
pub fn list_drives_cached(prev: &[OpticalDrive]) -> Vec<OpticalDrive> {
    #[cfg(target_os = "linux")]
    {
        platform::list_drives_cached(prev)
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Both short-circuits below answer with `prev`, so neither may run
        // when there is no `prev` to answer with. An empty cache means nothing
        // has successfully probed yet, and handing that back as though it were
        // an answer is how "no drives" became sticky: the Media Library showed
        // an empty drive list until something happened to invalidate it.
        let have_answer = !prev.is_empty();
        // A streaming read owns the drive: answer from the previous state
        // without touching the device.
        if have_answer && exclusive_read() {
            return prev.to_vec();
        }
        let fp = devfs_signature();
        // Nothing the kernel can see has changed, so neither has the disc.
        if have_answer {
            let unchanged = LAST_FINGERPRINT
                .lock()
                .map(|last| fp.is_some() && *last == fp)
                .unwrap_or(false);
            if unchanged {
                return prev.to_vec();
            }
        }
        // Recorded BEFORE the probe on purpose: media that changes while the
        // probe is running leaves this value stale, so the next poll sees a
        // mismatch and probes again rather than trusting a result that raced.
        if let Ok(mut last) = LAST_FINGERPRINT.lock() {
            *last = fp;
        }
        platform::list_drives()
    }
}

/// A cheap signature of what optical media the kernel currently sees, or
/// `None` on a platform with no cheap answer (which then always probes).
///
/// macOS has no equivalent of Linux's `CDROM_DRIVE_STATUS` ioctl — the only
/// way to ask `drutil` anything is to read the medium. devfs answers instead,
/// and for free: inserting a disc publishes a device node for the media (an
/// audio CD also gets one slice per track, `/dev/disk12s1`…`s15` for fifteen
/// tracks), and ejecting takes them away. Listing a directory in devfs costs
/// no device access at all.
///
/// Unrelated disk activity — mounting a disk image, plugging in a USB stick —
/// also changes this and costs one needless probe. That is the right way to be
/// wrong: an occasional extra read beats one every ten seconds forever.
///
/// It cannot see a change that leaves the node list alone, which is exactly
/// what burning or erasing does. [`invalidate_shared_cache`] covers that case,
/// and must keep doing so.
#[cfg(target_os = "macos")]
fn devfs_signature() -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let mut names: Vec<std::ffi::OsString> = std::fs::read_dir("/dev")
        .ok()?
        .flatten()
        .map(|e| e.file_name())
        .filter(|n| n.to_string_lossy().starts_with("disk"))
        .collect();
    // read_dir order is not defined, so sort before hashing or the signature
    // would change without the media changing.
    names.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    names.hash(&mut h);
    Some(h.finish())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn devfs_signature() -> Option<u64> {
    None
}

/// The [`devfs_signature`] the last probe was taken at.
#[cfg(not(target_os = "linux"))]
static LAST_FINGERPRINT: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);

/// Where each optical volume is mounted, as of the last poll.
///
/// Held apart from [`SHARED`] on purpose: this answers "is the file I am about
/// to play sitting on a disc?", and that question is asked on the path that
/// starts playback. `SHARED` is held for the length of a probe — seconds of
/// `drutil` subprocesses — so borrowing it here would stall the first note of
/// every track behind a poll that happened to be running.
static OPTICAL_MOUNTS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Whether `path` lives on a mounted optical volume.
///
/// Answered from the last poll's mount list, with no device access of its own.
/// A disc inserted since that poll reads as `false` until the next one, which
/// is the safe direction: the worst case is the guard going up one poll late,
/// not playback being blocked on a subprocess.
pub fn path_is_on_optical_media(path: &Path) -> bool {
    // A `cdda://N?device=/dev/srX` URI is optical by construction — it names a
    // device node, not a file. Neither answer below can reach it: statfs has
    // nothing to stat, and a Linux audio CD never mounts, so it appears in no
    // mount list. Without this the callers fall through to `path.exists()` and
    // conclude a perfectly present disc track is missing.
    if crate::model::is_disc_uri(path) {
        return true;
    }
    // Ask the filesystem first. `statfs` names the filesystem mounted at a
    // path, and an optical one is unmistakable — `cddafs` for an audio CD,
    // `cd9660`/`udf` for a data disc. One syscall, measured in microseconds,
    // and no dependence on anything having polled yet.
    //
    // That last part is the point. The mount list below is refreshed by the
    // drive poll, which runs on a background queue, so playback starting
    // before the first poll completes saw an empty list and read the disc
    // anyway: the first track after launch stalled 2.7 s where later ones
    // cost nothing.
    #[cfg(target_os = "macos")]
    if let Some(kind) = filesystem_type(path) {
        return matches!(kind.as_str(), "cddafs" | "cd9660" | "udf");
    }
    // Fallback: what the last poll saw. Still useful for a path that cannot be
    // stat'd, and the only answer on platforms without the syscall wired up.
    OPTICAL_MOUNTS
        .lock()
        .map(|mounts| mounts.iter().any(|m| path.starts_with(m)))
        .unwrap_or(false)
}

/// The name of the filesystem mounted at `path` (`cddafs`, `apfs`, …), or
/// `None` when it cannot be determined.
#[cfg(target_os = "macos")]
pub(crate) fn filesystem_type(path: &Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `statfs` fills the struct on success and touches nothing else;
    // the buffer is owned here and the path is a valid NUL-terminated C string.
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut buf) } != 0 {
        return None;
    }
    let raw = unsafe { std::ffi::CStr::from_ptr(buf.f_fstypename.as_ptr()) };
    Some(raw.to_string_lossy().into_owned())
}

/// Seed the mount list without a drive. Tests only — the real list is
/// refreshed by [`list_drives_shared`], which needs hardware.
#[cfg(test)]
pub(crate) fn set_optical_mounts_for_test(mounts: Vec<PathBuf>) {
    *OPTICAL_MOUNTS.lock().unwrap() = mounts;
}

#[cfg(test)]
mod devfs_signature_tests {
    use super::*;

    /// The signature must be stable when nothing changes, or every poll would
    /// look like an insertion and probe anyway — which is the bug it exists to
    /// fix. `read_dir` returns entries in no defined order, so this is really
    /// asserting that the sort is doing its job.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_signature_is_stable_across_calls() {
        let a = devfs_signature().expect("macOS always has devfs");
        let b = devfs_signature().expect("macOS always has devfs");
        assert_eq!(a, b, "an unchanged /dev must hash the same twice");
    }

    /// A second poll with the disc untouched must answer from cache and run no
    /// subprocess at all. The whole point: an idle app used to re-read the disc
    /// every ten seconds for the life of the process, so the drive never spun
    /// down.
    ///
    /// Timing is the assertion because "did it shell out to drutil" has no
    /// direct handle — a real probe runs `drutil list` plus a `drutil status`
    /// per drive, which cannot happen in single-digit milliseconds.
    ///
    /// `cargo test --lib live_second_poll_is_cached -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_second_poll_is_cached() {
        let _lock = exclusive_read_test_guard();
        invalidate_shared_cache();

        let t = std::time::Instant::now();
        let first = list_drives_shared();
        let cold = t.elapsed();

        let t = std::time::Instant::now();
        let second = list_drives_shared();
        let warm = t.elapsed();

        eprintln!("{} drive(s)", first.len());
        eprintln!("  first poll (probes):  {cold:?}");
        eprintln!("  second poll (cached): {warm:?}");
        assert_eq!(first, second, "a cached answer must match what it caches");
        assert!(
            warm < cold / 10,
            "the second poll should be far cheaper than the first \
             (cold {cold:?}, warm {warm:?}) — if they are alike, the cache is \
             not being consulted and the disc is still being read"
        );
    }
}

#[cfg(test)]
mod optical_mount_tests {
    use super::*;

    /// Path matching is on a path boundary, so a volume named as a prefix of
    /// another does not claim its files.
    #[test]
    fn only_paths_under_a_mount_count_as_optical() {
        let _lock = exclusive_read_test_guard();
        set_optical_mounts_for_test(vec![PathBuf::from("/Volumes/Audio CD 1")]);

        assert!(path_is_on_optical_media(Path::new(
            "/Volumes/Audio CD 1/1 Track 1.aiff"
        )));
        assert!(path_is_on_optical_media(Path::new("/Volumes/Audio CD 1")));
        assert!(!path_is_on_optical_media(Path::new("/Users/me/Music/a.mp3")));
        // A Linux disc track: no mount to match and nothing to stat, but
        // unambiguously on optical media.
        assert!(path_is_on_optical_media(Path::new(
            "cdda://1?device=/dev/sr0"
        )));
        assert!(
            !path_is_on_optical_media(Path::new("/Volumes/Audio CD 10/x.aiff")),
            "`starts_with` on a Path compares components, not bytes"
        );

        // An eject empties the list, which is what stops the guard being
        // raised for a disc that is no longer there.
        set_optical_mounts_for_test(Vec::new());
        assert!(!path_is_on_optical_media(Path::new(
            "/Volumes/Audio CD 1/1 Track 1.aiff"
        )));
    }
}

/// [`list_drives_cached`] over one process-wide cache, serialized: every
/// poller (the auto-open watcher, the Media Library poll) shares the same
/// previous-state snapshot, so a newly inserted disc is probed exactly once
/// no matter how many pollers fire — concurrent callers block briefly and
/// reuse the fresh result instead of contending for the drive.
static SHARED: std::sync::Mutex<Vec<OpticalDrive>> = std::sync::Mutex::new(Vec::new());

pub fn list_drives_shared() -> Vec<OpticalDrive> {
    let mut cache = SHARED.lock().unwrap();
    let drives = list_drives_cached(&cache);
    *cache = drives.clone();
    // Refresh the mount list every poll, so `path_is_on_optical_media` can
    // answer without touching a drive. Ejecting clears the entry, which is
    // what stops the guard being raised for a path that is no longer there.
    if let Ok(mut mounts) = OPTICAL_MOUNTS.lock() {
        *mounts = drives.iter().filter_map(|d| d.mount_path.clone()).collect();
    }
    drives
}

/// Drop the shared snapshot so the next poll re-probes. Needed after WE
/// change the medium (burn/erase finished): the kernel's media-changed flag
/// doesn't fire for our own writes, so the cache would keep reporting the
/// pre-burn state.
pub fn invalidate_shared_cache() {
    SHARED.lock().unwrap().clear();
    // The fingerprint too, and this is the case it cannot see for itself:
    // burning or erasing rewrites the medium while every device node stays
    // exactly where it was, so devfs looks untouched and the next poll would
    // hand back the pre-burn state forever.
    #[cfg(not(target_os = "linux"))]
    if let Ok(mut last) = LAST_FINGERPRINT.lock() {
        *last = None;
    }
}

/// While a streaming read owns the drive (cdda playback, a rip, a burn, or a
/// data-disc mount+browse), even the "harmless" status ioctls interleave SCSI
/// commands with the reads and make flaky drives fault mid-stream (verified
/// live). Each such scope flips this ON **before** touching the device and
/// OFF when its session ends; while the count is above zero, every Linux
/// detection entry point answers from its previous result without opening
/// the device at all. Frontend-level guards remain as a second layer, but
/// this closes the race where a poll is already in flight when a scope
/// starts.
///
/// A refcount, not a bool: two scopes can legitimately overlap on two
/// different drives (e.g. a burn running on drive B while a browse/rip
/// finishes on drive A) — with a plain bool the one finishing first would
/// clear the flag out from under the one still running, letting a poll
/// re-probe (and potentially fault) a drive mid-write.
static EXCLUSIVE_READ_DEPTH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Enter an exclusive-read scope. Must be paired with [`end_exclusive_read`];
/// nesting/overlapping scopes are additive (the guard stays up until every
/// entered scope has exited).
#[cfg_attr(test, track_caller)]
pub fn begin_exclusive_read() {
    #[cfg(test)]
    record_last_begin(std::panic::Location::caller());
    EXCLUSIVE_READ_DEPTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Where the most recent `begin` came from. Tests only, and only so that a
/// failure which depends on what else is running names its cause instead of
/// looking like a flake.
#[cfg(test)]
fn record_last_begin(loc: &'static std::panic::Location<'static>) {
    if let Ok(mut slot) = LAST_BEGIN.lock() {
        *slot = Some(loc);
    }
}

#[cfg(test)]
static LAST_BEGIN: std::sync::Mutex<Option<&'static std::panic::Location<'static>>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn last_begin() -> String {
    LAST_BEGIN
        .lock()
        .ok()
        .and_then(|s| *s)
        .map(|l| format!("{}:{}", l.file(), l.line()))
        .unwrap_or_else(|| "nowhere".into())
}

/// Exit an exclusive-read scope entered with [`begin_exclusive_read`].
/// Saturating: an unmatched call (a bug — every call site pairs begin/end)
/// is a no-op in release rather than wrapping the counter around to
/// `usize::MAX` and jamming detection off forever; debug builds assert.
pub fn end_exclusive_read() {
    let prev = EXCLUSIVE_READ_DEPTH.fetch_update(
        std::sync::atomic::Ordering::Relaxed,
        std::sync::atomic::Ordering::Relaxed,
        |d| Some(d.saturating_sub(1)),
    );
    debug_assert!(
        prev != Ok(0),
        "end_exclusive_read called without a matching begin_exclusive_read"
    );
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn exclusive_read() -> bool {
    EXCLUSIVE_READ_DEPTH.load(std::sync::atomic::Ordering::Relaxed) > 0
}

/// The raw depth. Tests only — production asks [`exclusive_read`], because
/// what a caller may act on is "is anyone holding it", never how many.
#[cfg(test)]
pub(crate) fn exclusive_read_depth() -> usize {
    EXCLUSIVE_READ_DEPTH.load(std::sync::atomic::Ordering::Relaxed)
}

/// Serializes every test that touches the process-wide exclusive-read
/// depth — cargo's parallel runner would otherwise interleave them. Any test
/// that calls into code taking the guard needs this too, not only the tests
/// asserting on the depth: `rip::run_job` holds it for the length of a rip,
/// which is long enough to be observed by an assertion elsewhere.
#[cfg(test)]
static EXCLUSIVE_READ_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the exclusive-read test lock. **Every test that can reach a
/// `begin_exclusive_read`, directly or through production code, must hold this
/// for its duration.**
///
/// The lock existed and said "every test" but only the two tests in this file
/// took it, so nothing enforced the claim. `burn::run_job` and `rip::run_job`
/// both enter an exclusive-read scope, and their cancel tests run them for
/// real — so `refcount_nesting_and_underflow`, which asserts the counter
/// starts clear, failed whenever cargo happened to schedule them together.
/// That looked like a flake for as long as nobody read the depth: it is
/// always exactly 1, and always somebody else's live scope.
///
/// Recovering from poisoning is deliberate — one panicking test must not
/// cascade into every other test failing to acquire.
#[cfg(test)]
pub(crate) fn exclusive_read_test_guard() -> std::sync::MutexGuard<'static, ()> {
    EXCLUSIVE_READ_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod exclusive_read_tests {
    use super::*;

    // These share process-global state (`EXCLUSIVE_READ_DEPTH`), so they run
    // as one test to avoid interleaving with any other test that touches the
    // guard (cargo runs `#[test]`s concurrently by default).
    #[test]
    fn refcount_nesting_and_underflow() {
        let _guard = exclusive_read_test_guard();
        assert!(
            !exclusive_read(),
            "must start clear, saw depth {} — last begin_exclusive_read was {}",
            exclusive_read_depth(),
            last_begin()
        );

        begin_exclusive_read();
        begin_exclusive_read();
        assert!(exclusive_read(), "still held with one outstanding begin");
        end_exclusive_read();
        assert!(exclusive_read(), "nested begin/begin/end leaves it held");
        end_exclusive_read();
        assert!(!exclusive_read(), "final end clears it");

        // An unmatched end is a caller bug — real call sites always pair
        // begin/end, so `end_exclusive_read` intentionally `debug_assert`s
        // on it to catch that bug in debug/test builds. Exercising that
        // exact misuse here means catching the expected panic (silencing
        // the default hook so the test output stays clean) rather than
        // letting it fail the test — what this asserts is the *saturating*
        // half of the contract: the counter itself stays at 0, it does not
        // wrap around to `usize::MAX` and wedge detection off forever. In a
        // release build (`debug_assertions` off) the same call is a plain
        // no-op with no panic at all — see `end_exclusive_read`'s doc.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(end_exclusive_read);
        std::panic::set_hook(prev_hook);
        if cfg!(debug_assertions) {
            assert!(result.is_err(), "unmatched end must debug_assert");
        }
        assert!(!exclusive_read(), "unmatched end left the count saturated at 0, not wrapped");

        begin_exclusive_read();
        assert!(exclusive_read());
        end_exclusive_read();
        assert!(!exclusive_read(), "count still balanced after the earlier no-op");
    }
}

// ---------------------------------------------------------------------------
// macOS `.TOC.plist` — the shape CoreFoundation decodes it into, and the
// platform-neutral rules that turn it into a `DiscToc`
// ---------------------------------------------------------------------------

/// One row of an audio CD's table of contents, as the disc reports it.
///
/// The shape both readers hand to [`toc_from_points`]: macOS pulls it out of
/// `.TOC.plist`, and it exists as its own type so the rules below stay a pure
/// function with tests on every platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct TocEntry {
    /// The TOC "Point". Points 1–99 are real tracks; 0xA0 and up are session
    /// markers.
    pub point: u32,
    /// CDDB-absolute start frame (track 1 is 150).
    pub start: u32,
    pub is_data: bool,
}

/// Build a [`DiscToc`] from the disc's raw TOC rows.
///
/// Keeps only points 1–99: the higher points are session markers (0xA0 is the
/// first track number, 0xA2 the lead-out) and are not tracks. A TOC with no
/// tracks, or with no lead-out to bound the last one, is not a TOC.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn toc_from_points(entries: &[TocEntry], leadout: Option<u32>) -> Option<DiscToc> {
    let mut tracks: Vec<TocTrack> = entries
        .iter()
        .filter(|e| (1..=99).contains(&e.point))
        .map(|e| TocTrack {
            number: e.point as u8,
            start_frame: e.start,
            is_audio: !e.is_data,
        })
        .collect();
    tracks.sort_by_key(|t| t.number);
    match (tracks.is_empty(), leadout) {
        (false, Some(leadout_frame)) => Some(DiscToc {
            tracks,
            leadout_frame,
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// macOS media status (filled by `DRDeviceCopyStatus`; the mapping below is
// plain data handling, so it compiles and is tested everywhere)
// ---------------------------------------------------------------------------

/// What the drive says about the disc in it.
///
/// Every field is a value the framework hands back structured — this used to
/// be scraped out of `drutil status` text, and the parsing went away with the
/// subprocess. Populated by [`crate::disc::discrecording::Device::status`] on
/// macOS; the mapping to [`MediaInfo`] below stays platform-neutral so it can
/// be tested anywhere.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct MediaStatus {
    /// `kDRDeviceMediaStateKey` is `kDRDeviceMediaStateMediaPresent`.
    pub present: bool,
    /// From `kDRDeviceMediaTypeKey`. A pressed disc has no writable kind and
    /// reads as `Unknown`.
    pub kind: MediaKind,
    pub is_blank: bool,
    pub is_erasable: bool,
    pub is_overwritable: bool,
    pub free_blocks: Option<u64>,
    pub used_blocks: Option<u64>,
    /// `kDRDeviceMediaTrackCountKey` — how the mounted `.TOC.plist` volume is
    /// matched to the drive that holds it.
    pub tracks: Option<u32>,
    /// The media's whole-disk BSD node (`/dev/disk13`) from
    /// `kDRDeviceMediaBSDNameKey`. A data disc's mounted slice
    /// (`/dev/disk13s1`) shares this prefix, which is how
    /// [`data_disc_mount_path`] finds the mount the framework does not report.
    pub device_node: Option<String>,
    /// `kDRDeviceIsTrayOpenKey`, read from the top-level status rather than
    /// the media dictionary, because an open tray has no media to describe.
    pub tray_open: bool,
}

/// Map a drive's reported media state into [`MediaInfo`].
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn media_from_status(st: &MediaStatus) -> MediaInfo {
    if !st.present {
        return MediaInfo::none();
    }
    // A disc the drive can rewrite says so directly. The kind is kept as a
    // second source because DVD-RAM and the RW types are rewritable by
    // definition, whatever the loaded medium's erase state reports.
    let rewritable = st.is_erasable
        || st.is_overwritable
        || matches!(st.kind, MediaKind::CdRw | MediaKind::DvdRw | MediaKind::DvdRam);
    // 2048-byte data blocks — close enough for capacity display; the burn
    // phases refine per-media accounting.
    MediaInfo {
        present: true,
        is_audio_cd: false, // decided by TOC-volume matching, not by the drive
        is_blank: st.is_blank,
        rewritable,
        kind: st.kind,
        free_bytes: st.free_blocks.unwrap_or(0) * 2048,
        capacity_bytes: (st.free_blocks.unwrap_or(0) + st.used_blocks.unwrap_or(0)) * 2048,
        // The drive reports the typing itself, so reaching here means we read it.
        typing_unknown: false,
    }
}

/// Find a data disc's mount point in BSD `mount`(8) output by matching a
/// device slice against the drive's whole-disk node
/// ([`MediaStatus::device_node`], e.g. `/dev/disk13` — a slice mounts as
/// `/dev/disk13s1`, `/dev/disk13s2`, …). DiscRecording never reports a mount
/// path, so this is Task 11's fill-in: macOS auto-mounts data discs the kernel
/// already knows about (unlike audio CDs, `list_drives`'s `.TOC.plist` walk
/// of `/Volumes` doesn't apply — a data disc's ISO9660/UDF volume carries no
/// such marker file).
///
/// Takes the mount table as `(device, mount point)` pairs rather than reading
/// it, so the matching rules stay a pure function with tests on every
/// platform while the reading of them is a syscall — see
/// [`data_disc_mount_path`].
///
/// Returns the first slice of `device_node` found mounted; `None` when
/// nothing matches (not yet auto-mounted, or the kernel is still probing it —
/// callers already only reach this after `media.present` is true, so a miss
/// here is surfaced as "no data-disc browsing" rather than retried).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn mount_for_device<'a>(
    mounts: impl IntoIterator<Item = (&'a str, &'a str)>,
    device_node: &str,
) -> Option<PathBuf> {
    // Two shapes, both real. A disc carrying a partition scheme mounts a slice
    // (`/dev/disk12s0`); a disc written as one plain filesystem image mounts
    // the whole device with no suffix at all (`/dev/disk12`), which is what a
    // DVD+RW burned from an ISO does. Matching only the slice missed the
    // second kind entirely, so its files never reached the disc view
    // (2026-08-12). Comparing the whole node by equality rather than by prefix
    // keeps `/dev/disk13` from matching `/dev/disk130`, same as the "s" does.
    let slice_prefix = format!("{device_node}s");
    mounts.into_iter().find_map(|(dev, mount)| {
        if dev != device_node && !dev.starts_with(&slice_prefix) {
            return None;
        }
        if mount.is_empty() {
            return None;
        }
        Some(PathBuf::from(mount))
    })
}

/// Resolve `device_node`'s data-disc mount path from the kernel's mount table.
///
/// `getfsstat(2)`, not `mount`(8). The App Sandbox blocks spawning
/// subprocesses, so the Mac App Store build cannot shell out for this — and
/// the syscall is what `mount`(8) itself calls, so nothing is lost by reading
/// it directly. It also removes a text parse: `f_mntfromname` and
/// `f_mntonname` are the two fields the line parser existed to recover, and a
/// volume name containing " (" can no longer confuse the split.
#[cfg(target_os = "macos")]
pub(crate) fn data_disc_mount_path(device_node: &str) -> Option<PathBuf> {
    let mounts = mount_table();
    mount_for_device(
        mounts.iter().map(|(d, m)| (d.as_str(), m.as_str())),
        device_node,
    )
}

/// The kernel's mount table as `(device, mount point)` pairs.
#[cfg(target_os = "macos")]
fn mount_table() -> Vec<(String, String)> {
    // Ask for the count first: the table can change between the two calls, so
    // the buffer is oversized a little and the second call's return — never
    // the first — says how many entries were actually written.
    // SAFETY: a null buffer with size 0 is the documented way to ask for the
    // count without writing anything.
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count <= 0 {
        return Vec::new();
    }
    let capacity = count as usize + 8;
    let mut buf: Vec<libc::statfs> = Vec::with_capacity(capacity);
    let bytes = (capacity * std::mem::size_of::<libc::statfs>()) as libc::c_int;
    // SAFETY: `buf` has room for `capacity` entries and `bytes` describes
    // exactly that much space. MNT_NOWAIT reads cached state rather than
    // querying every filesystem, which is what keeps this off the disc.
    let written = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    if written <= 0 {
        return Vec::new();
    }
    // SAFETY: the call reported `written` initialised entries, and `written`
    // cannot exceed the capacity the buffer was given.
    unsafe { buf.set_len((written as usize).min(capacity)) };
    buf.iter()
        .map(|fs| (c_str(&fs.f_mntfromname), c_str(&fs.f_mntonname)))
        .collect()
}

/// A fixed-size NUL-terminated C field as a `String`, lossily.
#[cfg(target_os = "macos")]
fn c_str(field: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = field
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

// ---------------------------------------------------------------------------
// Linux `cd-info` output parser (platform-neutral text handling)
// ---------------------------------------------------------------------------

/// Parse `cd-info` track-list output into a TOC. cd-info prints post-pregap
/// LSNs, so +150 converts to CDDB-absolute frames:
/// ```text
/// CD-ROM Track List (1 - 8)
///   #: MSF       LSN    Type   Green? Copy? Channels Premphasis?
///   1: 00:02:00  000000 audio  false  no    2        no
/// 170: 27:43:41  124616 leadout
/// ```
// Only the Linux platform glue calls this; it stays compiled (and tested)
// everywhere so the parser can't rot unnoticed on the other platforms.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_cd_info(out: &str) -> Option<DiscToc> {
    let mut tracks: Vec<TocTrack> = Vec::new();
    let mut leadout: Option<u32> = None;

    for line in out.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.len() < 4 {
            continue;
        }
        let Some(numtok) = toks[0].strip_suffix(':') else {
            continue;
        };
        let Ok(number) = numtok.parse::<u32>() else {
            continue;
        };
        let Some(kind) = toks.get(3) else { continue };
        let Ok(lsn) = toks[2].parse::<u32>() else {
            continue;
        };
        match *kind {
            "audio" | "data" if (1..=99).contains(&number) => tracks.push(TocTrack {
                number: number as u8,
                start_frame: lsn + 150,
                is_audio: *kind == "audio",
            }),
            "leadout" => leadout = Some(lsn + 150),
            _ => {}
        }
    }

    tracks.sort_by_key(|t| t.number);
    match (tracks.is_empty(), leadout) {
        (false, Some(leadout_frame)) => Some(DiscToc {
            tracks,
            leadout_frame,
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Subprocess helper (both platforms)
// ---------------------------------------------------------------------------

// Linux only now. macOS reached the framework directly for detection,
// CD-TEXT, eject and burning, then `getfsstat` for the mount table and
// CoreFoundation for `.TOC.plist` — so nothing on that platform shells out any
// more, which is what App Sandbox requires.
#[cfg(target_os = "linux")]
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// Poll cost control (Linux): what a status poll should do per drive
// ---------------------------------------------------------------------------

/// Linux `<linux/cdrom.h>` values for the cheap status ioctls.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const CDROM_DRIVE_STATUS: i32 = 0x5326;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const CDROM_MEDIA_CHANGED: i32 = 0x5325;
/// `CDSL_CURRENT` — "the currently loaded slot".
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const CDSL_CURRENT: i32 = i32::MAX;
/// `CDS_DISC_OK` — a readable disc is loaded.
const CDS_DISC_OK: i32 = 4;

/// Parse `cdrskin dev=… -minfo` output into the loaded media's typing —
/// the Linux probe for discs WITHOUT a readable TOC (blank / just-erased
/// media), where the burn phases need kind + capacity + blank/rewritable.
/// Pure `&str` parser, unit-tested against captured real output. `None`
/// when the output carries no "Mounted media type" line (no disc, or a
/// tool error).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_minfo(out: &str) -> Option<MediaInfo> {
    let mut kind: Option<MediaKind> = None;
    let mut blank = false;
    let mut erasable = false;
    let mut leadout_blocks: u64 = 0;
    for line in out.lines() {
        let l = line.trim();
        if let Some(v) = l.strip_prefix("Mounted media type:") {
            kind = Some(match v.trim() {
                "CD-R" => MediaKind::CdR,
                "CD-RW" => MediaKind::CdRw,
                "DVD-R" | "DVD+R" | "DVD+R/DL" => MediaKind::DvdR,
                "DVD-RW" | "DVD+RW" | "DVD-RW sequential recording"
                | "DVD-RW restricted overwrite" => MediaKind::DvdRw,
                "DVD-RAM" => MediaKind::DvdRam,
                _ => MediaKind::Unknown,
            });
        } else if let Some(v) = l.strip_prefix("disk status:") {
            blank = v.trim() == "empty";
        } else if l.contains("Is erasable") && !l.contains("not") {
            erasable = true;
        } else if let Some(v) = l.strip_prefix("ATIP start of lead out:") {
            leadout_blocks = v
                .trim()
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        }
    }
    let kind = kind?;
    // 2048 data bytes per block — the convention MediaInfo's byte fields use
    // (audio capacity derives as blocks/75 seconds from the same figure).
    // CDs carry an ATIP lead-out we can read; DVDs do NOT ("No reliable
    // track size" from cdrskin -minfo), so leadout_blocks stays 0 and the
    // over-capacity gate would be silently disabled (a data burn could be
    // attempted well past the disc's size). Fall back to the standard
    // single-layer capacity per DVD kind so the gate works (2026-07-17).
    let capacity_bytes = if leadout_blocks > 0 {
        leadout_blocks * 2048
    } else {
        match kind {
            // 4.7 GB nominal single-layer DVD (DVD±R/RW, DVD-RAM).
            MediaKind::DvdR | MediaKind::DvdRw | MediaKind::DvdRam => 4_700_000_000,
            _ => 0,
        }
    };
    Some(MediaInfo {
        present: true,
        is_audio_cd: false,
        is_blank: blank,
        rewritable: erasable,
        kind,
        free_bytes: if blank { capacity_bytes } else { 0 },
        capacity_bytes,
        // Parsing minfo output at all means the probe ran.
        typing_unknown: false,
    })
}

/// Overlay `-minfo` media typing (kind, blank, rewritable, capacity) onto
/// TOC-derived info. The TOC path owns `present` and `is_audio_cd` (typing
/// tools don't judge audio); everything else comes from the typing probe.
/// Without this a burned CD-RW — which has a readable TOC — looked
/// write-once-with-content and every erase/re-burn was refused.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn merge_minfo_typing(toc_media: MediaInfo, minfo: MediaInfo) -> MediaInfo {
    MediaInfo {
        present: true,
        is_audio_cd: toc_media.is_audio_cd,
        ..minfo
    }
}

/// Assemble a [`DiscToc`] from raw TOC entries as the `CDROMREADTOCENTRY`
/// ioctl reports them: `(track number, ctrl nibble, LBA)` per track plus the
/// lead-out LBA. Adds the +150 pregap (LBA → CDDB-absolute frame) and maps
/// the ctrl "data track" bit (0x04) to `is_audio`. Pure — the ioctl glue
/// only collects the tuples.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    allow(dead_code)
)]
pub(super) fn toc_from_entries(entries: &[(u8, u8, i32)], leadout_lba: i32) -> Option<DiscToc> {
    if entries.is_empty() || leadout_lba <= 0 {
        return None;
    }
    let tracks: Vec<TocTrack> = entries
        .iter()
        .filter(|(_, _, lba)| *lba >= 0)
        .map(|(number, ctrl, lba)| TocTrack {
            number: *number,
            start_frame: *lba as u32 + 150,
            is_audio: ctrl & 0x04 == 0,
        })
        .collect();
    if tracks.is_empty() {
        return None;
    }
    Some(DiscToc {
        tracks,
        leadout_frame: leadout_lba as u32 + 150,
    })
}

/// Parse an MMC `READ TOC` format-0 response into the `(track, ctrl, LBA)`
/// tuples [`toc_from_entries`] wants, plus the lead-out LBA.
///
/// The response is a four-byte header (a big-endian length counting from
/// byte 2, then the first and last track numbers) followed by eight-byte
/// descriptors. Byte 1 of a descriptor packs two nibbles as
/// `(ADR << 4) | CONTROL`, so the "this is a data track" bit is `0x04` of the
/// *low* nibble. Linux's `cdrom_tocentry` reports the same bit in the high
/// nibble, which is why that path shifts where this one masks. On an
/// all-audio disc both spellings agree, so getting it wrong here would only
/// show up on a mixed-mode disc.
///
/// Pure, so the byte handling is testable off a captured buffer instead of
/// only against a disc in a drive.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn parse_mmc_toc(buf: &[u8]) -> Option<(Vec<(u8, u8, i32)>, i32)> {
    if buf.len() < 4 {
        return None;
    }
    // The length field counts from byte 2, so the last meaningful byte sits
    // at `declared + 1`. Trust it over the buffer handed to the kernel, which
    // is deliberately oversized.
    let declared = u16::from_be_bytes([buf[0], buf[1]]) as usize;
    let end = (declared + 2).min(buf.len());

    let mut entries = Vec::new();
    let mut leadout = None;
    let mut at = 4;
    while at + 8 <= end {
        let d = &buf[at..at + 8];
        let ctrl = d[1] & 0x0F;
        let track = d[2];
        let lba = i32::from_be_bytes([d[4], d[5], d[6], d[7]]);
        match track {
            0xAA => leadout = Some(lba),
            1..=99 => entries.push((track, ctrl, lba)),
            _ => {}
        }
        at += 8;
    }
    // No lead-out means no usable TOC: track lengths are measured against it.
    Some((entries, leadout?))
}

/// What a poll should do for one drive, decided from the no-spin status
/// ioctl + the previous poll's entry. Pure so the matrix is unit-testable.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum ProbeAction {
    /// Same readable disc as last poll — reuse the previous entry, don't
    /// touch the medium.
    Reuse,
    /// No readable disc (empty, tray open, not ready) — report an empty
    /// drive without probing.
    Empty,
    /// New or changed disc (or no usable history) — run the full TOC probe.
    Probe,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn probe_action(status: i32, media_changed: bool, prev_present: Option<bool>) -> ProbeAction {
    if status != CDS_DISC_OK {
        return ProbeAction::Empty;
    }
    match prev_present {
        Some(true) if !media_changed => ProbeAction::Reuse,
        _ => ProbeAction::Probe,
    }
}

/// Eject the disc in a drive. Blocking (the tray takes a moment) — call off
/// the UI thread. `drive_id` is the same id `list_drives` reports: Linux the
/// device node (`eject /dev/srX`), macOS the drive's enumeration index, which
/// `DRDeviceEjectMedia` is asked for directly. The caller must not be reading
/// the drive (playback/rip) — the OS refuses to eject a busy device.
pub fn eject(drive_id: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let device = crate::disc::discrecording::device_at_id(drive_id)
            .ok_or_else(|| format!("no drive {drive_id}"))?;
        device.eject()
    }

    #[cfg(target_os = "linux")]
    {
        // `eject(1)` unmounts the filesystem first, but a udisks `/run/media`
        // mount isn't in fstab so its umount needs root → "must be superuser
        // to unmount". Drop the mount via udisks (session-owned) first;
        // best-effort, an already-unmounted disc is a no-op (2026-07-17).
        let _ = crate::disc::mount::unmount_disc(drive_id);

        let out = std::process::Command::new("eject")
            .arg(drive_id)
            .output()
            .map_err(|e| format!("couldn't run eject: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            let err = err.trim();
            Err(if err.is_empty() {
                format!("eject failed ({})", out.status)
            } else {
                err.to_string()
            })
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Err(format!("eject not supported on this platform ({drive_id})"))
}

// ---------------------------------------------------------------------------
// macOS platform glue
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::ffi::c_void;
    use std::path::Path;

    use objc2_core_foundation::{
        CFArray, CFBoolean, CFData, CFDictionary, CFNumber, CFPropertyListCreateWithData,
        CFRetained, CFString, CFType,
    };

    pub fn list_drives() -> Vec<OpticalDrive> {
        let devices = crate::disc::discrecording::devices();
        if devices.is_empty() {
            return Vec::new();
        }


        devices
            .into_iter()
            .enumerate()
            .map(|(i, device)| {
                let status = device.status();
                let mut media = media_from_status(&status);

                // Everything about the loaded disc is resolved from THIS
                // drive's device node, so nothing can be attributed to the
                // wrong drive.
                //
                // This used to claim a mounted audio volume by matching track
                // counts, and fell back to "the first unclaimed one" when no
                // volume matched. With two drives attached and one audio CD,
                // the drive holding something else claimed the audio CD's
                // volume: it reported its own name with the other drive's
                // 15-track TOC, and offered to play a disc it did not have.
                // Matching by device node cannot do that.
                let mut toc = None;
                let mut mount_path = None;
                if media.present {
                    if let Some(node) = &status.device_node {
                        // The mount that belongs to this node, from the
                        // kernel's own table. An audio CD mounts as cddafs
                        // and a data disc as ISO/UDF; both answer here.
                        mount_path = data_disc_mount_path(node);

                        // The drive's own TOC is authoritative, and is the
                        // only source that answers under the App Sandbox,
                        // which refuses every read inside `/Volumes/<disc>`.
                        //
                        // Only an answer with audio in it counts. A data disc
                        // has a TOC too, and accepting that would hand every
                        // caller of `toc` a single-track table where it used
                        // to get `None`.
                        toc = crate::disc::discrecording::read_toc(node)
                            .filter(|t| t.tracks.iter().any(|tr| tr.is_audio));

                        // Fallback for a drive whose TOC ioctl fails: the
                        // volume's own `.TOC.plist`, read from this drive's
                        // mount rather than from whichever volume happened to
                        // be first.
                        if toc.is_none() {
                            toc = mount_path
                                .as_ref()
                                .and_then(|m| toc_from_plist(&m.join(".TOC.plist")))
                                .filter(|t| t.tracks.iter().any(|tr| tr.is_audio));
                        }

                        media.is_audio_cd = toc.is_some();
                    }
                }

                OpticalDrive {
                    supports_writing: device.can_write(),
                    // The drive's own identity, not its position in the
                    // framework's array. That position moves when a drive is
                    // attached or removed, and it was being used as a name:
                    // Open Tray opened the wrong drive, and a disc could be
                    // attributed to a drive that did not hold it. Nothing
                    // shells out to `drutil -drive N` any more, so there is
                    // no longer a reason for the index to leak out here.
                    id: device
                        .stable_id()
                        .unwrap_or_else(|| format!("drive-{}", i + 1)),
                    label: device
                        .label()
                        .unwrap_or_else(|| format!("Optical drive {}", i + 1)),
                    media,
                    toc,
                    mount_path,
                }
            })
            .collect()
    }

    /// Read an audio CD's `.TOC.plist` and build its [`DiscToc`].
    ///
    /// `CFPropertyListCreateWithData`, not `plutil -convert xml1`. The file is
    /// a *binary* plist carrying a raw data blob, which is why the detector
    /// used to shell out and scan the converted XML — and why it could not: the
    /// App Sandbox forbids spawning, so the Mac App Store build has to decode
    /// it in-process. CoreFoundation reads either format, so the conversion
    /// step disappears along with the text scan.
    pub(super) fn toc_from_plist(plist: &Path) -> Option<DiscToc> {
        let bytes = std::fs::read(plist).ok()?;
        let data = CFData::from_bytes(&bytes);
        // SAFETY: a live CFData, no options, and null out-parameters — the
        // format and the error are both things this has no use for, and the
        // call documents null as "don't report it".
        let plist = unsafe {
            CFPropertyListCreateWithData(
                None,
                Some(&data),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        }?;
        let root = plist.downcast_ref::<CFDictionary>()?;
        toc_from_plist_root(root)
    }

    /// Walk the decoded plist: `Sessions` → each session's `Leadout Block` and
    /// `Track Array` → each track's `Point`, `Start Block` and `Data`.
    ///
    /// The first lead-out wins, matching what the text scan did. Start blocks
    /// in this file are already CDDB-absolute (track 1 is 150), so nothing is
    /// added here.
    fn toc_from_plist_root(root: &CFDictionary) -> Option<DiscToc> {
        let sessions = dict_value(root, "Sessions")?;
        let sessions = sessions.downcast_ref::<CFArray>()?;
        let mut entries: Vec<TocEntry> = Vec::new();
        let mut leadout: Option<u32> = None;
        for i in 0..sessions.count() {
            // SAFETY: `i` is in range, and the element is borrowed for as long
            // as the array is.
            let session = unsafe { sessions.value_at_index(i) };
            let Some(session) = (unsafe { session.cast::<CFType>().as_ref() }) else {
                continue;
            };
            let Some(session) = session.downcast_ref::<CFDictionary>() else {
                continue;
            };
            if leadout.is_none() {
                leadout = dict_number(session, "Leadout Block");
            }
            let Some(tracks) = dict_value(session, "Track Array") else {
                continue;
            };
            let Some(tracks) = tracks.downcast_ref::<CFArray>() else {
                continue;
            };
            for t in 0..tracks.count() {
                // SAFETY: `t` is in range; the element outlives this body.
                let track = unsafe { tracks.value_at_index(t) };
                let Some(track) = (unsafe { track.cast::<CFType>().as_ref() }) else {
                    continue;
                };
                let Some(track) = track.downcast_ref::<CFDictionary>() else {
                    continue;
                };
                let (Some(point), Some(start)) = (
                    dict_number(track, "Point"),
                    dict_number(track, "Start Block"),
                ) else {
                    continue;
                };
                entries.push(TocEntry {
                    point,
                    start,
                    is_data: dict_bool(track, "Data"),
                });
            }
        }
        toc_from_points(&entries, leadout)
    }

    /// One value out of a plist dictionary, by key.
    fn dict_value(dict: &CFDictionary, key: &str) -> Option<CFRetained<CFType>> {
        let key = CFString::from_str(key);
        let key_ptr = CFRetained::as_ptr(&key).as_ptr().cast::<c_void>().cast_const();
        // SAFETY: `dict` and `key` are live CoreFoundation objects; the value
        // comes back borrowed (Get rule), so it is retained before escaping.
        let value = unsafe { dict.value(key_ptr) };
        let value = std::ptr::NonNull::new(value.cast_mut())?.cast::<CFType>();
        // SAFETY: the pointer names a live CF object owned by the dictionary.
        Some(unsafe { CFRetained::retain(value) })
    }

    fn dict_number(dict: &CFDictionary, key: &str) -> Option<u32> {
        let n = dict_value(dict, key)?.downcast_ref::<CFNumber>()?.as_i64()?;
        u32::try_from(n).ok()
    }

    fn dict_bool(dict: &CFDictionary, key: &str) -> bool {
        dict_value(dict, key)
            .and_then(|v| v.downcast_ref::<CFBoolean>().map(CFBoolean::as_bool))
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Linux platform glue
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    pub fn list_drives() -> Vec<OpticalDrive> {
        list_drives_cached(&[])
    }

    /// Like [`list_drives`], but spins the disc up ONLY when something
    /// changed. The full `cd-info` probe reads the TOC, which physically
    /// spins the drive — running it on a 10 s poll keeps the disc spinning
    /// forever. Instead each poll asks the kernel for the drive status
    /// (`CDROM_DRIVE_STATUS`, a no-media-access ioctl) and reuses `prev`'s
    /// entry while the same disc is still sitting there.
    pub fn list_drives_cached(prev: &[OpticalDrive]) -> Vec<OpticalDrive> {
        // A streaming read owns the drive: answer from the previous state
        // without opening the device (see EXCLUSIVE_READ).
        if super::exclusive_read() {
            return prev.to_vec();
        }
        let mut drives: Vec<OpticalDrive> = Vec::new();
        let Ok(entries) = std::fs::read_dir("/sys/block") else {
            return drives;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let rest = name.strip_prefix("sr")?;
                rest.parse::<u32>().ok()?;
                Some(name)
            })
            .collect();
        names.sort();

        for name in names {
            let node = format!("/dev/{name}");
            let label = sysfs_label(&name).unwrap_or_else(|| node.clone());
            let prev_drive = prev.iter().find(|d| d.id == node);

            let action = match drive_status(&node) {
                Some(status) => super::probe_action(
                    status,
                    media_changed(&node).unwrap_or(true),
                    prev_drive.map(|d| d.media.present),
                ),
                // Status ioctl unavailable (permissions?) — fall back to the
                // old always-probe behavior rather than reporting nothing.
                None => super::ProbeAction::Probe,
            };

            match action {
                super::ProbeAction::Reuse => {
                    let mut d = prev_drive
                        .cloned()
                        .expect("Reuse only chosen when a previous entry exists");
                    d.label = label;
                    drives.push(d);
                }
                super::ProbeAction::Empty => drives.push(OpticalDrive {
                    // An empty tray still has hardware behind it, and whether
                    // that hardware writes is exactly what decides if a queue
                    // can be staged before a blank goes in.
                    supports_writing: drive_supports_writing(&node),
                    id: node,
                    label,
                    media: MediaInfo::none(),
                    toc: None,
                    mount_path: None,
                }),
                super::ProbeAction::Probe => drives.push(probe_drive(node, label)),
            }
        }
        drives
    }

    /// Full probe of one drive. The TOC comes from the `CDROMREADTOC*`
    /// ioctls — the drive caches the TOC when the disc loads, so this
    /// answers in milliseconds, where `cd-info` also reads MCN + CD-TEXT
    /// (tens of seconds of medium seeks on some discs). cd-info stays as
    /// the fallback when the ioctls fail. Finer media typing (blank/RW)
    /// lands with the burn phases.
    fn probe_drive(node: String, label: String) -> OpticalDrive {
        let toc = read_toc_ioctl(&node)
            .or_else(|| run("cd-info", &["--no-header", &node]).and_then(|o| parse_cd_info(&o)));
        let media = match &toc {
            // Readable TOC still needs the `-minfo` typing merged in —
            // a burned CD-RW has a TOC, and without kind/rewritable the
            // burn phases refuse to erase it. One extra subprocess per
            // media *change* only (unchanged poll ticks are ioctl-only).
            Some(t) => {
                let toc_media = MediaInfo {
                    present: true,
                    is_audio_cd: t.tracks.iter().any(|tr| tr.is_audio),
                    ..MediaInfo::none()
                };
                run("cdrskin", &[&format!("dev={node}"), "-minfo"])
                    .and_then(|o| super::parse_minfo(&o))
                    // udisks answers with the disc mounted, which is when
                    // -minfo can't open the device at all — the common case
                    // right after burning a data disc, since the desktop
                    // mounts what we just wrote (2026-08-10). It carries no
                    // lead-out, so its capacity is nominal; that is why it is
                    // second and not first.
                    .or_else(|| crate::disc::udisks::optical_media(&node))
                    .map(|m| super::merge_minfo_typing(toc_media.clone(), m))
                    // Neither probe answered: flag it rather than let the
                    // defaults read as "not blank, not rewritable", which
                    // `erase_decision` can only treat as
                    // write-once-with-content and refuse.
                    .unwrap_or(MediaInfo { typing_unknown: true, ..toc_media })
            }
            // No readable TOC but the status ioctl said "disc ok" (the
            // caller only probes then): blank / just-erased media — type it
            // via cdrskin -minfo (kind, capacity, blank/rewritable) for the
            // burn phases.
            None => run("cdrskin", &[&format!("dev={node}"), "-minfo"])
                .and_then(|o| super::parse_minfo(&o))
                .or_else(|| crate::disc::udisks::optical_media(&node))
                .unwrap_or_else(MediaInfo::none),
        };
        OpticalDrive {
            supports_writing: drive_supports_writing(&node),
            id: node,
            label,
            media,
            toc,
            mount_path: None,
        }
    }

    /// Whether the drive at `node` can write.
    ///
    /// udisks2 not answering is treated as "yes": a drive that burns must not
    /// lose its burn panel because the daemon was briefly unreachable, and the
    /// panel's own buttons still refuse a disc that cannot take a burn.
    fn drive_supports_writing(node: &str) -> bool {
        write_capability_cached(node, |n| crate::disc::udisks::drive_supports_writing(n))
    }

    /// Remember a drive's write capability across polls.
    ///
    /// Whether the hardware can burn is a property of the drive, not of the
    /// disc in it, so it cannot change while the machine is running. Asking
    /// udisks every time was a full `GetManagedObjects` per drive per poll —
    /// every two seconds — which makes udisks refresh its drive state and can
    /// leave an optical drive busy enough that the next media probe fails.
    /// The disc then types as unknown and an audio CD shows up as a data one.
    ///
    /// `probe` is injected so the caching is testable without a drive.
    pub(super) fn write_capability_cached(
        node: &str,
        probe: impl Fn(&str) -> Option<bool>,
    ) -> bool {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));

        if let Ok(map) = cache.lock() {
            if let Some(known) = map.get(node) {
                return *known;
            }
        }
        // Only a definite answer is remembered: udisks being briefly
        // unreachable must not pin a drive to the fallback for the session.
        match probe(node) {
            Some(answer) => {
                if let Ok(mut map) = cache.lock() {
                    map.insert(node.to_string(), answer);
                }
                answer
            }
            None => true,
        }
    }

    /// Read the loaded disc's TOC through the kernel (`CDROMREADTOCHDR` +
    /// one `CDROMREADTOCENTRY` per track + lead-out, LBA format). No medium
    /// seeks — the drive already holds the TOC. `None` when there's no
    /// readable disc or an ioctl fails.
    fn read_toc_ioctl(node: &str) -> Option<DiscToc> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        const CDROMREADTOCHDR: i32 = 0x5305;
        const CDROMREADTOCENTRY: i32 = 0x5306;
        const CDROM_LEADOUT: u8 = 0xAA;
        const CDROM_LBA: u8 = 0x01;

        /// `struct cdrom_tochdr`.
        #[repr(C)]
        #[derive(Default)]
        struct TocHdr {
            trk0: u8,
            trk1: u8,
        }
        /// `struct cdrom_tocentry` (adr/ctrl share one byte: adr low
        /// nibble, ctrl high — little-endian GCC bitfield order).
        #[repr(C)]
        #[derive(Default)]
        struct TocEntry {
            track: u8,
            adr_ctrl: u8,
            format: u8,
            lba: i32,
            datamode: u8,
        }

        let f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(node)
            .ok()?;
        let fd = f.as_raw_fd();

        let mut hdr = TocHdr::default();
        if unsafe { libc::ioctl(fd, CDROMREADTOCHDR as libc::c_ulong, &mut hdr) } < 0 {
            return None;
        }
        if hdr.trk0 == 0 || hdr.trk1 < hdr.trk0 {
            return None;
        }

        let read_entry = |track: u8| -> Option<(u8, u8, i32)> {
            let mut e = TocEntry {
                track,
                format: CDROM_LBA,
                ..TocEntry::default()
            };
            if unsafe { libc::ioctl(fd, CDROMREADTOCENTRY as libc::c_ulong, &mut e) } < 0 {
                return None;
            }
            Some((track, e.adr_ctrl >> 4, e.lba))
        };

        let mut entries = Vec::with_capacity((hdr.trk1 - hdr.trk0 + 1) as usize);
        for t in hdr.trk0..=hdr.trk1 {
            entries.push(read_entry(t)?);
        }
        let (_, _, leadout_lba) = read_entry(CDROM_LEADOUT)?;
        super::toc_from_entries(&entries, leadout_lba)
    }

    /// `CDROM_DRIVE_STATUS` for a device node — answered by the drive
    /// without touching the medium (no spin-up). `None` when the node can't
    /// be opened or the ioctl isn't supported.
    fn drive_status(node: &str) -> Option<i32> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        // O_NONBLOCK is the documented way to open an optical device without
        // requiring (or waiting on) a readable medium.
        let f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(node)
            .ok()?;
        let r = unsafe {
            libc::ioctl(
                f.as_raw_fd(),
                super::CDROM_DRIVE_STATUS as libc::c_ulong,
                super::CDSL_CURRENT,
            )
        };
        (r >= 0).then_some(r)
    }

    /// `CDROM_MEDIA_CHANGED`: has the medium changed since the last time
    /// anyone asked? Catches a disc swapped between two polls that both see
    /// "disc ok". Also a pure drive-firmware query — no spin-up.
    fn media_changed(node: &str) -> Option<bool> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        let f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(node)
            .ok()?;
        let r = unsafe {
            libc::ioctl(
                f.as_raw_fd(),
                super::CDROM_MEDIA_CHANGED as libc::c_ulong,
                super::CDSL_CURRENT,
            )
        };
        (r >= 0).then_some(r != 0)
    }

    /// "VENDOR MODEL" from sysfs, e.g. "/sys/block/sr0/device/{vendor,model}".
    fn sysfs_label(name: &str) -> Option<String> {
        let base = format!("/sys/block/{name}/device");
        let vendor = std::fs::read_to_string(format!("{base}/vendor")).ok()?;
        let model = std::fs::read_to_string(format!("{base}/model")).ok()?;
        let label = format!("{} {}", vendor.trim(), model.trim());
        let label = label.trim().to_string();
        if label.is_empty() { None } else { Some(label) }
    }
}

// ---------------------------------------------------------------------------
// Any other platform: no optical support.
// ---------------------------------------------------------------------------

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use super::OpticalDrive;
    pub fn list_drives() -> Vec<OpticalDrive> {
        Vec::new()
    }
}

/// Hash of the load-state a user can see: media kind/flags, TOC track
/// count, capacity. The GTK poll compares per-drive fingerprints across
/// ticks and refreshes an open detail view when the SHOWN drive's changes
/// (disc swapped/ejected/inserted) — unchanged drives are never disturbed.
pub fn media_fingerprint(d: &OpticalDrive) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    d.media.present.hash(&mut h);
    d.media.is_audio_cd.hash(&mut h);
    d.media.is_blank.hash(&mut h);
    d.media.rewritable.hash(&mut h);
    (d.media.kind as u8).hash(&mut h);
    d.media.capacity_bytes.hash(&mut h);
    d.media.free_bytes.hash(&mut h);
    d.toc.as_ref().map(|t| t.tracks.len()).unwrap_or(0).hash(&mut h);
    h.finish()
}

// ---------------------------------------------------------------------------
// Tests — all parsers, on every platform.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed capture of a real 8-track disc's `.TOC.plist` (xml1 form),
    /// tracks 4–7 elided.
    // Read only by the plist test, which is macOS-only.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    const TOC_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>Format 0x02 TOC Data</key>
	<data>
	AJEBAQEQAKAAAAAAAQAAARAAoQ==
	</data>
	<key>Sessions</key>
	<array>
		<dict>
			<key>First Track</key>
			<integer>1</integer>
			<key>Last Track</key>
			<integer>8</integer>
			<key>Leadout Block</key>
			<integer>124766</integer>
			<key>Session Number</key>
			<integer>1</integer>
			<key>Session Type</key>
			<integer>0</integer>
			<key>Track Array</key>
			<array>
				<dict>
					<key>Data</key>
					<false/>
					<key>Point</key>
					<integer>1</integer>
					<key>Session Number</key>
					<integer>1</integer>
					<key>Start Block</key>
					<integer>150</integer>
				</dict>
				<dict>
					<key>Data</key>
					<false/>
					<key>Point</key>
					<integer>2</integer>
					<key>Session Number</key>
					<integer>1</integer>
					<key>Start Block</key>
					<integer>13834</integer>
				</dict>
				<dict>
					<key>Data</key>
					<true/>
					<key>Point</key>
					<integer>3</integer>
					<key>Session Number</key>
					<integer>1</integer>
					<key>Start Block</key>
					<integer>30216</integer>
				</dict>
			</array>
		</dict>
	</array>
</dict>
</plist>"#;

    /// The CoreFoundation walk over a real `.TOC.plist`, against the same
    /// fixture the old `plutil` text scan used.
    ///
    /// `CFPropertyListCreateWithData` reads XML and binary alike, so the
    /// fixture survives the change from scanning converted text to decoding
    /// the file — and this now covers the dictionary walk, which is where the
    /// work moved to.
    #[cfg(target_os = "macos")]
    #[test]
    fn toc_plist_parses_tracks_leadout_and_data_flag() {
        let path = std::env::temp_dir().join(format!("sparkamp-toc-{}.plist", std::process::id()));
        std::fs::write(&path, TOC_XML).expect("write fixture");
        let toc = platform::toc_from_plist(&path).expect("toc");
        let _ = std::fs::remove_file(&path);
        assert_eq!(toc.leadout_frame, 124766);
        assert_eq!(toc.tracks.len(), 3);
        assert_eq!(toc.tracks[0].number, 1);
        assert_eq!(toc.tracks[0].start_frame, 150); // already CDDB-absolute
        assert!(toc.tracks[0].is_audio);
        assert_eq!(toc.tracks[1].start_frame, 13834);
        assert!(!toc.tracks[2].is_audio); // Data=true track
    }

    /// A file that is not a plist at all must read as "no TOC", not panic.
    #[cfg(target_os = "macos")]
    #[test]
    fn toc_plist_rejects_a_file_that_is_not_a_plist() {
        let path = std::env::temp_dir().join(format!("sparkamp-junk-{}.plist", std::process::id()));
        std::fs::write(&path, b"not a plist").expect("write");
        assert!(platform::toc_from_plist(&path).is_none());
        let _ = std::fs::remove_file(&path);
        assert!(platform::toc_from_plist(Path::new("/nonexistent/x.plist")).is_none());
    }

    /// The TOC rules, independent of where the rows came from. Session
    /// markers (0xA0 and up) are not tracks, the order is by track number
    /// whatever order they arrive in, and a `Data` row is not audio.
    #[test]
    fn toc_points_keep_only_real_tracks_in_order() {
        let toc = toc_from_points(
            &[
                TocEntry { point: 0xA2, start: 124766, is_data: false },
                TocEntry { point: 2, start: 13834, is_data: false },
                TocEntry { point: 1, start: 150, is_data: false },
                TocEntry { point: 3, start: 90000, is_data: true },
            ],
            Some(124766),
        )
        .expect("toc");
        assert_eq!(
            toc.tracks.iter().map(|t| t.number).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "session markers dropped, tracks sorted"
        );
        assert!(!toc.tracks[2].is_audio, "a Data row is not audio");
        assert_eq!(toc.leadout_frame, 124766);
    }

    /// Captured from the real blank TDK CD-RW in the MATSHITA drive
    /// (`cdrskin dev=/dev/sr0 -minfo`), trimmed to the parsed region.
    const MINFO_BLANK_CDRW: &str = "\
Device type    : Removable CD-ROM
Vendor_info    : 'MATSHITA'
Supported modes: TAO SAO
ATIP info from disk:
  Is erasable
  ATIP start of lead in:  -12900 (97:10/00)
  ATIP start of lead out: 359849 (79:59/74)
Product Id:    97m10s00f/79m59s74f
Producer:      TDK / Ritek

Mounted media class:      CD
Mounted media type:       CD-RW
Disk Is erasable
disk status:              empty
session status:           empty
number of sessions:       1
";

    #[test]
    fn minfo_parses_blank_cdrw() {
        let m = parse_minfo(MINFO_BLANK_CDRW).unwrap();
        assert!(m.present);
        assert!(m.is_blank);
        assert!(m.rewritable);
        assert_eq!(m.kind, MediaKind::CdRw);
        assert_eq!(m.capacity_bytes, 359_849 * 2048);
        assert_eq!(m.free_bytes, m.capacity_bytes);
        // ≈ 79:57 of audio from the same figure.
        let d = OpticalDrive {
            supports_writing: true,
            id: "/dev/sr0".into(),
            label: "T".into(),
            media: m,
            toc: None,
            mount_path: None,
        };
        assert_eq!(crate::disc::burn::audio_capacity_secs(&d), 4797);
    }

    /// Captured from the same TDK CD-RW after a real audio burn in the
    /// Slimtype DS8A5SH (`cdrskin dev=/dev/sr0 -minfo`), trimmed.
    const MINFO_BURNED_CDRW: &str = "\
ATIP info from disk:
  Is erasable
  ATIP start of lead in:  -12900 (97:10/00)
  ATIP start of lead out: 359849 (79:59/74)
Product Id:    97m10s00f/79m59s74f
Producer:      TDK / Ritek

Mounted media class:      CD
Mounted media type:       CD-RW
Disk Is erasable
disk status:              complete
session status:           complete
number of sessions:       1
";

    /// A burned CD-RW reads back with a valid TOC, so the TOC path builds
    /// the MediaInfo — the `-minfo` typing must be merged in or the disc
    /// looks write-once-with-content and every erase/re-burn is refused
    /// (found live: first hardware burn, 2026-07-15).
    #[test]
    fn merged_typing_keeps_audio_cd_and_gains_rewritable() {
        let toc_media = MediaInfo {
            present: true,
            is_audio_cd: true,
            ..MediaInfo::none()
        };
        let m = merge_minfo_typing(toc_media, parse_minfo(MINFO_BURNED_CDRW).unwrap());
        assert!(m.present);
        assert!(m.is_audio_cd, "TOC's audio-CD verdict must survive the merge");
        assert!(!m.is_blank);
        assert!(m.rewritable, "burned CD-RW must still probe rewritable");
        assert_eq!(m.kind, MediaKind::CdRw);
        assert_eq!(m.capacity_bytes, 359_849 * 2048);
        assert_eq!(m.free_bytes, 0);
    }

    #[test]
    fn minfo_written_cdr_and_edge_cases() {
        let written = "\
Mounted media type:       CD-R
disk status:              complete
session status:           complete
";
        let m = parse_minfo(written).unwrap();
        assert!(!m.is_blank);
        assert!(!m.rewritable);
        assert_eq!(m.kind, MediaKind::CdR);
        assert_eq!(m.free_bytes, 0);

        assert!(parse_minfo("cdrskin: no disc\n").is_none());
    }

    /// A mounted data disc makes `cdrskin -minfo` fail with EBUSY, so the
    /// typing never merges. The TOC-derived defaults then read as "not blank,
    /// not rewritable" — which `erase_decision` can only call
    /// write-once-with-content and refuse, disabling both burn buttons on a
    /// perfectly writable CD-RW. `typing_unknown` is what lets a frontend
    /// tell that apart from a genuine CD-R (2026-08-10).
    #[test]
    fn untyped_media_is_flagged_not_silently_write_once() {
        // What probe_drive builds when minfo yields nothing but a TOC read.
        let toc_media = MediaInfo {
            present: true,
            is_audio_cd: false,
            ..MediaInfo::none()
        };
        let untyped = MediaInfo { typing_unknown: true, ..toc_media.clone() };
        assert!(!untyped.is_blank && !untyped.rewritable);
        assert!(untyped.typing_unknown, "the frontend needs this to explain itself");

        // A real write-once disc with content looks identical apart from the
        // flag, which is exactly why the flag has to exist.
        assert!(!toc_media.typing_unknown);

        // Merging real typing in clears nothing and sets nothing: a parsed
        // minfo is by definition known.
        let minfo = parse_minfo(MINFO_BURNED_CDRW).expect("sample parses");
        assert!(!minfo.typing_unknown);
        assert!(!merge_minfo_typing(toc_media, minfo).typing_unknown);
        let ram = parse_minfo("Mounted media type:       DVD-RAM\ndisk status: empty\n").unwrap();
        assert_eq!(ram.kind, MediaKind::DvdRam);
    }

    #[test]
    fn minfo_dvd_gets_default_capacity_without_atip() {
        // DVDs carry no ATIP lead-out ("No reliable track size"), so the
        // capacity must fall back to the standard single-layer size — else
        // the over-capacity gate is silently disabled on DVD media.
        let blank = "Mounted media type:       DVD+RW\ndisk status: empty\n";
        let m = parse_minfo(blank).unwrap();
        assert_eq!(m.kind, MediaKind::DvdRw);
        assert_eq!(m.capacity_bytes, 4_700_000_000);
        assert_eq!(m.free_bytes, 4_700_000_000, "blank DVD's free == capacity");

        let full = "Mounted media type:       DVD+RW\ndisk status: complete\n";
        let f = parse_minfo(full).unwrap();
        assert_eq!(f.capacity_bytes, 4_700_000_000);
        assert_eq!(f.free_bytes, 0, "non-blank overwrite media reports 0 free");

        // A CD with a real ATIP lead-out still uses the measured value, not
        // the DVD default.
        let cd = "Mounted media type:       CD-RW\n  ATIP start of lead out: 359849\ndisk status: empty\n";
        assert_eq!(parse_minfo(cd).unwrap().capacity_bytes, 359_849 * 2048);
    }

    #[test]
    fn toc_from_entries_adds_pregap_and_audio_flag() {
        // Track 1 audio at LBA 0, track 2 data (ctrl bit 0x04) at LBA 7500.
        let toc = toc_from_entries(&[(1, 0x0, 0), (2, 0x4, 7500)], 15000).unwrap();
        assert_eq!(toc.tracks.len(), 2);
        assert_eq!(toc.tracks[0].start_frame, 150);
        assert!(toc.tracks[0].is_audio);
        assert_eq!(toc.tracks[1].start_frame, 7650);
        assert!(!toc.tracks[1].is_audio);
        assert_eq!(toc.leadout_frame, 15150);

        assert!(toc_from_entries(&[], 15000).is_none());
        assert!(toc_from_entries(&[(1, 0, 0)], 0).is_none());
        // Negative LBAs (ioctl quirk) are dropped, not wrapped.
        assert!(toc_from_entries(&[(1, 0, -1)], 15000).is_none());
    }

    /// A real 15-track audio CD's format-0 answer, captured off the Slimtype
    /// DS8A5SH this project tests against. Guards two things a synthetic
    /// buffer would not: the header trim, and the nibble the data bit lives
    /// in.
    #[test]
    fn parse_mmc_toc_reads_a_real_disc() {
        #[rustfmt::skip]
        let raw: [u8; 132] = [
            0x00, 0x82, 0x01, 0x0f, 0x00, 0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x10, 0x02, 0x00, 0x00, 0x00, 0x3c, 0xef, 0x00, 0x10, 0x03, 0x00,
            0x00, 0x00, 0x6a, 0x9f, 0x00, 0x10, 0x04, 0x00, 0x00, 0x00, 0x9b, 0x3f,
            0x00, 0x10, 0x05, 0x00, 0x00, 0x00, 0xce, 0x96, 0x00, 0x10, 0x06, 0x00,
            0x00, 0x01, 0x06, 0x88, 0x00, 0x10, 0x07, 0x00, 0x00, 0x01, 0x50, 0x8c,
            0x00, 0x10, 0x08, 0x00, 0x00, 0x01, 0x90, 0x49, 0x00, 0x10, 0x09, 0x00,
            0x00, 0x01, 0xc6, 0xde, 0x00, 0x10, 0x0a, 0x00, 0x00, 0x01, 0xfd, 0x1b,
            0x00, 0x10, 0x0b, 0x00, 0x00, 0x02, 0x3f, 0xdd, 0x00, 0x10, 0x0c, 0x00,
            0x00, 0x02, 0x82, 0x54, 0x00, 0x10, 0x0d, 0x00, 0x00, 0x02, 0xa9, 0x72,
            0x00, 0x10, 0x0e, 0x00, 0x00, 0x02, 0xf7, 0xf6, 0x00, 0x10, 0x0f, 0x00,
            0x00, 0x03, 0x2d, 0xfa, 0x00, 0x10, 0xaa, 0x00, 0x00, 0x03, 0x65, 0x49,
        ];

        let (entries, leadout) = parse_mmc_toc(&raw).unwrap();
        assert_eq!(entries.len(), 15, "lead-out must not be counted as a track");
        assert_eq!(entries[0], (1, 0, 0));
        assert_eq!(entries[1], (2, 0, 15599));
        assert_eq!(entries[14], (15, 0, 208378));
        assert_eq!(leadout, 222537);

        // The +150 conversion is what makes this agree with the `.TOC.plist`
        // the same disc mounts, which reports the lead-out as 222687. gnudb
        // disc IDs are computed off these frames, so an off-by-150 here would
        // silently look up the wrong album.
        let toc = toc_from_entries(&entries, leadout).unwrap();
        assert!(toc.tracks.iter().all(|t| t.is_audio));
        assert_eq!(toc.tracks[0].start_frame, 150);
        assert_eq!(toc.leadout_frame, 222687);
    }

    /// An answer with no lead-out is not "a disc with no tracks". It is an
    /// unusable TOC, and saying so lets the caller fall back.
    #[test]
    fn parse_mmc_toc_rejects_unusable_answers() {
        assert!(parse_mmc_toc(&[]).is_none());
        assert!(parse_mmc_toc(&[0x00, 0x02, 0x01, 0x0f]).is_none());
        // A data track's 0x04 sits in the low nibble: 0x14 is ADR 1, data.
        let mixed = [
            0x00, 0x1a, 0x01, 0x02, //
            0x00, 0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, //
            0x00, 0x14, 0x02, 0x00, 0x00, 0x00, 0x1d, 0x4c, //
            0x00, 0x10, 0xaa, 0x00, 0x00, 0x00, 0x3a, 0x98, //
        ];
        let (e, _) = parse_mmc_toc(&mixed).unwrap();
        assert_eq!(e[0].1 & 0x04, 0, "track 1 is audio");
        assert_eq!(e[1].1 & 0x04, 0x04, "track 2 is data");
    }

    /// Live: the ioctl TOC must match what cd-info parses (same disc), and
    /// must answer fast. `cargo test --lib live_ioctl_toc -- --ignored`.
    #[test]
    #[ignore]
    #[cfg(target_os = "linux")]
    fn live_ioctl_toc_matches_cd_info() {
        let started = std::time::Instant::now();
        let drives = list_drives();
        let elapsed = started.elapsed();
        let Some(d) = drives.iter().find(|d| d.media.present) else {
            println!("no disc loaded — skipping");
            return;
        };
        let toc = d.toc.as_ref().expect("loaded disc has a TOC");
        println!(
            "ioctl probe: {} tracks, discid {}, total {:.2?}",
            toc.tracks.len(),
            crate::disc::discid::freedb_discid(toc),
            elapsed
        );
        let cd_info = run("cd-info", &["--no-header", &d.id])
            .and_then(|o| parse_cd_info(&o))
            .expect("cd-info parses the same disc");
        assert_eq!(toc, &cd_info, "ioctl TOC must equal cd-info's");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn exclusive_read_freezes_polling() {
        let _guard = exclusive_read_test_guard();
        let fake = vec![OpticalDrive {
            supports_writing: true,
            id: "/dev/sr-test".into(),
            label: "FAKE".into(),
            media: MediaInfo::none(),
            toc: None,
            mount_path: None,
        }];
        begin_exclusive_read();
        let out = list_drives_cached(&fake);
        end_exclusive_read();
        // While a streaming read owns the drive, polling must echo the
        // previous state untouched — no device access, no re-enumeration.
        assert_eq!(out, fake);
    }

    #[test]
    fn probe_action_matrix() {
        const NO_DISC: i32 = 1;
        const TRAY_OPEN: i32 = 2;
        // A loaded, unchanged disc from last poll: reuse, never spin.
        assert_eq!(probe_action(CDS_DISC_OK, false, Some(true)), ProbeAction::Reuse);
        // Media-changed flag set (disc swapped between polls): re-probe.
        assert_eq!(probe_action(CDS_DISC_OK, true, Some(true)), ProbeAction::Probe);
        // Disc newly inserted (previous poll saw the drive empty): probe.
        assert_eq!(probe_action(CDS_DISC_OK, false, Some(false)), ProbeAction::Probe);
        // First sighting of the drive (no history): probe.
        assert_eq!(probe_action(CDS_DISC_OK, false, None), ProbeAction::Probe);
        assert_eq!(probe_action(CDS_DISC_OK, true, None), ProbeAction::Probe);
        // No readable disc: empty entry, regardless of history/changed flag.
        assert_eq!(probe_action(NO_DISC, true, Some(true)), ProbeAction::Empty);
        assert_eq!(probe_action(TRAY_OPEN, false, Some(true)), ProbeAction::Empty);
        assert_eq!(probe_action(0, false, None), ProbeAction::Empty);
    }

    /// Live check of the no-spin poll path: a full probe, then a cached
    /// poll that must return the same drives near-instantly (no cd-info).
    /// `cargo test --lib live_cached_poll -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_cached_poll() {
        let first = list_drives();
        println!("full probe: {} drive(s)", first.len());

        // The first cached call after a raw `list_drives` always probes: only
        // `list_drives_cached` records the devfs fingerprint, so there is
        // nothing yet to compare against. Timing it measured the probe, which
        // passed on a fast internal drive and failed on a USB one at 616 ms.
        let warm = list_drives_cached(&first);
        assert_eq!(first, warm, "cached poll must mirror the probe");

        let started = std::time::Instant::now();
        let second = list_drives_cached(&warm);
        let elapsed = started.elapsed();
        println!("cached poll took {elapsed:.2?}");
        assert_eq!(warm, second, "a warm cached poll must still mirror it");
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "cached poll looks like it ran a full probe ({elapsed:?})"
        );
    }

    #[test]
    fn media_fingerprint_tracks_meaningful_changes() {
        let mut d = OpticalDrive {
            supports_writing: true,
            id: "/dev/sr0".into(), label: "T".into(),
            media: MediaInfo::none(), toc: None, mount_path: None,
        };
        let empty = media_fingerprint(&d);
        d.media.present = true;
        d.media.kind = MediaKind::CdRw;
        let blank = media_fingerprint(&d);
        assert_ne!(empty, blank, "media arriving must change the fingerprint");
        let same = media_fingerprint(&d);
        assert_eq!(blank, same, "unchanged media must be stable");
        d.media.is_blank = true;
        assert_ne!(media_fingerprint(&d), blank, "blank flag change must show");
        d.media.capacity_bytes = 700_000_000;
        let with_cap = media_fingerprint(&d);
        d.media.capacity_bytes = 4_700_000_000;
        assert_ne!(media_fingerprint(&d), with_cap, "capacity change must show");
    }

    /// No tracks is not a TOC, and neither is tracks with no lead-out to
    /// bound the last one.
    #[test]
    fn toc_points_rejects_an_incomplete_toc() {
        assert!(toc_from_points(&[], Some(124766)).is_none());
        assert!(
            toc_from_points(&[TocEntry { point: 1, start: 150, is_data: false }], None).is_none()
        );
        // Session markers alone are not tracks either.
        assert!(
            toc_from_points(
                &[TocEntry { point: 0xA0, start: 0, is_data: false }],
                Some(124766)
            )
            .is_none()
        );
    }




    /// The mount table as `getfsstat` hands it over: `f_mntfromname` and
    /// `f_mntonname`, already separate fields.
    fn mounts(rows: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
        rows.to_vec()
    }

    #[test]
    fn mount_lookup_finds_matching_slice() {
        let table = mounts(&[
            ("/dev/disk1s1", "/"),
            ("/dev/disk13s1", "/Volumes/MY_DATA_CD"),
            ("/dev/disk2s1", "/Volumes/Other"),
        ]);
        assert_eq!(
            mount_for_device(table, "/dev/disk13"),
            Some(PathBuf::from("/Volumes/MY_DATA_CD"))
        );
    }

    /// A disc written as one plain filesystem image (no partition scheme)
    /// mounts the whole device with no slice suffix. Taken from a DVD+RW
    /// burned from an ISO, which is the case that reported no files at all.
    #[test]
    fn mount_lookup_finds_whole_disk_mount() {
        let table = mounts(&[
            ("/dev/disk3s5", "/System/Volumes/Data"),
            ("/dev/disk12", "/Volumes/ISOIMAGE"),
        ]);
        assert_eq!(
            mount_for_device(table, "/dev/disk12"),
            Some(PathBuf::from("/Volumes/ISOIMAGE"))
        );
    }

    /// A volume name containing " (" used to be able to confuse the line
    /// split; the kernel hands the mount point over as its own field, so it
    /// cannot any more. Pinned because the risk is what motivated the change.
    #[test]
    fn mount_lookup_keeps_punctuation_in_volume_names() {
        let table = mounts(&[("/dev/disk13s1", "/Volumes/My Burned Disc (2026)")]);
        assert_eq!(
            mount_for_device(table, "/dev/disk13"),
            Some(PathBuf::from("/Volumes/My Burned Disc (2026)"))
        );
    }

    /// The whole-disk match must not loosen the numeric-prefix guard: disk130
    /// is a different disk from disk13, sliced or not.
    #[test]
    fn mount_lookup_does_not_match_a_longer_number() {
        assert_eq!(
            mount_for_device(mounts(&[("/dev/disk130", "/Volumes/Unrelated")]), "/dev/disk13"),
            None
        );
        assert_eq!(
            mount_for_device(mounts(&[("/dev/disk130s1", "/Volumes/Unrelated")]), "/dev/disk13"),
            None
        );
    }

    #[test]
    fn mount_lookup_no_match_returns_none() {
        assert_eq!(mount_for_device(mounts(&[("/dev/disk1s1", "/")]), "/dev/disk13"), None);
        // A matching device with an empty mount point is not a mount.
        assert_eq!(mount_for_device(mounts(&[("/dev/disk13s1", "")]), "/dev/disk13"), None);
    }

    /// LIVE: with a data disc loaded, the detected drive must carry a mount
    /// path and that path must list files. Both halves of the browse chain the
    /// disc view depends on, which unit tests can only cover as text.
    /// `cargo test --lib live_data_disc_browse -- --ignored --nocapture`.
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn live_data_disc_browse() {
        // The per-file tag read falls back to GStreamer's Discoverer, which
        // panics without init — same reason mount.rs's live test does this.
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().ok();
        let drives = list_drives();
        let Some(d) = drives.iter().find(|d| d.media.present && !d.media.is_audio_cd) else {
            println!("no data disc loaded — skipping");
            return;
        };
        println!("drive {} ({}): {}", d.id, d.label, d.media_summary());
        let mount = d.mount_path.as_ref().expect("a mounted data disc has a mount path");
        println!("mounted at {}", mount.display());
        let files = crate::disc::mount::list_disc_files(mount);
        println!("{} file(s)", files.len());
        for f in files.iter().take(10) {
            println!("  {}  ({} bytes)", f.display, f.bytes);
        }
        assert!(!files.is_empty(), "a data disc with content must list files");
    }




    #[test]
    fn cd_info_parses_tracks_and_adds_pregap() {
        let out = "\
CD-ROM Track List (1 - 8)\n\
  #: MSF       LSN    Type   Green? Copy? Channels Premphasis?\n\
  1: 00:02:00  000000 audio  false  no    2        no\n\
  2: 03:04:34  013684 audio  false  no    2        no\n\
170: 27:43:41  124616 leadout\n";
        let toc = parse_cd_info(out).expect("toc");
        assert_eq!(toc.tracks.len(), 2);
        assert_eq!(toc.tracks[0].start_frame, 150); // 0 + 150
        assert_eq!(toc.tracks[1].start_frame, 13834); // 13684 + 150
        assert_eq!(toc.leadout_frame, 124766); // 124616 + 150
        assert!(toc.tracks[0].is_audio);
    }

    #[test]
    fn cd_info_no_disc_is_none() {
        assert!(parse_cd_info("++ WARN: error in ioctl: No medium found\n").is_none());
    }

    /// Manual live probe of the machine's real drives — run with
    /// `cargo test --lib live_list_drives -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_list_drives() {
        for d in list_drives() {
            println!("{} [{}] — {}", d.label, d.id, d.media_summary());
            println!("  media: {:?}", d.media);
            if let Some(m) = &d.mount_path {
                println!("  mount: {}", m.display());
            }
            if let Some(t) = &d.toc {
                for e in crate::disc::toc::track_entries(&d) {
                    println!(
                        "  {:2}. {} ({} s) -> {}",
                        e.number, e.title, e.duration_secs, e.path
                    );
                }
                println!("  leadout: {}", t.leadout_frame);
            }
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod write_capability_cache_tests {
    use super::platform::write_capability_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_drive_is_asked_once_and_remembered() {
        let calls = AtomicUsize::new(0);
        let probe = |_: &str| {
            calls.fetch_add(1, Ordering::Relaxed);
            Some(false)
        };
        // A unique node per test run: the cache is process-wide by design.
        let node = "/dev/sr-cache-test-a";
        assert!(!write_capability_cached(node, &probe));
        assert!(!write_capability_cached(node, &probe));
        assert!(!write_capability_cached(node, &probe));
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "the poll runs every 2 s; asking udisks each time is what kept the \
             drive busy and made an audio CD type as data"
        );
    }

    #[test]
    fn an_unreachable_daemon_is_not_cached_so_a_later_poll_can_learn() {
        let calls = AtomicUsize::new(0);
        let probe = |_: &str| -> Option<bool> {
            calls.fetch_add(1, Ordering::Relaxed);
            None
        };
        let node = "/dev/sr-cache-test-b";
        assert!(write_capability_cached(node, &probe), "falls back to writable");
        assert!(write_capability_cached(node, &probe));
        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "a miss must be retried, not pinned for the session"
        );
    }
}
