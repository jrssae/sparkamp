//! Playback transport, track navigation (shuffle/repeat aware), config
//! persistence, and background duration probing.
#![allow(unsafe_op_in_unsafe_fn)]

use std::os::raw::{c_double, c_int};
use std::sync::Mutex;
use std::time::Duration;

use crate::controller::{Controller, NavResult};
use crate::duration_probe;
use crate::engine::PlayerState;
use crate::shuffle::RepeatMode;

use super::SparkampCtx;

/// Serialises all GStreamer Discoverer calls to one at a time.
///
/// Each `discover_duration` call internally creates a GLib main loop.
/// On macOS, spinning up multiple GLib main loops simultaneously from
/// Rayon threads causes GLib's GObject type system to access freed or
/// uninitialised memory (EXC_BAD_ACCESS at 0x1).  A single Mutex is
/// sufficient: Symphonia probing (`probe_duration`) is still fully
/// parallel — only the GStreamer fallback is serialised.
static DISCOVER_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------------

/// Resume playback (no-op if already playing; resumes if paused).
/// If stopped and the playlist has a current track, loads its URI first.
///
/// On a Stopped→Playing transition the current track is also recorded into
/// shuffle history — matching the GTK frontend's Play button, which calls
/// `play_current()` (which records).  Without this, the very first track of
/// a session never enters the shuffle history, so the back button has
/// nothing to step through and the previous track from the shuffled
/// playthrough is effectively unreachable.  Pause→Resume deliberately does
/// NOT record — it is the same listening event continuing, not a new play.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_play(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let was_stopped = *ctx.player.state() == PlayerState::Stopped;
    if was_stopped {
        if let Some(track) = ctx.playlist.current() {
            let uri = track.uri();
            super::prime_rg_for_current(ctx);
            ctx.player.load(&uri).ok();
        }
    }
    ctx.player.play().ok();
    if was_stopped {
        let idx = ctx.playlist.current_index;
        if idx < ctx.playlist.len() {
            ctx.shuffle_state.record_played(idx);
        }
    }
}

/// Pause playback (no-op if already paused or stopped).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_pause(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    if *ctx.player.state() == PlayerState::Playing {
        ctx.player.toggle_pause().ok();
    }
}

/// Stop playback and reset position to zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_stop(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    (*ctx).player.stop().ok();
}

/// Stop with fadeout (Shift+V): ramp the output down, then stop.
///
/// Returns immediately — the ramp is advanced by `sparkamp_tick`, so the
/// caller sees the stop land `playback.fadeout_secs` later. No-op unless
/// playing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_stop_with_fadeout(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let duration = ctx.config.playback.fadeout_duration();
    ctx.player.begin_fadeout(duration);
}

/// Whether a stop-with-fadeout ramp is currently running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_is_fading_out(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    (*ctx).player.is_fading_out()
}

/// Seek to a fractional position in the current track (0.0 = start, 1.0 = end).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_seek(ctx: *mut SparkampCtx, fraction: c_double) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let fraction = fraction.clamp(0.0, 1.0);
    if let Some(total) = ctx.player.duration() {
        let target = Duration::from_secs_f64(total.as_secs_f64() * fraction);
        ctx.player.seek(target).ok();
    }
}

/// Set the playback volume (0.0 = silence, 1.0 = 100%).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_volume(ctx: *mut SparkampCtx, vol: c_double) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let vol = vol.clamp(0.0, 1.0);
    ctx.player.set_volume(vol);
    ctx.config.playback.volume = vol;
}

/// Get the current volume (0.0–1.0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_volume(ctx: *const SparkampCtx) -> c_double {
    if ctx.is_null() {
        return 0.0;
    }
    (*ctx).config.playback.volume
}

/// Get the current playback position in seconds.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_position(ctx: *const SparkampCtx) -> c_double {
    if ctx.is_null() {
        return 0.0;
    }
    (*ctx).player.position().map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Get the current track duration in seconds, or -1 if unknown.
///
/// Falls back to the last GStreamer-reported duration (cached during playback)
/// and then to the probe-derived duration stored on the playlist track, so the
/// value survives Stop (where GStreamer's pipeline is torn down).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_duration(ctx: *const SparkampCtx) -> c_double {
    if ctx.is_null() {
        return -1.0;
    }
    let ctx = &*ctx;
    ctx.player.duration()
        .or(ctx.last_known_duration)
        .or_else(|| {
            let idx = ctx.playlist.current_index;
            ctx.playlist.tracks.get(idx).and_then(|t| t.duration)
        })
        .map(|d| d.as_secs_f64())
        .unwrap_or(-1.0)
}

/// Get the player state: 0 = Stopped, 1 = Playing, 2 = Paused.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_state(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    match (*ctx).player.state() {
        PlayerState::Stopped => 0,
        PlayerState::Playing => 1,
        PlayerState::Paused => 2,
    }
}

// ---------------------------------------------------------------------------
// Navigation (respects shuffle + repeat)
// ---------------------------------------------------------------------------

/// Advance to the next track (respecting shuffle and repeat settings).
///
/// Only starts playback when the player was already playing or paused —
/// matches the GTK/TUI behaviour: pressing Next while stopped moves the
/// cursor but does not begin playing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_nav_next(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    // Reset the cached duration so the new track starts fresh.
    ctx.last_known_duration = None;
    let mut ctrl = Controller {
        player: &mut ctx.player,
        playlist: &mut ctx.playlist,
        config: &mut ctx.config,
        shuffle_state: &mut ctx.shuffle_state,
        queue: &mut ctx.queue,
        media_library: ctx.media_library.as_ref(),
    };
    match ctrl.nav_next() {
        NavResult::Target { was_playing: true } => {
            ctrl.play_current_no_record();
        }
        NavResult::Target { was_playing: false } => {
            // Pre-load so position/duration queries work without playing.
            if let Some(track) = ctrl.playlist.current() {
                let uri = track.uri();
                let _ = ctrl.player.load(&uri);
            }
        }
        NavResult::NoTarget => {}
    }
}

/// Advance to the next playable track after end-of-stream, respecting repeat and shuffle.
///
/// Use this instead of `sparkamp_nav_next` from the EOS callback — it correctly
/// handles `RepeatMode::Song` (loops the current track) and skips broken tracks.
/// `sparkamp_nav_next` explicitly ignores Song repeat, making it wrong for EOS use.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_advance_after_eos(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let mut ctrl = Controller {
        player: &mut ctx.player,
        playlist: &mut ctx.playlist,
        config: &mut ctx.config,
        shuffle_state: &mut ctx.shuffle_state,
        queue: &mut ctx.queue,
        media_library: ctx.media_library.as_ref(),
    };
    ctrl.advance_to_next_playable();
}

/// Jump to the previous track (or restart the current one) and play.
///
/// Matches GTK behaviour:
/// - pos ≥ 5 s → restart current track from the beginning.
/// - pos < 5 s, shuffle on → walk shuffle-history cursor backward.
/// - pos < 5 s, shuffle off → linear previous (wraps under RepeatMode::Playlist).
/// - Was stopped → move cursor / pre-load but do NOT start playing.
///
/// Recording into shuffle history is owned by the controller — this handler
/// always uses `play_current_no_record` to avoid double-recording.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_nav_prev(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    // Reset cached duration so UI refreshes for the new track.
    ctx.last_known_duration = None;
    let mut ctrl = Controller {
        player: &mut ctx.player,
        playlist: &mut ctx.playlist,
        config: &mut ctx.config,
        shuffle_state: &mut ctx.shuffle_state,
        queue: &mut ctx.queue,
        media_library: ctx.media_library.as_ref(),
    };
    match ctrl.nav_prev() {
        NavResult::Target { was_playing: true } => {
            ctrl.play_current_no_record();
        }
        NavResult::Target { was_playing: false } => {
            if let Some(track) = ctrl.playlist.current() {
                let uri = track.uri();
                let _ = ctrl.player.load(&uri);
            }
        }
        NavResult::NoTarget => {}
    }
}

// ---------------------------------------------------------------------------
// Repeat / Shuffle
// ---------------------------------------------------------------------------

/// Get the current repeat mode: 0 = Off, 1 = One (Song), 2 = All (Playlist).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_repeat_mode(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    match (*ctx).config.playback.repeat_mode {
        RepeatMode::Off => 0,
        RepeatMode::Song => 1,
        RepeatMode::Playlist => 2,
    }
}

/// Cycle the repeat mode: Off → One → All → Off.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_cycle_repeat(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.playback.repeat_mode = ctx.config.playback.repeat_mode.cycle();
}

/// Get shuffle state: 1 = enabled, 0 = disabled.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_shuffle(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    (*ctx).shuffle_state.enabled as c_int
}

/// Toggle shuffle on/off.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_toggle_shuffle(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.shuffle_state.toggle();
    ctx.config.playback.shuffle_enabled = ctx.shuffle_state.enabled;
}

// ---------------------------------------------------------------------------
// Config persistence
// ---------------------------------------------------------------------------

/// Persist config and playlist to disk.
///
/// Call this before the app terminates (e.g. from `applicationWillTerminate`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_save_config(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    // volume is already kept in sync with config.playback.volume by sparkamp_set_volume
    ctx.config.playback.shuffle_enabled = ctx.shuffle_state.enabled;
    ctx.playlist.save_last().ok();
    ctx.config.save().ok();
}

// ---------------------------------------------------------------------------
// Duration probing
// ---------------------------------------------------------------------------

/// Probe the duration of the track at `index` in the background (Rayon thread).
///
/// When the probe completes, the result is stored and applied by the next
/// `sparkamp_tick` call.  Swift can then re-read the duration via
/// `sparkamp_playlist_get_duration`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_probe_duration(ctx: *mut SparkampCtx, index: c_int) {
    if ctx.is_null() {
        return;
    }
    queue_probe_duration(&*ctx, index as usize);
}

/// The body of [`sparkamp_probe_duration`], safe and callable from tests.
/// Returns whether a probe was queued — false means the track was declined,
/// which is the only observable difference the optical guard makes.
pub(crate) fn queue_probe_duration(ctx: &SparkampCtx, i: usize) -> bool {
    if i >= ctx.playlist.tracks.len() {
        return false;
    }
    let path = ctx.playlist.tracks[i].path.clone();
    // Never probe a file on a mounted optical disc — same rule, and the same
    // reason, as `queue_scan_metadata`. The duration is worse than pointless
    // here: the TOC already gives every audio track's length exactly, while
    // the probe seeks the drive out from under playback. A Symphonia miss
    // then falls through to the GStreamer discoverer, which opens the file a
    // second time.
    if crate::disc::detect::path_is_on_optical_media(&path) {
        return false;
    }
    let tx = ctx.duration_tx.clone();
    let probe = move || {
        // Fast path: Symphonia reads the container header with no GStreamer involvement.
        // Slow path: GStreamer Discoverer handles CBR MP3 and formats Symphonia misses.
        //   Serialised via DISCOVER_LOCK — concurrent GLib main loops from Rayon
        //   threads crash on macOS (EXC_BAD_ACCESS at 0x1 in the GObject type system).
        let dur = duration_probe::probe_duration(&path).or_else(|| {
            let _guard = DISCOVER_LOCK.lock().ok()?;
            duration_probe::discover_duration(&path)
        });
        if let Some(dur) = dur {
            let _ = tx.send((i, dur));
        }
    };
    // The bounded pool, not the global one — the same reason as
    // `sparkamp_scan_metadata`, and worse here because of DISCOVER_LOCK: a
    // caller that probes a whole selection at once put one task per file on
    // the global pool, and every task whose format Symphonia could not read
    // queued behind that single mutex while still holding a rayon worker. On
    // an optical disc the reads are slow enough that the drive went on
    // seeking for minutes after the user had stopped asking for anything.
    match crate::duration_probe::shared_probe_pool() {
        Some(pool) => pool.spawn(probe),
        None => probe(),
    }
    true
}

