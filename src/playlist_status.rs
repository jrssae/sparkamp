//! Shared formatter for the active-playlist / Media Library status line.
//! `N tracks · MM:SS total · K selected · MM:SS` — the selected clause (count +
//! duration) is present only when `selected` is `Some` (frontends pass it when
//! ≥1 row is selected). Durations roll over to H:MM:SS above one hour.

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

/// `selected`: `Some((count, secs))` for the selected rows, or `None` when
/// nothing is selected. The clause shows both the count and the duration.
pub fn playlist_status_line(
    count: usize,
    total_secs: u64,
    selected: Option<(usize, u64)>,
) -> String {
    let noun = if count == 1 { "track" } else { "tracks" };
    let mut line = format!("{count} {noun} · {} total", fmt_hms(total_secs));
    if let Some((sel_count, sel_secs)) = selected {
        line.push_str(&format!(" · {sel_count} selected · {}", fmt_hms(sel_secs)));
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
    fn selected_clause_has_count_and_duration_plus_hour_rollover() {
        assert_eq!(
            playlist_status_line(12, 3665, Some((3, 664))),
            "12 tracks · 1:01:05 total · 3 selected · 11:04"
        );
        assert_eq!(
            playlist_status_line(5, 100, Some((1, 65))),
            "5 tracks · 1:40 total · 1 selected · 1:05"
        );
        assert_eq!(playlist_status_line(0, 0, None), "0 tracks · 0:00 total");
    }
}
