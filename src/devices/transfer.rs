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
pub struct TrackMeta<'a> {
    pub src: &'a Path,
    pub artist: &'a str,
    pub album: &'a str,
    pub title: &'a str,
    pub track_num: Option<i64>,
}

/// Build the on-device relative path:
/// `Music/<Artist>/<Album>/<NN - Title>.<ext>`.
///
/// Falls back to "Unknown Artist"/"Unknown Album" and the source filename stem
/// for the title; the source file extension is preserved. The track number
/// prefix is included only when present and positive.
pub fn device_relpath(meta: &TrackMeta) -> PathBuf {
    let ext = meta.src.extension().and_then(|e| e.to_str()).unwrap_or("");
    let stem = meta
        .src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("track");
    let artist = sanitize_component(meta.artist, "Unknown Artist");
    let album = sanitize_component(meta.album, "Unknown Album");
    let title = sanitize_component(
        if meta.title.is_empty() { stem } else { meta.title },
        stem,
    );
    let base = match meta.track_num {
        Some(n) if n > 0 => format!("{n:02} - {title}"),
        _ => title,
    };
    let filename = if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    };
    Path::new("Music").join(artist).join(album).join(filename)
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
    std::fs::copy(src, &dest)?;
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
    fn relpath_layout_with_and_without_track_number() {
        let src = PathBuf::from("/lib/song.mp3");
        let with_num = device_relpath(&TrackMeta {
            src: &src,
            artist: "Daft Punk",
            album: "Discovery",
            title: "Aerodynamic",
            track_num: Some(3),
        });
        assert_eq!(
            with_num,
            PathBuf::from("Music/Daft Punk/Discovery/03 - Aerodynamic.mp3")
        );

        let no_num = device_relpath(&TrackMeta {
            src: &src,
            artist: "",
            album: "",
            title: "",
            track_num: None,
        });
        // Falls back to Unknown Artist/Album and the source stem as title.
        assert_eq!(
            no_num,
            PathBuf::from("Music/Unknown Artist/Unknown Album/song.mp3")
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
