//! Pure decision helpers for play-count stats (F11) and album-artist
//! fallback (F12). No I/O — table-tested so every frontend agrees.

use crate::config::{PlayStatsConfig, PlayStatsMode};

/// Playback position (seconds) at which the current track counts as "played".
///
/// Returns `None` when stats are disabled — the caller must then never call
/// `record_play`.
///
/// Seconds mode: `min(cfg.seconds, length * 0.9)` when the length is known, so
/// a track shorter than the threshold still counts near its end (Winamp "reach
/// the threshold OR the file end"). Unknown length → `cfg.seconds` (no clamp).
///
/// Percent mode: `length * cfg.percent/100` when known; unknown length falls
/// back to seconds mode (`cfg.seconds`), per the 2026-07-28 user decision.
///
/// `dead_code` until the phase-10 controller wiring (record_play) consumes it.
#[allow(dead_code)]
pub fn play_counted_at(length_secs: Option<f64>, cfg: &PlayStatsConfig) -> Option<f64> {
    if !cfg.enabled {
        return None;
    }
    let seconds = f64::from(cfg.seconds);
    match cfg.mode {
        PlayStatsMode::Seconds => Some(match length_secs {
            Some(len) if len > 0.0 => seconds.min(len * 0.9),
            _ => seconds,
        }),
        PlayStatsMode::Percent => match length_secs {
            Some(len) if len > 0.0 => Some(len * (f64::from(cfg.percent) / 100.0)),
            _ => Some(seconds),
        },
    }
}

/// The album-artist to display/group by. When `artist_as_album` is true and
/// the track has no album-artist tag, fall back to the artist (F12). Trims so
/// whitespace-only tags count as empty.
///
/// NOTE: A4 (phase 11 album gallery) MUST also route its album-artist
/// grouping through this helper, so the gallery agrees with the Media
/// Library's display/grouping once that phase lands.
pub fn effective_album_artist(artist: &str, album_artist: &str, artist_as_album: bool) -> String {
    if !album_artist.trim().is_empty() {
        album_artist.to_string()
    } else if artist_as_album {
        artist.to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod album_artist_tests {
    use super::effective_album_artist;

    #[test]
    fn prefers_album_artist_when_present() {
        assert_eq!(effective_album_artist("A", "AA", true), "AA");
        assert_eq!(effective_album_artist("A", "AA", false), "AA");
    }

    #[test]
    fn falls_back_to_artist_only_when_enabled() {
        assert_eq!(effective_album_artist("A", "", true), "A");
        assert_eq!(effective_album_artist("A", "   ", true), "A");
        assert_eq!(effective_album_artist("A", "", false), "");
    }

    #[test]
    fn neither_present() {
        assert_eq!(effective_album_artist("", "", true), "");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PlayStatsConfig, PlayStatsMode};

    fn cfg(mode: PlayStatsMode, seconds: u32, percent: u8) -> PlayStatsConfig {
        PlayStatsConfig { enabled: true, mode, seconds, percent }
    }

    #[test]
    fn disabled_never_counts() {
        let mut c = cfg(PlayStatsMode::Seconds, 20, 50);
        c.enabled = false;
        assert_eq!(play_counted_at(Some(200.0), &c), None);
        assert_eq!(play_counted_at(None, &c), None);
    }

    #[test]
    fn seconds_mode_normal_track() {
        let c = cfg(PlayStatsMode::Seconds, 20, 50);
        assert_eq!(play_counted_at(Some(200.0), &c), Some(20.0));
    }

    #[test]
    fn seconds_mode_clamps_short_track_to_90pct() {
        let c = cfg(PlayStatsMode::Seconds, 20, 50);
        // 15 s jingle: 20 > 15*0.9 = 13.5 → count at 13.5 s.
        assert_eq!(play_counted_at(Some(15.0), &c), Some(13.5));
    }

    #[test]
    fn seconds_mode_unknown_length_uses_raw_seconds() {
        let c = cfg(PlayStatsMode::Seconds, 20, 50);
        assert_eq!(play_counted_at(None, &c), Some(20.0));
    }

    #[test]
    fn percent_mode_half_of_200() {
        let c = cfg(PlayStatsMode::Percent, 20, 50);
        assert_eq!(play_counted_at(Some(200.0), &c), Some(100.0));
    }

    #[test]
    fn percent_mode_unknown_length_falls_back_to_seconds() {
        let c = cfg(PlayStatsMode::Percent, 20, 50);
        assert_eq!(play_counted_at(None, &c), Some(20.0));
    }
}
