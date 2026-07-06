//! gnudb.org (CDDB protocol level 6) client: `query` and `read`.
//!
//! Plain-HTTP GETs against `cddb.cgi` via `minreq` (the endpoints have no
//! TLS, which keeps the dependency tree tiny). Every response parser is a
//! pure `&str` function, unit-tested offline; only [`query`]/[`read`] touch
//! the network, and a `#[ignore]`d live test exercises the real service.
//!
//! Protocol notes (cddb howto):
//! - The `hello` parameter is four `+`-joined fields
//!   `username+hostname+clientname+version` — the configured email is split
//!   at its last `@` for the first two; never send the whole address as one
//!   field.
//! - `proto=6` selects UTF-8.
//! - Query response codes: `200` exact match, `210`/`211` match list
//!   (exact/inexact), `202` none, `403` database entry corrupt.
//! - Read response: `210` followed by the xmcd body, terminated by `.`.

use super::{discid, DiscToc};
use serde::{Deserialize, Serialize};

const BASE_URL: &str = "http://gnudb.gnudb.org/~cddb/cddb.cgi";
const TIMEOUT_SECS: u64 = 10;

/// One disc the server proposed for our TOC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscMatch {
    /// gnudb category (one of the fixed CDDB set, e.g. "rock", "misc").
    pub category: String,
    /// The matched entry's 8-hex disc ID (can differ from ours on inexact).
    pub discid: String,
    /// "Artist / Album" display title.
    pub title: String,
    /// True when the server called the match exact (200/210).
    pub exact: bool,
}

/// Why a gnudb call failed — split so the frontends can phrase it honestly
/// ("couldn't reach gnudb" vs "gnudb answered something unexpected").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GnudbError {
    /// Network-level failure (offline, DNS, timeout).
    Offline(String),
    /// The server answered, but not with anything usable.
    Protocol(String),
}

impl std::fmt::Display for GnudbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GnudbError::Offline(e) => write!(f, "couldn't reach gnudb: {e}"),
            GnudbError::Protocol(e) => write!(f, "unexpected gnudb reply: {e}"),
        }
    }
}

// ─────────────────────────── request building ───────────────────────────

/// The CDDB `hello` value from the configured email:
/// `sparkamp@fastmail.com` → `sparkamp+fastmail.com+Sparkamp+<version>`.
/// No `@` → the whole string as username with `localhost` as host.
pub(crate) fn hello_param(email: &str) -> String {
    let (user, host) = match email.rsplit_once('@') {
        Some((u, h)) if !u.is_empty() && !h.is_empty() => (u, h),
        _ => (email, "localhost"),
    };
    format!("{user}+{host}+Sparkamp+{}", env!("CARGO_PKG_VERSION"))
}

/// Full query URL for a TOC: `cmd=cddb+query+<discid>+<n>+<off…>+<nsecs>`.
pub(crate) fn query_url(toc: &DiscToc, email: &str) -> String {
    let cmd = format!("cddb query {}", discid::query_args(toc)).replace(' ', "+");
    format!(
        "{BASE_URL}?cmd={cmd}&hello={}&proto=6",
        hello_param(email)
    )
}

/// Full read URL for one matched entry.
pub(crate) fn read_url(category: &str, discid: &str, email: &str) -> String {
    format!(
        "{BASE_URL}?cmd=cddb+read+{category}+{discid}&hello={}&proto=6",
        hello_param(email)
    )
}

// ─────────────────────────── response parsing ───────────────────────────

/// Parse a `cddb query` response body into the proposed matches.
/// `202` (no match) is an empty list, not an error.
pub(crate) fn parse_query_response(body: &str) -> Result<Vec<DiscMatch>, GnudbError> {
    let mut lines = body.lines();
    let first = lines.next().unwrap_or("").trim_end();
    let code = first.split_whitespace().next().unwrap_or("");

    match code {
        // Single exact match: "200 <categ> <discid> <artist / album>"
        "200" => Ok(parse_match_line(first.trim_start_matches("200").trim(), true)
            .into_iter()
            .collect()),
        // Match list follows, "." terminated. 210 = exact list, 211 = inexact.
        "210" | "211" => {
            let exact = code == "210";
            Ok(lines
                .take_while(|l| l.trim() != ".")
                .filter_map(|l| parse_match_line(l.trim(), exact))
                .collect())
        }
        "202" => Ok(Vec::new()),
        "403" => Err(GnudbError::Protocol(
            "database entry is corrupt (403)".to_string(),
        )),
        _ => Err(GnudbError::Protocol(first.to_string())),
    }
}

/// "<categ> <discid> <artist / album>" → a [`DiscMatch`].
fn parse_match_line(line: &str, exact: bool) -> Option<DiscMatch> {
    let mut parts = line.splitn(3, ' ');
    let category = parts.next()?.trim();
    let discid = parts.next()?.trim();
    let title = parts.next().unwrap_or("").trim();
    if category.is_empty() || discid.len() != 8 {
        return None;
    }
    Some(DiscMatch {
        category: category.to_string(),
        discid: discid.to_string(),
        title: title.to_string(),
        exact,
    })
}

/// Parse a `cddb read` response: `210 …` header, xmcd body, `.` terminator.
/// Returns the raw xmcd text (parsed further by [`super::xmcd`]).
pub(crate) fn parse_read_response(body: &str) -> Result<String, GnudbError> {
    let mut lines = body.lines();
    let first = lines.next().unwrap_or("").trim_end();
    if !first.starts_with("210") {
        return Err(GnudbError::Protocol(first.to_string()));
    }
    let xmcd: Vec<&str> = lines.take_while(|l| l.trim() != ".").collect();
    Ok(xmcd.join("\n"))
}

// ─────────────────────────── network entry points ───────────────────────────

fn http_get(url: &str) -> Result<String, GnudbError> {
    let resp = minreq::get(url)
        .with_timeout(TIMEOUT_SECS)
        .send()
        .map_err(|e| GnudbError::Offline(e.to_string()))?;
    if resp.status_code != 200 {
        return Err(GnudbError::Protocol(format!(
            "HTTP {} {}",
            resp.status_code, resp.reason_phrase
        )));
    }
    resp.as_str()
        .map(|s| s.to_string())
        .map_err(|e| GnudbError::Protocol(e.to_string()))
}

/// Ask gnudb which discs match this TOC.
pub fn query(toc: &DiscToc, email: &str) -> Result<Vec<DiscMatch>, GnudbError> {
    parse_query_response(&http_get(&query_url(toc, email))?)
}

/// Fetch one matched entry's xmcd record.
pub fn read(category: &str, discid: &str, email: &str) -> Result<String, GnudbError> {
    parse_read_response(&http_get(&read_url(category, discid, email))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disc::TocTrack;

    fn sample_toc() -> DiscToc {
        DiscToc {
            tracks: [150u32, 13834, 30216, 44337, 59560, 73612, 97120, 110977]
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
    fn hello_splits_email_at_last_at() {
        let h = hello_param("sparkamp@fastmail.com");
        assert!(h.starts_with("sparkamp+fastmail.com+Sparkamp+"));
        // Weird-but-legal quoted local part with an @ inside: split at LAST @.
        let h = hello_param("a@b@example.org");
        assert!(h.starts_with("a@b+example.org+Sparkamp+"));
    }

    #[test]
    fn hello_without_at_falls_back_to_localhost() {
        assert!(hello_param("nobody").starts_with("nobody+localhost+Sparkamp+"));
    }

    #[test]
    fn query_url_shape() {
        let url = query_url(&sample_toc(), "sparkamp@fastmail.com");
        assert!(url.starts_with(
            "http://gnudb.gnudb.org/~cddb/cddb.cgi?cmd=cddb+query+6f067d08+8+150+"
        ));
        assert!(url.contains("+110977+1663&hello=sparkamp+fastmail.com+Sparkamp+"));
        assert!(url.ends_with("&proto=6"));
    }

    #[test]
    fn parses_200_exact() {
        let body = "200 rock 6f067d08 Some Artist / Some Album\r\n";
        let m = parse_query_response(body).unwrap();
        assert_eq!(m.len(), 1);
        assert!(m[0].exact);
        assert_eq!(m[0].category, "rock");
        assert_eq!(m[0].discid, "6f067d08");
        assert_eq!(m[0].title, "Some Artist / Some Album");
    }

    #[test]
    fn parses_211_inexact_list() {
        let body = "211 Found inexact matches, list follows (until terminating `.')\n\
                    rock 6f067d08 Artist A / Album A\n\
                    misc 6f067d09 Artist B / Album B\n\
                    .\n";
        let m = parse_query_response(body).unwrap();
        assert_eq!(m.len(), 2);
        assert!(!m[0].exact);
        assert_eq!(m[1].category, "misc");
    }

    #[test]
    fn parses_202_as_empty() {
        assert!(parse_query_response("202 No match found\n")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn rejects_403_and_garbage() {
        assert!(parse_query_response("403 Database entry is corrupt\n").is_err());
        assert!(parse_query_response("500 whatever\n").is_err());
    }

    #[test]
    fn read_response_strips_header_and_terminator() {
        let body = "210 rock 6f067d08 CD database entry follows\n\
                    # xmcd\n\
                    DISCID=6f067d08\n\
                    DTITLE=Artist / Album\n\
                    .\n";
        let xmcd = parse_read_response(body).unwrap();
        assert!(xmcd.starts_with("# xmcd"));
        assert!(xmcd.ends_with("DTITLE=Artist / Album"));
        assert!(!xmcd.contains("210 "));
    }

    /// Live query against gnudb with the real test disc's TOC — run with
    /// `cargo test --lib live_gnudb -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_gnudb_query_real_disc() {
        match query(&sample_toc(), "sparkamp@fastmail.com") {
            Ok(matches) => {
                println!("{} match(es):", matches.len());
                for m in &matches {
                    println!(
                        "  [{}] {} {} — {}",
                        if m.exact { "exact" } else { "inexact" },
                        m.category,
                        m.discid,
                        m.title
                    );
                }
                if let Some(m) = matches.first() {
                    let xmcd = read(&m.category, &m.discid, "sparkamp@fastmail.com")
                        .expect("read should succeed for a query match");
                    println!("--- xmcd ({} bytes) ---\n{}", xmcd.len(), xmcd);
                }
            }
            Err(e) => println!("gnudb error: {e}"),
        }
    }
}
