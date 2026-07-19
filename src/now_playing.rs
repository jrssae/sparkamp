//! Now-playing data assembly — pure, UI-agnostic.  Frontends render the
//! `NowPlayingInfo` this module builds; they compute no metadata of their own.

use std::path::{Path, PathBuf};

/// Everything a now-playing panel needs to render, assembled once per
/// play-start so every frontend (GTK, TUI, macOS) shows identical data.
#[allow(dead_code)] // removed when T5 wires the GTK play-start caller
#[derive(Debug, Clone)]
pub struct NowPlayingInfo {
    /// Curated ID3 label/value pairs, non-empty only, in `TagFields::field_pairs` order.
    pub tags: Vec<(&'static str, String)>,
    /// e.g. "MP3 · 320kbps · 44.1kHz · Stereo · 3:45" — may be empty if nothing probed.
    pub tech_line: String,
    pub artwork_path: Option<PathBuf>,
    pub play_count: Option<i64>,
    pub last_played: Option<String>,
    pub artist_wiki_url: Option<String>,
    pub album_wiki_url: Option<String>,
}

/// Assemble the render-ready now-playing snapshot for `path`.
///
/// `lib_row` is `Some` when the track is indexed in the media library (its
/// `play_count`/`last_played` are live, post-play values — NOT what we want
/// here, hence `snapshot` carries the pre-play numbers separately). `None`
/// triggers the probe fallback inside `read_only_track_fields` so unindexed
/// files (Testing dirs, ad-hoc playback) still get a technical line.
#[allow(dead_code)] // removed when T5 wires the GTK play-start caller
pub fn build_now_playing_info(
    path: &Path,
    lib_row: Option<&crate::media_library::LibTrack>,
    snapshot: crate::media_library::PlaySnapshot,
) -> NowPlayingInfo {
    // Tags come straight off disk, curated + non-empty only — same source
    // the ID3 editor uses, so the panel and editor never disagree.
    let fields = crate::id3_editor::read_tag_fields(path);
    let tags: Vec<(&'static str, String)> = fields
        .field_pairs()
        .into_iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .collect();

    // Tech line + artwork share one fusion call (library row → embedded
    // APIC → folder image) so both match the ID3 editor's window byte-for-byte.
    let rof = crate::media_library::read_only_track_fields(path, lib_row);
    let tech_line = crate::media_library::tech_summary(&rof);
    let artwork_path = if rof.artwork_path.is_empty() {
        None
    } else {
        Some(PathBuf::from(&rof.artwork_path))
    };

    NowPlayingInfo {
        tags,
        tech_line,
        artwork_path,
        play_count: snapshot.play_count,
        last_played: snapshot.last_played,
        artist_wiki_url: wiki_search_url(&fields.artist),
        album_wiki_url: wiki_search_url(&fields.album),
    }
}

/// Percent-encode `s` for a URL query value.  Unreserved characters
/// (RFC 3986: A–Z a–z 0–9 `-` `_` `.` `~`) pass through; everything else is
/// `%XX` (spaces become `%20`, not `+`, so `Special:Search` treats them as a
/// literal phrase).
fn percent_encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Wikipedia Special:Search URL for `query`, or `None` when it is empty or
/// whitespace-only.
pub fn wiki_search_url(query: &str) -> Option<String> {
    if query.trim().is_empty() {
        return None;
    }
    Some(format!(
        "https://en.wikipedia.org/wiki/Special:Search?search={}",
        percent_encode_query(query)
    ))
}

/// Deterministic cache path for a `px`-sized thumbnail of `artwork_path`.
/// Frontends generate the PNG here on first display (gdk-pixbuf / NSImage);
/// core only owns the path so every frontend shares one cache. Mirrors the
/// artwork-cache hashing idiom in `tags.rs`.
#[allow(dead_code)] // removed when T8 wires the GTK thumbnail cell
pub fn thumb_path_for(artwork_path: &Path, px: u32) -> Option<PathBuf> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    artwork_path.hash(&mut h);
    let hash = h.finish();
    let dir = dirs::cache_dir()?.join("sparkamp").join("thumbs");
    Some(dir.join(format!("{:016x}-{}.png", hash, px)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_library::PlaySnapshot;
    use id3::{Tag, TagLike, Version};
    use tempfile::NamedTempFile;

    /// Create a temporary MP3-style file with an ID3v2 tag and return its
    /// path (the tempfile is leaked to the caller's `dir` via `.into_temp_path`
    /// isn't needed here — we keep the `NamedTempFile` alive by returning it).
    fn make_tagged_mp3(title: &str, artist: &str) -> NamedTempFile {
        // The fake frame below has no real audio data, so probing falls
        // through to the GStreamer Discoverer fallback inside
        // `read_only_track_fields`; that panics unless gst is initialized.
        // Matches the pattern in media_library/tests.rs.
        gstreamer::init().ok();
        let f = NamedTempFile::with_suffix(".mp3").unwrap();
        // Id3v23 (unlike v2.4) treats '/' in a text value as the multi-value
        // separator and re-joins it with NUL on read-back — it would mangle
        // "AC/DC" into "AC\0DC". v2.4 keeps '/' literal, which is what a
        // wiki-link test over an artist name containing a slash needs.
        let mut tag = Tag::new();
        tag.set_title(title);
        tag.set_artist(artist);
        tag.write_to_path(f.path(), Version::Id3v24).unwrap();
        f
    }

    #[test]
    fn info_keeps_only_populated_tags_in_curated_order() {
        let f = make_tagged_mp3("My Song", "AC/DC");
        let info = build_now_playing_info(f.path(), None, PlaySnapshot::default());
        assert_eq!(info.tags.first(), Some(&("Title", "My Song".to_string())));
        assert!(info.tags.iter().any(|(l, _)| *l == "Artist"));
        assert!(!info.tags.iter().any(|(_, v)| v.is_empty()));
    }

    #[test]
    fn info_builds_wiki_urls_from_artist_and_album() {
        let f = make_tagged_mp3("S", "AC/DC");
        let info = build_now_playing_info(f.path(), None, PlaySnapshot::default());
        assert_eq!(
            info.artist_wiki_url.as_deref(),
            Some("https://en.wikipedia.org/wiki/Special:Search?search=AC%2FDC")
        );
        assert_eq!(info.album_wiki_url, None); // album empty → no link
    }

    #[test]
    fn info_carries_snapshot_stats() {
        let f = make_tagged_mp3("S", "A");
        let snap = PlaySnapshot {
            play_count: Some(5),
            last_played: Some("2026-07-01T00:00:00Z".into()),
        };
        let info = build_now_playing_info(f.path(), None, snap);
        assert_eq!(info.play_count, Some(5));
        assert_eq!(info.last_played.as_deref(), Some("2026-07-01T00:00:00Z"));
    }

    #[test]
    fn info_tech_line_present_for_probeable_nonlibrary_file() {
        let f = make_tagged_mp3("S", "A");
        let info = build_now_playing_info(f.path(), None, PlaySnapshot::default());
        assert!(!info.tech_line.is_empty()); // probe fallback filled it in via extension
    }

    #[test]
    fn wiki_url_encodes_and_wraps() {
        assert_eq!(
            wiki_search_url("AC/DC"),
            Some("https://en.wikipedia.org/wiki/Special:Search?search=AC%2FDC".to_string())
        );
        assert_eq!(
            wiki_search_url("Miles Davis"),
            Some("https://en.wikipedia.org/wiki/Special:Search?search=Miles%20Davis".to_string())
        );
    }

    #[test]
    fn wiki_url_empty_is_none() {
        assert_eq!(wiki_search_url(""), None);
        assert_eq!(wiki_search_url("   "), None);
    }

    #[test]
    fn wiki_url_preserves_unreserved() {
        assert_eq!(
            wiki_search_url("A-B_C.D~E"),
            Some("https://en.wikipedia.org/wiki/Special:Search?search=A-B_C.D~E".to_string())
        );
    }

    #[test]
    fn thumb_path_is_deterministic_and_size_specific() {
        use std::path::Path;
        let a = thumb_path_for(Path::new("/music/cover.jpg"), 48).unwrap();
        let b = thumb_path_for(Path::new("/music/cover.jpg"), 48).unwrap();
        let c = thumb_path_for(Path::new("/music/cover.jpg"), 96).unwrap();
        assert_eq!(a, b); // same inputs → same path
        assert_ne!(a, c); // px is part of the filename
        assert!(a.to_string_lossy().contains("/thumbs/"));
        assert_eq!(a.extension().unwrap(), "png");
        assert!(a.file_name().unwrap().to_string_lossy().ends_with("-48.png"));
    }

    #[test]
    fn thumb_path_differs_by_source() {
        use std::path::Path;
        let a = thumb_path_for(Path::new("/music/a.jpg"), 48).unwrap();
        let b = thumb_path_for(Path::new("/music/b.jpg"), 48).unwrap();
        assert_ne!(a, b);
    }
}
