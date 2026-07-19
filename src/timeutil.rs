//! Minimal date/time helpers — ISO-8601 (UTC, second precision) formatting
//! and parsing without a chrono dependency.
//!
//! Used by the media library for `last_scanned` / `last_played` timestamps.
//! Gregorian rules only; correct for all dates from 1970 onwards, which is
//! all a "when did this file get scanned" timestamp ever needs.

/// Parse an ISO 8601 timestamp (format: YYYY-MM-DDTHH:MM:SSZ) to Unix seconds.
#[allow(dead_code)] // bin-unreachable on macOS (callers are GTK/FFI-gated)
pub(crate) fn parse_iso_timestamp(ts: &str) -> Option<u64> {
    // Expected format: "2024-01-15T10:30:00Z"
    let ts = ts.strip_suffix('Z')?;
    let parts: Vec<&str> = ts.split(|c| c == '-' || c == 'T' || c == ':').collect();
    if parts.len() < 6 {
        return None;
    }

    let year: u64 = parts[0].parse().ok()?;
    let month: u64 = parts[1].parse().ok()?;
    let day: u64 = parts[2].parse().ok()?;
    let hour: u64 = parts[3].parse().ok()?;
    let min: u64 = parts[4].parse().ok()?;
    let sec: u64 = parts[5].parse().ok()?;

    // Validate ranges
    if month < 1 || month > 12 {
        return None;
    }
    if day < 1 || day > 31 {
        return None;
    }
    if hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    if day > days_in_month(year, month) {
        return None;
    }

    // Simple conversion to Unix timestamp (ignoring leap seconds and timezone)
    let days_since_epoch = days_since_1970(year, month, day);
    let secs = days_since_epoch as u64 * 86400 + hour * 3600 + min * 60 + sec;
    Some(secs)
}

/// Calculate days since 1970-01-01 (simplified, not accounting for Julian calendar)
#[allow(dead_code)] // bin-unreachable on macOS (callers are GTK/FFI-gated)
pub(crate) fn days_since_1970(year: u64, month: u64, day: u64) -> u64 {
    let mut days = (year - 1970) * 365;
    days += (year - 1969) / 4 - (year - 1901) / 100 + (year - 1601) / 400; // leap days
    for m in 1..month {
        days += days_in_month(year, m);
    }
    days + day - 1
}

/// Get days in a month
pub(crate) fn days_in_month(year: u64, month: u64) -> u64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Format an arbitrary `SystemTime` as ISO 8601 (YYYY-MM-DDTHH:MM:SSZ, UTC).
/// Shared by `format_current_timestamp` (below) and the scanner's file-mtime
/// capture, so `last_scanned`, `added_at`, and `file_mtime` all use one
/// formatter and stay comparable.
#[allow(dead_code)] // bin-unreachable on macOS (callers are GTK/FFI-gated)
pub(crate) fn format_system_time(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86400;
    let rem = secs % 86400;
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;

    // Find year, month, day from days since 1970
    let (year, month, day) = year_month_day_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, min, sec
    )
}

/// Get current timestamp in ISO 8601 format (YYYY-MM-DDTHH:MM:SSZ, UTC).
pub(crate) fn format_current_timestamp() -> String {
    format_system_time(std::time::SystemTime::now())
}

/// Convert days since 1970 to (year, month, day).
pub(crate) fn year_month_day_from_days(days: u64) -> (u64, u64, u64) {
    let mut year = 1970;
    let mut remaining_days = days;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }

    let mut month = 1;
    loop {
        let dim = days_in_month(year, month);
        if remaining_days < dim {
            return (year, month, remaining_days + 1);
        }
        remaining_days -= dim;
        month += 1;
    }
}

fn is_leap_year(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_iso_timestamp_valid() {
        let secs = parse_iso_timestamp("2024-01-15T10:30:00Z");
        assert!(secs.is_some());
        // Round-trips through the formatter's calendar math.
        let (y, m, d) = year_month_day_from_days(secs.unwrap() / 86400);
        assert_eq!((y, m, d), (2024, 1, 15));
    }

    #[test]
    fn parse_iso_timestamp_invalid() {
        assert!(parse_iso_timestamp("not-a-date").is_none());
        assert!(parse_iso_timestamp("2024-13-45T10:30:00Z").is_none()); // Invalid date
        assert!(parse_iso_timestamp("").is_none());
    }

    #[test]
    fn format_current_timestamp_round_trips() {
        let ts = format_current_timestamp();
        // "YYYY-MM-DDTHH:MM:SSZ" is exactly 20 chars and must parse back.
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert!(parse_iso_timestamp(&ts).is_some());
    }

    #[test]
    fn leap_year_february() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2023, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29); // divisible by 400
        assert_eq!(days_in_month(1900, 2), 28); // divisible by 100, not 400
    }
}
