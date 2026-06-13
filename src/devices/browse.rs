//! Enumerate the audio files present on a device's mounted filesystem.
//!
//! Pure filesystem walk — no tag reading (kept fast for large devices; richer
//! metadata is a later refinement). Shared by the Linux and macOS frontends.

// Consumed by the GTK device view; unreferenced in the macOS bin until its
// frontend is built.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// Recursively collect audio files under `mount`, sorted by path.
///
/// Hidden entries (dotfiles/dirs, including our `.sparkamp-device-id` marker)
/// are skipped. Unreadable directories are ignored so a partially-readable
/// device still lists what it can.
pub fn list_audio_files(mount: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(mount, &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue; // hidden files/dirs (incl. the device marker)
        }
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => walk(&path, out),
            Ok(ft) if ft.is_file() && crate::model::is_audio_file(&path) => out.push(path),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_audio_recursively_skipping_hidden_and_nonaudio() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.mp3"), b"x").unwrap();
        std::fs::write(root.join("readme.txt"), b"x").unwrap();
        std::fs::write(root.join(".sparkamp-device-id"), b"x").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("b.flac"), b"x").unwrap();
        std::fs::create_dir(root.join(".hidden")).unwrap();
        std::fs::write(root.join(".hidden").join("c.mp3"), b"x").unwrap();

        let names: Vec<String> = list_audio_files(root)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        // a.mp3 (root) sorts before sub/b.flac; the .txt, marker, and the
        // file under the hidden dir are excluded.
        assert_eq!(names, vec!["a.mp3".to_string(), "b.flac".to_string()]);
    }

    #[test]
    fn empty_or_unreadable_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_audio_files(dir.path()).is_empty());
        assert!(list_audio_files(Path::new("/no/such/dir/xyz")).is_empty());
    }
}
