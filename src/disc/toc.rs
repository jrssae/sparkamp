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

/// A track title from a mounted audio-CD filename, or `None` if there is
/// nothing but the number.
///
/// macOS names each mounted AIFF `"<n> <title>.aiff"`, and when it has
/// resolved the disc that title is the real one. Reading it costs nothing and
/// needs no network — the lookup already happened.
///
/// Pure, and compiled everywhere, so the rule is testable off the platform
/// that produces these names.
pub(crate) fn title_from_mounted_name(name: &str) -> Option<String> {
    let stem = std::path::Path::new(name).file_stem()?.to_string_lossy();
    let rest = stem.trim_start_matches(|c: char| c.is_ascii_digit()).trim();
    if rest.is_empty() { None } else { Some(rest.to_string()) }
}

/// Whether a set of derived titles is macOS's generic placeholder rather than
/// real metadata.
///
/// An unresolved disc names every track the same — "Audio Track", and
/// localized, so the words cannot be matched on. What can be matched on is
/// that they are all identical, which no real track list is. Two tracks of the
/// same name on one disc is possible; eight is not.
///
/// A **single**-track disc is trusted, because there is nothing to compare it
/// against and the two outcomes are not symmetric: trusting a placeholder
/// costs a title that reads "Audio Track" instead of "Track 1", while
/// distrusting a real one throws the disc's only title away.
///
/// A partial list is not trusted either. A disc where some names resolved and
/// others did not is a disc that did not resolve.
fn titles_are_placeholders(titles: &[Option<String>]) -> bool {
    if titles.iter().any(|t| t.is_none()) {
        return true;
    }
    let mut named = titles.iter().flatten();
    let Some(first) = named.next() else {
        return true;
    };
    titles.len() > 1 && named.all(|t| t == first)
}

/// Build playlist-ready entries for every audio track on the drive's disc.
///
/// Titles come from the mounted filenames when macOS has resolved the disc,
/// and are "Track N" otherwise. Either way they are only a starting point: a
/// gnudb or CD-TEXT match overwrites them, and the rip window overwrites that.
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

    // Derived up front so the placeholder test can see all of them at once:
    // one track's name says nothing, and the whole list says everything.
    #[cfg(target_os = "macos")]
    let mounted_titles: Vec<Option<String>> = toc
        .tracks
        .iter()
        .filter(|t| t.is_audio)
        .map(|t| {
            aiffs
                .iter()
                .find(|p| {
                    p.file_name()
                        .and_then(|n| leading_number(&n.to_string_lossy()))
                        == Some(t.number as u32)
                })
                .and_then(|p| title_from_mounted_name(&p.file_name()?.to_string_lossy()))
        })
        .collect();
    #[cfg(target_os = "macos")]
    let use_mounted = !titles_are_placeholders(&mounted_titles);
    #[cfg(target_os = "macos")]
    let mut audio_index = 0usize;

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
            #[cfg(target_os = "macos")]
            let title = {
                let mounted = mounted_titles.get(audio_index).cloned().flatten();
                audio_index += 1;
                match mounted.filter(|_| use_mounted) {
                    Some(title) => title,
                    None => format!("Track {}", t.number),
                }
            };
            #[cfg(not(target_os = "macos"))]
            let title = format!("Track {}", t.number);
            Some(DiscTrackEntry {
                number: t.number,
                path,
                title,
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

    #[test]
    fn mounted_names_yield_titles() {
        assert_eq!(
            title_from_mounted_name("3 Hit That Jive, Jack.aiff").as_deref(),
            Some("Hit That Jive, Jack")
        );
        assert_eq!(
            title_from_mounted_name("12 It's A Sin To Tell A Lie.aiff").as_deref(),
            Some("It's A Sin To Tell A Lie")
        );
        // A name that is only a number carries no title.
        assert_eq!(title_from_mounted_name("07.aiff"), None);
        assert_eq!(title_from_mounted_name("  .aiff"), None);
    }

    /// macOS names every track of an unresolved disc the same thing, and
    /// localizes it — so the placeholder cannot be recognised by its words,
    /// only by every track sharing it. Real track lists do not.
    #[test]
    fn identical_titles_across_a_disc_are_placeholders() {
        let generic: Vec<Option<String>> = (0..8).map(|_| Some("Audio Track".to_string())).collect();
        assert!(titles_are_placeholders(&generic));
        // Localized, and still recognised, because nothing here reads the words.
        let localized: Vec<Option<String>> =
            (0..5).map(|_| Some("Audiospur".to_string())).collect();
        assert!(titles_are_placeholders(&localized));

        let real = vec![
            Some("When I Grow Too Old To Dream".to_string()),
            Some("Straighten Up And Fly Right".to_string()),
            Some("Hit That Jive, Jack".to_string()),
        ];
        assert!(!titles_are_placeholders(&real));

        // A single track has nothing to compare against, and is trusted:
        // losing a disc's only real title is the worse of the two mistakes.
        assert!(!titles_are_placeholders(&[Some("Anything".to_string())]));
        // Nothing derived at all is nothing to use.
        assert!(titles_are_placeholders(&[None, None]));
        // A partial list is not trusted: a disc where only some names
        // resolved is a disc that did not resolve.
        assert!(titles_are_placeholders(&[Some("A".to_string()), None]));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn linux_entries_use_cdda_uris() {
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
