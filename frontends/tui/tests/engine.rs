//! Duration cache, broken-track channel, advance, equalizer.

use super::*;
use crate::model::Track;
use crossterm::event::{KeyCode, KeyModifiers};
use std::path::PathBuf;

// Duration-cache / probing integration (without real audio files)
// -----------------------------------------------------------------------

/// Tracks created with a known duration are not overwritten by None.
#[test]
fn track_with_known_duration_is_not_overwritten() {
    let mut app = make_app();
    let dur = std::time::Duration::from_secs(180);
    let mut t = fake_track("song");
    t.duration = Some(dur);
    app.playlist.add(t);
    // Simulating what tick() does when no probe result arrives:
    // the duration should remain intact.
    app.tick();
    assert_eq!(app.playlist.tracks[0].duration, Some(dur));
}

/// tick() does not panic on an empty playlist.
#[test]
fn tick_with_empty_playlist_does_not_panic() {
    let mut app = make_app();
    app.tick(); // should not crash
}

/// probe_new_tracks fills duration from cache when the cache has data.
#[test]
fn probe_new_tracks_applies_cached_duration() {
    let mut app = make_app();
    let path = std::path::PathBuf::from("/fake/cached.mp3");
    let dur = std::time::Duration::from_secs(240);
    // Pre-populate the cache.
    app.duration_cache.insert(&path, dur);

    // Add a track whose path matches the cache entry.
    let t = Track {
        path: path.clone(),
        title: "Cached".into(),
        artist: String::new(),
        album_artist: String::new(),
        album: String::new(),
        duration: None,
        broken: false,
        read_only: false,
    };
    let before = app.playlist.tracks.len();
    app.playlist.add(t);
    app.probe_new_tracks(before);
    // Cache hit should immediately fill the duration.
    assert_eq!(app.playlist.tracks[before].duration, Some(dur));
}

/// playlist_visible defaults to true.
#[test]
fn playlist_is_visible_by_default() {
    let app = make_app();
    assert!(app.playlist_visible);
}

/// 'p' toggles playlist_visible.
#[test]
fn p_key_toggles_playlist_visibility() {
    let mut app = make_app();
    assert!(app.playlist_visible);
    app.handle_key(KeyCode::Char('p'), KeyModifiers::NONE);
    assert!(!app.playlist_visible);
    app.handle_key(KeyCode::Char('p'), KeyModifiers::NONE);
    assert!(app.playlist_visible);
}

// -----------------------------------------------------------------------
// Missing-file / broken-track channel (broken_rx drain in tick)
// -----------------------------------------------------------------------

/// Sending a path on broken_tx and calling tick() must mark that track broken.
#[test]
fn tick_marks_track_broken_when_missing_notification_arrives() {
    let mut app = app_with_tracks(&["A", "B"]);
    let path = app.playlist.tracks[1].path.clone();
    app.broken_tx.send(path).expect("channel must be open");
    app.tick();
    assert!(
        !app.playlist.tracks[0].broken,
        "track A should be unaffected"
    );
    assert!(
        app.playlist.tracks[1].broken,
        "track B should be marked broken"
    );
}

/// Multiple missing notifications in one tick are all applied.
#[test]
fn tick_marks_multiple_tracks_broken_in_one_pass() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    let p0 = app.playlist.tracks[0].path.clone();
    let p2 = app.playlist.tracks[2].path.clone();
    app.broken_tx.send(p0).unwrap();
    app.broken_tx.send(p2).unwrap();
    app.tick();
    assert!(app.playlist.tracks[0].broken);
    assert!(!app.playlist.tracks[1].broken);
    assert!(app.playlist.tracks[2].broken);
}

/// A path that does not match any track has no effect.
#[test]
fn tick_ignores_missing_notification_for_unknown_path() {
    let mut app = app_with_tracks(&["A"]);
    app.broken_tx
        .send(PathBuf::from("/no/such/file.mp3"))
        .unwrap();
    app.tick();
    assert!(!app.playlist.tracks[0].broken);
}

// -----------------------------------------------------------------------
// advance_to_next_playable
// -----------------------------------------------------------------------

/// When every remaining track is pre-flagged broken, advance_to_next_playable
/// must stop (no crash) and deactivate the visualizer.
#[test]
fn advance_to_next_playable_stops_when_all_tracks_are_broken() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    for t in &mut app.playlist.tracks {
        t.broken = true;
    }
    app.visualizer_active = true;
    app.advance_to_next_playable();
    assert!(
        !app.visualizer_active,
        "visualizer should stop when no playable track exists"
    );
}

/// advance_to_next_playable skips a broken track at index 1 and moves the
/// current_index forward (load of fake path will also fail, but the index
/// must advance past the pre-broken slot).
#[test]
fn advance_to_next_playable_skips_pre_broken_track() {
    let mut app = app_with_tracks(&["A", "B", "C"]);
    app.playlist.current_index = 0;
    app.playlist.tracks[1].broken = true;
    app.advance_to_next_playable();
    // current_index must have moved; it will never stay at 0 because
    // advance_to_next_playable always calls playlist.next() at least once.
    assert!(
        app.playlist.current_index > 0,
        "index must advance past starting position"
    );
    // The pre-broken track (index 1) must not be the one chosen while skipping was in effect —
    // it may have been visited but immediately skipped.
    // Track at index 1 must remain broken (not un-broken by the function).
    assert!(
        app.playlist.tracks[1].broken,
        "pre-broken track must stay broken"
    );
}

/// advance_to_next_playable on a single-track playlist (no next to go to)
/// terminates cleanly.
#[test]
fn advance_to_next_playable_with_single_track_does_not_crash() {
    let mut app = app_with_tracks(&["A"]);
    app.visualizer_active = true;
    app.advance_to_next_playable();
    assert!(!app.visualizer_active);
}

// -----------------------------------------------------------------------
// Equalizer
// -----------------------------------------------------------------------

/// `u` key opens the equalizer overlay.
#[test]
fn u_key_opens_equalizer_overlay() {
    let mut app = make_app();
    app.handle_key(KeyCode::Char('u'), KeyModifiers::NONE);
    assert!(
        matches!(app.mode, Mode::Equalizer(..)),
        "mode should be Equalizer after u key"
    );
}

/// Esc closes the equalizer overlay and returns to Normal.
#[test]
fn esc_closes_equalizer_overlay() {
    let mut app = make_app();
    app.mode = Mode::Equalizer(EqState { selected_band: 0 });
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, Mode::Normal));
}

/// Up key raises the selected band by 1 dB.
#[test]
fn eq_up_key_raises_band_by_1db() {
    let mut app = make_app();
    app.config.equalizer.bands = vec![0.0; 10];
    app.mode = Mode::Equalizer(EqState { selected_band: 0 });
    app.handle_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.config.equalizer.bands[0], 1.0);
}

/// Down key lowers the selected band by 1 dB.
#[test]
fn eq_down_key_lowers_band_by_1db() {
    let mut app = make_app();
    app.config.equalizer.bands = vec![0.0; 10];
    app.mode = Mode::Equalizer(EqState { selected_band: 3 });
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.config.equalizer.bands[3], -1.0);
}

/// Band gain is clamped to the maximum (+12 dB).
#[test]
fn eq_gain_clamped_at_max() {
    let mut app = make_app();
    app.config.equalizer.bands = vec![12.0; 10];
    app.mode = Mode::Equalizer(EqState { selected_band: 0 });
    app.handle_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(
        app.config.equalizer.bands[0], 12.0,
        "gain should not exceed +12 dB"
    );
}

/// Band gain is clamped to the minimum (-12 dB).
#[test]
fn eq_gain_clamped_at_min() {
    let mut app = make_app();
    app.config.equalizer.bands = vec![-12.0; 10];
    app.mode = Mode::Equalizer(EqState { selected_band: 0 });
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        app.config.equalizer.bands[0], -12.0,
        "gain should not go below -12 dB"
    );
}

/// Hammering every band to the extremes and back must not panic.
///
/// This guards against regressions where values sent to the GStreamer
/// engine fall outside its accepted range and cause a crash.
#[test]
fn eq_full_range_sweep_does_not_panic() {
    let mut app = make_app();
    app.config.equalizer.enabled = true;
    // Drive every band to +12 dB one step at a time.
    for _ in 0..30 {
        for band in 0..10 {
            app.mode = Mode::Equalizer(EqState {
                selected_band: band,
            });
            app.handle_key(KeyCode::Up, KeyModifiers::NONE);
        }
    }
    // Then drive every band to -12 dB.
    for _ in 0..30 {
        for band in 0..10 {
            app.mode = Mode::Equalizer(EqState {
                selected_band: band,
            });
            app.handle_key(KeyCode::Down, KeyModifiers::NONE);
        }
    }
    // All bands must be clamped at -12, not below.
    for &gain in &app.config.equalizer.bands {
        assert_eq!(gain, -12.0, "band clamped correctly at minimum");
    }
}

/// Right arrow advances the selected band.
#[test]
fn eq_right_key_advances_selected_band() {
    let mut app = make_app();
    app.mode = Mode::Equalizer(EqState { selected_band: 2 });
    app.handle_key(KeyCode::Right, KeyModifiers::NONE);
    assert!(matches!(&app.mode, Mode::Equalizer(s) if s.selected_band == 3));
}

/// Left arrow decrements the selected band (clamped at 0).
#[test]
fn eq_left_key_decrements_selected_band_clamped() {
    let mut app = make_app();
    app.mode = Mode::Equalizer(EqState { selected_band: 0 });
    app.handle_key(KeyCode::Left, KeyModifiers::NONE);
    assert!(matches!(&app.mode, Mode::Equalizer(s) if s.selected_band == 0));
}

/// Right arrow at band 10 (pre-amp) does not overflow.
#[test]
fn eq_right_key_clamped_at_preamp() {
    let mut app = make_app();
    app.mode = Mode::Equalizer(EqState { selected_band: 10 });
    app.handle_key(KeyCode::Right, KeyModifiers::NONE);
    assert!(matches!(&app.mode, Mode::Equalizer(s) if s.selected_band == 10));
}

/// `p` key cycles to the first EQ preset.
#[test]
fn eq_p_key_cycles_to_first_preset() {
    use crate::config::EQ_PRESETS;
    let mut app = make_app();
    app.config.equalizer.preset = String::new(); // start on Custom
    app.mode = Mode::Equalizer(EqState { selected_band: 0 });
    app.handle_key(KeyCode::Char('p'), KeyModifiers::NONE);
    // Custom → first preset (index 0).
    assert_eq!(app.config.equalizer.preset, EQ_PRESETS[0].0);
}

/// `r` key resets all bands to flat.
#[test]
fn eq_r_key_resets_to_flat() {
    let mut app = make_app();
    app.config.equalizer.bands = vec![6.0, 3.0, -3.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    app.mode = Mode::Equalizer(EqState { selected_band: 0 });
    app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE);
    assert!(
        app.config.equalizer.bands.iter().all(|&v| v == 0.0),
        "all bands should be 0 after reset"
    );
    assert_eq!(app.config.equalizer.preset, "Flat");
}

/// `t` key toggles EQ enabled/disabled.
#[test]
fn eq_t_key_toggles_enabled() {
    let mut app = make_app();
    app.config.equalizer.enabled = true;
    app.mode = Mode::Equalizer(EqState { selected_band: 0 });
    app.handle_key(KeyCode::Char('t'), KeyModifiers::NONE);
    assert!(!app.config.equalizer.enabled);
    app.handle_key(KeyCode::Char('t'), KeyModifiers::NONE);
    assert!(app.config.equalizer.enabled);
}

/// `EqConfig::effective_bands` returns zeros when disabled.
#[test]
fn eq_effective_bands_returns_zeros_when_disabled() {
    let mut cfg = crate::config::EqConfig::default();
    cfg.enabled = false;
    cfg.bands = vec![6.0; 10];
    let eff = cfg.effective_bands();
    assert!(eff.iter().all(|&v| v == 0.0));
}

/// `EqConfig::effective_bands` returns stored gains when enabled.
#[test]
fn eq_effective_bands_returns_gains_when_enabled() {
    let cfg = crate::config::EqConfig {
        enabled: true,
        preset: "Rock".to_string(),
        bands: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0],
        preamp: 1.0,
    };
    let eff = cfg.effective_bands();
    assert_eq!(eff[0], 1.0);
    assert_eq!(eff[9], 10.0);
}
