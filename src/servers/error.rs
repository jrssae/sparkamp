//! What can go wrong talking to a server, sorted by what Sparkamp should do
//! about it.
//!
//! The `Display` text of every variant is safe to log and show: it never
//! contains a request URL's query string, which is where credentials live.

/// A failed server call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerError {
    /// Could not reach the server: timeout, refused, DNS, TLS. Offline.
    Unreachable(String),
    /// The server answered with a non-success HTTP status. A 5xx, or a
    /// reverse proxy's maintenance page, means offline.
    Http(u16),
    /// The answer was not a Subsonic response: a captive portal, a proxy
    /// error page, the wrong URL. Treated as offline.
    NotSubsonic,
    /// The server refused the credentials (Subsonic 40, 41, 43, 44). Needs
    /// the user; automatic tries stop until the credentials change.
    Auth { code: u32, message: String },
    /// Any other Subsonic error, e.g. 70 "not found".
    Api { code: u32, message: String },
    /// HTTP 401 or 403 that is not a Subsonic answer: something in front of
    /// the server (a reverse proxy, a firewall, Cloudflare) turned the
    /// request away. `said` is the start of what it answered, cleaned up.
    /// Needs the user, but is not a wrong password.
    Refused { code: u16, said: String },
}

impl ServerError {
    /// An [`ServerError::Unreachable`] from a transport's error text, with
    /// any URL query string cut out. HTTP libraries like to quote the URL
    /// they failed on, and ours carry credentials.
    pub fn unreachable(text: &str) -> Self {
        ServerError::Unreachable(strip_query_strings(text))
    }

    /// A [`ServerError::Refused`] quoting `body`: tags dropped, whitespace
    /// collapsed, query strings cut, at most 120 characters.
    pub fn refused(code: u16, body: &[u8]) -> Self {
        let text = String::from_utf8_lossy(body);
        let mut plain = String::new();
        let mut in_tag = false;
        for c in text.chars() {
            match c {
                '<' => {
                    in_tag = true;
                    plain.push(' ');
                }
                '>' => in_tag = false,
                c if !in_tag => plain.push(c),
                _ => {}
            }
        }
        let said: String = strip_query_strings(&plain.split_whitespace().collect::<Vec<_>>().join(" "))
            .chars()
            .take(120)
            .collect();
        ServerError::Refused { code, said }
    }

    /// Whether this failure means "the server is not available right now",
    /// as opposed to something the user has to fix.
    pub fn is_offline(&self) -> bool {
        match self {
            ServerError::Unreachable(_) | ServerError::NotSubsonic => true,
            ServerError::Http(code) => (500..600).contains(code),
            ServerError::Auth { .. } | ServerError::Api { .. } | ServerError::Refused { .. } => false,
        }
    }
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerError::Unreachable(why) => write!(f, "server not reachable: {why}"),
            ServerError::Http(code) => write!(f, "server answered HTTP {code}"),
            ServerError::NotSubsonic => write!(f, "the answer was not from a Subsonic server"),
            ServerError::Auth { code, message } => write!(f, "sign-in failed ({code}): {message}"),
            ServerError::Api { code, message } => write!(f, "server error {code}: {message}"),
            ServerError::Refused { code, said } => {
                write!(f, "refused with HTTP {code}")?;
                if !said.is_empty() {
                    write!(f, " (\"{said}\")")?;
                }
                write!(
                    f,
                    ". Navidrome reports a wrong password differently, so a proxy, firewall or \
                     Cloudflare rule in front of the server likely turned the request away"
                )
            }
        }
    }
}

/// Remove every `?query` run from `text`. A query here is the characters our
/// request builder can emit after `?`: percent-encoded values joined by `=`
/// and `&`.
fn strip_query_strings(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_query = false;
    for c in text.chars() {
        if in_query {
            if c.is_ascii_alphanumeric() || "-_.~%=&+".contains(c) {
                continue;
            }
            in_query = false;
        }
        if c == '?' {
            in_query = true;
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreachable_5xx_and_non_subsonic_answers_count_as_offline() {
        assert!(ServerError::Unreachable("timed out".into()).is_offline());
        assert!(ServerError::Http(503).is_offline());
        assert!(ServerError::Http(502).is_offline());
        assert!(ServerError::NotSubsonic.is_offline());
    }

    #[test]
    fn auth_failures_and_client_errors_are_not_offline() {
        assert!(!ServerError::Auth { code: 40, message: "Wrong username or password".into() }
            .is_offline());
        assert!(!ServerError::Http(404).is_offline());
        assert!(!ServerError::Api { code: 70, message: "not found".into() }.is_offline());
    }

    #[test]
    fn unreachable_text_never_carries_a_query_string() {
        let e = ServerError::unreachable(
            "error sending request for http://oscar:4533/rest/ping?u=me&t=tok&s=salt: refused",
        );
        let shown = e.to_string();
        assert!(!shown.contains("t=tok") && !shown.contains("s=salt"), "{shown}");
        assert!(shown.contains("http://oscar:4533/rest/ping"), "{shown}");
        assert!(shown.contains("refused"), "{shown}");
    }

    #[test]
    fn display_is_plain_english() {
        assert_eq!(ServerError::Http(503).to_string(), "server answered HTTP 503");
        assert_eq!(
            ServerError::NotSubsonic.to_string(),
            "the answer was not from a Subsonic server"
        );
        assert_eq!(
            ServerError::Auth { code: 40, message: "Wrong username or password".into() }
                .to_string(),
            "sign-in failed (40): Wrong username or password"
        );
    }
}
