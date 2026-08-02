//! Lyrics view assembly (F15). One place builds the marquee title, the
//! DuckDuckGo search URL, and the USLT body so no frontend re-derives any of
//! them and the search query can never drift from the row label the user
//! right-clicked. The lyrics WINDOW always opens (even with no saved lyrics);
//! the search is an in-window affordance, not an alternate code path.

use std::path::Path;

/// Everything the lyrics window needs to render one track.
#[derive(Debug, Clone)]
pub struct LyricsView {
    /// Marquee identifier for the title bar (`<artist> - <track>`).
    pub title: String,
    /// Saved USLT text (multi-line preserved), or `None` when the file has no
    /// lyrics — the window shows "No lyrics available" in that case.
    pub body: Option<String>,
    /// DuckDuckGo search URL for the in-window "Search" button.
    pub search_url: String,
}

/// Assemble the lyrics view for one track: fresh-read the USLT (the Media
/// Library row may be stale), and precompute the title + search URL.
pub fn lyrics_view(path: &Path, artist: &str, title: &str, album_artist: &str) -> LyricsView {
    let raw = crate::id3_editor::read_tag_fields(path).lyric;
    let body = if raw.trim().is_empty() { None } else { Some(raw) };
    LyricsView {
        title: lyrics_display_title(artist, title, album_artist, path),
        body,
        search_url: lyrics_search_url(artist, title, album_artist, path),
    }
}

/// The effective artist: `artist` (TPE1), else `album_artist` (TPE2), else "".
/// Trim-aware so a whitespace-only field counts as absent. Mirrors the
/// marquee's `Track::display_name` precedence (src/model.rs).
fn eff_artist<'a>(artist: &'a str, album_artist: &'a str) -> &'a str {
    if !artist.trim().is_empty() {
        artist.trim()
    } else if !album_artist.trim().is_empty() {
        album_artist.trim()
    } else {
        ""
    }
}

/// The file stem (no extension) as a display fallback, "?" if unreadable.
fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string()
}

/// Marquee identifier for the window title: `"<artist> - <track>"`, artist
/// falling back to album_artist, track falling back to the filename stem, and
/// the whole thing collapsing to just the track when no artist is available.
/// Same precedence as the scrolling marquee (`Track::display_name`).
pub fn lyrics_display_title(artist: &str, title: &str, album_artist: &str, path: &Path) -> String {
    let a = eff_artist(artist, album_artist);
    let t = if title.trim().is_empty() {
        file_stem(path)
    } else {
        title.trim().to_string()
    };
    if a.is_empty() {
        t
    } else {
        format!("{a} - {t}")
    }
}

/// `https://duckduckgo.com/?q=<enc>` where the query is `"<artist> <track> lyrics"`
/// (SPACE-separated). Artist falls back to album_artist; when BOTH artist and
/// track are absent the query is `"<filename> lyrics"`.
pub fn lyrics_search_url(artist: &str, title: &str, album_artist: &str, path: &Path) -> String {
    let a = eff_artist(artist, album_artist);
    let t = title.trim();
    let core = if a.is_empty() && t.is_empty() {
        file_stem(path)
    } else {
        // Join the non-empty terms with a single space.
        [a, t]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let query = format!("{core} lyrics");
    // Reuse the now-playing encoder so DDG + Wikipedia URLs share one encoding
    // (space → %20, unreserved-set only) and can never drift.
    format!(
        "https://duckduckgo.com/?q={}",
        crate::now_playing::percent_encode_query(&query)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    fn p(s: &str) -> &Path {
        Path::new(s)
    }

    #[test]
    fn title_is_artist_dash_track() {
        assert_eq!(
            lyrics_display_title("Miles Davis", "So What", "", p("/x/y.mp3")),
            "Miles Davis - So What"
        );
    }

    #[test]
    fn title_falls_back_to_album_artist() {
        assert_eq!(
            lyrics_display_title("", "So What", "Coltrane", p("/x/y.mp3")),
            "Coltrane - So What"
        );
    }

    #[test]
    fn title_falls_back_to_filename_when_both_blank() {
        // No artist, no album_artist, no title → just the filename stem.
        assert_eq!(
            lyrics_display_title("", "", "", p("/music/track99.mp3")),
            "track99"
        );
    }

    #[test]
    fn title_no_artist_is_track_only() {
        assert_eq!(
            lyrics_display_title("", "So What", "", p("/x/y.mp3")),
            "So What"
        );
    }

    #[test]
    fn search_is_space_separated_with_lyrics_suffix() {
        // Space between artist and track (NOT the old " - " dash), "lyrics" suffix.
        let u = lyrics_search_url("Miles Davis", "So What", "", p("/x/y.mp3"));
        assert_eq!(u, "https://duckduckgo.com/?q=Miles%20Davis%20So%20What%20lyrics");
    }

    #[test]
    fn search_uses_album_artist_when_no_artist() {
        let u = lyrics_search_url("", "So What", "Coltrane", p("/x/y.mp3"));
        assert_eq!(u, "https://duckduckgo.com/?q=Coltrane%20So%20What%20lyrics");
    }

    #[test]
    fn search_uses_filename_when_artist_and_title_blank() {
        let u = lyrics_search_url("", "", "", p("/music/track99.mp3"));
        assert_eq!(u, "https://duckduckgo.com/?q=track99%20lyrics");
    }

    #[test]
    fn search_percent_encodes_specials() {
        let u = lyrics_search_url("AC/DC", "Café & Cream", "", p("/x/y.mp3"));
        assert_eq!(
            u,
            "https://duckduckgo.com/?q=AC%2FDC%20Caf%C3%A9%20%26%20Cream%20lyrics"
        );
    }

    #[test]
    fn view_body_some_when_uslt_present() {
        let mut f = tempfile::NamedTempFile::with_suffix(".mp3").unwrap();
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        let mut fields = crate::id3_editor::read_tag_fields(f.path());
        fields.lyric = "line one\nline two".to_string();
        crate::id3_editor::write_tag_fields(f.path(), &fields).unwrap();

        let v = lyrics_view(f.path(), "A", "T", "");
        assert_eq!(v.body.as_deref(), Some("line one\nline two"));
        assert_eq!(v.title, "A - T");
    }

    #[test]
    fn view_body_none_when_no_uslt() {
        let mut f = tempfile::NamedTempFile::with_suffix(".mp3").unwrap();
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        let v = lyrics_view(f.path(), "Miles Davis", "So What", "");
        assert_eq!(v.body, None);
        assert_eq!(
            v.search_url,
            "https://duckduckgo.com/?q=Miles%20Davis%20So%20What%20lyrics"
        );
    }

    #[test]
    fn view_body_none_when_path_unreadable() {
        // A missing file degrades to "no lyrics", never panics.
        let v = lyrics_view(Path::new("/no/such/file.mp3"), "A", "T", "");
        assert_eq!(v.body, None);
    }
}
