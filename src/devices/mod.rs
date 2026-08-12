//! External-device support: detecting removable storage, plus the
//! failure-diagnostics classifier for when the system disk service is
//! unreachable. Transfer and sync engines arrive in later phases.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod diagnostics;
// Marker-file identity fallback is pure filesystem logic, shared by the Linux
// and (future) macOS backends.
pub mod marker;
// Listing the audio files on a device's mounted filesystem.
pub mod browse;
// Copying library files onto a device under a Music/Artist/Album layout.
pub mod transfer;
// Tag sync (text + rating + play count) between paired library/device files.
pub mod sync;
// Per-backend filesystem IO (POSIX today; gio/MTP in a later phase).
pub mod io;

pub mod plan;

// udisks2-backed detection is Linux-only (macOS uses DiskArbitration, added
// in a later phase). The `zbus` dependency is itself Linux-gated.
#[cfg(target_os = "linux")]
pub mod detect;

/// Which transport/IO backend a device speaks.
///
/// `Udisks` devices are udisks2 block filesystems mounted in the POSIX
/// namespace (USB sticks, SD cards) — browsed and written with `std::fs`.
/// `Mtp` devices are Android phones surfaced by gvfs as a FUSE mount, browsed
/// and written through gio (added in a later phase). The backend decides which
/// [`io::DeviceIo`] implementation drives a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DeviceBackend {
    #[default]
    Udisks,
    /// Android phones surfaced by gvfs as `mtp://` mounts (GTK frontend's
    /// `detect_mtp_devices`); IO currently falls back to `PosixIo` over the
    /// gvfs FUSE path until the gio backend lands.
    Mtp,
    /// A connected device that is **not** a music-sync target: Apple iOS
    /// devices (iPad/iPhone) and any device in photo-transfer (PTP) mode, both
    /// surfaced by gvfs as `gphoto2://` mounts. PTP exposes only the camera roll
    /// read-only, and iOS has no writable music store reachable over the
    /// filesystem (the Music app uses a proprietary, signed media database).
    /// Driven by [`io::NullIo`]; the UI shows an explanatory banner instead of
    /// playlist/file lists and disables Sync.
    //
    // Constructed only by the GTK frontend's `mtp_raw_to_device` (Linux-gated),
    // so the macOS bin target — which compiles neither GTK nor the FFI — never
    // builds a value of this variant. Kept serde-ready for the future macOS
    // ImageCaptureCore detector (see macos-device-sync-parity plan).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Unsupported,
}

/// A connected external storage device (USB stick, SD card, or a player
/// mounted as a drive) that holds, or can hold, music.
///
/// Platform-neutral: the Linux [`detect`] backend (udisks2) and the future
/// macOS backend both produce these. `id` is the stable identity used to
/// pair files for sync — the filesystem UUID when available, otherwise a
/// marker-file id written to the device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// Stable identity (volume UUID, or marker-file id fallback).
    pub id: String,
    /// Human-readable volume label (may be empty).
    pub label: String,
    /// Where the device is currently mounted.
    pub mount_path: PathBuf,
    /// Filesystem type reported by the OS (e.g. `vfat`, `exfat`, `ext4`).
    pub fs_type: String,
    /// Total capacity in bytes (0 when unknown).
    pub total_bytes: u64,
    /// Free space in bytes (0 when unknown).
    pub free_bytes: u64,
    /// Whether the filesystem is mounted read-only (blocks sending files).
    pub read_only: bool,
    /// Whether the OS reports the drive as ejectable.
    pub ejectable: bool,
    /// The udisks2 block-device object path, kept so eject can act on it.
    /// Empty on platforms/paths that don't use udisks2.
    pub backend_id: String,
    /// Which IO backend drives this device (POSIX std::fs vs gio/MTP).
    pub backend: DeviceBackend,
    /// Whether the device's filesystem is actually readable. Always `true` for
    /// mounted block devices. `false` for an MTP phone that is connected but
    /// whose storage isn't visible (file transfer not authorized, or the OS
    /// hasn't exposed the storage volumes) — the UI shows a reconnect banner
    /// instead of empty playlist/file lists.
    pub fs_visible: bool,
}

#[cfg(test)]
mod live_device_tests {
    use std::path::PathBuf;

    /// Exercise the whole external-device path against a real removable
    /// volume: detect it, identify it, browse it, copy a file onto it, and
    /// clean up after itself.
    ///
    /// Everything else in this module is tested against fakes — `null_io`,
    /// temp directories — so nothing here had ever run against real removable
    /// hardware, on a path both the GTK and macOS frontends depend on.
    ///
    /// Writes into `Sparkamp Live Test/` at the root of the device and removes
    /// it again. Needs a writable external volume plugged in.
    ///
    /// `cargo test --lib live_external_device -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_external_device_round_trip() {
        let devices = match crate::devices::detect::list_devices() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("udisks2 unavailable ({e}) — skipping");
                return;
            }
        };
        eprintln!("{} external device(s):", devices.len());
        for d in &devices {
            eprintln!(
                "  {} [{}] {} — {} free of {} — ro={} ejectable={}",
                if d.label.is_empty() { "(no label)" } else { &d.label },
                d.fs_type,
                d.mount_path.display(),
                d.free_bytes,
                d.total_bytes,
                d.read_only,
                d.ejectable
            );
        }
        let Some(dev) = devices.iter().find(|d| !d.read_only) else {
            eprintln!("no writable external device — skipping");
            return;
        };
        let mount = dev.mount_path.clone();

        // Identity must be stable: the same device answers the same id twice,
        // which is what pairs synced files to a device across sessions.
        assert!(!dev.id.is_empty(), "a device must have a stable id");
        // `ensure_marker` writes a real identity file that production keeps.
        // Note whether it was already there, so a device that had never been
        // paired with Sparkamp does not silently acquire one from a test run.
        let marker_preexisting = crate::devices::marker::read_marker(&mount).is_some();
        let marker = crate::devices::marker::ensure_marker(&mount)
            .expect("marker should be writable on a writable device");
        let again = crate::devices::marker::ensure_marker(&mount).expect("second marker read");
        assert_eq!(marker, again, "the marker id must not change between reads");
        assert_eq!(
            crate::devices::marker::read_marker(&mount).as_deref(),
            Some(marker.as_str())
        );

        // Browsing must not blow up on a real filesystem, whatever is on it.
        let audio = crate::devices::browse::list_audio_files(&mount);
        let playlists = crate::devices::browse::device_playlist_files(&mount);
        eprintln!(
            "browse: {} audio file(s), {} playlist(s)",
            audio.len(),
            playlists.len()
        );

        // Copy something real onto it, then verify and remove it.
        let src_dir = std::env::temp_dir().join(format!("sparkamp-devsrc-{}", std::process::id()));
        std::fs::create_dir_all(&src_dir).expect("temp source dir");
        let src = src_dir.join("Live Test Track.mp3");
        let payload: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &payload).expect("write temp source");

        let relpath = PathBuf::from("Sparkamp Live Test").join("Live Test Track.mp3");
        let outcome = crate::devices::transfer::copy_to_device(&src, &mount, &relpath)
            .expect("copy to a writable device");
        assert_eq!(outcome, crate::devices::transfer::CopyOutcome::Copied);

        let dest = mount.join(&relpath);
        let written = std::fs::read(&dest).expect("read the file back off the device");
        assert_eq!(written, payload, "the bytes on the device must match the source");

        // Copying the same file again is skipped, not duplicated — the check
        // that stops a re-sync rewriting everything already there.
        let second = crate::devices::transfer::copy_to_device(&src, &mount, &relpath)
            .expect("second copy");
        assert_eq!(
            second,
            crate::devices::transfer::CopyOutcome::SkippedPresent,
            "an identical file already there must not be copied again"
        );

        eprintln!("copied and verified {} bytes at {}", payload.len(), dest.display());

        // Clean up: the device is the user's, not a scratch dir.
        let _ = std::fs::remove_file(&dest);
        let _ = std::fs::remove_dir(mount.join("Sparkamp Live Test"));
        let _ = std::fs::remove_dir_all(&src_dir);
        if !marker_preexisting {
            let _ = std::fs::remove_file(mount.join(crate::devices::marker::MARKER_FILE));
        }
        assert!(!dest.exists(), "the test must leave the device as it found it");
        assert_eq!(
            crate::devices::marker::read_marker(&mount).is_some(),
            marker_preexisting,
            "the marker must be left exactly as it was found"
        );
    }
}
