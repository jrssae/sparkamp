//! Read-only mount + audio-file listing for data discs.
//!
//! [`ensure_mounted`] talks to the host udisks2 service over the **system**
//! D-Bus — same connection/proxy pattern as [`crate::devices::detect`] — to
//! mount a data disc's ISO9660 filesystem (or find it already mounted).
//! [`list_disc_files`] then walks the mount point for audio files, reusing
//! [`crate::devices::browse::read_device_track`] for per-file tags exactly
//! like the external-device browser does.
//!
//! `ensure_mounted` is Linux-only: `zbus`/udisks2 has no macOS equivalent.
//! macOS auto-mounts data discs itself (no explicit mount call needed there —
//! `disc::detect`'s mac platform glue resolves the mount path from `mount`(8)
//! once the disc is inserted, Task 11), so [`list_disc_files`] and [`DiscFile`]
//! stay platform-neutral and callable directly against that path. Callers:
//! the GTK frontend's data-disc browse/play/import view (Linux, Task 9, via
//! `ensure_mounted` + `list_disc_files`) and `sparkamp_disc_mount_list`
//! (`src/ffi/disc.rs`, mac, `list_disc_files` only — the OS already mounted
//! it).

use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use std::collections::HashMap;

#[cfg(target_os = "linux")]
use zbus::zvariant::Value;

#[cfg(target_os = "linux")]
const UDISKS: &str = "org.freedesktop.UDisks2";
#[cfg(target_os = "linux")]
const FILESYSTEM_IFACE: &str = "org.freedesktop.UDisks2.Filesystem";

/// Recursive walk depth cap for [`list_disc_files`] (root = 0). Data discs
/// are shallow by construction; this just bounds pathological/malicious
/// directory nesting.
const MAX_DEPTH: u8 = 5;

/// One audio file found on a mounted disc, ready for the burn/import UI.
/// `Serialize`s for the mac FFI (`sparkamp_disc_mount_list`); GTK (Linux)
/// consumes it in-crate and never needs the JSON shape.
#[derive(Clone, serde::Serialize)]
pub struct DiscFile {
    pub path: PathBuf,
    pub display: String,
    pub duration_secs: Option<u32>,
    pub bytes: u64,
}

/// Ensure the disc in `drive` is mounted, returning its mount point.
///
/// Resolves `drive.id` (a Linux device node, e.g. `/dev/sr0`) to the matching
/// udisks2 block object (`/org/freedesktop/UDisks2/block_devices/<basename>`),
/// reads its `Filesystem.MountPoints` (via
/// [`crate::devices::detect::decode_mountpoints`]) and returns the first path
/// if already mounted; otherwise calls `Filesystem.Mount` with no options and
/// returns the path it yields. Read-only comes for free — the kernel always
/// mounts iso9660 `ro` — no explicit flag is passed or needed.
///
/// **Contention note:** this function itself performs a disc READ (the Mount
/// call spins the drive and probes the filesystem), exactly like a TOC probe
/// or a rip. Callers must invoke it only from the same guarded, exclusive-read
/// context other disc reads use (see `disc::detect::set_exclusive_read`) —
/// `ensure_mounted` does not take that guard itself; the GTK caller (Task 9)
/// is responsible for wrapping it.
///
/// Linux-only (see the module doc): macOS never calls this — the OS mounts
/// data discs itself, and `disc::detect`'s mac platform glue already resolves
/// the mount path without a zbus round-trip.
#[cfg(target_os = "linux")]
pub fn ensure_mounted(drive: &super::OpticalDrive) -> Result<PathBuf, String> {
    let basename = Path::new(&drive.id)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("cannot derive a device node from drive id {:?}", drive.id))?;
    let object_path = format!("/org/freedesktop/UDisks2/block_devices/{basename}");

    let conn = zbus::blocking::Connection::system()
        .map_err(|e| format!("connecting to the system D-Bus: {e}"))?;
    let fs = zbus::blocking::Proxy::new(&conn, UDISKS, object_path.as_str(), FILESYSTEM_IFACE)
        .map_err(|e| format!("building udisks2 Filesystem proxy for {object_path}: {e}"))?;

    let raw: Vec<Vec<u8>> = fs.get_property("MountPoints").unwrap_or_default();
    if let Some(existing) = crate::devices::detect::decode_mountpoints(&raw)
        .into_iter()
        .next()
    {
        return Ok(existing);
    }

    let no_opts: HashMap<String, Value> = HashMap::new();
    let mount_path: String = fs
        .call("Mount", &(no_opts,))
        .map_err(|e| format!("udisks2 Mount failed for {object_path}: {e}"))?;
    Ok(PathBuf::from(mount_path))
}

/// Recursively collect audio files under `mount` (depth-capped at
/// [`MAX_DEPTH`]), sorted by path.
///
/// Extensions are [`crate::model::AUDIO_EXTENSIONS`] — the same list the
/// library scanner uses — so a data disc burned by Sparkamp (or any other
/// tool) is filtered identically to a local folder scan. Each match is tag-
/// read via [`crate::devices::browse::read_device_track`] (the same reader
/// used for external devices) and displayed with
/// [`crate::media_library::lib_track_display`] — "Artist — Title", falling
/// back to the filename when no tags are present — so disc files, device
/// files, and library files all render identically. Hidden entries (dotfiles/
/// dirs) are skipped, matching `devices::browse`'s walk. Unreadable
/// directories are ignored so a partially-readable disc still lists what it
/// can.
pub fn list_disc_files(mount: &Path) -> Vec<DiscFile> {
    let mut out = Vec::new();
    walk(mount, 0, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn walk(dir: &Path, depth: u8, out: &mut Vec<DiscFile>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => walk(&path, depth + 1, out),
            Ok(ft) if ft.is_file() && crate::model::is_audio_file(&path) => {
                out.push(to_disc_file(path))
            }
            _ => {}
        }
    }
}

fn to_disc_file(path: PathBuf) -> DiscFile {
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let track = crate::devices::browse::read_device_track(&path);
    let display = crate::media_library::lib_track_display(&track);
    let duration_secs = track.length_secs.map(|s| s.round() as u32);
    DiscFile {
        path,
        display,
        duration_secs,
        bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_filter_keeps_audio_skips_other() {
        // read_device_track falls back to the GStreamer Discoverer for
        // headerless files; it panics unless GStreamer has been initialized
        // somewhere in-process (mirrors duration_probe.rs's own tests).
        gstreamer::init().ok();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("track.mp3"), b"x").unwrap();
        std::fs::write(root.join("playlist.m3u8"), b"x").unwrap();
        std::fs::write(root.join("readme.txt"), b"x").unwrap();

        let names: Vec<String> = list_disc_files(root)
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["track.mp3".to_string()]);
    }

    #[test]
    fn recurses_into_subdirectories() {
        gstreamer::init().ok();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.mp3"), b"x").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("b.flac"), b"x").unwrap();

        let names: Vec<String> = list_disc_files(root)
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.mp3".to_string(), "b.flac".to_string()]);
    }

    #[test]
    fn depth_beyond_cap_is_excluded() {
        gstreamer::init().ok();
        let dir = tempfile::tempdir().unwrap();
        let mut cur = dir.path().to_path_buf();
        // 5 nested directories: file here is at depth 5 (included).
        for i in 0..5 {
            cur = cur.join(format!("d{i}"));
            std::fs::create_dir(&cur).unwrap();
        }
        std::fs::write(cur.join("shallow.mp3"), b"x").unwrap();
        // One directory deeper: file at depth 6 (excluded).
        cur = cur.join("d5");
        std::fs::create_dir(&cur).unwrap();
        std::fs::write(cur.join("deep.mp3"), b"x").unwrap();

        let names: Vec<String> = list_disc_files(dir.path())
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["shallow.mp3".to_string()]);
    }

    #[test]
    fn hidden_entries_are_skipped() {
        gstreamer::init().ok();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join(".hidden.mp3"), b"x").unwrap();
        std::fs::create_dir(root.join(".hidden_dir")).unwrap();
        std::fs::write(root.join(".hidden_dir").join("c.mp3"), b"x").unwrap();
        std::fs::write(root.join("visible.mp3"), b"x").unwrap();

        let names: Vec<String> = list_disc_files(root)
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["visible.mp3".to_string()]);
    }

    #[test]
    fn display_falls_back_to_filename_when_untagged() {
        gstreamer::init().ok();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A single byte of garbage is not a valid MP3, so read_device_track's
        // ID3/Symphonia readers both fail and the display falls back to the
        // filename — the same degrade-gracefully behaviour `browse.rs`'s own
        // tests rely on.
        std::fs::write(root.join("untagged track.mp3"), b"x").unwrap();

        let files = list_disc_files(root);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].display, "untagged track.mp3");
        assert_eq!(files[0].bytes, 1);
        assert_eq!(files[0].duration_secs, None);
    }

    #[test]
    fn empty_or_unreadable_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_disc_files(dir.path()).is_empty());
        assert!(list_disc_files(Path::new("/no/such/dir/xyz")).is_empty());
    }

    /// Manual live probe: mounts the loaded data disc via udisks2, lists its
    /// audio files, and prints them. Run with
    /// `cargo test --lib live_disc_mount_and_list -- --ignored --nocapture`.
    /// Skips (prints + returns) when no disc is present; asserts non-empty
    /// files when a data disc is loaded. Linux-only: exercises `ensure_mounted`.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn live_disc_mount_and_list() {
        crate::disc::detect::set_exclusive_read(true);
        let drives = crate::disc::detect::list_drives();
        let Some(drive) = drives.iter().find(|d| d.media.present) else {
            crate::disc::detect::set_exclusive_read(false);
            println!("no disc loaded — skipping");
            return;
        };
        let mount_result = ensure_mounted(drive);
        let mount = match mount_result {
            Ok(m) => m,
            Err(e) => {
                crate::disc::detect::set_exclusive_read(false);
                println!("ensure_mounted failed ({e}) — skipping (likely an audio CD, not a data disc)");
                return;
            }
        };
        let files = list_disc_files(&mount);
        crate::disc::detect::set_exclusive_read(false);

        println!("mounted at {}", mount.display());
        for f in &files {
            println!(
                "  {} — {} bytes, {:?}s — {}",
                f.display,
                f.bytes,
                f.duration_secs,
                f.path.display()
            );
        }
        assert!(!files.is_empty(), "expected at least one audio file on the data disc");
    }
}
