//! TOC math + per-track playlist-entry construction.
//!
//! Pure helpers over [`DiscToc`] (durations) plus the platform-appropriate
//! playable path for each track: macOS uses the auto-mounted AIFF files,
//! Linux uses `cdda://` pseudo-URIs against the drive node.

use super::{DiscToc, DiscTrackEntry, OpticalDrive};

/// Seconds of audio in track `index` (0-based position in `toc.tracks`):
/// distance to the next track's start (or the leadout for the last track),
/// at 75 frames per second.
pub fn track_secs(toc: &DiscToc, index: usize) -> u32 {
    let Some(track) = toc.tracks.get(index) else {
        return 0;
    };
    let end = toc
        .tracks
        .get(index + 1)
        .map(|t| t.start_frame)
        .unwrap_or(toc.leadout_frame);
    end.saturating_sub(track.start_frame) / 75
}

/// Total playing time of the disc in seconds (first track start → leadout).
// Feeds the CDDB `query` command's `nsecs` argument — consumed by the gnudb
// client in Phase 2; tested now so the math can't rot before then.
#[allow(dead_code)]
pub fn total_secs(toc: &DiscToc) -> u32 {
    let first = toc.tracks.first().map(|t| t.start_frame).unwrap_or(0);
    toc.leadout_frame.saturating_sub(first) / 75
}

/// Build playlist-ready entries for every audio track on the drive's disc.
/// Titles are "Track N" until a gnudb match supplies real ones (Phase 2).
pub fn track_entries(drive: &OpticalDrive) -> Vec<DiscTrackEntry> {
    let Some(toc) = &drive.toc else {
        return Vec::new();
    };

    // macOS: the mounted volume holds one AIFF per audio track, named with a
    // leading track number (localized suffix — don't match on the words).
    #[cfg(target_os = "macos")]
    let aiffs: Vec<std::path::PathBuf> = drive
        .mount_path
        .as_deref()
        .map(mounted_aiffs)
        .unwrap_or_default();

    toc.tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_audio)
        .filter_map(|(i, t)| {
            #[cfg(target_os = "macos")]
            let path = aiffs
                .iter()
                .find(|p| {
                    p.file_name()
                        .and_then(|n| leading_number(&n.to_string_lossy()))
                        == Some(t.number as u32)
                })
                .map(|p| p.display().to_string())?;
            #[cfg(not(target_os = "macos"))]
            let path = format!("cdda://{}?device={}", t.number, drive.id);
            Some(DiscTrackEntry {
                number: t.number,
                path,
                title: format!("Track {}", t.number),
                duration_secs: track_secs(toc, i),
            })
        })
        .collect()
}

/// List the audio-track AIFF files in a mounted audio-CD volume, in track
/// order. Matching is by the leading integer in the filename ("1 Audio
/// Track.aiff" / localized variants), never the localized words.
#[cfg(target_os = "macos")]
fn mounted_aiffs(mount: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found: Vec<(u32, std::path::PathBuf)> = std::fs::read_dir(mount)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let ext = p.extension()?.to_ascii_lowercase();
            if ext != "aiff" && ext != "aif" {
                return None;
            }
            let n = leading_number(&p.file_name()?.to_string_lossy())?;
            Some((n, p))
        })
        .collect();
    found.sort_by_key(|(n, _)| *n);
    found.into_iter().map(|(_, p)| p).collect()
}

/// Parse the leading decimal integer of a filename ("12 Audio Track.aiff" → 12).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn leading_number(name: &str) -> Option<u32> {
    let digits: String = name.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disc::TocTrack;

    /// TOC shaped like the real 8-track test disc (values from its
    /// `.TOC.plist`): track 1 at absolute frame 150, leadout 124766.
    fn sample_toc() -> DiscToc {
        let starts = [150u32, 13834, 30216, 44337, 59560, 73612, 97120, 110977];
        DiscToc {
            tracks: starts
                .iter()
                .enumerate()
                .map(|(i, &s)| TocTrack {
                    number: (i + 1) as u8,
                    start_frame: s,
                    is_audio: true,
                })
                .collect(),
            leadout_frame: 124766,
        }
    }

    #[test]
    fn track_durations_from_gaps() {
        let toc = sample_toc();
        // Track 1: (13834 - 150) / 75 = 182 s.
        assert_eq!(track_secs(&toc, 0), 182);
        // Last track ends at the leadout: (124766 - 110977) / 75 = 183 s.
        assert_eq!(track_secs(&toc, 7), 183);
        // Out of range → 0.
        assert_eq!(track_secs(&toc, 8), 0);
    }

    #[test]
    fn total_is_first_to_leadout() {
        let toc = sample_toc();
        assert_eq!(total_secs(&toc), (124766 - 150) / 75);
    }

    #[test]
    fn leading_number_parses_and_rejects() {
        assert_eq!(leading_number("1 Audio Track.aiff"), Some(1));
        assert_eq!(leading_number("12 Audiospur.aiff"), Some(12));
        assert_eq!(leading_number("cover.jpg"), None);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn linux_entries_use_cdda_uris() {
        let drive = OpticalDrive {
            id: "/dev/sr0".into(),
            label: "TEST".into(),
            media: crate::disc::MediaInfo {
                present: true,
                is_audio_cd: true,
                ..crate::disc::MediaInfo::none()
            },
            toc: Some(sample_toc()),
            mount_path: None,
        };
        let entries = track_entries(&drive);
        assert_eq!(entries.len(), 8);
        assert_eq!(entries[0].path, "cdda://1?device=/dev/sr0");
        assert_eq!(entries[0].title, "Track 1");
        assert_eq!(entries[0].duration_secs, 182);
    }
}
