//! Checking a server before it is added.

use crate::config::ServerConfig;

/// Why `new` cannot be added next to `existing`, in words for the user, or
/// `Ok` if it can.
pub fn validate_new_server(new: &ServerConfig, existing: &[ServerConfig]) -> Result<(), String> {
    let name = new.name.trim();
    if name.is_empty() {
        return Err("A server needs a name.".into());
    }
    if existing.iter().any(|s| s.name.trim().eq_ignore_ascii_case(name)) {
        return Err(format!("There is already a server called {name}."));
    }
    let lan = new.lan_url.as_deref().map(str::trim).filter(|u| !u.is_empty());
    let remote = new.remote_url.as_deref().map(str::trim).filter(|u| !u.is_empty());
    if lan.is_none() && remote.is_none() {
        return Err("Give at least one URL.".into());
    }
    if let Some(u) = lan {
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            return Err("The LAN URL must start with http:// or https://.".into());
        }
    }
    if let Some(u) = remote {
        if !u.starts_with("https://") {
            return Err("The remote URL must use https: plain HTTP is only allowed on the LAN URL.".into());
        }
    }
    if new.username.trim().is_empty() {
        return Err("A username is needed.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(name: &str, lan: Option<&str>, remote: Option<&str>) -> ServerConfig {
        ServerConfig {
            name: name.into(),
            lan_url: lan.map(str::to_string),
            remote_url: remote.map(str::to_string),
            username: "me".into(),
            ..ServerConfig::new(name)
        }
    }

    #[test]
    fn a_server_with_one_good_url_is_fine() {
        assert_eq!(validate_new_server(&server("oscar", Some("http://oscar.local:4533"), None), &[]), Ok(()));
        assert_eq!(validate_new_server(&server("oscar", None, Some("https://music.example.com")), &[]), Ok(()));
    }

    #[test]
    fn each_problem_is_named() {
        let e = |s: ServerConfig| validate_new_server(&s, &[]).unwrap_err();
        assert!(e(server(" ", Some("http://x"), None)).contains("name"));
        assert!(e(server("oscar", None, None)).contains("URL"));
        assert!(e(server("oscar", Some("oscar.local"), None)).contains("http"));
        assert!(e(server("oscar", None, Some("http://music.example.com"))).contains("https"));
        let mut no_user = server("oscar", Some("http://x"), None);
        no_user.username = String::new();
        assert!(validate_new_server(&no_user, &[]).unwrap_err().contains("username"));
    }

    #[test]
    fn names_must_be_unique_ignoring_case() {
        let existing = [server("Oscar", Some("http://a"), None)];
        assert!(validate_new_server(&server("oscar", Some("http://b"), None), &existing)
            .unwrap_err()
            .contains("already"));
    }
}
