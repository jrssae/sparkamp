//! Shared formatter for the active-playlist status line (phase 7).
//! `N tracks · MM:SS total · MM:SS selected` — the selected clause is present
//! only when `selected_secs` is `Some` (frontends pass it when ≥1 row is
//! selected). Durations roll over to H:MM:SS above one hour.

/// Format a duration as `M:SS` under an hour, `H:MM:SS` at/above an hour.
fn fmt_hms(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn playlist_status_line(count: usize, total_secs: u64, selected_secs: Option<u64>) -> String {
    let noun = if count == 1 { "track" } else { "tracks" };
    let mut line = format!("{count} {noun} · {} total", fmt_hms(total_secs));
    if let Some(sel) = selected_secs {
        line.push_str(&format!(" · {} selected", fmt_hms(sel)));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::playlist_status_line;

    #[test]
    fn singular_plural_and_no_selection() {
        assert_eq!(playlist_status_line(1, 65, None), "1 track · 1:05 total");
        assert_eq!(playlist_status_line(12, 2900, None), "12 tracks · 48:20 total");
    }

    #[test]
    fn selected_clause_and_hour_rollover() {
        assert_eq!(
            playlist_status_line(12, 3665, Some(664)),
            "12 tracks · 1:01:05 total · 11:04 selected"
        );
        assert_eq!(playlist_status_line(0, 0, None), "0 tracks · 0:00 total");
    }
}
