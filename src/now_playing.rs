//! Now-playing data assembly — pure, UI-agnostic.  Frontends render the
//! `NowPlayingInfo` this module builds; they compute no metadata of their own.

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
