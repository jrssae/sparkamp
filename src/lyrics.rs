//! View/Search-lyrics decision (F15). One place decides Show-vs-Search and
//! builds the search URL so the query can never drift from the row label the
//! user right-clicked.

// Consumed by the FFI entry point and every frontend in the following phase-12
// tasks; until the first consumer lands the bin target sees the whole module as
// dead. Removed once `sparkamp_lyrics_action` (Task 2) references it.
#![allow(dead_code)]

use std::path::Path;

#[derive(Debug, Clone)]
pub enum LyricsAction {
    /// Non-empty USLT text read fresh from the file (multi-line preserved).
    Show(String),
    /// DuckDuckGo search URL to hand to the default browser.
    Search(String),
}

/// Fresh-read a track's USLT. Non-empty → `Show`; empty/unreadable → `Search`.
/// The Media Library row may be stale, so this always re-reads from disk.
pub fn lyrics_action(path: &Path, artist: &str, title: &str, album_artist: &str) -> LyricsAction {
    let lyric = crate::id3_editor::read_tag_fields(path).lyric;
    if lyric.trim().is_empty() {
        LyricsAction::Search(lyrics_search_url(artist, title, album_artist))
    } else {
        LyricsAction::Show(lyric)
    }
}

/// `https://duckduckgo.com/?q=<enc>` where the query is `"{artist} - {title} lyrics"`,
/// or `"{title} lyrics"` when no artist is available. Mirrors the row display
/// fallback (artist → album_artist → none).
pub fn lyrics_search_url(artist: &str, title: &str, album_artist: &str) -> String {
    let a = query_artist(artist, album_artist);
    let query = if a.is_empty() {
        format!("{title} lyrics")
    } else {
        format!("{a} - {title} lyrics")
    };
    // Reuse the now-playing Wikipedia encoder so both URLs share one encoding
    // (space → %20, unreserved-set only) and can never drift.
    format!(
        "https://duckduckgo.com/?q={}",
        crate::now_playing::percent_encode_query(&query)
    )
}

/// artist, else album_artist, else "" — same precedence as the display label
/// (keep in sync with the row-display fallback in `src/model.rs`).
fn query_artist(artist: &str, album_artist: &str) -> String {
    if !artist.trim().is_empty() {
        artist.to_string()
    } else if !album_artist.trim().is_empty() {
        album_artist.to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    #[test]
    fn url_has_artist_dash_title_lyrics() {
        let u = lyrics_search_url("Miles Davis", "So What", "");
        assert_eq!(u, "https://duckduckgo.com/?q=Miles%20Davis%20-%20So%20What%20lyrics");
    }

    #[test]
    fn url_falls_back_to_album_artist_when_artist_blank() {
        let u = lyrics_search_url("", "So What", "Miles Davis");
        assert_eq!(u, "https://duckduckgo.com/?q=Miles%20Davis%20-%20So%20What%20lyrics");
    }

    #[test]
    fn url_omits_dash_when_no_artist() {
        // No artist and no album_artist → just "<title> lyrics", no leading "- ".
        let u = lyrics_search_url("", "So What", "");
        assert_eq!(u, "https://duckduckgo.com/?q=So%20What%20lyrics");
    }

    #[test]
    fn url_percent_encodes_ampersand_and_unicode() {
        let u = lyrics_search_url("AC/DC", "Café & Cream", "");
        // '/', '&', space, and non-ASCII must all be percent-encoded.
        assert_eq!(
            u,
            "https://duckduckgo.com/?q=AC%2FDC%20-%20Caf%C3%A9%20%26%20Cream%20lyrics"
        );
    }

    #[test]
    fn action_shows_uslt_when_present_multiline_preserved() {
        // SAME temp-mp3 idiom as src/id3_editor.rs tests: a NamedTempFile with a
        // minimal MPEG frame header, then write USLT via the existing writer.
        let mut f = tempfile::NamedTempFile::with_suffix(".mp3").unwrap();
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        let mut fields = crate::id3_editor::read_tag_fields(f.path());
        fields.lyric = "line one\nline two".to_string();
        crate::id3_editor::write_tag_fields(f.path(), &fields).unwrap();

        match lyrics_action(f.path(), "A", "T", "") {
            LyricsAction::Show(text) => assert_eq!(text, "line one\nline two"),
            other => panic!("expected Show, got {other:?}"),
        }
    }

    #[test]
    fn action_searches_when_no_uslt() {
        let mut f = tempfile::NamedTempFile::with_suffix(".mp3").unwrap();
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        match lyrics_action(f.path(), "Miles Davis", "So What", "") {
            LyricsAction::Search(u) => {
                assert_eq!(u, "https://duckduckgo.com/?q=Miles%20Davis%20-%20So%20What%20lyrics");
            }
            other => panic!("expected Search, got {other:?}"),
        }
    }

    #[test]
    fn action_searches_when_path_unreadable() {
        // A missing file must degrade to Search, never panic.
        match lyrics_action(Path::new("/no/such/file.mp3"), "A", "T", "") {
            LyricsAction::Search(_) => {}
            other => panic!("expected Search, got {other:?}"),
        }
    }
}
