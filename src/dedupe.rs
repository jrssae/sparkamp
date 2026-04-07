//! Music deduplication — core matching algorithm (no GTK).
//!
//! [`find_duplicates`] takes a flat list of already-scanned [`LibTrack`]s and
//! clusters tracks that are likely the same song.  The GTK layer calls this
//! from a background thread; no display types appear here.
//!
//! ## Matching strategy (in order of priority)
//!
//! 1. **Metadata grouping** — tracks whose normalised `artist + title` string
//!    is identical are placed in the same group.
//! 2. **Filename cross-match** — tracks with no usable title metadata are
//!    matched against existing metadata groups by comparing the normalised
//!    filename stem (and one or two parent directory components) to each
//!    group's compact (spaceless) key.
//! 3. **Filename-only grouping** — tracks that still have no match are
//!    grouped by their normalised filename stem alone.
//!
//! ## Confidence levels
//!
//! * **Probable** — metadata group whose members all have durations within
//!   10 s of each other (or have no duration data at all).
//! * **LessLikely** — metadata group with a duration spread > 10 s, or a
//!   filename / path-only match.

use std::collections::{HashMap, HashSet};
use crate::media_library::LibTrack;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Confidence that the members of a [`DupeGroup`] are the same song.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DupeConfidence {
    /// Strong match: metadata agrees and duration spread ≤ 10 s.
    Probable,
    /// Weaker match: duration spread > 10 s, or match came from filename /
    /// path rather than embedded metadata.
    LessLikely,
}

/// A group of tracks believed to be duplicates, with per-file filesystem info.
#[derive(Debug, Clone)]
pub struct DupeGroup {
    /// Human-readable label, e.g. `"Ed Sheeran — Don't"`.
    pub label: String,
    pub confidence: DupeConfidence,
    pub tracks: Vec<DupeTrackInfo>,
}

/// One track within a [`DupeGroup`], enriched with its on-disk file size.
#[derive(Debug, Clone)]
pub struct DupeTrackInfo {
    pub track: LibTrack,
    /// Byte count from `std::fs::metadata`; `None` if the file is missing.
    pub file_size_bytes: Option<u64>,
}

// ---------------------------------------------------------------------------
// Normalisation helpers
// ---------------------------------------------------------------------------

/// Normalise `s` for comparison: lowercase, keep alphanumerics and spaces,
/// drop all other punctuation (apostrophes, hyphens, etc.), collapse runs of
/// whitespace, trim.
///
/// `"Ed Sheeran - Don't"` → `"ed sheeran dont"`
/// `"edshearandont"` → `"edshearandont"`
pub fn normalize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter_map(|c| {
            if c.is_alphanumeric() {
                Some(c.to_ascii_lowercase())
            } else if c.is_whitespace() {
                Some(' ')
            } else {
                None // strip punctuation (apostrophes, hyphens, …)
            }
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Compact form used for substring look-ups: [`normalize`] then strip spaces.
///
/// `"ed sheeran dont"` → `"edshearandont"`
fn compact(s: &str) -> String {
    normalize(s).replace(' ', "")
}

/// Return the best available artist string for `t`: prefers `artist`,
/// falls back to `album_artist`, otherwise returns `""`.
fn effective_artist(t: &LibTrack) -> &str {
    t.artist
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| t.album_artist.as_deref().filter(|s| !s.trim().is_empty()))
        .unwrap_or("")
}

/// Build the metadata grouping key for `t`.  Returns `None` when no title is
/// available so that title-less tracks do not create spurious meta groups.
fn meta_key(t: &LibTrack) -> Option<String> {
    let title = t.title.as_deref().unwrap_or("").trim();
    if title.is_empty() {
        return None;
    }
    let art = effective_artist(t);
    let key = if art.is_empty() {
        normalize(title)
    } else {
        format!("{} {}", normalize(art), normalize(title))
    };
    if key.is_empty() { None } else { Some(key) }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Find duplicate groups in `tracks`.
///
/// Pass only already-scanned tracks (`last_scanned IS NOT NULL`); un-scanned
/// entries have no reliable metadata and produce spurious filename-only matches.
///
/// Returns groups sorted by confidence (Probable first) then label.
pub fn find_duplicates(tracks: Vec<LibTrack>) -> Vec<DupeGroup> {
    let n = tracks.len();

    // ── 1. Group by metadata key ────────────────────────────────────────────
    // meta_groups: normalised_key → [track indices]
    let mut meta_groups: HashMap<String, Vec<usize>> = HashMap::new();
    let mut in_meta = vec![false; n];

    for (i, t) in tracks.iter().enumerate() {
        if let Some(key) = meta_key(t) {
            meta_groups.entry(key).or_default().push(i);
            in_meta[i] = true;
        }
    }

    // ── 2. Compact reverse-lookup for filename cross-matching ───────────────
    // compact_key → full_meta_key
    let compact_to_meta: HashMap<String, String> = meta_groups
        .keys()
        .map(|k| (k.replace(' ', ""), k.clone()))
        .collect();

    // ── 3. Cross-match unmatched tracks via filename / path ─────────────────
    for (i, t) in tracks.iter().enumerate() {
        if in_meta[i] {
            continue;
        }
        let path = std::path::Path::new(&t.path);
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

        // 3a — Exact compact match on filename stem alone.
        // "edshearandont.mp3" → compact "edshearandont" == compact meta key.
        let stem_c = compact(stem);
        if let Some(mkey) = compact_to_meta.get(&stem_c) {
            meta_groups.entry(mkey.clone()).or_default().push(i);
            in_meta[i] = true;
            continue;
        }

        // 3b — Parent directory + stem ("Artist/Song.mp3").
        if let Some(parent) = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
        {
            let c = compact(&format!("{} {}", parent, stem));
            if let Some(mkey) = compact_to_meta.get(&c) {
                meta_groups.entry(mkey.clone()).or_default().push(i);
                in_meta[i] = true;
                continue;
            }
        }

        // 3c — Grandparent + stem ("Artist/Album/Song.mp3").
        if let Some(gp) = path
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
        {
            let c = compact(&format!("{} {}", gp, stem));
            if let Some(mkey) = compact_to_meta.get(&c) {
                meta_groups.entry(mkey.clone()).or_default().push(i);
                in_meta[i] = true;
                continue;
            }
        }
    }

    // ── 4. Filename-only groups for still-unmatched tracks ──────────────────
    let mut file_groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, t) in tracks.iter().enumerate() {
        if in_meta[i] {
            continue;
        }
        let stem = std::path::Path::new(&t.path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let key = normalize(stem);
        if !key.is_empty() {
            file_groups.entry(key).or_default().push(i);
        }
    }

    // ── 5. Build DupeGroup objects ──────────────────────────────────────────
    let mut groups: Vec<DupeGroup> = Vec::new();

    // Avoid reading file sizes for the same path twice across groups.
    let mut size_cache: HashMap<String, Option<u64>> = HashMap::new();
    let mut get_size = |path: &str| -> Option<u64> {
        *size_cache
            .entry(path.to_string())
            .or_insert_with(|| std::fs::metadata(path).ok().map(|m| m.len()))
    };

    // From metadata groups.
    for (_, indices) in &meta_groups {
        if indices.len() < 2 {
            continue;
        }
        // Deduplicate indices (a track can be added via multiple paths above).
        let deduped: Vec<usize> = {
            let mut seen = HashSet::new();
            indices.iter().copied().filter(|i| seen.insert(*i)).collect()
        };
        if deduped.len() < 2 {
            continue;
        }

        // Label from the first track's actual metadata (not the normalised key).
        let sample = &tracks[deduped[0]];
        let art = effective_artist(sample);
        let title = sample.title.as_deref().unwrap_or(sample.filename.as_str());
        let label = if art.is_empty() {
            title.to_string()
        } else {
            format!("{} \u{2014} {}", art, title) // em-dash
        };

        // Confidence: downgrade if duration spread exceeds 10 s.
        let (max_dur, min_dur) =
            deduped
                .iter()
                .fold((None::<f64>, None::<f64>), |(mx, mn), &i| {
                    match tracks[i].length_secs {
                        Some(d) => (
                            Some(mx.map_or(d, |m: f64| m.max(d))),
                            Some(mn.map_or(d, |m: f64| m.min(d))),
                        ),
                        None => (mx, mn),
                    }
                });
        let dur_spread = match (max_dur, min_dur) {
            (Some(hi), Some(lo)) => hi - lo,
            _ => 0.0,
        };
        let confidence = if dur_spread > 10.0 {
            DupeConfidence::LessLikely
        } else {
            DupeConfidence::Probable
        };

        let track_infos = deduped
            .iter()
            .map(|&idx| DupeTrackInfo {
                file_size_bytes: get_size(&tracks[idx].path),
                track: tracks[idx].clone(),
            })
            .collect();

        groups.push(DupeGroup { label, confidence, tracks: track_infos });
    }

    // From filename-only groups.
    for (_, indices) in &file_groups {
        if indices.len() < 2 {
            continue;
        }
        let deduped: Vec<usize> = {
            let mut seen = HashSet::new();
            indices.iter().copied().filter(|i| seen.insert(*i)).collect()
        };
        if deduped.len() < 2 {
            continue;
        }

        let sample = &tracks[deduped[0]];
        let label = std::path::Path::new(&sample.path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(sample.filename.as_str())
            .to_string();

        let track_infos = deduped
            .iter()
            .map(|&idx| DupeTrackInfo {
                file_size_bytes: get_size(&tracks[idx].path),
                track: tracks[idx].clone(),
            })
            .collect();

        groups.push(DupeGroup {
            label,
            confidence: DupeConfidence::LessLikely,
            tracks: track_infos,
        });
    }

    // Sort: Probable first, then label (case-insensitive).
    groups.sort_by(|a, b| {
        use DupeConfidence::*;
        match (&a.confidence, &b.confidence) {
            (Probable, LessLikely) => std::cmp::Ordering::Less,
            (LessLikely, Probable) => std::cmp::Ordering::Greater,
            _ => a.label.to_lowercase().cmp(&b.label.to_lowercase()),
        }
    });

    groups
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_track(id: i64, path: &str, artist: &str, title: &str, secs: Option<f64>) -> LibTrack {
        let filename = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path)
            .to_string();
        LibTrack {
            id,
            path: path.to_string(),
            artist: if artist.is_empty() { None } else { Some(artist.to_string()) },
            title: if title.is_empty() { None } else { Some(title.to_string()) },
            album: None,
            track_num: None,
            genre: None,
            year: None,
            bpm: None,
            length_secs: secs,
            bitrate: None,
            channels: None,
            filetype: None,
            filename,
            play_count: 0,
            last_played: None,
            comment: None,
            album_artist: None,
            disc_num: None,
            disc_total: None,
            composer: None,
            original_artist: None,
            copyright: None,
            url: None,
            encoded_by: None,
            lyric: None,
            artwork_path: None,
            last_scanned: Some("2024-01-01T00:00:00".to_string()),
            sort_keys: Default::default(),
        }
    }

    #[test]
    fn normalize_strips_punctuation_and_lowercases() {
        assert_eq!(normalize("Ed Sheeran - Don't"), "ed sheeran dont");
        assert_eq!(normalize("edshearandont"), "edshearandont");
        assert_eq!(normalize("  HELLO   WORLD  "), "hello world");
    }

    #[test]
    fn finds_exact_metadata_duplicates() {
        let tracks = vec![
            make_track(1, "/a/song.mp3", "Ed Sheeran", "Don't", Some(220.0)),
            make_track(2, "/b/song.mp3", "Ed Sheeran", "Don't", Some(221.0)),
        ];
        let groups = find_duplicates(tracks);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].confidence, DupeConfidence::Probable);
        assert_eq!(groups[0].tracks.len(), 2);
    }

    #[test]
    fn downgrades_confidence_when_duration_spread_exceeds_10s() {
        let tracks = vec![
            make_track(1, "/a/song.mp3", "Artist", "Title", Some(180.0)),
            make_track(2, "/b/song.mp3", "Artist", "Title", Some(195.0)),
        ];
        let groups = find_duplicates(tracks);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].confidence, DupeConfidence::LessLikely);
    }

    #[test]
    fn groups_by_filename_when_no_metadata() {
        let tracks = vec![
            make_track(1, "/a/dont.mp3", "", "", None),
            make_track(2, "/b/dont.mp3", "", "", None),
        ];
        let groups = find_duplicates(tracks);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].confidence, DupeConfidence::LessLikely);
    }

    #[test]
    fn cross_matches_compact_filename_to_metadata_group() {
        let tracks = vec![
            make_track(1, "/a/ed sheeran - dont.mp3", "Ed Sheeran", "Don't", Some(220.0)),
            // "edsheerandont" is the compact form of "ed sheeran dont"
            make_track(2, "/b/edsheerandont.mp3", "", "", Some(220.0)),
        ];
        let groups = find_duplicates(tracks);
        assert_eq!(groups.len(), 1, "compact filename should join metadata group");
        assert_eq!(groups[0].tracks.len(), 2);
    }

    #[test]
    fn single_track_produces_no_group() {
        let tracks = vec![make_track(1, "/a/song.mp3", "Artist", "Title", Some(180.0))];
        let groups = find_duplicates(tracks);
        assert!(groups.is_empty());
    }

    #[test]
    fn probable_groups_sort_before_less_likely() {
        let tracks = vec![
            // Probable pair
            make_track(1, "/a/aa.mp3", "AA", "Song", Some(200.0)),
            make_track(2, "/b/aa.mp3", "AA", "Song", Some(201.0)),
            // Less-likely pair (duration spread > 10 s)
            make_track(3, "/a/bb.mp3", "BB", "Song", Some(200.0)),
            make_track(4, "/b/bb.mp3", "BB", "Song", Some(215.0)),
        ];
        let groups = find_duplicates(tracks);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].confidence, DupeConfidence::Probable);
        assert_eq!(groups[1].confidence, DupeConfidence::LessLikely);
    }
}
