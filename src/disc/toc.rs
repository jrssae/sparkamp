//! TOC math + per-track playlist-entry construction.
//!
//! Pure helpers over [`DiscToc`] (durations) plus the playable path for each
//! track, which is a `cdda://` pseudo-URI against the drive on every
//! platform.

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

/// The sector range of one track as LBAs: start inclusive, end exclusive.
///
/// TOC frames are CDDB-absolute, so track 1 begins at 150 and an LBA is that
/// frame minus the 150-frame pregap. A track runs to wherever the next one
/// starts, and the last runs to the lead-out.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn track_span(toc: &DiscToc, track: u8) -> Option<(u32, u32)> {
    let i = toc.tracks.iter().position(|t| t.number == track)?;
    let start = toc.tracks[i].start_frame;
    let end = toc
        .tracks
        .get(i + 1)
        .map(|t| t.start_frame)
        .unwrap_or(toc.leadout_frame);
    if end <= start {
        return None;
    }
    Some((start.saturating_sub(150), end.saturating_sub(150)))
}

/// Build playlist-ready entries for every audio track on the drive's disc.
///
/// Every track is a `cdda://` pseudo-URI naming the drive and the track
/// number, on both platforms. Titles are "Track N" here and only here: a
/// gnudb or CD-TEXT match overwrites them, and the rip window overwrites
/// that.
///
/// macOS used to read the auto-mounted AIFF files instead, taking both the
/// path and a title from each filename. That is gone: the App Sandbox refuses
/// every read inside a mounted audio-CD volume, so the App Store build listed
/// no tracks at all. Reading the drive works in both builds, and one path
/// means the sandboxed and unsandboxed builds cannot drift apart.
pub fn track_entries(drive: &OpticalDrive) -> Vec<DiscTrackEntry> {
    let Some(toc) = &drive.toc else {
        return Vec::new();
    };
    toc.tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_audio)
        .map(|(i, t)| DiscTrackEntry {
            number: t.number,
            path: format!("cdda://{}?device={}", t.number, drive.id),
            title: format!("Track {}", t.number),
            duration_secs: track_secs(toc, i),
        })
        .collect()
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

    /// Every platform now names a track by drive and number rather than by
    /// a mounted file, so this is no longer a Linux-only expectation.
    #[test]
    fn entries_use_cdda_uris() {
        let drive = OpticalDrive {
            supports_writing: true,
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
