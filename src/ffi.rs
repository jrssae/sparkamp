//! C FFI layer — exposes Sparkamp's core to Swift via an opaque `SparkampCtx` pointer.
// Raw pointer dereferences inside `unsafe extern "C"` functions are safe by
// construction — callers are documented to uphold the preconditions.  The
// lint is suppressed here to keep the function bodies readable.
#![allow(unsafe_op_in_unsafe_fn)]
//!
//! ## Threading model
//! All FFI functions (except the callback thunks themselves) are called from
//! Swift's main thread.  `sparkamp_tick` is called ~10× per second by Swift's
//! `Timer` and is the only place callbacks fire — so they also run on the main
//! thread.  Swift does **not** need to dispatch-to-main inside the callbacks.
//!
//! Background work (metadata scanning, duration probing) runs on Rayon threads.
//! Results are delivered via `std::sync::mpsc` channels — the same delivery
//! mechanism used by the GTK frontend — and applied in `sparkamp_tick` via
//! non-blocking `try_recv()` loops, mirroring GTK's `glib::timeout_add_local`.
//!
//! ## Ownership rules
//! - `sparkamp_create` allocates a `SparkampCtx` on the heap; returns a raw pointer.
//! - `sparkamp_destroy` drops it; the pointer is invalid afterward.
//! - Strings returned as `*mut c_char` are heap-allocated and must be freed with
//!   `sparkamp_free_string`. Never free them with the system `free()`.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_double, c_int, c_void};
use std::path::Path;
use std::sync::{mpsc, Mutex};
use std::time::Duration;

/// Serialises all GStreamer Discoverer calls to one at a time.
///
/// Each `discover_duration` call internally creates a GLib main loop.
/// On macOS, spinning up multiple GLib main loops simultaneously from
/// Rayon threads causes GLib's GObject type system to access freed or
/// uninitialised memory (EXC_BAD_ACCESS at 0x1).  A single Mutex is
/// sufficient: Symphonia probing (`probe_duration`) is still fully
/// parallel — only the GStreamer fallback is serialised.
static DISCOVER_LOCK: Mutex<()> = Mutex::new(());

use crate::config::Config;
use crate::controller::{Controller, NavResult};
use crate::duration_probe;
use crate::engine::{Player, PlayerState};
use crate::model::{Playlist, Track};
use crate::plugin_manager::PluginManager;
use crate::shuffle::{RepeatMode, ShuffleState};

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

/// Opaque heap object — one per running app instance.
///
/// Swift holds this as `UnsafeMutablePointer<SparkampCtx>` and passes it to
/// every FFI call.  The pointer is valid from `sparkamp_create` until
/// `sparkamp_destroy`.
pub struct SparkampCtx {
    player: Player,
    playlist: Playlist,
    config: Config,
    shuffle_state: ShuffleState,
    plugin_manager: PluginManager,
    /// Sender half kept in the ctx so `sparkamp_scan_metadata` can clone it for
    /// each Rayon task.  Receiver half is polled in `sparkamp_tick`.
    meta_tx: mpsc::Sender<(usize, String, String, String)>,
    meta_rx: mpsc::Receiver<(usize, String, String, String)>,
    /// Sender half kept in the ctx so `sparkamp_probe_duration` can clone it for
    /// each Rayon task.  Receiver half is polled in `sparkamp_tick`.
    duration_tx: mpsc::Sender<(usize, Duration)>,
    duration_rx: mpsc::Receiver<(usize, Duration)>,
    /// Incremented each time `sparkamp_tick` applies any pending result (duration or
    /// metadata). Swift calls `sparkamp_take_playlist_dirty_count` to read and reset
    /// this counter so it knows when to refresh playlist rows.
    dirty_count: u32,
    /// Last duration successfully reported by GStreamer while playing/paused.
    /// Kept after stop so the seek bar and time display remain correct.
    last_known_duration: Option<Duration>,
    // Callback slots — set from Swift main thread, called from `sparkamp_tick`.
    eos_cb: Option<unsafe extern "C" fn(*mut c_void)>,
    eos_userdata: *mut c_void,
    error_cb: Option<unsafe extern "C" fn(*mut c_void, *const c_char)>,
    error_userdata: *mut c_void,
    position_cb: Option<unsafe extern "C" fn(*mut c_void, c_double, c_double)>,
    position_userdata: *mut c_void,
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Create and return a new `SparkampCtx`.
///
/// Initialises GStreamer, loads config from disk, restores the last playlist,
/// and applies the saved volume.  Returns null on fatal error (GStreamer init
/// failure or player construction failure).
///
/// Called once at app startup before any other function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_create() -> *mut SparkampCtx {
    if gstreamer::init().is_err() {
        return std::ptr::null_mut();
    }
    gstreamer::log::set_default_threshold(gstreamer::DebugLevel::None);

    let player = match Player::new() {
        Ok(p) => p,
        Err(_) => return std::ptr::null_mut(),
    };

    let config = Config::load().unwrap_or_default();
    let playlist = Playlist::load_last().unwrap_or_default();
    let mut shuffle_state = ShuffleState::new();
    shuffle_state.enabled = config.playback.shuffle_enabled;

    let plugin_manager = PluginManager::new();

    let (meta_tx, meta_rx) = mpsc::channel();
    let (duration_tx, duration_rx) = mpsc::channel();

    let mut ctx = Box::new(SparkampCtx {
        player,
        playlist,
        config,
        shuffle_state,
        plugin_manager,
        meta_tx,
        meta_rx,
        duration_tx,
        duration_rx,
        dirty_count: 0,
        last_known_duration: None,
        eos_cb: None,
        eos_userdata: std::ptr::null_mut(),
        error_cb: None,
        error_userdata: std::ptr::null_mut(),
        position_cb: None,
        position_userdata: std::ptr::null_mut(),
    });

    // Apply persisted volume to the player.
    let vol = ctx.config.playback.volume;
    ctx.player.set_volume(vol);

    // Pre-load the current track's URI so the first sparkamp_play() call works
    // without GStreamer firing an error due to no URI being set on the pipeline.
    // We do not call play() here — startup is always paused until the user acts.
    if let Some(track) = ctx.playlist.current() {
        let uri = track.uri();
        ctx.player.load(&uri).ok();
    }

    Box::into_raw(ctx)
}

/// Destroy a context created by `sparkamp_create`.
///
/// Stops playback, saves nothing (call `sparkamp_save_config` first if needed).
/// The pointer is invalid after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_destroy(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    drop(Box::from_raw(ctx));
}

// ---------------------------------------------------------------------------
// Main tick — drives callbacks from Swift's Timer (~10 Hz)
// ---------------------------------------------------------------------------

/// Poll the GStreamer bus and fire any pending callbacks.
///
/// Call this from a `Timer` at ~10 Hz on the main thread.  It:
/// 1. Applies any pending duration-probe results to the playlist.
/// 2. Drains the GStreamer bus (fires EOS / error callbacks).
/// 3. Fires the position callback with the current playback position.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tick(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;

    // Apply background metadata-scan results (title, artist, album_artist).
    // Non-blocking: mirrors GTK's glib::timeout_add_local + try_recv pattern.
    while let Ok((index, title, artist, album_artist)) = ctx.meta_rx.try_recv() {
        if let Some(track) = ctx.playlist.tracks.get_mut(index) {
            track.title = title;
            track.artist = artist;
            track.album_artist = album_artist;
            ctx.dirty_count += 1;
        }
    }

    // Apply background duration-probe results.
    while let Ok((index, dur)) = ctx.duration_rx.try_recv() {
        if index < ctx.playlist.tracks.len() {
            ctx.playlist.tracks[index].duration = Some(dur);
            ctx.dirty_count += 1;
        }
    }

    // Drain the GStreamer message bus.
    while let Some(event) = ctx.player.poll_bus() {
        match event {
            crate::engine::BusEvent::Eos => {
                if let Some(cb) = ctx.eos_cb {
                    cb(ctx.eos_userdata);
                }
            }
            crate::engine::BusEvent::Error => {
                if let Some(cb) = ctx.error_cb {
                    let msg = CString::new("Playback error").unwrap_or_default();
                    cb(ctx.error_userdata, msg.as_ptr());
                }
            }
            crate::engine::BusEvent::RetrySpectrum => {}
        }
    }

    // Persist duration to the playlist track and last_known_duration while
    // GStreamer has it (it returns None when stopped, so we cache it here).
    if let Some(dur) = ctx.player.duration() {
        ctx.last_known_duration = Some(dur);
        let idx = ctx.playlist.current_index;
        if idx < ctx.playlist.tracks.len() {
            ctx.playlist.tracks[idx].duration = Some(dur);
        }
    }

    // Fire the position callback.
    if let Some(cb) = ctx.position_cb {
        let pos = ctx.player.position().map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let dur = ctx.player.duration()
            .or(ctx.last_known_duration)
            .map(|d| d.as_secs_f64())
            .unwrap_or(-1.0);
        cb(ctx.position_userdata, pos, dur);
    }
}

// ---------------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------------

/// Load a URI and immediately begin playing it.
///
/// The URI must be a `file://` URL or an absolute path; the player converts
/// plain paths to `file://` internally via `Track::uri()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_load_and_play(ctx: *mut SparkampCtx, uri: *const c_char) {
    if ctx.is_null() || uri.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let s = CStr::from_ptr(uri).to_string_lossy();
    ctx.player.load(s.as_ref()).ok();
    ctx.player.play().ok();
}

/// Resume playback (no-op if already playing; resumes if paused).
/// If stopped and the playlist has a current track, loads its URI first.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_play(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    // If stopped, make sure the current track is loaded before playing.
    // This handles the case where sparkamp_create pre-loading was skipped
    // (empty playlist at startup) and a track was added afterward.
    if *ctx.player.state() == PlayerState::Stopped {
        if let Some(track) = ctx.playlist.current() {
            let uri = track.uri();
            ctx.player.load(&uri).ok();
        }
    }
    ctx.player.play().ok();
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
// Playlist
// ---------------------------------------------------------------------------

/// Add an audio file or folder (recursively scanned) to the playlist.
///
/// Uses the full `Track::from_path` path — reads ID3 tags synchronously.
/// Prefer `sparkamp_playlist_add_fast` when adding many files and following
/// up with `sparkamp_scan_metadata` to fill tags in the background.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_add(ctx: *mut SparkampCtx, path: *const c_char) {
    if ctx.is_null() || path.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let s = CStr::from_ptr(path).to_string_lossy();
    let p = Path::new(s.as_ref());
    if p.is_dir() {
        ctx.playlist.add_paths(&[p]);
    } else if let Ok(track) = Track::from_path(p) {
        ctx.playlist.add(track);
    }
}

/// Fast-add a single audio file to the playlist using only the filename as a
/// temporary title (no disk I/O beyond path validation).
///
/// Returns the 0-based playlist index of the newly added track, or -1 on
/// failure (file not found, not audio, etc.).  Immediately call
/// `sparkamp_scan_metadata` and `sparkamp_probe_duration` on the returned
/// index to fill in real tags and duration in the background.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_add_fast(
    ctx: *mut SparkampCtx,
    path: *const c_char,
) -> c_int {
    if ctx.is_null() || path.is_null() {
        return -1;
    }
    let ctx = &mut *ctx;
    let s = CStr::from_ptr(path).to_string_lossy();
    let p = Path::new(s.as_ref());
    match Track::from_path_fast(p) {
        Ok(track) => {
            let idx = ctx.playlist.tracks.len() as c_int;
            ctx.playlist.add(track);
            idx
        }
        Err(_) => -1,
    }
}

/// Remove all tracks from the playlist.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_clear(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    (*ctx).playlist.clear();
}

/// Remove the track at `index` from the playlist.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_remove(ctx: *mut SparkampCtx, index: c_int) {
    if ctx.is_null() {
        return;
    }
    (*ctx).playlist.remove(index as usize);
}

/// Move the track at `from` to position `to` (drag-reorder).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_move(
    ctx: *mut SparkampCtx,
    from: c_int,
    to: c_int,
) {
    if ctx.is_null() {
        return;
    }
    (*ctx).playlist.move_track(from as usize, to as usize);
}

/// Return the number of tracks in the playlist.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_len(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    (*ctx).playlist.len() as c_int
}

/// Return the index of the currently selected track, or -1 if the playlist is empty.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_current_index(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return -1;
    }
    let ctx = &*ctx;
    if ctx.playlist.is_empty() {
        -1
    } else {
        ctx.playlist.current_index as c_int
    }
}

/// Return the title of the track at `index`. The caller must free the string
/// with `sparkamp_free_string`. Returns null if `index` is out of range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_get_title(
    ctx: *const SparkampCtx,
    index: c_int,
) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let i = index as usize;
    if i >= ctx.playlist.tracks.len() {
        return std::ptr::null_mut();
    }
    CString::new(ctx.playlist.tracks[i].title.as_str())
        .unwrap_or_default()
        .into_raw()
}

/// Return the artist of the track at `index`. Caller must free with
/// `sparkamp_free_string`. Returns null if `index` is out of range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_get_artist(
    ctx: *const SparkampCtx,
    index: c_int,
) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let i = index as usize;
    if i >= ctx.playlist.tracks.len() {
        return std::ptr::null_mut();
    }
    CString::new(ctx.playlist.tracks[i].artist.as_str())
        .unwrap_or_default()
        .into_raw()
}

/// Return the album artist (TPE2) of the track at `index`. Caller must free with
/// `sparkamp_free_string`. Returns null if `index` is out of range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_get_album_artist(
    ctx: *const SparkampCtx,
    index: c_int,
) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let i = index as usize;
    if i >= ctx.playlist.tracks.len() {
        return std::ptr::null_mut();
    }
    CString::new(ctx.playlist.tracks[i].album_artist.as_str())
        .unwrap_or_default()
        .into_raw()
}

/// Return the duration of the track at `index` in seconds, or -1 if unknown.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_get_duration(
    ctx: *const SparkampCtx,
    index: c_int,
) -> c_double {
    if ctx.is_null() {
        return -1.0;
    }
    let ctx = &*ctx;
    let i = index as usize;
    if i >= ctx.playlist.tracks.len() {
        return -1.0;
    }
    ctx.playlist.tracks[i]
        .duration
        .map(|d| d.as_secs_f64())
        .unwrap_or(-1.0)
}

/// Mark the track at `index` as broken (file missing or unreadable).
///
/// Broken tracks are skipped by navigation and shown with an error indicator
/// in the playlist.  Call this from the error callback before advancing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_mark_broken(ctx: *mut SparkampCtx, index: c_int) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let i = index as usize;
    if let Some(track) = ctx.playlist.tracks.get_mut(i) {
        track.broken = true;
    }
}

/// Return 1 if the track at `index` is marked broken (file missing or unreadable),
/// 0 otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_is_broken(
    ctx: *const SparkampCtx,
    index: c_int,
) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    let i = index as usize;
    if i >= ctx.playlist.tracks.len() {
        return 0;
    }
    ctx.playlist.tracks[i].broken as c_int
}

/// Jump to `index`, load the track, and begin playing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_jump(ctx: *mut SparkampCtx, index: c_int) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.last_known_duration = None;
    if ctx.playlist.jump_to(index as usize).is_some() {
        let uri = ctx.playlist.current().map(|t| t.uri()).unwrap_or_default();
        ctx.player.load(&uri).ok();
        ctx.player.play().ok();
        let idx = index as usize;
        ctx.shuffle_state.record_played(idx);
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
        plugin_manager: &mut ctx.plugin_manager,
    };
    match ctrl.nav_next() {
        NavResult::Target { was_playing: true } => {
            // Record in shuffle history and play.
            ctrl.play_current();
        }
        NavResult::Target { was_playing: false } => {
            // Just pre-load so position/duration queries work without playing.
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
        plugin_manager: &mut ctx.plugin_manager,
    };
    ctrl.advance_to_next_playable();
}

/// Jump to the previous track (or restart the current one) and play.
///
/// Matches GTK behaviour:
/// - pos ≥ 5 s → restart (play_current, which re-records in shuffle history).
/// - pos < 5 s → step back (play_current_no_record, so history is NOT
///   corrupted — recording again would truncate the history and prevent
///   stepping back further).
/// - Was stopped → move cursor / pre-load but do NOT start playing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_nav_prev(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let before_index = ctx.playlist.current_index;
    // Reset cached duration so UI refreshes for the new track.
    ctx.last_known_duration = None;
    let mut ctrl = Controller {
        player: &mut ctx.player,
        playlist: &mut ctx.playlist,
        config: &mut ctx.config,
        shuffle_state: &mut ctx.shuffle_state,
        plugin_manager: &mut ctx.plugin_manager,
    };
    match ctrl.nav_prev() {
        NavResult::Target { was_playing: true } => {
            let is_restart = ctrl.playlist.current_index == before_index;
            if is_restart {
                // Restart: counts as a fresh listen, re-record in history.
                ctrl.play_current();
            } else {
                // Stepping back: do NOT append to history — that would truncate
                // it and prevent further back navigation in shuffle mode.
                ctrl.play_current_no_record();
            }
        }
        NavResult::Target { was_playing: false } => {
            // Stopped: just pre-load the target track without playing.
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

/// Reload config and playlist from disk, applying the new settings immediately.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_load_config(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    if let Ok(cfg) = Config::load() {
        let vol = cfg.playback.volume;
        let shuffle = cfg.playback.shuffle_enabled;
        ctx.config = cfg;
        ctx.player.set_volume(vol);
        ctx.shuffle_state.enabled = shuffle;
    }
    if let Ok(pl) = Playlist::load_last() {
        ctx.playlist = pl;
    }
}

// ---------------------------------------------------------------------------
// Callbacks
// ---------------------------------------------------------------------------

/// Register a callback fired when the current track reaches end-of-stream.
///
/// The callback is called from the main thread (inside `sparkamp_tick`).
/// Pass null to clear the callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_eos_callback(
    ctx: *mut SparkampCtx,
    cb: Option<unsafe extern "C" fn(*mut c_void)>,
    userdata: *mut c_void,
) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.eos_cb = cb;
    ctx.eos_userdata = userdata;
}

/// Register a callback fired on a GStreamer playback error.
///
/// The `error` string is valid only for the duration of the callback; do not
/// store the pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_error_callback(
    ctx: *mut SparkampCtx,
    cb: Option<unsafe extern "C" fn(*mut c_void, *const c_char)>,
    userdata: *mut c_void,
) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.error_cb = cb;
    ctx.error_userdata = userdata;
}

/// Register a callback fired ~10× per second with the current playback position.
///
/// Arguments: `(userdata, position_seconds, duration_seconds)`.
/// `duration_seconds` is -1 when the duration is unknown.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_position_callback(
    ctx: *mut SparkampCtx,
    cb: Option<unsafe extern "C" fn(*mut c_void, c_double, c_double)>,
    userdata: *mut c_void,
) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.position_cb = cb;
    ctx.position_userdata = userdata;
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
    let ctx = &*ctx;
    let i = index as usize;
    if i >= ctx.playlist.tracks.len() {
        return;
    }
    let path = ctx.playlist.tracks[i].path.clone();
    let tx = ctx.duration_tx.clone();
    rayon::spawn(move || {
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
    });
}

// ---------------------------------------------------------------------------
// Background metadata scanning
// ---------------------------------------------------------------------------

/// Scan full ID3/Vorbis metadata for the track at `index` on a Rayon worker
/// thread.  When done, queues `(index, title, artist, album_artist)` into
/// `pending_metadata`; the next `sparkamp_tick` call applies it to the
/// playlist and increments `dirty_count`.
///
/// Call immediately after `sparkamp_playlist_add` for each newly added track
/// so the quick-added filename placeholder is replaced by real tag data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_scan_metadata(ctx: *mut SparkampCtx, index: c_int) {
    if ctx.is_null() {
        return;
    }
    let ctx = &*ctx;
    let i = index as usize;
    if i >= ctx.playlist.tracks.len() {
        return;
    }
    let path = ctx.playlist.tracks[i].path.clone();
    let tx = ctx.meta_tx.clone();
    rayon::spawn(move || {
        if let Ok(track) = crate::model::Track::from_path(&path) {
            let _ = tx.send((i, track.title, track.artist, track.album_artist));
        }
    });
}

/// Return the number of playlist updates applied by `sparkamp_tick` since the
/// last call to this function, then reset the counter to zero.
///
/// A non-zero return means at least one track's title, artist, or duration
/// changed — Swift should re-read the affected items and refresh the playlist
/// display.  Returns 0 when no background work is pending.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_take_playlist_dirty_count(ctx: *mut SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &mut *ctx;
    let n = ctx.dirty_count as c_int;
    ctx.dirty_count = 0;
    n
}

// ---------------------------------------------------------------------------
// String utilities
// ---------------------------------------------------------------------------

/// Free a string previously returned by any `sparkamp_*` function.
///
/// Do not call the system `free()` on these strings — they were allocated by
/// Rust and must be returned to Rust's allocator.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_free_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    drop(CString::from_raw(s));
}
