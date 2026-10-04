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

/// A warning for a plain-HTTP address that leaves the home network, or
/// `None`. Over plain HTTP anyone along the way can read the sign-in token,
/// and a token keeps working for as long as the password does. That is fine
/// on a home network, on this machine, or over a private network such as
/// Tailscale; across the internet it is not. Not an error: the user decides.
pub fn home_address_warning(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") {
        return None;
    }
    if is_private_host(&url_host(rest)) {
        return None;
    }
    Some(
        "This is plain HTTP to an address outside your home network, so anyone along the way \
         could read your sign-in and use it. Use an https:// address instead."
            .into(),
    )
}

/// The host of the part of a URL after `scheme://`, lowercased, without
/// user info, port or IPv6 brackets.
fn url_host(rest: &str) -> String {
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host_port.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => host_port.split(':').next().unwrap_or(""),
    };
    host.to_ascii_lowercase()
}

/// Whether traffic to `host` stays off the open internet: this machine, a
/// private or link-local range, Tailscale's range and names, or a name only
/// a home network resolves (`.local`, `.lan`, `.home.arpa`, a bare name).
fn is_private_host(host: &str) -> bool {
    use std::net::IpAddr;
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            let [a, b, ..] = ip.octets();
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                // 100.64/10, carrier-grade NAT: where Tailscale puts devices.
                || (a == 100 && (64..128).contains(&b))
        }
        Ok(IpAddr::V6(ip)) => {
            let first = ip.segments()[0];
            ip.is_loopback()
                || (first & 0xfe00) == 0xfc00 // unique local, fc00::/7
                || (first & 0xffc0) == 0xfe80 // link local, fe80::/10
        }
        Err(_) => {
            !host.contains('.')
                || [".local", ".lan", ".home.arpa", ".internal", ".ts.net"]
                    .iter()
                    .any(|suffix| host.ends_with(suffix))
        }
    }
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

    /// Plain HTTP shows the sign-in token to anyone on the path, and the token
    /// keeps working. Fine at home or over a private network, not across the
    /// internet.
    #[test]
    fn plain_http_is_flagged_only_when_it_leaves_the_home_network() {
        for fine in [
            "http://192.168.1.137:4533",
            "http://10.0.0.5",
            "http://172.20.1.1:4533/navidrome",
            "http://oscar.local:4533",
            "http://oscar:4533",
            "http://localhost:4533",
            "http://127.0.0.1:4533",
            "http://100.101.102.103:4533",
            "http://oscar.tail1234.ts.net",
            "http://nas.lan",
            "http://nas.home.arpa",
            "http://[::1]:4533",
            "http://[fd12:3456::1]:4533",
            "https://music.example.com",
        ] {
            assert_eq!(home_address_warning(fine), None, "{fine}");
        }
        for exposed in ["http://music.example.com", "http://203.0.113.9:4533", "http://172.32.0.1", "HTTP://Music.Example.com"] {
            let w = home_address_warning(exposed).unwrap_or_else(|| panic!("{exposed}"));
            assert!(w.contains("plain HTTP"), "{w}");
        }
    }

    #[test]
    fn names_must_be_unique_ignoring_case() {
        let existing = [server("Oscar", Some("http://a"), None)];
        assert!(validate_new_server(&server("oscar", Some("http://b"), None), &existing)
            .unwrap_err()
            .contains("already"));
    }
}
