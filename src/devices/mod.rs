//! External-device support: detecting removable storage, plus the
//! failure-diagnostics classifier for when the system disk service is
//! unreachable. Transfer and sync engines arrive in later phases.

use std::path::PathBuf;

pub mod diagnostics;
// Marker-file identity fallback is pure filesystem logic, shared by the Linux
// and (future) macOS backends.
pub mod marker;
// Listing the audio files on a device's mounted filesystem.
pub mod browse;

// udisks2-backed detection is Linux-only (macOS uses DiskArbitration, added
// in a later phase). The `zbus` dependency is itself Linux-gated.
#[cfg(target_os = "linux")]
pub mod detect;

/// A connected external storage device (USB stick, SD card, or a player
/// mounted as a drive) that holds, or can hold, music.
///
/// Platform-neutral: the Linux [`detect`] backend (udisks2) and the future
/// macOS backend both produce these. `id` is the stable identity used to
/// pair files for sync — the filesystem UUID when available, otherwise a
/// marker-file id written to the device.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}
