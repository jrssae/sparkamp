//! Copying library files onto a device under a `Music/Artist/Album` layout,
//! skipping files already present, and reporting how much space a transfer
//! needs. Pure file operations — the GTK layer orchestrates which files to
//! copy and records the sync pairs.

// The GTK "Copy to device" action wires these in below; unreferenced in the
// macOS bin until its frontend is built.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// Sanitize one path component: replace path-illegal / control characters,
/// trim surrounding whitespace and dots, and fall back when empty.
pub fn sanitize_component(s: &str, fallback: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

/// Metadata needed to place a track on the device.
/// Build the on-device relative path for `src`: a flat `Music/<filename>`,
/// keeping the source filename (sanitized for FAT-illegal characters). Flat by
/// design — collisions between genuinely different files are resolved with a
/// `-N` suffix via [`resolve_collision`].
pub fn device_flat_relpath(src: &Path) -> PathBuf {
    let name = src
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("track");
    Path::new("Music").join(sanitize_component(name, "track"))
}

/// If `mount/relpath` is already taken (by a *different* file — callers
/// dedup the same file via sync pairs first), return a free path by appending
/// `-2`, `-3`, … to the stem. Otherwise return `relpath` unchanged.
pub fn resolve_collision(mount: &Path, relpath: &Path) -> PathBuf {
    if !mount.join(relpath).exists() {
        return relpath.to_path_buf();
    }
    let dir = relpath.parent().unwrap_or_else(|| Path::new(""));
    let stem = relpath
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("track");
    let ext = relpath.extension().and_then(|e| e.to_str());
    for n in 2..100_000 {
        let name = match ext {
            Some(e) => format!("{stem}-{n}.{e}"),
            None => format!("{stem}-{n}"),
        };
        let candidate = dir.join(name);
        if !mount.join(&candidate).exists() {
            return candidate;
        }
    }
    relpath.to_path_buf()
}

/// Result of copying one file to the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyOutcome {
    Copied,
    /// A file of the same size already existed at the destination.
    SkippedPresent,
}

/// Copy `src` to `mount/relpath`, creating parent directories. Skips when a
/// file of the same size already exists at the destination (cheap
/// already-present check that avoids needless re-copies).
pub fn copy_to_device(src: &Path, mount: &Path, relpath: &Path) -> std::io::Result<CopyOutcome> {
    let dest = mount.join(relpath);
    if let (Ok(d), Ok(s)) = (std::fs::metadata(&dest), std::fs::metadata(src)) {
        if d.len() == s.len() {
            return Ok(CopyOutcome::SkippedPresent);
        }
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Manual byte copy rather than std::fs::copy: the latter also copies the
    // source's permissions (chmod), which MTP/gvfs rejects — making the copy
    // report failure even though the bytes were written. A plain read→write
    // never touches permissions and works on both POSIX and MTP devices.
    use std::io::Write;
    let mut reader = std::fs::File::open(src)?;
    let mut writer = std::fs::File::create(&dest)?;
    std::io::copy(&mut reader, &mut writer)?;
    writer.flush()?;
    Ok(CopyOutcome::Copied)
}

/// Total bytes a transfer will actually consume: the sum of source sizes for
/// files not already present (same-size) at their device destination.
pub fn bytes_needed(pairs: &[(PathBuf, PathBuf)], mount: &Path) -> u64 {
    pairs
        .iter()
        .filter_map(|(src, rel)| {
            let src_len = std::fs::metadata(src).ok()?.len();
            if let Ok(d) = std::fs::metadata(mount.join(rel)) {
                if d.len() == src_len {
                    return None; // already present
                }
            }
            Some(src_len)
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_relpath_keeps_sanitized_filename_under_music() {
        assert_eq!(
            device_flat_relpath(Path::new("/lib/03 - Aerodynamic.mp3")),
            PathBuf::from("Music/03 - Aerodynamic.mp3")
        );
        // FAT-illegal characters in the name are replaced.
        assert_eq!(
            device_flat_relpath(Path::new("/lib/AC:DC?.mp3")),
            PathBuf::from("Music/AC_DC_.mp3")
        );
    }

    #[test]
    fn resolve_collision_suffixes_only_when_taken() {
        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path();
        let rel = Path::new("Music/song.mp3");
        // Free → unchanged.
        assert_eq!(resolve_collision(mount, rel), rel.to_path_buf());
        // Taken → -2.
        std::fs::create_dir_all(mount.join("Music")).unwrap();
        std::fs::write(mount.join(rel), b"x").unwrap();
        assert_eq!(
            resolve_collision(mount, rel),
            PathBuf::from("Music/song-2.mp3")
        );
        // -2 also taken → -3.
        std::fs::write(mount.join("Music/song-2.mp3"), b"x").unwrap();
        assert_eq!(
            resolve_collision(mount, rel),
            PathBuf::from("Music/song-3.mp3")
        );
    }

    #[test]
    fn sanitize_strips_separators_and_falls_back() {
        assert_eq!(sanitize_component("AC/DC", "x"), "AC_DC");
        assert_eq!(sanitize_component("  ..  ", "Unknown"), "Unknown");
        assert_eq!(sanitize_component("a:b*c?", "x"), "a_b_c_");
    }

    #[test]
    fn copy_creates_dirs_then_skips_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("song.mp3");
        std::fs::write(&src, b"hello world").unwrap();
        let mount = dir.path().join("device");
        std::fs::create_dir(&mount).unwrap();
        let rel = Path::new("Music/A/B/03 - t.mp3");

        assert_eq!(
            copy_to_device(&src, &mount, rel).unwrap(),
            CopyOutcome::Copied
        );
        assert!(mount.join(rel).exists());
        // Second copy of the same-size file is skipped.
        assert_eq!(
            copy_to_device(&src, &mount, rel).unwrap(),
            CopyOutcome::SkippedPresent
        );
    }

    #[test]
    fn bytes_needed_excludes_already_present() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.mp3");
        std::fs::write(&src, b"12345").unwrap(); // 5 bytes
        let mount = dir.path().join("dev");
        std::fs::create_dir(&mount).unwrap();
        let rel = PathBuf::from("Music/x/y/a.mp3");
        let pairs = vec![(src.clone(), rel.clone())];

        assert_eq!(bytes_needed(&pairs, &mount), 5);
        copy_to_device(&src, &mount, &rel).unwrap();
        assert_eq!(bytes_needed(&pairs, &mount), 0); // now present
    }
}
