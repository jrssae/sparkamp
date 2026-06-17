//! Per-backend device IO.
//!
//! udisks2 devices are POSIX block filesystems, browsed and written with
//! `std::fs` over their mount path (today). MTP devices (Android phones) are
//! surfaced by gvfs as a FUSE mount and need gio for reliable IO — that backend
//! arrives in a later phase. The browse / transfer / sync orchestration calls
//! through this trait so it never hard-codes one transport.

// Not every method is routed through the trait yet during the backend-
// abstraction migration; mirrors the allow used across the devices module.
#![allow(dead_code)]

use super::transfer::CopyOutcome;
use super::{Device, DeviceBackend};
use std::path::{Path, PathBuf};

/// Backend-specific filesystem operations on one device. `Send` so a backend
/// handle can be moved onto a worker thread for a blocking copy/scan.
pub trait DeviceIo: Send {
    /// All audio files on the device, in path order.
    fn list_audio_files(&self) -> Vec<PathBuf>;
    /// All playlist files (`.m3u` / `.m3u8`) on the device, in path order.
    fn playlist_files(&self) -> Vec<PathBuf>;
    /// Copy a local file onto the device at `relpath` (relative to the mount
    /// root), creating parent directories.
    fn copy_to_device(&self, src: &Path, relpath: &Path) -> std::io::Result<CopyOutcome>;
    /// Delete a file on the device by absolute path.
    fn delete(&self, path: &Path) -> std::io::Result<()>;
}

/// POSIX backend for udisks2 block filesystems: `std::fs` over the mount path.
/// Delegates to the existing `browse` / `transfer` helpers so behaviour is
/// identical to the pre-abstraction code.
pub struct PosixIo {
    mount: PathBuf,
}

impl PosixIo {
    pub fn new(mount: PathBuf) -> Self {
        Self { mount }
    }
}

impl DeviceIo for PosixIo {
    fn list_audio_files(&self) -> Vec<PathBuf> {
        super::browse::list_audio_files(&self.mount)
    }
    fn playlist_files(&self) -> Vec<PathBuf> {
        super::browse::device_playlist_files(&self.mount)
    }
    fn copy_to_device(&self, src: &Path, relpath: &Path) -> std::io::Result<CopyOutcome> {
        super::transfer::copy_to_device(src, &self.mount, relpath)
    }
    fn delete(&self, path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }
}

/// Build the IO backend handle for a device.
pub fn for_device(dev: &Device) -> Box<dyn DeviceIo> {
    match dev.backend {
        // MTP falls back to POSIX until the gio backend lands — gvfs exposes a
        // FUSE path, so `std::fs` partially works in the meantime.
        DeviceBackend::Udisks | DeviceBackend::Mtp => {
            Box::new(PosixIo::new(dev.mount_path.clone()))
        }
    }
}
