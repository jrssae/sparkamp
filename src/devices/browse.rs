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

/// Read a device file into a non-DB [`crate::media_library::LibTrack`] for
/// column display: text tags via the shared reader, play count from the POPM
/// frame, duration from the container header, and filetype from the extension.
/// `id` is 0 (the file is not in the library). Reading tags is I/O, so callers
/// should do this off the UI thread for many files.
pub fn read_device_track(path: &Path) -> crate::media_library::LibTrack {
    use crate::media_library::{LibTrack, SortKeys};
    let tags = crate::tags::read_track_tags(path);
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let filetype = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase());
    // Header probe first (fast); fall back to the GStreamer discoverer for
    // CBR MP3 and anything Symphonia can't measure, so duration is populated
    // for device files just like the files view.
    let length_secs = crate::duration_probe::probe_duration(path)
        .or_else(|| crate::duration_probe::discover_duration(path))
        .map(|d| d.as_secs_f64());
    let play_count = crate::devices::sync::read_tag_state(path).play_count as i64;
    let mut t = LibTrack {
        id: 0,
        path: path.to_string_lossy().into_owned(),
        artist: tags.artist,
        title: tags.title,
        album: tags.album,
        track_num: tags.track_num,
        genre: tags.genre,
        year: tags.year,
        bpm: tags.bpm,
        length_secs,
        bitrate: tags.bitrate,
        channels: tags.channels,
        filetype,
        filename,
        play_count,
        last_played: None,
        comment: tags.comment,
        album_artist: tags.album_artist,
        disc_num: tags.disc_num,
        disc_total: tags.disc_total,
        composer: tags.composer,
        original_artist: tags.original_artist,
        copyright: tags.copyright,
        url: tags.url,
        encoded_by: tags.encoded_by,
        lyric: tags.lyric,
        artwork_path: tags.artwork_path,
        last_scanned: None,
        sample_rate: None,
        file_size: None,
        file_mtime: None,
        added_at: None,
        bitrate_mode: None,
        rg_track_gain: None,
        rg_track_peak: None,
        rg_album_gain: None,
        rg_album_peak: None,
        sort_keys: SortKeys::default(),
    };
    t.sort_keys = SortKeys::from_track(&t);
    t
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

/// All `.m3u` / `.m3u8` playlist files under `mount`, sorted by path.
pub fn device_playlist_files(mount: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_playlists(mount, &mut out);
    out.sort();
    out
}

/// Audio files under a single directory, sorted. Like [`list_audio_files`] but
/// for an arbitrary subtree — used to scan only the music folders on a large
/// MTP device instead of its whole (slow, FUSE-backed) filesystem.
pub fn audio_files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

/// Playlist files under a single directory, sorted (MTP-scoped counterpart of
/// [`device_playlist_files`]).
pub fn playlist_files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_playlists(dir, &mut out);
    out.sort();
    out
}

fn walk_playlists(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => walk_playlists(&path, out),
            Ok(ft) if ft.is_file() => {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|s| s.to_ascii_lowercase());
                if matches!(ext.as_deref(), Some("m3u") | Some("m3u8")) {
                    out.push(path);
                }
            }
            _ => {}
        }
    }
}

/// The filename component of each track entry in an m3u/m3u8 playlist, in
/// playlist order. Comment/`#` lines are ignored; both `/` and `\` separators
/// are handled. Used to filter and order the device track view.
pub fn playlist_entry_order(playlist: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(playlist) else {
        return Vec::new();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            l.replace('\\', "/")
                .rsplit('/')
                .next()
                .unwrap_or(l)
                .to_string()
        })
        .filter(|n| !n.is_empty())
        .collect()
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
