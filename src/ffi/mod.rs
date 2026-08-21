//! C FFI layer — exposes Sparkamp's core to Swift via an opaque `SparkampCtx` pointer.
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
//!
//! ## Module layout
//! One file per FFI domain.  `#[no_mangle]` symbol names are unaffected by
//! module location, so functions can move between these files without any
//! change to `sparkamp_bridge.h`.  `SparkampCtx` fields are private to the
//! `ffi` module but remain visible to the child modules (Rust privacy:
//! descendant modules see a parent's private items).
// Raw pointer dereferences inside `unsafe extern "C"` functions are safe by
// construction — callers are documented to uphold the preconditions.  The
// lint is suppressed in every file of this module to keep bodies readable.
#![allow(unsafe_op_in_unsafe_fn)]

mod dedupe;
mod devices;
mod disc;
mod eq;
mod granite;
mod id3;
mod lyrics;
mod media_library;
mod now_playing;
mod playback;
mod playlist;
mod queue;
mod settings;
mod viz;

use std::ffi::CString;
use std::os::raw::{c_char, c_double, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use crate::config::Config;
use crate::engine::{Player, PlayerState};
use crate::media_library::MediaLibrary;
use crate::model::Playlist;
use crate::shuffle::ShuffleState;

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
    /// Manual play queue (session-only). Drained ahead of shuffle/linear
    /// advance via the shared `Controller`; manipulated by the mac queue FFI.
    queue: crate::queue::Queue,
    /// Sender half kept in the ctx so `sparkamp_scan_metadata` can clone it for
    /// each Rayon task.  Receiver half is polled in `sparkamp_tick`.
    ///
    /// Keyed by `Track::id`, not by row. A row index is only true for as long
    /// as nothing above it moves, and a background read outlives that: drop
    /// forty files, reorder or delete a row while they are still being read,
    /// and every result still in flight lands on whatever track now occupies
    /// the index it remembered. Entry ids are assigned once by `Playlist::add`
    /// and travel with the track through any reorder.
    meta_tx: mpsc::Sender<(u64, String, String, String)>,
    meta_rx: mpsc::Receiver<(u64, String, String, String)>,
    /// Sender half kept in the ctx so `sparkamp_probe_duration` can clone it for
    /// each Rayon task.  Receiver half is polled in `sparkamp_tick`.
    /// Keyed by `Track::id` — same reason as `meta_tx`.
    duration_tx: mpsc::Sender<(u64, Duration)>,
    duration_rx: mpsc::Receiver<(u64, Duration)>,
    /// Incremented each time `sparkamp_tick` applies any pending result (duration or
    /// metadata). Swift calls `sparkamp_take_playlist_dirty_count` to read and reset
    /// this counter so it knows when to refresh playlist rows.
    dirty_count: u32,
    /// Last duration successfully reported by GStreamer while playing/paused.
    /// Kept after stop so the seek bar and time display remain correct.
    last_known_duration: Option<Duration>,
    /// Fractional position to restore once the freshly loaded pipeline can
    /// report a duration. Set when a ReplayGain chain change forces a reload
    /// mid-track; drained by `sparkamp_tick`. Mirrors GTK's `pending_seek`.
    pending_seek: Option<f64>,
    // Callback slots — set from Swift main thread, called from `sparkamp_tick`.
    eos_cb: Option<unsafe extern "C" fn(*mut c_void)>,
    eos_userdata: *mut c_void,
    error_cb: Option<unsafe extern "C" fn(*mut c_void, *const c_char)>,
    error_userdata: *mut c_void,
    position_cb: Option<unsafe extern "C" fn(*mut c_void, c_double, c_double)>,
    position_userdata: *mut c_void,

    // ── Media Library ────────────────────────────────────────────────────────
    /// Main-thread read/query connection.  Populated by `sparkamp_ml_open`.
    media_library: Option<MediaLibrary>,
    /// High 32 bits = total files to scan; low 32 bits = files scanned so far.
    ml_progress: Arc<AtomicU64>,
    /// True while a background scan is running.
    ml_scanning: Arc<AtomicBool>,
    /// Set to true to request scan cancellation.
    ml_cancel: Arc<AtomicBool>,
    /// ReplayGain analysis background-job progress (packed done/total like
    /// `ml_progress`), running flag, and cancel flag. A separate set from the
    /// metadata-scan atomics so an RG analysis and a scan report independently.
    rg_progress: Arc<AtomicU64>,
    rg_running: Arc<AtomicBool>,
    rg_cancel: Arc<AtomicBool>,

    /// Live background filesystem watcher (Phase 8 Task 9). `None` when
    /// `watch_folders` is off, the library isn't open, or the last start
    /// attempt failed (degraded to manual rescan — see `rebuild_watcher`).
    watch: Option<crate::watch::FolderWatcher>,
    /// Receiver half of `watch`'s event channel. Always `Some` exactly when
    /// `watch` is `Some` — both are set/cleared together by `rebuild_watcher`.
    watch_rx: Option<std::sync::mpsc::Receiver<crate::watch::WatchAction>>,
}

/// Prime the player with the current playlist track's stored ReplayGain, for
/// the FFI paths that call `player.load` directly instead of going through
/// `Controller::play_current_no_record` (which does this itself). Must be
/// called immediately before the load — `load` consumes the value.
pub(crate) fn prime_rg_for_current(ctx: &mut SparkampCtx) {
    let Some(path) = ctx
        .playlist
        .current()
        .map(|t| t.path.to_string_lossy().into_owned())
    else {
        return;
    };
    let album = crate::config::rg_album_mode(
        ctx.config.playback.replaygain.source,
        ctx.config.playback.shuffle_enabled,
    );
    crate::replaygain::prime_player_gain(
        &mut ctx.player,
        ctx.media_library.as_ref(),
        &path,
        album,
    );
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

    let (meta_tx, meta_rx) = mpsc::channel();
    let (duration_tx, duration_rx) = mpsc::channel();

    let mut ctx = Box::new(SparkampCtx {
        player,
        playlist,
        config,
        shuffle_state,
        queue: crate::queue::Queue::new(),
        meta_tx,
        meta_rx,
        duration_tx,
        duration_rx,
        dirty_count: 0,
        last_known_duration: None,
        pending_seek: None,
        eos_cb: None,
        eos_userdata: std::ptr::null_mut(),
        error_cb: None,
        error_userdata: std::ptr::null_mut(),
        position_cb: None,
        position_userdata: std::ptr::null_mut(),
        media_library: None,
        ml_progress: Arc::new(AtomicU64::new(0)),
        ml_scanning: Arc::new(AtomicBool::new(false)),
        ml_cancel: Arc::new(AtomicBool::new(false)),
        rg_progress: Arc::new(AtomicU64::new(0)),
        rg_running: Arc::new(AtomicBool::new(false)),
        rg_cancel: Arc::new(AtomicBool::new(false)),
        watch: None,
        watch_rx: None,
    });

    // Apply the saved ReplayGain chain + album-vs-track mode so the first
    // track is normalized correctly from the start (mirrors GTK/TUI startup).
    {
        let rg = &ctx.config.playback.replaygain;
        let chain = crate::engine::RgChain {
            enabled: rg.enabled,
            clip_protection: rg.clip_protection,
            fallback_db: rg.fallback_db as f64,
        };
        let album = crate::config::rg_album_mode(rg.source, ctx.config.playback.shuffle_enabled);
        ctx.player.set_replaygain(chain);
        ctx.player.set_rg_album_mode(album);
    }

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

    // Apply background metadata-scan and duration-probe results.
    // Non-blocking: mirrors GTK's glib::timeout_add_local + try_recv pattern.
    //
    // Results name a track by entry id, so finding its row is a lookup. One
    // map per tick that has any results, rather than a scan of the playlist
    // per result — with a 36k-row list the latter is a million comparisons
    // for a single dropped folder.
    let mut rows: Option<std::collections::HashMap<u64, usize>> = None;
    let mut row_of = |playlist: &Playlist, id: u64| -> Option<usize> {
        rows.get_or_insert_with(|| {
            playlist
                .tracks
                .iter()
                .enumerate()
                .map(|(i, t)| (t.id, i))
                .collect()
        })
        .get(&id)
        .copied()
    };

    while let Ok((id, title, artist, album_artist)) = ctx.meta_rx.try_recv() {
        let Some(i) = row_of(&ctx.playlist, id) else {
            continue;
        };
        let track = &mut ctx.playlist.tracks[i];
        track.title = title;
        track.artist = artist;
        track.album_artist = album_artist;
        ctx.dirty_count += 1;
    }

    while let Ok((id, dur)) = ctx.duration_rx.try_recv() {
        let Some(i) = row_of(&ctx.playlist, id) else {
            continue;
        };
        ctx.playlist.tracks[i].duration = Some(dur);
        ctx.dirty_count += 1;
    }

    // Advance a stop-with-fadeout ramp. Ahead of the bus drain so that a fade
    // expiring on this tick has already stopped the player, and everything
    // below reads one consistent state rather than the pre-stop one.
    ctx.player.poll_fadeout();

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

    // Restore the position after a ReplayGain-forced reload, as soon as the
    // new pipeline reports a duration (it cannot right after load()/play(),
    // which is why this waits for a tick instead of seeking inline).
    if let Some(fraction) = ctx.pending_seek {
        if let Some(total) = ctx.player.duration() {
            let target = Duration::from_secs_f64(total.as_secs_f64() * fraction);
            let _ = ctx.player.seek(target);
            ctx.pending_seek = None;
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

