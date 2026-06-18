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

/// POSIX backend: `std::fs` over the mount path. Delegates to the existing
/// `browse` / `transfer` helpers so behaviour is identical to the
/// pre-abstraction code for udisks2 devices.
///
/// `music_only` scopes scans to the device's `Music` folders instead of walking
/// the whole filesystem — essential for MTP phones, where a full recursive walk
/// of (e.g.) 117 GB over the gvfs FUSE mount never finishes in practice.
pub struct PosixIo {
    mount: PathBuf,
    music_only: bool,
}

impl PosixIo {
    pub fn new(mount: PathBuf) -> Self {
        Self { mount, music_only: false }
    }
    pub fn music_scoped(mount: PathBuf) -> Self {
        Self { mount, music_only: true }
    }

    /// Directories to scan: the whole mount normally, or just the `Music`
    /// folders (mount/Music and mount/<storage>/Music) when `music_only`.
    fn scan_roots(&self) -> Vec<PathBuf> {
        if self.music_only {
            let roots = music_scan_roots(&self.mount);
            if roots.is_empty() {
                vec![self.mount.clone()] // no Music folder found — fall back
            } else {
                roots
            }
        } else {
            vec![self.mount.clone()]
        }
    }
}

impl DeviceIo for PosixIo {
    fn list_audio_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for root in self.scan_roots() {
            out.extend(super::browse::audio_files_under(&root));
        }
        out.sort();
        out.dedup();
        out
    }
    fn playlist_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        // Device playlists are written at the storage root (so their relative
        // `Music/<file>` entries resolve), which the music-scoped roots don't
        // cover — shallow-scan the mount root for them too.
        if self.music_only {
            if let Ok(entries) = std::fs::read_dir(&self.mount) {
                for e in entries.flatten() {
                    let p = e.path();
                    let is_m3u = p
                        .extension()
                        .and_then(|x| x.to_str())
                        .map(|x| {
                            let l = x.to_ascii_lowercase();
                            l == "m3u" || l == "m3u8"
                        })
                        .unwrap_or(false);
                    if is_m3u {
                        out.push(p);
                    }
                }
            }
        }
        for root in self.scan_roots() {
            out.extend(super::browse::playlist_files_under(&root));
        }
        out.sort();
        out.dedup();
        out
    }
    fn copy_to_device(&self, src: &Path, relpath: &Path) -> std::io::Result<CopyOutcome> {
        super::transfer::copy_to_device(src, &self.mount, relpath)
    }
    fn delete(&self, path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }
}

/// Find the `Music` directories on a device: `mount/Music` plus
/// `mount/<storage>/Music` (Android exposes per-storage roots one level down).
/// Only shallow `read_dir`s — cheap even over a slow MTP FUSE mount.
fn music_scan_roots(mount: &Path) -> Vec<PathBuf> {
    fn is_music(p: &Path) -> bool {
        p.file_name()
            .map(|n| n.to_string_lossy().eq_ignore_ascii_case("music"))
            .unwrap_or(false)
    }
    let mut roots = Vec::new();
    let Ok(entries) = std::fs::read_dir(mount) else {
        return roots;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let path = entry.path();
        if is_music(&path) {
            roots.push(path);
            continue;
        }
        // One level deeper: storage roots (e.g. "Internal shared storage").
        if let Ok(sub) = std::fs::read_dir(&path) {
            for s in sub.flatten() {
                let sp = s.path();
                if s.file_type().map(|t| t.is_dir()).unwrap_or(false) && is_music(&sp) {
                    roots.push(sp);
                }
            }
        }
    }
    roots
}

/// Build the IO backend handle for a device.
pub fn for_device(dev: &Device) -> Box<dyn DeviceIo> {
    match dev.backend {
        DeviceBackend::Udisks => Box::new(PosixIo::new(dev.mount_path.clone())),
        // MTP falls back to POSIX over the gvfs FUSE path until the gio backend
        // lands, but scoped to the Music folders so scans actually finish.
        DeviceBackend::Mtp => Box::new(PosixIo::music_scoped(dev.mount_path.clone())),
    }
}
