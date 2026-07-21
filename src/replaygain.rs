//! ReplayGain analysis — album batching, value formatting, and the GStreamer
//! `rganalysis` pipeline that computes track/album gain + peak. The playback
//! side (applying gain via `rgvolume`) lives in `engine.rs`; this module only
//! MEASURES and hands results to the media library / tag write-back.
//!
//! Analysis decodes whole files, so callers run it on a single background
//! worker (never per-track in parallel — decoding is CPU-bound).

#![allow(dead_code)] // wired by P4-T6 (library actions) / P4-T9 (auto-analyze).

use crate::media_library::LibTrack;

/// One track's ReplayGain result: gains in dB, peaks linear (0..~1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RgResult {
    pub track_gain: f64,
    pub track_peak: f64,
    pub album_gain: f64,
    pub album_peak: f64,
}

/// Format a gain value as Winamp-compatible ReplayGain text, e.g. `-6.20 dB`.
pub fn format_gain_db(gain_db: f64) -> String {
    format!("{:.2} dB", gain_db)
}

/// Format a linear peak value the way ReplayGain tags store it, e.g.
/// `0.988123` (six decimals).
pub fn format_peak(peak: f64) -> String {
    format!("{:.6}", peak)
}

/// The album-grouping key for a track: album + album-artist (falling back to
/// artist), case-insensitive. `None` when the album tag is empty — such tracks
/// analyze alone (a per-track batch), since an "album gain" over unrelated
/// singletons is meaningless.
fn album_key(t: &LibTrack) -> Option<(String, String)> {
    let album = t.album.as_deref().unwrap_or("").trim();
    if album.is_empty() {
        return None;
    }
    let artist = t
        .album_artist
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or(t.artist.as_deref())
        .unwrap_or("")
        .trim();
    Some((album.to_lowercase(), artist.to_lowercase()))
}

/// Group `tracks` into ReplayGain analysis batches (as index lists into the
/// input slice). Tracks sharing an album batch together so album gain is
/// meaningful; album-less tracks each get their own batch. Input order is
/// preserved (a batch appears at the position of its first member).
pub fn album_batches(tracks: &[LibTrack]) -> Vec<Vec<usize>> {
    let mut batches: Vec<Vec<usize>> = Vec::new();
    // Maps an album key to the index of its batch in `batches`.
    let mut by_key: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for (i, t) in tracks.iter().enumerate() {
        match album_key(t) {
            Some(key) => {
                if let Some(&b) = by_key.get(&key) {
                    batches[b].push(i);
                } else {
                    by_key.insert(key, batches.len());
                    batches.push(vec![i]);
                }
            }
            None => batches.push(vec![i]), // album-less → analyze alone
        }
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_library::SortKeys;

    fn track(path: &str, album: Option<&str>, album_artist: Option<&str>, artist: Option<&str>) -> LibTrack {
        LibTrack {
            id: 0,
            path: path.to_string(),
            artist: artist.map(String::from),
            title: None,
            album: album.map(String::from),
            track_num: None,
            genre: None,
            year: None,
            bpm: None,
            length_secs: None,
            bitrate: None,
            channels: None,
            filetype: None,
            filename: path.to_string(),
            play_count: 0,
            last_played: None,
            comment: None,
            album_artist: album_artist.map(String::from),
            disc_num: None,
            disc_total: None,
            composer: None,
            original_artist: None,
            copyright: None,
            url: None,
            encoded_by: None,
            lyric: None,
            artwork_path: None,
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
        }
    }

    #[test]
    fn format_helpers_match_winamp() {
        assert_eq!(format_gain_db(-6.2), "-6.20 dB");
        assert_eq!(format_gain_db(3.4), "3.40 dB");
        assert_eq!(format_gain_db(0.0), "0.00 dB");
        assert_eq!(format_peak(0.988123), "0.988123");
        assert_eq!(format_peak(1.0), "1.000000");
    }

    #[test]
    fn batches_group_by_album_and_artist() {
        let tracks = vec![
            track("/a1.mp3", Some("Album X"), Some("Artist A"), Some("Artist A")),
            track("/b.mp3", Some("Other"), Some("Artist B"), Some("Artist B")),
            track("/a2.mp3", Some("album x"), Some("artist a"), None), // same album, case-insensitive
        ];
        let b = album_batches(&tracks);
        assert_eq!(b, vec![vec![0, 2], vec![1]]);
    }

    #[test]
    fn album_artist_falls_back_to_artist() {
        // Same album, no album_artist → keyed on artist; same artist groups.
        let tracks = vec![
            track("/1.mp3", Some("LP"), None, Some("Band")),
            track("/2.mp3", Some("LP"), None, Some("Band")),
            track("/3.mp3", Some("LP"), None, Some("Other")), // different artist → own batch
        ];
        let b = album_batches(&tracks);
        assert_eq!(b, vec![vec![0, 1], vec![2]]);
    }

    #[test]
    fn albumless_tracks_analyze_alone() {
        let tracks = vec![
            track("/x.mp3", None, None, Some("A")),
            track("/y.mp3", Some(""), None, Some("A")), // empty album == none
            track("/z.mp3", None, None, Some("A")),
        ];
        let b = album_batches(&tracks);
        assert_eq!(b, vec![vec![0], vec![1], vec![2]]);
    }
}
