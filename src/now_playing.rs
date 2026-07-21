//! Now-playing data assembly — pure, UI-agnostic.  Frontends render the
//! `NowPlayingInfo` this module builds; they compute no metadata of their own.

use std::path::{Path, PathBuf};

/// Everything a now-playing panel needs to render, assembled once per
/// play-start so every frontend (GTK, TUI, macOS) shows identical data.
#[derive(Debug, Clone)]
pub struct NowPlayingInfo {
    /// Curated ID3 label/value pairs, non-empty only, in `TagFields::field_pairs` order.
    pub tags: Vec<(&'static str, String)>,
    /// e.g. "MP3 · 320kbps · 44.1kHz · Stereo · 3:45" — may be empty if nothing
    /// probed. Kept for the TUI / mac / MPRIS single-line display; the GTK panel
    /// renders `technical` (below) as discrete label/value rows instead.
    pub tech_line: String,
    /// Discrete technical fields as label/value pairs (Format / Bitrate /
    /// Sample rate / Channels), non-empty only — the length is deliberately
    /// omitted (shown by the seek bar). Same rendering as `tags`.
    pub technical: Vec<(&'static str, String)>,
    pub artwork_path: Option<PathBuf>,
    pub play_count: Option<i64>,
    pub last_played: Option<String>,
    /// ISO-8601 timestamp of the last metadata scan, or `None` when the file
    /// isn't in the library.
    pub last_scanned: Option<String>,
    /// ISO-8601 timestamp the file first entered the library, or `None` when
    /// it isn't indexed.
    pub added_at: Option<String>,
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
pub fn build_now_playing_info(
    path: &Path,
    lib_row: Option<&crate::media_library::LibTrack>,
    snapshot: crate::media_library::PlaySnapshot,
) -> NowPlayingInfo {
    // Tags come straight off disk, curated + non-empty only — same source
    // the ID3 editor uses, so the panel and editor never disagree.
    let fields = crate::id3_editor::read_tag_fields(path);
    let mut tags: Vec<(&'static str, String)> = fields
        .field_pairs()
        .into_iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .collect();

    // When a file carries no usable ID3 text at all, fall back to the filename
    // stem — mirrors the marquee's display_name (artist → album_artist →
    // filename) so the panel never shows an empty title group.
    if tags.is_empty() {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string();
        tags.push(("Title", name));
    }

    // Tech line + artwork share one fusion call (library row → embedded
    // APIC → folder image) so both match the ID3 editor's window byte-for-byte.
    let rof = crate::media_library::read_only_track_fields(path, lib_row);
    let tech_line = crate::media_library::tech_summary(&rof);
    // Discrete technical rows (label/value) — same fields as `tech_line` minus
    // the length. Uppercased filetype matches `tech_summary`'s formatting.
    let mut technical: Vec<(&'static str, String)> = Vec::new();
    if !rof.filetype.is_empty() {
        technical.push(("Format", rof.filetype.to_uppercase()));
    }
    if !rof.bitrate.is_empty() {
        // The bitrate value is size×8/duration — exact for CBR, the honest
        // average for VBR. Flag VBR inline (e.g. "192k VBR") rather than as a
        // separate row. Mode comes from the indexed row, else a direct Xing/
        // Info-header sniff so non-library MP3s are flagged too.
        let is_vbr = lib_row
            .and_then(|t| t.bitrate_mode.clone())
            .filter(|m| !m.is_empty())
            .or_else(|| crate::technical_probe::mp3_bitrate_mode(path).map(str::to_string))
            .as_deref()
            == Some("VBR");
        let value = if is_vbr {
            format!("{} VBR", rof.bitrate)
        } else {
            rof.bitrate.clone()
        };
        technical.push(("Bitrate", value));
    }
    if !rof.sample_rate.is_empty() {
        technical.push(("Sample rate", rof.sample_rate.clone()));
    }
    if !rof.channels.is_empty() {
        technical.push(("Channels", rof.channels.clone()));
    }
    // File size — from the library row, else stat the file directly so
    // non-library playback still shows it.
    let file_size = lib_row
        .and_then(|t| t.file_size)
        .or_else(|| std::fs::metadata(path).ok().map(|m| m.len() as i64));
    if let Some(bytes) = file_size.filter(|b| *b > 0) {
        technical.push(("File size", format_bytes(bytes)));
    }
    // `read_only_track_fields` only probes embedded/folder art for files
    // OUTSIDE the library (its own `artwork_path` block gates the probe on
    // `track.is_none()`) — probing for library rows too would leak into the
    // ID3 editor's save path (it calls that fn directly to pre-fill its
    // artwork entry, then embeds whatever is non-empty as APIC on save) and
    // silently embed a loose folder image into the file on an unrelated
    // edit. The now-playing display has no save path, so the fallback lives
    // here instead: when the library's cached art column is empty, probe
    // embedded APIC / folder image directly. Display-only — never mutates.
    let artwork_path = if !rof.artwork_path.is_empty() {
        Some(PathBuf::from(&rof.artwork_path))
    } else {
        crate::tags::read_track_tags(path).artwork_path.map(PathBuf::from)
    };

    NowPlayingInfo {
        tags,
        tech_line,
        technical,
        artwork_path,
        play_count: snapshot.play_count,
        last_played: snapshot.last_played,
        last_scanned: lib_row.and_then(|t| t.last_scanned.clone()),
        added_at: lib_row.and_then(|t| t.added_at.clone()),
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

/// Human-readable byte size (MB / KB / B) for the Technical panel row.
fn format_bytes(bytes: i64) -> String {
    let b = bytes as f64;
    if b >= 1_048_576.0 {
        format!("{:.1} MB", b / 1_048_576.0)
    } else if b >= 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else {
        format!("{bytes} B")
    }
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
    fn info_falls_back_to_filename_when_no_tags() {
        gstreamer::init().ok();
        // An untagged file: every curated ID3 field is empty.
        let f = NamedTempFile::with_suffix(".mp3").unwrap();
        let info = build_now_playing_info(f.path(), None, PlaySnapshot::default());
        let stem = f
            .path()
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap()
            .to_string();
        assert_eq!(info.tags, vec![("Title", stem)]);
    }

    #[test]
    fn info_technical_has_discrete_rows_without_length() {
        let f = make_tagged_mp3("S", "A");
        let info = build_now_playing_info(f.path(), None, PlaySnapshot::default());
        // Format is derived from the extension even for an unprobeable fake file.
        assert!(info.technical.iter().any(|(l, _)| *l == "Format"));
        // Length is never a technical row — the seek bar shows it.
        assert!(!info.technical.iter().any(|(l, _)| *l == "Length"));
        // Not-in-library file → no scan timestamp.
        assert_eq!(info.last_scanned, None);
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

    /// A minimal `LibTrack` stub with every optional field empty except the
    /// ones a caller sets — mirrors the pattern in
    /// `media_library::tests::sort_keys_are_precomputed_from_libtrack`.
    fn stub_lib_track(path: &str, artwork_path: Option<String>) -> crate::media_library::LibTrack {
        crate::media_library::LibTrack {
            id: 1,
            path: path.to_string(),
            artist: None,
            title: None,
            album: None,
            track_num: None,
            genre: None,
            year: None,
            bpm: None,
            length_secs: None,
            bitrate: None,
            channels: None,
            filetype: None,
            filename: String::new(),
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
            artwork_path,
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
            sort_keys: crate::media_library::SortKeys::default(),
        }
    }

    /// Finding-2 regression: a LIBRARY row whose cached `artwork_path` column
    /// is empty (never populated, or indexed before art extraction) but whose
    /// folder carries a loose cover image must still show art on the
    /// now-playing panel — the fallback moved from `read_only_track_fields`
    /// (which no longer probes for library rows, see below) into
    /// `build_now_playing_info` itself, so display keeps working.
    #[test]
    fn info_falls_back_to_folder_image_for_library_row_with_empty_art_column() {
        gstreamer::init().ok();
        let dir = tempfile::tempdir().unwrap();
        let song_path = dir.path().join("song.mp3");
        // `Tag::write_to_path` opens the target for read-modify-write, so the
        // file must already exist on disk (unlike `NamedTempFile`, which
        // creates one for us).
        std::fs::write(&song_path, b"").unwrap();
        let mut tag = Tag::new();
        tag.set_title("Folder Art Song");
        tag.write_to_path(&song_path, Version::Id3v24).unwrap();
        // Loose cover image beside the track, no embedded APIC.
        std::fs::write(dir.path().join("cover.jpg"), b"fake-jpeg-bytes").unwrap();

        let lib_row = stub_lib_track(&song_path.to_string_lossy(), None);
        let info = build_now_playing_info(&song_path, Some(&lib_row), PlaySnapshot::default());
        assert!(
            info.artwork_path.is_some(),
            "now-playing panel should still fall back to the folder image \
             even though the library's art column is empty"
        );
    }

    /// Finding-2 regression: `read_only_track_fields` — called directly by the
    /// GTK ID3 editor to pre-fill its artwork entry, then embedded verbatim
    /// as APIC on save — must NOT probe folder/embedded art for library rows
    /// any more. Otherwise opening the editor on a library track that only
    /// has a loose folder image and saving any unrelated edit silently
    /// embeds that image into the file.
    #[test]
    fn read_only_track_fields_does_not_probe_art_for_library_rows() {
        gstreamer::init().ok();
        let dir = tempfile::tempdir().unwrap();
        let song_path = dir.path().join("song.mp3");
        // `Tag::write_to_path` opens the target for read-modify-write, so the
        // file must already exist on disk (unlike `NamedTempFile`, which
        // creates one for us).
        std::fs::write(&song_path, b"").unwrap();
        let mut tag = Tag::new();
        tag.set_title("Folder Art Song");
        tag.write_to_path(&song_path, Version::Id3v24).unwrap();
        std::fs::write(dir.path().join("cover.jpg"), b"fake-jpeg-bytes").unwrap();

        let lib_row = stub_lib_track(&song_path.to_string_lossy(), None);
        let rof = crate::media_library::read_only_track_fields(&song_path, Some(&lib_row));
        assert!(
            rof.artwork_path.is_empty(),
            "read_only_track_fields must not probe folder/embedded art for \
             indexed library rows — that would make the ID3 editor silently \
             embed a loose folder image on save"
        );
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
