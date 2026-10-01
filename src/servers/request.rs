//! Building Subsonic request URLs, and keeping their secrets out of text.
//!
//! Every request URL carries credentials in its query string: `u`, `t` and
//! `s` for token auth, or `apiKey`. So a URL is never logged, stored or shown.
//! [`redact_url`] is what error messages and logs use instead.

use super::auth;
use crate::now_playing::percent_encode_query as enc;

/// The Subsonic API version Sparkamp speaks. Navidrome reports 1.16.1.
pub const API_VERSION: &str = "1.16.1";
/// The client name servers record for Sparkamp's requests.
pub const CLIENT_NAME: &str = "sparkamp";

/// How a request proves who it is.
#[derive(Clone, PartialEq, Eq)]
pub enum Credentials {
    /// Classic token auth: username plus password, sent as a salted MD5.
    Password { username: String, password: String },
    /// OpenSubsonic `apiKeyAuthentication`. The key identifies the user, so
    /// no username is sent (sending one is error 43).
    ApiKey(String),
}

// Hand-written so a stray `{:?}` never prints a password.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credentials::Password { username, .. } => {
                write!(f, "Credentials::Password {{ username: {username:?}, .. }}")
            }
            Credentials::ApiKey(_) => write!(f, "Credentials::ApiKey(..)"),
        }
    }
}

/// The full request URL for `endpoint` on the server at `base`.
///
/// `salt` is passed in rather than generated here so tests can pin it;
/// callers use [`new_salt`].
pub fn build_url(
    base: &str,
    endpoint: &str,
    creds: &Credentials,
    salt: &str,
    params: &[(&str, &str)],
) -> String {
    let mut url = format!("{}/rest/{endpoint}?", base.trim_end_matches('/'));
    match creds {
        Credentials::Password { username, password } => {
            url.push_str(&format!(
                "u={}&t={}&s={}",
                enc(username),
                auth::token(password, salt),
                enc(salt)
            ));
        }
        Credentials::ApiKey(key) => url.push_str(&format!("apiKey={}", enc(key))),
    }
    url.push_str(&format!("&v={API_VERSION}&c={CLIENT_NAME}&f=json"));
    for (k, v) in params {
        url.push_str(&format!("&{}={}", enc(k), enc(v)));
    }
    url
}

/// A fresh random salt for one request.
pub fn new_salt() -> String {
    use rand::Rng;
    let n: u64 = rand::thread_rng().r#gen();
    format!("{n:016x}")
}

/// `url` with its query string removed: safe to log or show.
pub fn redact_url(url: &str) -> String {
    match url.split_once('?') {
        Some((before, _)) => before.to_string(),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pw() -> Credentials {
        Credentials::Password { username: "me".into(), password: "sesame".into() }
    }

    #[test]
    fn password_request_carries_username_token_and_salt() {
        assert_eq!(
            build_url("http://oscar.local:4533", "ping", &pw(), "c19b2d", &[]),
            "http://oscar.local:4533/rest/ping?u=me&t=26719a1196d2a940705a59634eb18eab\
             &s=c19b2d&v=1.16.1&c=sparkamp&f=json"
        );
    }

    #[test]
    fn base_path_and_trailing_slash_are_kept_and_params_are_encoded() {
        assert_eq!(
            build_url(
                "https://music.example.com/navidrome/",
                "search3",
                &pw(),
                "c19b2d",
                &[("query", ""), ("songCount", "500"), ("musicFolderId", "a b&c")],
            ),
            "https://music.example.com/navidrome/rest/search3?u=me\
             &t=26719a1196d2a940705a59634eb18eab&s=c19b2d&v=1.16.1&c=sparkamp&f=json\
             &query=&songCount=500&musicFolderId=a%20b%26c"
        );
    }

    #[test]
    fn api_key_request_sends_the_key_and_no_username() {
        assert_eq!(
            build_url(
                "https://music.example.com",
                "ping",
                &Credentials::ApiKey("nav_abc".into()),
                "unused",
                &[],
            ),
            "https://music.example.com/rest/ping?apiKey=nav_abc&v=1.16.1&c=sparkamp&f=json"
        );
    }

    #[test]
    fn redacted_url_keeps_where_the_request_went_and_drops_the_query() {
        assert_eq!(
            redact_url("https://music.example.com/rest/search3?u=me&t=abc&s=def"),
            "https://music.example.com/rest/search3"
        );
    }

    #[test]
    fn debug_output_never_contains_the_password_or_key() {
        let shown = format!("{:?} {:?}", pw(), Credentials::ApiKey("nav_abc".into()));
        assert!(!shown.contains("sesame") && !shown.contains("nav_abc"), "{shown}");
    }
}
