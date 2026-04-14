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

    // If the player is actively playing, the current track is healthy — clear
    // any stale broken flag left over from a previous failed load (e.g. the
    // file was renamed back to its original name and the user played it again).
    // Checked after the bus drain so error events have already been processed.
    if *ctx.player.state() == PlayerState::Playing {
        let idx = ctx.playlist.current_index;
        if let Some(track) = ctx.playlist.tracks.get_mut(idx) {
            if track.broken {
                track.broken = false;
                ctx.dirty_count += 1;
            }
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
// Visualizer data
// ---------------------------------------------------------------------------

/// Fill `out` with `len` spectrum display-band amplitudes, normalised to 0–1.
///
/// `len` should equal `sparkamp_get_spectrum_bands()`.  Returns zeros when no
/// audio data is available.  Caller provides the output buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_spectrum(
    ctx: *const SparkampCtx,
    out: *mut f32,
    len: c_int,
) {
    if ctx.is_null() || out.is_null() || len <= 0 {
        return;
    }
    let ctx = &*ctx;
    let n = len as usize;
    let bands = ctx.player.get_spectrum_display_bands(n as u32);
    let slice = std::slice::from_raw_parts_mut(out, n);
    for (dst, src) in slice.iter_mut().zip(bands.iter()) {
        *dst = *src as f32;
    }
}

/// Return the number of spectrum display bands currently configured.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_spectrum_bands(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 16;
    }
    (*ctx).config.visualizer.display_bands as c_int
}

/// Fill `out` with `len` waveform PCM samples in `[-1, 1]`.
///
/// Returns zeros when not enough audio has been buffered yet.
/// Caller provides the output buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_waveform(
    ctx: *const SparkampCtx,
    out: *mut f32,
    len: c_int,
) {
    if ctx.is_null() || out.is_null() || len <= 0 {
        return;
    }
    let ctx = &*ctx;
    let n = len as usize;
    let samples = ctx.player.get_waveform_samples(n);
    let slice = std::slice::from_raw_parts_mut(out, n);
    for (dst, src) in slice.iter_mut().zip(samples.iter()) {
        *dst = *src as f32;
    }
}

// ---------------------------------------------------------------------------
// Visualizer mode
// ---------------------------------------------------------------------------

/// Return the current visualizer mode: 0 = Bars, 1 = Waveform.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_viz_mode(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    match (*ctx).config.visualizer.mode {
        crate::config::VisualizerMode::Bars => 0,
        crate::config::VisualizerMode::Waveform => 1,
    }
}

/// Set the visualizer mode. 0 = Bars, 1 = Waveform.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_viz_mode(ctx: *mut SparkampCtx, mode: c_int) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.visualizer.mode = match mode {
        1 => crate::config::VisualizerMode::Waveform,
        _ => crate::config::VisualizerMode::Bars,
    };
}

/// Cycle visualizer mode: Bars → Waveform → Bars → …
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_cycle_viz_mode(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.visualizer.mode = match ctx.config.visualizer.mode {
        crate::config::VisualizerMode::Bars => crate::config::VisualizerMode::Waveform,
        crate::config::VisualizerMode::Waveform => crate::config::VisualizerMode::Bars,
    };
}

// ---------------------------------------------------------------------------
// Waveform style
// ---------------------------------------------------------------------------

/// Return the waveform rendering style: 0 = Lines, 1 = Filled.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_waveform_style(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    match (*ctx).config.visualizer.waveform_style {
        crate::config::WaveformStyle::Lines => 0,
        crate::config::WaveformStyle::Filled => 1,
    }
}

/// Set the waveform rendering style. 0 = Lines, 1 = Filled.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_waveform_style(ctx: *mut SparkampCtx, style: c_int) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.visualizer.waveform_style = match style {
        1 => crate::config::WaveformStyle::Filled,
        _ => crate::config::WaveformStyle::Lines,
    };
}

// ---------------------------------------------------------------------------
// Bars zone config
// ---------------------------------------------------------------------------

/// Return the number of color zones for the bars visualizer (1–6).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_viz_zones(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 5;
    }
    (*ctx).config.visualizer.color_zones as c_int
}

/// Set the number of color zones for the bars visualizer (clamped to 1–6).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_viz_zones(ctx: *mut SparkampCtx, count: c_int) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.visualizer.color_zones = (count as u8).clamp(1, 6);
}

/// Return the hex color string for bars zone `zone_index` (0 = bottom zone).
///
/// Caller must free the returned string with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_zone_color(
    ctx: *const SparkampCtx,
    zone_index: c_int,
) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let i = zone_index as usize;
    let color = ctx
        .config
        .visualizer
        .zone_colors
        .get(i)
        .cloned()
        .unwrap_or_else(|| "#006600".to_string());
    CString::new(color).unwrap_or_default().into_raw()
}

/// Set the hex color for bars zone `zone_index`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_zone_color(
    ctx: *mut SparkampCtx,
    zone_index: c_int,
    hex: *const c_char,
) {
    if ctx.is_null() || hex.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let i = zone_index as usize;
    let s = CStr::from_ptr(hex).to_string_lossy().into_owned();
    if i < ctx.config.visualizer.zone_colors.len() {
        ctx.config.visualizer.zone_colors[i] = s;
    }
}

// ---------------------------------------------------------------------------
// Waveform zone config
// ---------------------------------------------------------------------------

/// Return the number of color zones for the waveform visualizer (1–6).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_waveform_zones(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 5;
    }
    (*ctx).config.visualizer.waveform_color_zones as c_int
}

/// Set the number of color zones for the waveform visualizer (clamped to 1–6).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_waveform_zones(ctx: *mut SparkampCtx, count: c_int) {
    if ctx.is_null() {
        return;
    }
    (*ctx).config.visualizer.waveform_color_zones = (count as u8).clamp(1, 6);
}

/// Return the hex color string for waveform zone `zone_index` (0 = bottom zone).
///
/// Caller must free the returned string with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_waveform_zone_color(
    ctx: *const SparkampCtx,
    zone_index: c_int,
) -> *mut c_char {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let i = zone_index as usize;
    let color = ctx
        .config
        .visualizer
        .waveform_zone_colors
        .get(i)
        .cloned()
        .unwrap_or_else(|| "#006600".to_string());
    CString::new(color).unwrap_or_default().into_raw()
}

/// Set the hex color for waveform zone `zone_index`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_waveform_zone_color(
    ctx: *mut SparkampCtx,
    zone_index: c_int,
    hex: *const c_char,
) {
    if ctx.is_null() || hex.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    let i = zone_index as usize;
    let s = CStr::from_ptr(hex).to_string_lossy().into_owned();
    if i < ctx.config.visualizer.waveform_zone_colors.len() {
        ctx.config.visualizer.waveform_zone_colors[i] = s;
    }
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

// ---------------------------------------------------------------------------
// Equalizer
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_has_eq(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.player.has_eq()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_eq_enabled(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.config.equalizer.enabled
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_eq_enabled(ctx: *mut SparkampCtx, enabled: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.equalizer.enabled = enabled;
    if enabled {
        let bands = ctx.config.equalizer.effective_bands();
        ctx.player.apply_eq_bands(&bands);
        let preamp = ctx.config.equalizer.effective_preamp();
        ctx.player.set_preamp(preamp);
    } else {
        ctx.player.apply_eq_bands(&[0.0f64; 10]);
        ctx.player.set_preamp(1.0);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_eq_band(ctx: *const SparkampCtx, band: c_int) -> f32 {
    if ctx.is_null() || band < 0 || band >= 10 {
        return 0.0;
    }
    let ctx = &*ctx;
    ctx.config.equalizer.effective_bands()[band as usize] as f32
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_eq_band(ctx: *mut SparkampCtx, band: c_int, db: f32) {
    if ctx.is_null() || band < 0 || band >= 10 {
        return;
    }
    let ctx = &mut *ctx;
    let clamped = ctx.config.equalizer.set_band_gain(band as usize, db as f64);
    if ctx.config.equalizer.enabled {
        ctx.player.set_eq_band(band as usize, clamped);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_apply_eq_preset(ctx: *mut SparkampCtx, preset_index: c_int) {
    if ctx.is_null() || preset_index < 0 {
        return;
    }
    let idx = preset_index as usize;
    if idx >= crate::config::EQ_PRESETS.len() {
        return;
    }
    let ctx = &mut *ctx;
    let (name, bands) = crate::config::EQ_PRESETS[idx];
    ctx.config.equalizer.preset = name.to_string();
    ctx.config.equalizer.bands = bands.to_vec();
    if ctx.config.equalizer.enabled {
        ctx.player.apply_eq_bands(&bands);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_eq_preset_count(_ctx: *const SparkampCtx) -> c_int {
    crate::config::EQ_PRESETS.len() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_eq_preset_name(
    _ctx: *const SparkampCtx,
    preset_index: c_int,
) -> *mut c_char {
    if preset_index < 0 || preset_index as usize >= crate::config::EQ_PRESETS.len() {
        return CString::new("").unwrap().into_raw();
    }
    let name = crate::config::EQ_PRESETS[preset_index as usize].0;
    CString::new(name).unwrap_or_default().into_raw()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_preamp(ctx: *const SparkampCtx) -> f32 {
    if ctx.is_null() {
        return 1.0;
    }
    let ctx = &*ctx;
    ctx.config.equalizer.effective_preamp() as f32
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_preamp(ctx: *mut SparkampCtx, multiplier: f32) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.equalizer.preamp = (multiplier as f64).clamp(0.5, 1.5);
    if ctx.config.equalizer.enabled {
        ctx.player.set_preamp(ctx.config.equalizer.preamp);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_reset_eq(ctx: *mut SparkampCtx) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.equalizer.bands = vec![0.0f64; 10];
    ctx.config.equalizer.preset = String::new();
    ctx.config.equalizer.preamp = 1.0;
    if ctx.config.equalizer.enabled {
        ctx.player.apply_eq_bands(&[0.0f64; 10]);
        ctx.player.set_preamp(1.0);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_eq_band_label(band: c_int) -> *mut c_char {
    if band < 0 || band as usize >= crate::config::EQ_BAND_FREQS.len() {
        return CString::new("").unwrap().into_raw();
    }
    let label = crate::config::EQ_BAND_FREQS[band as usize];
    CString::new(label).unwrap_or_default().into_raw()
}

// ---------------------------------------------------------------------------
// Behavior / Settings
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_playlist_add_behavior(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    match ctx.config.behavior.playlist_add_behavior {
        crate::config::PlaylistAddBehavior::Append => 0,
        crate::config::PlaylistAddBehavior::Replace => 1,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_playlist_add_behavior(
    ctx: *mut SparkampCtx,
    value: c_int,
) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.behavior.playlist_add_behavior = match value {
        1 => crate::config::PlaylistAddBehavior::Replace,
        _ => crate::config::PlaylistAddBehavior::Append,
    };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_autoplay_on_add(ctx: *const SparkampCtx) -> bool {
    if ctx.is_null() {
        return false;
    }
    let ctx = &*ctx;
    ctx.config.behavior.autoplay_on_add
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_autoplay_on_add(ctx: *mut SparkampCtx, value: bool) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.behavior.autoplay_on_add = value;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_get_ml_rescan_interval(ctx: *const SparkampCtx) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let ctx = &*ctx;
    ctx.config.media_library.rescan_interval_mins as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_set_ml_rescan_interval(ctx: *mut SparkampCtx, mins: c_int) {
    if ctx.is_null() {
        return;
    }
    let ctx = &mut *ctx;
    ctx.config.media_library.rescan_interval_mins = if mins <= 0 {
        0
    } else {
        (mins as u64).max(1)
    };
}

// ---------------------------------------------------------------------------
// Playlist path accessor
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_playlist_get_path(
    ctx: *const SparkampCtx,
    index: c_int,
) -> *mut c_char {
    if ctx.is_null() || index < 0 {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let idx = index as usize;
    if idx >= ctx.playlist.tracks.len() {
        return std::ptr::null_mut();
    }
    let path_str = ctx.playlist.tracks[idx].path.to_string_lossy().into_owned();
    CString::new(path_str).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut())
}

// ---------------------------------------------------------------------------
// ID3 Tag Editor
// ---------------------------------------------------------------------------

pub struct SparkampTagCtx {
    path: String,
    fields: crate::id3_editor::TagFields,
    extra_frames: Vec<crate::id3_editor::ExtraFrame>,
    artwork: Option<Vec<u8>>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_open(path: *const c_char) -> *mut SparkampTagCtx {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    let path_str = match CStr::from_ptr(path).to_str() {
        Ok(s) => s.to_owned(),
        Err(_) => return std::ptr::null_mut(),
    };
    let path_buf = Path::new(&path_str);
    let fields = crate::id3_editor::read_tag_fields(path_buf);
    let extra_frames = crate::id3_editor::read_extra_frames(path_buf);
    let artwork = id3::Tag::read_from_path(path_buf)
        .ok()
        .and_then(|tag| tag.pictures().next().map(|p| p.data.clone()));
    let tag_ctx = SparkampTagCtx {
        path: path_str,
        fields,
        extra_frames,
        artwork,
    };
    Box::into_raw(Box::new(tag_ctx))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_close(tag: *mut SparkampTagCtx) {
    if tag.is_null() {
        return;
    }
    drop(Box::from_raw(tag));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_get(
    tag: *const SparkampTagCtx,
    frame_id: *const c_char,
) -> *mut c_char {
    if tag.is_null() || frame_id.is_null() {
        return CString::new("").unwrap().into_raw();
    }
    let tag = &*tag;
    let frame = CStr::from_ptr(frame_id).to_string_lossy();
    let value = match frame.as_ref() {
        "TIT2" => &tag.fields.title,
        "TPE1" => &tag.fields.artist,
        "TALB" => &tag.fields.album,
        "TPE2" => &tag.fields.album_artist,
        "TCON" => &tag.fields.genre,
        "TDRC" => &tag.fields.year,
        "TRCK" => &tag.fields.track_number,
        "TPOS" => &tag.fields.disc_number,
        "TBPM" => &tag.fields.bpm,
        "COMM" => &tag.fields.comment,
        _ => return CString::new("").unwrap().into_raw(),
    };
    CString::new(value.as_str()).unwrap_or_default().into_raw()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_set(
    tag: *mut SparkampTagCtx,
    frame_id: *const c_char,
    value: *const c_char,
) {
    if tag.is_null() || frame_id.is_null() || value.is_null() {
        return;
    }
    let tag = &mut *tag;
    let frame = CStr::from_ptr(frame_id).to_string_lossy();
    let val = CStr::from_ptr(value).to_string_lossy().into_owned();
    match frame.as_ref() {
        "TIT2" => tag.fields.title = val,
        "TPE1" => tag.fields.artist = val,
        "TALB" => tag.fields.album = val,
        "TPE2" => tag.fields.album_artist = val,
        "TCON" => tag.fields.genre = val,
        "TDRC" => tag.fields.year = val,
        "TRCK" => tag.fields.track_number = val,
        "TPOS" => tag.fields.disc_number = val,
        "TBPM" => tag.fields.bpm = val,
        "COMM" => tag.fields.comment = val,
        _ => {}
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_frame_count(tag: *const SparkampTagCtx) -> c_int {
    if tag.is_null() {
        return 0;
    }
    let tag = &*tag;
    tag.extra_frames.len() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_frame_id(
    tag: *const SparkampTagCtx,
    index: c_int,
) -> *mut c_char {
    if tag.is_null() || index < 0 {
        return CString::new("").unwrap().into_raw();
    }
    let tag = &*tag;
    let idx = index as usize;
    if idx >= tag.extra_frames.len() {
        return CString::new("").unwrap().into_raw();
    }
    CString::new(tag.extra_frames[idx].id.as_str())
        .unwrap_or_default()
        .into_raw()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_frame_value(
    tag: *const SparkampTagCtx,
    index: c_int,
) -> *mut c_char {
    if tag.is_null() || index < 0 {
        return CString::new("").unwrap().into_raw();
    }
    let tag = &*tag;
    let idx = index as usize;
    if idx >= tag.extra_frames.len() {
        return CString::new("").unwrap().into_raw();
    }
    CString::new(tag.extra_frames[idx].value.as_str())
        .unwrap_or_default()
        .into_raw()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_save(tag: *mut SparkampTagCtx) -> c_int {
    if tag.is_null() {
        return -2;
    }
    let tag = &mut *tag;
    let path = Path::new(&tag.path);
    // Check if file is read-only
    match std::fs::metadata(path).map(|m| m.permissions().readonly()) {
        Ok(true) => return -1,
        Err(_) => return -1,
        Ok(false) => {}
    }
    match crate::id3_editor::write_tag_fields(path, &tag.fields) {
        Ok(_) => 0,
        Err(_) => -2,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_get_artwork_data(
    tag: *const SparkampTagCtx,
    len_out: *mut c_int,
) -> *mut u8 {
    if tag.is_null() {
        return std::ptr::null_mut();
    }
    let tag = &*tag;
    match &tag.artwork {
        None => std::ptr::null_mut(),
        Some(bytes) => {
            if !len_out.is_null() {
                *len_out = bytes.len() as c_int;
            }
            let mut boxed: Box<[u8]> = bytes.clone().into_boxed_slice();
            let ptr = boxed.as_mut_ptr();
            std::mem::forget(boxed);
            ptr
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_free_artwork(ptr: *mut u8, len: c_int) {
    if ptr.is_null() {
        return;
    }
    drop(Box::from_raw(std::slice::from_raw_parts_mut(ptr, len as usize)));
}
