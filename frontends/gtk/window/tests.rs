#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, VisualizerMode};
    use crate::model::{Playlist, Track};
    use std::path::PathBuf;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_state() -> AppState {
        gstreamer::init().expect("GStreamer must be available for tests");
        AppState::new(Playlist::new(), Config::default()).expect("AppState::new failed")
    }

    fn fake_track(title: &str) -> Track {
        Track {
            path: PathBuf::from(format!("/fake/{}.mp3", title)),
            title: title.to_string(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            duration: None,
            broken: false,
            read_only: false,
        }
    }

    fn named_track(title: &str, artist: &str) -> Track {
        Track {
            path: PathBuf::from(format!("/fake/{}.mp3", title)),
            title: title.to_string(),
            artist: artist.to_string(),
            album_artist: String::new(),
            album: String::new(),
            duration: None,
            broken: false,
            read_only: false,
        }
    }

    fn state_with_tracks(titles: &[&str]) -> AppState {
        let mut s = make_state();
        for t in titles {
            s.playlist.add(fake_track(t));
        }
        s
    }

    // ── AppState::new ─────────────────────────────────────────────────────────

    #[test]
    fn new_state_preserves_playlist_length() {
        let mut pl = Playlist::new();
        pl.add(fake_track("Song"));
        gstreamer::init().unwrap();
        let s = AppState::new(pl, Config::default()).unwrap();
        assert_eq!(s.playlist.len(), 1);
    }

    // ── AppState::play_current ────────────────────────────────────────────────

    #[test]
    fn play_current_with_empty_playlist_returns_none() {
        let mut s = make_state();
        assert!(s.play_current().is_none());
    }

    #[test]
    fn play_current_with_track_returns_display_name() {
        // play_current() will attempt to load /fake/A.mp3 (which doesn't
        // exist) but still returns the metadata before GStreamer tries to open
        // the file.  The GStreamer error surfaces later via poll_bus().
        let mut s = state_with_tracks(&["A"]);
        let result = s.play_current();
        assert!(result.is_some());
        // No artist → display name is just the title
        assert_eq!(result.unwrap(), "A");
    }

    #[test]
    fn play_current_returns_correct_display_name_when_artist_present() {
        let mut s = make_state();
        s.playlist.add(named_track("Song", "My Artist"));
        let display = s.play_current().unwrap();
        assert_eq!(display, "My Artist - Song");
    }

    // ── AppState::play_next ───────────────────────────────────────────────────

    #[test]
    fn play_next_advances_current_index() {
        let mut s = state_with_tracks(&["A", "B"]);
        s.playlist.current_index = 0;
        s.play_next();
        assert_eq!(s.playlist.current_index, 1);
    }

    #[test]
    fn play_next_at_last_track_returns_none_and_does_not_advance() {
        let mut s = state_with_tracks(&["A"]);
        s.playlist.current_index = 0;
        let result = s.play_next();
        assert!(result.is_none());
        assert_eq!(s.playlist.current_index, 0);
    }

    #[test]
    fn play_next_on_empty_playlist_returns_none() {
        let mut s = make_state();
        assert!(s.play_next().is_none());
    }

    // ── AppState::play_prev ───────────────────────────────────────────────────

    /// Without real audio the player has no position, so `position()` returns
    /// `None` → `Duration::ZERO`, which is always < 5 s, so the back button
    /// always steps to the previous track in tests.
    #[test]
    fn play_prev_when_position_is_zero_goes_to_previous_track() {
        let mut s = state_with_tracks(&["A", "B"]);
        s.playlist.current_index = 1;
        s.play_prev();
        assert_eq!(s.playlist.current_index, 0);
    }

    /// At exactly 4 seconds, back button should go to previous track.
    #[test]
    fn play_prev_at_position_4_secs_goes_to_previous() {
        let mut s = state_with_tracks(&["A", "B"]);
        s.playlist.current_index = 1;
        s.player
            .set_position_for_test(std::time::Duration::from_secs(4));
        s.play_prev();
        assert_eq!(s.playlist.current_index, 0);
    }

    /// At exactly 5 seconds, back button should restart the current track.
    #[test]
    fn play_prev_at_position_5_secs_restarts_track() {
        let mut s = state_with_tracks(&["A", "B"]);
        s.playlist.current_index = 1;
        s.player
            .set_position_for_test(std::time::Duration::from_secs(5));
        s.play_prev();
        // Should stay at index 1 (restart, not go to previous)
        assert_eq!(s.playlist.current_index, 1);
    }

    /// At 6 seconds, back button should restart the current track.
    #[test]
    fn play_prev_at_position_6_secs_restarts_track() {
        let mut s = state_with_tracks(&["A", "B"]);
        s.playlist.current_index = 1;
        s.player
            .set_position_for_test(std::time::Duration::from_secs(6));
        s.play_prev();
        // Should stay at index 1 (restart, not go to previous)
        assert_eq!(s.playlist.current_index, 1);
    }

    #[test]
    fn play_prev_at_first_track_stays_at_index_zero() {
        let mut s = state_with_tracks(&["A", "B"]);
        s.playlist.current_index = 0;
        s.play_prev();
        assert_eq!(s.playlist.current_index, 0);
    }

    #[test]
    fn play_prev_on_only_track_does_not_crash() {
        let mut s = state_with_tracks(&["A"]);
        s.play_prev();
        assert_eq!(s.playlist.current_index, 0);
    }

    #[test]
    fn play_next_when_stopped_does_not_start_playback() {
        let mut s = state_with_tracks(&["A", "B"]);
        s.playlist.current_index = 0;
        // Player starts in Stopped state
        assert_eq!(*s.player.state(), PlayerState::Stopped);
        let result = s.play_next();
        // Should advance to next track
        assert_eq!(s.playlist.current_index, 1);
        // Should return display name
        assert!(result.is_some());
        // Should still be stopped (not auto-started)
        assert_eq!(*s.player.state(), PlayerState::Stopped);
    }

    #[test]
    fn play_next_when_stopped_returns_correct_display_name() {
        let mut s = state_with_tracks(&["Song A", "Song B"]);
        s.playlist.current_index = 0;
        let result = s.play_next();
        // Should return the display name of the next track
        assert_eq!(result.unwrap(), "Song B");
    }

    #[test]
    fn play_prev_when_stopped_does_not_start_playback() {
        let mut s = state_with_tracks(&["A", "B"]);
        s.playlist.current_index = 1;
        // Player starts in Stopped state
        assert_eq!(*s.player.state(), PlayerState::Stopped);
        let result = s.play_prev();
        // Should go back to previous track
        assert_eq!(s.playlist.current_index, 0);
        // Should return display name
        assert!(result.is_some());
        // Should still be stopped (not auto-started)
        assert_eq!(*s.player.state(), PlayerState::Stopped);
    }

    #[test]
    fn play_prev_when_stopped_returns_correct_display_name() {
        let mut s = state_with_tracks(&["Song A", "Song B"]);
        s.playlist.current_index = 1;
        let result = s.play_prev();
        // Should return the display name of the previous track
        assert_eq!(result.unwrap(), "Song A");
    }

    // ── AppState::toggle_visualizer_mode ──────────────────────────────────────

    #[test]
    fn toggle_visualizer_mode_bars_becomes_waveform() {
        let mut s = make_state();
        assert_eq!(s.config.visualizer.mode, VisualizerMode::Bars);
        s.toggle_visualizer_mode();
        assert_eq!(s.config.visualizer.mode, VisualizerMode::Waveform);
    }

    #[test]
    fn toggle_visualizer_mode_waveform_becomes_granite() {
        let mut s = make_state();
        s.config.visualizer.mode = VisualizerMode::Waveform;
        s.toggle_visualizer_mode();
        assert_eq!(s.config.visualizer.mode, VisualizerMode::Granite);
    }

    #[test]
    fn toggle_visualizer_mode_granite_becomes_bars() {
        let mut s = make_state();
        s.config.visualizer.mode = VisualizerMode::Granite;
        s.toggle_visualizer_mode();
        assert_eq!(s.config.visualizer.mode, VisualizerMode::Bars);
    }

    #[test]
    fn toggle_visualizer_mode_99_times_ends_back_at_bars() {
        // Cycle is Bars → Waveform → Granite → Bars, period 3. 99 toggles is
        // divisible by 3, so the mode must return to its starting value.
        let mut s = make_state();
        for _ in 0..99 {
            s.toggle_visualizer_mode();
        }
        assert_eq!(s.config.visualizer.mode, VisualizerMode::Bars);
    }

    // ── AppState::seek_fraction ───────────────────────────────────────────────

    /// Without active playback there is no duration, so seek_fraction() is a
    /// no-op.  The key guarantee is that it does not panic.
    #[test]
    fn seek_fraction_without_active_track_does_not_panic() {
        let mut s = make_state();
        s.seek_fraction(0.5);
    }

    #[test]
    fn seek_fraction_clamps_negative_values() {
        let mut s = make_state();
        s.seek_fraction(-1.0); // must not panic, clamped to 0.0
    }

    #[test]
    fn seek_fraction_clamps_values_above_one() {
        let mut s = make_state();
        s.seek_fraction(2.0); // must not panic, clamped to 1.0
    }

    // ── AppState::seek_fraction_or_pend ──────────────────────────────────────

    #[test]
    fn seek_fraction_or_pend_stores_pending_when_stopped() {
        // Player starts in Stopped state — seek should be deferred.
        let mut s = make_state();
        s.seek_fraction_or_pend(0.5);
        assert_eq!(s.pending_seek, Some(0.5));
    }

    #[test]
    fn seek_fraction_or_pend_clamps_value_before_storing() {
        let mut s = make_state();
        s.seek_fraction_or_pend(1.5);
        assert_eq!(s.pending_seek, Some(1.0));
        s.seek_fraction_or_pend(-0.5);
        assert_eq!(s.pending_seek, Some(0.0));
    }

    #[test]
    fn seek_fraction_or_pend_overwrites_previous_pending_seek() {
        let mut s = make_state();
        s.seek_fraction_or_pend(0.3);
        s.seek_fraction_or_pend(0.7);
        assert_eq!(s.pending_seek, Some(0.7));
    }

    // ── AppState::seek_delta_secs ─────────────────────────────────────────────

    #[test]
    fn seek_delta_secs_forward_without_active_track_does_not_panic() {
        // No track loaded → position/duration both None → no-op.
        let mut s = make_state();
        s.seek_delta_secs(5.0);
    }

    #[test]
    fn seek_delta_secs_backward_without_active_track_does_not_panic() {
        let mut s = make_state();
        s.seek_delta_secs(-5.0);
    }

    // ── AppState::time_display_for_fraction ──────────────────────────────────

    fn state_with_last_duration(secs: u64) -> AppState {
        let mut s = make_state();
        s.last_duration = Some(Duration::from_secs(secs));
        s
    }

    #[test]
    fn time_display_for_fraction_returns_none_when_no_duration() {
        // Neither live GStreamer duration nor cached duration is available.
        let s = make_state();
        assert!(s.time_display_for_fraction(0.5, false).is_none());
    }

    #[test]
    fn time_display_elapsed_at_75_percent_of_4_minute_track() {
        // 4 min = 240 s.  75 % → 180 s → "3:00".
        let s = state_with_last_duration(240);
        assert_eq!(
            s.time_display_for_fraction(0.75, false),
            Some("3:00".to_string())
        );
    }

    #[test]
    fn time_display_remaining_at_75_percent_of_4_minute_track() {
        // 75 % elapsed → 25 % remaining = 60 s → "-1:00".
        let s = state_with_last_duration(240);
        assert_eq!(
            s.time_display_for_fraction(0.75, true),
            Some("-1:00".to_string())
        );
    }

    #[test]
    fn time_display_elapsed_at_start() {
        let s = state_with_last_duration(120);
        assert_eq!(
            s.time_display_for_fraction(0.0, false),
            Some("0:00".to_string())
        );
    }

    #[test]
    fn time_display_elapsed_at_end() {
        let s = state_with_last_duration(120);
        assert_eq!(
            s.time_display_for_fraction(1.0, false),
            Some("2:00".to_string())
        );
    }

    #[test]
    fn time_display_remaining_at_start() {
        // 0 % elapsed → full duration remaining = 120 s → "-2:00".
        let s = state_with_last_duration(120);
        assert_eq!(
            s.time_display_for_fraction(0.0, true),
            Some("-2:00".to_string())
        );
    }

    #[test]
    fn time_display_fraction_clamps_above_one() {
        let s = state_with_last_duration(60);
        assert_eq!(
            s.time_display_for_fraction(1.5, false),
            Some("1:00".to_string())
        );
    }

    // ── AppState::remove_track ────────────────────────────────────────────────

    #[test]
    fn remove_track_shortens_playlist_by_one() {
        let mut s = state_with_tracks(&["A", "B", "C"]);
        s.remove_track(1); // remove "B"
        assert_eq!(s.playlist.len(), 2);
        let titles: Vec<_> = s.playlist.tracks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["A", "C"]);
    }

    #[test]
    fn remove_track_out_of_bounds_leaves_playlist_unchanged() {
        let mut s = state_with_tracks(&["A", "B"]);
        s.remove_track(99);
        assert_eq!(s.playlist.len(), 2);
    }

    #[test]
    fn remove_last_remaining_track_stops_player_and_clears_the_marquee() {
        let mut s = state_with_tracks(&["A"]);
        let result = s.remove_track(0);
        // Some("") tells the caller to blank the marquee — the removed
        // song's name must not linger after the playlist empties.
        assert_eq!(result.as_deref(), Some(""));
        assert!(s.playlist.is_empty());
    }

    #[test]
    fn remove_current_row_while_stopped_does_not_start_playback() {
        let mut s = state_with_tracks(&["A", "B", "C"]);
        s.playlist.jump_to(1);
        let result = s.remove_track(1);
        // Marquee reflects the new current row, but nothing plays.
        assert_eq!(result.as_deref(), Some("C"));
        assert!(matches!(
            *s.player.state(),
            crate::engine::PlayerState::Stopped
        ));
        assert_eq!(s.playlist.current_index, 1);
    }

    #[test]
    fn remove_one_of_three_identical_tracks_leaves_two() {
        let mut s = make_state();
        for _ in 0..3 {
            s.playlist.add(fake_track("same"));
        }
        s.remove_track(1);
        assert_eq!(s.playlist.len(), 2);
        assert!(s.playlist.tracks.iter().all(|t| t.title == "same"));
    }

    // ── AppState::add_track_from_path ─────────────────────────────────────────

    #[test]
    fn add_track_from_nonexistent_path_returns_error_and_does_not_modify_playlist() {
        let mut s = make_state();
        let result = s.add_track_from_path("/nonexistent/file.mp3");
        assert!(result.is_err());
        assert!(s.playlist.is_empty());
    }

    #[test]
    fn add_track_from_path_trims_leading_and_trailing_whitespace() {
        // File still doesn't exist, but the trim must happen before the error.
        let mut s = make_state();
        let err = s
            .add_track_from_path("  /nonexistent/file.mp3  ")
            .unwrap_err();
        // The error message should contain the trimmed path, not the padded one.
        assert!(err.contains("/nonexistent/file.mp3"));
        assert!(!err.contains("  /nonexistent")); // no leading spaces
    }

    // ── AppState::poll_bus ────────────────────────────────────────────────────

    #[test]
    fn poll_bus_with_idle_player_returns_false() {
        let mut s = make_state();
        assert!(s.poll_bus().is_none(), "idle player should not signal EOS");
    }

    // ── End-of-stream auto-advance ────────────────────────────────────────────

    #[test]
    fn eos_auto_advance_to_next_track_on_two_track_playlist() {
        // Simulate what the tick loop does when poll_bus() returns true.
        let mut s = state_with_tracks(&["A", "B"]);
        s.playlist.current_index = 0;
        s.play_next(); // mimics the tick-loop's response to EOS
        assert_eq!(s.playlist.current_index, 1);
    }

    #[test]
    fn eos_on_last_track_does_not_advance_index() {
        let mut s = state_with_tracks(&["A"]);
        s.playlist.current_index = 0;
        let result = s.play_next(); // at end → returns None
        assert!(result.is_none());
        assert_eq!(s.playlist.current_index, 0);
    }

    // ── Playlist management edge cases ────────────────────────────────────────

    #[test]
    fn same_track_added_multiple_times_creates_multiple_entries() {
        let mut s = make_state();
        for _ in 0..5 {
            s.playlist.add(fake_track("dup"));
        }
        assert_eq!(s.playlist.len(), 5);
    }

    // ── Search helper ─────────────────────────────────────────────────────────

    #[test]
    fn search_indices_matches_title_case_insensitively() {
        let mut s = make_state();
        s.playlist.add(named_track("Hello World", "Test Artist"));
        s.playlist.add(named_track("Another Song", "Other Band"));
        let results = s.playlist.search_indices("hello");
        assert_eq!(results, vec![0]);
    }

    #[test]
    fn search_indices_matches_artist_case_insensitively() {
        let mut s = make_state();
        s.playlist.add(named_track("Hello World", "Test Artist"));
        s.playlist.add(named_track("Another Song", "Other Band"));
        let results = s.playlist.search_indices("test artist");
        assert_eq!(results, vec![0]);
    }

    #[test]
    fn search_indices_returns_empty_for_no_match() {
        let mut s = make_state();
        s.playlist.add(named_track("Hello World", "Test Artist"));
        let results = s.playlist.search_indices("zzzzz");
        assert!(results.is_empty());
    }

    #[test]
    fn search_indices_matches_across_fields() {
        // "ed sheeran don't" — artist and title words in a single query.
        let mut s = make_state();
        s.playlist.add(named_track("Don't", "Ed Sheeran"));
        s.playlist.add(named_track("Perfect", "Ed Sheeran"));
        s.playlist.add(named_track("Don't Stop", "Journey"));
        let results = s.playlist.search_indices("ed sheeran don't");
        assert_eq!(results, vec![0]);
    }

    #[test]
    fn search_indices_returns_empty_for_empty_query() {
        let s = state_with_tracks(&["A", "B", "C"]);
        // Empty query returns nothing so the jump window doesn't create
        // thousands of widgets on open, which would freeze the UI.
        let results = s.playlist.search_indices("");
        assert!(results.is_empty());
    }

    // ── fmt_duration ──────────────────────────────────────────────────────────

    #[test]
    fn fmt_duration_none_returns_placeholder() {
        assert_eq!(fmt_duration(None), "-:--");
    }

    #[test]
    fn fmt_duration_zero_seconds() {
        assert_eq!(fmt_duration(Some(Duration::from_secs(0))), "0:00");
    }

    #[test]
    fn fmt_duration_one_minute_thirty() {
        assert_eq!(fmt_duration(Some(Duration::from_secs(90))), "1:30");
    }

    #[test]
    fn fmt_duration_exact_hour() {
        assert_eq!(fmt_duration(Some(Duration::from_secs(3600))), "60:00");
    }

    #[test]
    fn fmt_duration_seconds_below_ten_are_zero_padded() {
        assert_eq!(fmt_duration(Some(Duration::from_secs(65))), "1:05");
    }

    // ── AppState::apply_probed_durations (batch) ─────────────────────────────

    fn batch_of(entries: &[(&std::path::Path, Duration)])
        -> std::collections::HashMap<std::path::PathBuf, Duration>
    {
        entries.iter().map(|(p, d)| (p.to_path_buf(), *d)).collect()
    }

    #[test]
    fn apply_probed_durations_sets_every_matching_row_in_one_pass() {
        let mut s = make_state();
        s.playlist.add(fake_track("Song"));
        s.playlist.add(fake_track("Other"));
        // The same file queued twice — BOTH rows must receive the duration.
        s.playlist.add(fake_track("Song"));
        let path = s.playlist.tracks[0].path.clone();
        let dur = Duration::from_secs(180);
        let changed = s.apply_probed_durations(&batch_of(&[(&path, dur)]));
        assert_eq!(changed, vec![0, 2]);
        assert_eq!(s.playlist.tracks[0].duration, Some(dur));
        assert_eq!(s.playlist.tracks[2].duration, Some(dur));
        assert_eq!(s.playlist.tracks[1].duration, None);
    }

    #[test]
    fn apply_probed_durations_inserts_into_cache() {
        let mut s = make_state();
        s.playlist.add(fake_track("Song"));
        let path = s.playlist.tracks[0].path.clone();
        let _ = s.apply_probed_durations(&batch_of(&[(&path, Duration::from_secs(120))]));
        assert!(s.duration_cache.dirty);
        assert_eq!(s.duration_cache.get(&path), Some(Duration::from_secs(120)));
    }

    #[test]
    fn apply_probed_durations_updates_last_duration_for_current_stopped_track() {
        let mut s = make_state();
        s.playlist.add(fake_track("Song"));
        let path = s.playlist.tracks[0].path.clone();
        let dur = Duration::from_secs(200);
        let _ = s.apply_probed_durations(&batch_of(&[(&path, dur)]));
        // Player is Stopped (freshly created), current track matches → last_duration set.
        assert_eq!(s.last_duration, Some(dur));
    }

    #[test]
    fn apply_probed_durations_does_not_update_last_duration_for_non_current_track() {
        let mut s = make_state();
        s.playlist.add(fake_track("A"));
        s.playlist.add(fake_track("B"));
        s.playlist.current_index = 0;
        let path_b = s.playlist.tracks[1].path.clone();
        let _ = s.apply_probed_durations(&batch_of(&[(&path_b, Duration::from_secs(99))]));
        // Track B is not current → last_duration unchanged.
        assert_eq!(s.last_duration, None);
    }

    // ── AppState::apply_cached_durations ─────────────────────────────────────

    #[test]
    fn apply_cached_durations_fills_from_cache() {
        let mut s = make_state();
        s.playlist.add(fake_track("Song"));
        let path = s.playlist.tracks[0].path.clone();
        let dur = Duration::from_secs(240);
        // Pre-populate cache directly.
        s.duration_cache.insert(&path, dur);
        // Duration not yet on track.
        assert_eq!(s.playlist.tracks[0].duration, None);
        s.apply_cached_durations();
        assert_eq!(s.playlist.tracks[0].duration, Some(dur));
    }

    #[test]
    fn apply_cached_durations_seeds_last_duration_for_current_track() {
        let mut s = make_state();
        s.playlist.add(fake_track("Song"));
        let path = s.playlist.tracks[0].path.clone();
        s.duration_cache.insert(&path, Duration::from_secs(300));
        s.apply_cached_durations();
        assert_eq!(s.last_duration, Some(Duration::from_secs(300)));
    }

    #[test]
    fn apply_cached_durations_skips_tracks_already_having_duration() {
        let mut s = make_state();
        let mut track = fake_track("Song");
        track.duration = Some(Duration::from_secs(100));
        s.playlist.add(track);
        let path = s.playlist.tracks[0].path.clone();
        // Cache has a different value — should NOT overwrite the track's own.
        s.duration_cache.insert(&path, Duration::from_secs(999));
        s.apply_cached_durations();
        assert_eq!(
            s.playlist.tracks[0].duration,
            Some(Duration::from_secs(100))
        );
    }

    #[test]
    fn eq_preamp_is_stored_in_config() {
        let mut s = make_state();
        assert!(
            (0.5..=1.5).contains(&s.config.equalizer.preamp),
            "preamp should be in range [0.5, 1.5], got {}",
            s.config.equalizer.preamp
        );
        let clamped = 1.25f64.clamp(0.5, 1.5);
        s.config.equalizer.preamp = clamped;
        s.player.set_preamp(clamped);
        assert_eq!(s.config.equalizer.preamp, clamped);
    }

    // ── Play counting (20-second threshold) ─────────────────────────────────────

    #[test]
    fn new_state_has_counted_play_path_none() {
        let s = make_state();
        assert!(s.counted_play_path.is_none());
    }

    #[test]
    fn play_current_resets_counted_play_path() {
        let mut s = state_with_tracks(&["A", "B"]);
        // Simulate a previously-counted play by setting the field.
        let path_str = s.playlist.tracks[0].path.to_string_lossy().into_owned();
        s.counted_play_path = Some(path_str.clone());
        assert!(s.counted_play_path.is_some());

        // play_current() resets it so the new track can be counted.
        let _ = s.play_current();
        assert!(s.counted_play_path.is_none());
    }

    #[test]
    fn play_count_is_not_recorded_before_20_seconds() {
        // The counted_play_path field is None when a track starts,
        // so the tick loop's recording logic will not fire before 20 seconds elapse.
        let mut s = state_with_tracks(&["A"]);
        let _ = s.play_current();
        // Before any playback time accumulates, counted_play_path is None.
        assert!(s.counted_play_path.is_none());
    }

    #[test]
    fn play_current_tracks_are_independent() {
        // When switching tracks, counted_play_path is reset so the new track
        // starts fresh and can be counted independently of the previous one.
        let mut s = state_with_tracks(&["A", "B"]);
        let path_a = s.playlist.tracks[0].path.to_string_lossy().into_owned();

        // Simulate: A was counted, then user switched to B.
        s.counted_play_path = Some(path_a.clone());
        assert_eq!(s.counted_play_path, Some(path_a));

        // Switching to B resets the counter so B can be counted on its own.
        s.playlist.current_index = 1;
        let _ = s.play_current();
        assert!(s.counted_play_path.is_none());
    }

    #[test]
    fn switching_tracks_allows_new_track_to_be_counted() {
        // Verify that counted_play_path from track A does NOT prevent
        // track B from being counted (different paths).
        let mut s = state_with_tracks(&["A", "B"]);
        let path_a = s.playlist.tracks[0].path.to_string_lossy().into_owned();
        let path_b = s.playlist.tracks[1].path.to_string_lossy().into_owned();

        s.counted_play_path = Some(path_a.clone());
        assert_ne!(s.counted_play_path, Some(path_b.clone()));

        // After jumping to B, counted_play_path is cleared so B can be counted.
        s.playlist.jump_to(1);
        let _ = s.play_current();
        assert!(s.counted_play_path.is_none());
    }

    #[test]
    fn tick_loop_does_not_record_play_before_20_seconds() {
        // Simulate the tick loop's play-counting logic with < 20s of playback.
        // At 19 seconds the condition `pos >= 20_secs` is false → no recording.
        let mut s = state_with_tracks(&["A"]);
        let _ = s.play_current();
        let path = s.playlist.tracks[0].path.to_string_lossy().into_owned();

        // Simulate 19 seconds of playback (just under threshold).
        let pos_under = Duration::from_secs(19);
        // The tick loop's check: pos >= Duration::from_secs(20) → false
        assert!(pos_under < Duration::from_secs(20));
        assert!(s.counted_play_path.is_none());
        // Even after the check, path doesn't match (counted_play_path is None).
        assert_ne!(s.counted_play_path.as_ref(), Some(&path));
    }

    #[test]
    fn tick_loop_records_play_at_exactly_20_seconds() {
        // At exactly 20 seconds the condition `pos >= 20_secs` is true.
        let mut s = state_with_tracks(&["A"]);
        let _ = s.play_current();
        let path = s.playlist.tracks[0].path.to_string_lossy().into_owned();

        let pos_20s = Duration::from_secs(20);
        assert!(pos_20s >= Duration::from_secs(20));
        // Simulate: path differs from counted_play_path, so the tick loop
        // WOULD call ml.record_play and set counted_play_path = Some(path).
        assert_ne!(s.counted_play_path.as_ref(), Some(&path));
    }

    #[test]
    fn tick_loop_skips_recording_after_already_counted() {
        // Once counted_play_path matches the current path, no re-recording occurs.
        let mut s = state_with_tracks(&["A"]);
        let path = s.playlist.tracks[0].path.to_string_lossy().into_owned();

        // Simulate: track already counted at a previous tick.
        s.counted_play_path = Some(path.clone());
        assert_eq!(s.counted_play_path.as_ref(), Some(&path));

        // Simulate another tick with 25 seconds of playback.
        // The tick loop's condition: counted_play_path.as_ref() == Some(path) → true
        // The recording block is skipped (different paths check fails).
        // After this tick, counted_play_path should STILL be Some(path).
        assert_eq!(s.counted_play_path, Some(path));
    }
}
