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

use std::path::PathBuf;

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
/// any medium access. On macOS this is [`list_drives`] (drutil's status
/// query doesn't spin the disc).
pub fn list_drives_cached(prev: &[OpticalDrive]) -> Vec<OpticalDrive> {
    #[cfg(target_os = "linux")]
    {
        platform::list_drives_cached(prev)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = prev;
        platform::list_drives()
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
    drives
}

/// Drop the shared snapshot so the next poll re-probes. Needed after WE
/// change the medium (burn/erase finished): the kernel's media-changed flag
/// doesn't fire for our own writes, so the cache would keep reporting the
/// pre-burn state.
pub fn invalidate_shared_cache() {
    SHARED.lock().unwrap().clear();
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
pub fn begin_exclusive_read() {
    EXCLUSIVE_READ_DEPTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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

/// Serializes every test that touches the process-wide exclusive-read
/// depth — cargo's parallel runner would otherwise interleave them.
#[cfg(test)]
static EXCLUSIVE_READ_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod exclusive_read_tests {
    use super::*;

    // These share process-global state (`EXCLUSIVE_READ_DEPTH`), so they run
    // as one test to avoid interleaving with any other test that touches the
    // guard (cargo runs `#[test]`s concurrently by default).
    #[test]
    fn refcount_nesting_and_underflow() {
        let _guard = EXCLUSIVE_READ_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        assert!(!exclusive_read(), "must start clear");

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
// macOS `.TOC.plist` (shared parser — the plist is what macOS mounts, but the
// parser itself is platform-neutral text handling)
// ---------------------------------------------------------------------------

/// Parse the XML form of a mounted audio CD's `.TOC.plist` (as produced by
/// `plutil -convert xml1 -o -`).
///
/// Strategy: a sequential scan tracking the last seen `<key>`; any `</dict>`
/// that closed a dict containing both "Point" and "Start Block" was a track.
/// The session dict itself has neither, so nesting needn't be modelled.
/// "Leadout Block" is taken first-wins, i.e. from session 1 — right for audio
/// CDs (and for CD-Extra, whose audio session is first; the data session's
/// tracks are dropped by the `is_audio` filter downstream).
// macOS-only (called from the `drutil`/plist detector); exercised by the
// cross-platform tests below, so kept compiled everywhere but allowed-dead off macOS.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn parse_toc_plist(xml: &str) -> Option<DiscToc> {
    let mut last_key = String::new();
    let mut cur_point: Option<u32> = None;
    let mut cur_start: Option<u32> = None;
    let mut cur_is_data = false;
    let mut leadout: Option<u32> = None;
    let mut tracks: Vec<TocTrack> = Vec::new();

    for raw in xml.lines() {
        let line = raw.trim();
        if let Some(k) = line
            .strip_prefix("<key>")
            .and_then(|r| r.strip_suffix("</key>"))
        {
            last_key = k.to_string();
        } else if let Some(v) = line
            .strip_prefix("<integer>")
            .and_then(|r| r.strip_suffix("</integer>"))
        {
            let v: u32 = v.parse().ok()?;
            match last_key.as_str() {
                "Point" => cur_point = Some(v),
                "Start Block" => cur_start = Some(v),
                "Leadout Block" => leadout = leadout.or(Some(v)),
                _ => {}
            }
        } else if line == "<true/>" {
            if last_key == "Data" {
                cur_is_data = true;
            }
        } else if line == "<false/>" {
            if last_key == "Data" {
                cur_is_data = false;
            }
        } else if line == "</dict>" {
            if let (Some(p), Some(s)) = (cur_point, cur_start) {
                // Points 1–99 are real tracks (0xA0+ session markers never
                // appear in the Track Array, but stay defensive).
                if (1..=99).contains(&p) {
                    tracks.push(TocTrack {
                        number: p as u8,
                        start_frame: s,
                        is_audio: !cur_is_data,
                    });
                }
            }
            cur_point = None;
            cur_start = None;
            cur_is_data = false;
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
// macOS `drutil` output parsers (platform-neutral text handling)
// ---------------------------------------------------------------------------

/// One row of `drutil list`: the drive index drutil uses for `-drive N`, and
/// the human label (vendor + product).
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct DrutilDriveRow {
    pub index: u32,
    pub label: String,
}

/// Parse `drutil list`. Column positions come from the header line, so the
/// label is sliced between "Vendor" and "Rev" no matter how wide the fields
/// print:
/// ```text
///    Vendor   Product           Rev   Bus       SupportLevel
/// 1  MATSHITA DVD-RAM UJ8C2     1.00  USB       Unsupported
/// ```
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn parse_drutil_list(out: &str) -> Vec<DrutilDriveRow> {
    let Some(header) = out.lines().find(|l| l.contains("Vendor")) else {
        return Vec::new();
    };
    let Some(vendor_col) = header.find("Vendor") else {
        return Vec::new();
    };
    let rev_col = header.find("Rev").unwrap_or(header.len());

    out.lines()
        .filter_map(|line| {
            let index: u32 = line.split_whitespace().next()?.parse().ok()?;
            let end = rev_col.min(line.len());
            let start = vendor_col.min(end);
            let label = line.get(start..end)?.trim().to_string();
            if label.is_empty() {
                return None;
            }
            Some(DrutilDriveRow { index, label })
        })
        .collect()
}

/// Media facts pulled from `drutil status -drive N`.
#[derive(Debug, Default, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct DrutilStatus {
    /// The "Type:" value ("CD-ROM", "CD-R", "DVD-RAM", "No Media Inserted"…).
    pub media_type: String,
    /// "Tracks:" value when media is present.
    pub tracks: Option<u32>,
    /// "Space Used:" blocks value.
    pub used_blocks: Option<u64>,
    /// "Space Free:" blocks value.
    pub free_blocks: Option<u64>,
    /// Whole "Writability:" line value (tokens like "appendable, blank…").
    pub writability: String,
    /// The whole-disk BSD device node from the "Type:" line's "Name:" field
    /// (e.g. `/dev/disk13`) — a data disc's mounted slice (`/dev/disk13s1`)
    /// shares this prefix, which is how [`data_disc_mount_path`] finds the
    /// mount `drutil` itself never reports.
    pub device_node: Option<String>,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn parse_drutil_status(out: &str) -> DrutilStatus {
    let mut st = DrutilStatus::default();
    for raw in out.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("Type:") {
            // "Type: CD-ROM   Name: /dev/disk13" — split off the device node.
            match rest.split_once("Name:") {
                Some((ty, name)) => {
                    st.media_type = ty.trim().to_string();
                    let name = name.trim();
                    if !name.is_empty() {
                        st.device_node = Some(name.to_string());
                    }
                }
                None => st.media_type = rest.trim().to_string(),
            }
            // "Sessions: 1  Tracks: 8" shares a line in some layouts; Tracks
            // is parsed generically below either way.
        }
        if let Some(pos) = line.find("Tracks:") {
            st.tracks = line[pos + "Tracks:".len()..]
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok());
        }
        for (prefix, slot) in [
            ("Space Free:", &mut st.free_blocks),
            ("Space Used:", &mut st.used_blocks),
        ] {
            if let Some(rest) = line.strip_prefix(prefix) {
                // "…  27:41:41         blocks:   124616 / 255.21MB / …"
                if let Some(bpos) = rest.find("blocks:") {
                    *slot = rest[bpos + "blocks:".len()..]
                        .split_whitespace()
                        .next()
                        .and_then(|v| v.parse().ok());
                }
            }
        }
        if let Some(rest) = line.strip_prefix("Writability:") {
            st.writability = rest.trim().to_string();
        }
    }
    st
}

/// Map a `drutil` media type + writability into [`MediaInfo`]. Order matters:
/// "CD-ROM" must not match the "CD-R" prefix.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn media_from_drutil(st: &DrutilStatus) -> MediaInfo {
    let ty = st.media_type.as_str();
    let present = !ty.is_empty() && !ty.contains("No Media");
    if !present {
        return MediaInfo::none();
    }
    let kind = if ty.contains("CD-ROM") {
        MediaKind::Unknown // pressed disc
    } else if ty.contains("CD-RW") {
        MediaKind::CdRw
    } else if ty.contains("CD-R") {
        MediaKind::CdR
    } else if ty.contains("DVD-RAM") {
        MediaKind::DvdRam
    } else if ty.contains("DVD-RW") || ty.contains("DVD+RW") {
        MediaKind::DvdRw
    } else if ty.contains("DVD-R") || ty.contains("DVD+R") {
        MediaKind::DvdR
    } else {
        MediaKind::Unknown
    };
    let is_blank =
        st.writability.contains("blank") || (st.used_blocks == Some(0) && st.free_blocks.unwrap_or(0) > 0);
    let rewritable = matches!(kind, MediaKind::CdRw | MediaKind::DvdRw | MediaKind::DvdRam)
        || st.writability.contains("overwritable")
        || st.writability.contains("erasable");
    // 2048-byte data blocks — close enough for capacity display; the burn
    // phases refine per-media accounting.
    MediaInfo {
        present,
        is_audio_cd: false, // decided by TOC-volume matching, not drutil
        is_blank,
        rewritable,
        kind,
        free_bytes: st.free_blocks.unwrap_or(0) * 2048,
        capacity_bytes: (st.free_blocks.unwrap_or(0) + st.used_blocks.unwrap_or(0)) * 2048,
    }
}

/// Find a data disc's mount point in BSD `mount`(8) output by matching a
/// device slice against the drive's whole-disk node (`drutil status`'s
/// "Name:", e.g. `/dev/disk13` — a slice mounts as `/dev/disk13s1`,
/// `/dev/disk13s2`, …). `drutil` itself never reports a mount path, so this
/// is Task 11's fill-in: macOS auto-mounts data discs the kernel already
/// knows about (unlike audio CDs, `list_drives`'s existing `.TOC.plist` walk
/// of `/Volumes` doesn't apply — a data disc's ISO9660/UDF volume carries no
/// such marker file). One line of `mount` output looks like:
/// ```text
/// /dev/disk13s1 on /Volumes/MY_DATA_CD (cd9660, local, nodev, nosuid, read-only, noowners)
/// ```
/// Returns the first slice of `device_node` found mounted; `None` when no
/// line matches (not yet auto-mounted, or the kernel is still probing it —
/// callers already only reach this after `media.present` is true, so a miss
/// here is surfaced as "no data-disc browsing" rather than retried).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn parse_mount_output(out: &str, device_node: &str) -> Option<PathBuf> {
    let slice_prefix = format!("{device_node}s");
    out.lines().find_map(|line| {
        let dev = line.split_whitespace().next()?;
        if !dev.starts_with(&slice_prefix) {
            return None;
        }
        let rest = line.strip_prefix(dev)?.trim_start();
        let rest = rest.strip_prefix("on ")?;
        // The mount point runs up to " (<options>)"; paths can contain
        // spaces (macOS volume names commonly do), so split on the LAST
        // " (" rather than the first space.
        let mount = rest.rsplit_once(" (").map(|(m, _)| m).unwrap_or(rest);
        if mount.is_empty() {
            None
        } else {
            Some(PathBuf::from(mount))
        }
    })
}

/// Run `mount` and resolve `device_node`'s data-disc mount path via
/// [`parse_mount_output`]. Subprocess IO — only ever called from the same
/// background-queue context every other drutil probe already runs on.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn data_disc_mount_path(device_node: &str) -> Option<PathBuf> {
    let out = run("mount", &[])?;
    parse_mount_output(&out, device_node)
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

#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
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
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn toc_from_entries(entries: &[(u8, u8, i32)], leadout_lba: i32) -> Option<DiscToc> {
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
/// device node (`eject /dev/srX`), macOS the drutil index
/// (`drutil eject -drive N`). The caller must not be reading the drive
/// (playback/rip) — the OS refuses to eject a busy device.
pub fn eject(drive_id: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let (cmd, args): (&str, Vec<&str>) = ("drutil", vec!["eject", "-drive", drive_id]);
    #[cfg(target_os = "linux")]
    let (cmd, args): (&str, Vec<&str>) = ("eject", vec![drive_id]);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err(format!("eject not supported on this platform ({drive_id})"));

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let out = std::process::Command::new(cmd)
            .args(&args)
            .output()
            .map_err(|e| format!("couldn't run {cmd}: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            let err = err.trim();
            Err(if err.is_empty() {
                format!("{cmd} failed ({})", out.status)
            } else {
                err.to_string()
            })
        }
    }
}

// ---------------------------------------------------------------------------
// macOS platform glue
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::path::{Path, PathBuf};

    pub fn list_drives() -> Vec<OpticalDrive> {
        let rows = run("drutil", &["list"])
            .map(|o| parse_drutil_list(&o))
            .unwrap_or_default();
        if rows.is_empty() {
            return Vec::new();
        }

        // Mounted audio-CD volumes (a `.TOC.plist` marks one), parsed once
        // and claimed by matching drives below.
        let mut volumes = audio_volumes();

        rows.into_iter()
            .map(|row| {
                let status = run("drutil", &["status", "-drive", &row.index.to_string()])
                    .map(|o| parse_drutil_status(&o))
                    .unwrap_or_default();
                let mut media = media_from_drutil(&status);

                // Claim the mounted volume whose TOC matches this drive's
                // media (track count, then used-block sanity), or the first
                // unclaimed one as a fallback — exact per-drive attribution
                // only matters with two audio CDs in at once.
                let mut toc = None;
                let mut mount_path = None;
                if media.present {
                    let claim = volumes
                        .iter()
                        .position(|(_, t)| {
                            status.tracks == Some(t.tracks.len() as u32)
                                && status
                                    .used_blocks
                                    .map(|u| u == (t.leadout_frame as u64).saturating_sub(150))
                                    .unwrap_or(true)
                        })
                        .or(if volumes.is_empty() { None } else { Some(0) });
                    if let Some(i) = claim {
                        let (path, parsed) = volumes.remove(i);
                        media.is_audio_cd = parsed.tracks.iter().any(|t| t.is_audio);
                        toc = Some(parsed);
                        mount_path = Some(path);
                    } else if let Some(node) = &status.device_node {
                        // Not an audio CD (no `.TOC.plist` volume claimed it)
                        // but present: a data disc, which macOS auto-mounts
                        // without any `.TOC.plist` marker — resolve its
                        // mount point from `mount`(8) so the data-disc
                        // browse/import FFI (`sparkamp_disc_mount_list`) has
                        // somewhere to read (Task 11).
                        mount_path = data_disc_mount_path(node);
                    }
                }

                OpticalDrive {
                    id: row.index.to_string(),
                    label: row.label,
                    media,
                    toc,
                    mount_path,
                }
            })
            .collect()
    }

    /// Every mounted volume containing a `.TOC.plist` (audio CDs), with its
    /// parsed TOC.
    fn audio_volumes() -> Vec<(PathBuf, DiscToc)> {
        std::fs::read_dir("/Volumes")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let vol = e.path();
                let plist = vol.join(".TOC.plist");
                if !plist.exists() {
                    return None;
                }
                let toc = toc_from_plist(&plist)?;
                Some((vol, toc))
            })
            .collect()
    }

    fn toc_from_plist(plist: &Path) -> Option<DiscToc> {
        // The plist is binary and contains a raw <data> blob, so JSON
        // conversion fails; XML always works and the parser scans it.
        let xml = run(
            "plutil",
            &["-convert", "xml1", "-o", "-", &plist.display().to_string()],
        )?;
        parse_toc_plist(&xml)
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
                    .map(|m| super::merge_minfo_typing(toc_media.clone(), m))
                    .unwrap_or(toc_media)
            }
            // No readable TOC but the status ioctl said "disc ok" (the
            // caller only probes then): blank / just-erased media — type it
            // via cdrskin -minfo (kind, capacity, blank/rewritable) for the
            // burn phases.
            None => run("cdrskin", &[&format!("dev={node}"), "-minfo"])
                .and_then(|o| super::parse_minfo(&o))
                .unwrap_or_else(MediaInfo::none),
        };
        OpticalDrive {
            id: node,
            label,
            media,
            toc,
            mount_path: None,
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

    #[test]
    fn toc_plist_parses_tracks_leadout_and_data_flag() {
        let toc = parse_toc_plist(TOC_XML).expect("toc");
        assert_eq!(toc.leadout_frame, 124766);
        assert_eq!(toc.tracks.len(), 3);
        assert_eq!(toc.tracks[0].number, 1);
        assert_eq!(toc.tracks[0].start_frame, 150); // already CDDB-absolute
        assert!(toc.tracks[0].is_audio);
        assert_eq!(toc.tracks[1].start_frame, 13834);
        assert!(!toc.tracks[2].is_audio); // Data=true track
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
        let _guard = EXCLUSIVE_READ_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let fake = vec![OpticalDrive {
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
        let started = std::time::Instant::now();
        let second = list_drives_cached(&first);
        let elapsed = started.elapsed();
        println!("cached poll took {elapsed:.2?}");
        assert_eq!(first, second, "cached poll must mirror the probe");
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "cached poll looks like it ran a full probe ({elapsed:?})"
        );
    }

    #[test]
    fn media_fingerprint_tracks_meaningful_changes() {
        let mut d = OpticalDrive {
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

    #[test]
    fn toc_plist_rejects_empty() {
        assert!(parse_toc_plist("<plist></plist>").is_none());
    }

    #[test]
    fn drutil_list_slices_label_by_header_columns() {
        let out = "   Vendor   Product           Rev   Bus       SupportLevel\n\
                   1  MATSHITA DVD-RAM UJ8C2     1.00  USB       Unsupported\n";
        let rows = parse_drutil_list(out);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].index, 1);
        assert_eq!(rows[0].label, "MATSHITA DVD-RAM UJ8C2");
    }

    #[test]
    fn drutil_status_parses_audio_cd() {
        let out = "\
 Vendor   Product           Rev \n\
 MATSHITA DVD-RAM UJ8C2     1.00\n\
\n\
           Type: CD-ROM               Name: /dev/disk13\n\
       Sessions: 1                  Tracks: 8 \n\
   Overwritable:   00:00:00         blocks:        0 /   0.00MB /   0.00MiB\n\
     Space Free:   00:00:00         blocks:        0 /   0.00MB /   0.00MiB\n\
     Space Used:   27:41:41         blocks:   124616 / 255.21MB / 243.39MiB\n\
    Writability: \n";
        let st = parse_drutil_status(out);
        assert_eq!(st.media_type, "CD-ROM");
        assert_eq!(st.tracks, Some(8));
        assert_eq!(st.used_blocks, Some(124616));
        assert_eq!(st.free_blocks, Some(0));
        assert_eq!(st.writability, "");

        let media = media_from_drutil(&st);
        assert!(media.present);
        assert!(!media.is_blank);
        assert!(!media.rewritable);
        assert_eq!(media.kind, MediaKind::Unknown); // pressed CD-ROM
    }

    #[test]
    fn drutil_status_captures_device_node() {
        let out = "           Type: CD-ROM               Name: /dev/disk13\n";
        let st = parse_drutil_status(out);
        assert_eq!(st.media_type, "CD-ROM");
        assert_eq!(st.device_node.as_deref(), Some("/dev/disk13"));
    }

    #[test]
    fn mount_output_finds_matching_slice() {
        let out = "\
/dev/disk1s1 on / (apfs, local, journaled)\n\
/dev/disk13s1 on /Volumes/MY_DATA_CD (cd9660, local, nodev, nosuid, read-only, noowners)\n\
/dev/disk2s1 on /Volumes/Other (msdos, local, nodev, nosuid)\n";
        assert_eq!(
            parse_mount_output(out, "/dev/disk13"),
            Some(PathBuf::from("/Volumes/MY_DATA_CD"))
        );
    }

    #[test]
    fn mount_output_handles_spaces_in_volume_name() {
        let out = "/dev/disk13s1 on /Volumes/My Burned Disc (cd9660, local, nodev, nosuid, read-only, noowners)\n";
        assert_eq!(
            parse_mount_output(out, "/dev/disk13"),
            Some(PathBuf::from("/Volumes/My Burned Disc"))
        );
    }

    #[test]
    fn mount_output_no_match_returns_none() {
        let out = "/dev/disk1s1 on / (apfs, local, journaled)\n";
        assert_eq!(parse_mount_output(out, "/dev/disk13"), None);
        // A different disk sharing a numeric prefix (13 vs 130) must not
        // match — the "s" separator check keeps `/dev/disk13` from
        // accidentally matching `/dev/disk130s1`.
        let out2 = "/dev/disk130s1 on /Volumes/Unrelated (cd9660, local)\n";
        assert_eq!(parse_mount_output(out2, "/dev/disk13"), None);
    }

    #[test]
    fn drutil_status_no_media() {
        let st = parse_drutil_status("           Type: No Media Inserted\n");
        assert_eq!(st.media_type, "No Media Inserted");
        assert!(!media_from_drutil(&st).present);
    }

    #[test]
    fn drutil_blank_cdr_is_blank_not_rewritable() {
        let out = "\
           Type: CD-R                 Name: /dev/disk13\n\
     Space Free:   79:59:74         blocks:   359999 / 737.28MB / 703.12MiB\n\
     Space Used:   00:00:00         blocks:        0 /   0.00MB /   0.00MiB\n\
    Writability: appendable, blank, overwritable\n";
        let st = parse_drutil_status(out);
        let media = media_from_drutil(&st);
        assert!(media.present);
        assert!(media.is_blank);
        assert_eq!(media.kind, MediaKind::CdR);
        assert_eq!(media.capacity_bytes, 359999 * 2048);
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
