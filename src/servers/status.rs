//! Each server's health, and when its catalog is due for an update.
//!
//! Availability is tracked per server, never globally: one server failing
//! never blocks another, the UI, or playback of anything else. Offline is a
//! normal state, shown in one status-bar line per server and never in a
//! dialog.

use super::error::ServerError;
use std::time::{Duration, SystemTime};

/// What Sparkamp currently believes about one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// Not contacted yet this session.
    Unknown,
    Online,
    /// Could not be reached. `network_down` is the OS's hint that the
    /// machine itself is offline, which only changes the wording.
    Offline { network_down: bool },
    /// The server refused the credentials. Automatic tries stop until the
    /// user changes them, so a changed password is never hammered.
    SignInFailed,
    /// No password is stored for this server, so it is never contacted.
    NoPassword,
    /// Reachable, but its TLS certificate was refused.
    CertificateProblem,
    /// Reachable but scanning; the catalog pull waits.
    Scanning,
    /// Something in front of the server answered 401 or 403 instead of it:
    /// see [`ServerError::Refused`].
    Refused(u16),
    /// The address answered with a redirect, which is never followed: see
    /// [`ServerError::Redirected`].
    Redirected,
}

impl Health {
    /// The health a failed call implies.
    pub fn after_error(e: &ServerError, network_down: bool) -> Health {
        match e {
            ServerError::Auth { .. } => Health::SignInFailed,
            ServerError::Refused { code, .. } => Health::Refused(*code),
            ServerError::Redirected { .. } => Health::Redirected,
            ServerError::Unreachable(why)
                if why.to_ascii_lowercase().contains("certificate") =>
            {
                Health::CertificateProblem
            }
            e if e.is_offline() => Health::Offline { network_down },
            // It answered, just not with what was asked for.
            _ => Health::Online,
        }
    }

    /// Whether automatic contact (periodic update, queued sends) may try
    /// this server.
    pub fn allows_automatic_contact(&self) -> bool {
        !matches!(self, Health::SignInFailed | Health::NoPassword)
    }
}

/// Whether the periodic update is due. `last_success` is when the last
/// update completed; `None` (never, e.g. a server just added) is due. A
/// failed update does not move `last_success`, so it stays due and is
/// retried at the next trigger. An interval of 0 turns periodic updates off.
pub fn update_due(
    last_success: Option<SystemTime>,
    interval_hours: u32,
    now: SystemTime,
) -> bool {
    if interval_hours == 0 {
        return false;
    }
    match last_success {
        None => true,
        Some(t) => match now.duration_since(t) {
            Ok(age) => age >= Duration::from_secs(interval_hours as u64 * 3600),
            // The clock went backwards; waiting for it to catch up could
            // take forever.
            Err(_) => true,
        },
    }
}

/// The status-bar line for one server, e.g. `oscar: not responding, updated
/// 3h ago`.
/// How far a catalog download has got: songs received, and the server's
/// song count when it reports one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PullProgress {
    pub fetched: u64,
    pub total: Option<u64>,
}

/// The status line while a catalog download runs, in place of the usual one.
pub fn progress_line(name: &str, p: &PullProgress) -> String {
    match p.total {
        Some(total) => format!("{name}: getting the catalog, {} of {} songs", thousands(p.fetched), thousands(total)),
        None => format!("{name}: getting the catalog, {} songs so far", thousands(p.fetched)),
    }
}

/// `n` with commas between thousands: 37,243.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn status_line(name: &str, health: &Health, since_update: Option<Duration>) -> String {
    let refused;
    let state = match health {
        Health::Unknown | Health::Online => None,
        Health::Refused(code) => {
            refused = format!("refused (HTTP {code})");
            Some(refused.as_str())
        }
        Health::Offline { network_down: false } => Some("not responding"),
        Health::Offline { network_down: true } => Some("no network"),
        Health::SignInFailed => return format!("{name}: sign-in failed"),
        Health::NoPassword => return format!("{name}: no password stored (add it under Servers)"),
        Health::CertificateProblem => return format!("{name}: certificate problem"),
        Health::Redirected => {
            return format!("{name}: the address redirects elsewhere (change it under Servers)")
        }
        Health::Scanning => Some("scanning, update postponed"),
    };
    let age = match since_update {
        Some(d) => format!("updated {} ago", short_age(d)),
        None => "never updated".to_string(),
    };
    match state {
        Some(state) => format!("{name}: {state}, {age}"),
        None => format!("{name}: {age}"),
    }
}

fn short_age(d: Duration) -> String {
    let mins = d.as_secs() / 60;
    if mins < 60 {
        format!("{mins}m")
    } else if mins < 24 * 60 {
        format!("{}h", mins / 60)
    } else {
        format!("{}d", mins / (24 * 60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_759_000_000)
    }

    #[test]
    fn a_daily_update_is_due_once_a_day_has_passed() {
        assert!(!update_due(Some(t0()), 24, t0() + 23 * HOUR));
        assert!(update_due(Some(t0()), 24, t0() + 24 * HOUR));
    }

    #[test]
    fn a_server_never_updated_is_due() {
        assert!(update_due(None, 24, t0()));
    }

    #[test]
    fn an_interval_of_zero_means_never_automatically() {
        assert!(!update_due(Some(t0()), 0, t0() + 1000 * HOUR));
    }

    #[test]
    fn a_clock_set_backwards_does_not_block_updates_forever() {
        assert!(update_due(Some(t0() + 100 * HOUR), 24, t0()));
    }

    #[test]
    fn errors_map_to_what_the_user_should_see() {
        let down = ServerError::unreachable("timed out");
        assert_eq!(Health::after_error(&down, false), Health::Offline { network_down: false });
        assert_eq!(Health::after_error(&down, true), Health::Offline { network_down: true });
        assert_eq!(
            Health::after_error(&ServerError::Http(503), false),
            Health::Offline { network_down: false }
        );
        assert_eq!(
            Health::after_error(&ServerError::Auth { code: 40, message: "x".into() }, false),
            Health::SignInFailed
        );
        assert_eq!(
            Health::after_error(&ServerError::unreachable("invalid peer certificate: UnknownIssuer"), false),
            Health::CertificateProblem
        );
    }

    #[test]
    fn a_server_with_no_stored_password_says_so() {
        assert_eq!(
            status_line("oscar", &Health::NoPassword, None),
            "oscar: no password stored (add it under Servers)"
        );
        assert!(!Health::NoPassword.allows_automatic_contact());
    }

    #[test]
    fn a_refused_sign_in_stops_automatic_contact() {
        assert!(!Health::SignInFailed.allows_automatic_contact());
        assert!(Health::Offline { network_down: false }.allows_automatic_contact());
        assert!(Health::Unknown.allows_automatic_contact());
    }

    #[test]
    fn a_catalog_download_in_progress_reads_as_a_count() {
        let p = PullProgress { fetched: 324, total: Some(37_243) };
        assert_eq!(progress_line("oscar", &p), "oscar: getting the catalog, 324 of 37,243 songs");
        let p = PullProgress { fetched: 1_500, total: None };
        assert_eq!(progress_line("oscar", &p), "oscar: getting the catalog, 1,500 songs so far");
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000_000), "1,000,000");
    }

    #[test]
    fn a_refused_request_reads_as_refused_not_online() {
        let e = ServerError::Refused { code: 403, said: "error code: 1010".into() };
        assert_eq!(Health::after_error(&e, false), Health::Refused(403));
        assert_eq!(
            status_line("oscar", &Health::Refused(403), Some(HOUR)),
            "oscar: refused (HTTP 403), updated 1h ago"
        );
    }

    #[test]
    fn a_redirect_reads_as_a_wrong_address() {
        let e = ServerError::Redirected { to: "https://music.example.com/rest/ping".into() };
        assert_eq!(Health::after_error(&e, false), Health::Redirected);
        assert_eq!(
            status_line("oscar", &Health::Redirected, Some(HOUR)),
            "oscar: the address redirects elsewhere (change it under Servers)"
        );
    }

    #[test]
    fn status_lines_read_plainly() {
        assert_eq!(status_line("oscar", &Health::Online, Some(2 * HOUR)), "oscar: updated 2h ago");
        assert_eq!(
            status_line("oscar", &Health::Offline { network_down: false }, Some(3 * HOUR)),
            "oscar: not responding, updated 3h ago"
        );
        assert_eq!(
            status_line("oscar", &Health::Offline { network_down: true }, Some(Duration::from_secs(90))),
            "oscar: no network, updated 1m ago"
        );
        assert_eq!(status_line("oscar", &Health::SignInFailed, None), "oscar: sign-in failed");
        assert_eq!(
            status_line("oscar", &Health::Scanning, Some(26 * HOUR)),
            "oscar: scanning, update postponed, updated 1d ago"
        );
        assert_eq!(status_line("oscar", &Health::Unknown, None), "oscar: never updated");
        assert_eq!(status_line("oscar", &Health::CertificateProblem, None), "oscar: certificate problem");
    }
}
