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
mod skin;
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
/// Load a URI, and say so when it fails.
///
/// Every FFI entry point that starts playback used to write
/// `player.load(&uri).ok()`, discarding the error, while `sparkamp_play` and
/// its siblings return `void`. So a track that could not be opened was
/// indistinguishable from one that played: no error, no log line, nothing for
/// a user to report except "it does not play". `Controller` has always
/// surfaced this (see `PlayResult::Error`), which is why GTK and the TUI show
/// a reason and only the Mac was silent.
///
/// Changing the return type of eight `extern "C"` functions is a bigger job
/// than this, so it logs, the way `json_in` does and for the same reason: a
/// silent failure nobody can diagnose is worse than a noisy one.
pub(crate) fn load_or_report<B: crate::engine::backend::AudioBackend>(
    player: &mut crate::engine::Player<B>,
    uri: &str,
) {
    if let Err(e) = player.load(uri) {
        eprintln!("[sparkamp] could not load {uri}: {e}");
    }
}

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
    // Linux plays through GStreamer, so a failed init is a dead app and
    // saying so at startup beats every later call failing on its own.
    #[cfg(not(target_os = "macos"))]
    {
        if gstreamer::init().is_err() {
            return std::ptr::null_mut();
        }
        gstreamer::log::set_default_threshold(gstreamer::DebugLevel::None);
    }
    // macOS does not. Playback, burning, ripping and duration probing all go
    // through AVFoundation, so nothing here needs GStreamer to exist — and the
    // App Store build ships none. Returning null on a failed init would have
    // made a missing plugin set the difference between an app and a bounce in
    // the Dock.

    let player = match Player::new() {
        Ok(p) => p,
        Err(_) => return std::ptr::null_mut(),
    };

    // Before anything reads the library: under the App Sandbox a stored path
    // grants nothing, and the folders are unreadable until their bookmarks are
    // resolved. Outside a sandbox no folder has one and this is a no-op.
    //
    // Here rather than in `MediaLibrary::open`, which background threads call
    // for their own connections — the grants belong to the process and are
    // taken once, not once per connection.
    match crate::media_library::MediaLibrary::open()
        .and_then(|lib| lib.restore_folder_access())
    {
        Ok(unreachable) if !unreachable.is_empty() => {
            // Not fatal, and not silent. These folders need the user to pick
            // them again, which is a UI flow the frontend owns.
            eprintln!(
                "sparkamp: {} library folder(s) could not be reopened and need re-picking: {}",
                unreachable.len(),
                unreachable.join(", ")
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("sparkamp: could not restore library folder access: {e}"),
    }

    // The same again for volumes the user has granted: USB devices and
    // optical data discs. A volume that is not plugged in right now simply
    // does not resolve, which is ordinary and keeps its row.
    match crate::media_library::MediaLibrary::open().and_then(|lib| lib.restore_volume_access()) {
        Ok(absent) if !absent.is_empty() => {
            eprintln!(
                "sparkamp: {} granted volume(s) are not attached: {}",
                absent.len(),
                absent.join(", ")
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("sparkamp: could not restore volume access: {e}"),
    }

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
        load_or_report(&mut ctx.player, &uri);
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


/// The C header and the `#[repr(C)]` structs describe one memory layout.
///
/// Nothing else checks that they agree. There is no cbindgen and no build
/// script, so `sparkamp_bridge.h` is maintained by hand next to
/// `src/ffi/*.rs`, and the only safeguard is a comment asking the next person
/// to keep them in step. A divergence does not fail to compile: Swift reads
/// the header, Rust writes its own layout, and every field past the point they
/// disagree is silently misread. It shows up on macOS only, which is the one
/// platform nothing in this repository builds.
///
/// That is not hypothetical. `bitrate_mode` was widened from 8 bytes to 16 on
/// 4 September 2026 by editing both files by hand, and nothing verified it.
///
/// This reads both declarations as text and compares them field by field. That
/// is deliberate: the two declarations are the contract, so comparing them is
/// the subject rather than an implementation detail. It also computes the C
/// layout and checks it against Rust's own `size_of`, which catches a
/// disagreement the name comparison cannot see.
#[cfg(test)]
mod layout_tests {

    use std::collections::BTreeMap;

    const HEADER: &str = include_str!("../../frontends/SparkampMac/SparkampCore/sparkamp_bridge.h");
    const RUST_MEDIA_LIBRARY: &str = include_str!("media_library.rs");
    const RUST_DEDUPE: &str = include_str!("dedupe.rs");

    /// One field, reduced to what the layout depends on.
    #[derive(Debug, PartialEq, Eq, Clone)]
    struct Field {
        name: String,
        /// Width of one element in bytes.
        width: usize,
        /// Element count; 1 for a scalar.
        count: usize,
    }

    impl Field {
        fn size(&self) -> usize {
            self.width * self.count
        }
        /// These are all naturally aligned scalars, and an array aligns as its
        /// element does.
        fn align(&self) -> usize {
            self.width
        }
    }

    /// Strip `/* ... */` and `// ...` so a comment mentioning a type cannot be
    /// read as a declaration.
    fn strip_comments(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let b = s.as_bytes();
        let (mut i, mut in_block) = (0, false);
        while i < b.len() {
            if in_block {
                if b[i..].starts_with(b"*/") {
                    in_block = false;
                    i += 2;
                } else {
                    i += 1;
                }
            } else if b[i..].starts_with(b"/*") {
                in_block = true;
                i += 2;
            } else if b[i..].starts_with(b"//") {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            } else {
                out.push(b[i] as char);
                i += 1;
            }
        }
        out
    }

    /// Fields of one `typedef struct { ... } Name;` in the header.
    fn c_struct(name: &str) -> Vec<Field> {
        let src = strip_comments(HEADER);
        let end_marker = format!("}} {name};");
        let end = src
            .find(&end_marker)
            .unwrap_or_else(|| panic!("{name} is not declared in the header"));
        let start = src[..end]
            .rfind("typedef struct {")
            .expect("a struct end without a beginning");
        let body = &src[start + "typedef struct {".len()..end];

        body.split(';')
            .filter_map(|decl| {
                let decl = decl.trim();
                if decl.is_empty() {
                    return None;
                }
                let mut parts = decl.split_whitespace();
                let ty = parts.next()?;
                let rest = parts.next()?;
                // A pointer field is written `Type *field`, so the star lands
                // on the name rather than the type. Pointers are eight bytes on
                // every target this ships to.
                let (rest, is_ptr) = match rest.strip_prefix('*') {
                    Some(r) => (r, true),
                    None => (rest, false),
                };
                let width = if is_ptr {
                    8
                } else {
                    match ty {
                        "uint8_t" => 1,
                        "int32_t" => 4,
                        "int64_t" | "double" => 8,
                        other => panic!(
                            "{name}: unhandled C type {other:?}; teach this test its width"
                        ),
                    }
                };
                let (field, count) = match rest.split_once('[') {
                    Some((f, n)) => (
                        f,
                        n.trim_end_matches(']')
                            .parse::<usize>()
                            .unwrap_or_else(|_| panic!("{name}.{f}: array length is not a number")),
                    ),
                    None => (rest, 1),
                };
                // The header spells out trailing padding that `repr(C)` adds on
                // its own, so Rust has no field to match against it. Dropping
                // it keeps the comparison to fields both sides declare; the
                // size assertion still accounts for the bytes.
                if field.starts_with("_pad") {
                    return None;
                }
                Some(Field { name: field.to_string(), width, count })
            })
            .collect()
    }

    /// Fields of one `pub struct Name { ... }` in a Rust FFI source file.
    fn rust_struct(src: &str, name: &str) -> Vec<Field> {
        let src = strip_comments(src);
        let start = src
            .find(&format!("pub struct {name} {{"))
            .unwrap_or_else(|| panic!("{name} is not declared in Rust"));
        let body_start = src[start..].find('{').unwrap() + start + 1;
        let end = src[body_start..]
            .find("\n}")
            .expect("unterminated struct")
            + body_start;
        let body = &src[body_start..end];

        body.split(',')
            .filter_map(|decl| {
                let decl = decl.trim();
                let decl = decl.strip_prefix("pub ")?;
                let (field, ty) = decl.split_once(':')?;
                let ty = ty.trim();
                let (width, count) = if let Some(inner) = ty.strip_prefix('[') {
                    let inner = inner.trim_end_matches(']');
                    let (elem, n) = inner.split_once(';')?;
                    assert_eq!(elem.trim(), "u8", "{name}.{field}: only byte arrays are mapped");
                    (1, n.trim().parse::<usize>().ok()?)
                } else {
                    let w = if ty.starts_with("*mut ") || ty.starts_with("*const ") {
                        8
                    } else {
                        match ty {
                            "c_int" => 4,
                            "i64" | "f64" => 8,
                            "u8" => 1,
                            other => panic!("{name}.{field}: unhandled Rust type {other:?}"),
                        }
                    };
                    (w, 1)
                };
                Some(Field { name: field.trim().to_string(), width, count })
            })
            .collect()
    }

    /// Offsets and total size under the C rules these types follow: each field
    /// starts at a multiple of its own alignment, and the struct is padded to a
    /// multiple of its widest member.
    fn layout(fields: &[Field]) -> (BTreeMap<String, usize>, usize) {
        let mut offsets = BTreeMap::new();
        let mut at = 0usize;
        let mut widest = 1usize;
        for f in fields {
            let a = f.align();
            widest = widest.max(a);
            at = at.div_ceil(a) * a;
            offsets.insert(f.name.clone(), at);
            at += f.size();
        }
        (offsets, at.div_ceil(widest) * widest)
    }

    fn compare(struct_name: &str, rust_src: &str, rust_size: usize) {
        let c = c_struct(struct_name);
        let r = rust_struct(rust_src, struct_name);

        assert!(!c.is_empty(), "{struct_name}: parsed no C fields");
        assert_eq!(
            r.iter().map(|f| &f.name).collect::<Vec<_>>(),
            c.iter().map(|f| &f.name).collect::<Vec<_>>(),
            "{struct_name}: the field names or their order differ between \
             sparkamp_bridge.h and the Rust struct"
        );
        for (rf, cf) in r.iter().zip(c.iter()) {
            assert_eq!(
                (rf.width, rf.count),
                (cf.width, cf.count),
                "{struct_name}.{}: width or array length differs between the header \
                 and Rust",
                rf.name
            );
        }

        let (_, c_size) = layout(&c);
        assert_eq!(
            rust_size, c_size,
            "{struct_name}: Rust lays this out as {rust_size} bytes, the header as \
             {c_size}. Swift will read every field past the disagreement wrongly."
        );
    }

    #[test]
    fn the_library_track_struct_matches_its_header_declaration() {
        compare(
            "SparkampLibTrack",
            RUST_MEDIA_LIBRARY,
            std::mem::size_of::<super::media_library::SparkampLibTrack>(),
        );
    }

    #[test]
    fn the_album_struct_matches_its_header_declaration() {
        compare(
            "SparkampAlbum",
            RUST_MEDIA_LIBRARY,
            std::mem::size_of::<super::media_library::SparkampAlbum>(),
        );
    }

    #[test]
    fn the_dedupe_structs_match_their_header_declarations() {
        compare(
            "SparkampDedupTrack",
            RUST_DEDUPE,
            std::mem::size_of::<super::dedupe::SparkampDedupTrack>(),
        );
        compare(
            "SparkampDedupGroup",
            RUST_DEDUPE,
            std::mem::size_of::<super::dedupe::SparkampDedupGroup>(),
        );
    }

}
