//! GTK4 main window — widget layout, callbacks, and application logic.
#![allow(deprecated)]
//!
//! ## Architecture
//!
//! All mutable runtime state is held in an [`AppState`] value that is wrapped
//! in `Rc<RefCell<AppState>>`.  GTK4 runs on a single thread, so `Rc` (rather
//! than `Arc`) is the right primitive: it is cheaper and there is no risk of
//! data races.  Each callback that needs to read or write state receives its
//! own `Rc::clone`, which is cheap (just an integer increment).
//!
//! ### Borrow discipline
//! `RefCell` enforces single-writer / multiple-reader rules at runtime.  To
//! prevent a panic, every borrow is kept as short as possible:
//! - Immutable borrows (`.borrow()`) are dropped before any mutable borrow.
//! - Mutable borrows (`.borrow_mut()`) are dropped before calling any GTK
//!   method that might re-enter a callback (e.g. `queue_draw()`).
//!
//! ## GUI features
//! - Now-playing title and artist labels
//! - Seek bar with drag-detection (prevents the tick loop from fighting user)
//! - Animated visualizer (bars / waveform, toggled with `a`; waveform fullscreen with `f`)
//! - Transport buttons: ⏮ ▶ ⏸ ⏹ ⏭
//! - Volume slider (0 – 100 %)
//! - Live search / jump overlay (`j` key)
//! - Native file-chooser for adding tracks (`n` key)
//! - `Delete` key removes the highlighted playlist row
//! - Winamp keyboard bindings: z x c v b a q

use anyhow::Result;
use glib::ControlFlow;
use gtk4::prelude::*;
// Suppress deprecated warnings for GTK4 APIs that are still widely used
// but have modern replacements (ComboBoxText, ColorButton, ListStore, TreeView, etc.)
// TODO: Migrate to modern APIs (DropDown, ListStore, TreeView, etc.) when feasible
#[allow(deprecated)]
use gtk4::{
    gdk, gdk_pixbuf, gio, glib, Adjustment, Align, Application, ApplicationWindow, Box as GtkBox,
    Button, CellRendererText, CheckButton, ColorButton, ColumnView, ColumnViewColumn,
    ContentFit, CustomSorter, DragSource, DrawingArea, DropDown, DropTarget, Entry,
    EventControllerKey, GestureClick, Grid, Image, Label, ListBox, ListBoxRow, ListStore,
    MultiSelection, Notebook, Orientation, Paned, Picture, PolicyType, Scale, ScrolledWindow,
    Separator, SignalListItemFactory, SortListModel, SpinButton, Stack, StackTransitionType,
    TreeView, TreeViewColumn,
};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use crate::{
    config::{Config, VisualizerMode, WaveformStyle},
    duration_cache::DurationCache,
    duration_probe,
    engine::{BusEvent, Player, PlayerState},
    model::{fmt_duration, Playlist, Track},
    shuffle::ShuffleState,
};
// Device sync/plan/apply logic lives in core (`crate::devices::plan`); the
// thin `device_*`/`apply_*` functions below forward to it. These two types are
// produced/consumed by that logic and the frontend, so they are shared from
// core rather than redefined here.
use crate::devices::plan::{PlaylistSyncItem, TagConflictItem};

// Disc (optical media) UI: rip dialog/worker + drive-view helpers. A child
// module so it can use this file's private AppState/gtk_safe; new disc UI
// (submit, burn) goes there, not here.
#[path = "disc.rs"]
mod disc;
use disc::{disc_overview_detail_line, selected_disc_discid};

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// All mutable runtime state backing the GTK4 window.
///
/// This struct is the single source of truth for the player, playlist, and
/// configuration.  It intentionally contains no GTK widget references;
/// those live in the surrounding closures.  This separation makes the core
/// logic independently testable without a display server.
struct AppState {
    player: Player,
    playlist: Playlist,
    config: Config,
    /// Session-only shuffle and playback-history state.
    /// Not persisted — reset on each launch.
    shuffle_state: ShuffleState,
    /// Seek fraction [0, 1] to apply on the first tick after the pipeline starts
    /// playing.  Set when the user scrubs the seek bar while the player is
    /// Stopped (pipeline not loaded), so the desired position is remembered and
    /// applied once GStreamer has a duration to seek against.
    pending_seek: Option<f64>,
    /// The most recently observed track duration.  Updated every tick while
    /// playing or paused.  Kept after stop so that seek-bar drags in the
    /// Stopped state (where GStreamer cannot report duration) can still
    /// compute and display the correct time offset.
    last_duration: Option<Duration>,
    /// When `Some(vol)`, the player was muted before play to hide the brief
    /// audio from position 0 while GStreamer starts.  The tick loop restores
    /// this volume after the pending seek is applied.
    mute_pending: Option<f64>,
    /// On-disk cache of audio file durations, keyed by canonical path.
    /// Populated by background probes and saved periodically to
    /// `~/.cache/gnomamp/duration_cache.toml`.
    duration_cache: DurationCache,
    /// Media library — open on startup, or `None` when the DB cannot be opened.
    media_lib: Option<crate::media_library::MediaLibrary>,
    /// The media library browser window, if one is currently open.
    ml_window: Option<gtk4::Window>,
    /// The ID3 tag editor window, if one is currently open.
    id3_editor_window: Option<gtk4::Window>,
    /// Callback to refresh the media library window, registered by the window itself.
    rebuild_ml_callback: Option<Rc<dyn Fn()>>,
    /// Callback to update ML scan UI in all windows, registered by each window.
    ml_scan_ui_callback: Option<Rc<dyn Fn()>>,
    /// Callback to rebuild the playlist widget, set during build().
    rebuild_pl_callback: Option<Rc<dyn Fn()>>,
    /// Callback that plays the current track and updates all UI labels, set during build().
    play_and_update_callback: Option<Rc<dyn Fn()>>,
    /// Callback that updates the marquee with a new display string, set during build().
    set_track_callback: Option<Rc<dyn Fn(&str)>>,
    /// Number of background operations (rescan, add folder, etc.) currently in flight.
    /// Used to force-exit the main loop if the user closes the main window while
    /// a background operation is still running.
    pending_bg_ops: std::cell::Cell<usize>,
    /// Path whose play has already been recorded in the media library this session.
    /// Reset to `None` when a new track starts playing so the same track can be
    /// counted again after a user-initiated restart.
    counted_play_path: Option<String>,
    /// Scan state for media library operations.
    ml_scan: Option<ScanState>,
    /// Scan state for playlist operations.
    playlist_scan: Option<ScanState>,
}

/// State for tracking background scan operations.
#[derive(Clone)]
#[allow(dead_code)]
struct ScanState {
    /// Type of scan operation.
    scan_type: ScanType,
    /// Number of files processed so far.
    current: usize,
    /// Total number of files to process.
    total: usize,
    /// Flag to signal cancellation.
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Type of scan operation.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ScanType {
    AddFolder,
    AddFiles,
    Rescan,
}

/// Shared helper: start an ML scan with the given scan type and total count.
fn start_ml_scan(
    state: &Rc<RefCell<AppState>>,
    scan_type: ScanType,
    total: usize,
) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel_clone = cancel_flag.clone();
    {
        let mut s = state.borrow_mut();
        s.ml_scan = Some(ScanState {
            scan_type,
            current: 0,
            total,
            cancel: cancel_clone,
        });
        s.pending_bg_ops.set(s.pending_bg_ops.get() + 1);
    }
    if let Some(ref cb) = state.borrow().ml_scan_ui_callback {
        cb();
    }
    cancel_flag
}

/// Shared helper: update ML scan progress and notify UI.
fn update_ml_scan_progress(state: &Rc<RefCell<AppState>>, current: usize, total: usize) {
    {
        let mut s = state.borrow_mut();
        if let Some(ref mut scan) = s.ml_scan {
            scan.current = current;
            scan.total = total;
        }
    }
    if let Some(ref cb) = state.borrow().ml_scan_ui_callback {
        cb();
    }
}

/// Shared helper: complete an ML scan and notify UI.
fn complete_ml_scan(state: &Rc<RefCell<AppState>>) {
    {
        let mut s = state.borrow_mut();
        s.ml_scan = None;
        s.pending_bg_ops.set(s.pending_bg_ops.get() - 1);
    }
    if let Some(ref cb) = state.borrow().ml_scan_ui_callback {
        cb();
    }
}

/// Shared helper: cancel an ML scan and notify UI.
fn cancel_ml_scan(state: &Rc<RefCell<AppState>>) {
    {
        let s = state.borrow_mut();
        if let Some(ref scan) = s.ml_scan {
            scan.cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    if let Some(ref cb) = state.borrow().ml_scan_ui_callback {
        cb();
    }
}

/// Shared helper: update scan UI elements based on current ml_scan state.
/// Returns true if scanning is in progress.
#[allow(dead_code)]
fn update_scan_ui_elements(
    state: &Rc<RefCell<AppState>>,
    status_label: &gtk4::Label,
    rescan_btn: &gtk4::Button,
    cancel_btn: &gtk4::Button,
) -> bool {
    let scan_state = state.borrow().ml_scan.clone();
    if let Some(scan) = scan_state {
        rescan_btn.set_visible(false);
        cancel_btn.set_visible(true);
        if scan.total > 0 {
            status_label.set_text(&format!("Reading tags {}/{}…", scan.current, scan.total));
        } else {
            status_label.set_text("Reading tags…");
        }
        true
    } else {
        rescan_btn.set_visible(true);
        cancel_btn.set_visible(false);
        status_label.set_text("");
        false
    }
}

impl AppState {
    /// Initialise `AppState` from the given playlist and config.
    ///
    /// Creates a new GStreamer player and immediately applies the configured
    /// volume.  Returns an error if the GStreamer `playbin` element is
    /// unavailable.
    fn new(playlist: Playlist, config: Config) -> Result<Self> {
        let mut player = Player::new()?;
        player.set_volume(config.playback.volume);
        // Apply the saved EQ config so the correct settings are active from
        // the very first track — even before the user opens the EQ window.
        player.apply_eq_bands(&config.equalizer.effective_bands());
        let media_lib = crate::media_library::MediaLibrary::open().ok();

        // Startup cleanup: purge any soft-deleted records from previous sessions
        {
            let db_path = crate::media_library::MediaLibrary::db_path_pub();
            std::thread::spawn(move || {
                if let Ok(lib) = crate::media_library::MediaLibrary::open_at(&db_path) {
                    let _ = lib.cleanup_on_startup();
                }
            });
        }

        let shuffle_state = {
            let mut s = ShuffleState::new();
            s.enabled = config.playback.shuffle_enabled;
            s
        };
        Ok(AppState {
            player,
            playlist,
            config,
            shuffle_state,
            pending_seek: None,
            last_duration: None,
            mute_pending: None,
            duration_cache: DurationCache::load(),
            media_lib,
            ml_window: None,
            id3_editor_window: None,
            rebuild_ml_callback: None,
            ml_scan_ui_callback: None,
            rebuild_pl_callback: None,
            play_and_update_callback: None,
            set_track_callback: None,
            pending_bg_ops: std::cell::Cell::new(0),
            counted_play_path: None,
            ml_scan: None,
            playlist_scan: None,
        })
    }

    /// Load and start playback of the track at `playlist.current_index`.
    ///
    /// Returns `Some(display_name)` so the caller can update the marquee, or
    /// `None` if the playlist is empty.  Load / play errors surface on the
    /// next `poll_bus()` call in the tick loop.
    fn play_current(&mut self) -> Option<String> {
        let track = self.playlist.current()?;
        let uri = track.uri();
        let display = track.display_name();
        // Record this track in shuffle history so the previous button can step back.
        let idx = self.playlist.current_index;
        self.shuffle_state.record_played(idx);
        // Reset so the new track can be counted when it plays long enough.
        self.counted_play_path = None;
        let _ = self.player.load(&uri);
        if self.pending_seek.is_some() {
            // HACK: GStreamer's playbin does not expose a duration query while
            // the pipeline is in the Paused state on this system, so we cannot
            // seek-before-play the way e.g. XMMS does (preroll → seek → play).
            // Instead we start playing immediately (so GStreamer decodes audio
            // and a duration becomes available) but mute first so the brief
            // audio from position 0 is inaudible.  The tick loop restores the
            // volume after it successfully applies the pending seek.
            //
            // TODO: Investigate whether a GStreamer pipeline bus watch (rather
            // than polling) could give us a reliable ASYNC_DONE + duration
            // signal that would let us seek silently before play() instead.
            self.mute_pending = Some(self.config.playback.volume);
            self.player.set_volume(0.0);
        }
        let _ = self.player.play();
        Some(display)
    }

    /// Same as `play_current()` but does not record to shuffle history.
    /// Used for back navigation via history to avoid corrupting the history cursor.
    fn play_current_no_record(&mut self) -> Option<String> {
        let track = self.playlist.current()?;
        let uri = track.uri();
        let display = track.display_name();
        // Reset so the new track can be counted when it plays long enough.
        self.counted_play_path = None;
        let _ = self.player.load(&uri);
        if self.pending_seek.is_some() {
            self.mute_pending = Some(self.config.playback.volume);
            self.player.set_volume(0.0);
        }
        let _ = self.player.play();
        Some(display)
    }

    /// Advance to the next track, respecting shuffle and repeat modes.
    ///
    /// Returns `Some(display_name)` if a next track was found, or `None` if
    /// playback should stop (end of playlist with repeat off).
    ///
    /// In shuffle mode, the session history is walked forward first (so
    /// pressing Forward after Back replays the same track) before falling
    /// back to a fresh random pick.  When stopped, fresh picks are still
    /// recorded into shuffle history so a subsequent Back can return to the
    /// original track instead of falling through to linear-prev.
    fn play_next(&mut self) -> Option<String> {
        let total = self.playlist.len();
        let current = self.playlist.current_index;
        let repeat = self.config.playback.repeat_mode;

        // Try walking forward through existing shuffle history first.
        // Seed history with the current track so even a fresh stopped-state
        // session leaves something for Back to step into afterwards.
        if self.shuffle_state.enabled {
            self.shuffle_state.ensure_seeded(current);
            if let Some(idx) = self.shuffle_state.next_from_history() {
                self.playlist.jump_to(idx);
                return if *self.player.state() != PlayerState::Stopped {
                    // History walk — don't re-record (would truncate the
                    // remaining future entries the user might still want).
                    self.play_current_no_record()
                } else {
                    self.playlist.current().map(|t| t.display_name())
                };
            }
        }

        let idx = self.shuffle_state.next_index(current, total, repeat)?;
        self.playlist.jump_to(idx);
        if *self.player.state() != PlayerState::Stopped {
            self.play_current()
        } else {
            // Stopped-state pre-load: record the fresh pick manually so the
            // shuffle history reflects the navigation even though the
            // playback layer never gets a chance to call play_current.
            self.shuffle_state.record_played(idx);
            self.playlist.current().map(|t| t.display_name())
        }
    }

    /// Implement the "back button" behaviour with shuffle history support.
    ///
    /// - ≥ 5 s elapsed → restart the current track from the beginning.
    /// - < 5 s elapsed + shuffle on → step back through session history.
    /// - < 5 s elapsed + shuffle off → linear previous track (wraps with Repeat::Playlist).
    ///
    /// Returns `Some(display_name)` of the track that will now play.
    fn play_prev(&mut self) -> Option<String> {
        let pos = self.player.position().unwrap_or(Duration::ZERO);
        let do_play = *self.player.state() != PlayerState::Stopped;

        if pos.as_secs() >= 5 {
            return if do_play {
                self.play_current()
            } else {
                self.playlist.current().map(|t| t.display_name())
            };
        }

        if self.shuffle_state.enabled {
            // Seed history with the current track if shuffle is on but
            // nothing has been recorded yet — Back after a stopped-state
            // Next must return to the original current track, not a
            // linear-prev surprise.
            self.shuffle_state.ensure_seeded(self.playlist.current_index);
            if let Some(idx) = self.shuffle_state.prev_from_history() {
                self.playlist.jump_to(idx);
                return if do_play {
                    self.play_current_no_record()
                } else {
                    self.playlist.current().map(|t| t.display_name())
                };
            }
        } else {
            if self.playlist.current_index == 0 {
                if self.config.playback.repeat_mode == crate::shuffle::RepeatMode::Playlist {
                    self.playlist.jump_to(self.playlist.len().saturating_sub(1));
                }
            } else {
                self.playlist.previous();
            }
            return if do_play {
                self.play_current()
            } else {
                self.playlist.current().map(|t| t.display_name())
            };
        }

        if self.playlist.current_index == 0 {
            return None;
        }
        self.playlist.previous();
        if do_play {
            self.play_current_no_record()
        } else {
            self.playlist.current().map(|t| t.display_name())
        }
    }

    /// Cycle the visualizer to the next built-in mode.
    ///
    /// Cycle order: Bars → Waveform → Granite → Bars.
    fn toggle_visualizer_mode(&mut self) {
        self.config.visualizer.mode = match self.config.visualizer.mode {
            VisualizerMode::Bars => VisualizerMode::Waveform,
            VisualizerMode::Waveform => VisualizerMode::Granite,
            VisualizerMode::Granite => VisualizerMode::Bars,
        };
    }

    /// Attempt to retry spectrum initialization.
    ///
    /// Returns Ok(()) if retry was initiated, Err if spectrum is not available.
    fn retry_spectrum(&mut self) -> Result<(), &'static str> {
        if !self.player.has_spectrum() {
            return Err("Spectrum element not available");
        }

        // If currently playing, just trigger a pipeline state change to help
        // re-establish links. Don't stop playback.
        let current_state = self.player.state().clone();
        if current_state == PlayerState::Playing {
            // The spectrum element is already in the pipeline; a state nudge
            // can help re-establish links if no data is flowing.
        }

        Ok(())
    }

    /// Seek to a fractional position `[0.0, 1.0]` within the current track.
    ///
    /// Values outside the range are clamped silently.  Does nothing if no
    /// track duration is available yet (e.g. during initial buffering).
    fn seek_fraction(&mut self, fraction: f64) {
        let fraction = fraction.clamp(0.0, 1.0);
        // Use the live GStreamer duration first; fall back to the cached
        // last_duration so seeks work even when the pipeline just started
        // and has not yet reported duration (e.g. right after set_state(Playing)).
        let dur = match self
            .player
            .duration()
            .or(self.last_duration)
            .or_else(|| self.playlist.current().and_then(|t| t.duration))
        {
            Some(d) => d,
            None => return,
        };
        let nanos = (fraction * dur.as_nanos() as f64) as u64;
        let _ = self.player.seek(Duration::from_nanos(nanos));
    }

    /// Seek to `fraction` immediately when playing/paused, or store it in
    /// `pending_seek` when the pipeline is stopped so it can be applied once
    /// GStreamer has a duration to seek against.
    ///
    /// This is the canonical entry point for seek-bar interaction.
    fn seek_fraction_or_pend(&mut self, fraction: f64) {
        let fraction = fraction.clamp(0.0, 1.0);
        if *self.player.state() == PlayerState::Stopped {
            self.pending_seek = Some(fraction);
        } else {
            self.seek_fraction(fraction);
        }
    }

    /// Seek forward (`secs` > 0) or backward (`secs` < 0) by that many
    /// seconds within the current track.
    ///
    /// The new position is clamped to `[0, duration]`.  Does nothing if no
    /// position or duration is available (pipeline not loaded).
    fn seek_delta_secs(&mut self, secs: f64) {
        if let (Some(pos), Some(dur)) = (self.player.position(), self.player.duration()) {
            let new_secs = (pos.as_secs_f64() + secs).clamp(0.0, dur.as_secs_f64());
            let _ = self.player.seek(Duration::from_secs_f64(new_secs));
        }
    }

    /// Pre-populate `Track.duration` for every track in the playlist from the
    /// on-disk duration cache.  Should be called once after startup so that
    /// the seek bar can display correct time immediately for known files.
    ///
    /// Also seeds `last_duration` for the current track so that seek-bar drags
    /// in the initial Stopped state work without waiting for a probe result.
    fn apply_cached_durations(&mut self) {
        for track in &mut self.playlist.tracks {
            if track.duration.is_none() {
                track.duration = self.duration_cache.get(&track.path);
            }
        }
        if *self.player.state() == PlayerState::Stopped {
            if let Some(dur) = self.playlist.current().and_then(|t| t.duration) {
                self.last_duration = Some(dur);
            }
        }
    }

    /// Apply a duration result that arrived from a background probe.
    ///
    /// Updates the matching track in the playlist, persists the value to the
    /// in-memory cache (written to disk on the next save tick), and refreshes
    /// `last_duration` when the player is stopped so seek-bar drags show the
    /// correct time immediately.
    /// Collect paths of tracks added at or after `start` that still lack a
    /// cached duration.  Pass the result straight to `duration_probe::spawn_probes`
    /// to schedule background header reads for newly-added files.
    fn uncached_paths_from(&self, start: usize) -> Vec<std::path::PathBuf> {
        self.playlist.tracks[start..]
            .iter()
            .filter(|t| t.duration.is_none())
            .map(|t| t.path.clone())
            .collect()
    }

    fn apply_probed_duration(&mut self, path: &std::path::PathBuf, dur: Duration) -> Option<usize> {
        let mut found_idx = None;
        for (i, track) in self.playlist.tracks.iter_mut().enumerate() {
            if &track.path == path {
                track.duration = Some(dur);
                found_idx = Some(i);
                break;
            }
        }
        self.duration_cache.insert(path, dur);
        // Refresh last_duration so the seek bar shows correct time right away
        // when the player is stopped (GStreamer reports None from a Null pipeline).
        if *self.player.state() == PlayerState::Stopped {
            if self.playlist.current().map(|t| &t.path) == Some(path) {
                self.last_duration = Some(dur);
            }
        }
        found_idx
    }

    /// Format a time display string for the given seek `fraction` [0.0, 1.0].
    ///
    /// Uses the live GStreamer duration when the pipeline is loaded, or falls
    /// back to the cached `last_duration` when the pipeline is Stopped (Null
    /// state) and GStreamer cannot report a duration.
    ///
    /// Returns `None` when no duration is available at all (e.g. on first
    /// launch with no track ever loaded).
    fn time_display_for_fraction(&self, fraction: f64, show_remaining: bool) -> Option<String> {
        let dur = self
            .player
            .duration()
            .or(self.last_duration)
            .or_else(|| self.playlist.current().and_then(|t| t.duration))?;
        let fraction = fraction.clamp(0.0, 1.0);
        let pos_secs = (fraction * dur.as_secs_f64()) as u64;
        if show_remaining {
            let rem_secs = dur.as_secs().saturating_sub(pos_secs);
            Some(format!("-{}:{:02}", rem_secs / 60, rem_secs % 60))
        } else {
            Some(format!("{}:{:02}", pos_secs / 60, pos_secs % 60))
        }
    }

    /// Remove the track at `index` (0-based) from the playlist.
    ///
    /// If the removed track was the one currently playing, playback of the
    /// new current track begins automatically.  If the playlist becomes empty,
    /// the player is stopped.
    ///
    /// Returns `Some(display_name)` if auto-advance triggered a new track,
    /// or `None` otherwise.  Returns `None` immediately for out-of-bounds
    /// indices (playlist is unchanged).
    fn remove_track(&mut self, index: usize) -> Option<String> {
        if index >= self.playlist.tracks.len() {
            return None;
        }
        let was_current = index == self.playlist.current_index;
        self.playlist.remove(index);

        if self.playlist.is_empty() {
            let _ = self.player.stop();
            None
        } else if was_current {
            self.play_current()
        } else {
            None
        }
    }

    /// Add a single audio file from a raw path string.
    ///
    /// Leading and trailing whitespace is trimmed before the path is
    /// resolved.  Returns `Ok(display_name)` on success or `Err(message)`
    /// on failure.  Use [`add_path`] when the input might be a directory.
    fn add_track_from_path(&mut self, raw_path: &str) -> Result<String, String> {
        let path = std::path::Path::new(raw_path.trim());
        match Track::from_path(path) {
            Ok(track) => {
                let name = track.display_name();
                self.playlist.add(track);
                Ok(name)
            }
            Err(e) => Err(format!("Cannot add '{}': {}", raw_path.trim(), e)),
        }
    }

    /// Add audio content from a filesystem path that may be a file **or** a
    /// directory.
    ///
    /// - **File**: added as a single track (delegates to [`add_track_from_path`]).
    /// - **Directory**: scanned recursively; every audio file found is added.
    ///   The scan uses [`Playlist::collect_audio_files`] which already handles
    ///   permission errors gracefully.
    ///
    /// Returns a human-readable summary string suitable for the status bar, or
    /// an error string if the path does not exist / cannot be resolved at all.
    fn add_path(&mut self, path: &std::path::Path) -> Result<String, String> {
        if path.is_dir() {
            // Recursively collect all audio files under the directory.
            let files = Playlist::collect_audio_files(path);
            let total = files.len();
            if total == 0 {
                return Err(format!("No audio files found in '{}'", path.display()));
            }
            let mut added = 0usize;
            for file in files {
                if let Ok(track) = Track::from_path(&file) {
                    self.playlist.add(track);
                    added += 1;
                }
            }
            Ok(format!(
                "Added {} / {} files from '{}'",
                added,
                total,
                path.display()
            ))
        } else {
            // Treat as a single audio file.
            self.add_track_from_path(&path.to_string_lossy())
        }
    }

    /// Poll the GStreamer message bus for end-of-stream or error events.
    ///
    /// Returns `Some(BusEvent)` when the current track ended or failed, or
    /// `None` when nothing noteworthy is pending.  The caller is responsible
    /// for marking broken tracks and advancing the playlist.
    fn poll_bus(&mut self) -> Option<BusEvent> {
        self.player.poll_bus()
    }
}

// ---------------------------------------------------------------------------
// Window construction
// ---------------------------------------------------------------------------

/// Build and present the SparkAmp main window and companion playlist window.
///
/// ## Layout overview
///
/// **Main window** (always visible):
/// ```text
/// [mini viz | title / artist]   ← now-playing row
/// [seek bar                  ]
/// [⏮ ▶ ⏸ ⏹ ⏭  VOL  PL     ]   ← transport + PL toggle
/// [status bar                ]
/// ```
///
/// **Playlist window** (shown/hidden with `p` or the PL button):
/// ```text
/// [Playlist — N tracks              ]
/// [+ File] [+ Files] [+ Folder] [✕ Remove]
/// [scrollable playlist ListBox      ]
/// [status bar                       ]
/// ```
///
/// ## Playlist window positioning / snap
///
/// GTK4 on Wayland does **not** allow applications to control window
/// positions programmatically — the compositor exclusively manages
/// placement.  We use `set_transient_for` to hint to the window manager
/// that the playlist window belongs with the main window; most WMs will
/// group them in the taskbar and may place the playlist near the main
/// window on first display.
///
/// On X11 / XWayland, position control is possible via platform-specific
/// GDK APIs (`gdk_x11_surface_get_xid` + `XMoveWindow`), but doing so
/// requires `unsafe` FFI and is not implemented here to keep the code
/// portable.  The Winamp-style "snap within 10–20 px" behaviour would
/// require that platform path.
///
/// In practice, with `set_transient_for` and a modern WM the windows
/// behave as a logical unit: they share the taskbar and are typically
/// raised/lowered together.
/// Re-export built-in skin CSS from the skin module for use in this file.
use crate::skin::{self, render_gtk_css, SkinVars};

/// Read the user's GNOME accent-colour choice from gsettings and return
/// the matching hex string.  Falls back to GNOME's default blue when
/// gsettings is unavailable or the value is unrecognised.
/// Returns the label for the repeat button based on the current mode.
fn repeat_btn_icon(mode: crate::shuffle::RepeatMode) -> &'static str {
    match mode {
        // Song mode shows the dedicated "repeat single" icon. Off and All
        // share the generic "repeat" icon — the .mode-btn-active class on
        // the button distinguishes Off (inactive) from All (active).
        crate::shuffle::RepeatMode::Song => "media-playlist-repeat-song-symbolic",
        crate::shuffle::RepeatMode::Off | crate::shuffle::RepeatMode::Playlist =>
            "media-playlist-repeat-symbolic",
    }
}

/// Returns the visible text for the repeat button — mirrors the macOS
/// PlayerWindow repeatLabel ("Repeat", "Repeat 1", "Repeat All").
fn repeat_btn_text(mode: crate::shuffle::RepeatMode) -> &'static str {
    match mode {
        crate::shuffle::RepeatMode::Off => "Repeat",
        crate::shuffle::RepeatMode::Song => "Repeat 1",
        crate::shuffle::RepeatMode::Playlist => "Repeat All",
    }
}

fn gtk_safe(s: &str) -> String {
    if s.contains('\0') {
        s.replace('\0', "")
    } else {
        s.to_owned()
    }
}

fn sanitize_id3_text(s: &str) -> String {
    gtk_safe(s.trim())
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(256)
        .collect()
}

fn sanitize_id3_numeric(s: &str) -> String {
    let trimmed = s.trim();
    let numeric: String = trimmed.chars().filter(|c| c.is_ascii_digit()).collect();
    numeric.chars().take(8).collect()
}

fn format_last_played(iso_timestamp: &str) -> String {
    if iso_timestamp.is_empty() {
        return String::new();
    }
    let parts: Vec<&str> = iso_timestamp
        .trim_end_matches('Z')
        .split(|c| c == 'T' || c == ':' || c == '-')
        .collect();
    if parts.len() < 5 {
        return iso_timestamp.to_string();
    }
    let year = parts[0];
    let month = parts[1];
    let day = parts[2];
    let hour: u32 = parts.get(3).and_then(|h| h.parse().ok()).unwrap_or(0);
    let minute = parts.get(4).unwrap_or(&"00");
    let (hour_12, am_pm) = if hour == 0 {
        (12, "AM")
    } else if hour < 12 {
        (hour, "AM")
    } else if hour == 12 {
        (12, "PM")
    } else {
        (hour - 12, "PM")
    };
    format!(
        "{}-{}-{} {:02}:{} {}",
        year, month, day, hour_12, minute, am_pm
    )
}

#[allow(deprecated)]
fn make_genre_combo(initial_value: &str) -> (gtk4::DropDown, gtk4::Entry) {
    // First item clears the genre; a custom (non-ID3v1) value gets its own
    // item so the dropdown reflects what's actually in the tag. The genre
    // list itself is shown alphabetically (ID3v1 declaration order is
    // meaningless to a human scanning the dropdown).
    const UNDEFINED: &str = "(undefined)";
    let mut items: Vec<&str> = Vec::with_capacity(crate::id3_editor::ID3V1_GENRES.len() + 2);
    items.push(UNDEFINED);
    if !initial_value.is_empty()
        && !crate::id3_editor::ID3V1_GENRES.contains(&initial_value)
    {
        items.push(initial_value);
    }
    let mut genres: Vec<&str> = crate::id3_editor::ID3V1_GENRES.to_vec();
    genres.sort_unstable_by_key(|g| g.to_ascii_lowercase());
    items.extend_from_slice(&genres);
    let dd = DropDown::from_strings(&items);

    // Hidden value carrier — the save handler reads this entry, so the
    // dropdown mirrors every selection into it ("" for undefined).
    let entry = Entry::new();
    entry.set_width_chars(16);
    entry.set_text(initial_value);

    let selected = if initial_value.is_empty() {
        0
    } else {
        items.iter().position(|g| *g == initial_value).unwrap_or(0)
    };
    dd.set_selected(selected as u32);

    {
        let entry_sync = entry.clone();
        dd.connect_selected_notify(move |d| {
            let text = d
                .selected_item()
                .and_then(|o| o.downcast::<gtk4::StringObject>().ok())
                .map(|s| s.string().to_string())
                .unwrap_or_default();
            entry_sync.set_text(if text == UNDEFINED { "" } else { &text });
        });
    }

    (dd, entry)
}

/// Make a sidebar/manage playlist row draggable, carrying `pl:<id>` so a drop
/// onto a device row can send the whole playlist.
fn attach_pl_row_drag(row: &gtk4::ListBoxRow, id: i64) {
    let src = gtk4::DragSource::new();
    src.set_actions(gdk::DragAction::COPY);
    let payload = format!("pl:{id}");
    src.connect_prepare(move |_, _, _| {
        Some(gdk::ContentProvider::for_value(&payload.to_value()))
    });
    row.add_controller(src);
}

/// Index of the ML sidebar's Devices header (= the end of the Playlists
/// section). New playlist rows insert here so they land inside the Playlists
/// section rather than below Devices.
fn sidebar_pl_end_index(sidebar: &gtk4::ListBox) -> i32 {
    let mut idx = 0i32;
    while let Some(r) = sidebar.row_at_index(idx) {
        if r.widget_name() == "devices" {
            return idx;
        }
        idx += 1;
    }
    idx
}

/// Find a ListBoxRow by its widget name.
fn find_row_by_name(listbox: &gtk4::ListBox, name: &str) -> Option<gtk4::ListBoxRow> {
    let mut child = listbox.first_child();
    while let Some(c) = child {
        if let Ok(row) = c.clone().downcast::<gtk4::ListBoxRow>() {
            if row.widget_name().as_str() == name {
                return Some(row);
            }
        }
        child = c.next_sibling();
    }
    None
}

/// Show a modal alert parented to `parent` (avoids the "GtkDialog mapped
/// without a transient parent" warning).
fn show_alert_parented(parent: Option<&gtk4::Window>, msg: &str) {
    let alert = gtk4::AlertDialog::builder()
        .message("Sparkamp")
        .detail(msg)
        .modal(true)
        .build();
    alert.show(parent);
}

/// Embedded app logo PNG bytes (compiled into the binary).
/// Replace `square logo.png` in the project root with the SparkAmp logo asset.
static LOGO_BYTES: &[u8] = include_bytes!("../../square logo.png");

/// Load the app logo as a pixbuf scaled to `size × size` pixels.
/// Returns `None` if the PNG fails to decode (handled gracefully so the
/// rest of the UI still starts up even if the asset is missing).
fn load_logo_pixbuf(size: i32) -> Option<gdk_pixbuf::Pixbuf> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    loader.write(LOGO_BYTES).ok()?;
    loader.close().ok()?;
    let pb = loader.pixbuf()?;
    pb.scale_simple(size, size, gdk_pixbuf::InterpType::Bilinear)
}

/// Set up a 100ms polling timer that drains the three scan channels and updates
/// the playlist UI.  Shared by "Add Folder" and "Add Files" so both use identical
/// behaviour.
///
/// `scan_start` is the index into `playlist.tracks` where this scan's tracks begin.
/// It is captured at the moment the user confirms the dialog, before any tracks are
/// added, so that `playlist.tracks[scan_start + scan_index]` always addresses the
/// right track during the metadata phase.
///
/// ## Poller phases
/// 1. **Fast phase** – drain up to 100 fast tracks per tick, rebuild once per batch.
/// 2. **Transition** – when the first metadata message arrives, all fast tracks are
///    guaranteed to have been sent (the background thread completes Phase 1 before
///    starting Phase 2).  Drain any remaining fast tracks, rebuild, spawn duration
///    probes for all newly-added tracks.
/// 3. **Metadata phase** – patch `playlist.tracks[scan_start + idx]` in O(1);
///    rebuild every 5 ticks (~500 ms) so tags fill in gradually.
/// 4. **Done** – drain any remaining metadata, final rebuild, clear scan state.
fn start_playlist_scan_poller(
    state: std::rc::Rc<RefCell<AppState>>,
    status: Label,
    rebuild: std::rc::Rc<dyn Fn()>,
    cancel_btn: Button,
    probe_tx: std::sync::mpsc::Sender<(std::path::PathBuf, std::time::Duration)>,
    broken_tx: std::sync::mpsc::Sender<std::path::PathBuf>,
    patch_row: std::rc::Rc<dyn Fn(usize)>,
    // Called when Phase 2 updates the currently playing track's metadata so the
    // marquee immediately reflects the new "Artist - Title" display name.
    set_track: std::rc::Rc<dyn Fn(&str)>,
    fast_rx: std::sync::mpsc::Receiver<crate::model::Track>,
    meta_rx: std::sync::mpsc::Receiver<(usize, String, String, String, String)>,
    done_rx: std::sync::mpsc::Receiver<usize>,
    phase1_done_rx: std::sync::mpsc::Receiver<usize>,
    scan_start: usize,
) {
    use gtk4::prelude::*;
    use std::cell::Cell;

    // How many fast tracks this scan has added to state.playlist so far.
    let fast_added = Cell::new(0usize);
    // True once the scan thread has confirmed it finished sending all Phase 1 tracks.
    // We wait for this signal before treating an empty fast_rx as "exhausted" —
    // without it we'd give up on Phase 1 as soon as the channel is momentarily
    // empty (e.g. while the scan thread is still walking the directory).
    let phase1_signal_received = Cell::new(false);
    // True once fast_rx is empty AND phase1_signal_received — all fast tracks are
    // now in state.playlist and Phase 2 / probe spawning can proceed.
    let fast_exhausted = Cell::new(false);
    // True once duration probes have been spawned for the fast tracks.
    let probes_spawned = Cell::new(false);
    // Set when done_rx fires; we keep polling until meta_rx is also empty.
    let completion_pending = Cell::new(false);
    // True once we have done the one intermediate rebuild that shows initial filenames.
    let phase1_rebuilt = Cell::new(false);

    // Phase 1 and Phase 2 update only the in-memory model during the scan.
    // The TreeView is rebuilt once after Phase 1 (first_display) and again at
    // FINALISING.  Because TreeView virtualizes rows, a full rebuild() is O(n)
    // in data and O(visible_rows) in paint cost — no row cap needed.

    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        // ── Phase 1: add tracks to the in-memory model ───────────────────
        // We update the model here and let the TreeView render whatever is
        // visible on demand — no O(n²) layout penalty from per-row widgets.

        // Check whether the scan thread has finished sending all Phase 1 tracks.
        // We must receive this signal before treating an empty fast_rx as truly
        // exhausted — without it we would give up on tick 1 when the channel is
        // momentarily empty while the scan thread is still walking the directory.
        if !phase1_signal_received.get() && phase1_done_rx.try_recv().is_ok() {
            phase1_signal_received.set(true);
        }

        let p1_before = fast_added.get();
        if !fast_exhausted.get() {
            // Drain all available fast tracks with no per-tick cap.  The scan
            // thread produces them almost instantly (filesystem stat + canonicalize
            // only), so all tracks usually land in the channel within the first
            // 100 ms and are consumed in a single tick.
            loop {
                match fast_rx.try_recv() {
                    Ok(track) => {
                        state.borrow_mut().playlist.add(track);
                        fast_added.set(fast_added.get() + 1);
                    }
                    Err(_) => {
                        // Channel temporarily empty.  Only mark Phase 1 exhausted if
                        // the scan thread has confirmed it sent everything — otherwise
                        // the directory walk may still be in progress and more tracks
                        // will arrive on a future tick.
                        if phase1_signal_received.get() {
                            fast_exhausted.set(true);
                        }
                        break;
                    }
                }
            }
            if fast_added.get() > p1_before {
                status.set_text(&format!("Adding {}…", fast_added.get()));
            }
        }
        // Rebuild to show all Phase 1 filenames once the channel is drained.
        // Phase 2 starts immediately after and updates rows in place via
        // patch_row(), so the user sees names replace filenames live.
        if !phase1_rebuilt.get() && fast_exhausted.get() {
            phase1_rebuilt.set(true);
            rebuild();
        }

        // Once all fast tracks are in, spawn duration probes.
        if fast_exhausted.get() && !probes_spawned.get() {
            probes_spawned.set(true);
            let paths = state.borrow().uncached_paths_from(scan_start);
            if !paths.is_empty() {
                duration_probe::spawn_probes(paths, probe_tx.clone(), broken_tx.clone());
            }
            let total = fast_added.get();
            if total > 0 {
                status.set_text(&format!("Reading tags… 0/{}", total));
            }
        }

        // ── Phase 2: apply metadata and update individual rows ───────────
        // patch_row is O(1) per call: it finds the store iter by position
        // and updates that row's text in place, so live updates are cheap.
        let mut meta_drained = 0usize;
        while meta_drained < 200 {
            let Ok((idx, title, artist, album_artist, album)) = meta_rx.try_recv() else {
                break;
            };
            let playlist_idx = scan_start + idx;
            let is_current = {
                let mut s = state.borrow_mut();
                if let Some(track) = s.playlist.tracks.get_mut(playlist_idx) {
                    track.title = title;
                    track.artist = artist;
                    track.album_artist = album_artist;
                    track.album = album;
                }
                if let Some(ref mut scan) = s.playlist_scan {
                    scan.current += 1;
                }
                s.playlist.current_index == playlist_idx
            };
            // Update just this row in the ListView store; O(1), no full rebuild needed.
            patch_row(playlist_idx);
            // If Phase 2 just filled in metadata for the currently playing track,
            // refresh the marquee so it shows "Artist - Title" instead of the
            // filename that was used as a placeholder during Phase 1.
            if is_current {
                let display = state
                    .borrow()
                    .playlist
                    .tracks
                    .get(playlist_idx)
                    .map(|t| t.display_name())
                    .unwrap_or_default();
                if !display.is_empty() {
                    set_track(&display);
                }
            }
            meta_drained += 1;
        }
        // Update the status label with metadata progress.
        if meta_drained > 0 {
            let s = state.borrow();
            let current = s.playlist_scan.as_ref().map(|sc| sc.current).unwrap_or(0);
            let total = fast_added.get();
            drop(s);
            status.set_text(&format!("Reading tags… {}/{}", current, total));
        }

        // ── Completion ────────────────────────────────────────────────────
        if !completion_pending.get() && done_rx.try_recv().is_ok() {
            completion_pending.set(true);
            // Edge case: folder had no files or all failed Phase 1.
            if !probes_spawned.get() {
                probes_spawned.set(true);
                let paths = state.borrow().uncached_paths_from(scan_start);
                if !paths.is_empty() {
                    duration_probe::spawn_probes(paths, probe_tx.clone(), broken_tx.clone());
                }
            }
        }

        // Finalise when done_rx has fired, all fast tracks are received, and
        // meta_rx is drained for this tick.
        if completion_pending.get() && fast_exhausted.get() && meta_drained == 0 {
            let added = fast_added.get();
            {
                let mut s = state.borrow_mut();
                s.playlist_scan = None;
                s.pending_bg_ops.set(s.pending_bg_ops.get() - 1);
            }
            status.set_text(&format!(
                "Added {} track{}",
                added,
                if added == 1 { "" } else { "s" }
            ));
            // Apply any durations that are already in the on-disk cache for the
            // newly-added tracks, so the final rebuild can show them immediately
            // without waiting for background probes to return.
            state.borrow_mut().apply_cached_durations();
            // TreeView rebuild() is O(n) in data and O(visible_rows) in paint —
            // no row cap needed; all tracks are inserted and rendered efficiently.
            rebuild();
            cancel_btn.set_visible(false);
            return ControlFlow::Break;
        }

        ControlFlow::Continue
    });
}

/// Determine the default initial folder for the playlist Save dialog.
/// Mirrors `SparkampModel.mlDefaultSaveAsDir()` on macOS:
///
/// 1. First watched ML folder if one exists on disk.
/// 2. The current user's `~/Music` folder.
/// 3. The home directory as a last-resort fallback.
///
/// Avoids defaulting to Sparkamp's managed `~/.config/sparkamp/playlists/`
/// directory — saving there has the side effect of registering that
/// internal dir as a watched folder via `add_playlist_file`.
fn default_playlist_save_dir(
    state: &std::rc::Rc<std::cell::RefCell<AppState>>,
) -> std::path::PathBuf {
    if let Some(lib) = state.borrow().media_lib.as_ref() {
        if let Ok(folders) = lib.list_folders() {
            if let Some((_, p)) = folders.first() {
                let pb = std::path::PathBuf::from(p);
                if pb.exists() { return pb; }
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        let music = home.join("Music");
        if music.exists() { return music; }
        return home;
    }
    std::path::PathBuf::from("/")
}

/// Run the native Save dialog for a playlist file (`.m3u8`).  Initial
/// name is `initial_stem.m3u8`, initial folder is the first watched ML
/// folder or `~/Music`.  On accept, calls `on_accept` with the chosen
/// absolute path (extension is forced to `.m3u8` if the user didn't add
/// one).  Single helper used by every playlist-creation flow so all
/// paths share the same defaults.
fn run_playlist_save_dialog<W, F>(
    state: std::rc::Rc<std::cell::RefCell<AppState>>,
    win: W,
    initial_stem: &str,
    on_accept: F,
) where
    W: gtk4::prelude::IsA<gtk4::Window>,
    F: 'static + FnOnce(std::path::PathBuf, gtk4::Window),
{
    let ext = state
        .borrow()
        .config
        .media_library
        .playlist_format
        .extension();
    let dialog = gtk4::FileDialog::new();
    dialog.set_title("Save Playlist As");
    dialog.set_initial_name(Some(&format!("{initial_stem}.{ext}")));
    let initial_folder = default_playlist_save_dir(&state);
    if initial_folder.exists() {
        dialog.set_initial_folder(Some(&gio::File::for_path(&initial_folder)));
    }
    let win_for_cb: gtk4::Window = win.clone().upcast();
    let ext = ext.to_string();
    dialog.save(Some(&win), gio::Cancellable::NONE, move |res| {
        let Ok(file) = res else { return };
        let Some(mut path) = file.path() else { return };
        if path.extension().is_none() {
            path.set_extension(&ext);
        }
        on_accept(path, win_for_cb);
    });
}

thread_local! {
    /// Editor-refresh callback registered by the ML window when it opens.
    /// Any view that appends to a saved playlist (active-playlist menu,
    /// ML files menu, drag/drop onto sidebar) invokes this with the
    /// target playlist id so the open editor reloads when its current
    /// playlist is the one being modified.  No-op when no ML window is
    /// open or the hook hasn't been registered yet.
    static EDITOR_REFRESH_HOOK: RefCell<Option<Rc<dyn Fn(i64)>>> =
        const { RefCell::new(None) };

    /// Refresh the editor's currently-open playlist, regardless of which
    /// pid changed.  Fired after a track is recorded as played so the
    /// editor reflects updated last_played / play_count + the unread
    /// glyph clears alongside the files view's own refresh.
    static EDITOR_CURRENT_REFRESH_HOOK: RefCell<Option<Rc<dyn Fn()>>> =
        const { RefCell::new(None) };

    /// Re-sync the ML window's playlist navigation (sidebar sub-rows +
    /// manage list) with the playlists table.  Fired after a playlist is
    /// created outside the ML window (e.g. "Add to new playlist" in the
    /// active-playlist window) so it appears immediately.  No-op when no
    /// ML window is open.
    static PLAYLIST_NAV_REFRESH_HOOK: RefCell<Option<Rc<dyn Fn()>>> =
        const { RefCell::new(None) };
}

fn notify_playlist_changed(pid: i64) {
    EDITOR_REFRESH_HOOK.with(|h| {
        if let Some(cb) = h.borrow().as_ref() {
            cb(pid);
        }
    });
}

fn notify_editor_refresh() {
    EDITOR_CURRENT_REFRESH_HOOK.with(|h| {
        if let Some(cb) = h.borrow().as_ref() {
            cb();
        }
    });
}

fn notify_playlist_nav_refresh() {
    PLAYLIST_NAV_REFRESH_HOOK.with(|h| {
        if let Some(cb) = h.borrow().as_ref() {
            cb();
        }
    });
}

/// Build an "Add to Playlist" submenu with a leading "New Playlist…" entry
/// (bound to `new_action`, no parameter) followed by one entry per saved
/// playlist (each bound to `append_action(<playlist-id>: i64)`).  Always
/// returns a menu — "New Playlist…" is shown even when no saved playlists
/// exist so the user can seed a fresh playlist from any selection.
fn build_add_to_playlist_submenu(
    state: &std::rc::Rc<std::cell::RefCell<AppState>>,
    new_action: &str,
    append_action: &str,
) -> gio::Menu {
    let submenu = gio::Menu::new();
    let new_item = gio::MenuItem::new(Some("New Playlist…"), Some(new_action));
    submenu.append_item(&new_item);

    let playlists: Vec<(i64, String)> = state.borrow()
        .media_lib.as_ref()
        .and_then(|lib| lib.all_playlists().ok())
        .map(|v| v.into_iter().map(|p| (p.id, p.name)).collect())
        .unwrap_or_default();
    if !playlists.is_empty() {
        // Separator between "New" and the saved-playlist list — matches the
        // macOS frontend's Add-to-Playlist submenu structure.
        let saved_section = gio::Menu::new();
        for (pid, name) in playlists {
            let item = gio::MenuItem::new(Some(&name), None);
            item.set_action_and_target_value(
                Some(append_action),
                Some(&pid.to_variant()),
            );
            saved_section.append_item(&item);
        }
        submenu.append_section(None, &saved_section);
    }
    submenu
}

/// Walk every descendant of `root` looking for cell labels tagged with a
/// `pos:<N>` widget name (set by the editor column binder).  Returns the
/// canonical play-order index plus the label's vertical bounds (top + height)
/// relative to `root`.  Used by the editor's drop target to resolve a drop
/// coordinate that lands between two rows — the picked widget at that y is
/// the inner ListView, not a cell, so a coordinate-to-row scan is needed.
fn editor_cell_positions(root: &gtk4::Widget) -> Vec<(usize, f32, f32)> {
    use gtk4::prelude::*;
    let mut out: Vec<(usize, f32, f32)> = Vec::new();
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    fn walk(
        w: &gtk4::Widget,
        root: &gtk4::Widget,
        out: &mut Vec<(usize, f32, f32)>,
        seen: &mut std::collections::HashSet<usize>,
    ) {
        let name = w.widget_name().to_string();
        if let Some(rest) = name.strip_prefix("pos:") {
            if let Ok(canonical) = rest.parse::<usize>() {
                if seen.insert(canonical) {
                    if let Some(b) = w.compute_bounds(root) {
                        out.push((canonical, b.y(), b.height()));
                    }
                }
            }
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            walk(&c, root, out, seen);
            child = c.next_sibling();
        }
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        walk(&c, root, &mut out, &mut seen);
        child = c.next_sibling();
    }
    out
}

/// Show a modal AlertDialog reporting a playlist-save failure.
/// Caller-side error reporting for [`run_playlist_save_dialog`] callbacks.
fn show_playlist_save_error(parent: &gtk4::Window, target: &std::path::Path, err: &anyhow::Error) {
    let dialog = gtk4::AlertDialog::builder()
        .message("Couldn't save playlist")
        .detail(format!(
            "Failed to write {}\n\n{}",
            target.display(),
            err
        ))
        .modal(true)
        .build();
    dialog.show(Some(parent));
}

pub fn build(
    app: &Application,
    playlist: Playlist,
    config: Config,
    // Receives batches of file paths from the `open` GApplication signal so that
    // "Open with Sparkamp" in the file manager reaches the running instance
    // rather than spawning a new one.
    open_rx: std::sync::mpsc::Receiver<Vec<std::path::PathBuf>>,
) {
    // ── CSS theme ─────────────────────────────────────────────────────────────
    // Load the active skin from config. Fall back to Dark if the named
    // skin cannot be resolved.
    let initial_vars = skin::load_skin(&config.appearance.active_skin)
        .map(|s| s.vars)
        .unwrap_or_else(SkinVars::dark_defaults);
    let initial_css = render_gtk_css(&initial_vars);

    let provider = Rc::new(gtk4::CssProvider::new());
    provider.load_from_data(&initial_css);
    gtk4::style_context_add_provider_for_display(
        &gdk::Display::default().expect("No display"),
        &*provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    // Use the dark Adwaita variant for built-in widgets whenever the
    // skin's window background is dark.
    let initial_dark = initial_vars.background.luminance() < 0.5;
    if let Some(gtk_settings) = gtk4::Settings::default() {
        gtk_settings.set_gtk_application_prefer_dark_theme(initial_dark);
    }

    // Cloned Rc references used by the Appearance tab handlers.
    let provider_for_settings = provider.clone();

    // ── AppState ──────────────────────────────────────────────────────────────
    let state = match AppState::new(playlist, config) {
        Ok(s) => Rc::new(RefCell::new(s)),
        Err(e) => {
            eprintln!("Failed to initialise GStreamer player: {e}");
            return;
        }
    };

    // ── Duration probe channel ─────────────────────────────────────────────────
    // std::sync::mpsc::Sender is Clone+Send so it can be handed to Rayon
    // worker threads.  The Receiver is polled non-blocking from the tick loop
    // (try_recv), keeping the GTK main thread fully responsive.
    let (probe_tx, probe_rx) = std::sync::mpsc::channel::<(std::path::PathBuf, Duration)>();
    let (broken_tx, broken_rx) = std::sync::mpsc::channel::<std::path::PathBuf>();

    // ── Current track metadata scan channel ─────────────────────────────────────
    // When the player starts a track that has no metadata (empty artist/album_artist),
    // this channel receives the scanned metadata so we can update the marquee display.
    let (current_track_meta_tx, current_track_meta_rx) =
        std::sync::mpsc::channel::<(std::path::PathBuf, String, String, String, String)>();

    // Populate durations from the on-disk cache for the already-loaded
    // playlist, then probe any tracks that are still unknown.
    {
        state.borrow_mut().apply_cached_durations();
        let paths = state.borrow().uncached_paths_from(0);
        if !paths.is_empty() {
            duration_probe::spawn_probes(paths, probe_tx.clone(), broken_tx.clone());
        }
    }

    // ── Read window geometry from config ──────────────────────────────────────
    // All values are mutable so the display-bounds check below can clamp them.
    let init_playlist_visible = state.borrow().config.window.playlist_visible;
    let init_ml_visible = state.borrow().config.window.ml_visible;
    let mut init_player_width = state.borrow().config.window.player_width;
    let mut init_player_height = state.borrow().config.window.player_height;
    let mut init_pl_width = state.borrow().config.window.playlist_width;
    let mut init_pl_height = state.borrow().config.window.playlist_height;
    let mut init_ml_width = state.borrow().config.window.ml_width;
    let mut init_ml_height = state.borrow().config.window.ml_height;

    // Defensive: if any stored dimension exceeds the largest available monitor,
    // reset that window's geometry to first-launch defaults so it is never
    // sized off-screen.
    {
        use crate::config::WindowConfig;
        if let Some(display) = gdk::Display::default() {
            let monitors = display.monitors();
            let (mut max_w, mut max_h) = (1920i32, 1080i32);
            for i in 0..monitors.n_items() {
                if let Some(obj) = monitors.item(i) {
                    if let Ok(mon) = obj.downcast::<gdk::Monitor>() {
                        let g = mon.geometry();
                        max_w = max_w.max(g.width());
                        max_h = max_h.max(g.height());
                    }
                }
            }
            if init_player_width > max_w || init_player_height > max_h {
                init_player_width = WindowConfig::default_player_width();
                init_player_height = WindowConfig::default_player_height();
            }
            if init_pl_width > max_w || init_pl_height > max_h {
                init_pl_width = WindowConfig::default_playlist_width();
                init_pl_height = WindowConfig::default_playlist_height();
            }
            if init_ml_width > max_w || init_ml_height > max_h {
                init_ml_width = WindowConfig::default_ml_width();
                init_ml_height = WindowConfig::default_ml_height();
            }
        }
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Main window
    // ══════════════════════════════════════════════════════════════════════════

    // Player window — fixed 384 px wide. Non-resizable so the seek bar /
    // transport row / now-playing column proportions can never drift.
    let _ = init_player_width;
    let window = ApplicationWindow::builder()
        .application(app)
        .title("SparkAmp")
        .default_width(384)
        .default_height(init_player_height)
        .resizable(false)
        .build();

    let root = GtkBox::new(Orientation::Vertical, 0);

    // Deferred fullscreen opener — set after handle_key is built (chicken-and-egg).
    // Declared early so the visualiser click handler can reference it.
    let open_fullscreen_fn: Rc<RefCell<Option<Rc<dyn Fn()>>>> =
        Rc::new(RefCell::new(None));

    // ── Marquee / scrolling-title state ───────────────────────────────────────
    // The full "Title — Artist" string is stored as a Vec<char> so we can slice
    // it by character index without UTF-8 boundary arithmetic.  Each 100 ms tick
    // the scroll offset advances by 1 column; marquee_tick throttles this to
    // one advance every 3 ticks (≈ 3 chars/second — matches classic Winamp).
    let marquee_chars: Rc<RefCell<Vec<char>>> = Rc::new(RefCell::new(Vec::new()));
    let marquee_offset = Rc::new(Cell::new(0usize));
    let marquee_tick = Rc::new(Cell::new(0u32));

    // Helper: called whenever the playing track changes.  Updates the marquee
    // state and resets the scroll position to the beginning.
    let set_track: Rc<dyn Fn(&str)> = {
        let chars_ref = marquee_chars.clone();
        let off_ref = marquee_offset.clone();
        let tick_ref = marquee_tick.clone();
        Rc::new(move |display: &str| {
            *chars_ref.borrow_mut() = display.chars().collect();
            off_ref.set(0);
            tick_ref.set(0);
        })
    };

    // ── Now-playing row: [time + viz (left)] [marquee title + index (right)] ──
    // Mirrors the classic Winamp 2.x layout: visualizer left, scrolling title
    // right.  The time display (elapsed or remaining) sits just above the viz
    // and toggles on click.
    let np_row = GtkBox::new(Orientation::Horizontal, 14);
    np_row.set_margin_top(6);
    np_row.set_margin_start(8);
    np_row.set_margin_end(8);
    np_row.set_margin_bottom(2);

    // ── Left column: [state icon | time display] ABOVE the mini visualizer ────
    let left_col = GtkBox::new(Orientation::Vertical, 2);
    left_col.set_valign(Align::Center);

    // Small play/pause/stop indicator — sits inside the same dark box as
    // the time display. Class-less label inherits styling from the parent.
    // Reserve 2 character widths so the emoji glyphs (⏹/▶/⏸), which can have
    // slightly different widths depending on font fallback, can swap without
    // changing the row's natural size.
    let state_label = Label::builder()
        .label("⏹")
        .halign(Align::Center)
        .valign(Align::Center)
        .width_chars(2)
        .max_width_chars(2)
        .xalign(0.5)
        .build();

    // Time display label — single-line, monospace, centered.
    // Clicking toggles between elapsed and remaining time.
    // Reserve 7 character widths so "0:00", "12:34", and "-123:45" all
    // allocate the same horizontal slot — without this the time text grows
    // during playback and drags the whole left column wider, causing the
    // visualizer below to widen on play and shrink on stop.
    let show_remaining = Rc::new(Cell::new(false));
    let time_disp_label = Label::builder()
        .label("0:00")
        .halign(Align::Center)
        .width_chars(6)
        .max_width_chars(6)
        .xalign(0.5)
        .build();

    // Row containing [state_icon | time_display] — carries the `.time-disp`
    // dark background so both labels sit in a single box.
    let time_row = GtkBox::new(Orientation::Horizontal, 4);
    time_row.set_halign(Align::Fill);
    time_row.add_css_class("time-disp");
    time_row.append(&state_label);
    time_row.append(&time_disp_label);
    {
        let show_rem = show_remaining.clone();
        let click = GestureClick::new();
        click.connect_released(move |_, _, _, _| {
            show_rem.set(!show_rem.get());
        });
        time_row.add_controller(click);
    }

    // Mini visualizer — a Stack holding the Cairo DrawingArea (Bars / Waveform)
    // and a Picture (Granite plasma RGBA buffer). The visible child is swapped
    // to match the active visualizer mode.
    let viz = DrawingArea::new();
    viz.set_content_height(52);
    viz.set_valign(Align::Center);
    viz.set_hexpand(true);
    viz.add_css_class("mini-viz");

    let granite_pic = Picture::new();
    granite_pic.set_height_request(52);
    granite_pic.set_valign(Align::Center);
    granite_pic.set_hexpand(true);
    granite_pic.set_content_fit(ContentFit::Fill);
    granite_pic.add_css_class("mini-viz");

    let viz_stack = Stack::new();
    viz_stack.set_hexpand(true);
    viz_stack.set_valign(Align::Center);
    viz_stack.set_height_request(52);
    viz_stack.add_named(&viz, Some("cairo"));
    viz_stack.add_named(&granite_pic, Some("granite"));
    viz_stack.set_visible_child_name(
        match state.borrow().config.visualizer.mode {
            VisualizerMode::Granite => "granite",
            _ => "cairo",
        },
    );

    {
        let state_vc = state.clone();
        let open_fs_vc = open_fullscreen_fn.clone();
        let click = GestureClick::new();
        // Single click: cycle mode (or retry spectrum).
        // Double click: open fullscreen when in Waveform or Granite mode.
        // GestureClick fires `released` once per click (n_press 1 then 2),
        // so the first release of a double-click has already cycled the mode
        // by the time the second arrives. Remember the pre-click state so
        // the double-click can undo the cycle and judge fullscreen support
        // on the mode the user actually double-clicked.
        let pre_click: Rc<RefCell<Option<VisualizerMode>>> =
            Rc::new(RefCell::new(None));
        click.connect_released(move |_, n_press, _, _| {
            if n_press == 2 {
                if let Some(mode) = pre_click.borrow_mut().take() {
                    let mut s = state_vc.borrow_mut();
                    s.config.visualizer.mode = mode;
                }
                let supports_fs = matches!(
                    state_vc.borrow().config.visualizer.mode,
                    VisualizerMode::Waveform | VisualizerMode::Granite,
                );
                if supports_fs {
                    if let Some(ref opener) = *open_fs_vc.borrow() {
                        opener();
                    }
                }
                return;
            }
            let needs_retry = {
                let s = state_vc.borrow();
                !s.player.has_spectrum_data() && s.config.visualizer.mode == VisualizerMode::Bars
            };
            if needs_retry {
                *pre_click.borrow_mut() = None;
                let _ = state_vc.borrow_mut().retry_spectrum();
            } else {
                let mut s = state_vc.borrow_mut();
                *pre_click.borrow_mut() = Some(s.config.visualizer.mode.clone());
                s.toggle_visualizer_mode();
            }
        });
        // Attach the click controller to the Stack rather than each child so
        // events fire whether the Cairo DrawingArea or the Granite Picture
        // is the visible child.
        viz_stack.add_controller(click);
    }

    left_col.append(&time_row);
    left_col.append(&viz_stack);
    // Pin the left column to a fixed width (70 px). Without this, the
    // time-display string ("0:00" vs "12:34 / 45:67") would drag the column
    // wider when it grows and snap it narrower when it shrinks, jiggling
    // the visualizer below it. A fixed-width column also means the marquee
    // on the right always has the same horizontal budget.
    left_col.set_size_request(70, -1);
    time_row.set_hexpand(true);

    // ── Right column: marquee frame (title only) + index + vol row ───────────
    // `np_info` fills the full height of `np_row` so the vol row at the bottom
    // aligns horizontally with the bottom of the 68 px visualizer on the left.
    let np_info = GtkBox::new(Orientation::Vertical, 0);
    np_info.set_hexpand(true);
    np_info.set_valign(Align::Fill);

    // The `.np-frame` border wraps ONLY the scrolling title, not the vol row.
    let marquee_frame = GtkBox::new(Orientation::Vertical, 0);
    marquee_frame.add_css_class("np-frame");
    marquee_frame.set_margin_top(4);
    marquee_frame.set_margin_start(4);
    marquee_frame.set_margin_end(4);

    // Marquee label — no ellipsize; we manually slide the text window each tick.
    // single_line_mode ensures overflow is hidden at the label boundary rather
    // than wrapping to a second line.
    let title_label = Label::builder()
        .label("No track loaded")
        .halign(Align::Fill)
        .xalign(0.0) // text left-aligned within the full-width label
        .hexpand(true)
        .margin_start(8) // aligns with the VOL label start in the row below
        .single_line_mode(true)
        .css_classes(["np-title"])
        .build();

    marquee_frame.append(&title_label);
    np_info.append(&marquee_frame);

    // Expanding spring pushes the vol row to the bottom of the column so it
    // sits on the same horizontal line as the bottom of the visualizer.
    let info_spring = GtkBox::new(Orientation::Vertical, 0);
    info_spring.set_vexpand(true);
    np_info.append(&info_spring);

    np_row.append(&left_col);
    np_row.append(&np_info);
    root.append(&np_row);

    // ── Buttons created early so they can all live in the vol row ───────────
    // Mode buttons are icon-only to mirror the macOS layout's compact look.
    // The `.mode-btn-active` class is toggled by the corresponding window's
    // visible-notify handler so the icon lights up while the window is open.
    let init_repeat = state.borrow().config.playback.repeat_mode;
    // Repeat / shuffle are icon+text to match the macOS ModeButton layout.
    // Inner Image / Label refs are kept so the cycle handlers can swap both
    // when the repeat mode rotates.
    let repeat_icon = Image::from_icon_name(repeat_btn_icon(init_repeat));
    let repeat_label = Label::new(Some(repeat_btn_text(init_repeat)));
    // Reserve width for the widest mode text ("Repeat All") so the button
    // doesn't reflow when cycling between modes. xalign default 0.5 keeps
    // the icon+label visually centered inside the reserved width.
    repeat_label.set_width_chars(10);
    repeat_label.set_max_width_chars(10);
    repeat_label.set_xalign(0.5);
    let repeat_box = GtkBox::new(Orientation::Horizontal, 3);
    repeat_box.append(&repeat_icon);
    repeat_box.append(&repeat_label);
    let btn_repeat = Button::new();
    btn_repeat.set_child(Some(&repeat_box));
    btn_repeat.add_css_class("mode-btn");
    btn_repeat.set_tooltip_text(Some("Repeat: off / 1 (song) / all"));
    if init_repeat != crate::shuffle::RepeatMode::Off {
        btn_repeat.add_css_class("mode-btn-active");
    }
    let init_shuffle = state.borrow().shuffle_state.enabled;
    let shuffle_box = GtkBox::new(Orientation::Horizontal, 3);
    shuffle_box.append(&Image::from_icon_name("media-playlist-shuffle-symbolic"));
    shuffle_box.append(&Label::new(Some("Shuffle")));
    let btn_shuffle = Button::new();
    btn_shuffle.set_child(Some(&shuffle_box));
    btn_shuffle.add_css_class("mode-btn");
    btn_shuffle.set_tooltip_text(Some("Shuffle on/off"));
    if init_shuffle {
        btn_shuffle.add_css_class("mode-btn-active");
    }

    let btn_pl = Button::from_icon_name("view-list-symbolic");
    btn_pl.add_css_class("mode-btn");
    btn_pl.set_tooltip_text(Some("Playlist (p)"));
    let btn_eq = Button::from_icon_name("applications-multimedia-symbolic");
    btn_eq.add_css_class("mode-btn");
    btn_eq.set_tooltip_text(Some("10-band equalizer (u)"));
    // Size the "ⓘ" glyph to match the other mode-btn icons (which use SVG
    // icon-name buttons sized by GTK).  Pango markup avoids a global font
    // bump on every mode-btn label.
    let btn_info = {
        let lbl = Label::new(None);
        lbl.set_markup("<span size=\"x-large\">ⓘ</span>");
        let b = Button::new();
        b.set_child(Some(&lbl));
        b
    };
    btn_info.add_css_class("mode-btn");
    btn_info.set_tooltip_text(Some("Keyboard shortcuts (i)"));
    let btn_jump_vol = Button::from_icon_name("edit-find-symbolic");
    btn_jump_vol.add_css_class("mode-btn");
    btn_jump_vol.set_tooltip_text(Some("Jump to track (j)"));
    let btn_ml = Button::from_icon_name("folder-music-symbolic");
    btn_ml.add_css_class("mode-btn");
    btn_ml.set_tooltip_text(Some("Media library"));

    // ── Vol row: [VOL] [vol_bar(half-width)] [spring] [ℹ] [ML] [EQ] [PL] ───
    // Vol bar is fixed-width so it reads as secondary to the seek bar below.
    // PL is pushed to the far right with an expanding spacer.
    let vol_row = GtkBox::new(Orientation::Horizontal, 4);
    vol_row.set_margin_start(8);
    vol_row.set_margin_end(8);
    vol_row.set_margin_bottom(2);

    let vol_label = Label::builder()
        .label("VOL")
        .css_classes(["vol-label"])
        .build();

    let init_vol = state.borrow().config.playback.volume;
    let vol_adj = Adjustment::new(init_vol, 0.0, 1.0, 0.05, 0.1, 0.0);
    let vol_bar = Scale::new(Orientation::Horizontal, Some(&vol_adj));
    vol_bar.set_draw_value(false);
    vol_bar.set_hexpand(false);
    vol_bar.set_width_request(90);
    vol_bar.add_css_class("vol-scale");

    // Expanding spacer pushes PL to the right edge of np_info.
    let vol_spring = GtkBox::new(Orientation::Horizontal, 0);
    vol_spring.set_hexpand(true);

    vol_row.append(&vol_label);
    vol_row.append(&vol_bar);
    vol_row.append(&vol_spring);
    vol_row.append(&btn_info);
    vol_row.append(&btn_jump_vol);
    vol_row.append(&btn_ml);
    vol_row.append(&btn_eq);
    vol_row.append(&btn_pl);

    np_info.append(&vol_row);

    // ── Progress / seek row ───────────────────────────────────────────────────
    // Time labels have moved above the visualizer; the seek row now contains
    // only the bar itself so it reads as the dominant control in this area.
    let prog_row = GtkBox::new(Orientation::Horizontal, 4);
    prog_row.set_margin_start(8);
    prog_row.set_margin_end(8);
    prog_row.set_margin_bottom(0);

    let seek_adj = Adjustment::new(0.0, 0.0, 1.0, 0.01, 0.1, 0.0);
    let seek_bar = Scale::new(Orientation::Horizontal, Some(&seek_adj));
    seek_bar.set_draw_value(false);
    seek_bar.set_hexpand(true);
    seek_bar.add_css_class("seek-scale");

    prog_row.append(&seek_bar);
    root.append(&prog_row);

    // ── Transport buttons + GNOME logo ───────────────────────────────────────
    // Row spans the full width: buttons left-aligned, logo pinned to the right.
    let transport = GtkBox::new(Orientation::Horizontal, 8);
    transport.set_hexpand(true);
    transport.set_margin_start(8);
    transport.set_margin_end(8);
    transport.set_margin_top(8);
    transport.set_margin_bottom(8);

    let btn_prev = Button::from_icon_name("media-skip-backward-symbolic");
    let btn_play = Button::from_icon_name("media-playback-start-symbolic");
    let btn_pause = Button::from_icon_name("media-playback-pause-symbolic");
    let btn_stop = Button::from_icon_name("media-playback-stop-symbolic");
    let btn_next = Button::from_icon_name("media-skip-forward-symbolic");

    for btn in [&btn_prev, &btn_play, &btn_pause, &btn_stop, &btn_next] {
        btn.add_css_class("transport");
    }
    // `transport-play` accent is toggled dynamically by the tick loop based on
    // the engine's playback state — applied while Playing/Paused, removed when
    // Stopped — so initial Stopped state matches the visual.
    // Sparkamp skin-format CSS classes — used by skins to target individual
    // buttons with background-image overrides (.sparkamp-button-play { ... }).
    btn_prev.add_css_class("sparkamp-button-prev");
    btn_play.add_css_class("sparkamp-button-play");
    btn_pause.add_css_class("sparkamp-button-pause");
    btn_stop.add_css_class("sparkamp-button-stop");
    btn_next.add_css_class("sparkamp-button-next");

    // Load logo at ~42 px (50 % larger than the transport buttons).
    // If the PNG fails to load (e.g. asset missing), the image slot stays blank.
    const LOGO_PX: i32 = 42;
    let logo_pixbuf = load_logo_pixbuf(LOGO_PX);
    let logo_img = Image::new();
    logo_img.set_valign(Align::Center);
    logo_img.set_pixel_size(LOGO_PX);
    // Extra right-side padding so the logo's right edge aligns with the PL
    // button and progress bar end (both sit at 8px from the window edge; the
    // transport box itself already has margin_end(8)).
    logo_img.set_margin_end(8);
    if let Some(ref pb) = logo_pixbuf {
        logo_img.set_from_pixbuf(Some(pb));
    }

    // Two equal springs place repeat/shuffle equidistant between Next and logo.
    let transport_spring_l = GtkBox::new(Orientation::Horizontal, 0);
    transport_spring_l.set_hexpand(true);
    let transport_spring_r = GtkBox::new(Orientation::Horizontal, 0);
    transport_spring_r.set_hexpand(true);

    // Repeat/shuffle sit at natural (shorter) height rather than stretching
    // to fill the transport row.
    btn_repeat.set_valign(Align::Center);
    btn_shuffle.set_valign(Align::Center);

    transport.append(&btn_prev);
    transport.append(&btn_play);
    transport.append(&btn_pause);
    transport.append(&btn_stop);
    transport.append(&btn_next);
    transport.append(&transport_spring_l);
    transport.append(&btn_repeat);
    transport.append(&btn_shuffle);
    transport.append(&transport_spring_r);
    transport.append(&logo_img);
    root.append(&transport);

    // ── Status bar (main window) ──────────────────────────────────────────────
    let status_label = Label::builder()
        .label("")
        .halign(Align::Start)
        .css_classes(["status-label"])
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .build();
    status_label.set_margin_start(8);
    status_label.set_margin_end(8);
    status_label.set_margin_bottom(4);
    root.append(&status_label);
    // Hidden probe label carries .np-title CSS class.  Appended to the main
    // window root so it is realized — and its computed text color readable —
    // as soon as the main window opens, not only when the playlist opens.
    let np_probe = Label::builder()
        .css_classes(["np-title"])
        .visible(false)
        .build();
    root.append(&np_probe);

    window.set_child(Some(&root));


    // ══════════════════════════════════════════════════════════════════════════
    // Playlist window (separate, transient to main window)
    // ══════════════════════════════════════════════════════════════════════════
    //
    // `set_transient_for` groups the playlist with the main window in the
    // taskbar and prompts the WM to raise/lower them together.  On Wayland the
    // compositor controls exact placement; on X11 it opens wherever the WM
    // decides (typically near the main window).  Both windows remember their
    // last size via the config and restore it on the next launch.

    let playlist_win = ApplicationWindow::builder()
        .application(app)
        .title("SparkAmp — Playlist")
        .default_width(init_pl_width)
        .default_height(init_pl_height)
        .transient_for(&window)
        .build();

    // Mirror playlist-window visibility onto the PL toggle button so it lights
    // up while the playlist is open and dims when it closes.
    playlist_win.connect_visible_notify({
        let btn = btn_pl.clone();
        move |w| {
            if w.is_visible() {
                btn.add_css_class("mode-btn-active");
            } else {
                btn.remove_css_class("mode-btn-active");
            }
        }
    });

    let pl_root = GtkBox::new(Orientation::Vertical, 0);

    // ── Playlist window header: track count ───────────────────────────────────
    let pl_count_label = Label::builder()
        .label("Playlist — 0 tracks")
        .halign(Align::Start)
        .css_classes(["pl-count-label"])
        .build();
    pl_root.append(&pl_count_label);

    pl_root.append(&Separator::new(Orientation::Horizontal));

    // ── Playlist button bar: Add / Remove ─────────────────────────────────────
    let pl_btn_row = GtkBox::new(Orientation::Horizontal, 4);
    pl_btn_row.set_margin_start(8);
    pl_btn_row.set_margin_end(8);
    pl_btn_row.set_margin_top(4);
    pl_btn_row.set_margin_bottom(4);

    // "+ Files" opens a multi-select dialog — selecting one file also works,
    // making a separate single-file button redundant.
    let btn_add_files = Button::with_label("+ Files"); // one or more audio files
    let btn_add_dir = Button::with_label("+ Folder"); // directory (recursive scan)
    // Save the entire active playlist to an M3U8 file via the native
    // Save dialog.  Mirrors the macOS frontend's Save button.
    let btn_save_active = Button::with_label("⤓ Save");
    btn_save_active.add_css_class("pl-btn");
    btn_save_active.set_tooltip_text(Some("Save active playlist to an M3U8 file"));
    let btn_remove = Button::with_label("✕ Remove"); // remove selected row(s)
    let btn_clear_all = Button::with_label("✕ All"); // clear entire playlist
    let btn_cancel = Button::with_label("✕ Cancel Scan");
    btn_cancel.add_css_class("pl-btn");
    btn_cancel.add_css_class("destructive");
    btn_cancel.set_visible(false);

    for btn in [&btn_add_files, &btn_add_dir] {
        btn.add_css_class("pl-btn");
    }
    for btn in [&btn_remove, &btn_clear_all] {
        btn.add_css_class("pl-btn");
        btn.add_css_class("destructive");
    }

    // Left-align the add buttons; right-align destructive buttons with a flexible spacer.
    pl_btn_row.append(&btn_add_files);
    pl_btn_row.append(&btn_add_dir);
    pl_btn_row.append(&btn_save_active);
    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    pl_btn_row.append(&spacer);
    pl_btn_row.append(&btn_remove);
    pl_btn_row.append(&btn_clear_all);
    pl_btn_row.append(&btn_cancel);

    // ── Playlist TreeView + ListStore ─────────────────────────────────────────
    // GtkTreeView uses virtual scrolling — only visible rows create cell renderers,
    // so 30k+ tracks render instantly without memory pressure.
    // Four-column ListStore: position | display name | duration | font weight.
    // Col 3 (i32): Pango weight — 700 for the active track, 400 for all others.
    // Col 4 (RGBA): Foreground color — accent for active, white for selected, grey for default.
    // Using attribute binding instead of cell_data_func for reliable color updates.
    #[allow(deprecated)]
    let pl_store = ListStore::new(&[
        String::static_type(),    // col 0: position ("1.", "2.", …)
        String::static_type(),    // col 1: display name ("Artist - Title" or filename)
        String::static_type(),    // col 2: duration ("-:--" or "3:45")
        i32::static_type(),       // col 3: Pango font weight (700 = active, 400 = normal)
        gdk::RGBA::static_type(), // col 4: foreground color
    ]);

    // Shared accent RGBA populated after main window realization by reading the
    // computed color of the hidden .np-title probe label.
    let accent_rgba: Rc<RefCell<Option<gdk::RGBA>>> = Rc::new(RefCell::new(None));

    // Playlist TreeView overrides cell foreground per-row via col 4; CSS alone
    // won't reach deprecated cell renderers. Keep an Rc-shared RGBA derived
    // from the active skin's text_color, updated whenever the skin changes.
    let text_rgba: Rc<RefCell<gdk::RGBA>> = Rc::new(RefCell::new(gdk::RGBA::new(
        initial_vars.text_color.r as f32 / 255.0,
        initial_vars.text_color.g as f32 / 255.0,
        initial_vars.text_color.b as f32 / 255.0,
        1.0,
    )));

    // Deferred rebuild_playlist handle — populated later when the closure is
    // defined. Lets the logo-click and other early-bound callbacks dispatch
    // to it even though construction happens further down.
    let rebuild_pl_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> =
        Rc::new(RefCell::new(None));

    // ── Left-click on the logo → open settings window ────────────────────────
    {
        let state_rc = state.clone();
        let win_wk = window.downgrade();
        let provider_for_lclick = provider_for_settings.clone();
        let text_rgba_for_lclick = text_rgba.clone();
        let accent_rgba_for_lclick = accent_rgba.clone();
        let rebuild_pl_holder_lclick = rebuild_pl_holder.clone();
        let lclick = GestureClick::new();
        lclick.set_button(1); // primary button only
        lclick.connect_released(move |_, _, _, _| {
            let parent_win = win_wk.upgrade();
            // Fall back to a no-op if rebuild_playlist hasn't been assigned
            // yet (should never happen post-init).
            let rebuild_pl: Rc<dyn Fn()> = rebuild_pl_holder_lclick
                .borrow()
                .clone()
                .unwrap_or_else(|| Rc::new(|| {}));
            open_settings_window(
                parent_win.as_ref().map(|w| w.upcast_ref()),
                state_rc.clone(),
                None,
                provider_for_lclick.clone(),
                text_rgba_for_lclick.clone(),
                accent_rgba_for_lclick.clone(),
                rebuild_pl,
            );
        });
        logo_img.add_controller(lclick);
    }

    // Track the single-clicked row index (separate from the playing row).
    // usize::MAX means no row is selected.
    let pl_selected_idx: Rc<Cell<usize>> = Rc::new(Cell::new(usize::MAX));

    // Track the currently-playing row index (active row styling).
    // usize::MAX means no row is playing.
    let pl_active_idx: Rc<Cell<usize>> = Rc::new(Cell::new(usize::MAX));

    #[allow(deprecated)]
    let pl_view = TreeView::builder()
        .model(&pl_store)
        .headers_visible(false)
        .hexpand(true)
        .vexpand(true)
        .build();
    pl_view.add_css_class("playlist");
    #[allow(deprecated)]
    pl_view.selection().set_mode(gtk4::SelectionMode::Multiple);

    // Position column — narrow, right-aligned, monospace.
    #[allow(deprecated)]
    let pos_col = TreeViewColumn::new();
    #[allow(deprecated)]
    let pos_cell = CellRendererText::new();
    pos_cell.set_xalign(1.0);
    #[allow(deprecated)]
    pos_col.pack_start(&pos_cell, false);
    #[allow(deprecated)]
    pos_col.add_attribute(&pos_cell, "text", 0);
    #[allow(deprecated)]
    pl_view.append_column(&pos_col);

    // Name column — expands to fill remaining width, ellipsizes long strings.
    // Using add_attribute for all properties (text, weight, foreground-rgba).
    // Foreground color is stored in column 4 and updated by patch_pl_row.
    #[allow(deprecated)]
    let name_col = TreeViewColumn::new();
    name_col.set_expand(true);
    #[allow(deprecated)]
    let name_cell = CellRendererText::new();
    name_cell.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    #[allow(deprecated)]
    name_col.pack_start(&name_cell, true);
    #[allow(deprecated)]
    name_col.add_attribute(&name_cell, "text", 1);
    #[allow(deprecated)]
    name_col.add_attribute(&name_cell, "weight", 3);
    #[allow(deprecated)]
    name_col.add_attribute(&name_cell, "foreground-rgba", 4);
    #[allow(deprecated)]
    pl_view.append_column(&name_col);

    // Duration column — fixed width, right-aligned, monospace.
    #[allow(deprecated)]
    let dur_col = TreeViewColumn::new();
    #[allow(deprecated)]
    let dur_cell = CellRendererText::new();
    dur_cell.set_xalign(1.0);
    #[allow(deprecated)]
    dur_col.pack_start(&dur_cell, false);
    #[allow(deprecated)]
    dur_col.add_attribute(&dur_cell, "text", 2);
    #[allow(deprecated)]
    pl_view.append_column(&dur_col);

    let pl_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .min_content_height(350)
        .child(&pl_view)
        .build();
    pl_root.append(&pl_scroll);

    // ── Playlist window status bar ────────────────────────────────────────────
    let pl_status_label = Label::builder()
        .label("")
        .halign(Align::Start)
        .css_classes(["status-label"])
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .build();
    pl_status_label.set_margin_start(8);
    pl_status_label.set_margin_end(8);
    pl_status_label.set_margin_bottom(4);
    pl_root.append(&pl_status_label);

    // ── Playlist button bar: Add / Remove (pinned to the bottom) ─────────────
    // Mirrors the layout of classic Winamp where the playlist action buttons
    // sit below the track list rather than above it.
    pl_root.append(&Separator::new(Orientation::Horizontal));
    pl_root.append(&pl_btn_row);

    playlist_win.set_child(Some(&pl_root));

    // Closing the playlist window hides it (not destroys) so the next toggle
    // brings it back without rebuilding.  Save its size to both the in-memory
    // config (in state) and to disk so the main close handler and the next
    // launch both see the correct dimensions.
    playlist_win.connect_close_request({
        let state = state.clone();
        move |pw| {
            let (w, h) = (pw.width(), pw.height());
            // Update in-memory config so the main-window close handler reads
            // the correct size even after the playlist window is hidden
            // (a hidden GTK window reports width/height of 0).
            {
                let mut s = state.borrow_mut();
                s.config.window.playlist_width = w;
                s.config.window.playlist_height = h;
            }
            let _ = state.borrow().config.save();
            pw.set_visible(false);
            glib::Propagation::Stop
        }
    });

    // ══════════════════════════════════════════════════════════════════════════
    // Shared closures
    // ══════════════════════════════════════════════════════════════════════════

    // rebuild_playlist — repopulate the ListStore from the current playlist model.
    //
    // The TreeView is temporarily disconnected from the model while the store is
    // cleared and repopulated.  This prevents the TreeView from processing one
    // row-deleted / row-inserted signal per track (which would block the UI for
    // several seconds on a 30k-track playlist).  Reconnecting the model triggers
    // a single bulk re-read; only visible rows are painted, so it remains O(1).
    let rebuild_playlist = {
        let state = state.clone();
        let pl_store = pl_store.clone();
        let pl_view = pl_view.clone();
        let pl_count_label = pl_count_label.clone();
        let pl_active_idx = pl_active_idx.clone();
        let accent_rgba = accent_rgba.clone();
        let text_rgba = text_rgba.clone();
        Rc::new(move || {
            let s = state.borrow();
            let current = s.playlist.current_index;
            let is_playing = matches!(
                *s.player.state(),
                PlayerState::Playing | PlayerState::Paused
            );
            let n = s.playlist.tracks.len();
            // Update pl_active_idx to match current playing track.
            if is_playing {
                pl_active_idx.set(current);
            } else {
                pl_active_idx.set(usize::MAX);
            }
            // Remember the current scroll offset so a rebuild (e.g. enqueueing
            // files) repaints in place instead of jumping back to the top.
            let saved_scroll = pl_view.vadjustment().map(|a| a.value()).unwrap_or(0.0);
            // Detach TreeView so bulk model changes don't trigger per-row signals.
            #[allow(deprecated)]
            pl_view.set_model(None::<&ListStore>);
            #[allow(deprecated)]
            pl_store.clear();
            for (i, t) in s.playlist.tracks.iter().enumerate() {
                let lock_suffix = if t.read_only { " 🔒" } else { "" };
                let pos = format!("{}.{}", i + 1, lock_suffix);
                let name = t.display_name();
                let is_active = is_playing && i == current;
                let display = if t.broken {
                    format!("⚠ {}", name)
                } else if is_active {
                    format!("▶ {}", name)
                } else {
                    name
                };
                let weight: i32 = if is_active { 700 } else { 400 };
                // Compute foreground color.  Active (playing) rows get the
                // skin's highlight/accent; everything else (including the
                // GTK-selected row) uses the skin's text color.
                let fg_rgba = if is_active {
                    accent_rgba
                        .borrow()
                        .clone()
                        .unwrap_or_else(|| text_rgba.borrow().clone())
                } else {
                    text_rgba.borrow().clone()
                };
                #[allow(deprecated)]
                pl_store.insert_with_values(
                    None,
                    &[
                        (0, &gtk_safe(&pos) as &dyn ToValue),
                        (1, &gtk_safe(&display) as &dyn ToValue),
                        (2, &gtk_safe(&fmt_duration(t.duration)) as &dyn ToValue),
                        (3, &weight as &dyn ToValue),
                        (4, &fg_rgba as &dyn ToValue),
                    ],
                );
            }
            drop(s);
            // Reconnect — TreeView does one bulk re-read, only paints visible rows.
            #[allow(deprecated)]
            pl_view.set_model(Some(&pl_store));
            // Restore the scroll offset after layout settles (the adjustment's
            // upper bound only updates once the new rows are measured).
            if saved_scroll > 0.0 {
                if let Some(adj) = pl_view.vadjustment() {
                    glib::idle_add_local_once(move || {
                        let target = saved_scroll.min(adj.upper() - adj.page_size());
                        adj.set_value(target.max(0.0));
                    });
                }
            }
            pl_count_label.set_label(&format!(
                "Playlist — {} track{}",
                n,
                if n == 1 { "" } else { "s" },
            ));
        })
    };
    *rebuild_pl_holder.borrow_mut() = Some(rebuild_playlist.clone());

    // scroll_to_row_if_needed — scroll the playlist to make a row visible.
    //
    // Uses TreeView::visible_range + scroll_to_cell so that GTK's actual
    // rendered row heights drive the math rather than a hardcoded estimate.
    // A hardcoded estimate drifts after many skips and the row stops scrolling
    // into view.
    let scroll_to_row_if_needed = {
        let pl_scroll = pl_scroll.clone();
        let state    = state.clone();
        Rc::new(move |target_idx: usize| {
            let adj       = pl_scroll.vadjustment();
            let page_size = adj.page_size();
            let upper     = adj.upper();
            let current   = adj.value();
            let n         = state.borrow().playlist.len();

            if n == 0 || upper <= 0.0 || page_size <= 0.0 {
                return;
            }

            let row_h       = upper / n as f64;
            let row_top     = target_idx as f64 * row_h;
            let row_bottom  = row_top + row_h;
            let visible_end = current + page_size;

            if row_top < current || row_bottom > visible_end {
                let target = (row_top - page_size / 2.0 + row_h / 2.0)
                    .clamp(0.0, (upper - page_size).max(0.0));
                adj.set_value(target);
            }
        })
    };

    // patch_pl_row — update a single store row's text without a full rebuild.
    //
    // Called by the probe drain so name and duration updates appear row by row
    // as background probes complete.  O(1): finds the iter by position and
    // calls set() on just that row.
    let patch_pl_row = {
        let state = state.clone();
        let pl_store = pl_store.clone();
        let pl_active_idx = pl_active_idx.clone();
        let accent_rgba = accent_rgba.clone();
        let text_rgba = text_rgba.clone();
        Rc::new(move |idx: usize| {
            let (display, duration_str, weight, is_active) = {
                let s = state.borrow();
                let Some(t) = s.playlist.tracks.get(idx) else {
                    return;
                };
                let name = t.display_name();
                let is_playing = matches!(
                    *s.player.state(),
                    PlayerState::Playing | PlayerState::Paused
                );
                let is_active = is_playing && idx == s.playlist.current_index;
                let display = if t.broken {
                    format!("⚠ {}", name)
                } else if is_active {
                    format!("▶ {}", name)
                } else {
                    name
                };
                let weight: i32 = if is_active { 700 } else { 400 };
                (display, fmt_duration(t.duration), weight, is_active)
            };
            #[allow(deprecated)]
            let Some(iter) = pl_store.iter_nth_child(None, idx as i32) else {
                return;
            };
            // Update pl_active_idx state.
            let current_active = pl_active_idx.get();
            if is_active && current_active != idx {
                pl_active_idx.set(idx);
            } else if !is_active && current_active == idx {
                pl_active_idx.set(usize::MAX);
            }
            // Compute foreground color: active row → accent, all others → skin text.
            let fg_rgba = {
                let active_idx = pl_active_idx.get();
                let is_row_active = active_idx != usize::MAX && active_idx == idx;
                if is_row_active {
                    accent_rgba
                        .borrow()
                        .clone()
                        .unwrap_or_else(|| text_rgba.borrow().clone())
                } else {
                    text_rgba.borrow().clone()
                }
            };
            // Update name, duration, weight, and foreground color columns.
            #[allow(deprecated)]
            pl_store.set(
                &iter,
                &[
                    (1, &gtk_safe(&display) as &dyn ToValue),
                    (2, &gtk_safe(&duration_str) as &dyn ToValue),
                    (3, &weight as &dyn ToValue),
                    (4, &fg_rgba as &dyn ToValue),
                ],
            );
        })
    };

    // Handle single-click row selection changes for highlighting.
    // Updates pl_selected_idx and repaints old/new selected rows.
    {
        let pl_selected_idx = pl_selected_idx.clone();
        let patch_pl_row = patch_pl_row.clone();
        let pl_view = pl_view.clone();
        #[allow(deprecated)]
        pl_view.selection().connect_changed(move |selection| {
            // Guard against model being detached (e.g., during rebuild_playlist).
            #[allow(deprecated)]
            if pl_view.model().is_none() {
                return;
            }
            // Guard against initial model setup (count is 0 when model is initializing).
            #[allow(deprecated)]
            if selection.count_selected_rows() == 0 && pl_selected_idx.get() == usize::MAX {
                return;
            }
            let old_idx = pl_selected_idx.get();
            #[allow(deprecated)]
            let (paths, _model): (Vec<_>, _) = selection.selected_rows();
            let new_idx = paths
                .into_iter()
                .next()
                .and_then(|p| p.indices().first().copied())
                .map(|i| i as usize)
                .unwrap_or(usize::MAX);
            if old_idx != new_idx {
                pl_selected_idx.set(new_idx);
                // Repaint old and new selected rows.
                if old_idx != usize::MAX {
                    patch_pl_row(old_idx);
                }
                if new_idx != usize::MAX {
                    patch_pl_row(new_idx);
                }
            }
        });
    }

    // scan_current_track_metadata — if the current track has no metadata (empty
    // artist AND album_artist), spawn a background thread to read the ID3 tags
    // and send the result via current_track_meta_tx so the marquee can be updated.
    fn scan_current_track_metadata(
        state: &Rc<RefCell<AppState>>,
        meta_tx: std::sync::mpsc::Sender<(PathBuf, String, String, String, String)>,
    ) {
        let (path, has_metadata) = {
            let s = state.borrow();
            match s.playlist.current() {
                Some(t) => {
                    let has_meta = !t.artist.is_empty() || !t.album_artist.is_empty();
                    (t.path.clone(), has_meta)
                }
                None => return,
            }
        };
        if has_metadata {
            return;
        }
        let path_for_thread = path.clone();
        std::thread::spawn(move || {
            if let Ok(track) = crate::model::Track::from_path(&path_for_thread) {
                let _ = meta_tx.send((
                    track.path,
                    track.title,
                    track.artist,
                    track.album_artist,
                    track.album,
                ));
            }
        });
    }

    // play_and_update — play the current track and refresh the UI labels.
    //
    // All "start playing" paths (buttons, keyboard, auto-advance) funnel
    // through here so the marquee and playlist stay in sync.  Label text is
    // NOT set directly here; the 100 ms tick loop renders the marquee window
    // each frame so the scrolling starts immediately after track change.
    let play_and_update = {
        let state = state.clone();
        let set_track = set_track.clone();
        let patch_pl_row = patch_pl_row.clone();
        let scroll_to_row_if_needed = scroll_to_row_if_needed.clone();
        let current_track_meta_tx = current_track_meta_tx.clone();
        Rc::new(move || {
            // Record which row was playing before so we can un-bold it.
            let old_idx = state.borrow().playlist.current_index;
            let result = { state.borrow_mut().play_current() };
            if let Some(display) = result {
                let new_idx = state.borrow().playlist.current_index;
                set_track(&display);
                // Scan metadata for the current track if it hasn't been scanned yet.
                // This updates the marquee with "Artist - Title" once the scan completes.
                scan_current_track_metadata(&state, current_track_meta_tx.clone());
                // Scroll to make the new current track visible
                scroll_to_row_if_needed(new_idx);
                // Patch the new current track to ensure active styling is applied.
                // Also patch old track if it was different.
                if old_idx != new_idx {
                    patch_pl_row(old_idx);
                }
                patch_pl_row(new_idx);
            }
        })
    };

    // Store play/rebuild callbacks in AppState so secondary windows (dedupe,
    // etc.) can trigger playlist updates without needing direct closure refs.
    {
        let mut s = state.borrow_mut();
        s.rebuild_pl_callback = Some(rebuild_playlist.clone());
        s.play_and_update_callback = Some(play_and_update.clone());
        s.set_track_callback = Some(set_track.clone());
    }

    // remove_selected — remove every currently selected playlist row.
    //
    // Indices are sorted highest-first before removal so that earlier removes
    // do not shift the positions of later ones.  Does not delete files from
    // disk; only removes the entries from the in-memory playlist.
    let remove_selected = {
        let state = state.clone();
        let pl_view = pl_view.clone();
        let pl_scroll = pl_scroll.clone();
        let rebuild_rm = rebuild_playlist.clone();
        let set_track_rm = set_track.clone();
        Rc::new(move || {
            #[allow(deprecated)]
            let (paths, _) = pl_view.selection().selected_rows();
            let mut indices: Vec<usize> = paths
                .iter()
                .filter_map(|p| p.indices().first().copied())
                .map(|i| i as usize)
                .collect();
            if indices.is_empty() {
                return;
            }
            // Highest first so earlier removes don't invalidate later indices.
            indices.sort_unstable_by(|a, b| b.cmp(a));
            let mut last_nowplaying: Option<String> = None;
            for idx in indices {
                if let Some(display) = { state.borrow_mut().remove_track(idx) } {
                    last_nowplaying = Some(display);
                }
            }
            if let Some(display) = last_nowplaying {
                set_track_rm(&display);
            }
            // Save and restore the scroll position around the rebuild so the
            // visible region doesn't jump after a removal.
            let adj = pl_scroll.vadjustment();
            let saved_scroll = adj.value();
            rebuild_rm();
            // The model re-attach resets the scroll; restore on next idle tick
            // after GTK has committed the new layout.
            glib::idle_add_local_once(move || {
                adj.set_value(saved_scroll);
            });
        })
    };

    // ── Initial state ─────────────────────────────────────────────────────────

    rebuild_playlist();
    {
        let s = state.borrow();
        if let Some(t) = s.playlist.current() {
            set_track(&t.display_name());
        }
    }

    // ── DragSource on the TreeView ──────────────────────────────────────────
    // Persistent multi-selection snapshot for the active playlist.
    // Updated by selection.connect_changed whenever count > 1.
    // Cleared only on count==0 AND when a drag isn't in progress —
    // GtkTreeView transiently drops to 0 selected rows during the drag
    // event chain, and clearing then would wipe the snapshot before
    // the drop target gets a chance to read it.
    let pl_drag_selection: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let pl_drag_active: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    {
        let snap_obs = pl_drag_selection.clone();
        let active_obs = pl_drag_active.clone();
        #[allow(deprecated)]
        pl_view.selection().connect_changed(move |sel| {
            #[allow(deprecated)]
            let count = sel.count_selected_rows() as usize;
            if count > 1 {
                #[allow(deprecated)]
                let (paths, _model) = sel.selected_rows();
                let v: Vec<usize> = paths.iter()
                    .filter_map(|p| p.indices().first().copied())
                    .map(|i| i as usize)
                    .collect();
                *snap_obs.borrow_mut() = v;
            } else if count == 0 && !active_obs.get() {
                snap_obs.borrow_mut().clear();
            } else {
            }
        });
    }

    // Press-time selection restorer — re-applies the multi-select
    // visually so the drag-icon shows every dragged row's highlight,
    // not just the row under the cursor.  GTK's default press handler
    // collapses selection to the clicked row; we schedule an idle
    // restore that runs after that collapse but before drag-icon
    // rendering settles.
    {
        let press = GestureClick::new();
        press.set_button(gtk4::gdk::BUTTON_PRIMARY);
        let pl_view_p = pl_view.clone();
        let snap = pl_drag_selection.clone();
        let active_press = pl_drag_active.clone();
        press.connect_pressed(move |_, _n, x, y| {
            #[allow(deprecated)]
            let row_under = pl_view_p.path_at_pos(x as i32, y as i32)
                .and_then(|(p, _, _, _)| p)
                .and_then(|p| p.indices().first().copied())
                .map(|i| i as usize);
            let snapshot = snap.borrow().clone();
            if snapshot.len() > 1 && row_under.map_or(false, |r| snapshot.contains(&r)) {
                let pv = pl_view_p.clone();
                let snap_c = snapshot.clone();
                glib::idle_add_local_once(move || {
                    #[allow(deprecated)]
                    let selection = pv.selection();
                    #[allow(deprecated)]
                    selection.unselect_all();
                    #[allow(deprecated)]
                    if let Some(model) = pv.model() {
                        for idx in &snap_c {
                            if let Some(iter) = model.iter_nth_child(None, *idx as i32) {
                                #[allow(deprecated)]
                                selection.select_iter(&iter);
                            }
                        }
                    }
                });
            } else if !active_press.get() {
                // Click landed outside the multi-selection (and no drag is in
                // progress) → forget the snapshot, so a later click on a row
                // that used to be selected doesn't resurrect the whole group.
                snap.borrow_mut().clear();
            }
        });
        pl_view.add_controller(press);
    }
    // Emits a FileList of every currently-selected row's path so the drag
    // is consumable by both the active-playlist internal reorder target
    // and external destinations like sidebar pl rows / GNOME Files.  Uses
    // the press-time snapshot when GTK has already collapsed selection to
    // a single row.
    {
        let drag_src = DragSource::new();
        drag_src.set_actions(gdk::DragAction::COPY);
        let pl_view_ds = pl_view.clone();
        let state_ds   = state.clone();
        let drag_sel_ds = pl_drag_selection.clone();
        // Flip drag_active on drag begin / end so the selection-changed
        // observer doesn't wipe the snapshot during the drag chain.
        {
            let active = pl_drag_active.clone();
            drag_src.connect_drag_begin(move |_, _| {
                active.set(true);
            });
        }
        {
            let active = pl_drag_active.clone();
            drag_src.connect_drag_end(move |_, _, _| {
                active.set(false);
            });
        }
        {
            let active = pl_drag_active.clone();
            drag_src.connect_drag_cancel(move |_, _, _| {
                active.set(false);
                false
            });
        }
        let drag_active_pp = pl_drag_active.clone();
        drag_src.connect_prepare(move |_, x, y| {
            // Flip drag_active up-front — selection-changed events
            // between prepare and drag-begin would otherwise wipe the
            // snapshot before drop reads it.
            drag_active_pp.set(true);
            #[allow(deprecated)]
            let row_under = match pl_view_ds.path_at_pos(x as i32, y as i32) {
                Some((Some(p), _, _, _)) => p.indices().first().copied().map(|i| i as usize),
                _ => None,
            };
            // Prefer the connect_changed snapshot (multi-select); fall back
            // to the live selection; final fallback is the row under cursor.
            let snapshot = drag_sel_ds.borrow().clone();
            let sel_indices: Vec<usize> = if snapshot.len() > 1
                && row_under.map_or(false, |r| snapshot.contains(&r))
            {
                snapshot
            } else {
                #[allow(deprecated)]
                let (selected_paths, _model) = pl_view_ds.selection().selected_rows();
                let live: Vec<usize> = selected_paths
                    .iter()
                    .filter_map(|p| p.indices().first().copied())
                    .map(|i| i as usize)
                    .collect();
                if !live.is_empty() { live }
                else { row_under.into_iter().collect() }
            };
            // Stash final source indices so the drop target can do a
            // precise reorder without round-tripping through paths.
            *drag_sel_ds.borrow_mut() = sel_indices.clone();
            let s = state_ds.borrow();
            let paths: Vec<std::path::PathBuf> = sel_indices.iter()
                .filter_map(|i| s.playlist.tracks.get(*i))
                .map(|t| t.path.clone())
                .collect();
            if paths.is_empty() { return None }
            let files: Vec<gio::File> = paths.iter()
                .map(|p| gio::File::for_path(p))
                .collect();
            let fl = gdk::FileList::from_array(&files);
            Some(gdk::ContentProvider::for_value(&fl.to_value()))
        });
        pl_view.add_controller(drag_src);
    }

    // Active-playlist internal reorder via FileList drop on pl_view.
    // Source indices are recovered by looking up each dropped path in the
    // current playlist; any path not found is treated as a new track and
    // appended (so cross-window drops from ML/editor also work here).
    {
        let drop_tgt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let state_dnd = state.clone();
        let rebuild_dnd = rebuild_playlist.clone();
        let pl_view_dnd = pl_view.clone();
        let drag_sel_drop = pl_drag_selection.clone();

        drop_tgt.connect_drop(move |_, value, x, y| {
            let file_list = match value.get::<gdk::FileList>() {
                Ok(fl) => fl,
                Err(_) => {
                    return false;
                }
            };
            let dropped: Vec<std::path::PathBuf> = file_list.files().iter()
                .filter_map(|f| f.path())
                .collect();
            if dropped.is_empty() { return false }

            let n = state_dnd.borrow().playlist.len();
            // Use dest_row_at_pos so the drop position honors the
            // before/after halves of the target row — dropping on the
            // bottom half of row 13 inserts at 14, top half at 13.
            // Without this, dropping [10,11,12,13] onto row 14 always
            // computed insert_at=10 (no visible move).
            #[allow(deprecated)]
            let dst_pos = match pl_view_dnd.dest_row_at_pos(x as i32, y as i32) {
                Some((Some(p), drop_pos)) => {
                    let row = p.indices().first().copied().unwrap_or(0) as usize;
                    match drop_pos {
                        gtk4::TreeViewDropPosition::Before
                        | gtk4::TreeViewDropPosition::IntoOrBefore => row,
                        gtk4::TreeViewDropPosition::After
                        | gtk4::TreeViewDropPosition::IntoOrAfter => row + 1,
                        _ => row,
                    }
                }
                _ => n,
            };

            // Prefer the press-time drag selection snapshot — it's the
            // authoritative source-row list and avoids the path-comparison
            // mismatch that round-trips through gio::File.  Empty snapshot
            // means the drop came from another window, so fall back to
            // path matching to decide reorder-vs-add.
            let snapshot: Vec<usize> = drag_sel_drop.borrow().clone();
            let mut existing_src_indices: Vec<usize>;
            let mut new_paths: Vec<std::path::PathBuf>;
            if !snapshot.is_empty() {
                existing_src_indices = snapshot;
                new_paths = Vec::new();
            } else {
                existing_src_indices = Vec::new();
                new_paths = Vec::new();
                let s = state_dnd.borrow();
                for dp in &dropped {
                    let idx = s.playlist.tracks.iter().position(|t| &t.path == dp);
                    match idx {
                        Some(i) => existing_src_indices.push(i),
                        None    => new_paths.push(dp.clone()),
                    }
                }
            }

            let did_move = !existing_src_indices.is_empty();
            // Capture the post-move range so the idle rebuild below can
            // re-select the moved rows — without this, the drop appears
            // to clear selection and the user can't see what was moved.
            let mut moved_range: Option<(usize, usize)> = None;
            if did_move {
                let mut s = state_dnd.borrow_mut();
                // Remove highest-first so earlier removes don't invalidate later indices.
                let mut sorted = existing_src_indices.clone();
                sorted.sort_unstable_by(|a, b| b.cmp(a));
                let mut adjusted_dst = dst_pos;
                let mut removed: Vec<crate::model::Track> = Vec::new();
                for src in sorted.iter() {
                    if *src < s.playlist.tracks.len() {
                        let t = s.playlist.tracks.remove(*src);
                        if *src < adjusted_dst { adjusted_dst -= 1; }
                        removed.push(t);
                    }
                }
                // Reinsert in original drop order at adjusted_dst.
                removed.reverse();
                let cap = s.playlist.tracks.len();
                let insert_at = adjusted_dst.min(cap);
                let removed_n = removed.len();
                for (i, t) in removed.into_iter().enumerate() {
                    s.playlist.tracks.insert(insert_at + i, t);
                }
                moved_range = Some((insert_at, removed_n));
            }

            let mut did_add = false;
            for p in new_paths {
                if state_dnd.borrow_mut().add_path(&p).is_ok() { did_add = true; }
            }
            // Clear the press-time selection snapshot so a subsequent
            // single-row drag doesn't accidentally reorder the whole set.
            drag_sel_drop.borrow_mut().clear();
            if did_move || did_add {
                // Defer to next idle tick — splicing the model while GTK
                // is still unwinding the drop event can segfault.
                let rb = rebuild_dnd.clone();
                let pv = pl_view_dnd.clone();
                let snap_restore = drag_sel_drop.clone();
                glib::idle_add_local_once(move || {
                    rb();
                    // Restore selection to the moved range so the user
                    // sees what was just reordered.
                    if let Some((start, n)) = moved_range {
                        #[allow(deprecated)]
                        let selection = pv.selection();
                        #[allow(deprecated)]
                        selection.unselect_all();
                        let mut restored: Vec<usize> = Vec::new();
                        #[allow(deprecated)]
                        if let Some(model) = pv.model() {
                            for i in 0..n {
                                let row_idx = start + i;
                                if let Some(iter) = model.iter_nth_child(None, row_idx as i32) {
                                    #[allow(deprecated)]
                                    selection.select_iter(&iter);
                                    restored.push(row_idx);
                                }
                            }
                        }
                        // Re-seed the multi-select snapshot so a follow-up
                        // drag of the same rows works without redoing the
                        // shift-click sequence.
                        if restored.len() > 1 {
                            *snap_restore.borrow_mut() = restored;
                        }
                    }
                });
            }
            true
        });

        pl_view.add_controller(drop_tgt);
    }

    // ── Drop target: accept files dragged from an external file manager ───────
    // Handles gdk::FileList drops (the standard type produced by GNOME Files
    // and most GTK4-aware file managers).  Files are appended to the playlist;
    // directories are scanned recursively.  Attached to the ScrolledWindow so
    // the full visible playlist area is a valid drop zone.
    {
        let file_drop = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let state_fd = state.clone();
        let rebuild_fd = rebuild_playlist.clone();
        let status_fd = pl_status_label.clone();
        let probe_tx_fd = probe_tx.clone();
        let broken_tx_fd = broken_tx.clone();
        file_drop.connect_drop(move |_, value, _, _| {
            let file_list = match value.get::<gdk::FileList>() {
                Ok(fl) => fl,
                Err(_) => return false,
            };
            let before = state_fd.borrow().playlist.tracks.len();
            let mut added = 0usize;
            for file in file_list.files() {
                if let Some(path) = file.path() {
                    if state_fd.borrow_mut().add_path(&path).is_ok() {
                        added += 1;
                    }
                }
            }
            if added > 0 {
                status_fd.set_text(&format!(
                    "Dropped {} file{}",
                    added,
                    if added == 1 { "" } else { "s" }
                ));
                rebuild_fd();
                let paths = state_fd.borrow().uncached_paths_from(before);
                if !paths.is_empty() {
                    duration_probe::spawn_probes(paths, probe_tx_fd.clone(), broken_tx_fd.clone());
                }
            }
            added > 0
        });
        pl_scroll.add_controller(file_drop);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Transport button callbacks
    // ══════════════════════════════════════════════════════════════════════════

    // ▶ Play / resume.
    btn_play.connect_clicked({
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        move |_| {
            let ps = state.borrow().player.state().clone();
            match ps {
                PlayerState::Stopped => play_and_update(),
                PlayerState::Paused => {
                    let _ = state.borrow_mut().player.toggle_pause();
                }
                PlayerState::Playing => {}
            }
        }
    });

    // ⏸ Pause / resume toggle.
    btn_pause.connect_clicked({
        let state = state.clone();
        move |_| {
            let _ = state.borrow_mut().player.toggle_pause();
        }
    });

    // ⏹ Stop.
    btn_stop.connect_clicked({
        let state = state.clone();
        let seek_bar = seek_bar.clone();
        let patch_pl_row = patch_pl_row.clone();
        move |_| {
            let old_idx = state.borrow().playlist.current_index;
            let _ = state.borrow_mut().player.stop();
            seek_bar.set_value(0.0);
            // Remove the bold/arrow from the now-stopped track.
            patch_pl_row(old_idx);
        }
    });

    // ⏭ Next track.
    btn_next.connect_clicked({
        let state = state.clone();
        let set_track = set_track.clone();
        let patch_pl_row = patch_pl_row.clone();
        let scroll_to_row_if_needed = scroll_to_row_if_needed.clone();
        let current_track_meta_tx = current_track_meta_tx.clone();
        move |_| {
            let old_idx = state.borrow().playlist.current_index;
            if let Some(display) = { state.borrow_mut().play_next() } {
                let new_idx = state.borrow().playlist.current_index;
                set_track(&display);
                scan_current_track_metadata(&state, current_track_meta_tx.clone());
                scroll_to_row_if_needed(new_idx);
                if old_idx != new_idx {
                    patch_pl_row(old_idx);
                }
                patch_pl_row(new_idx);
            }
        }
    });

    // ⏮ Previous / restart (PRD back-button logic).
    btn_prev.connect_clicked({
        let state = state.clone();
        let set_track = set_track.clone();
        let patch_pl_row = patch_pl_row.clone();
        let scroll_to_row_if_needed = scroll_to_row_if_needed.clone();
        let current_track_meta_tx = current_track_meta_tx.clone();
        move |_| {
            let old_idx = state.borrow().playlist.current_index;
            if let Some(display) = { state.borrow_mut().play_prev() } {
                let new_idx = state.borrow().playlist.current_index;
                set_track(&display);
                scan_current_track_metadata(&state, current_track_meta_tx.clone());
                scroll_to_row_if_needed(new_idx);
                if old_idx != new_idx {
                    patch_pl_row(old_idx);
                }
                patch_pl_row(new_idx);
            }
        }
    });

    // 🔁 Repeat — cycle Off → Song → Playlist → Off.
    // Updates the button label and tooltip immediately so the user can see
    // the current mode without opening the help window.
    btn_repeat.connect_clicked({
        let state = state.clone();
        let btn_repeat = btn_repeat.clone();
        let repeat_icon = repeat_icon.clone();
        let repeat_label = repeat_label.clone();
        move |_| {
            let new_mode = {
                let mut s = state.borrow_mut();
                let m = s.config.playback.repeat_mode.cycle();
                s.config.playback.repeat_mode = m;
                m
            };
            // Update both icon and text so the button matches macOS visuals.
            repeat_icon.set_icon_name(Some(repeat_btn_icon(new_mode)));
            repeat_label.set_text(repeat_btn_text(new_mode));
            // Highlight with accent class when not off.
            if new_mode == crate::shuffle::RepeatMode::Off {
                btn_repeat.remove_css_class("mode-btn-active");
            } else {
                btn_repeat.add_css_class("mode-btn-active");
            }
        }
    });

    // 🔀 Shuffle — toggle on/off; accent-highlighted when on.
    btn_shuffle.connect_clicked({
        let state = state.clone();
        let btn_shuffle = btn_shuffle.clone();
        move |_| {
            let enabled = {
                let mut s = state.borrow_mut();
                s.shuffle_state.toggle();
                // Reset the shuffle history so the new setting takes effect cleanly.
                s.shuffle_state.reset();
                let on = s.shuffle_state.enabled;
                // Mirror to config so the setting survives to the next session.
                s.config.playback.shuffle_enabled = on;
                on
            };
            if enabled {
                btn_shuffle.add_css_class("mode-btn-active");
            } else {
                btn_shuffle.remove_css_class("mode-btn-active");
            }
        }
    });

    // PL — toggle the playlist window.
    btn_pl.connect_clicked({
        let playlist_win = playlist_win.clone();
        let state = state.clone();
        move |_| {
            if playlist_win.is_visible() {
                let (w, h) = (playlist_win.width(), playlist_win.height());
                {
                    let mut s = state.borrow_mut();
                    s.config.window.playlist_width = w;
                    s.config.window.playlist_height = h;
                }
                let _ = state.borrow().config.save();
            }
            playlist_win.set_visible(!playlist_win.is_visible());
        }
    });

    // ℹ Info button — connected after handle_key is defined (see below).

    // ══════════════════════════════════════════════════════════════════════════
    // Playlist TreeView interactions
    // ══════════════════════════════════════════════════════════════════════════

    // Double-click / Enter on a row: jump to that track and start playback.
    #[allow(deprecated)]
    pl_view.connect_row_activated({
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        let patch_pl_row = patch_pl_row.clone();
        move |_, path, _| {
            if let Some(&idx) = path.indices().first() {
                // Record the previously-playing track before changing current_index
                // so we can de-highlight it after the jump.
                let old_idx = state.borrow().playlist.current_index;
                state.borrow_mut().playlist.jump_to(idx as usize);
                play_and_update();
                if old_idx != idx as usize {
                    patch_pl_row(old_idx);
                }
            }
        }
    });

    // Right-click context menu on a row: Play / View+Edit ID3 / Remove.
    // NOTE: Attached to ScrolledWindow instead of TreeView to work around GTK4 bug
    // where PopoverMenu doesn't receive hover events when attached directly to TreeView.
    {
        let ctx_click = GestureClick::new();
        ctx_click.set_button(3); // right mouse button
        // Capture phase pre-empts GtkTreeView's default Bubble-phase right-
        // click handler, which otherwise clears the multi-selection before
        // our `path_is_selected` guard sees it.  Claimed state at the end
        // prevents the default handler from running afterward.
        ctx_click.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let pl_view_ctx = pl_view.clone();
        let pl_scroll_ctx = pl_scroll.clone();

        // Create action group and attach to the ScrolledWindow (not TreeView)
        let pl_action_group = gio::SimpleActionGroup::new();
        pl_scroll_ctx.insert_action_group("pl", Some(&pl_action_group));

        // Store the current row index for the action handlers
        let current_row: Rc<RefCell<Option<i64>>> = Rc::new(RefCell::new(None));

        // Register playlist menu actions in the action group
        let action_play = gio::SimpleAction::new("play", None); // Short name
        let state_play = state.clone();
        let play_callback = play_and_update.clone();
        let patch_callback = patch_pl_row.clone();
        let scroll_callback = scroll_to_row_if_needed.clone();
        let row_play = current_row.clone();
        action_play.connect_activate(move |_, _| {
            let row_idx = *row_play.borrow();
            if let Some(idx) = row_idx {
                let idx = idx as usize;
                let old_idx = state_play.borrow().playlist.current_index;
                state_play.borrow_mut().playlist.jump_to(idx);
                play_callback();
                scroll_callback(idx);
                if old_idx != idx {
                    patch_callback(old_idx);
                }
            }
        });
        pl_action_group.add_action(&action_play);

        let action_id3 = gio::SimpleAction::new("edit-id3", None); // Short name
        let state_id3 = state.clone();
        let win_id3 = window.downgrade();
        let rebuild_id3 = rebuild_playlist.clone();
        let row_id3 = current_row.clone();
        action_id3.connect_activate(move |_, _| {
            let row_idx = *row_id3.borrow();
            if let Some(idx) = row_idx {
                let path = state_id3
                    .borrow()
                    .playlist
                    .tracks
                    .get(idx as usize)
                    .map(|t| t.path.clone());
                if let Some(path) = path {
                    open_id3_editor_window(
                        win_id3.upgrade().as_ref(),
                        path,
                        state_id3.clone(),
                        rebuild_id3.clone(),
                        None,
                    );
                }
            }
        });
        pl_action_group.add_action(&action_id3);

        let action_remove = gio::SimpleAction::new("remove", None); // Short name
        let remove_callback = remove_selected.clone();
        action_remove.connect_activate(move |_, _| {
            remove_callback();
        });
        pl_action_group.add_action(&action_remove);

        // Seed a brand new saved playlist from the current selection.
        // Opens the standard playlist save dialog so the user picks the
        // filename + folder; the resulting M3U8 contains EXTINF metadata
        // for every selected active-playlist row.
        let action_add_to_new = gio::SimpleAction::new("add-to-new", None);
        {
            let state_atn  = state.clone();
            let pl_view_atn = pl_view.clone();
            let win_atn    = playlist_win.clone();
            action_add_to_new.connect_activate(move |_, _| {
                #[allow(deprecated)]
                let (sel_paths, _) = pl_view_atn.selection().selected_rows();
                let indices: Vec<usize> = sel_paths.iter()
                    .filter_map(|p| p.indices().first().copied())
                    .map(|i| i as usize)
                    .collect();
                let paths: Vec<String> = {
                    let s = state_atn.borrow();
                    indices.iter()
                        .filter_map(|i| s.playlist.tracks.get(*i))
                        .map(|t| t.path.to_string_lossy().into_owned())
                        .collect()
                };
                if paths.is_empty() { return }
                let default_stem = glib::DateTime::now_local()
                    .ok()
                    .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Playlist".to_string());
                let state_cb = state_atn.clone();
                run_playlist_save_dialog(
                    state_atn.clone(),
                    win_atn.clone(),
                    &default_stem,
                    move |path, win_cb| {
                        if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                            if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths) {
                                eprintln!("save_playlist_tracks_to_path: {e}");
                                show_playlist_save_error(&win_cb, &path, &e);
                            }
                        }
                        notify_playlist_nav_refresh();
                    },
                );
            });
        }
        pl_action_group.add_action(&action_add_to_new);

        // Add selection to a saved playlist (parameterised by id).
        // Multi-select aware: pulls every selected row from the active
        // playlist and appends their paths to the chosen saved playlist.
        let state_add_pl = state.clone();
        let pl_view_add  = pl_view.clone();
        let action_add_to_saved = gio::SimpleAction::new(
            "add-to-saved",
            Some(glib::VariantTy::INT64),
        );
        action_add_to_saved.connect_activate(move |_, param| {
            let Some(pid) = param.and_then(|p| p.get::<i64>()) else { return };
            #[allow(deprecated)]
            let (paths_models, _model) = pl_view_add.selection().selected_rows();
            let indices: Vec<i64> = paths_models
                .iter()
                .filter_map(|p| p.indices().first().copied())
                .map(|i| i as i64)
                .collect();
            let paths: Vec<String> = {
                let s = state_add_pl.borrow();
                indices.iter()
                    .filter_map(|i| s.playlist.tracks.get(*i as usize))
                    .map(|t| t.path.to_string_lossy().into_owned())
                    .collect()
            };
            if paths.is_empty() { return }
            let mut ok = false;
            if let Some(lib) = state_add_pl.borrow().media_lib.as_ref() {
                match lib.append_paths_to_playlist(pid, &paths) {
                    Ok(_)  => ok = true,
                    Err(e) => eprintln!("append_paths_to_playlist {pid}: {e}"),
                }
            }
            if ok { notify_playlist_changed(pid); }
        });
        pl_action_group.add_action(&action_add_to_saved);

        let state_menu_pl = state.clone();
        let pl_drag_sel_ctx = pl_drag_selection.clone();
        ctx_click.connect_pressed(move |gest, _, x, y| {
            #[allow(deprecated)]
            let row_idx = match pl_view_ctx.path_at_pos(x as i32, y as i32) {
                Some((Some(path), _, _, _)) => match path.indices().first().copied() {
                    Some(i) => i as i64,
                    None => return,
                },
                _ => return,
            };

            // Store the row index for the action handlers
            *current_row.borrow_mut() = Some(row_idx);

            // Restore the press-time multi-selection snapshot when the
            // clicked row was part of a prior multi-select.  GtkTreeView
            // (deprecated) collapses selection to the clicked row on
            // secondary-button press even when our Capture-phase
            // handler claims the event, so we explicitly re-select the
            // snapshot rows here.
            let snapshot = pl_drag_sel_ctx.borrow().clone();
            let row_idx_u = row_idx as usize;
            let should_restore = snapshot.len() > 1 && snapshot.contains(&row_idx_u);

            #[allow(deprecated)]
            let path = gtk4::TreePath::from_indices(&[row_idx as i32]);
            #[allow(deprecated)]
            let already_selected = pl_view_ctx.selection().path_is_selected(&path);
            if should_restore {
                #[allow(deprecated)]
                pl_view_ctx.selection().unselect_all();
                let model = pl_view_ctx.model();
                #[allow(deprecated)]
                if let Some(model) = model {
                    for idx in &snapshot {
                        if let Some(iter) = model.iter_nth_child(None, *idx as i32) {
                            #[allow(deprecated)]
                            pl_view_ctx.selection().select_iter(&iter);
                        }
                    }
                }
            } else if !already_selected {
                #[allow(deprecated)]
                pl_view_ctx.selection().unselect_all();
                #[allow(deprecated)]
                if let Some(iter) = pl_view_ctx
                    .model()
                    .and_then(|m| m.iter_nth_child(None, row_idx as i32))
                {
                    #[allow(deprecated)]
                    pl_view_ctx.selection().select_iter(&iter);
                }
            }

            // Number of currently-selected rows — drives single-only menu
            // items (Edit ID3 is hidden in multi-select since the editor
            // can only bind to one track at a time).
            #[allow(deprecated)]
            let sel_count = pl_view_ctx.selection().count_selected_rows() as usize;

            // Build menu model with prefixed action names
            let menu = gio::Menu::new();
            menu.append_item(&gio::MenuItem::new(Some("▶ Play"), Some("pl.play")));
            if sel_count <= 1 {
                menu.append_item(&gio::MenuItem::new(
                    Some("🎵 View / Edit ID3"),
                    Some("pl.edit-id3"),
                ));
            }
            menu.append_item(&gio::MenuItem::new(Some("✕ Remove"), Some("pl.remove")));
            let submenu = build_add_to_playlist_submenu(
                &state_menu_pl,
                "pl.add-to-new",
                "pl.add-to-saved",
            );
            menu.append_submenu(Some("Add to Playlist"), &submenu);

            // Create popover menu — NESTED keeps the Add-to-Playlist
            // submenu from being clipped to the parent menu's height.
            let popover = gtk4::PopoverMenu::from_model_full(
                &menu,
                gtk4::PopoverMenuFlags::NESTED,
            );
            popover.set_parent(&pl_scroll_ctx);
            let rect = gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            popover.set_pointing_to(Some(&rect));
            popover.popup();
            gest.set_state(gtk4::EventSequenceState::Claimed);
        });
        #[allow(deprecated)]
        pl_view.add_controller(ctx_click);
    }

    // Selection change: show a status hint when a broken track is selected.
    #[allow(deprecated)]
    pl_view.selection().connect_changed({
        let state     = state.clone();
        let pl_status = pl_status_label.clone();
        let pl_view_sc = pl_view.clone();
        move |_| {
            // set_model(None) during a bulk rebuild fires this signal with a null
            // model; selected_rows() would then panic.  Bail early if no model.
            #[allow(deprecated)]
            if pl_view_sc.model().is_none() { return; }
            #[allow(deprecated)]
            let (paths, _) = pl_view_sc.selection().selected_rows();
            let Some(path) = paths.first() else {
                pl_status.set_text("");
                return;
            };
            let Some(&idx) = path.indices().first() else {
                pl_status.set_text("");
                return;
            };
            let idx = idx as usize;
            let is_broken = state.borrow().playlist.tracks
                .get(idx)
                .map(|t| t.broken)
                .unwrap_or(false);
            if is_broken {
                let path_hint = state.borrow().playlist.tracks
                    .get(idx)
                    .map(|t| t.path.display().to_string())
                    .unwrap_or_default();
                pl_status.set_text(&format!(
                    "⚠  This file can't be played — it may have been moved, renamed, or deleted.  ({})",
                    path_hint
                ));
            } else {
                pl_status.set_text("");
            }
        }
    });

    // ── Playlist window "Remove" button ───────────────────────────────────────
    btn_remove.connect_clicked({
        let remove_selected = remove_selected.clone();
        move |_| remove_selected()
    });

    // ── Playlist window "✕ All" button — clear entire playlist ───────────────
    btn_clear_all.connect_clicked({
        let state = state.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let set_track = set_track.clone();
        move |_| {
            {
                let mut s = state.borrow_mut();
                let _ = s.player.stop();
                s.playlist.tracks.clear();
                s.playlist.current_index = 0;
                s.last_duration = None;
                s.pending_seek = None;
                s.mute_pending = None;
            }
            set_track("No track loaded");
            rebuild_playlist();
        }
    });

    // ── Left-click on the marquee title → open ID3 editor for current track ──
    // Adding the click controller to title_label so only the text area is
    // clickable, not the whole now-playing frame.
    {
        let state_mc = state.clone();
        let win_mc = window.downgrade();
        let rebuild_mc = rebuild_playlist.clone();
        let click = GestureClick::new();
        click.set_button(1); // primary button
        click.connect_released(move |_, _, _, _| {
            let path = state_mc.borrow().playlist.current().map(|t| t.path.clone());
            if let Some(path) = path {
                if let Some(w) = win_mc.upgrade() {
                    open_id3_editor_window(
                        Some(&w),
                        path,
                        state_mc.clone(),
                        rebuild_mc.clone(),
                        None,
                    );
                }
            }
        });
        title_label.add_controller(click);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Playlist window: Add-file buttons
    // ══════════════════════════════════════════════════════════════════════════

    // Helper: build a FileFilter matching all common audio formats.
    // Used by all three add dialogs to avoid re-creating the filter object.
    let make_audio_filter = || {
        let f = gtk4::FileFilter::new();
        f.set_name(Some("Audio files"));
        // MIME types cover most desktop environments and file managers.
        for mime in &[
            "audio/mpeg",
            "audio/flac",
            "audio/ogg",
            "audio/opus",
            "audio/wav",
            "audio/x-wav",
            "audio/aac",
            "audio/mp4",
            "audio/x-m4a",
            "audio/x-ms-wma",
        ] {
            f.add_mime_type(mime);
        }
        // Extension patterns as fallback for systems without full MIME support.
        for pat in &[
            "*.mp3", "*.flac", "*.ogg", "*.opus", "*.wav", "*.aac", "*.m4a", "*.wma", "*.ape",
            "*.aiff",
        ] {
            f.add_pattern(pat);
        }
        f
    };

    // Cancel button: stops any active playlist scan (Add Folder or Add Files).
    // Wired once here, before the add handlers, so it is always connected.
    btn_cancel.connect_clicked({
        let state = state.clone();
        let pl_status = pl_status_label.clone();
        let cancel_btn = btn_cancel.clone();
        move |_| {
            let s = state.borrow();
            if let Some(ref scan) = s.playlist_scan {
                scan.cancel
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            drop(s);
            pl_status.set_text("Cancelling…");
            cancel_btn.set_visible(false);
        }
    });

    // [+ Files]: open the desktop file browser to pick one or more audio files.
    // For small selections this is near-instant; for large selections it uses the
    // same two-phase background scan as Add Folder to avoid blocking the UI.
    btn_add_files.connect_clicked({
        let state = state.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let pl_status = pl_status_label.clone();
        let window_wk = playlist_win.downgrade();
        let make_filt = make_audio_filter.clone();
        let probe_tx = probe_tx.clone();
        let broken_tx = broken_tx.clone();
        let cancel_btn = btn_cancel.clone();
        let patch_pl_row_af = patch_pl_row.clone();
        let set_track_af = set_track.clone();
        move |_| {
            let dialog = gtk4::FileDialog::builder().title("Add Audio Files").build();
            let filter_store = gio::ListStore::new::<gtk4::FileFilter>();
            filter_store.append(&make_filt());
            dialog.set_filters(Some(&filter_store));

            let state_cb = state.clone();
            let rebuild_cb = rebuild_playlist.clone();
            let status_cb = pl_status.clone();
            let probe_tx_cb = probe_tx.clone();
            let broken_tx_cb = broken_tx.clone();
            let cancel_ref = cancel_btn.clone();
            let patch_cb = patch_pl_row_af.clone();
            let set_track_cb = set_track_af.clone();
            let parent = window_wk.upgrade();
            dialog.open_multiple(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                let Ok(list) = result else { return };

                // Collect selected paths on the main thread before spawning.
                let files: Vec<PathBuf> = (0..list.n_items())
                    .filter_map(|i| list.item(i))
                    .filter_map(|obj| obj.downcast::<gio::File>().ok())
                    .filter_map(|f| f.path())
                    .collect();

                if files.is_empty() {
                    return;
                }

                let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                {
                    let mut s = state_cb.borrow_mut();
                    s.playlist_scan = Some(ScanState {
                        scan_type: ScanType::AddFiles,
                        current: 0,
                        total: 0,
                        cancel: cancel.clone(),
                    });
                    s.pending_bg_ops.set(s.pending_bg_ops.get() + 1);
                }

                status_cb.set_text("Scanning…");
                cancel_ref.set_visible(true);

                // Capture where the new tracks will start before any are added.
                let scan_start = state_cb.borrow().playlist.len();

                let (fast_tx, fast_rx) = std::sync::mpsc::channel::<crate::model::Track>();
                let (meta_tx, meta_rx) =
                    std::sync::mpsc::channel::<(usize, String, String, String, String)>();
                let (done_tx, done_rx) = std::sync::mpsc::channel::<usize>();
                let (phase1_done_tx, phase1_done_rx) = std::sync::mpsc::channel::<usize>();

                crate::model::Playlist::scan_files_for_ui(
                    files,
                    cancel,
                    fast_tx,
                    meta_tx,
                    done_tx,
                    phase1_done_tx,
                );

                start_playlist_scan_poller(
                    state_cb.clone(),
                    status_cb.clone(),
                    rebuild_cb.clone(),
                    cancel_ref.clone(),
                    probe_tx_cb.clone(),
                    broken_tx_cb.clone(),
                    patch_cb.clone(),
                    set_track_cb.clone(),
                    fast_rx,
                    meta_rx,
                    done_rx,
                    phase1_done_rx,
                    scan_start,
                );
            });
        }
    });

    // [⤓ Save] active playlist: open the native Save dialog, write the
    // current queue's track paths to the chosen .m3u8 file via the core
    // helper (which emits #EXTINF lines and registers the playlist in
    // the library), then refresh the sidebar so the new entry appears.
    btn_save_active.connect_clicked({
        let state = state.clone();
        let window_wk = playlist_win.downgrade();
        move |_| {
            let Some(win) = window_wk.upgrade() else { return };
            let paths: Vec<String> = state.borrow().playlist.tracks
                .iter().map(|t| t.path.to_string_lossy().into_owned()).collect();
            if paths.is_empty() { return }
            // Timestamped default name (readable, sortable, no colons).
            // Uses glib's local time so we don't add a chrono dependency.
            let default_stem = glib::DateTime::now_local()
                .ok()
                .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Playlist".to_string());
            let state_cb = state.clone();
            run_playlist_save_dialog(state.clone(), win, &default_stem, move |path, win_cb| {
                if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                    if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths) {
                        eprintln!("save_playlist_tracks_to_path: {e}");
                        show_playlist_save_error(&win_cb, &path, &e);
                    }
                }
                notify_playlist_nav_refresh();
            });
        }
    });

    // [+ Folder]: open the desktop folder browser; recursively add all audio files.
    // Uses the same two-phase scan as Add Files: fast tracks appear immediately,
    // metadata fills in as it is read in the background.
    btn_add_dir.connect_clicked({
        let state = state.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let pl_status = pl_status_label.clone();
        let window_wk = playlist_win.downgrade();
        let probe_tx = probe_tx.clone();
        let broken_tx = broken_tx.clone();
        let cancel_btn = btn_cancel.clone();
        let patch_pl_row_adir = patch_pl_row.clone();
        let set_track_adir = set_track.clone();
        move |_| {
            let dialog = gtk4::FileDialog::new();
            dialog.set_title("Add Folder to Playlist");

            let state_cb = state.clone();
            let rebuild_cb = rebuild_playlist.clone();
            let status_cb = pl_status.clone();
            let probe_tx_cb = probe_tx.clone();
            let broken_tx_cb = broken_tx.clone();
            let cancel_ref = cancel_btn.clone();
            let patch_cb = patch_pl_row_adir.clone();
            let set_track_cb = set_track_adir.clone();
            let parent = window_wk.upgrade();
            dialog.select_folder(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                let Ok(file) = result else { return };
                let Some(folder) = file.path() else { return };

                let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                {
                    let mut s = state_cb.borrow_mut();
                    s.playlist_scan = Some(ScanState {
                        scan_type: ScanType::AddFolder,
                        current: 0,
                        total: 0,
                        cancel: cancel.clone(),
                    });
                    s.pending_bg_ops.set(s.pending_bg_ops.get() + 1);
                }

                status_cb.set_text("Scanning…");
                cancel_ref.set_visible(true);

                // Capture where the new tracks will start before any are added.
                let scan_start = state_cb.borrow().playlist.len();

                let (fast_tx, fast_rx) = std::sync::mpsc::channel::<crate::model::Track>();
                let (meta_tx, meta_rx) =
                    std::sync::mpsc::channel::<(usize, String, String, String, String)>();
                let (done_tx, done_rx) = std::sync::mpsc::channel::<usize>();
                let (phase1_done_tx, phase1_done_rx) = std::sync::mpsc::channel::<usize>();

                crate::model::Playlist::scan_folder_for_ui(
                    folder,
                    cancel,
                    fast_tx,
                    meta_tx,
                    done_tx,
                    phase1_done_tx,
                );

                start_playlist_scan_poller(
                    state_cb.clone(),
                    status_cb.clone(),
                    rebuild_cb.clone(),
                    cancel_ref.clone(),
                    probe_tx_cb.clone(),
                    broken_tx_cb.clone(),
                    patch_cb.clone(),
                    set_track_cb.clone(),
                    fast_rx,
                    meta_rx,
                    done_rx,
                    phase1_done_rx,
                    scan_start,
                );
            });
        }
    });

    // ══════════════════════════════════════════════════════════════════════════
    // Volume slider
    // ══════════════════════════════════════════════════════════════════════════

    // connect_change_value fires only on user-driven changes, avoiding a loop.
    vol_bar.connect_change_value({
        let state = state.clone();
        move |_, _, value| {
            let mut s = state.borrow_mut();
            s.config.playback.volume = value;
            s.player.set_volume(value);
            glib::Propagation::Proceed
        }
    });

    // ══════════════════════════════════════════════════════════════════════════
    // Seek bar interaction
    // ══════════════════════════════════════════════════════════════════════════

    // connect_change_value fires for both a single trough click and thumb drag.
    // It does NOT fire when set_value() is called programmatically (GTK only
    // emits change-value for user-initiated changes), so there is no feedback
    // loop between the tick-loop's set_value calls and this handler.
    //
    // Note: GestureClick added directly to GtkScale does not reliably fire
    // its released signal because the Scale's internal GestureDrag claims the
    // pointer sequence after the press.  We therefore skip the is_seeking flag
    // and let the tick loop freely update the bar and label — set_value()
    // cannot re-trigger this handler so there is no oscillation risk.
    seek_bar.connect_change_value({
        let state = state.clone();
        let time_lbl = time_disp_label.clone();
        let show_rem = show_remaining.clone();
        move |_, _, value| {
            // Update the time display immediately so the user sees the correct
            // offset while scrubbing (stopped or paused), without waiting for
            // the next 100 ms tick.
            if let Some(text) = state
                .borrow()
                .time_display_for_fraction(value, show_rem.get())
            {
                time_lbl.set_text(&text);
            }
            state.borrow_mut().seek_fraction_or_pend(value);
            glib::Propagation::Proceed // allow the Scale to update its visual position
        }
    });

    // ══════════════════════════════════════════════════════════════════════════
    // Tick loop — fires every 100 ms
    // ══════════════════════════════════════════════════════════════════════════
    // Shutdown flag set by window.connect_close_request below; the
    // visualizer timer breaks on it before gsk paints a freed surface.
    let viz_shutting_down: std::rc::Rc<std::cell::Cell<bool>> =
        std::rc::Rc::new(std::cell::Cell::new(false));
    // Single-driver rule (mirrors GraniteView.swift on macOS): while the
    // fullscreen visualizer is open it owns the shared Granite renderer.
    // The mini tick must yield — its aspect-derived width differs from the
    // fullscreen one, and alternating sizes makes Granite::resize() wipe
    // the feedback buffer every frame (leaving just the raw waveform ink).
    let fs_viz_open: std::rc::Rc<std::cell::Cell<bool>> =
        std::rc::Rc::new(std::cell::Cell::new(false));
    {
        let state = state.clone();
        let time_disp_label = time_disp_label.clone();
        let viz_shutting_down = viz_shutting_down.clone();
        let title_label = title_label.clone();
        let seek_bar = seek_bar.clone();
        let play_update = play_and_update.clone();
        let viz = viz.clone();
        let marquee_chars = marquee_chars.clone();
        let marquee_offset = marquee_offset.clone();
        let marquee_tick = marquee_tick.clone();
        let show_remaining = show_remaining.clone();
        let state_label = state_label.clone();
        let btn_play = btn_play.clone();
        let patch_pl_row = patch_pl_row.clone();
        let current_track_meta_rx = std::cell::RefCell::new(current_track_meta_rx);
        let set_track = set_track.clone();
        let rebuild_playlist_tick = rebuild_playlist.clone();
        let play_update_tick = play_and_update.clone();
        let scroll_tick = scroll_to_row_if_needed.clone();
        // Granite-mode renderer state captured by the tick closure. Weak
        // refs so the timer doesn't keep widgets alive after the main window
        // closes — calling `set_paintable` on a destroyed widget triggers a
        // Gdk-CRITICAL and (on Wayland) a segfault during gsk paint.
        let viz_stack_tick = viz_stack.downgrade();
        let granite_pic_tick = granite_pic.downgrade();
        let granite_buf_tick: std::rc::Rc<std::cell::RefCell<Vec<u8>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        // Tick-side handle on the shutdown flag declared above.
        let viz_shut_for_tick = viz_shutting_down.clone();
        let fs_viz_open_tick = fs_viz_open.clone();
        // Counter for periodic cache saves: fires every 300 ticks = 30 seconds.
        let mut cache_save_countdown = 300u32;

        // 33 ms (~30 fps) so the visualizer (Bars / Waveform / Granite) animates
        // smoothly. Bars/Waveform queue_draw is cheap; Granite renders into a
        // ~640×360 buffer that gets GPU-upscaled by gsk.
        glib::timeout_add_local(Duration::from_millis(33), move || {
            // Shutdown short-circuit. Set in connect_close_request below.
            if viz_shut_for_tick.get() {
                return ControlFlow::Break;
            }
            // 0. Drain probe results from background threads.
            // patch_pl_row is O(1) per call (updates a single TreeView store row).
            // Cap to 50 per tick so we never block the main thread for long when
            // a large library delivers thousands of results at once.
            let is_scanning = state.borrow().playlist_scan.is_some();
            let probe_cap = if is_scanning { 50usize } else { 500usize };
            let mut probes_this_tick = 0usize;
            while probes_this_tick < probe_cap {
                let Ok((path, dur)) = probe_rx.try_recv() else {
                    break;
                };
                // Bind the return value to a `let` so the temporary RefMut is
                // dropped at the semicolon — before patch_pl_row tries to borrow.
                let probed_idx = state.borrow_mut().apply_probed_duration(&path, dur);
                if let Some(idx) = probed_idx {
                    patch_pl_row(idx);
                }
                probes_this_tick += 1;
            }
            // 0b. Drain missing-file notifications; mark those tracks broken.
            while let Ok(path) = broken_rx.try_recv() {
                let found_idx = {
                    let mut s = state.borrow_mut();
                    let mut found = None;
                    for (idx, track) in s.playlist.tracks.iter_mut().enumerate() {
                        if track.path == path {
                            track.broken = true;
                            found = Some(idx);
                            break;
                        }
                    }
                    found
                };
                if let Some(idx) = found_idx {
                    patch_pl_row(idx);
                }
            }

            // 0c. Drain current track metadata scan results.
            // This is separate from the playlist scan (meta_rx) — it handles metadata
            // reads triggered by play_and_update when a track starts without metadata.
            while let Ok((path, title, artist, album_artist, album)) =
                current_track_meta_rx.borrow().try_recv()
            {
                let (updated_idx, is_current) = {
                    let mut s = state.borrow_mut();
                    let mut updated_idx = None;
                    let mut is_current = false;
                    for (idx, track) in s.playlist.tracks.iter_mut().enumerate() {
                        if track.path == path {
                            track.title = title;
                            track.artist = artist;
                            track.album_artist = album_artist;
                            track.album = album;
                            updated_idx = Some(idx);
                            is_current = idx == s.playlist.current_index;
                            break;
                        }
                    }
                    (updated_idx, is_current)
                };
                // Update the marquee with the new "Artist - Title" display name.
                if is_current {
                    let display = state
                        .borrow()
                        .playlist
                        .current()
                        .map(|t| t.display_name())
                        .unwrap_or_default();
                    if !display.is_empty() {
                        set_track(&display);
                    }
                }
                // Patch the row to show the new title/artist.
                if let Some(idx) = updated_idx {
                    patch_pl_row(idx);
                }
            }

            // 0d. Handle files received from "Open with Sparkamp" in the file manager.
            // Each batch respects playlist_add_behavior (append/replace) and
            // autoplay_on_add from config.
            while let Ok(paths) = open_rx.try_recv() {
                if paths.is_empty() {
                    continue;
                }
                use crate::config::PlaylistAddBehavior;
                let behavior = state.borrow().config.behavior.playlist_add_behavior.clone();
                let autoplay = state.borrow().config.behavior.autoplay_on_add;

                if behavior == PlaylistAddBehavior::Replace {
                    let _ = state.borrow_mut().player.stop();
                    {
                        let mut s = state.borrow_mut();
                        s.playlist.tracks.clear();
                        s.playlist.current_index = 0;
                        s.last_duration = None;
                        s.pending_seek = None;
                        s.mute_pending = None;
                    }
                }

                let insert_start = state.borrow().playlist.len();
                for path in &paths {
                    if let Ok(track) = crate::model::Track::from_path_fast(path) {
                        state.borrow_mut().playlist.tracks.push(track);
                    }
                }
                let inserted = state.borrow().playlist.len() - insert_start;
                if inserted == 0 {
                    continue;
                }
                rebuild_playlist_tick();

                if autoplay
                    && (behavior == PlaylistAddBehavior::Replace || insert_start == 0)
                {
                    state.borrow_mut().playlist.jump_to(insert_start);
                    play_update_tick();
                    scroll_tick(insert_start);
                }
            }

            // 1. Check for end-of-stream or GStreamer error.
            let bus_event = state.borrow_mut().poll_bus();

            // 1b. Apply any pending seek once the pipeline is running.
            //     Covers two cases:
            //       1. Live scrubbing while Playing/Paused.
            //       2. Pressing Play while Stopped with a pending seek: play_current()
            //          mutes audio and starts playing; the seek is applied here on the
            //          first tick that duration becomes available, then volume is restored.
            {
                let should_seek = {
                    let s = state.borrow();
                    s.pending_seek.is_some()
                        && *s.player.state() != PlayerState::Stopped
                        && (s.player.duration().is_some() || s.last_duration.is_some())
                };
                if should_seek {
                    let restore_vol = {
                        let mut s = state.borrow_mut();
                        let rv = s.mute_pending.take();
                        if let Some(fraction) = s.pending_seek.take() {
                            s.seek_fraction(fraction);
                        }
                        rv
                    };
                    if let Some(vol) = restore_vol {
                        state.borrow_mut().player.set_volume(vol);
                    }
                }
            }
            if let Some(event) = bus_event {
                // Record which track just finished so we can de-highlight it
                // after the advance changes current_index.
                let pre_advance_idx = state.borrow().playlist.current_index;

                // On error, mark the current track broken so it shows a
                // warning indicator and is skipped in future auto-advances.
                if matches!(event, BusEvent::Error) {
                    let mut s = state.borrow_mut();
                    let idx = s.playlist.current_index;
                    if let Some(t) = s.playlist.tracks.get_mut(idx) {
                        t.broken = true;
                    }
                }
                // Advance to the next track via shuffle/repeat logic.
                // Skips over tracks already marked broken.
                let advanced = {
                    let mut s = state.borrow_mut();
                    let total = s.playlist.len();
                    let repeat = s.config.playback.repeat_mode;
                    let current = s.playlist.current_index;

                    // Ask the shuffle engine for the next index.
                    let mut found = false;
                    if let Some(mut next_idx) = s.shuffle_state.next_index(current, total, repeat) {
                        // Skip broken tracks (up to `total` attempts to avoid infinite loop).
                        for _ in 0..total {
                            if s.playlist
                                .tracks
                                .get(next_idx)
                                .map(|t| t.broken)
                                .unwrap_or(false)
                            {
                                s.shuffle_state.record_played(next_idx);
                                match s.shuffle_state.next_index(next_idx, total, repeat) {
                                    Some(i) => {
                                        next_idx = i;
                                    }
                                    None => break,
                                }
                            } else {
                                s.playlist.jump_to(next_idx);
                                found = true;
                                break;
                            }
                        }
                    }
                    found
                };
                if advanced {
                    // play_update (play_and_update) patches the new current track.
                    // We also patch pre_advance_idx because jump_to() already
                    // updated current_index before play_and_update runs, so
                    // play_and_update won't know the finished track is different.
                    play_update();
                    let new_idx = state.borrow().playlist.current_index;
                    if pre_advance_idx != new_idx {
                        patch_pl_row(pre_advance_idx);
                    }
                }
            }

            // 2. Update time display and seek bar position.
            let (pos, dur_opt) = {
                let s = state.borrow();
                (s.player.position(), s.player.duration())
            };
            // Cache duration while it is available so seek-bar drags while
            // stopped can still show the correct time (GStreamer reports None
            // from a Null-state pipeline).
            let gst_dur_written = if let Some(dur) = dur_opt {
                let mut s = state.borrow_mut();
                s.last_duration = Some(dur);
                // Write GStreamer-queried duration back to the current track so
                // the playlist can show it even after playback stops.
                let idx = s.playlist.current_index;
                if let Some(track) = s.playlist.tracks.get_mut(idx) {
                    if track.duration.is_none() {
                        let path = track.path.clone();
                        track.duration = Some(dur);
                        s.duration_cache.insert(&path, dur);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };
            if gst_dur_written {
                // Only the current track's duration changed; patch just that row.
                let idx = state.borrow().playlist.current_index;
                patch_pl_row(idx);
            }

            // Record play in media library after 20 seconds of playback.
            // The rebuild_ml_callback borrows state immutably, so it must be
            // called AFTER the mutable borrow is released — extract the Rc
            // first, then drop the borrow, then invoke the callback.
            let ml_rebuild_needed: Option<Rc<dyn Fn()>> = {
                let mut s = state.borrow_mut();
                let pos = pos.unwrap_or(Duration::ZERO);
                let path_str = s
                    .playlist
                    .current()
                    .map(|t| t.path.to_string_lossy().into_owned());
                if pos >= Duration::from_secs(20) {
                    if let Some(ref p) = path_str {
                        if s.counted_play_path.as_ref() != Some(p) {
                            if let Some(ref ml) = s.media_lib {
                                let _ = ml.record_play(p);
                                s.counted_play_path = Some(p.clone());
                                s.rebuild_ml_callback.clone()
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(rebuild_ml) = ml_rebuild_needed {
                rebuild_ml();
                // Editor mirrors the same DB rows; reload its currently
                // open playlist so the just-recorded play count / last-
                // played timestamp / unread glyph reflect immediately.
                notify_editor_refresh();
            }

            {
                let (player_state, pending) = {
                    let s = state.borrow();
                    (s.player.state().clone(), s.pending_seek)
                };
                let show_rem = show_remaining.get();

                if player_state == PlayerState::Stopped {
                    // When stopped with a pending seek, hold the bar at the
                    // pending position and show its time.  set_value() does
                    // not re-trigger connect_change_value (GTK only emits
                    // change-value for user-initiated changes), so there is
                    // no feedback loop here.
                    if let Some(fraction) = pending {
                        seek_bar.set_value(fraction);
                        // Update the label if duration is known; otherwise
                        // leave whatever connect_change_value last set.
                        if let Some(text) =
                            state.borrow().time_display_for_fraction(fraction, show_rem)
                        {
                            time_disp_label.set_text(&text);
                        }
                    } else {
                        // Truly stopped with no pending seek — reset to zero.
                        seek_bar.set_value(0.0);
                        time_disp_label.set_text(if show_rem { "--:--" } else { "0:00" });
                    }
                } else {
                    // Playing or Paused — show live GStreamer position.
                    let pos = pos.unwrap_or(Duration::ZERO);
                    if show_rem {
                        if let Some(dur) = dur_opt {
                            let rem = dur.saturating_sub(pos);
                            let rs = rem.as_secs();
                            time_disp_label.set_text(&format!("-{}:{:02}", rs / 60, rs % 60));
                        } else {
                            time_disp_label.set_text("--:--");
                        }
                    } else {
                        let ps = pos.as_secs();
                        time_disp_label.set_text(&format!("{}:{:02}", ps / 60, ps % 60));
                    }
                    if let Some(dur) = dur_opt {
                        if dur.as_nanos() > 0 {
                            seek_bar.set_value(pos.as_nanos() as f64 / dur.as_nanos() as f64);
                        }
                    }
                }
            }

            // 3. Marquee / scrolling title.
            // Display a sliding window into the full "Title — Artist" text.
            // The window width is estimated from the label's allocated pixel
            // width divided by 8 (conservative px-per-char for the 13 px font).
            {
                let chars = marquee_chars.borrow();
                // Fallback to 30 chars before the label is laid out (width = 0).
                let label_w = title_label.allocated_width();
                let display_cols = if label_w > 0 {
                    (label_w / 8).max(10) as usize
                } else {
                    30
                };

                if chars.len() <= display_cols {
                    // Short enough to fit without scrolling.
                    title_label.set_text(&chars.iter().collect::<String>());
                    marquee_offset.set(0);
                } else {
                    // Advance offset every 3 ticks (≈ 300 ms, ~3 chars/second).
                    let tick = marquee_tick.get() + 1;
                    marquee_tick.set(tick);
                    if tick % 3 == 0 {
                        // 5-space visual gap between repetitions.
                        let cycle = chars.len() + 5;
                        marquee_offset.set((marquee_offset.get() + 1) % cycle);
                    }

                    let offset = marquee_offset.get();
                    // Pad with spaces so wrap-around reads cleanly.
                    let gap: Vec<char> = "     ".chars().collect();
                    let looped: Vec<char> = chars.iter().chain(gap.iter()).cloned().collect();
                    let loop_len = looped.len();
                    let visible: String = (0..display_cols)
                        .map(|i| *looped.get((offset + i) % loop_len).unwrap_or(&' '))
                        .collect();
                    title_label.set_text(&visible);
                }
            }

            // 4. State icon (left of time display) + dynamic play-button accent.
            //    The play button gains the `.transport-play` skin accent while
            //    the engine is Playing or Paused, and loses it when Stopped.
            {
                let s = state.borrow();
                let icon = match s.player.state() {
                    PlayerState::Playing => "▶",
                    PlayerState::Paused => "⏸",
                    PlayerState::Stopped => "⏹",
                };
                state_label.set_text(icon);
                match s.player.state() {
                    PlayerState::Playing | PlayerState::Paused => {
                        if !btn_play.has_css_class("transport-play") {
                            btn_play.add_css_class("transport-play");
                        }
                    }
                    PlayerState::Stopped => {
                        btn_play.remove_css_class("transport-play");
                    }
                }
            }

            // 5. Trigger a Cairo repaint of the visualizer (Bars / Waveform).
            // Granite renders into a Picture instead — see step 5b below.
            viz.queue_draw();

            // 5b. Granite plasma path. Cheap when not the active mode (the
            // match is the only cost). When active, render into the persistent
            // RGBA buffer and hand it to the GTK renderer as a MemoryTexture
            // — gsk uploads to the GPU once per frame and bilinear-upscales
            // for free in the compositor.
            {
                // Upgrade weak refs first; if the main window has closed,
                // both widgets are gone — break the timer instead of touching
                // freed Gdk surfaces.
                let (Some(stack), Some(pic)) = (
                    viz_stack_tick.upgrade(),
                    granite_pic_tick.upgrade(),
                ) else {
                    return ControlFlow::Break;
                };

                // If the widget has no root (no GtkWindow ancestor), the
                // surface is being torn down. Skip set_paintable to avoid a
                // gsk paint on a freed Gdk surface.
                if pic.root().is_none() {
                    return ControlFlow::Break;
                }

                let mode = state.borrow().config.visualizer.mode.clone();
                if mode == VisualizerMode::Granite {
                    if stack.visible_child_name().as_deref() != Some("granite") {
                        stack.set_visible_child_name("granite");
                    }
                    // Single-driver rule: yield while the fullscreen window
                    // owns the renderer (the mini keeps its last texture).
                    if !fs_viz_open_tick.get() {
                        // Aspect-matched internal width: viewport-aspect × fixed
                        // 360 short axis. Fall back to 16:9 when the widget hasn't
                        // been allocated yet.
                        let viewport_w = pic.width().max(1) as f64;
                        let viewport_h = pic.height().max(1) as f64;
                        let aspect = (viewport_w / viewport_h).max(0.5).min(4.0);
                        let h: u32 = crate::granite::GRANITE_INTERNAL_HEIGHT;
                        let w: u32 = (h as f64 * aspect).round() as u32;
                        let mut buf = granite_buf_tick.borrow_mut();
                        let need = (w as usize) * (h as usize) * 4;
                        if buf.len() != need {
                            buf.resize(need, 0);
                        }
                        let cfg = state.borrow().config.visualizer.granite;
                        state.borrow_mut().player.render_granite(&mut buf, w, h, &cfg);
                        let bytes = glib::Bytes::from(&buf[..]);
                        let texture = gdk::MemoryTexture::new(
                            w as i32,
                            h as i32,
                            gdk::MemoryFormat::R8g8b8a8,
                            &bytes,
                            (w * 4) as usize,
                        );
                        pic.set_paintable(Some(&texture));
                    }
                } else if stack.visible_child_name().as_deref() != Some("cairo") {
                    stack.set_visible_child_name("cairo");
                }
            }

            // 6. Periodically flush the duration cache and config to disk (every 30 s).
            // Saving config here ensures settings survive force-kills.
            cache_save_countdown -= 1;
            if cache_save_countdown == 0 {
                cache_save_countdown = 300;
                state.borrow_mut().duration_cache.save_if_dirty();
                let _ = state.borrow().config.save();
            }

            ControlFlow::Continue
        });
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Visualizer draw function (mini box in the now-playing row)
    // ══════════════════════════════════════════════════════════════════════════
    // Note: parse_hex_color / draw_zoned_bar / draw_waveform are module-level
    // functions defined near the bottom of this file so they can also be called
    // from open_waveform_fullscreen.

    {
        let state = state.clone();
        viz.set_draw_func(move |_da, cr, width, height| {
            // ── Background ────────────────────────────────────────────────
            cr.set_source_rgb(0.05, 0.05, 0.05);
            cr.paint().ok();

            let s = state.borrow();
            let is_playing = *s.player.state() == PlayerState::Playing;
            let mode = s.config.visualizer.mode.clone();
            let display_bands_count = s.config.visualizer.display_bands;
            let bars_mirror = s.config.visualizer.bars_mirror;
            let color_zones = s.config.visualizer.color_zones as usize;
            let zone_colors = s.config.visualizer.zone_colors.clone();
            let wf_zones = s.config.visualizer.waveform_color_zones as usize;
            let wf_zone_colors = s.config.visualizer.waveform_zone_colors.clone();
            let wf_style = s.config.visualizer.waveform_style.clone();

            // Get spectrum and waveform data before dropping the borrow.
            let display_bands_data = s.player.get_spectrum_display_bands(display_bands_count);
            let waveform_samples = s.player.get_waveform_samples(width.max(64) as usize);
            drop(s);

            if !is_playing {
                // Idle: flat dim centre line.
                cr.set_source_rgb(0.0, 0.3, 0.1);
                cr.set_line_width(1.0);
                let mid = height as f64 / 2.0;
                cr.move_to(0.0, mid);
                cr.line_to(width as f64, mid);
                cr.stroke().ok();
                return;
            }

            match mode {
                VisualizerMode::Bars => {
                    let num_bars = display_bands_count.max(10) as usize;
                    let bar_w = width as f64 / num_bars as f64;

                    if !display_bands_data.iter().all(|&v| v == 0.0) {
                        for (i, &amp) in display_bands_data.iter().enumerate() {
                            let x = i as f64 * bar_w;
                            draw_zoned_bar(
                                &cr,
                                x,
                                bar_w,
                                height as f64,
                                amp,
                                bars_mirror,
                                color_zones,
                                &zone_colors,
                            );
                        }
                    } else {
                        cr.set_source_rgb(0.0, 0.3, 0.1);
                        cr.set_line_width(1.0);
                        let mid = height as f64 / 2.0;
                        cr.move_to(0.0, mid);
                        cr.line_to(width as f64, mid);
                        cr.stroke().ok();

                        cr.set_source_rgb(0.0, 0.5, 0.2);
                        let font_size = 10.0_f64.min(height as f64 * 0.4);
                        cr.set_font_size(font_size);
                        let text = "Retry";
                        if let Ok(extents) = cr.text_extents(text) {
                            let text_x =
                                (width as f64 - extents.width()) / 2.0 - extents.x_bearing();
                            let text_y =
                                (height as f64 - extents.height()) / 2.0 - extents.y_bearing();
                            cr.move_to(text_x, text_y);
                            cr.show_text(text).ok();
                        }
                    }
                }
                VisualizerMode::Waveform => {
                    draw_waveform(
                        &cr,
                        width as f64,
                        height as f64,
                        &waveform_samples,
                        wf_zones,
                        &wf_zone_colors,
                        &wf_style,
                    );
                }
                // Granite is rendered by a separate Picture widget swapped in
                // via a Stack (see step 4); the Cairo DrawingArea sits behind
                // the Picture and isn't visible while Granite is active. Draw
                // nothing here so we don't waste cycles on a hidden surface.
                VisualizerMode::Granite => {}
            }
        });
    }

    // ══════════════════════════════════════════════════════════════════════════
    // ══════════════════════════════════════════════════════════════════════════
    // ══════════════════════════════════════════════════════════════════════════
    // Jump window — dedicated search/jump interface (opened with 'j').
    // Lives in its own window separate from the playlist so the two don't
    // overlap.  Populated fresh every time it opens.
    // ══════════════════════════════════════════════════════════════════════════
    let jump_entry = gtk4::SearchEntry::new();
    jump_entry.set_placeholder_text(Some("Search… (↑↓ navigate, Enter play, Esc close)"));
    jump_entry.set_margin_top(8);
    jump_entry.set_margin_bottom(4);
    jump_entry.set_margin_start(8);
    jump_entry.set_hexpand(true);

    let jump_clear_btn = Button::with_label("✕");
    jump_clear_btn.add_css_class("pl-btn");
    jump_clear_btn.set_margin_top(8);
    jump_clear_btn.set_margin_bottom(4);
    jump_clear_btn.set_margin_end(8);

    let jump_search_row = GtkBox::new(Orientation::Horizontal, 4);
    jump_search_row.append(&jump_entry);
    jump_search_row.append(&jump_clear_btn);

    let jump_box = ListBox::new();
    jump_box.add_css_class("playlist");
    jump_box.set_selection_mode(gtk4::SelectionMode::Single);

    let jump_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .min_content_height(280)
        .child(&jump_box)
        .build();

    // Status line below the results box: shows match count or a hint.
    let jump_status = gtk4::Label::builder()
        .halign(Align::Start)
        .margin_start(8)
        .margin_end(8)
        .margin_top(2)
        .margin_bottom(4)
        .build();
    jump_status.add_css_class("status-label");

    let jump_root = gtk4::Box::new(Orientation::Vertical, 0);
    jump_root.append(&jump_search_row);
    jump_root.append(&jump_scroll);
    jump_root.append(&jump_status);

    let jump_win = gtk4::Window::builder()
        .title("Jump to Track")
        .default_width(380)
        .default_height(360)
        .modal(false)
        .build();
    jump_win.set_transient_for(Some(&window));
    jump_win.set_child(Some(&jump_root));
    // Hide instead of destroy when the user closes the window so it can be
    // reopened later.  Without this, the underlying GObject may be freed after
    // the first close, making subsequent `present()` calls a no-op.
    jump_win.set_hide_on_close(true);
    jump_win.connect_visible_notify({
        let btn = btn_jump_vol.clone();
        move |w| {
            if w.is_visible() {
                btn.add_css_class("mode-btn-active");
            } else {
                btn.remove_css_class("mode-btn-active");
            }
        }
    });

    // Maps each visible row in jump_box → the original track index in the playlist.
    let jump_indices: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    // Maximum rows shown in the jump list.  Caps widget creation so the window
    // stays responsive on playlists with tens of thousands of tracks.
    const MAX_JUMP_RESULTS: usize = 500;

    // Closure: clear and repopulate jump_box based on the current query.
    let rebuild_jump: Rc<dyn Fn()> = {
        let state = state.clone();
        let jump_entry = jump_entry.clone();
        let jump_box = jump_box.clone();
        let jump_indices = jump_indices.clone();
        let jump_status = jump_status.clone();
        Rc::new(move || {
            // remove_all() is a single GTK call instead of O(n) individual removes.
            jump_box.remove_all();
            let mut indices = jump_indices.borrow_mut();
            indices.clear();

            let q = jump_entry.text();
            // Empty query: show a hint and leave the list empty.
            // Without this guard, an empty query would match every track and
            // create tens of thousands of widgets, freezing the UI.
            if q.trim().is_empty() {
                let total = state.borrow().playlist.len();
                jump_status.set_text(&format!("{total} tracks — type to search"));
                return;
            }

            let all_matches = {
                let s = state.borrow();
                s.playlist.search_indices(&q)
            };
            let total_matches = all_matches.len();
            let capped = total_matches > MAX_JUMP_RESULTS;
            let s = state.borrow();
            for &idx in all_matches.iter().take(MAX_JUMP_RESULTS) {
                let track = &s.playlist.tracks[idx];
                let label_text = if track.artist.is_empty() {
                    format!("{:2}. {}", idx + 1, track.title)
                } else {
                    format!("{:2}. {} — {}", idx + 1, track.artist, track.title)
                };
                let row_label = gtk4::Label::builder()
                    .label(&label_text)
                    .halign(Align::Start)
                    .ellipsize(gtk4::pango::EllipsizeMode::End)
                    .build();
                row_label.set_margin_start(6);
                row_label.set_margin_end(6);
                row_label.set_margin_top(3);
                row_label.set_margin_bottom(3);
                let row = gtk4::ListBoxRow::new();
                row.set_child(Some(&row_label));
                jump_box.append(&row);
                indices.push(idx);
            }
            drop(s);

            // Status line.
            if total_matches == 0 {
                jump_status.set_text("No matches");
            } else if capped {
                jump_status.set_text(&format!(
                    "Showing {} of {} matches — type more to narrow",
                    MAX_JUMP_RESULTS, total_matches
                ));
            } else {
                jump_status.set_text(&format!("{total_matches} match{}", if total_matches == 1 { "" } else { "es" }));
            }

            // Auto-select the first row so Enter immediately plays.
            if let Some(row) = jump_box.row_at_index(0) {
                jump_box.select_row(Some(&row));
            }
        })
    };

    // Wire up the jump-window clear button now that rebuild_jump is in scope.
    {
        let e = jump_entry.clone();
        let rj = rebuild_jump.clone();
        jump_clear_btn.connect_clicked(move |_| {
            gtk4::prelude::EditableExt::set_text(&e, "");
            rj();
        });
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Keyboard shortcuts — shared handler applied to player + playlist windows.
    // ══════════════════════════════════════════════════════════════════════════

    let handle_key: Rc<dyn Fn(gdk::Key) -> glib::Propagation> = {
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let status_label = status_label.clone();
        let pl_status = pl_status_label.clone();
        let kbd_set_track = set_track.clone();
        let kbd_rebuild = rebuild_playlist.clone();
        let kbd_vol_bar = vol_bar.clone();
        let kbd_seek_bar = seek_bar.clone();
        let playlist_win_wk = playlist_win.downgrade();
        // Strong reference: keeps the window alive even when hidden, so
        // repeated open/close cycles work without recreating the widget tree.
        let kbd_jump_win = jump_win.clone();
        let window_weak = window.downgrade();
        let remove_sel = remove_selected.clone();
        let kbd_probe_tx = probe_tx.clone();
        let kbd_broken_tx = broken_tx.clone();
        let kbd_rebuild_jump = rebuild_jump.clone();
        let kbd_jump_entry = jump_entry.clone();
        let kbd_btn_info = btn_info.clone();
        // Clones for r/s key handlers to update button visuals.
        let kbd_btn_repeat = btn_repeat.clone();
        let kbd_repeat_icon = repeat_icon.clone();
        let kbd_repeat_label = repeat_label.clone();
        let kbd_btn_shuffle = btn_shuffle.clone();
        // Clones for z/b (prev/next) handlers — use patch instead of rebuild
        // so the scroll position is preserved rather than reset to the top.
        let kbd_patch_row = patch_pl_row.clone();
        let kbd_scroll = scroll_to_row_if_needed.clone();
        let kbd_open_fs = open_fullscreen_fn.clone();

        Rc::new(move |key: gdk::Key| -> glib::Propagation {
            match key {
                // ── Winamp transport bindings ──────────────────────────────
                gdk::Key::z => {
                    let old_idx = state.borrow().playlist.current_index;
                    let result = { state.borrow_mut().play_prev() };
                    if let Some(d) = result {
                        kbd_set_track(&d);
                        let new_idx = state.borrow().playlist.current_index;
                        if old_idx != new_idx {
                            kbd_patch_row(old_idx);
                        }
                        kbd_patch_row(new_idx);
                        kbd_scroll(new_idx);
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::x => {
                    let ps = state.borrow().player.state().clone();
                    match ps {
                        PlayerState::Stopped | PlayerState::Paused => play_and_update(),
                        PlayerState::Playing => {}
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::c => {
                    let _ = state.borrow_mut().player.toggle_pause();
                    glib::Propagation::Stop
                }
                gdk::Key::v => {
                    let _ = state.borrow_mut().player.stop();
                    kbd_seek_bar.set_value(0.0);
                    glib::Propagation::Stop
                }
                gdk::Key::b => {
                    let old_idx = state.borrow().playlist.current_index;
                    let result = { state.borrow_mut().play_next() };
                    if let Some(d) = result {
                        kbd_set_track(&d);
                        let new_idx = state.borrow().playlist.current_index;
                        if old_idx != new_idx {
                            kbd_patch_row(old_idx);
                        }
                        kbd_patch_row(new_idx);
                        kbd_scroll(new_idx);
                    }
                    glib::Propagation::Stop
                }

                // ── Arrow keys: seek ±5 seconds ───────────────────────────
                // GTK fires key-repeat while the key is held, so holding Left
                // or Right continuously rewinds / fast-forwards the track.
                gdk::Key::Left => {
                    state.borrow_mut().seek_delta_secs(-5.0);
                    glib::Propagation::Stop
                }
                gdk::Key::Right => {
                    state.borrow_mut().seek_delta_secs(5.0);
                    glib::Propagation::Stop
                }

                // ── Volume: - decreases, = / + increases ──────────────────
                // GTK fires key-repeat while the key is held, so volume
                // continues to ramp as long as the key is held down.
                gdk::Key::minus => {
                    let new_vol = {
                        let s = state.borrow();
                        (s.config.playback.volume - 0.05).clamp(0.0, 1.0)
                    };
                    {
                        let mut s = state.borrow_mut();
                        s.config.playback.volume = new_vol;
                        s.player.set_volume(new_vol);
                    }
                    kbd_vol_bar.set_value(new_vol);
                    glib::Propagation::Stop
                }
                gdk::Key::equal | gdk::Key::plus => {
                    let new_vol = {
                        let s = state.borrow();
                        (s.config.playback.volume + 0.05).clamp(0.0, 1.0)
                    };
                    {
                        let mut s = state.borrow_mut();
                        s.config.playback.volume = new_vol;
                        s.player.set_volume(new_vol);
                    }
                    kbd_vol_bar.set_value(new_vol);
                    glib::Propagation::Stop
                }

                // ── Visualizer mode toggle ─────────────────────────────────
                gdk::Key::a | gdk::Key::A => {
                    state.borrow_mut().toggle_visualizer_mode();
                    glib::Propagation::Stop
                }

                // ── Random Granite effect (e — Granite mode) ───────────────
                gdk::Key::e | gdk::Key::E => {
                    let mut s = state.borrow_mut();
                    if matches!(s.config.visualizer.mode, VisualizerMode::Granite) {
                        if let Some(eff) = s.player.granite_random_effect() {
                            // Record in config so pinned mode (auto-switch
                            // off) follows along instead of snapping back.
                            s.config.visualizer.granite.effect = eff;
                        }
                    }
                    glib::Propagation::Stop
                }

                // ── Visualizer fullscreen (f — Waveform or Granite mode) ──
                gdk::Key::f | gdk::Key::F => {
                    let supports_fs = matches!(
                        state.borrow().config.visualizer.mode,
                        VisualizerMode::Waveform | VisualizerMode::Granite,
                    );
                    if supports_fs {
                        if let Some(ref opener) = *kbd_open_fs.borrow() {
                            opener();
                        }
                    }
                    glib::Propagation::Stop
                }

                // ── Jump window ────────────────────────────────────────────
                gdk::Key::j | gdk::Key::J => {
                    kbd_jump_entry.set_text("");
                    kbd_rebuild_jump();
                    kbd_jump_win.present();
                    kbd_jump_entry.grab_focus();
                    glib::Propagation::Stop
                }

                // ── Add file (single file via desktop file browser) ────────
                gdk::Key::n => {
                    // Build a reusable audio filter for all common formats.
                    let filter = gtk4::FileFilter::new();
                    filter.set_name(Some("Audio files"));
                    for mime in &[
                        "audio/mpeg",
                        "audio/flac",
                        "audio/ogg",
                        "audio/opus",
                        "audio/wav",
                        "audio/aac",
                        "audio/mp4",
                        "audio/x-m4a",
                    ] {
                        filter.add_mime_type(mime);
                    }
                    for pat in &[
                        "*.mp3", "*.flac", "*.ogg", "*.opus", "*.wav", "*.aac", "*.m4a",
                    ] {
                        filter.add_pattern(pat);
                    }
                    let filters = gio::ListStore::new::<gtk4::FileFilter>();
                    filters.append(&filter);

                    let dialog = gtk4::FileDialog::builder().title("Add Audio File").build();
                    dialog.set_filters(Some(&filters));

                    let state_cb = state.clone();
                    let rebuild_cb = rebuild_playlist.clone();
                    let status_cb = status_label.clone();
                    let pl_stat_cb = pl_status.clone();
                    let probe_tx_cb = kbd_probe_tx.clone();
                    let broken_tx_cb = kbd_broken_tx.clone();
                    let parent = window_weak.upgrade();
                    dialog.open(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                        if let Ok(file) = result {
                            if let Some(path) = file.path() {
                                let before = state_cb.borrow().playlist.tracks.len();
                                let outcome = state_cb.borrow_mut().add_path(&path);
                                match outcome {
                                    Ok(msg) => {
                                        status_cb.set_text(&msg);
                                        pl_stat_cb.set_text(&msg);
                                        rebuild_cb();
                                        let paths = state_cb.borrow().uncached_paths_from(before);
                                        if !paths.is_empty() {
                                            duration_probe::spawn_probes(
                                                paths,
                                                probe_tx_cb.clone(),
                                                broken_tx_cb.clone(),
                                            );
                                        }
                                    }
                                    Err(msg) => {
                                        status_cb.set_text(&msg);
                                    }
                                }
                            }
                        }
                    });
                    glib::Propagation::Stop
                }

                // ── Playlist window toggle ─────────────────────────────────
                gdk::Key::p | gdk::Key::P => {
                    if let Some(pw) = playlist_win_wk.upgrade() {
                        pw.set_visible(!pw.is_visible());
                    }
                    glib::Propagation::Stop
                }

                // ── Delete: remove all selected playlist rows ──────────────
                gdk::Key::Delete => {
                    remove_sel();
                    glib::Propagation::Stop
                }

                // ── Repeat mode cycle (r) ─────────────────────────────────
                gdk::Key::r | gdk::Key::R => {
                    let new_mode = {
                        let mut s = state.borrow_mut();
                        let m = s.config.playback.repeat_mode.cycle();
                        s.config.playback.repeat_mode = m;
                        m
                    };
                    kbd_repeat_icon.set_icon_name(Some(repeat_btn_icon(new_mode)));
                    kbd_repeat_label.set_text(repeat_btn_text(new_mode));
                    if new_mode == crate::shuffle::RepeatMode::Off {
                        kbd_btn_repeat.remove_css_class("mode-btn-active");
                    } else {
                        kbd_btn_repeat.add_css_class("mode-btn-active");
                    }
                    glib::Propagation::Stop
                }

                // ── Shuffle toggle (s — hidden; only shown in help) ───────
                gdk::Key::s | gdk::Key::S => {
                    let enabled = {
                        let mut s = state.borrow_mut();
                        s.shuffle_state.toggle();
                        s.shuffle_state.reset();
                        let on = s.shuffle_state.enabled;
                        // Mirror to config so the setting survives to the next session.
                        s.config.playback.shuffle_enabled = on;
                        on
                    };
                    if enabled {
                        kbd_btn_shuffle.add_css_class("mode-btn-active");
                    } else {
                        kbd_btn_shuffle.remove_css_class("mode-btn-active");
                    }
                    glib::Propagation::Stop
                }

                // ── ID3 tag editor (d) — open for the currently playing track ─
                gdk::Key::d | gdk::Key::D => {
                    let path = state.borrow().playlist.current().map(|t| t.path.clone());
                    if let Some(path) = path {
                        if let Some(w) = window_weak.upgrade() {
                            open_id3_editor_window(
                                Some(&w),
                                path,
                                state.clone(),
                                kbd_rebuild.clone(),
                                None,
                            );
                        }
                    } else {
                        status_label.set_text("No track loaded");
                    }
                    glib::Propagation::Stop
                }

                // ── Info / keyboard shortcuts window ──────────────────────
                gdk::Key::i | gdk::Key::I => {
                    kbd_btn_info.activate();
                    glib::Propagation::Stop
                }

                // ── Quit ──────────────────────────────────────────────────
                gdk::Key::q | gdk::Key::Q => {
                    let _ = state.borrow().playlist.save_last();
                    if let Some(w) = window_weak.upgrade() {
                        // Closing the main window triggers connect_close_request
                        // which also saves the playlist — belt-and-suspenders.
                        w.close();
                    }
                    glib::Propagation::Stop
                }

                _ => glib::Propagation::Proceed,
            }
        })
    };

    // Wire up the fullscreen opener now that handle_key is fully defined.
    {
        let hk = handle_key.clone();
        let state_fs = state.clone();
        let jump_win_fs = jump_win.clone();
        let jump_entry_fs = jump_entry.clone();
        let rebuild_jump_fs = rebuild_jump.clone();
        let btn_info_fs = btn_info.clone();
        let fs_viz_open_fs = fs_viz_open.clone();
        *open_fullscreen_fn.borrow_mut() = Some(Rc::new(move || {
            open_waveform_fullscreen(
                state_fs.clone(),
                hk.clone(),
                jump_win_fs.clone(),
                jump_entry_fs.clone(),
                rebuild_jump_fs.clone(),
                btn_info_fs.clone(),
                fs_viz_open_fs.clone(),
            );
        }));
    }

    // Attach the shared handler to the main player window.
    // Capture phase ensures keys reach the handler even when a child widget
    // (e.g. the visualizer DrawingArea) has keyboard focus.
    {
        let key_ctrl = EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let handler = handle_key.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, _| handler(key));
        window.add_controller(key_ctrl);
    }

    // Attach the same handler to the playlist window so all shortcuts work
    // even when the playlist window has keyboard focus.  Use Capture phase so
    // the ListBox cannot swallow keys (e.g. 'j') before they reach this handler.
    {
        let key_ctrl = EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let handler = handle_key.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, _| handler(key));
        playlist_win.add_controller(key_ctrl);
    }

    // ── Persistent shortcuts window (created once; shown/hidden as a toggle) ──
    // Built here after handle_key is defined so the Esc/transport shortcuts
    // work inside it.
    let shortcuts_win = {
        let win = gtk4::Window::builder()
            .title("Keyboard Shortcuts")
            .modal(false)
            .default_width(420)
            .default_height(480)
            .build();
        win.set_transient_for(Some(window.upcast_ref::<gtk4::Window>()));

        let sections: &[(&str, &[(&str, &str)])] = &[
            ("Playback", &[
                ("z",          "Previous track / restart"),
                ("x",          "Play"),
                ("c",          "Pause / resume"),
                ("v",          "Stop"),
                ("b",          "Next track"),
                ("← →",        "Seek −5 s / +5 s"),
                ("r",          "Cycle repeat (off / song / playlist)"),
                ("s",          "Toggle shuffle on/off"),
            ]),
            ("Volume", &[
                ("-",          "Volume down 5 %"),
                ("=",          "Volume up 5 %"),
            ]),
            ("Playlist", &[
                ("n",          "Add file(s) or folder(s)"),
                ("j",          "Jump / search"),
                ("↑ k / ↓ l",  "Browse up / down"),
                ("Enter",      "Play selected track"),
                ("Del",        "Remove highlighted track"),
                ("p",          "Toggle playlist window"),
            ]),
            ("View & Tags", &[
                ("a",           "Cycle visualizer mode (Bars / Waveform / Granite)"),
                ("e",           "Random Granite effect (Granite mode)"),
                ("f",           "Fullscreen visualizer (Waveform or Granite mode; Esc to exit)"),
                ("g",           "Toggle FPS / BPM overlay (fullscreen only)"),
                ("d",           "View/Edit ID3 tags for current track"),
                ("u",           "Open EQ (TUI only — use EQ button in GUI)"),
                ("Click logo",  "Open settings"),
            ]),
            ("Other", &[
                ("i",          "Toggle this help"),
                ("q / Esc",    "Quit"),
            ]),
        ];

        let grid = gtk4::Grid::builder()
            .column_spacing(16)
            .row_spacing(4)
            .halign(gtk4::Align::Fill)
            .valign(gtk4::Align::Start)
            .build();

        // Title row.
        let title = gtk4::Label::builder()
            .label("Sparkamp — Keyboard Shortcuts")
            .halign(gtk4::Align::Start)
            .css_classes(["info-title"])
            .build();
        grid.attach(&title, 0, 0, 2, 1);

        let mut row: i32 = 1;
        // Spacer below title.
        let spacer = gtk4::Label::new(Some(""));
        grid.attach(&spacer, 0, row, 2, 1);
        row += 1;

        for (section, entries) in sections.iter() {
            let header = gtk4::Label::builder()
                .label(*section)
                .halign(gtk4::Align::Start)
                .css_classes(["info-section"])
                .build();
            grid.attach(&header, 0, row, 2, 1);
            row += 1;

            for (key, desc) in entries.iter() {
                let key_lbl = gtk4::Label::builder()
                    .label(*key)
                    .halign(gtk4::Align::Start)
                    .css_classes(["info-key"])
                    .build();
                let desc_lbl = gtk4::Label::builder()
                    .label(*desc)
                    .halign(gtk4::Align::Start)
                    .css_classes(["info-desc"])
                    .build();
                grid.attach(&key_lbl,  0, row, 1, 1);
                grid.attach(&desc_lbl, 1, row, 1, 1);
                row += 1;
            }

            // Section spacer.
            let spc = gtk4::Label::new(Some(""));
            grid.attach(&spc, 0, row, 2, 1);
            row += 1;
        }

        let body = GtkBox::new(Orientation::Vertical, 0);
        body.set_css_classes(&["info-text"]);
        body.append(&grid);

        let scroll = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .margin_top(12).margin_bottom(12)
            .margin_start(12).margin_end(12)
            .child(&body)
            .build();
        let key_ctrl = gtk4::EventControllerKey::new();
        let handler = handle_key.clone();
        let win_wk = win.downgrade();
        key_ctrl.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(w) = win_wk.upgrade() { w.hide(); }
                return glib::Propagation::Stop;
            }
            handler(key)
        });
        win.add_controller(key_ctrl);
        win.set_child(Some(&scroll));
        win.set_hide_on_close(true);
        win.connect_visible_notify({
            let btn = btn_info.clone();
            move |w| {
                if w.is_visible() {
                    btn.add_css_class("mode-btn-active");
                } else {
                    btn.remove_css_class("mode-btn-active");
                }
            }
        });
        win
    };

    // ℹ Info button — toggle keyboard shortcuts window.
    btn_info.connect_clicked({
        let sw = shortcuts_win.clone();
        move |_| {
            if sw.is_visible() { sw.hide(); } else { sw.present(); }
        }
    });

    // J button — toggle jump window.
    btn_jump_vol.connect_clicked({
        let jump_win_wk = jump_win.downgrade();
        let entry = jump_entry.clone();
        let rebuild = rebuild_jump.clone();
        move |_| {
            if let Some(w) = jump_win_wk.upgrade() {
                if w.is_visible() {
                    w.hide();
                } else {
                    entry.set_text("");
                    rebuild();
                    w.present();
                    entry.grab_focus();
                }
            }
        }
    });

    // ML button — toggle the media library browser window.
    btn_ml.connect_clicked({
        let window_wk = window.downgrade();
        let state_rc = state.clone();
        let rebuild_pl = rebuild_playlist.clone();
        let set_track_ml = set_track.clone();
        let btn_ml_for_notify = btn_ml.clone();
        move |_| {
            // If already open (visible or hidden), toggle visibility.
            {
                let s = state_rc.borrow();
                if let Some(ref w) = s.ml_window {
                    if w.is_visible() { w.hide(); } else { w.present(); }
                    return;
                }
            }
            // First open: create the window.
            let parent = window_wk.upgrade().map(|w| w.upcast::<gtk4::Window>());
            let (w, h) = {
                let cfg = &state_rc.borrow().config.window;
                (cfg.ml_width, cfg.ml_height)
            };
            let ml_win = open_media_library_window(
                parent.as_ref(),
                state_rc.clone(),
                rebuild_pl.clone(),
                set_track_ml.clone(),
                w,
                h,
            );
            ml_win.set_hide_on_close(true);
            ml_win.connect_visible_notify({
                let btn = btn_ml_for_notify.clone();
                move |w| {
                    if w.is_visible() {
                        btn.add_css_class("mode-btn-active");
                    } else {
                        btn.remove_css_class("mode-btn-active");
                    }
                }
            });
            // open_media_library_window already calls present() before
            // returning, so the visible-notify above has already fired and
            // skipped attaching — sync the button state to match.
            btn_ml_for_notify.add_css_class("mode-btn-active");
            state_rc.borrow_mut().ml_window = Some(ml_win);
        }
    });

    // EQ button — toggle the 10-band equalizer window.
    let eq_win_ref: Rc<RefCell<Option<gtk4::Window>>> = Rc::new(RefCell::new(None));
    btn_eq.connect_clicked({
        let window_wk = window.downgrade();
        let state_rc = state.clone();
        let eq_ref = eq_win_ref.clone();
        let btn_eq_for_notify = btn_eq.clone();
        move |_| {
            // Toggle if already created.
            {
                let existing = eq_ref.borrow();
                if let Some(ref w) = *existing {
                    if w.is_visible() { w.hide(); } else { w.present(); }
                    return;
                }
            }
            // First open: create the window.
            let parent = window_wk.upgrade().map(|w| w.upcast::<gtk4::Window>());
            let win = open_eq_window(parent.as_ref(), state_rc.clone());
            win.connect_visible_notify({
                let btn = btn_eq_for_notify.clone();
                move |w| {
                    if w.is_visible() {
                        btn.add_css_class("mode-btn-active");
                    } else {
                        btn.remove_css_class("mode-btn-active");
                    }
                }
            });
            // open_eq_window calls present() before returning; sync the
            // button state since the notify handler attached above fires only
            // on subsequent visibility changes.
            btn_eq_for_notify.add_css_class("mode-btn-active");
            *eq_ref.borrow_mut() = Some(win);
        }
    });

    // ══════════════════════════════════════════════════════════════════════════
    // Jump window callbacks (wired after handle_key so the key controller can
    // delegate transport shortcuts to it).
    // ══════════════════════════════════════════════════════════════════════════

    // Typing in the jump entry: immediately refilter results.
    jump_entry.connect_changed({
        let rebuild_jump = rebuild_jump.clone();
        move |_| {
            rebuild_jump();
        }
    });

    // Enter: play the selected (or first) result and close the window.
    jump_entry.connect_activate({
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        let patch_pl_row = patch_pl_row.clone();
        let jump_box = jump_box.clone();
        let jump_indices = jump_indices.clone();
        let jump_win_wk = jump_win.downgrade();
        move |_| {
            let sel_row_idx = jump_box.selected_row().map(|r| r.index() as usize);
            if let Some(list_pos) = sel_row_idx {
                if let Some(&track_idx) = jump_indices.borrow().get(list_pos) {
                    let old_idx = state.borrow().playlist.current_index;
                    state.borrow_mut().playlist.jump_to(track_idx);
                    play_and_update();
                    if old_idx != track_idx {
                        patch_pl_row(old_idx);
                    }
                }
            }
            if let Some(w) = jump_win_wk.upgrade() {
                w.close();
            }
        }
    });

    // SearchEntry emits stop-search (and consumes Escape) before window-level
    // key controllers see it.  Wire the signal directly so Escape always closes.
    jump_entry.connect_stop_search({
        let jw = jump_win.clone();
        move |_| {
            jw.close();
        }
    });

    // Key controller for the jump window: ↑↓ navigate rows; Escape as a
    // fallback in case focus is on the list box rather than the entry.
    // PropagationPhase::Capture ensures we intercept before child widgets.
    {
        let key_ctrl = EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let jb = jump_box.clone();
        let jw_wk = jump_win.downgrade();
        key_ctrl.connect_key_pressed(move |_, key, _, _| match key {
            gdk::Key::Escape => {
                if let Some(w) = jw_wk.upgrade() {
                    w.close();
                }
                glib::Propagation::Stop
            }
            gdk::Key::Up => {
                let cur = jb.selected_row().map(|r| r.index()).unwrap_or(1);
                if let Some(row) = jb.row_at_index((cur - 1).max(0)) {
                    jb.select_row(Some(&row));
                }
                glib::Propagation::Stop
            }
            gdk::Key::Down => {
                let cur = jb.selected_row().map(|r| r.index()).unwrap_or(-1);
                if let Some(row) = jb.row_at_index(cur + 1) {
                    jb.select_row(Some(&row));
                }
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        jump_win.add_controller(key_ctrl);
    }

    // Double-clicking a result plays it immediately.
    jump_box.connect_row_activated({
        let state = state.clone();
        let play_and_update = play_and_update.clone();
        let patch_pl_row = patch_pl_row.clone();
        let jump_indices = jump_indices.clone();
        let jump_win_wk = jump_win.downgrade();
        move |_, row| {
            let list_pos = row.index() as usize;
            if let Some(&track_idx) = jump_indices.borrow().get(list_pos) {
                let old_idx = state.borrow().playlist.current_index;
                state.borrow_mut().playlist.jump_to(track_idx);
                play_and_update();
                if old_idx != track_idx {
                    patch_pl_row(old_idx);
                }
            }
            if let Some(w) = jump_win_wk.upgrade() {
                w.close();
            }
        }
    });

    // ══════════════════════════════════════════════════════════════════════════
    // Window close handlers
    // ══════════════════════════════════════════════════════════════════════════

    // Main window close: save both windows' geometry and playlist-visible state,
    // then destroy the playlist window so the app quits cleanly.
    // Using destroy() bypasses playlist_win's close_request handler (which only
    // hides it) so no ApplicationWindow is left alive keeping the process running.
    window.connect_close_request({
        let state = state.clone();
        let playlist_win = playlist_win.clone();
        let viz_shut = viz_shutting_down.clone();
        move |w| {
            // Stop the 33 ms visualizer timer before any gsk paint can run
            // against a freed surface.
            viz_shut.set(true);
            // Stop new blocking device FUSE work from starting during teardown,
            // so a slow MTP mount can't pin a worker thread and delay exit.
            DEVICE_IO_SHUTDOWN.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = state.borrow().playlist.save_last();

            let mut cfg = state.borrow().config.clone();
            cfg.window.player_width = w.width();
            cfg.window.player_height = w.height();
            cfg.window.playlist_visible = playlist_win.is_visible();
            // If the playlist window is currently visible, capture its live
            // size.  If it was already hidden, its size was already written to
            // cfg by playlist_win.connect_close_request, so we leave it alone.
            if playlist_win.is_visible() {
                cfg.window.playlist_width = playlist_win.width();
                cfg.window.playlist_height = playlist_win.height();
            }
            cfg.window.ml_visible = state.borrow().ml_window.is_some();
            // Record ML window size for next launch.
            if let Some(ref ml_win) = state.borrow().ml_window {
                cfg.window.ml_width = ml_win.width();
                cfg.window.ml_height = ml_win.height();
            }
            let _ = cfg.save();

            // Quit through the GApplication rather than destroying the other
            // ApplicationWindows (playlist_win / ml_win) by hand: a manual
            // `.destroy()` from inside this close-request handler re-enters GTK's
            // window teardown and segfaults (GtkApplication mutates its window
            // list mid signal-emission). `app.quit()` closes every window and
            // unwinds the main loop cleanly, and still guarantees the process
            // exits even though those windows use hide-on-close.
            if let Some(app) = w.application() {
                app.quit();
            }
            glib::Propagation::Proceed
        }
    });

    // After the main window is realized, read the computed text color of the
    // hidden .np-title probe label and cache it as gdk::RGBA.  The cell data
    // func reads this directly — no string parsing, no GTK color warnings.
    // Hooking the main window (not the playlist window) means the color is
    // available the moment the app starts.
    {
        let accent_rgba = accent_rgba.clone();
        let np_probe = np_probe.clone();
        let patch_pl_row = patch_pl_row.clone();
        let state = state.clone();
        window.connect_realize(move |_| {
            *accent_rgba.borrow_mut() = Some(np_probe.color());
            // Re-patch the current row so it immediately gets the accent color
            // if a track is already playing when the app starts.
            let idx = state.borrow().playlist.current_index;
            patch_pl_row(idx);
        });
    }

    window.present();
    if init_playlist_visible {
        // Delay the playlist window slightly so the Wayland compositor has
        // time to place and map the main window first.  Without this, the
        // playlist window often appears half off-screen because the compositor
        // hasn't resolved the transient-parent relationship yet.
        glib::timeout_add_local_once(Duration::from_millis(50), move || {
            playlist_win.present();
        });
    }
    if init_ml_visible {
        let set_track_init_ml = set_track.clone();
        let btn_ml_for_restore = btn_ml.clone();
        glib::timeout_add_local_once(Duration::from_millis(50), move || {
            let state_rc = state.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let ml_win = open_media_library_window(
                Some(&window.upcast::<gtk4::Window>()),
                state_rc.clone(),
                rebuild_pl.clone(),
                set_track_init_ml.clone(),
                init_ml_width,
                init_ml_height,
            );
            // Mirror the click-handler path: hide-on-close keeps the window
            // alive across toggles, and visible-notify keeps the toolbar
            // button's active class in sync with whether the window is shown.
            ml_win.set_hide_on_close(true);
            ml_win.connect_visible_notify({
                let btn = btn_ml_for_restore.clone();
                move |w| {
                    if w.is_visible() {
                        btn.add_css_class("mode-btn-active");
                    } else {
                        btn.remove_css_class("mode-btn-active");
                    }
                }
            });
            // open_media_library_window calls present() before returning, so
            // the notify above missed the initial show — sync the class now.
            btn_ml_for_restore.add_css_class("mode-btn-active");
            state_rc.borrow_mut().ml_window = Some(ml_win);
        });
    }
}

// ---------------------------------------------------------------------------
// ID3 editor windows
// ---------------------------------------------------------------------------

/// Get the display value for an ID3 editable field.
fn get_id3_field_value(
    fields: &crate::id3_editor::TagFields,
    track_meta: &Option<crate::media_library::LibTrack>,
    id: &str,
) -> String {
    match id {
        "title" => fields.title.clone(),
        "artist" => fields.artist.clone(),
        "album" => fields.album.clone(),
        "album_artist" => fields.album_artist.clone(),
        "year" => fields.year.clone(),
        "genre" => fields.genre.clone(),
        "track_num" => fields.track_number.clone(),
        "track_total" => fields.track_total.clone(),
        "disc_num" => fields.disc_number.clone(),
        "disc_total" => fields.disc_total.clone(),
        "bpm" => fields.bpm.clone(),
        "comment" => fields.comment.clone(),
        "composer" => track_meta
            .as_ref()
            .and_then(|t| t.composer.clone())
            .unwrap_or_default(),
        "original_artist" => track_meta
            .as_ref()
            .and_then(|t| t.original_artist.clone())
            .unwrap_or_default(),
        "copyright" => track_meta
            .as_ref()
            .and_then(|t| t.copyright.clone())
            .unwrap_or_default(),
        "url" => track_meta
            .as_ref()
            .and_then(|t| t.url.clone())
            .unwrap_or_default(),
        "encoded_by" => track_meta
            .as_ref()
            .and_then(|t| t.encoded_by.clone())
            .unwrap_or_default(),
        "lyric" => track_meta
            .as_ref()
            .and_then(|t| t.lyric.clone())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// ID3 field customizer — two-column layout with up/down reorder and DnD
// ---------------------------------------------------------------------------

fn open_id3_field_customizer(
    parent: Option<&gtk4::Window>,
    state: Rc<RefCell<AppState>>,
    on_close: Option<Rc<dyn Fn()>>,
) {
    #[derive(Clone)]
    struct FE {
        id: String,
        label: String,
        visible: bool,
        column: usize, // 0 = left, 1 = right
    }

    let visible_ids = state.borrow().config.media_library.id3_visible_columns.clone();
    let col_pos = state.borrow().config.media_library.id3_column_position.clone();
    let editable: Vec<&MlColumnDef> = ALL_COLUMNS.iter().filter(|c| c.id3_editable).collect();

    // Visible fields first (in their saved order), then invisible fields appended
    let mut entries: Vec<FE> = visible_ids
        .iter()
        .filter_map(|id| editable.iter().find(|c| c.id == id.as_str()))
        .map(|c| FE {
            id: c.id.to_string(),
            label: c.header.to_string(),
            visible: true,
            column: if col_pos.get(c.id).map_or(false, |p| p == "right") { 1 } else { 0 },
        })
        .collect();
    for c in &editable {
        if !visible_ids.contains(&c.id.to_string()) {
            entries.push(FE {
                id: c.id.to_string(),
                label: c.header.to_string(),
                visible: false,
                column: if col_pos.get(c.id).map_or(false, |p| p == "right") { 1 } else { 0 },
            });
        }
    }

    let fs: Rc<RefCell<Vec<FE>>> = Rc::new(RefCell::new(entries));

    // Persist current fs → config
    let save_cfg = {
        let fs = fs.clone();
        let st = state.clone();
        Rc::new(move || {
            let entries = fs.borrow();
            let vis: Vec<String> = entries.iter().filter(|e| e.visible).map(|e| e.id.clone()).collect();
            let pos: std::collections::HashMap<String, String> = entries
                .iter()
                .map(|e| (e.id.clone(), if e.column == 1 { "right" } else { "left" }.to_string()))
                .collect();
            let mut s = st.borrow_mut();
            s.config.media_library.id3_visible_columns = vis;
            s.config.media_library.id3_column_position = pos;
            let _ = s.config.save();
        })
    };

    // Window
    let dlg = gtk4::Window::new();
    dlg.set_title(Some("Customize ID3 Fields"));
    dlg.set_default_size(520, 440);
    dlg.set_resizable(true);
    if let Some(p) = parent {
        dlg.set_transient_for(Some(p));
    }

    let root_vbox = GtkBox::new(Orientation::Vertical, 0);

    // ── Header ──────────────────────────────────────────────────────────────
    {
        let hdr = GtkBox::new(Orientation::Horizontal, 8);
        hdr.set_margin_top(8);
        hdr.set_margin_bottom(8);
        hdr.set_margin_start(12);
        hdr.set_margin_end(12);
        let hint = Label::builder()
            .label("Use ▲ ▼ to reorder within a column, or drag rows. Use → ← to switch columns.")
            .halign(Align::Start)
            .hexpand(true)
            .build();
        hint.add_css_class("status-label");
        let spring = GtkBox::new(Orientation::Horizontal, 0);
        spring.set_hexpand(true);
        let done = Button::with_label("Done");
        done.add_css_class("suggested-action");
        hdr.append(&hint);
        hdr.append(&spring);
        hdr.append(&done);
        root_vbox.append(&hdr);
        root_vbox.append(&Separator::new(Orientation::Horizontal));

        let dlg_wk = dlg.downgrade();
        let oc = on_close.clone();
        done.connect_clicked(move |_| {
            if let Some(d) = dlg_wk.upgrade() { d.close(); }
            if let Some(ref cb) = oc { cb(); }
        });
    }

    // ── Column header row ────────────────────────────────────────────────────
    {
        let chr = GtkBox::new(Orientation::Horizontal, 0);
        let lh = Label::builder()
            .label("Left Column")
            .halign(Align::Center)
            .hexpand(true)
            .build();
        lh.add_css_class("ml-section-header");
        let rh = Label::builder()
            .label("Right Column")
            .halign(Align::Center)
            .hexpand(true)
            .build();
        rh.add_css_class("ml-section-header");
        chr.append(&lh);
        chr.append(&Separator::new(Orientation::Vertical));
        chr.append(&rh);
        root_vbox.append(&chr);
        root_vbox.append(&Separator::new(Orientation::Horizontal));
    }

    // ── Two-column list area ─────────────────────────────────────────────────
    let panels = GtkBox::new(Orientation::Horizontal, 0);
    panels.set_vexpand(true);
    panels.set_hexpand(true);

    let left_lb: Rc<ListBox> = Rc::new({
        let lb = ListBox::new();
        lb.add_css_class("playlist");
        lb.set_selection_mode(gtk4::SelectionMode::None);
        lb
    });
    let right_lb: Rc<ListBox> = Rc::new({
        let lb = ListBox::new();
        lb.add_css_class("playlist");
        lb.set_selection_mode(gtk4::SelectionMode::None);
        lb
    });

    let left_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .child(&*left_lb)
        .build();
    let right_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .child(&*right_lb)
        .build();

    panels.append(&left_scroll);
    panels.append(&Separator::new(Orientation::Vertical));
    panels.append(&right_scroll);
    root_vbox.append(&panels);

    dlg.set_child(Some(&root_vbox));

    // ── Rebuild holder (allows rebuild closure to call itself) ───────────────
    let rebuild_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    let rebuild = {
        let fs = fs.clone();
        let left_ref = left_lb.clone();
        let right_ref = right_lb.clone();
        let sc = save_cfg.clone();
        let rh = rebuild_holder.clone();
        Rc::new(move || {
            // Clear both panels
            while let Some(c) = left_ref.first_child() { left_ref.remove(&c); }
            while let Some(c) = right_ref.first_child() { right_ref.remove(&c); }

            let entries = fs.borrow().clone();

            // Indices per column, in Vec order
            let col0: Vec<usize> = entries.iter().enumerate()
                .filter(|(_, e)| e.column == 0).map(|(i, _)| i).collect();
            let col1: Vec<usize> = entries.iter().enumerate()
                .filter(|(_, e)| e.column == 1).map(|(i, _)| i).collect();

            for (col_idx, col_globals) in [col0.as_slice(), col1.as_slice()].iter().enumerate() {
                let lb: &ListBox = if col_idx == 0 { &left_ref } else { &right_ref };
                let n = col_globals.len();

                for (col_pos, &g_idx) in col_globals.iter().enumerate() {
                    let entry = &entries[g_idx];
                    let rb_box = GtkBox::new(Orientation::Horizontal, 4);
                    rb_box.set_margin_top(2);
                    rb_box.set_margin_bottom(2);
                    rb_box.set_margin_start(4);
                    rb_box.set_margin_end(4);

                    // ▲ button
                    let up_btn = Button::with_label("▲");
                    up_btn.add_css_class("pl-btn");
                    up_btn.set_sensitive(col_pos > 0);
                    if col_pos > 0 {
                        let fs2 = fs.clone(); let sc2 = sc.clone(); let rh2 = rh.clone();
                        let g = g_idx; let prev = col_globals[col_pos - 1];
                        up_btn.connect_clicked(move |_| {
                            fs2.borrow_mut().swap(g, prev);
                            sc2();
                            if let Some(ref r) = *rh2.borrow() { r(); }
                        });
                    }

                    // ▼ button
                    let dn_btn = Button::with_label("▼");
                    dn_btn.add_css_class("pl-btn");
                    dn_btn.set_sensitive(col_pos + 1 < n);
                    if col_pos + 1 < n {
                        let fs2 = fs.clone(); let sc2 = sc.clone(); let rh2 = rh.clone();
                        let g = g_idx; let next = col_globals[col_pos + 1];
                        dn_btn.connect_clicked(move |_| {
                            fs2.borrow_mut().swap(g, next);
                            sc2();
                            if let Some(ref r) = *rh2.borrow() { r(); }
                        });
                    }

                    // Visibility checkbox
                    let cb = CheckButton::new();
                    cb.set_active(entry.visible);
                    {
                        let fs2 = fs.clone(); let sc2 = sc.clone(); let rh2 = rh.clone();
                        let g = g_idx;
                        cb.connect_toggled(move |btn| {
                            fs2.borrow_mut()[g].visible = btn.is_active();
                            sc2();
                            if let Some(ref r) = *rh2.borrow() { r(); }
                        });
                    }

                    // Field label (greyed when invisible)
                    let lbl = Label::builder()
                        .label(entry.label.as_str())
                        .halign(Align::Start)
                        .hexpand(true)
                        .build();
                    if !entry.visible {
                        lbl.add_css_class("status-label");
                    }

                    // → / ← column-switch button
                    let sw_lbl = if col_idx == 0 { "→" } else { "←" };
                    let sw_btn = Button::with_label(sw_lbl);
                    sw_btn.add_css_class("pl-btn");
                    {
                        let fs2 = fs.clone(); let sc2 = sc.clone(); let rh2 = rh.clone();
                        let g = g_idx;
                        let new_col: usize = if col_idx == 0 { 1 } else { 0 };
                        sw_btn.connect_clicked(move |_| {
                            // Move to end of the destination column
                            let insert_at = {
                                let e = fs2.borrow();
                                e.iter().enumerate().rev()
                                    .find(|(j, ent)| *j != g && ent.column == new_col)
                                    .map(|(j, _)| j + 1)
                                    .unwrap_or(e.len())
                            };
                            {
                                let mut e = fs2.borrow_mut();
                                e[g].column = new_col;
                                let entry = e.remove(g);
                                let adj = if insert_at > g { insert_at - 1 } else { insert_at };
                                let cap = e.len();
                                e.insert(adj.min(cap), entry);
                            }
                            sc2();
                            if let Some(ref r) = *rh2.borrow() { r(); }
                        });
                    }

                    // DragSource — provides global index as string
                    {
                        let drag_src = DragSource::new();
                        drag_src.set_actions(gtk4::gdk::DragAction::MOVE);
                        let g_str = g_idx.to_string();
                        drag_src.connect_prepare(move |_, _, _| {
                            Some(gdk::ContentProvider::for_value(&(&g_str).to_value()))
                        });
                        rb_box.add_controller(drag_src);
                    }

                    rb_box.append(&up_btn);
                    rb_box.append(&dn_btn);
                    rb_box.append(&cb);
                    rb_box.append(&lbl);
                    rb_box.append(&sw_btn);

                    let row = ListBoxRow::new();
                    row.set_widget_name(&g_idx.to_string());
                    row.set_child(Some(&rb_box));
                    lb.append(&row);
                }
            }
        })
    };

    *rebuild_holder.borrow_mut() = Some(rebuild.clone());

    // ── DropTargets — one per column panel; supports cross-column DnD ────────
    for (col_target, lb_rc) in [(0usize, left_lb.clone()), (1usize, right_lb.clone())] {
        let dt = DropTarget::new(glib::Type::STRING, gtk4::gdk::DragAction::MOVE);
        let lb_dt = lb_rc.clone();
        let fs_dt = fs.clone();
        let sc_dt = save_cfg.clone();
        let rh_dt = rebuild_holder.clone();
        dt.connect_drop(move |_, value, _x, y| {
            let src_global: usize = match value.get::<String>() {
                Ok(s) => match s.parse() { Ok(n) => n, Err(_) => return false },
                Err(_) => return false,
            };
            {
                let e = fs_dt.borrow();
                if src_global >= e.len() { return false; }
            }

            // Find the target row by y-coordinate in this ListBox
            let target_global: Option<usize> = lb_dt.row_at_y(y as i32)
                .and_then(|r| r.widget_name().to_string().parse::<usize>().ok());

            {
                let mut e = fs_dt.borrow_mut();
                e[src_global].column = col_target;
                let entry = e.remove(src_global);
                if let Some(tg) = target_global {
                    let adj = if tg > src_global { tg - 1 } else { tg };
                    let cap = e.len();
                    e.insert(adj.min(cap), entry);
                } else {
                    // Dropped below all rows — append to end of target column
                    let insert_at = e.iter().enumerate().rev()
                        .find(|(_, ent)| ent.column == col_target)
                        .map(|(j, _)| j + 1)
                        .unwrap_or_else(|| e.len());
                    let cap = e.len();
                    e.insert(insert_at.min(cap), entry);
                }
            }

            sc_dt();
            if let Some(ref r) = *rh_dt.borrow() { r(); }
            true
        });
        lb_rc.add_controller(dt);
    }

    rebuild();
    dlg.present();
}

// ---------------------------------------------------------------------------

#[derive(Clone)]
enum ColumnCustomizerMode {
    MediaLibrary,
    Id3Editor,
}

fn open_customize_columns_dialog(
    parent: Option<&gtk4::Window>,
    state: Rc<RefCell<AppState>>,
    title: &str,
    mode: ColumnCustomizerMode,
    on_toggle: Option<Rc<dyn Fn(String, bool)>>,
    on_close: Option<Rc<dyn Fn()>>,
) {
    use gtk4::prelude::*;

    // ID3 editor gets its own two-column customizer
    if matches!(mode, ColumnCustomizerMode::Id3Editor) {
        open_id3_field_customizer(parent, state, on_close);
        return;
    }

    let dlg = gtk4::Window::new();
    dlg.set_title(Some(title));
    dlg.set_default_size(400, 480);
    dlg.set_resizable(true);
    if let Some(p) = parent {
        dlg.set_transient_for(Some(p));
    }

    let main_vbox = GtkBox::new(Orientation::Vertical, 8);
    main_vbox.set_margin_top(12);
    main_vbox.set_margin_bottom(12);
    main_vbox.set_margin_start(12);
    main_vbox.set_margin_end(12);

    // ── Build ordered entry list ─────────────────────────────────────────────
    #[derive(Clone)]
    struct ColEntry {
        id: String,
        header: String,
        visible: bool,
    }

    let saved_order = state.borrow().config.media_library.ml_file_col_order.clone();
    let visible_vec: Vec<String> = state.borrow().config.media_library.visible_columns.clone();
    let visible_set: std::collections::HashSet<String> = visible_vec.iter().cloned().collect();

    let mut init_entries: Vec<ColEntry> = Vec::new();
    // 1. Visible columns in saved order
    for id in &saved_order {
        if visible_set.contains(id) {
            if let Some(col) = ALL_COLUMNS.iter().find(|c| c.id == id.as_str()) {
                init_entries.push(ColEntry { id: id.clone(), header: col.header.to_string(), visible: true });
            }
        }
    }
    // 2. Visible columns not in saved order (newly enabled)
    for id in &visible_vec {
        if !saved_order.contains(id) {
            if let Some(col) = ALL_COLUMNS.iter().find(|c| c.id == id.as_str()) {
                init_entries.push(ColEntry { id: id.clone(), header: col.header.to_string(), visible: true });
            }
        }
    }
    // 3. Hidden columns (no order controls needed)
    for col in ALL_COLUMNS.iter() {
        if !visible_set.contains(col.id) {
            init_entries.push(ColEntry { id: col.id.to_string(), header: col.header.to_string(), visible: false });
        }
    }

    let entries: Rc<RefCell<Vec<ColEntry>>> = Rc::new(RefCell::new(init_entries));

    // Persist entries → config on every change
    let save_cfg: Rc<dyn Fn()> = {
        let entries = entries.clone();
        let st = state.clone();
        Rc::new(move || {
            let es = entries.borrow();
            let order: Vec<String> = es.iter().filter(|e| e.visible).map(|e| e.id.clone()).collect();
            let mut s = st.borrow_mut();
            s.config.media_library.visible_columns = order.clone();
            s.config.media_library.ml_file_col_order = order;
            let _ = s.config.save();
        })
    };

    let hdr = Label::builder()
        .label("Use ▲ ▼ to reorder visible columns:")
        .halign(Align::Start)
        .build();
    main_vbox.append(&hdr);

    let scrolled = ScrolledWindow::new();
    scrolled.set_hexpand(true);
    scrolled.set_vexpand(true);
    scrolled.set_has_frame(true);

    let list_lb = ListBox::new();
    list_lb.add_css_class("playlist");
    list_lb.set_selection_mode(gtk4::SelectionMode::None);
    scrolled.set_child(Some(&list_lb));
    main_vbox.append(&scrolled);

    // rebuild_holder allows the rebuild closure to call itself recursively
    let rebuild_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    let rebuild: Rc<dyn Fn()> = {
        let entries = entries.clone();
        let lb_ref = list_lb.clone();
        let sc = save_cfg.clone();
        let rh = rebuild_holder.clone();
        let on_toggle_rb = on_toggle.clone();
        let scrolled_rb = scrolled.clone();
        Rc::new(move || {
            // Preserve scroll position: clearing and re-adding rows resets
            // the ScrolledWindow's vadjustment to 0, which yanks the user
            // back to the top on every toggle. Snapshot → rebuild → restore.
            let prev_scroll = scrolled_rb.vadjustment().value();
            while let Some(c) = lb_ref.first_child() { lb_ref.remove(&c); }

            let es = entries.borrow().clone();

            for (i, entry) in es.iter().enumerate() {
                let row_box = GtkBox::new(Orientation::Horizontal, 4);
                row_box.set_margin_top(2);
                row_box.set_margin_bottom(2);
                row_box.set_margin_start(4);
                row_box.set_margin_end(4);

                if entry.visible {
                    // ▲ button — enabled when a visible column precedes this one
                    let up_btn = Button::with_label("▲");
                    up_btn.add_css_class("pl-btn");
                    let prev_idx = es[..i].iter().rposition(|e| e.visible);
                    up_btn.set_sensitive(prev_idx.is_some());
                    if let Some(prev) = prev_idx {
                        let entries2 = entries.clone();
                        let sc2 = sc.clone();
                        let rh2 = rh.clone();
                        up_btn.connect_clicked(move |_| {
                            entries2.borrow_mut().swap(i, prev);
                            sc2();
                            if let Some(ref r) = *rh2.borrow() { r(); }
                        });
                    }
                    row_box.append(&up_btn);

                    // ▼ button — enabled when a visible column follows this one
                    let dn_btn = Button::with_label("▼");
                    dn_btn.add_css_class("pl-btn");
                    let next_rel = es[i + 1..].iter().position(|e| e.visible);
                    dn_btn.set_sensitive(next_rel.is_some());
                    if let Some(rel) = next_rel {
                        let next = i + 1 + rel;
                        let entries2 = entries.clone();
                        let sc2 = sc.clone();
                        let rh2 = rh.clone();
                        dn_btn.connect_clicked(move |_| {
                            entries2.borrow_mut().swap(i, next);
                            sc2();
                            if let Some(ref r) = *rh2.borrow() { r(); }
                        });
                    }
                    row_box.append(&dn_btn);
                } else {
                    // Spacer to align labels with visible rows
                    let spacer = GtkBox::new(Orientation::Horizontal, 4);
                    spacer.set_width_request(64);
                    row_box.append(&spacer);
                }

                // Visibility checkbox
                let cb = CheckButton::new();
                cb.set_active(entry.visible);
                {
                    let entries2 = entries.clone();
                    let sc2 = sc.clone();
                    let rh2 = rh.clone();
                    let on_tgl = on_toggle_rb.clone();
                    cb.connect_toggled(move |btn| {
                        let visible = btn.is_active();
                        let id = entries2.borrow()[i].id.clone();
                        entries2.borrow_mut()[i].visible = visible;
                        sc2();
                        if let Some(ref cb) = on_tgl { cb(id, visible); }
                        if let Some(ref r) = *rh2.borrow() { r(); }
                    });
                }
                row_box.append(&cb);

                let lbl = Label::builder()
                    .label(entry.header.as_str())
                    .halign(Align::Start)
                    .xalign(0.0)
                    .hexpand(true)
                    .build();
                if !entry.visible { lbl.add_css_class("status-label"); }
                row_box.append(&lbl);

                let row = ListBoxRow::new();
                row.set_child(Some(&row_box));
                lb_ref.append(&row);
            }
            // Restore scroll position after the new rows lay out. idle_add
            // defers until GTK finishes sizing the listbox so the adjustment
            // upper bound reflects the post-rebuild content height.
            let adj = scrolled_rb.vadjustment();
            glib::idle_add_local_once(move || {
                let upper = adj.upper();
                let page  = adj.page_size();
                let max   = (upper - page).max(0.0);
                adj.set_value(prev_scroll.min(max));
            });
        })
    };

    *rebuild_holder.borrow_mut() = Some(rebuild.clone());
    rebuild();

    // ── Buttons ──────────────────────────────────────────────────────────────
    let btn_row = GtkBox::new(Orientation::Horizontal, 8);

    let btn_reset = Button::with_label("Reset Defaults");
    {
        let entries2 = entries.clone();
        let sc2 = save_cfg.clone();
        let rb2 = rebuild.clone();
        let on_tgl = on_toggle.clone();
        let st2 = state.clone();
        btn_reset.connect_clicked(move |_| {
            let defaults = crate::config::MediaLibraryConfig::default_visible_columns();
            let default_set: std::collections::HashSet<String> = defaults.iter().cloned().collect();
            {
                let mut es = entries2.borrow_mut();
                for e in es.iter_mut() { e.visible = default_set.contains(&e.id); }
                es.sort_by_key(|e| {
                    if e.visible {
                        defaults.iter().position(|d| d == &e.id).unwrap_or(usize::MAX)
                    } else {
                        usize::MAX
                    }
                });
            }
            if let Some(ref cb) = on_tgl {
                for e in entries2.borrow().iter() { cb(e.id.clone(), e.visible); }
            }
            {
                let mut s = st2.borrow_mut();
                s.config.media_library.visible_columns = defaults.clone();
                s.config.media_library.ml_file_col_order = defaults;
                let _ = s.config.save();
            }
            sc2();
            rb2();
        });
    }
    btn_row.append(&btn_reset);

    let spring = GtkBox::new(Orientation::Horizontal, 0);
    spring.set_hexpand(true);
    btn_row.append(&spring);

    let btn_close = Button::with_label("Close");
    {
        let dlg_wk = dlg.downgrade();
        let oc = on_close.clone();
        btn_close.connect_clicked(move |_| {
            if let Some(ref cb) = oc { cb(); }
            if let Some(w) = dlg_wk.upgrade() { w.close(); }
        });
    }
    btn_row.append(&btn_close);

    main_vbox.append(&btn_row);
    dlg.set_child(Some(&main_vbox));

    dlg.connect_close_request(move |_| {
        if let Some(ref cb) = on_close { cb(); }
        glib::Propagation::Proceed
    });

    dlg.present();
}

/// Open the ID3 tag editor window for `path`.
///
/// Pre-populates all 12 default fields from the file's existing tag and lets
/// the user edit them in a two-column grid.  Ctrl+S or the Save button writes
/// the tag back to disk and reloads the in-memory track so the playlist row
/// immediately shows the updated title/artist.  Esc or Cancel discards changes.
///
/// A "Customize…" button opens a secondary window ([`open_id3_extra_window`])
/// for any additional ID3v2 frames present in the file.
///
/// This is a singleton: if an editor is already open, it will be updated
/// with the new file instead of opening a second window.
fn open_id3_editor_window(
    _parent: Option<&impl gtk4::prelude::IsA<gtk4::Window>>,
    path: std::path::PathBuf,
    state: Rc<RefCell<AppState>>,
    rebuild_cb: Rc<dyn Fn()>,
    initial_values: Option<std::collections::HashMap<String, String>>,
) {
    use crate::id3_editor::{read_tag_fields, write_tag_fields, TagFields};
    use gtk4::prelude::*;

    // If an editor is already open, close it and build a fresh one for the new
    // file — the same filename can live at a different path, so the window must
    // reflect the exact file just requested rather than being reused as-is.
    // Take in its own statement so the borrow is released before `close()`,
    // which synchronously fires the close-request handler (it borrows too).
    let existing_editor = state.borrow_mut().id3_editor_window.take();
    if let Some(existing_win) = existing_editor {
        existing_win.close();
    }

    let fields = read_tag_fields(&path);
    let fname = gtk_safe(path.file_name().and_then(|n| n.to_str()).unwrap_or("?"));
    let path_str = path.to_string_lossy().into_owned();

    let track_meta = state
        .borrow()
        .media_lib
        .as_ref()
        .and_then(|ml| ml.track_by_path(&path_str).ok());

    let ro = crate::media_library::read_only_track_fields(&path, track_meta.as_ref());

    let win = gtk4::Window::builder()
        .title(format!("ID3 Tag Editor — {fname}"))
        .default_width(600)
        .default_height(480)
        .build();

    let state_for_close = state.clone();
    win.connect_close_request(move |w| {
        // Only clear the handle if it still points at *this* window — a newer
        // editor may have replaced it (close fires as the old one is swapped).
        let mut s = state_for_close.borrow_mut();
        if s.id3_editor_window.as_ref() == Some(w) {
            s.id3_editor_window = None;
        }
        glib::Propagation::Proceed
    });
    state.borrow_mut().id3_editor_window = Some(win.clone());

    // ── Get visible columns from config (preserve order for left/right split) ──
    let visible_ids: Vec<String> = state
        .borrow()
        .config
        .media_library
        .id3_visible_columns
        .clone();

    // ── Collect entry widgets for the save handler ───────────────────────────
    // Stores (field_id, Entry) for editable fields.
    let entries: Rc<RefCell<std::collections::HashMap<String, Entry>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));

    // ── 2-column field grid ───────────────────────────────────────────────
    let grid = Grid::new();
    grid.set_margin_top(12);
    grid.set_margin_bottom(8);
    grid.set_margin_start(12);
    grid.set_margin_end(12);
    grid.set_row_spacing(6);
    grid.set_column_spacing(8);
    grid.set_hexpand(true);

    // Get column positions from config
    let column_positions: std::collections::HashMap<String, String> = state
        .borrow()
        .config
        .media_library
        .id3_column_position
        .clone();

    // Get editable columns in visible order
    let editable_ids: std::collections::HashSet<&str> = ALL_COLUMNS
        .iter()
        .filter(|c| c.id3_editable)
        .map(|c| c.id)
        .collect();

    let visible_editable: Vec<&str> = visible_ids
        .iter()
        .filter(|id| editable_ids.contains(id.as_str()))
        .map(|s| s.as_str())
        .collect();

    // Separate into left/right based on column position config
    let mut left_ids: Vec<&str> = Vec::new();
    let mut right_ids: Vec<&str> = Vec::new();
    for id in &visible_editable {
        let pos = column_positions
            .get(*id)
            .map(|s| s.as_str())
            .unwrap_or("left");
        if pos == "right" {
            right_ids.push(*id);
        } else {
            left_ids.push(*id);
        }
    }

    // Build left column (cols 0-1)
    let mut left_entries: Vec<(String, gtk4::Entry)> = Vec::new();
    for (row, id) in left_ids.iter().enumerate() {
        let col_def = ALL_COLUMNS.iter().find(|c| c.id == *id).unwrap();
        let lbl = Label::new(Some(col_def.header));
        lbl.set_xalign(1.0);
        lbl.set_margin_end(4);
        grid.attach(&lbl, 0, row as i32, 1, 1);

        let value = if let Some(ref vals) = initial_values {
            vals.get(*id)
                .cloned()
                .unwrap_or_else(|| get_id3_field_value(&fields, &track_meta, id))
        } else {
            get_id3_field_value(&fields, &track_meta, id)
        };
        if *id == "genre" {
            let (combo, entry) = make_genre_combo(&value);
            combo.set_hexpand(true);
            grid.attach(&combo, 1, row as i32, 1, 1);
            // Register the hidden carrier entry so Save picks the genre up;
            // without it the save handler writes an empty genre.
            left_entries.push((id.to_string(), entry));
        } else {
            let entry = Entry::new();
            entry.set_hexpand(true);
            entry.set_text(&gtk_safe(&value));
            grid.attach(&entry, 1, row as i32, 1, 1);
            left_entries.push((id.to_string(), entry));
        }
    }

    // Build right column (cols 2-3)
    let mut right_entries: Vec<(String, gtk4::Entry)> = Vec::new();
    for (row, id) in right_ids.iter().enumerate() {
        let col_def = ALL_COLUMNS.iter().find(|c| c.id == *id).unwrap();
        let lbl = Label::new(Some(col_def.header));
        lbl.set_xalign(1.0);
        lbl.set_margin_end(4);
        grid.attach(&lbl, 2, row as i32, 1, 1);

        let value = if let Some(ref vals) = initial_values {
            vals.get(*id)
                .cloned()
                .unwrap_or_else(|| get_id3_field_value(&fields, &track_meta, id))
        } else {
            get_id3_field_value(&fields, &track_meta, id)
        };
        if *id == "genre" {
            let (combo, entry) = make_genre_combo(&value);
            combo.set_hexpand(true);
            grid.attach(&combo, 3, row as i32, 1, 1);
            right_entries.push((id.to_string(), entry));
        } else {
            let entry = Entry::new();
            entry.set_hexpand(true);
            entry.set_text(&gtk_safe(&value));
            grid.attach(&entry, 3, row as i32, 1, 1);
            right_entries.push((id.to_string(), entry));
        }
    }

    // Insert all entries into the HashMap in one operation
    for (id, entry) in left_entries.into_iter().chain(right_entries) {
        entries.borrow_mut().insert(id, entry);
    }

    // ── Check if file is read-only ───────────────────────────────────────────
    let is_read_only = crate::media_library::is_read_only(&path);

    // ── Artwork section ─────────────────────────────────────────────────────
    let artwork_vbox = GtkBox::new(Orientation::Vertical, 4);
    artwork_vbox.set_margin_start(12);
    artwork_vbox.set_margin_end(12);
    artwork_vbox.set_margin_top(8);
    artwork_vbox.set_margin_bottom(8);

    let art_path_entry = Entry::new();
    art_path_entry.set_text(&gtk_safe(&ro.artwork_path));
    art_path_entry.set_hexpand(true);

    let btn_browse = Button::with_label("Browse…");
    let btn_view = Button::with_label("View");
    btn_view.set_sensitive(!ro.artwork_path.is_empty());

    let art_entry_clone = art_path_entry.clone();
    let btn_view_for_browse = btn_view.clone();
    btn_browse.connect_clicked(move |b| {
        let dialog = gtk4::FileDialog::new();
        dialog.set_title("Select Artwork");
        let filters = gtk4::FileFilter::new();
        filters.set_name(Some("Images"));
        filters.add_mime_type("image/png");
        filters.add_mime_type("image/jpeg");
        filters.add_mime_type("image/jpg");
        filters.add_mime_type("image/gif");
        filters.add_mime_type("image/webp");
        dialog.set_default_filter(Some(&filters));
        let entry_clone = art_entry_clone.clone();
        let btn_view_clone = btn_view_for_browse.clone();
        // Parent to the editor window (the button's toplevel) so the chooser
        // has a transient parent instead of a throwaway, unmapped window.
        let parent = b.root().and_downcast::<gtk4::Window>();
        dialog.open(
            parent.as_ref(),
            None::<&gtk4::gio::Cancellable>,
            move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let path_str = path.to_string_lossy().into_owned();
                        entry_clone.set_text(&path_str);
                        btn_view_clone.set_sensitive(true);
                    }
                }
            },
        );
    });

    let art_path_clone = art_path_entry.clone();
    btn_view.connect_clicked(move |_| {
        let p = art_path_clone.text();
        if !p.is_empty() {
            open_image_viewer(&p);
        }
    });

    let art_path_row = GtkBox::new(Orientation::Horizontal, 8);
    art_path_row.append(&Label::new(Some("Artwork:")));
    art_path_row.append(&art_path_entry);
    art_path_row.append(&btn_browse);
    art_path_row.append(&btn_view);
    artwork_vbox.append(&art_path_row);

    // Track art_path_entry in the entries HashMap
    entries
        .borrow_mut()
        .insert("artwork_path".to_string(), art_path_entry);

    // Show 128x128 thumbnail preview
    if visible_ids.contains(&"artwork_path".to_string()) && !ro.artwork_path.is_empty() {
        let art_picture = gtk4::Picture::new();
        art_picture.set_width_request(128);
        art_picture.set_height_request(128);
        art_picture.set_can_shrink(true);
        art_picture.set_content_fit(gtk4::ContentFit::Contain);
        art_picture.set_filename(Some(&ro.artwork_path));

        let art_clone = ro.artwork_path.clone();
        let click = gtk4::GestureClick::new();
        click.connect_pressed(move |_, _, _, _| {
            open_image_viewer(&art_clone);
        });
        art_picture.add_controller(click);
        artwork_vbox.append(&art_picture);
    }

    // ── Status label ─────────────────────────────────────────────────────────
    let status_lbl = Label::builder()
        .label("")
        .halign(Align::Start)
        .css_classes(["status-label"])
        .build();
    status_lbl.set_margin_start(12);
    status_lbl.set_margin_bottom(4);

    // ── Read-only notice (only shown for read-only files) ────────────────────
    let read_only_notice = Label::builder()
        .label("🔒 This file is read only")
        .halign(Align::Center)
        .build();
    read_only_notice.set_margin_start(12);
    read_only_notice.set_margin_end(12);
    read_only_notice.set_margin_top(8);
    read_only_notice.set_margin_bottom(4);
    read_only_notice.set_visible(is_read_only);

    // Disable all entry widgets for read-only files
    if is_read_only {
        for (_, entry) in entries.borrow().iter() {
            entry.set_sensitive(false);
        }
    }

    // ── Button row ───────────────────────────────────────────────────────────
    let btn_row = GtkBox::new(Orientation::Horizontal, 8);
    btn_row.set_margin_top(4);
    btn_row.set_margin_start(12);
    btn_row.set_margin_end(12);
    btn_row.set_margin_bottom(8);

    let btn_customize = Button::with_label("Customize…");
    let btn_cancel = Button::with_label("Cancel");
    let btn_save = Button::with_label("Save");
    btn_save.add_css_class("suggested-action");
    btn_save.set_visible(!is_read_only);

    let spring = GtkBox::new(Orientation::Horizontal, 0);
    spring.set_hexpand(true);
    btn_row.append(&btn_customize);
    btn_row.append(&spring);
    btn_row.append(&btn_cancel);
    btn_row.append(&btn_save);

    // ── Main layout ──────────────────────────────────────────────────────────
    // Full path + filename header — a read-only Entry so a long path that
    // doesn't fit scrolls horizontally (cursor/drag) without a scrollbar, and
    // stays selectable/copyable, confirming the file's exact source location.
    let path_entry = Entry::new();
    path_entry.set_text(&gtk_safe(&path_str));
    path_entry.set_editable(false);
    path_entry.set_can_focus(true);
    path_entry.set_hexpand(true);
    path_entry.set_margin_top(10);
    path_entry.set_margin_bottom(10);
    path_entry.set_margin_start(12);
    path_entry.set_margin_end(12);
    path_entry.set_tooltip_text(Some(&path_str));
    // Show the end (filename) first rather than the start of the path.
    path_entry.set_position(-1);

    let vbox = GtkBox::new(Orientation::Vertical, 0);
    vbox.append(&path_entry);
    vbox.append(&Separator::new(Orientation::Horizontal));
    vbox.append(&grid);
    vbox.append(&artwork_vbox);
    vbox.append(&Separator::new(Orientation::Horizontal));
    vbox.append(&status_lbl);
    vbox.append(&read_only_notice);
    vbox.append(&btn_row);
    win.set_child(Some(&vbox));

    // ── Collect fields → TagFields and write to disk ─────────────────────────
    let do_save = {
        let path = path.clone();
        let state_s = state.clone();
        let rebuild_s = rebuild_cb.clone();
        let status_s = status_lbl.clone();
        let win_wk = win.downgrade();
        let entries_r = entries.clone();

        move || {
            let entries = entries_r.borrow();
            let new_fields = TagFields {
                title: entries
                    .get("title")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                artist: entries
                    .get("artist")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                album: entries
                    .get("album")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                album_artist: entries
                    .get("album_artist")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                genre: entries
                    .get("genre")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                year: entries
                    .get("year")
                    .map(|e| sanitize_id3_numeric(&e.text()))
                    .unwrap_or_default(),
                track_number: entries
                    .get("track_num")
                    .map(|e| sanitize_id3_numeric(&e.text()))
                    .unwrap_or_default(),
                track_total: entries
                    .get("track_total")
                    .map(|e| sanitize_id3_numeric(&e.text()))
                    .unwrap_or_default(),
                disc_number: entries
                    .get("disc_num")
                    .map(|e| sanitize_id3_numeric(&e.text()))
                    .unwrap_or_default(),
                disc_total: entries
                    .get("disc_total")
                    .map(|e| sanitize_id3_numeric(&e.text()))
                    .unwrap_or_default(),
                bpm: entries
                    .get("bpm")
                    .map(|e| sanitize_id3_numeric(&e.text()))
                    .unwrap_or_default(),
                comment: entries
                    .get("comment")
                    .map(|e| sanitize_id3_text(&e.text()))
                    .unwrap_or_default(),
                artwork_path: entries
                    .get("artwork_path")
                    .map(|e| e.text().to_string())
                    .unwrap_or_default(),
            };

            match write_tag_fields(&path, &new_fields) {
                Ok(()) => {
                    for track in &mut state_s.borrow_mut().playlist.tracks {
                        if track.path == path {
                            if let Ok(fresh) = crate::model::Track::from_path(&path) {
                                track.title = fresh.title;
                                track.artist = fresh.artist;
                                track.album_artist = fresh.album_artist;
                                track.album = fresh.album;
                            }
                            break;
                        }
                    }

                    // If the saved track is currently playing, update the marquee
                    // immediately so the new artist/title is reflected without
                    // requiring a track change.
                    let is_current = state_s
                        .borrow()
                        .playlist
                        .current()
                        .map(|t| t.path == path)
                        .unwrap_or(false);
                    if is_current {
                        let display = state_s
                            .borrow()
                            .playlist
                            .current()
                            .map(|t| t.display_name())
                            .unwrap_or_default();
                        if let Some(ref cb) = state_s.borrow().set_track_callback.clone() {
                            cb(&display);
                        }
                    }

                    // Update the Media Library DB record and artwork cache for
                    // the edited file, then refresh the ML window if it is open.
                    if let Some(lib) = state_s.borrow().media_lib.as_ref() {
                        let path_str = path.to_string_lossy().into_owned();
                        let _ = lib.rescan_track(&path_str);
                        if let Ok(lib_track) = lib.track_by_path(&path_str) {
                            let _ = lib.refresh_artwork(lib_track.id, &path_str);
                        }
                    }

                    let rebuild = rebuild_s.clone();
                    let rebuild_ml = state_s.borrow().rebuild_ml_callback.clone();
                    if let Some(w) = win_wk.upgrade() {
                        w.close();
                    }
                    glib::idle_add_local(move || {
                        rebuild();
                        if let Some(ref cb) = rebuild_ml {
                            cb();
                        }
                        glib::ControlFlow::Break
                    });
                }
                Err(e) => {
                    status_s.set_text(&format!("Save error: {e}"));
                }
            }
        }
    };

    // ── Cancel button ────────────────────────────────────────────────────────
    btn_cancel.connect_clicked({
        let win_wk = win.downgrade();
        move |_| {
            if let Some(w) = win_wk.upgrade() {
                w.close();
            }
        }
    });

    // ── Save button ──────────────────────────────────────────────────────────
    btn_save.connect_clicked({
        let save = do_save.clone();
        move |_| {
            save();
        }
    });

    // ── Customize button — open column customization dialog ──────────────────
    btn_customize.connect_clicked({
        let state_outer = state.clone();
        let win_wk_outer = win.downgrade();
        let path_outer = path.clone();
        let rebuild_outer = rebuild_cb.clone();
        let entries_outer = entries.clone();
        move |_| {
            let state_inner = state_outer.clone();
            let win_wk = win_wk_outer.clone();
            let path_clone = path_outer.clone();
            let rebuild_clone = rebuild_outer.clone();
            let entries_clone = entries_outer.clone();
            let current_values: std::collections::HashMap<String, String> = entries_clone
                .borrow()
                .iter()
                .map(|(k, v)| (k.clone(), v.text().to_string()))
                .collect();
            open_customize_columns_dialog(
                win_wk.upgrade().as_ref(),
                state_inner.clone(),
                "Customize ID3 Fields",
                ColumnCustomizerMode::Id3Editor,
                None::<Rc<dyn Fn(String, bool)>>,
                Some(Rc::new(move || {
                    if let Some(w) = win_wk.upgrade() {
                        w.close();
                    }
                    open_id3_editor_window(
                        None::<&gtk4::Window>,
                        path_clone.clone(),
                        state_inner.clone(),
                        rebuild_clone.clone(),
                        Some(current_values.clone()),
                    );
                }) as Rc<dyn Fn()>),
            );
        }
    });

    // ── Keyboard: Ctrl+S saves, Esc cancels ──────────────────────────────────
    {
        let key_ctrl = gtk4::EventControllerKey::new();
        let save_fn = do_save.clone();
        let win_wk2 = win.downgrade();
        key_ctrl.connect_key_pressed(move |_, key, _, modifiers| match key {
            gdk::Key::Escape => {
                if let Some(w) = win_wk2.upgrade() {
                    w.close();
                }
                glib::Propagation::Stop
            }
            gdk::Key::s | gdk::Key::S if modifiers.contains(gdk::ModifierType::CONTROL_MASK) => {
                save_fn();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        win.add_controller(key_ctrl);
    }

    win.present();
}

// ---------------------------------------------------------------------------
// Settings window
// ---------------------------------------------------------------------------
// Settings window
// ---------------------------------------------------------------------------

/// Open the Settings window with tabs: Appearance, Behavior, Visualizer,
/// Filetypes, Media Library.
///
/// Changes made in any tab are written back to `state.config` immediately
/// when a control's value changes.  Pressing "Close" (or closing the
/// window) persists the config to disk.  `initial_tab` selects the starting
/// tab page (0-indexed), or opens at the default page if `None`.
/// `css_provider` is updated live when the user switches skins in the
/// Appearance tab.
#[allow(deprecated)]
/// Modal asking for the gnudb/CDDB email, prefilled from config. On Save it
/// stores + persists the address and runs `on_done` (e.g. retry the lookup);
/// Cancel just closes. Used when a disc action needs an email that's unset.
fn prompt_gnudb_email(
    parent: Option<&gtk4::Window>,
    state: Rc<RefCell<AppState>>,
    on_done: Rc<dyn Fn()>,
) {
    let dialog = gtk4::Window::builder()
        .title("gnudb email")
        .modal(true)
        .default_width(380)
        .build();
    if let Some(p) = parent {
        dialog.set_transient_for(Some(p));
    }
    let vbox = GtkBox::new(Orientation::Vertical, 8);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);
    let info = Label::builder()
        .label(
            "gnudb needs an email address for its lookup/submission handshake. \
             It's stored locally and used only to talk to gnudb.",
        )
        .wrap(true)
        .halign(Align::Start)
        .xalign(0.0)
        .build();
    let entry = Entry::new();
    entry.set_placeholder_text(Some("you@example.com"));
    entry.set_text(&gtk_safe(&state.borrow().config.disc.gnudb_email));
    let btns = GtkBox::new(Orientation::Horizontal, 6);
    btns.set_halign(Align::End);
    let cancel = Button::with_label("Cancel");
    let save = Button::with_label("Save");
    save.add_css_class("suggested-action");
    btns.append(&cancel);
    btns.append(&save);
    vbox.append(&info);
    vbox.append(&entry);
    vbox.append(&btns);
    dialog.set_child(Some(&vbox));
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    {
        let save = save.clone();
        entry.connect_activate(move |_| {
            save.activate();
        });
    }
    let d = dialog.clone();
    save.connect_clicked(move |_| {
        {
            let mut s = state.borrow_mut();
            s.config.disc.gnudb_email = entry.text().to_string();
            let _ = s.config.save();
        }
        on_done();
        d.close();
    });
    dialog.present();
}

fn open_settings_window(
    parent: Option<&gtk4::Window>,
    state: Rc<RefCell<AppState>>,
    initial_tab: Option<u32>,
    css_provider: Rc<gtk4::CssProvider>,
    text_rgba: Rc<RefCell<gdk::RGBA>>,
    accent_rgba: Rc<RefCell<Option<gdk::RGBA>>>,
    rebuild_playlist: Rc<dyn Fn()>,
) {
    let win = gtk4::Window::new();
    win.set_title(Some("Settings — SparkAmp"));
    win.set_default_size(480, 340);
    win.set_resizable(false);
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }

    let notebook = Notebook::new();
    notebook.set_margin_top(8);
    notebook.set_margin_bottom(8);
    notebook.set_margin_start(8);
    notebook.set_margin_end(8);

    // ── Tab 0: Appearance ─────────────────────────────────────────────────
    {
        use gtk4::{Box as GtkBox, Button, Label, ListBox, ListBoxRow, Orientation,
                   PolicyType, ScrolledWindow, SelectionMode, FileDialog, FileFilter};

        let root = GtkBox::new(Orientation::Vertical, 10);
        root.set_margin_top(16);
        root.set_margin_bottom(16);
        root.set_margin_start(16);
        root.set_margin_end(16);

        // Header
        let header = Label::new(Some("Skin"));
        header.set_halign(Align::Start);
        header.add_css_class("heading");
        root.append(&header);

        // Scrollable list of skins
        let listbox = ListBox::new();
        listbox.set_selection_mode(SelectionMode::Single);
        listbox.add_css_class("rich-list");

        let scrolled = ScrolledWindow::new();
        scrolled.set_policy(PolicyType::Never, PolicyType::Automatic);
        scrolled.set_min_content_height(200);
        scrolled.set_child(Some(&listbox));
        root.append(&scrolled);

        // Suppress the row_selected handler while we programmatically
        // re-select the active row after rebuild. GtkNotebook tab switches
        // can also fire spurious row_selected events on re-show; we only
        // want user clicks to apply a skin.
        let suppress_sel: Rc<Cell<bool>> = Rc::new(Cell::new(false));

        // Populate rows
        let rebuild_list = {
            let listbox = listbox.clone();
            let state_rc = state.clone();
            let suppress = suppress_sel.clone();
            Rc::new(move || {
                suppress.set(true);
                while let Some(row) = listbox.first_child() {
                    listbox.remove(&row);
                }
                let hidden = state_rc.borrow().config.appearance.hidden_skins.clone();
                let entries = crate::skin::list_skins(&hidden);
                let active = state_rc.borrow().config.appearance.active_skin.clone();
                let mut active_row: Option<ListBoxRow> = None;

                for entry in entries {
                    let row = ListBoxRow::new();
                    let hbox = GtkBox::new(Orientation::Horizontal, 8);
                    hbox.set_margin_top(4);
                    hbox.set_margin_bottom(4);
                    hbox.set_margin_start(8);
                    hbox.set_margin_end(8);

                    let name_lbl = Label::new(Some(&entry.display_name));
                    name_lbl.set_halign(Align::Start);
                    name_lbl.set_hexpand(true);
                    hbox.append(&name_lbl);

                    if entry.is_builtin {
                        let tag = Label::new(Some("(built-in)"));
                        tag.add_css_class("dim-label");
                        hbox.append(&tag);
                    }

                    if entry.name == active {
                        let mark = Label::new(Some("● Active"));
                        mark.add_css_class("dim-label");
                        hbox.append(&mark);
                    }

                    row.set_child(Some(&hbox));
                    row.set_widget_name(&entry.name);
                    listbox.append(&row);
                    if entry.name == active {
                        active_row = Some(row);
                    }
                }
                if let Some(r) = active_row {
                    listbox.select_row(Some(&r));
                }
                suppress.set(false);
            })
        };
        rebuild_list();

        // Selecting a row applies the skin live.
        {
            let state_rc = state.clone();
            let provider = css_provider.clone();
            let text_rgba = text_rgba.clone();
            let accent_rgba = accent_rgba.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let rebuild = rebuild_list.clone();
            let suppress = suppress_sel.clone();
            listbox.connect_row_selected(move |_, row| {
                if suppress.get() { return; }
                let Some(row) = row else { return };
                let name = row.widget_name().to_string();
                if name.is_empty() { return; }
                // User-clicked a row while the skin was already active
                // (e.g., re-click to re-apply) — nothing to do.
                if state_rc.borrow().config.appearance.active_skin == name {
                    return;
                }
                let Some(skin) = crate::skin::load_skin(&name) else { return };
                let css = crate::skin::render_gtk_css(&skin.vars);
                provider.load_from_data(&css);
                if let Some(gtk_settings) = gtk4::Settings::default() {
                    gtk_settings.set_gtk_application_prefer_dark_theme(
                        skin.vars.background.luminance() < 0.5);
                }
                *text_rgba.borrow_mut() = gdk::RGBA::new(
                    skin.vars.text_color.r as f32 / 255.0,
                    skin.vars.text_color.g as f32 / 255.0,
                    skin.vars.text_color.b as f32 / 255.0,
                    1.0,
                );
                // Playlist TreeView stores fg color per-row via RGBA column;
                // update the shared accent from the new skin's highlight so
                // the playing row re-renders in the new skin's accent rather
                // than the color captured at startup.
                *accent_rgba.borrow_mut() = Some(gdk::RGBA::new(
                    skin.vars.highlight.r as f32 / 255.0,
                    skin.vars.highlight.g as f32 / 255.0,
                    skin.vars.highlight.b as f32 / 255.0,
                    1.0,
                ));
                state_rc.borrow_mut().config.appearance.active_skin = name;
                // Refresh all playlist rows so the new text / accent colors
                // propagate — CSS alone doesn't reach the deprecated cell
                // renderer's foreground-rgba column.
                rebuild_pl();
                rebuild();
            });
        }

        // Row of action buttons
        let btn_row = GtkBox::new(Orientation::Horizontal, 8);
        let btn_add = Button::with_label("Add skin…");
        let btn_remove = Button::with_label("Remove");
        let btn_download = Button::with_label("Download skin…");
        btn_row.append(&btn_add);
        btn_row.append(&btn_remove);
        btn_row.append(&btn_download);
        root.append(&btn_row);

        // Wire Add
        {
            let state_rc = state.clone();
            let rebuild = rebuild_list.clone();
            let listbox = listbox.clone();
            let win_ref = win.clone();
            btn_add.connect_clicked(move |_| {
                let dialog = FileDialog::new();
                dialog.set_title("Add Sparkamp skin");
                let filter = FileFilter::new();
                filter.add_suffix("css");
                filter.set_name(Some("Sparkamp skin (*.css)"));
                let filters = gio::ListStore::new::<FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));

                let state_rc = state_rc.clone();
                let rebuild = rebuild.clone();
                let listbox = listbox.clone();
                let win_alert = win_ref.clone();
                dialog.open(Some(&win_ref), gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    match crate::skin::add_user_skin(&path) {
                        Ok(entry) => {
                            state_rc.borrow_mut().config.appearance.active_skin =
                                entry.name.clone();
                            state_rc.borrow_mut().config.appearance.hidden_skins
                                .retain(|n| !n.eq_ignore_ascii_case(&entry.name));
                            rebuild();
                            if let Some(row) = find_row_by_name(&listbox, &entry.name) {
                                listbox.select_row(Some(&row));
                            }
                        }
                        Err(e) => {
                            show_alert_parented(
                                Some(&win_alert),
                                &format!("Could not add skin: {e}"),
                            );
                        }
                    }
                });
            });
        }

        // Wire Remove (disabled for built-ins)
        {
            let state_rc = state.clone();
            let rebuild = rebuild_list.clone();
            let listbox = listbox.clone();
            btn_remove.connect_clicked(move |_| {
                let Some(row) = listbox.selected_row() else { return };
                let name = row.widget_name().to_string();
                if name == "dark" || name == "light" || name.is_empty() {
                    return;
                }
                {
                    let mut s = state_rc.borrow_mut();
                    if !s.config.appearance.hidden_skins.iter().any(|h| h.eq_ignore_ascii_case(&name)) {
                        s.config.appearance.hidden_skins.push(name.clone());
                    }
                    if s.config.appearance.active_skin == name {
                        s.config.appearance.active_skin = "dark".to_string();
                    }
                }
                rebuild();
            });
        }

        // Update Remove-disabled state reactively on selection changes.
        {
            let btn_remove = btn_remove.clone();
            listbox.connect_row_selected(move |_, row| {
                let name = row.map(|r| r.widget_name().to_string()).unwrap_or_default();
                let is_builtin = name == "dark" || name == "light" || name.is_empty();
                btn_remove.set_sensitive(!is_builtin);
            });
        }

        // Wire Download (Export template CSS…)
        {
            let listbox = listbox.clone();
            let win_ref = win.clone();
            btn_download.connect_clicked(move |_| {
                let Some(row) = listbox.selected_row() else { return };
                let name = row.widget_name().to_string();
                let Some(skin) = crate::skin::load_skin(&name) else { return };

                let dialog = FileDialog::new();
                dialog.set_title("Save Sparkamp skin");
                dialog.set_initial_name(Some(&format!("{name}.css")));

                let skin_copy = skin.clone();
                dialog.save(Some(&win_ref), gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    let css = match &skin_copy.source {
                        crate::skin::SkinSource::BuiltIn => match skin_copy.name.as_str() {
                            "dark" => crate::skin::DARK_TEMPLATE_CSS.to_string(),
                            "light" => crate::skin::LIGHT_TEMPLATE_CSS.to_string(),
                            _ => crate::skin::DARK_TEMPLATE_CSS.to_string(),
                        },
                        crate::skin::SkinSource::UserFile(p) => {
                            std::fs::read_to_string(p).unwrap_or_default()
                        }
                    };
                    let _ = std::fs::write(&path, css);
                });
            });
        }

        // Separator
        let sep = gtk4::Separator::new(Orientation::Horizontal);
        sep.set_margin_top(8);
        sep.set_margin_bottom(8);
        root.append(&sep);

        // Documentation header + button
        let doc_header = Label::new(Some("Documentation"));
        doc_header.set_halign(Align::Start);
        doc_header.add_css_class("heading");
        root.append(&doc_header);

        let btn_guide = Button::with_label("Export how-to guide…");
        root.append(&btn_guide);
        {
            let win_ref = win.clone();
            btn_guide.connect_clicked(move |_| {
                let dialog = FileDialog::new();
                dialog.set_title("Save Sparkamp skin guide");
                dialog.set_initial_name(Some("sparkamp-skin-guide.md"));
                dialog.save(Some(&win_ref), gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    let _ = std::fs::write(&path, crate::skin::SKIN_GUIDE_MD);
                });
            });
        }

        let tab_lbl = Label::new(Some("Appearance"));
        notebook.append_page(&root, Some(&tab_lbl));
    }

    // ── Tab 1: Behavior ───────────────────────────────────────────────────
    {
        use crate::config::PlaylistAddBehavior;

        let grid = Grid::new();
        grid.set_row_spacing(12);
        grid.set_column_spacing(16);
        grid.set_margin_top(16);
        grid.set_margin_bottom(16);
        grid.set_margin_start(16);
        grid.set_margin_end(16);

        let lbl = Label::new(Some("Autoplay on add"));
        lbl.set_halign(Align::Start);
        grid.attach(&lbl, 0, 0, 1, 1);

        let chk = CheckButton::new();
        chk.set_active(state.borrow().config.behavior.autoplay_on_add);
        {
            let state_rc = state.clone();
            chk.connect_toggled(move |c| {
                state_rc.borrow_mut().config.behavior.autoplay_on_add = c.is_active();
            });
        }
        grid.attach(&chk, 1, 0, 1, 1);

        // Row 1: Default playlist behavior for media library add.
        let lbl_add = Label::new(Some("Media library → playlist"));
        lbl_add.set_halign(Align::Start);
        grid.attach(&lbl_add, 0, 1, 1, 1);

        let dd_add = DropDown::from_strings(&["Append to current", "Replace current"]);
        {
            let behavior = state.borrow().config.behavior.playlist_add_behavior.clone();
            dd_add.set_selected(match behavior {
                PlaylistAddBehavior::Append => 0,
                PlaylistAddBehavior::Replace => 1,
            });
        }
        {
            let state_rc = state.clone();
            dd_add.connect_selected_notify(move |d| {
                let behavior = match d.selected() {
                    1 => PlaylistAddBehavior::Replace,
                    _ => PlaylistAddBehavior::Append,
                };
                state_rc.borrow_mut().config.behavior.playlist_add_behavior = behavior;
            });
        }
        grid.attach(&dd_add, 1, 1, 1, 1);

        // Row 2: gnudb email — used for the CDDB/gnudb handshake on disc
        // identify and (later) submission. Stored locally only.
        let lbl_email = Label::new(Some("gnudb email"));
        lbl_email.set_halign(Align::Start);
        lbl_email.set_tooltip_text(Some(
            "Your email for the gnudb/CDDB handshake — needed to identify and \
             submit disc metadata. Stored locally and used only to talk to gnudb.",
        ));
        grid.attach(&lbl_email, 0, 2, 1, 1);

        let email_entry = gtk4::Entry::new();
        email_entry.set_hexpand(true);
        email_entry.set_placeholder_text(Some("you@example.com"));
        email_entry.set_text(&gtk_safe(&state.borrow().config.disc.gnudb_email));
        {
            let state_rc = state.clone();
            email_entry.connect_changed(move |e| {
                let mut s = state_rc.borrow_mut();
                s.config.disc.gnudb_email = e.text().to_string();
                let _ = s.config.save();
            });
        }
        grid.attach(&email_entry, 1, 2, 1, 1);

        let tab_lbl = Label::new(Some("Behavior"));
        notebook.append_page(&grid, Some(&tab_lbl));
    }

    // ── Tab 2: Visualizer ─────────────────────────────────────────────────
    {
        let grid = Grid::new();
        grid.set_row_spacing(12);
        grid.set_column_spacing(16);
        grid.set_margin_top(16);
        grid.set_margin_bottom(16);
        grid.set_margin_start(16);
        grid.set_margin_end(16);

        // ── Mode selector ──────────────────────────────────────────────
        let lbl = Label::new(Some("Visualizer mode"));
        lbl.set_halign(Align::Start);
        grid.attach(&lbl, 0, 0, 1, 1);

        // DropDown: index 0 = Bars, 1 = Waveform, 2 = Granite.
        let dd_mode = DropDown::from_strings(&["Bars", "Waveform", "Granite"]);
        {
            let mode = state.borrow().config.visualizer.mode.clone();
            dd_mode.set_selected(match mode {
                VisualizerMode::Bars     => 0,
                VisualizerMode::Waveform => 1,
                VisualizerMode::Granite  => 2,
            });
        }
        {
            let state_rc = state.clone();
            dd_mode.connect_selected_notify(move |d| {
                let mut s = state_rc.borrow_mut();
                s.config.visualizer.mode = match d.selected() {
                    0 => VisualizerMode::Bars,
                    1 => VisualizerMode::Waveform,
                    _ => VisualizerMode::Granite,
                };
            });
        }
        grid.attach(&dd_mode, 1, 0, 1, 1);

        // ── Keep display awake during fullscreen visualizer ────────────
        // Mode-independent: applies to Waveform and Granite fullscreen.
        let lbl_awake = Label::new(Some("Keep display awake in fullscreen"));
        lbl_awake.set_halign(Align::Start);
        grid.attach(&lbl_awake, 0, 4, 1, 1);
        let chk_awake = CheckButton::new();
        chk_awake.set_active(state.borrow().config.visualizer.keep_screen_awake);
        {
            let state_rc = state.clone();
            chk_awake.connect_toggled(move |c| {
                state_rc.borrow_mut().config.visualizer.keep_screen_awake =
                    c.is_active();
            });
        }
        grid.attach(&chk_awake, 1, 4, 1, 1);

        // ── Bars Settings (visible only when Bars mode is selected) ───
        let bars_settings_box = Grid::new();
        bars_settings_box.set_row_spacing(12);
        bars_settings_box.set_column_spacing(16);
        bars_settings_box.set_margin_top(16);
        bars_settings_box.set_margin_start(16);
        bars_settings_box.attach(&Label::new(Some("Bars Settings")), 0, 0, 2, 1);

        // Mirror bars toggle
        let lbl_mirror = Label::new(Some("Mirror bars"));
        lbl_mirror.set_halign(Align::Start);
        bars_settings_box.attach(&lbl_mirror, 0, 1, 1, 1);

        let chk_mirror = CheckButton::new();
        {
            let bars_mirror = state.borrow().config.visualizer.bars_mirror;
            chk_mirror.set_active(bars_mirror);
        }
        {
            let state_rc = state.clone();
            chk_mirror.connect_toggled(move |c| {
                state_rc.borrow_mut().config.visualizer.bars_mirror = c.is_active();
            });
        }
        bars_settings_box.attach(&chk_mirror, 1, 1, 1, 1);

        // Color zones selector
        let lbl_zones = Label::new(Some("Color zones"));
        lbl_zones.set_halign(Align::Start);
        bars_settings_box.attach(&lbl_zones, 0, 2, 1, 1);

        let spin_zones = SpinButton::with_range(1.0, 6.0, 1.0);
        {
            let zones = state.borrow().config.visualizer.color_zones;
            spin_zones.set_value(zones as f64);
        }
        bars_settings_box.attach(&spin_zones, 1, 2, 1, 1);

        // Zone colors - create 6 color buttons (one per possible zone)
        let zone_color_buttons: Vec<(Label, ColorButton)> = (0..6)
            .map(|i| {
                let lbl = Label::new(Some(&format!("Zone {} color:", i + 1)));
                lbl.set_halign(Align::Start);

                let btn = ColorButton::new();
                let zone_colors = state.borrow().config.visualizer.zone_colors.clone();
                if let Some(hex) = zone_colors.get(i) {
                    if let Ok(rgba) = gdk::RGBA::parse(hex) {
                        btn.set_rgba(&rgba);
                    }
                }

                (lbl, btn)
            })
            .collect();

        // Add color buttons to grid (start at row 3)
        for (i, (lbl, btn)) in zone_color_buttons.iter().enumerate() {
            bars_settings_box.attach(lbl, 0, 3 + i as i32, 1, 1);
            bars_settings_box.attach(btn, 1, 3 + i as i32, 1, 1);
            // Start with all hidden; they'll be shown based on zone count
            lbl.set_visible(false);
            btn.set_visible(false);
        }

        // Helper to update zone button visibility
        let update_zone_visibility = {
            let zone_labels: Vec<_> = zone_color_buttons.iter().map(|(l, _)| l.clone()).collect();
            let zone_buttons: Vec<_> = zone_color_buttons.iter().map(|(_, b)| b.clone()).collect();
            move |num_zones: u8| {
                for i in 0..6 {
                    let visible = (i as u8) < num_zones;
                    zone_labels[i].set_visible(visible);
                    zone_buttons[i].set_visible(visible);
                }
            }
        };

        // Connect zone count changes
        {
            let state_rc = state.clone();
            let update_zone_visibility = update_zone_visibility.clone();
            spin_zones.connect_value_changed(move |s| {
                let num_zones = s.value() as u8;
                state_rc.borrow_mut().config.visualizer.color_zones = num_zones;
                update_zone_visibility(num_zones);
            });
        }

        // Connect color button signals
        for (i, (_, btn)) in zone_color_buttons.iter().enumerate() {
            let state_rc = state.clone();
            btn.connect_color_set(move |button| {
                let rgba = button.rgba();
                let hex = format!(
                    "#{:02x}{:02x}{:02x}",
                    (rgba.red() * 255.0) as u8,
                    (rgba.green() * 255.0) as u8,
                    (rgba.blue() * 255.0) as u8,
                );
                let mut s = state_rc.borrow_mut();
                let zone_colors = &mut s.config.visualizer.zone_colors;
                // Ensure we have at least i+1 entries
                while zone_colors.len() <= i {
                    zone_colors.push("#000000".to_string());
                }
                zone_colors[i] = hex;
            });
        }

        // Set initial visibility based on current zone count
        {
            let num_zones = state.borrow().config.visualizer.color_zones;
            update_zone_visibility(num_zones);
        }

        // Show/hide bars settings based on mode
        bars_settings_box.set_visible(false); // Start hidden
        {
            let bars_settings = bars_settings_box.clone();
            dd_mode.connect_selected_notify(move |d| {
                bars_settings.set_visible(d.selected() == 0);
            });
        }
        {
            let bars_settings = bars_settings_box.clone();
            bars_settings.set_visible(
                state.borrow().config.visualizer.mode == VisualizerMode::Bars,
            );
        }

        grid.attach(&bars_settings_box, 0, 1, 2, 1);

        // ── Waveform Settings (visible only when Waveform mode is selected) ─
        let wf_settings_box = Grid::new();
        wf_settings_box.set_row_spacing(12);
        wf_settings_box.set_column_spacing(16);
        wf_settings_box.set_margin_top(16);
        wf_settings_box.set_margin_start(16);
        wf_settings_box.attach(&Label::new(Some("Waveform Settings")), 0, 0, 2, 1);

        // Style selector (Lines / Filled)
        let lbl_wf_style = Label::new(Some("Style"));
        lbl_wf_style.set_halign(Align::Start);
        wf_settings_box.attach(&lbl_wf_style, 0, 1, 1, 1);

        let dd_wf_style = DropDown::from_strings(&["Lines", "Filled"]);
        {

            let cur = state.borrow().config.visualizer.waveform_style.clone();
            dd_wf_style.set_selected(match cur {
                WaveformStyle::Lines => 0,
                WaveformStyle::Filled => 1,
            });
        }
        {

            let state_rc = state.clone();
            dd_wf_style.connect_selected_notify(move |d| {
                state_rc.borrow_mut().config.visualizer.waveform_style = match d.selected() {
                    1 => WaveformStyle::Filled,
                    _ => WaveformStyle::Lines,
                };
            });
        }
        wf_settings_box.attach(&dd_wf_style, 1, 1, 1, 1);

        // Color zones count
        let lbl_wf_zones = Label::new(Some("Color zones"));
        lbl_wf_zones.set_halign(Align::Start);
        wf_settings_box.attach(&lbl_wf_zones, 0, 2, 1, 1);

        let spin_wf_zones = SpinButton::with_range(1.0, 6.0, 1.0);
        {
            let zones = state.borrow().config.visualizer.waveform_color_zones;
            spin_wf_zones.set_value(zones as f64);
        }
        wf_settings_box.attach(&spin_wf_zones, 1, 2, 1, 1);

        // 6 zone colour buttons
        let wf_zone_color_buttons: Vec<(Label, ColorButton)> = (0..6)
            .map(|i| {
                let lbl = Label::new(Some(&format!("Zone {} color:", i + 1)));
                lbl.set_halign(Align::Start);
                let btn = ColorButton::new();
                let colors = state.borrow().config.visualizer.waveform_zone_colors.clone();
                if let Some(hex) = colors.get(i) {
                    if let Ok(rgba) = gdk::RGBA::parse(hex) {
                        btn.set_rgba(&rgba);
                    }
                }
                (lbl, btn)
            })
            .collect();

        for (i, (lbl, btn)) in wf_zone_color_buttons.iter().enumerate() {
            wf_settings_box.attach(lbl, 0, 3 + i as i32, 1, 1);
            wf_settings_box.attach(btn, 1, 3 + i as i32, 1, 1);
            lbl.set_visible(false);
            btn.set_visible(false);
        }

        let update_wf_zone_visibility = {
            let lbls: Vec<_> = wf_zone_color_buttons.iter().map(|(l, _)| l.clone()).collect();
            let btns: Vec<_> = wf_zone_color_buttons.iter().map(|(_, b)| b.clone()).collect();
            move |num: u8| {
                for i in 0..6 {
                    let v = (i as u8) < num;
                    lbls[i].set_visible(v);
                    btns[i].set_visible(v);
                }
            }
        };

        {
            let state_rc = state.clone();
            let upd = update_wf_zone_visibility.clone();
            spin_wf_zones.connect_value_changed(move |s| {
                let n = s.value() as u8;
                state_rc.borrow_mut().config.visualizer.waveform_color_zones = n;
                upd(n);
            });
        }

        for (i, (_, btn)) in wf_zone_color_buttons.iter().enumerate() {
            let state_rc = state.clone();
            btn.connect_color_set(move |button| {
                let rgba = button.rgba();
                let hex = format!(
                    "#{:02x}{:02x}{:02x}",
                    (rgba.red() * 255.0) as u8,
                    (rgba.green() * 255.0) as u8,
                    (rgba.blue() * 255.0) as u8,
                );
                let mut s = state_rc.borrow_mut();
                let colors = &mut s.config.visualizer.waveform_zone_colors;
                while colors.len() <= i {
                    colors.push("#000000".to_string());
                }
                colors[i] = hex;
            });
        }

        {
            let n = state.borrow().config.visualizer.waveform_color_zones;
            update_wf_zone_visibility(n);
        }

        // Show/hide waveform settings based on mode
        wf_settings_box.set_visible(false);
        {
            let wf_settings = wf_settings_box.clone();
            dd_mode.connect_selected_notify(move |d| {
                wf_settings.set_visible(d.selected() == 1);
            });
        }
        {
            let wf_settings = wf_settings_box.clone();
            wf_settings.set_visible(
                state.borrow().config.visualizer.mode == VisualizerMode::Waveform,
            );
        }

        grid.attach(&wf_settings_box, 0, 2, 2, 1);

        // ── Granite Settings (visible only when Granite mode is selected) ─
        let gr_settings_box = Grid::new();
        gr_settings_box.set_row_spacing(12);
        gr_settings_box.set_column_spacing(16);
        gr_settings_box.set_margin_top(16);
        gr_settings_box.set_margin_start(16);
        gr_settings_box.attach(&Label::new(Some("Granite Settings")), 0, 0, 2, 1);

        // Credit where it's due: Granite is a re-creation, not an original
        // idea. Same text as the macOS Settings window.
        let lbl_gr_credit = Label::new(None);
        lbl_gr_credit.set_markup(
            "<small>Granite is an interpretation of the Geiss Winamp plugin \
             by Ryan Geiss. All credit to his amazing work on the original. \
             <a href=\"https://www.geisswerks.com/geiss/\">Click</a> for \
             more information.</small>",
        );
        lbl_gr_credit.set_wrap(true);
        lbl_gr_credit.set_xalign(0.0);
        lbl_gr_credit.set_halign(Align::Start);
        // Pin min width == natural width so the wrap point — and therefore
        // the measured height — is the same in every measure pass. A wrapped
        // label whose min and natural widths differ makes the fixed-size
        // Settings window log "Trying to measure GtkWindow for height of X,
        // but it needs at least Y" warnings.
        lbl_gr_credit.set_width_chars(52);
        lbl_gr_credit.set_max_width_chars(52);
        lbl_gr_credit.add_css_class("dim-label");
        gr_settings_box.attach(&lbl_gr_credit, 0, 1, 2, 1);

        // Speed slider (0.1–5.0).
        let lbl_gr_speed = Label::new(Some("Speed"));
        lbl_gr_speed.set_halign(Align::Start);
        gr_settings_box.attach(&lbl_gr_speed, 0, 2, 1, 1);
        let speed_adj = Adjustment::new(
            state.borrow().config.visualizer.granite.speed as f64,
            0.1, 5.0, 0.1, 0.5, 0.0,
        );
        let scale_gr_speed = Scale::new(Orientation::Horizontal, Some(&speed_adj));
        scale_gr_speed.set_hexpand(true);
        scale_gr_speed.set_digits(2);
        scale_gr_speed.set_draw_value(true);
        {
            let state_rc = state.clone();
            speed_adj.connect_value_changed(move |a| {
                state_rc.borrow_mut().config.visualizer.granite.speed =
                    a.value().clamp(0.1, 5.0) as f32;
            });
        }
        gr_settings_box.attach(&scale_gr_speed, 1, 2, 1, 1);

        // Palette dropdown — order must match GranitePalette declaration.
        let lbl_gr_palette = Label::new(Some("Palette"));
        lbl_gr_palette.set_halign(Align::Start);
        gr_settings_box.attach(&lbl_gr_palette, 0, 3, 1, 1);
        let dd_gr_palette = DropDown::from_strings(&[
            "Granite", "Fire", "Neon", "Ocean", "Violet", "Sunset", "CRT", "Spectrum",
        ]);
        {
            use crate::granite::GranitePalette;
            let cur = state.borrow().config.visualizer.granite.palette;
            dd_gr_palette.set_selected(match cur {
                GranitePalette::Granite  => 0,
                GranitePalette::Fire     => 1,
                GranitePalette::Neon     => 2,
                GranitePalette::Ocean    => 3,
                GranitePalette::Violet   => 4,
                GranitePalette::Sunset   => 5,
                GranitePalette::Crt      => 6,
                GranitePalette::Spectrum => 7,
            });
        }
        {
            use crate::granite::GranitePalette;
            let state_rc = state.clone();
            dd_gr_palette.connect_selected_notify(move |d| {
                let p = match d.selected() {
                    1 => GranitePalette::Fire,
                    2 => GranitePalette::Neon,
                    3 => GranitePalette::Ocean,
                    4 => GranitePalette::Violet,
                    5 => GranitePalette::Sunset,
                    6 => GranitePalette::Crt,
                    7 => GranitePalette::Spectrum,
                    _ => GranitePalette::Granite,
                };
                let mut s = state_rc.borrow_mut();
                s.config.visualizer.granite.palette = p;
                // Apply to the live renderer too — it auto-rolls palettes on
                // beats, so the config value alone never reaches the screen.
                s.player.granite_set_palette(p);
            });
        }
        gr_settings_box.attach(&dd_gr_palette, 1, 3, 1, 1);

        // Feedback slider (0.0–0.9). Higher = stronger trail.
        let lbl_gr_fb = Label::new(Some("Feedback"));
        lbl_gr_fb.set_halign(Align::Start);
        gr_settings_box.attach(&lbl_gr_fb, 0, 4, 1, 1);
        let fb_adj = Adjustment::new(
            state.borrow().config.visualizer.granite.feedback as f64,
            0.0, 0.9, 0.05, 0.1, 0.0,
        );
        let scale_gr_fb = Scale::new(Orientation::Horizontal, Some(&fb_adj));
        scale_gr_fb.set_hexpand(true);
        scale_gr_fb.set_digits(2);
        scale_gr_fb.set_draw_value(true);
        {
            let state_rc = state.clone();
            fb_adj.connect_value_changed(move |a| {
                state_rc.borrow_mut().config.visualizer.granite.feedback =
                    a.value().clamp(0.0, 0.9) as f32;
            });
        }
        gr_settings_box.attach(&scale_gr_fb, 1, 4, 1, 1);

        // Effect dropdown — one entry per warp-map family.
        let lbl_gr_effect = Label::new(Some("Effect"));
        lbl_gr_effect.set_halign(Align::Start);
        gr_settings_box.attach(&lbl_gr_effect, 0, 5, 1, 1);
        let dd_gr_effect = DropDown::from_strings(&[
            "Plasma", "Tunnel", "Swirl", "Spin", "Cells", "Explode",
            "Ripple", "Shear", "Kaleidoscope", "Gravity Well", "Drain", "Flag",
        ]);
        {
            use crate::granite::GraniteEffect;
            let cur = state.borrow().config.visualizer.granite.effect;
            dd_gr_effect.set_selected(match cur {
                GraniteEffect::Plasma      => 0,
                GraniteEffect::Tunnel      => 1,
                GraniteEffect::Swirl       => 2,
                GraniteEffect::RadialSweep => 3,
                GraniteEffect::Cells       => 4,
                GraniteEffect::Explode     => 5,
                GraniteEffect::Ripple      => 6,
                GraniteEffect::Shear       => 7,
                GraniteEffect::Kaleido     => 8,
                GraniteEffect::GravityWell => 9,
                GraniteEffect::Drain       => 10,
                GraniteEffect::Flag        => 11,
            });
        }
        {
            use crate::granite::GraniteEffect;
            let state_rc = state.clone();
            dd_gr_effect.connect_selected_notify(move |d| {
                let e = match d.selected() {
                    1  => GraniteEffect::Tunnel,
                    2  => GraniteEffect::Swirl,
                    3  => GraniteEffect::RadialSweep,
                    4  => GraniteEffect::Cells,
                    5  => GraniteEffect::Explode,
                    6  => GraniteEffect::Ripple,
                    7  => GraniteEffect::Shear,
                    8  => GraniteEffect::Kaleido,
                    9  => GraniteEffect::GravityWell,
                    10 => GraniteEffect::Drain,
                    11 => GraniteEffect::Flag,
                    _  => GraniteEffect::Plasma,
                };
                let mut s = state_rc.borrow_mut();
                s.config.visualizer.granite.effect = e;
                s.player.granite_set_effect(e);
            });
        }
        gr_settings_box.attach(&dd_gr_effect, 1, 5, 1, 1);

        // Auto-switch toggle (rotates effects every ~15s).
        let lbl_gr_auto = Label::new(Some("Auto-switch effect"));
        lbl_gr_auto.set_halign(Align::Start);
        gr_settings_box.attach(&lbl_gr_auto, 0, 6, 1, 1);
        let chk_gr_auto = CheckButton::new();
        chk_gr_auto.set_active(state.borrow().config.visualizer.granite.auto_switch);
        {
            let state_rc = state.clone();
            chk_gr_auto.connect_toggled(move |c| {
                state_rc.borrow_mut().config.visualizer.granite.auto_switch = c.is_active();
            });
        }
        gr_settings_box.attach(&chk_gr_auto, 1, 6, 1, 1);

        // Show/hide based on mode (mirrors Bars/Waveform pattern).
        gr_settings_box.set_visible(false);
        {
            let gr_settings = gr_settings_box.clone();
            dd_mode.connect_selected_notify(move |d| {
                gr_settings.set_visible(d.selected() == 2);
            });
        }
        {
            let gr_settings = gr_settings_box.clone();
            gr_settings.set_visible(
                state.borrow().config.visualizer.mode == VisualizerMode::Granite,
            );
        }
        grid.attach(&gr_settings_box, 0, 3, 2, 1);

        let tab_lbl = Label::new(Some("Visualizer"));
        notebook.append_page(&grid, Some(&tab_lbl));
    }

    // ── Tab 3: Filetypes ──────────────────────────────────────────────────
    {
        use crate::config::PlaylistFormat;
        let grid = Grid::new();
        grid.set_row_spacing(12);
        grid.set_column_spacing(16);
        grid.set_margin_top(16);
        grid.set_margin_bottom(16);
        grid.set_margin_start(16);
        grid.set_margin_end(16);

        // Preferred playlist format for new saves.
        let lbl_fmt = Label::new(Some("Playlist format"));
        lbl_fmt.set_halign(Align::Start);
        grid.attach(&lbl_fmt, 0, 0, 1, 1);

        let dd_fmt = DropDown::from_strings(&["m3u8", "m3u"]);
        dd_fmt.set_selected(match state.borrow().config.media_library.playlist_format {
            PlaylistFormat::M3u8 => 0,
            PlaylistFormat::M3u => 1,
        });
        {
            let state_rc = state.clone();
            dd_fmt.connect_selected_notify(move |d| {
                let fmt = if d.selected() == 1 {
                    PlaylistFormat::M3u
                } else {
                    PlaylistFormat::M3u8
                };
                state_rc.borrow_mut().config.media_library.playlist_format = fmt;
            });
        }
        grid.attach(&dd_fmt, 1, 0, 1, 1);

        let hint = Label::new(Some(
            "New playlists, Save As, and device exports use this format. \
             Existing playlists keep their own.",
        ));
        hint.set_halign(Align::Start);
        hint.set_wrap(true);
        hint.add_css_class("status-label");
        grid.attach(&hint, 0, 1, 2, 1);

        let tab_lbl = Label::new(Some("Filetypes"));
        notebook.append_page(&grid, Some(&tab_lbl));
    }

    // ── Tab 4: Media Library (watched folders) ───────────────────────────
    {
        let grid = Grid::new();
        grid.set_row_spacing(8);
        grid.set_column_spacing(12);
        grid.set_margin_top(12);
        grid.set_margin_bottom(12);
        grid.set_margin_start(12);
        grid.set_margin_end(12);

        // Row 0: Label + button row
        let lbl_folders = Label::new(Some("Watched folders:"));
        lbl_folders.set_halign(Align::Start);

        let btn_add_folder = Button::with_label("Add Folder…");
        let btn_remove = Button::with_label("Remove");
        btn_remove.set_sensitive(false);

        let folder_list = ListBox::new();
        folder_list.add_css_class("playlist");
        folder_list.set_selection_mode(gtk4::SelectionMode::Single);

        let folder_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .min_content_height(200)
            .width_request(300)
            .child(&folder_list)
            .build();

        let status_lbl = Label::new(None);
        status_lbl.set_halign(Align::Start);
        status_lbl.add_css_class("dim-label");

        let rebuild_list = {
            let state_rc = state.clone();
            let folder_list_rc = folder_list.clone();
            let status_rc = status_lbl.clone();
            let btn_rm = btn_remove.clone();
            Rc::new(move || {
                // Snapshot folders before mutating the list.
                let folders: Vec<(i64, String)> = state_rc
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|lib| lib.list_folders().ok())
                    .unwrap_or_default();

                // Remove all rows.
                while let Some(child) = folder_list_rc.first_child() {
                    folder_list_rc.remove(&child);
                }

                // Repopulate.
                for (_, path) in &folders {
                    let row = gtk4::ListBoxRow::new();
                    let row_box = GtkBox::new(Orientation::Horizontal, 6);
                    let icon = Image::from_icon_name("folder-open");
                    let lbl = Label::new(Some(path));
                    lbl.set_hexpand(true);
                    lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                    lbl.set_halign(Align::Start);
                    row_box.append(&icon);
                    row_box.append(&lbl);
                    row.set_child(Some(&row_box));
                    row.set_activatable(true);
                    folder_list_rc.append(&row);
                }

                btn_rm.set_sensitive(!folders.is_empty());

                let count = folders.len();
                status_rc.set_text(&match count {
                    0 => "No folders — click \"Add Folder…\" to add music".to_string(),
                    1 => "1 folder".to_string(),
                    n => format!("{n} folders"),
                });
            })
        };

        rebuild_list();

        // Filled once the Rescan button is built (below). Lets "Add Folder"
        // trigger a rescan after a concurrent scan finishes.
        let rescan_holder: Rc<RefCell<Option<Button>>> = Rc::new(RefCell::new(None));

        let rebuild_for_add = rebuild_list.clone();
        let status_for_add = status_lbl.clone();
        let state_for_add = state.clone();
        let win_add = win.downgrade();
        let rescan_holder_add = rescan_holder.clone();
        btn_add_folder.connect_clicked(move |_| {
            let dialog = gtk4::FileDialog::builder()
                .title("Select Music Folder")
                .build();
            let rebuild_cb = rebuild_for_add.clone();
            let status_rc = status_for_add.clone();
            let state_rc = state_for_add.clone();
            let rescan_holder = rescan_holder_add.clone();
            dialog.select_folder(
                win_add.upgrade().as_ref(),
                None::<&gio::Cancellable>,
                move |result| {
                    let path = match result {
                        Ok(f) => f.path().map(|p| p.to_string_lossy().into_owned()),
                        Err(_) => None,
                    };
                    let Some(path_str) = path else {
                        return;
                    };
                    // A scan is already running (only one metadata scan may run
                    // at a time). Register + fast-scan the folder now so it
                    // appears immediately, then queue a full rescan to pick up
                    // its metadata once the current scan finishes.
                    if state_rc.borrow().ml_scan.is_some() {
                        let db_path = crate::media_library::MediaLibrary::db_path_pub();
                        let path_for_thread = path_str.clone();
                        status_rc.set_text(
                            "Adding folder — it will be scanned after the current scan finishes…",
                        );
                        let (fast_tx, fast_rx) =
                            std::sync::mpsc::channel::<Result<(), String>>();
                        std::thread::spawn(move || {
                            let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
                                Ok(l) => l,
                                Err(e) => {
                                    let _ = fast_tx.send(Err(format!("DB error: {e}")));
                                    return;
                                }
                            };
                            let folder_id = match lib.add_folder(&path_for_thread) {
                                Ok(r) => r.id(),
                                Err(e) => {
                                    let _ = fast_tx.send(Err(format!("Could not add: {e}")));
                                    return;
                                }
                            };
                            if let Err(e) = lib.rescan_folder_fast(folder_id, &path_for_thread) {
                                let _ = fast_tx.send(Err(format!("Fast scan error: {e}")));
                                return;
                            }
                            let _ = fast_tx.send(Ok(()));
                        });
                        let fast_rx = std::cell::RefCell::new(fast_rx);
                        let fast_done = std::cell::Cell::new(false);
                        let rebuild_q = rebuild_cb.clone();
                        let status_q = status_rc.clone();
                        let state_q = state_rc.clone();
                        let rescan_q = rescan_holder.clone();
                        glib::timeout_add_local(
                            std::time::Duration::from_millis(400),
                            move || {
                                if !fast_done.get() {
                                    match fast_rx.borrow().try_recv() {
                                        Ok(Ok(())) => {
                                            fast_done.set(true);
                                            rebuild_q();
                                            if let Some(ref cb) =
                                                state_q.borrow().rebuild_ml_callback
                                            {
                                                cb();
                                            }
                                            status_q.set_text("Folder added — waiting to scan…");
                                        }
                                        Ok(Err(e)) => {
                                            status_q.set_text(&e);
                                            return glib::ControlFlow::Break;
                                        }
                                        Err(_) => {}
                                    }
                                    return glib::ControlFlow::Continue;
                                }
                                // Fast add done; once the running scan ends,
                                // trigger a rescan to scan the new folder.
                                if state_q.borrow().ml_scan.is_none() {
                                    if let Some(btn) = rescan_q.borrow().as_ref() {
                                        btn.emit_clicked();
                                    }
                                    return glib::ControlFlow::Break;
                                }
                                glib::ControlFlow::Continue
                            },
                        );
                        return;
                    }
                    let path_for_thread = path_str.clone();

                    let cancel_flag = start_ml_scan(&state_rc, ScanType::AddFolder, 0);
                    status_rc.set_text("Reading tags…");

                    // Three channels: fast done, metadata progress, final result.
                    let (fast_tx, fast_rx) = std::sync::mpsc::channel::<Result<usize, String>>();
                    let (progress_tx, progress_rx) = std::sync::mpsc::channel::<(usize, usize)>();
                    let (result_tx, result_rx) =
                        std::sync::mpsc::channel::<Result<(bool, usize), String>>();

                    std::thread::spawn(move || {
                        let lib = match crate::media_library::MediaLibrary::open_at(
                            &crate::media_library::MediaLibrary::db_path_pub(),
                        ) {
                            Ok(l) => l,
                            Err(e) => {
                                let _ = fast_tx.send(Err(format!("DB error: {e}")));
                                return;
                            }
                        };

                        let folder_id = match lib.add_folder(&path_for_thread) {
                            Ok(r) => r.id(),
                            Err(e) => {
                                let _ = fast_tx.send(Err(format!("Could not add: {e}")));
                                return;
                            }
                        };

                        // Phase 1: fast scan
                        if let Err(e) = lib.rescan_folder_fast(folder_id, &path_for_thread) {
                            let _ = fast_tx.send(Err(format!("Fast scan error: {e}")));
                            return;
                        }
                        let _ = fast_tx.send(Ok(0usize));

                        // Phase 2: metadata scan. Reset tracks with no metadata first
                        // so scan_folder picks up any that a previous scan missed.
                        let _ = lib.reset_unscanned_metadata();
                        let count = lib
                            .scan_folder(folder_id, &cancel_flag, |c, t| {
                                let _ = progress_tx.send((c, t));
                            })
                            .map(|(scanned, _, _)| scanned)
                            .unwrap_or(0);
                        let _ = result_tx.send(Ok((true, count)));
                    });

                    let fast_rx = std::cell::RefCell::new(fast_rx);
                    let progress_rx = std::cell::RefCell::new(progress_rx);
                    let result_rx = std::cell::RefCell::new(result_rx);
                    let fast_handled = std::cell::Cell::new(false);
                    let path_str_clone = path_str.clone();
                    glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                        // Handle fast scan completion
                        if !fast_handled.get() {
                            if let Ok(fast_result) = fast_rx.borrow().try_recv() {
                                fast_handled.set(true);
                                if let Err(e) = fast_result {
                                    status_rc.set_text(&e);
                                    complete_ml_scan(&state_rc);
                                    return glib::ControlFlow::Break;
                                }
                                rebuild_cb();
                                // Rebuild ML window to show added files
                                if let Some(ref cb) = state_rc.borrow().rebuild_ml_callback {
                                    cb();
                                }
                            }
                        }

                        // Drain progress updates
                        while let Ok((current, total)) = progress_rx.borrow().try_recv() {
                            update_ml_scan_progress(&state_rc, current, total);
                            status_rc.set_text(&format!("Reading tags {}/{}…", current, total));
                        }

                        // Check for completion
                        if let Ok(result) = result_rx.borrow().try_recv() {
                            rebuild_cb();
                            match result {
                                Err(e) => status_rc.set_text(&e),
                                Ok((_, count)) => {
                                    let path_short = if path_str_clone.len() > 40 {
                                        format!("{}…", &path_str_clone[..40])
                                    } else {
                                        path_str_clone.clone()
                                    };
                                    status_rc.set_text(&format!(
                                        "Added: {} ({} tracks)",
                                        path_short, count
                                    ));
                                }
                            }
                            if let Some(ref cb) = state_rc.borrow().rebuild_ml_callback {
                                cb();
                            }
                            complete_ml_scan(&state_rc);
                            return glib::ControlFlow::Break;
                        }

                        glib::ControlFlow::Continue
                    });
                },
            );
        });

        let btn_rm_state = state.clone();
        let btn_rm_rebuild = rebuild_list.clone();
        let btn_rm_status = status_lbl.clone();
        let btn_rm_list = folder_list.clone();
        let btn_rm_win = win.downgrade();
        btn_remove.connect_clicked(move |_| {
            let idx = btn_rm_list.selected_row().map(|r| r.index() as usize);
            if let Some(idx) = idx {
                let folders: Vec<(i64, String)> = btn_rm_state
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|lib| lib.list_folders().ok())
                    .unwrap_or_default();
                if idx < folders.len() {
                    let (folder_id, folder_path) = folders[idx].clone();

                    // Clone for use in dialog callback
                    let state_for_dialog = btn_rm_state.clone();
                    let rebuild_for_dialog = btn_rm_rebuild.clone();
                    let status_for_dialog = btn_rm_status.clone();
                    let win_for_dialog = btn_rm_win.clone();

                    let dialog = gtk4::AlertDialog::builder()
                        .message("Remove Folder from Library")
                        .detail("Removing this folder will remove all files in this folder from the media library.\n\nNo files will be deleted from your disk, but they will not appear in the library any longer.\n\nContinue?")
                        .buttons(vec!["Cancel".to_string(), "Continue".to_string()])
                        .cancel_button(0)
                        .default_button(0)
                        .modal(true)
                        .build();

                    let folder_id_cb = folder_id;
                    let folder_path_cb = folder_path.clone();

                    dialog.choose(
                        win_for_dialog.upgrade().as_ref(),
                        None::<&gio::Cancellable>,
                        move |result| {
                            if result == Ok(1) {
                                status_for_dialog.set_text(&format!("Removing: {}", folder_path_cb));

                                // Soft-delete the tracks AND delete the folder
                                // row on the main thread so the watched-folder
                                // list reflects the removal immediately (the
                                // folder row is what `list_folders` reads). The
                                // heavy purge runs in the background.
                                if let Some(ref lib) = state_for_dialog.borrow().media_lib {
                                    if let Ok(track_ids) = lib.track_ids_for_folder(folder_id_cb) {
                                        let _ = lib.soft_delete_tracks(&track_ids);
                                    }
                                    let _ = lib.remove_folder(folder_id_cb);
                                }

                                // Rebuild UI immediately — folder is now gone.
                                rebuild_for_dialog();
                                status_for_dialog.set_text(&format!("Removed: {}", folder_path_cb));

                                // Trigger Media Library window to refresh if open
                                if let Some(ref cb) = state_for_dialog.borrow().rebuild_ml_callback {
                                    cb();
                                }

                                // Background: purge the soft-deleted track rows.
                                let db_path = crate::media_library::MediaLibrary::db_path_pub();
                                std::thread::spawn(move || {
                                    if let Ok(lib) =
                                        crate::media_library::MediaLibrary::open_at(&db_path)
                                    {
                                        let _ = lib.purge_deleted_tracks();
                                    }
                                });
                            }
                        },
                    );
                }
            }
        });

        grid.attach(&lbl_folders, 0, 0, 2, 1);
        grid.attach(&btn_add_folder, 2, 0, 1, 1);
        grid.attach(&btn_remove, 3, 0, 1, 1);
        grid.attach(&folder_scroll, 0, 1, 4, 1);
        grid.attach(&status_lbl, 0, 2, 4, 1);

        // Row 3: Rescan button (shares state with media library window).
        let lbl_rescan = Label::new(Some("Scan:"));
        lbl_rescan.set_halign(Align::Start);

        let btn_rescan = Button::with_label("⟳ Rescan");
        let btn_cancel_scan = Button::with_label("✕ Cancel Scan");
        btn_cancel_scan.set_visible(false);
        // Let "Add Folder" trigger a rescan once a concurrent scan finishes.
        *rescan_holder.borrow_mut() = Some(btn_rescan.clone());

        let status_scan = Label::new(None);
        status_scan.set_halign(Align::Start);
        status_scan.add_css_class("dim-label");

        // Update button visibility based on scan state.
        // Clone references for the closure to avoid moving the originals.
        let state_rc_for_update = state.clone();
        let btn_rescan_ref = btn_rescan.clone();
        let btn_cancel_ref = btn_cancel_scan.clone();
        let btn_add_folder_ref = btn_add_folder.clone();
        let status_ref = status_scan.clone();
        let update_scan_ui = Rc::new(move || {
            let scan_state = state_rc_for_update.borrow().ml_scan.clone();
            if let Some(scan) = scan_state {
                btn_rescan_ref.set_visible(false);
                btn_cancel_ref.set_visible(true);
                // Disable Add Folder so a second concurrent scan cannot be started.
                btn_add_folder_ref.set_sensitive(false);
                if scan.total > 0 {
                    status_ref.set_text(&format!("Scanning {} / {}…", scan.current, scan.total));
                } else {
                    status_ref.set_text("Scanning…");
                }
            } else {
                btn_rescan_ref.set_visible(true);
                btn_cancel_ref.set_visible(false);
                btn_add_folder_ref.set_sensitive(true);
                status_ref.set_text("");
            }
        });

        // Initial UI state.
        update_scan_ui();

        // Refresh scan UI when this tab is shown.
        {
            let update_cb = update_scan_ui.clone();
            notebook.connect_switch_page(move |_, _, _| {
                update_cb();
            });
        }

        // Rescan button: trigger a full rescan of all watched folders.
        // Note: This shares state with the media library window via state.ml_scan.
        {
            let state_rc = state.clone();
            let btn_rescan_ref = btn_rescan.clone();
            let btn_cancel_ref = btn_cancel_scan.clone();
            let status_ref = status_scan.clone();

            btn_rescan.connect_clicked(move |_| {
                if state_rc.borrow().ml_scan.is_some() {
                    status_ref.set_text("Scan already in progress");
                    return;
                }
                if state_rc.borrow().media_lib.is_none() {
                    status_ref.set_text("Error: Media library not available");
                    return;
                }

                let db_path = crate::media_library::MediaLibrary::db_path_pub();

                let cancel_flag = start_ml_scan(&state_rc, ScanType::Rescan, 0);
                status_ref.set_text("Reading tags…");
                btn_rescan_ref.set_sensitive(false);
                btn_cancel_ref.set_visible(true);

                let (progress_tx, progress_rx) = std::sync::mpsc::channel();
                let (result_tx, result_rx) = std::sync::mpsc::channel();

                std::thread::spawn(move || {
                    let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
                        Ok(l) => l,
                        Err(e) => {
                            let _ = result_tx.send(Err(format!("DB error: {e}")));
                            return;
                        }
                    };
                    // Clear last_scanned for tracks with no metadata so scan_folder
                    // re-processes them (handles recovery from a prior broken scan).
                    let _ = lib.reset_unscanned_metadata();
                    let result = lib
                        .scan_all_folders(&cancel_flag, |current, total| {
                            let _ = progress_tx.send((current, total));
                        })
                        .map_err(|e| e.to_string());
                    let _ = result_tx.send(result);
                });

                let progress_rx = std::cell::RefCell::new(progress_rx);
                let result_rx = std::cell::RefCell::new(result_rx);
                let state_rc2 = state_rc.clone();
                let status_ref2 = status_ref.clone();
                let btn_rescan_ref2 = btn_rescan_ref.clone();
                let btn_cancel_ref2 = btn_cancel_ref.clone();
                glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                    // Check for progress updates
                    while let Ok((current, total)) = progress_rx.borrow().try_recv() {
                        update_ml_scan_progress(&state_rc2, current, total);
                        status_ref2.set_text(&format!("Reading tags {}/{}…", current, total));
                    }

                    // Check for completion
                    if let Ok(result) = result_rx.borrow().try_recv() {
                        {
                            let mut s = state_rc2.borrow_mut();
                            s.media_lib = crate::media_library::MediaLibrary::open().ok();
                        }
                        complete_ml_scan(&state_rc2);
                        if let Some(ref cb) = state_rc2.borrow().rebuild_ml_callback {
                            cb();
                        }
                        match result {
                            Err(e) => status_ref2.set_text(&format!("Rescan error: {}", e)),
                            Ok(_) => status_ref2.set_text("Scan complete"),
                        }
                        btn_rescan_ref2.set_sensitive(true);
                        btn_cancel_ref2.set_visible(false);
                        glib::ControlFlow::Break
                    } else {
                        glib::ControlFlow::Continue
                    }
                });
            });
        }

        // Cancel scan button.
        {
            let state_rc = state.clone();
            let status_ref = status_scan.clone();
            btn_cancel_scan.connect_clicked(move |_| {
                cancel_ml_scan(&state_rc);
                status_ref.set_text("Cancelling…");
            });
        }

        // Polling timer to sync scan state with UI.
        {
            let update_ui = update_scan_ui.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                update_ui();
                glib::ControlFlow::Continue
            });
        }

        grid.attach(&lbl_rescan, 0, 3, 1, 1);
        grid.attach(&btn_rescan, 1, 3, 1, 1);
        grid.attach(&btn_cancel_scan, 1, 3, 1, 1);
        grid.attach(&status_scan, 2, 3, 2, 1);

        // Row 4: Deduplication
        let sep_row4 = gtk4::Separator::new(Orientation::Horizontal);
        sep_row4.set_margin_top(4);
        sep_row4.set_margin_bottom(4);
        grid.attach(&sep_row4, 0, 4, 4, 1);

        let btn_dedupe = Button::with_label("Deduplicate Music…");
        btn_dedupe.set_tooltip_text(Some(
            "Find tracks that appear more than once in your library",
        ));
        btn_dedupe.set_hexpand(false);
        btn_dedupe.set_halign(Align::Start);
        {
            let state_rc = state.clone();
            let win_wk = win.downgrade();
            btn_dedupe.connect_clicked(move |_| {
                open_dedupe_window(
                    win_wk.upgrade().as_ref(),
                    state_rc.clone(),
                );
            });
        }
        grid.attach(&btn_dedupe, 0, 5, 4, 1);

        let tab_lbl = Label::new(Some("Media Library"));
        notebook.append_page(&grid, Some(&tab_lbl));
    }

    // ── Tab: About ─────────────────────────────────────────────────────────
    {
        let outer = GtkBox::new(Orientation::Vertical, 16);
        outer.set_margin_top(24);
        outer.set_margin_bottom(24);
        outer.set_margin_start(24);
        outer.set_margin_end(24);

        // Header: title + version + description.
        let header = GtkBox::new(Orientation::Vertical, 4);

        let title = Label::new(Some("Sparkamp"));
        title.set_halign(Align::Start);
        title.add_css_class("about-title");
        header.append(&title);

        let version = Label::new(Some(&format!("Version {}", env!("CARGO_PKG_VERSION"))));
        version.set_halign(Align::Start);
        version.add_css_class("about-subtle");
        header.append(&version);

        let desc = Label::new(Some(
            "A compact, fast, open-source Winamp-style music player with the \
             backend built in Rust and support for UI in GNOME desktop with \
             GTK4 & macOS with Swift.",
        ));
        desc.set_halign(Align::Start);
        desc.set_xalign(0.0);
        desc.set_wrap(true);
        desc.set_max_width_chars(60);
        desc.add_css_class("about-subtle");
        header.append(&desc);

        outer.append(&header);
        outer.append(&gtk4::Separator::new(Orientation::Horizontal));

        // Engine.
        let engine_box = GtkBox::new(Orientation::Vertical, 4);
        let engine_h = Label::new(Some("Engine"));
        engine_h.set_halign(Align::Start);
        engine_h.add_css_class("about-section");
        let engine_b = Label::new(Some("GStreamer — playbin, equalizer-10bands, volume"));
        engine_b.set_halign(Align::Start);
        engine_b.add_css_class("about-subtle");
        engine_box.append(&engine_h);
        engine_box.append(&engine_b);
        outer.append(&engine_box);

        // License.
        let license_box = GtkBox::new(Orientation::Vertical, 4);
        let license_h = Label::new(Some("License"));
        license_h.set_halign(Align::Start);
        license_h.add_css_class("about-section");
        let license_link = gtk4::LinkButton::with_label(
            "https://www.gnu.org/licenses/agpl-3.0.html",
            "GNU Affero General Public License v3 (AGPL-3.0)",
        );
        license_link.set_halign(Align::Start);
        license_box.append(&license_h);
        license_box.append(&license_link);
        outer.append(&license_box);

        // GitHub.
        let gh_box = GtkBox::new(Orientation::Vertical, 4);
        let gh_h = Label::new(Some("Get the latest"));
        gh_h.set_halign(Align::Start);
        gh_h.add_css_class("about-section");
        let gh_b = Label::new(Some(
            "Source code, releases, and issue tracking are hosted on GitHub. \
             Clone the repository or grab the latest build there.",
        ));
        gh_b.set_halign(Align::Start);
        gh_b.set_xalign(0.0);
        gh_b.set_wrap(true);
        gh_b.set_max_width_chars(60);
        gh_b.add_css_class("about-subtle");
        let gh_link = gtk4::LinkButton::with_label(
            "https://github.com/jrssae/sparkamp",
            "github.com/jrssae/sparkamp",
        );
        gh_link.set_halign(Align::Start);
        gh_box.append(&gh_h);
        gh_box.append(&gh_b);
        gh_box.append(&gh_link);
        outer.append(&gh_box);

        let scroll = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .child(&outer)
            .build();

        let tab_lbl = Label::new(Some("About"));
        notebook.append_page(&scroll, Some(&tab_lbl));
        // Move About to leftmost position.
        notebook.reorder_child(&scroll, Some(0));
    }

    // About tab is index 0 — the default landing tab when no specific tab
    // was requested by the caller. Other tabs shifted right by one:
    // Appearance(1), Behavior(2), Visualizer(3), Filetypes(4), Media Library(5).
    notebook.set_current_page(Some(initial_tab.unwrap_or(0)));

    // ── Close button ───────────────────────────────────────────────────────
    // Changes are applied immediately; this button just closes the window.
    let close_btn = Button::with_label("Close");
    close_btn.set_margin_top(4);
    close_btn.set_margin_bottom(8);
    close_btn.set_margin_start(8);
    close_btn.set_margin_end(8);
    close_btn.set_halign(Align::End);
    {
        let win_wk = win.downgrade();
        close_btn.connect_clicked(move |_| {
            if let Some(w) = win_wk.upgrade() {
                w.close();
            }
        });
    }

    // Save when the window is closed via the window-manager button.
    {
        let state_rc = state.clone();
        win.connect_close_request(move |_| {
            let _ = state_rc.borrow().config.save();
            glib::Propagation::Proceed
        });
    }

    let vbox = GtkBox::new(Orientation::Vertical, 0);
    vbox.append(&notebook);
    vbox.append(&close_btn);
    win.set_child(Some(&vbox));
    win.present();
}

// ---------------------------------------------------------------------------
// Equalizer window
// ---------------------------------------------------------------------------

/// Open the 10-band parametric equalizer window.
///
/// The window shows a row of 10 vertical `Scale` sliders (one per band),
/// a preset `DropDown`, an Enable toggle, and a Reset button.
///
/// All control changes update `state.config.equalizer` immediately AND apply
/// to the live GStreamer pipeline so the user hears the result in real time.
/// Config is saved to disk when the window is closed.
fn open_eq_window(parent: Option<&gtk4::Window>, state: Rc<RefCell<AppState>>) -> gtk4::Window {
    use crate::config::EQ_PRESETS;
    use gtk4::{Adjustment, Box as GtkBox, CheckButton, DropDown, Label, Orientation, Scale};

    let win = gtk4::Window::new();
    win.set_title(Some("Equalizer — SparkAmp"));
    win.set_default_size(560, 240);
    win.set_resizable(false);
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }

    let vbox = GtkBox::new(Orientation::Vertical, 8);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    // ── Enable toggle + preset row ───────────────────────────────────────────
    let top_row = GtkBox::new(Orientation::Horizontal, 8);

    let enable_btn = CheckButton::with_label("Enable EQ");
    enable_btn.set_active(state.borrow().config.equalizer.enabled);

    // Build preset list: the names + "Custom" entry at the end.
    let mut preset_names: Vec<&str> = EQ_PRESETS.iter().map(|(n, _)| *n).collect();
    preset_names.push("Custom");
    let preset_dd = DropDown::from_strings(&preset_names);
    preset_dd.set_tooltip_text(Some("EQ Preset"));

    // Select the current preset (or "Custom" if not found).
    {
        let current = state.borrow().config.equalizer.preset.clone();
        let idx = EQ_PRESETS
            .iter()
            .position(|(n, _)| *n == current)
            .unwrap_or(EQ_PRESETS.len()); // fallback: Custom
        preset_dd.set_selected(idx as u32);
    }

    let reset_btn = gtk4::Button::with_label("Reset");
    reset_btn.set_tooltip_text(Some("Set all bands to 0 dB"));

    top_row.append(&enable_btn);
    top_row.append(&preset_dd);
    top_row.append(&reset_btn);
    vbox.append(&top_row);

    // ── Pre-amp slider ────────────────────────────────────────────────────────
    let preamp_row = GtkBox::new(Orientation::Horizontal, 8);
    preamp_row.set_margin_top(4);
    preamp_row.set_margin_bottom(4);

    let preamp_label = Label::new(Some("Pre-amp"));
    preamp_label.set_halign(gtk4::Align::Start);
    preamp_label.set_width_request(70);

    let init_preamp = state.borrow().config.equalizer.preamp.clamp(0.5, 1.5);
    let preamp_adj = Adjustment::new(init_preamp, 0.5, 1.5, 0.01, 0.1, 0.0);
    let preamp_scale = Scale::new(Orientation::Horizontal, Some(&preamp_adj));
    preamp_scale.add_css_class("eq-scale");
    preamp_scale.set_hexpand(true);
    preamp_scale.set_draw_value(false);
    preamp_scale.add_mark(0.5, gtk4::PositionType::Bottom, Some("50%"));
    preamp_scale.add_mark(1.0, gtk4::PositionType::Bottom, Some("100%"));
    preamp_scale.add_mark(1.5, gtk4::PositionType::Bottom, Some("150%"));

    let preamp_pct_label = Label::new(Some(&format!("{:.0}%", init_preamp * 100.0)));
    preamp_pct_label.set_width_request(40);
    preamp_pct_label.set_halign(gtk4::Align::End);

    preamp_scale.set_sensitive(state.borrow().config.equalizer.enabled);

    preamp_row.append(&preamp_label);
    preamp_row.append(&preamp_scale);
    preamp_row.append(&preamp_pct_label);
    vbox.append(&preamp_row);

    preamp_scale.connect_value_changed({
        let state_rc = state.clone();
        let preamp_pct_label = preamp_pct_label.clone();
        move |s| {
            let clamped = s.value().clamp(0.5, 1.5);
            {
                let mut st = state_rc.borrow_mut();
                st.config.equalizer.preamp = clamped;
                st.player.set_preamp(clamped);
            }
            preamp_pct_label.set_text(&format!("{:.0}%", clamped * 100.0));
        }
    });

    // ── Band sliders ─────────────────────────────────────────────────────────
    // One column per band: frequency label on top, vertical scale in the middle,
    // gain-value label at the bottom.
    let bands_row = GtkBox::new(Orientation::Horizontal, 2);
    bands_row.set_hexpand(true);

    let mut sliders: Vec<Scale> = Vec::with_capacity(10);
    let bands_snapshot: Vec<f64> = {
        let eq = &state.borrow().config.equalizer;
        let mut v = eq.bands.clone();
        v.resize(10, 0.0);
        v
    };

    for i in 0..10 {
        let col = GtkBox::new(Orientation::Vertical, 2);
        col.set_hexpand(true);

        // Vertical scale: user-facing range ±12 dB (engine clamps internally).
        let adj = Adjustment::new(bands_snapshot[i].clamp(-12.0, 12.0),
                                  -12.0, 12.0, 1.0, 3.0, 0.0);
        let scale = Scale::new(Orientation::Vertical, Some(&adj));
        scale.add_css_class("eq-scale");
        scale.set_inverted(true); // top = positive, bottom = negative
        scale.set_draw_value(false);
        scale.set_vexpand(true);
        scale.set_height_request(100);
        scale.add_mark(0.0, gtk4::PositionType::Right, Some("0"));
        scale.add_mark(12.0, gtk4::PositionType::Right, Some("+12"));
        scale.add_mark(-12.0, gtk4::PositionType::Right, Some("-12"));
        col.append(&scale);

        // Gain value label (updated live as the slider moves).
        let gain_label = Label::new(Some(&format!("{:+.0}", bands_snapshot[i])));
        gain_label.set_halign(gtk4::Align::Center);
        col.append(&gain_label);

        // Wire slider to live-update engine + config.
        scale.connect_value_changed({
            let state_rc = state.clone();
            let gain_label = gain_label.clone();
            move |s| {
                let gain = s.value();
                gain_label.set_text(&format!("{:+.0}", gain));
                let mut st = state_rc.borrow_mut();
                if st.config.equalizer.bands.len() < 10 {
                    st.config.equalizer.bands.resize(10, 0.0);
                }
                st.config.equalizer.bands[i] = gain;
                st.config.equalizer.preset = String::new(); // custom
                if st.config.equalizer.enabled {
                    st.player.set_eq_band(i, gain);
                }
            }
        });

        sliders.push(scale);
        bands_row.append(&col);
    }
    vbox.append(&bands_row);

    // ── Enable toggle callback: apply / zero all bands ───────────────────────
    enable_btn.connect_toggled({
        let state_rc = state.clone();
        let sliders = sliders.clone();
        let preamp_sc = preamp_scale.clone();
        move |btn| {
            let enabled = btn.is_active();
            let mut st = state_rc.borrow_mut();
            st.config.equalizer.enabled = enabled;
            let effective = st.config.equalizer.effective_bands();
            st.player.apply_eq_bands(&effective);
            // Grey-out sliders when EQ is disabled.
            preamp_sc.set_sensitive(enabled);
            for s in &sliders {
                s.set_sensitive(enabled);
            }
        }
    });

    // ── Preset dropdown callback ──────────────────────────────────────────────
    preset_dd.connect_selected_notify({
        let state_rc = state.clone();
        let sliders = sliders.clone();
        move |dd| {
            let idx = dd.selected() as usize;
            if idx >= EQ_PRESETS.len() {
                return;
            } // "Custom"
            let (name, bands) = EQ_PRESETS[idx];
            let mut st = state_rc.borrow_mut();
            st.config.equalizer.preset = name.to_string();
            st.config.equalizer.bands = bands.to_vec();
            // Move sliders without retriggering the band change callback.
            drop(st); // release borrow before calling set_value
            for (i, s) in sliders.iter().enumerate() {
                s.set_value(bands[i]);
            }
            // Re-borrow mutably to apply to engine.
            let mut st = state_rc.borrow_mut();
            if st.config.equalizer.enabled {
                st.player.apply_eq_bands(&bands);
            }
        }
    });

    // ── Reset button: all bands to 0 dB ──────────────────────────────────────
    reset_btn.connect_clicked({
        let state_rc = state.clone();
        let sliders = sliders.clone();
        let preset_dd = preset_dd.clone();
        move |_| {
            let flat = [0.0f64; 10];
            // Find "Flat" preset index to select it, or leave as Custom.
            let flat_idx = EQ_PRESETS
                .iter()
                .position(|(n, _)| *n == "Flat")
                .unwrap_or(EQ_PRESETS.len());
            preset_dd.set_selected(flat_idx as u32);
            let mut st = state_rc.borrow_mut();
            st.config.equalizer.preset = "Flat".to_string();
            st.config.equalizer.bands = flat.to_vec();
            drop(st);
            for (i, s) in sliders.iter().enumerate() {
                s.set_value(flat[i]);
            }
            let mut st = state_rc.borrow_mut();
            st.player.apply_eq_bands(&flat);
        }
    });

    // ── Save config on close ─────────────────────────────────────────────────
    win.connect_close_request({
        let state_rc = state.clone();
        move |_w| {
            let _ = state_rc.borrow().config.save();
            glib::Propagation::Proceed
        }
    });

    win.set_child(Some(&vbox));
    win.set_hide_on_close(true);
    win.present();
    win
}

// ---------------------------------------------------------------------------
// Deduplication window
// ---------------------------------------------------------------------------

/// Messages sent from the background scan thread to the GTK tick loop.
enum DedupeMsg {
    Status(String),
    Done(Vec<crate::dedupe::DupeGroup>),
}

/// Open the standalone Deduplicate Music window.
///
/// Results are shown in a single virtualised `TreeView` backed by a
/// `TreeStore` so that scrolling stays smooth even with thousands of
/// duplicate groups.  Group rows are collapsed by default; clicking the
/// expander reveals each group's individual file rows.
///
/// The window immediately starts a background scan of the media library.  It
/// is independent and non-modal so the user can continue playback while
/// waiting.  A cancel button with a confirmation prompt guards against
/// accidental cancellation; closing the window while scanning also prompts.
///
/// ## TreeStore column layout
///
/// | # | Type   | Meaning                                          |
/// |---|--------|--------------------------------------------------|
/// | 0 | String | Primary label (group heading or track path)      |
/// | 1 | String | Secondary (confidence+count, or track title)     |
/// | 2 | String | Artist (empty for group rows)                    |
/// | 3 | String | Album  (empty for group rows)                    |
/// | 4 | String | Duration (empty for group rows)                  |
/// | 5 | String | File size (empty for group rows)                 |
/// | 6 | String | Bitrate  (empty for group rows)                  |
/// | 7 | String | Format   (empty for group rows)                  |
/// | 8 | i64    | Track ID (0 for group rows)                      |
/// | 9 | bool   | `true` → group row, `false` → track row          |
/// |10 | i32    | Pango weight (700 group, 400 track)              |
/// |11 | String | Full path (empty for group rows; for file-open)  |
fn open_dedupe_window(parent: Option<&gtk4::Window>, state: Rc<RefCell<AppState>>) {
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

    let win = gtk4::Window::new();
    win.set_title(Some("Deduplicate Music — Sparkamp"));
    win.set_default_size(900, 600);
    win.set_resizable(true);
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }

    // ── Layout ───────────────────────────────────────────────────────────────
    let root = GtkBox::new(Orientation::Vertical, 0);

    // Status bar
    let status_row = GtkBox::new(Orientation::Horizontal, 8);
    status_row.set_margin_top(8);
    status_row.set_margin_bottom(4);
    status_row.set_margin_start(10);
    status_row.set_margin_end(10);

    let status_lbl = Label::new(Some("Preparing scan…"));
    status_lbl.set_hexpand(true);
    status_lbl.set_halign(Align::Start);

    let action_btn = Button::with_label("✕ Cancel");
    action_btn.add_css_class("pl-btn");

    status_row.append(&status_lbl);
    status_row.append(&action_btn);

    // ── Single virtualised TreeView for all groups and their tracks ───────────
    // Using TreeStore so GTK only creates widgets for visible rows regardless
    // of the total group count — essential for libraries with thousands of dupes.
    #[allow(deprecated)]
    let tree_store = gtk4::TreeStore::new(&[
        String::static_type(), // 0  primary label
        String::static_type(), // 1  secondary
        String::static_type(), // 2  artist
        String::static_type(), // 3  album
        String::static_type(), // 4  duration
        String::static_type(), // 5  size
        String::static_type(), // 6  bitrate
        String::static_type(), // 7  format
        i64::static_type(),    // 8  track id (0 for group rows)
        bool::static_type(),   // 9  is_group
        i32::static_type(),    // 10 pango weight
        String::static_type(), // 11 full path (empty for group rows)
    ]);

    #[allow(deprecated)]
    let tree_view = TreeView::with_model(&tree_store);
    tree_view.set_headers_visible(true);
    tree_view.set_enable_search(false);
    tree_view.set_activate_on_single_click(false);
    tree_view.add_css_class("playlist");
    tree_view.set_hexpand(true);
    tree_view.set_vexpand(true);

    // Build visible columns: expander on col 0, then cols 1-7.
    {
        let col_defs: &[(&str, i32, bool)] = &[
            ("Group / Path", 0, true),
            ("Title / Info", 1, false),
            ("Artist",       2, false),
            ("Album",        3, false),
            ("Duration",     4, false),
            ("Size",         5, false),
            ("Bitrate",      6, false),
            ("Format",       7, false),
        ];
        for (title, data_col, expands) in col_defs {
            #[allow(deprecated)]
            let renderer = CellRendererText::new();
            #[allow(deprecated)]
            let col = TreeViewColumn::new();
            col.set_title(title);
            col.set_resizable(true);
            col.set_expand(*expands);
            #[allow(deprecated)]
            col.pack_start(&renderer, true);
            #[allow(deprecated)]
            col.add_attribute(&renderer, "text", *data_col);
            #[allow(deprecated)]
            col.add_attribute(&renderer, "weight", 10); // pango weight col
            #[allow(deprecated)]
            tree_view.append_column(&col);
        }
    }

    let scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Automatic)
        .vscrollbar_policy(PolicyType::Automatic)
        .hexpand(true)
        .vexpand(true)
        .child(&tree_view)
        .build();

    root.append(&status_row);
    root.append(&gtk4::Separator::new(Orientation::Horizontal));
    root.append(&scroll);
    win.set_child(Some(&root));

    // ── Shared scan state ────────────────────────────────────────────────────
    // cancel_flag is shared with the background thread.
    let cancel_flag: Rc<RefCell<Arc<AtomicBool>>> =
        Rc::new(RefCell::new(Arc::new(AtomicBool::new(false))));
    let is_scanning = Rc::new(Cell::new(false));
    // Channel receiver is replaceable so Rescan can start a new thread.
    let result_rx: Rc<RefCell<Option<std::sync::mpsc::Receiver<DedupeMsg>>>> =
        Rc::new(RefCell::new(None));

    // ── Helper: format a file size as "X.X MB" / "X KB" ────────────────────
    fn fmt_size(bytes: Option<u64>) -> String {
        match bytes {
            None => "—".to_string(),
            Some(b) if b >= 1_000_000 => format!("{:.1} MB", b as f64 / 1_000_000.0),
            Some(b) if b >= 1_000 => format!("{} KB", b / 1_000),
            Some(b) => format!("{} B", b),
        }
    }

    // ── Helper: format duration as "M:SS" ───────────────────────────────────
    fn fmt_dur(secs: Option<f64>) -> String {
        match secs {
            None => "—".to_string(),
            Some(s) => {
                let total = s as u64;
                format!("{}:{:02}", total / 60, total % 60)
            }
        }
    }

    // ── Helper: shorten a path for display ──────────────────────────────────
    fn shorten_path(path: &str, max_chars: usize) -> String {
        if path.len() <= max_chars {
            return path.to_string();
        }
        format!("…{}", &path[path.len().saturating_sub(max_chars)..])
    }

    // ── Track info lookup for playlist operations ────────────────────────────
    // Populated by `populate`; keyed by track id.
    let track_map: Rc<RefCell<std::collections::HashMap<i64, crate::dedupe::DupeTrackInfo>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));

    // ── Populate the TreeStore after scan completes ──────────────────────────
    let populate = {
        let tree_store = tree_store.clone();
        let status_lbl = status_lbl.clone();
        let action_btn = action_btn.clone();
        let is_scanning = is_scanning.clone();
        let track_map = track_map.clone();

        Rc::new(move |groups: Vec<crate::dedupe::DupeGroup>| {
            let probable = groups
                .iter()
                .filter(|g| g.confidence == crate::dedupe::DupeConfidence::Probable)
                .count();
            let total = groups.len();

            #[allow(deprecated)]
            tree_store.clear();
            track_map.borrow_mut().clear();

            if total == 0 {
                status_lbl.set_text("No duplicates found.");
                action_btn.set_label("↺ Rescan");
                action_btn.set_visible(true);
                is_scanning.set(false);
                return;
            }
            status_lbl.set_text(&format!(
                "{probable} probable group(s), {} less-likely group(s) found",
                total - probable
            ));
            action_btn.set_label("↺ Rescan");
            action_btn.set_visible(true);
            is_scanning.set(false);

            let mut tm = track_map.borrow_mut();
            for group in &groups {
                let bullet = if group.confidence == crate::dedupe::DupeConfidence::Probable {
                    "●"
                } else {
                    "◎"
                };
                let conf_str = if group.confidence == crate::dedupe::DupeConfidence::Probable {
                    "Probable"
                } else {
                    "Less likely"
                };
                let n = group.tracks.len();
                let group_label =
                    format!("{bullet} {}  ({conf_str} · {n} files)", group.label);

                #[allow(deprecated)]
                let group_iter = tree_store.insert_with_values(
                    None,
                    None,
                    &[
                        (0, &group_label),
                        (1, &conf_str.to_string()),
                        (2, &String::new()),
                        (3, &String::new()),
                        (4, &String::new()),
                        (5, &String::new()),
                        (6, &String::new()),
                        (7, &String::new()),
                        (8, &0i64),
                        (9, &true),
                        (10, &700i32),
                        (11, &String::new()),
                    ],
                );

                for info in &group.tracks {
                    tm.insert(info.track.id, info.clone());
                    let title = info
                        .track
                        .title
                        .as_deref()
                        .unwrap_or(info.track.filename.as_str())
                        .to_string();
                    let artist =
                        info.track.artist.as_deref().unwrap_or("—").to_string();
                    let album =
                        info.track.album.as_deref().unwrap_or("—").to_string();
                    let dur = fmt_dur(info.track.length_secs);
                    let size = fmt_size(info.file_size_bytes);
                    let kbps = info
                        .track
                        .bitrate
                        .map_or("—".to_string(), |b| format!("{b} kbps"));
                    let fmt =
                        info.track.filetype.as_deref().unwrap_or("—").to_string();
                    let short = shorten_path(&info.track.path, 55);

                    #[allow(deprecated)]
                    tree_store.insert_with_values(
                        Some(&group_iter),
                        None,
                        &[
                            (0, &short),
                            (1, &title),
                            (2, &artist),
                            (3, &album),
                            (4, &dur),
                            (5, &size),
                            (6, &kbps),
                            (7, &fmt),
                            (8, &info.track.id),
                            (9, &false),
                            (10, &400i32),
                            (11, &info.track.path),
                        ],
                    );
                }
            }
        })
    };

    // ── Right-click on a group or track row ──────────────────────────────────
    {
        let tree_store_rc = tree_store.clone();
        let tree_view_rc = tree_view.clone();
        let track_map_rc = track_map.clone();
        let state_rc = state.clone();

        let rclick = GestureClick::new();
        rclick.set_button(gdk::BUTTON_SECONDARY);
        rclick.connect_pressed(move |_, _, x, y| {
            // GestureClick gives widget-space coordinates (origin at top-left of
            // the TreeView widget, including the column-header row).
            // path_at_pos expects bin-window coordinates (origin at the top of
            // the scrollable content area, below the headers).
            // Convert before calling so the header row does not cause an
            // off-by-one in row detection.
            #[allow(deprecated)]
            let (bx, by) = tree_view_rc.convert_widget_to_bin_window_coords(x as i32, y as i32);
            #[allow(deprecated)]
            let Some((Some(tpath), _, _, _)) =
                tree_view_rc.path_at_pos(bx, by)
            else {
                return;
            };
            #[allow(deprecated)]
            let Some(row_iter) = tree_store_rc.iter(&tpath) else { return };

            // Determine row type by position in the tree: top-level rows are
            // groups, child rows are individual tracks.
            #[allow(deprecated)]
            let is_group = tree_store_rc.iter_parent(&row_iter).is_none();

            let pop_box = GtkBox::new(Orientation::Vertical, 0);

            if is_group {
                // ── Group row: add/replace playlist ──────────────────────
                let add_group = {
                    let ts = tree_store_rc.clone();
                    let giter = row_iter.clone();
                    let tm = track_map_rc.clone();
                    let st = state_rc.clone();
                    move |replace: bool| {
                        let giter = giter.clone();
                        let autoplay = st.borrow().config.behavior.autoplay_on_add;
                        let was_empty = st.borrow().playlist.is_empty();
                        if replace {
                            let _ = st.borrow_mut().player.stop();
                            st.borrow_mut().playlist.clear();
                        }
                        let insert_start = st.borrow().playlist.len();
                        let tm_borrow = tm.borrow();
                        #[allow(deprecated)]
                        if let Some(ci) = ts.iter_children(Some(&giter)) {
                            loop {
                                #[allow(deprecated)]
                                let tid: i64 =
                                    ts.get_value(&ci, 8).get::<i64>().unwrap_or(0);
                                if let Some(info) = tm_borrow.get(&tid) {
                                    st.borrow_mut()
                                        .playlist
                                        .add(crate::model::Track::from(&info.track));
                                }
                                #[allow(deprecated)]
                                if !ts.iter_next(&ci) {
                                    break;
                                }
                            }
                        }
                        drop(tm_borrow);
                        if let Some(ref cb) =
                            st.borrow().rebuild_pl_callback.clone()
                        {
                            cb();
                        }
                        if autoplay && (was_empty || replace) {
                            st.borrow_mut().playlist.jump_to(insert_start);
                            if let Some(ref cb) =
                                st.borrow().play_and_update_callback.clone()
                            {
                                cb();
                            }
                        }
                    }
                };
                let add_group = Rc::new(add_group);

                let btn_add = Button::with_label("Add to playlist");
                btn_add.add_css_class("popover-button");
                {
                    let ag = add_group.clone();
                    btn_add.connect_clicked(move |_| ag(false));
                }
                let btn_replace = Button::with_label("Replace playlist");
                btn_replace.add_css_class("popover-button");
                {
                    let ag = add_group;
                    btn_replace.connect_clicked(move |_| ag(true));
                }

                pop_box.append(&btn_add);
                pop_box.append(&btn_replace);
            } else {
                // ── Track row: open file location / dismiss ───────────────
                #[allow(deprecated)]
                let full_path: String = tree_store_rc
                    .get_value(&row_iter, 11)
                    .get::<String>()
                    .unwrap_or_default();

                let parent_dir = std::path::Path::new(&full_path)
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();

                let btn_open = Button::with_label("Open file location");
                btn_open.add_css_class("popover-button");
                {
                    let dir = parent_dir.clone();
                    btn_open.connect_clicked(move |_| {
                        let uri = format!("file://{dir}");
                        let _ = gio::AppInfo::launch_default_for_uri(
                            &uri,
                            None::<&gio::AppLaunchContext>,
                        );
                    });
                }

                let btn_dismiss = Button::with_label("Not a duplicate");
                btn_dismiss.add_css_class("popover-button");
                {
                    let ts = tree_store_rc.clone();
                    let path_str = tpath.to_str().map(|s| s.to_string()).unwrap_or_default();
                    btn_dismiss.connect_clicked(move |_| {
                        #[allow(deprecated)]
                        let Some(ti) = ts.iter_from_string(&path_str) else {
                            return;
                        };
                        #[allow(deprecated)]
                        let parent_opt = ts.iter_parent(&ti);
                        #[allow(deprecated)]
                        ts.remove(&ti);
                        // Remove the group row when fewer than 2 tracks remain.
                        if let Some(pi) = parent_opt {
                            #[allow(deprecated)]
                            let remaining = ts.iter_n_children(Some(&pi));
                            if remaining < 2 {
                                #[allow(deprecated)]
                                ts.remove(&pi);
                            }
                        }
                    });
                }

                pop_box.append(&btn_open);
                pop_box.append(&btn_dismiss);
            }

            let popover = gtk4::Popover::new();
            popover.set_child(Some(&pop_box));
            popover.set_parent(&tree_view_rc);
            popover.set_pointing_to(Some(&gdk::Rectangle::new(
                x as i32, y as i32, 1, 1,
            )));
            popover.popup();
        });
        #[allow(deprecated)]
        tree_view.add_controller(rclick);
    }

    // ── Start a background scan ──────────────────────────────────────────────
    let start_scan = {
        let cancel_flag = cancel_flag.clone();
        let result_rx = result_rx.clone();
        let is_scanning = is_scanning.clone();
        let status_lbl = status_lbl.clone();
        let action_btn = action_btn.clone();
        let tree_store = tree_store.clone();

        Rc::new(move || {
            #[allow(deprecated)]
            tree_store.clear();

            // Fresh cancel flag for the new scan.
            let new_cancel = Arc::new(AtomicBool::new(false));
            *cancel_flag.borrow_mut() = new_cancel.clone();

            let (tx, rx) = std::sync::mpsc::channel::<DedupeMsg>();
            *result_rx.borrow_mut() = Some(rx);

            is_scanning.set(true);
            status_lbl.set_text("Loading tracks from library…");
            action_btn.set_label("✕ Cancel");
            action_btn.set_visible(true);

            let db_path = crate::media_library::MediaLibrary::db_path_pub();
            std::thread::spawn(move || {
                let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = tx.send(DedupeMsg::Status(format!("Error opening library: {e}")));
                        return;
                    }
                };

                if new_cancel.load(Ordering::Relaxed) {
                    return;
                }

                let tracks = match lib.scanned_tracks() {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = tx.send(DedupeMsg::Status(format!("Error reading tracks: {e}")));
                        return;
                    }
                };

                if new_cancel.load(Ordering::Relaxed) {
                    return;
                }

                let n = tracks.len();
                let _ = tx.send(DedupeMsg::Status(format!("Analyzing {n} tracks…")));

                let groups = crate::dedupe::find_duplicates(tracks);

                if new_cancel.load(Ordering::Relaxed) {
                    return;
                }

                let _ = tx.send(DedupeMsg::Done(groups));
            });
        })
    };

    // ── Tick loop — drain the channel while the window is open ───────────────
    {
        let result_rx = result_rx.clone();
        let status_lbl = status_lbl.clone();
        let is_scanning = is_scanning.clone();
        let populate = populate.clone();
        let win_wk = win.downgrade();

        glib::timeout_add_local(Duration::from_millis(200), move || {
            if win_wk.upgrade().is_none() {
                return ControlFlow::Break;
            }
            if !is_scanning.get() {
                return ControlFlow::Continue;
            }
            let msg = result_rx.borrow().as_ref().and_then(|rx| rx.try_recv().ok());
            match msg {
                Some(DedupeMsg::Status(s)) => {
                    status_lbl.set_text(&s);
                }
                Some(DedupeMsg::Done(groups)) => {
                    populate(groups);
                }
                None => {} // still scanning or disconnected
            }
            ControlFlow::Continue
        });
    }

    // ── Cancel / Rescan button ────────────────────────────────────────────────
    {
        let cancel_flag = cancel_flag.clone();
        let is_scanning = is_scanning.clone();
        let status_lbl = status_lbl.clone();
        let action_btn2 = action_btn.clone();
        let start_scan2 = start_scan.clone();
        let win_wk = win.downgrade();

        action_btn.connect_clicked(move |_btn| {
            if is_scanning.get() {
                // Show confirmation before cancelling.
                let dialog = gtk4::AlertDialog::builder()
                    .message("Cancel scan?")
                    .detail(
                        "The scan will need to restart from the beginning if you cancel.",
                    )
                    .buttons(vec!["Keep scanning".to_string(), "Cancel scan".to_string()])
                    .cancel_button(0)
                    .default_button(0)
                    .modal(true)
                    .build();
                let flag = cancel_flag.borrow().clone();
                let scanning = is_scanning.clone();
                let lbl = status_lbl.clone();
                let btn2 = action_btn2.clone();
                dialog.choose(
                    win_wk.upgrade().as_ref(),
                    None::<&gio::Cancellable>,
                    move |result| {
                        if result == Ok(1) {
                            flag.store(true, Ordering::Relaxed);
                            scanning.set(false);
                            lbl.set_text("Scan cancelled.");
                            btn2.set_label("↺ Rescan");
                        }
                    },
                );
            } else {
                // Rescan.
                start_scan2();
            }
        });
    }

    // ── Confirm close if scan is in progress ─────────────────────────────────
    win.connect_close_request({
        let cancel_flag = cancel_flag.clone();
        let is_scanning = is_scanning.clone();
        move |w| {
            if is_scanning.get() {
                let dialog = gtk4::AlertDialog::builder()
                    .message("Scan in progress")
                    .detail("Closing this window will cancel the scan.")
                    .buttons(vec!["Keep open".to_string(), "Close anyway".to_string()])
                    .cancel_button(0)
                    .default_button(0)
                    .modal(true)
                    .build();
                let flag = cancel_flag.borrow().clone();
                let scanning = is_scanning.clone();
                let win_wk = w.downgrade();
                dialog.choose(Some(w), None::<&gio::Cancellable>, move |result| {
                    if result == Ok(1) {
                        flag.store(true, Ordering::Relaxed);
                        scanning.set(false);
                        if let Some(w) = win_wk.upgrade() {
                            w.destroy();
                        }
                    }
                });
                return glib::Propagation::Stop; // prevent default close
            }
            glib::Propagation::Proceed
        }
    });

    win.present();

    // Start the initial scan immediately after presenting the window.
    start_scan();
}

// ---------------------------------------------------------------------------
// Media Library browser window
// ---------------------------------------------------------------------------

/// Defines all columns that can appear in both the Media Library window
/// and the ID3 tag editor.  `id3_editable` fields are shown as text entries
/// in the ID3 editor; `read_only` fields are shown as non-editable labels.
struct MlColumnDef {
    id: &'static str,
    header: &'static str,
    expand: bool,
    #[allow(dead_code)]
    id3_editable: bool,
    #[allow(dead_code)]
    default_ml_visible: bool,
    #[allow(dead_code)]
    default_id3_visible: bool,
}

const ALL_COLUMNS: &[MlColumnDef] = &[
    // ── Read-only file data ────────────────────────────────────────────────
    MlColumnDef {
        id: "num",
        header: "#",
        expand: false,
        id3_editable: false,
        default_ml_visible: true,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "filename",
        header: "Filename",
        expand: true,
        id3_editable: false,
        default_ml_visible: true,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "path",
        header: "Path",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "filetype",
        header: "Type",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "bitrate",
        header: "Bitrate",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "channels",
        header: "Ch",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "duration",
        header: "Duration",
        expand: false,
        id3_editable: false,
        default_ml_visible: true,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "play_count",
        header: "# Play",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "last_played",
        header: "Last Played",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "last_scanned",
        header: "Last Scanned",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "artwork_path",
        header: "Artwork",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    // ── Editable ID3 fields ────────────────────────────────────────────────
    MlColumnDef {
        id: "title",
        header: "Title",
        expand: false,
        id3_editable: true,
        default_ml_visible: true,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "artist",
        header: "Artist",
        expand: false,
        id3_editable: true,
        default_ml_visible: true,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "album",
        header: "Album",
        expand: false,
        id3_editable: true,
        default_ml_visible: true,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "album_artist",
        header: "Album Artist",
        expand: false,
        id3_editable: true,
        default_ml_visible: true,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "year",
        header: "Year",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "genre",
        header: "Genre",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "track_num",
        header: "Track #",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "track_total",
        header: "Track Total",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "disc_num",
        header: "Disc",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "disc_total",
        header: "Disc Total",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "bpm",
        header: "BPM",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "comment",
        header: "Comment",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "composer",
        header: "Composer",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "original_artist",
        header: "Original Artist",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "copyright",
        header: "Copyright",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "url",
        header: "URL",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "encoded_by",
        header: "Encoded By",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "lyric",
        header: "Lyric",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
];

/// Re-apply the shared media-library column config (visibility, widths, order)
/// to a ColumnView's named columns. `fixed_leading` is how many pinned columns
/// precede the named ones (the files view has 0, the editor 2 = status +
/// position, the device view 1 = playlist-order). Used so the files view, the
/// playlist editor, and the device view all reflect the same column settings.
fn apply_ml_columns_to(
    col_view: &ColumnView,
    named: &[(String, ColumnViewColumn)],
    state: &Rc<RefCell<AppState>>,
    fixed_leading: u32,
) {
    let (visible_ids, widths, order): (
        Vec<String>,
        std::collections::HashMap<String, i32>,
        Vec<String>,
    ) = {
        let s = state.borrow();
        (
            s.config.media_library.visible_columns.clone(),
            s.config.media_library.ml_file_col_widths.clone(),
            s.config.media_library.ml_file_col_order.clone(),
        )
    };
    for (id, col) in named {
        col.set_visible(visible_ids.contains(id));
        if let Some(&w) = widths.get(id) {
            if w > 0 {
                col.set_fixed_width(w);
            }
        }
    }
    if !order.is_empty() {
        for (_, col) in named {
            col_view.remove_column(col);
        }
        let mut pos = fixed_leading;
        for col_id in &order {
            if let Some((_, col)) = named.iter().find(|(id, _)| id == col_id) {
                col_view.insert_column(pos, col);
                pos += 1;
            }
        }
        for (id, col) in named {
            if !order.contains(id) {
                col_view.insert_column(pos, col);
                pos += 1;
            }
        }
    }
}

/// Text shown for a `LibTrack` in a given media-library column. Shared by the
/// device track view so it mirrors the files view's columns.
fn ml_cell_text(t: &crate::media_library::LibTrack, id: &str) -> String {
    match id {
        "num" | "track_num" => t.track_num.map(|n| n.to_string()).unwrap_or_default(),
        "title" => t.title.clone().unwrap_or_else(|| t.filename.clone()),
        "artist" => t.artist.clone().unwrap_or_default(),
        "album" => t.album.clone().unwrap_or_default(),
        "album_artist" => t.album_artist.clone().unwrap_or_default(),
        "duration" => t
            .length_secs
            .map(|s| {
                let ss = s as u64;
                format!("{}:{:02}", ss / 60, ss % 60)
            })
            .unwrap_or_else(|| "-:--".to_string()),
        "filename" => t.filename.clone(),
        "path" => t.path.clone(),
        "year" => t.year.map(|y| y.to_string()).unwrap_or_default(),
        "genre" => t.genre.clone().unwrap_or_default(),
        "bitrate" => t.bitrate.map(|b| format!("{b}k")).unwrap_or_default(),
        "channels" => match t.channels.unwrap_or(0) {
            0 => String::new(),
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            n => format!("{n}ch"),
        },
        "filetype" => t.filetype.clone().unwrap_or_default(),
        "play_count" => t.play_count.to_string(),
        "last_played" => t
            .last_played
            .as_deref()
            .map(format_last_played)
            .unwrap_or_default(),
        "last_scanned" => t.last_scanned.clone().unwrap_or_default(),
        "disc_num" => {
            let d = t.disc_num.unwrap_or(0);
            if d == 0 {
                String::new()
            } else if let Some(total) = t.disc_total.filter(|x| *x > 0) {
                format!("{d}/{total}")
            } else {
                d.to_string()
            }
        }
        "disc_total" => t.disc_total.map(|d| d.to_string()).unwrap_or_default(),
        "bpm" => t.bpm.clone().unwrap_or_default(),
        "comment" => t.comment.clone().unwrap_or_default(),
        "composer" => t.composer.clone().unwrap_or_default(),
        "original_artist" => t.original_artist.clone().unwrap_or_default(),
        "copyright" => t.copyright.clone().unwrap_or_default(),
        "url" => t.url.clone().unwrap_or_default(),
        "encoded_by" => t.encoded_by.clone().unwrap_or_default(),
        "lyric" => {
            let ly = t.lyric.as_deref().unwrap_or("");
            if ly.chars().count() > 30 {
                format!("{}…", ly.chars().take(30).collect::<String>())
            } else {
                ly.to_string()
            }
        }
        "artwork_path" => {
            if t.artwork_path.is_some() {
                "Yes".to_string()
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

fn ml_sort_key(t: &crate::media_library::LibTrack, col: &str) -> String {
    match col {
        "num" => t.sort_keys.num.clone(),
        "title" => t.sort_keys.title.clone(),
        "artist" => t.sort_keys.artist.clone(),
        "album" => t.sort_keys.album.clone(),
        "duration" => t.sort_keys.duration.clone(),
        "filename" => t.sort_keys.filename.clone(),
        "year" => t.sort_keys.year.clone(),
        "genre" => t.sort_keys.genre.clone(),
        "bitrate" => t.sort_keys.bitrate.clone(),
        "channels" => format!("{:02}", t.channels.unwrap_or(0)),
        "path" => t.path.to_lowercase(),
        "play_count" => format!("{:010}", t.play_count),
        "last_played" => t.last_played.clone().unwrap_or_default(),
        "last_scanned" => t.last_scanned.clone().unwrap_or_default(),
        "comment" => t.sort_keys.comment.clone(),
        "album_artist" => t.sort_keys.album_artist.clone(),
        "disc_num" => format!("{:010}", t.disc_num.unwrap_or(0)),
        "disc_total" => format!("{:010}", t.disc_total.unwrap_or(0)),
        "composer" => t.sort_keys.composer.clone(),
        "original_artist" => t.original_artist.as_deref().unwrap_or("").to_lowercase(),
        "copyright" => t.copyright.as_deref().unwrap_or("").to_lowercase(),
        "url" => t.url.as_deref().unwrap_or("").to_lowercase(),
        "encoded_by" => t.encoded_by.as_deref().unwrap_or("").to_lowercase(),
        "bpm" => t.bpm.as_deref().unwrap_or("").to_lowercase(),
        "lyric" => t.lyric.as_deref().unwrap_or("").to_lowercase(),
        "artwork_path" => t.artwork_path.as_deref().unwrap_or("").to_lowercase(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Visualizer draw helpers (module-level so both build() and open_waveform_fullscreen can use them)
// ---------------------------------------------------------------------------

/// Parse a hex color string (`"#RRGGBB"`) to RGB components in [0, 1].
fn parse_hex_color(hex: &str) -> (f64, f64, f64) {
    let hex = hex.trim_start_matches('#');
    if hex.len() >= 6 {
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0) as f64 / 255.0;
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0) as f64 / 255.0;
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0) as f64 / 255.0;
        (r, g, b)
    } else {
        (0.0, 0.4, 0.0) // fallback dark green
    }
}

/// Draw a single zone-coloured frequency bar.
/// For singular mode: bar extends from bottom to `amp × height`.
/// For mirrored mode: bar extends `amp × height / 2` above and below centre.
fn draw_zoned_bar(
    cr: &gtk4::cairo::Context,
    x: f64,
    bar_w: f64,
    height: f64,
    amp: f64,
    mirror: bool,
    num_zones: usize,
    zone_colors: &[String],
) {
    let num_zones = num_zones.max(1);
    let half_gap = 0.75;
    let bar_w = bar_w - half_gap;

    let get_color = |zone: usize| -> (f64, f64, f64) {
        let idx = zone.min(zone_colors.len().saturating_sub(1));
        parse_hex_color(&zone_colors[idx])
    };

    if mirror {
        let center = height / 2.0;
        let max_extent = amp * center;

        for zone in 0..num_zones {
            let zone_inner = zone as f64 * (center / num_zones as f64);
            let zone_outer = (zone + 1) as f64 * (center / num_zones as f64);

            if zone_outer <= max_extent {
                let (r, g, b) = get_color(zone);
                cr.set_source_rgb(r, g, b);
                let y = center + zone_inner;
                let h = zone_outer - zone_inner;
                cr.rectangle(x + 0.5, y, bar_w, h);
                cr.fill().ok();
                let y = center - zone_outer;
                cr.rectangle(x + 0.5, y, bar_w, h);
                cr.fill().ok();
            } else if zone_inner < max_extent {
                let (r, g, b) = get_color(zone);
                cr.set_source_rgb(r, g, b);
                let y = center + zone_inner;
                let h = max_extent - zone_inner;
                cr.rectangle(x + 0.5, y, bar_w, h);
                cr.fill().ok();
                let y = center - max_extent;
                cr.rectangle(x + 0.5, y, bar_w, h);
                cr.fill().ok();
            }
        }
    } else {
        let bar_height = amp * height;
        let bar_top = height - bar_height;
        let zone_h = height / num_zones as f64;

        for zone in 0..num_zones {
            let zone_bottom = height - (zone + 1) as f64 * zone_h;
            let zone_top = height - zone as f64 * zone_h;

            if zone_top > bar_top {
                let (r, g, b) = get_color(zone);
                cr.set_source_rgb(r, g, b);
                let draw_bottom = zone_bottom.max(bar_top);
                let draw_top = zone_top.min(height);
                let h = (draw_top - draw_bottom).max(1.0);
                cr.rectangle(x + 0.5, draw_bottom, bar_w, h);
                cr.fill().ok();
            }
        }
    }
}

/// Draw the real-audio waveform visualizer using Cairo.
///
/// `samples` are bipolar PCM in `[-1, 1]` (0 = centre / silence).
/// Zones are horizontal bands; zone 0 (index 0 in `zone_colors`) is the
/// bottom of the widget and zone N-1 is the top.
///
/// - **Lines** — draws the stroke only; each segment coloured by zone.
/// - **Filled** — fills the area between the waveform and the centre
///   baseline, coloured per zone.
fn draw_waveform(
    cr: &gtk4::cairo::Context,
    width: f64,
    height: f64,
    samples: &[f64],
    num_zones: usize,
    zone_colors: &[String],
    style: &WaveformStyle,
) {
    let num_zones = num_zones.max(1);
    let center_y = height / 2.0;
    let n = samples.len();
    if n == 0 {
        return;
    }

    // Dim centre baseline.
    cr.set_source_rgb(0.0, 0.2, 0.08);
    cr.set_line_width(0.5);
    cr.move_to(0.0, center_y);
    cr.line_to(width, center_y);
    cr.stroke().ok();

    // Zone index for a Cairo y-coordinate. Zone 0 = bottom, zone N-1 = top.
    let zone_for_y = |y: f64| -> usize {
        let frac = (height - y) / height;
        ((frac * num_zones as f64) as usize).min(num_zones - 1)
    };

    let get_color = |zone: usize| -> (f64, f64, f64) {
        let idx = zone.min(zone_colors.len().saturating_sub(1));
        parse_hex_color(&zone_colors[idx])
    };

    // sample ∈ [-1, 1] → y = center - sample × (center × 0.9)
    let ys: Vec<f64> = samples
        .iter()
        .map(|&s| (center_y - s * center_y * 0.9).clamp(0.0, height))
        .collect();

    match style {
        WaveformStyle::Lines => {
            cr.set_line_width(1.5);
            for i in 0..n.saturating_sub(1) {
                let x0 = i as f64 * width / n as f64;
                let x1 = (i + 1) as f64 * width / n as f64;
                let y0 = ys[i];
                let y1 = ys[i + 1];
                let zone = zone_for_y((y0 + y1) / 2.0);
                let (r, g, b) = get_color(zone);
                cr.set_source_rgb(r, g, b);
                cr.move_to(x0, y0);
                cr.line_to(x1, y1);
                cr.stroke().ok();
            }
        }
        WaveformStyle::Filled => {
            for i in 0..n {
                let x = i as f64 * width / n as f64;
                let col_w = (width / n as f64).max(1.0);
                let y = ys[i];
                let (y_top, y_bot) = if y < center_y { (y, center_y) } else { (center_y, y) };
                for zone in 0..num_zones {
                    let zone_top_y = height - (zone + 1) as f64 * height / num_zones as f64;
                    let zone_bot_y = height - zone as f64 * height / num_zones as f64;
                    let draw_top = y_top.max(zone_top_y);
                    let draw_bot = y_bot.min(zone_bot_y);
                    if draw_top < draw_bot {
                        let (r, g, b) = get_color(zone);
                        cr.set_source_rgb(r, g, b);
                        cr.rectangle(x, draw_top, col_w, draw_bot - draw_top);
                        cr.fill().ok();
                    }
                }
            }
        }
    }
}

// Visualizer fullscreen (Waveform or Granite)
// ---------------------------------------------------------------------------

/// Open the active visualizer (Waveform or Granite) in fullscreen mode.
///
/// The window covers all other windows on the desktop.  While open:
/// - `z x c v b r s` are passed to the shared `handle_key` handler.
/// - `i` opens the information/shortcuts window.
/// - `j` opens the jump-to-track window.
/// - Status changes appear as a 3-second translucent toast at the bottom.
/// - `Esc` closes the fullscreen window.
///
/// Double-clicking the mini visualiser or pressing `f` when the active mode is
/// Waveform or Granite triggers this function. Bars is excluded.
fn open_waveform_fullscreen(
    state: Rc<RefCell<AppState>>,
    handle_key: Rc<dyn Fn(gdk::Key) -> glib::Propagation>,
    jump_win: gtk4::Window,
    jump_entry: gtk4::SearchEntry,
    rebuild_jump: Rc<dyn Fn()>,
    btn_info: gtk4::Button,
    // Single-driver rule: set while this window is open so the mini-viz
    // tick yields the shared Granite renderer (see the tick loop in build).
    fs_viz_open: Rc<Cell<bool>>,
) {
    fs_viz_open.set(true);
    let fs_win = gtk4::Window::new();
    fs_win.set_decorated(false);

    // ── Canvas (Stack: cairo + granite) + toast overlay ───────────────────
    let overlay = gtk4::Overlay::new();

    let canvas = DrawingArea::new();
    canvas.set_hexpand(true);
    canvas.set_vexpand(true);

    let granite_canvas = Picture::new();
    granite_canvas.set_hexpand(true);
    granite_canvas.set_vexpand(true);
    granite_canvas.set_content_fit(ContentFit::Fill);

    let canvas_stack = Stack::new();
    canvas_stack.set_hexpand(true);
    canvas_stack.set_vexpand(true);
    canvas_stack.add_named(&canvas, Some("cairo"));
    canvas_stack.add_named(&granite_canvas, Some("granite"));
    canvas_stack.set_visible_child_name(
        match state.borrow().config.visualizer.mode {
            VisualizerMode::Granite => "granite",
            _ => "cairo",
        },
    );
    overlay.set_child(Some(&canvas_stack));

    // Translucent status toast label at the bottom of the screen.
    let toast = gtk4::Label::new(None);
    toast.add_css_class("wf-fs-toast");
    toast.set_halign(Align::Center);
    toast.set_valign(Align::End);
    toast.set_margin_bottom(48);
    toast.set_visible(false);
    overlay.add_overlay(&toast);

    // FPS counter, top-right; toggled with the `g` key.
    let fps_label = gtk4::Label::new(Some("FPS: --"));
    fps_label.add_css_class("wf-fs-toast"); // share the toast pill style
    fps_label.set_halign(Align::End);
    fps_label.set_valign(Align::Start);
    fps_label.set_margin_top(16);
    fps_label.set_margin_end(20);
    fps_label.set_visible(false);
    overlay.add_overlay(&fps_label);

    fs_win.set_child(Some(&overlay));

    // ── Draw function ──────────────────────────────────────────────────────
    let state_draw = state.clone();
    canvas.set_draw_func(move |_da, cr, width, height| {
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.paint().ok();

        let s = state_draw.borrow();
        let is_playing = *s.player.state() == PlayerState::Playing;
        let wf_zones = s.config.visualizer.waveform_color_zones as usize;
        let wf_zone_colors = s.config.visualizer.waveform_zone_colors.clone();
        let wf_style = s.config.visualizer.waveform_style.clone();
        // Use 2× width for sharper fullscreen detail.
        let sample_count = (width * 2).max(512) as usize;
        let waveform_samples = s.player.get_waveform_samples(sample_count);
        drop(s);

        if !is_playing {
            // Flat dim centre line when idle.
            cr.set_source_rgb(0.0, 0.15, 0.05);
            cr.set_line_width(1.0);
            cr.move_to(0.0, height as f64 / 2.0);
            cr.line_to(width as f64, height as f64 / 2.0);
            cr.stroke().ok();
            return;
        }

        draw_waveform(
            cr,
            width as f64,
            height as f64,
            &waveform_samples,
            wf_zones,
            &wf_zone_colors,
            &wf_style,
        );
    });

    // ── Redraw timer (~30 fps) ─────────────────────────────────────────────
    // Cairo canvas redraws via queue_draw; Granite renders into the Picture
    // via the same MemoryTexture path the mini-viz uses.
    let canvas_weak = canvas.downgrade();
    let granite_canvas_weak = granite_canvas.downgrade();
    let stack_weak = canvas_stack.downgrade();
    let fps_label_weak = fps_label.downgrade();
    let state_tick = state.clone();
    let granite_buf: std::rc::Rc<std::cell::RefCell<Vec<u8>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    // Shut-down flag flipped from the fullscreen window's close_request so
    // the timer breaks before gsk gets a chance to paint a dead surface.
    let fs_shutting_down: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let fs_shut_for_tick = fs_shutting_down.clone();
    // FPS smoothing state. EMA of inter-frame interval; updated every tick,
    // displayed every ~10 frames so the number doesn't flicker.
    let last_instant: Rc<Cell<Option<std::time::Instant>>> = Rc::new(Cell::new(None));
    let ema_dt_ms: Rc<Cell<f32>> = Rc::new(Cell::new(33.3));
    let fps_update_countdown: Rc<Cell<u32>> = Rc::new(Cell::new(0));
    glib::timeout_add_local(std::time::Duration::from_millis(33), move || {
        if fs_shut_for_tick.get() {
            return glib::ControlFlow::Break;
        }
        let Some(c) = canvas_weak.upgrade() else { return glib::ControlFlow::Break; };
        let Some(pic) = granite_canvas_weak.upgrade() else { return glib::ControlFlow::Break; };
        let Some(stack) = stack_weak.upgrade() else { return glib::ControlFlow::Break; };
        if pic.root().is_none() {
            return glib::ControlFlow::Break;
        }
        let mode = state_tick.borrow().config.visualizer.mode.clone();
        if mode == VisualizerMode::Granite {
            if stack.visible_child_name().as_deref() != Some("granite") {
                stack.set_visible_child_name("granite");
            }
            let viewport_w = pic.width().max(1) as f64;
            let viewport_h = pic.height().max(1) as f64;
            let aspect = (viewport_w / viewport_h).max(0.5).min(4.0);
            let h: u32 = crate::granite::GRANITE_INTERNAL_HEIGHT;
            let w: u32 = (h as f64 * aspect).round() as u32;
            let mut buf = granite_buf.borrow_mut();
            let need = (w as usize) * (h as usize) * 4;
            if buf.len() != need {
                buf.resize(need, 0);
            }
            let cfg = state_tick.borrow().config.visualizer.granite;
            state_tick.borrow_mut().player.render_granite(&mut buf, w, h, &cfg);
            let bytes = glib::Bytes::from(&buf[..]);
            let texture = gdk::MemoryTexture::new(
                w as i32,
                h as i32,
                gdk::MemoryFormat::R8g8b8a8,
                &bytes,
                (w * 4) as usize,
            );
            pic.set_paintable(Some(&texture));
        } else {
            if stack.visible_child_name().as_deref() != Some("cairo") {
                stack.set_visible_child_name("cairo");
            }
            c.queue_draw();
        }

        // FPS tracking. EMA on inter-frame ms; display rounded each ~10 ticks.
        if let Some(label) = fps_label_weak.upgrade() {
            let now = std::time::Instant::now();
            if let Some(prev) = last_instant.get() {
                let dt_ms = now.duration_since(prev).as_secs_f32() * 1000.0;
                let cur = ema_dt_ms.get();
                ema_dt_ms.set(cur * 0.9 + dt_ms * 0.1);
            }
            last_instant.set(Some(now));

            if label.is_visible() {
                let n = fps_update_countdown.get();
                if n == 0 {
                    let fps = if ema_dt_ms.get() > 0.0 { 1000.0 / ema_dt_ms.get() } else { 0.0 };
                    // BPM from the Granite beat detector; "--" until it locks.
                    // Same format as the macOS overlay.
                    let (bpm, meter) = {
                        let s = state_tick.borrow();
                        (s.player.granite_bpm(), s.player.granite_meter())
                    };
                    let bpm_str = if bpm > 0.0 {
                        format!("{bpm:.0}")
                    } else {
                        "--".to_string()
                    };
                    let meter_str = if meter > 0 {
                        format!(" ({meter}/4)")
                    } else {
                        String::new()
                    };
                    label.set_text(&format!("FPS: {fps:.0}   BPM: {bpm_str}{meter_str}"));
                    fps_update_countdown.set(10);
                } else {
                    fps_update_countdown.set(n - 1);
                }
            }
        }

        glib::ControlFlow::Continue
    });

    // ── Toast helpers ──────────────────────────────────────────────────────
    let toast_label = toast.clone();
    let toast_source: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));

    let show_toast = {
        let tl = toast_label.clone();
        let ts = toast_source.clone();
        Rc::new(move |msg: String| {
            tl.set_text(&msg);
            tl.set_visible(true);
            if let Some(id) = ts.take() {
                id.remove();
            }
            let tl2 = tl.clone();
            let ts2 = ts.clone();
            let id = glib::timeout_add_local(std::time::Duration::from_secs(3), move || {
                tl2.set_visible(false);
                ts2.set(None);
                glib::ControlFlow::Break
            });
            ts.set(Some(id));
        })
    };

    // ── Key bindings ───────────────────────────────────────────────────────
    let key_ctrl = EventControllerKey::new();
    key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);

    let fs_win_weak = fs_win.downgrade();
    let state_keys = state.clone();
    let show_toast_key = show_toast.clone();
    let fps_label_keys = fps_label.clone();

    key_ctrl.connect_key_pressed(move |_, key, _, _| {
        match key {
            gdk::Key::Escape => {
                if let Some(w) = fs_win_weak.upgrade() {
                    w.close();
                }
                glib::Propagation::Stop
            }
            // FPS overlay toggle
            gdk::Key::g | gdk::Key::G => {
                fps_label_keys.set_visible(!fps_label_keys.is_visible());
                glib::Propagation::Stop
            }
            // Jump window
            gdk::Key::j | gdk::Key::J => {
                gtk4::prelude::EditableExt::set_text(&jump_entry, "");
                rebuild_jump();
                jump_win.present();
                jump_entry.grab_focus();
                glib::Propagation::Stop
            }
            // Info / shortcuts window
            gdk::Key::i | gdk::Key::I => {
                btn_info.activate();
                glib::Propagation::Stop
            }
            // Random Granite effect — forward to the main-window handler.
            // The fullscreen window has its own key controller, so keys not
            // matched here never reach the main window.
            gdk::Key::e | gdk::Key::E => handle_key(key),
            // Transport + mode keys — pass through, then show toast
            gdk::Key::z
            | gdk::Key::x
            | gdk::Key::c
            | gdk::Key::v
            | gdk::Key::b
            | gdk::Key::r
            | gdk::Key::R
            | gdk::Key::s
            | gdk::Key::S => {
                let result = handle_key(key);
                let msg = {
                    let s = state_keys.borrow();
                    if let Some(track) = s.playlist.current() {
                        let ps = s.player.state().clone();
                        let verb = match ps {
                            PlayerState::Playing => "Playing",
                            PlayerState::Paused => "Paused",
                            PlayerState::Stopped => "Stopped",
                        };
                        format!("{}: {}", verb, track.display_name())
                    } else {
                        String::new()
                    }
                };
                if !msg.is_empty() {
                    show_toast_key(msg);
                }
                result
            }
            _ => glib::Propagation::Proceed,
        }
    });
    fs_win.add_controller(key_ctrl);

    // Keep the display awake while fullscreen, when configured. The
    // session manager auto-releases the inhibit if the app dies, so only
    // the orderly close path needs the explicit uninhibit.
    let inhibit_cookie: Rc<Cell<u32>> = Rc::new(Cell::new(0));
    if state.borrow().config.visualizer.keep_screen_awake {
        if let Some(app) = gtk4::gio::Application::default()
            .and_downcast::<gtk4::Application>()
        {
            let cookie = app.inhibit(
                Some(&fs_win),
                gtk4::ApplicationInhibitFlags::IDLE,
                Some("Fullscreen visualizer"),
            );
            inhibit_cookie.set(cookie);
        }
    }

    // Stop the 33 ms tick before the fullscreen window's surface is freed,
    // and let the display sleep again.
    let cookie_close = inhibit_cookie.clone();
    fs_win.connect_close_request(move |_| {
        fs_shutting_down.set(true);
        fs_viz_open.set(false);
        if cookie_close.get() != 0 {
            if let Some(app) = gtk4::gio::Application::default()
                .and_downcast::<gtk4::Application>()
            {
                app.uninhibit(cookie_close.get());
            }
            cookie_close.set(0);
        }
        glib::Propagation::Proceed
    });

    // ── Show fullscreen ────────────────────────────────────────────────────
    fs_win.present();
    fs_win.fullscreen();
}

// Image viewer popup
// ---------------------------------------------------------------------------

/// Open a resizable window displaying the image at `path`.
fn open_image_viewer(path: &str) {
    use gtk4::ContentFit;

    let exists = std::path::Path::new(path).exists();

    let win = gtk4::Window::new();
    win.set_title(Some("Artwork — Sparkamp"));
    win.set_default_size(400, 400);
    win.set_resizable(true);

    if !exists {
        // File missing — show an inline message instead of a blank window.
        let lbl = gtk4::Label::builder()
            .label(format!("Artwork file not found:\n{path}"))
            .halign(Align::Center)
            .valign(Align::Center)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .wrap(true)
            .build();
        win.set_child(Some(&lbl));
        win.present();
        return;
    }

    // Load via Gdk Texture so we can surface decode failures explicitly
    // instead of silently rendering a blank Picture.
    match gtk4::gdk::Texture::from_filename(path) {
        Ok(tex) => {
            let picture = gtk4::Picture::for_paintable(&tex);
            picture.set_can_shrink(true);
            picture.set_content_fit(ContentFit::Contain);
            picture.set_hexpand(true);
            picture.set_vexpand(true);
            win.set_child(Some(&picture));
        }
        Err(e) => {
            let lbl = gtk4::Label::builder()
                .label(format!("Could not decode artwork:\n{e}"))
                .halign(Align::Center)
                .valign(Align::Center)
                .margin_top(24)
                .margin_bottom(24)
                .margin_start(24)
                .margin_end(24)
                .wrap(true)
                .build();
            win.set_child(Some(&lbl));
        }
    }
    win.present();
}

/// Filesystems Sparkamp can't reliably read/write yet — shown with a warning.
fn device_fs_unsupported(fs_type: &str) -> bool {
    crate::devices::plan::device_fs_unsupported(fs_type)
}

/// Whether a udisks volume is optical media (a mounted data CD/DVD). These
/// belong to the Disc Drives group, not the removable-Devices list, so the
/// device poll filters them out. `iso9660`/`udf` are the optical data
/// filesystems; audio CDs have no filesystem and never reach the device list.
fn is_optical_fs(fs_type: &str) -> bool {
    matches!(fs_type.to_ascii_lowercase().as_str(), "iso9660" | "udf")
}

/// Case-insensitive substring match of a per-view search query against a
/// track's visible text fields — the in-memory counterpart of the Files
/// view's DB-backed search, used by the playlist-editor and device views.
/// `q` must already be lowercased; an empty query matches everything.
fn lib_track_matches_query(t: &crate::media_library::LibTrack, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    let has = |s: &Option<String>| s.as_deref().map(|v| v.to_lowercase().contains(q)).unwrap_or(false);
    has(&t.title)
        || has(&t.artist)
        || has(&t.album)
        || has(&t.genre)
        || t.filename.to_lowercase().contains(q)
}

/// A search entry + ✕ clear button row, styled like the Files view's search
/// bar. Returns `(row, entry)`; the caller wires `connect_changed`.
fn make_view_search_row(placeholder: &str) -> (GtkBox, Entry) {
    let entry = Entry::new();
    entry.set_placeholder_text(Some(placeholder));
    entry.set_hexpand(true);
    let clear = Button::with_label("✕");
    clear.add_css_class("pl-btn");
    {
        let e = entry.clone();
        clear.connect_clicked(move |_| e.set_text(""));
    }
    let row = GtkBox::new(Orientation::Horizontal, 4);
    row.set_margin_top(4);
    row.set_margin_start(4);
    row.set_margin_end(4);
    row.append(&entry);
    row.append(&clear);
    (row, entry)
}

/// Leading status glyphs for a device label: ⚠ for an unsupported filesystem,
/// 🔒 for read-only (matching the read-only file convention).
fn device_glyph_prefix(read_only: bool, fs_type: &str) -> String {
    let mut p = String::new();
    if device_fs_unsupported(fs_type) {
        p.push_str("⚠ ");
    }
    if read_only {
        p.push_str("🔒 ");
    }
    p
}

/// Themed icon name for a device card. Generic removable-media icon for now;
/// the MTP backend (Android phones) will map to a phone icon when added.
fn device_icon_name(_dev: &crate::devices::Device) -> &'static str {
    "drive-removable-media"
}

/// Apply a copy's progress to an overview card's bar. `Some((done, total))`
/// shows the bar with an `x/y` label and fraction; `None` makes it transparent
/// (idle) while still reserving its space, so the card never changes height.
fn apply_card_progress(bar: &gtk4::ProgressBar, state: Option<(usize, usize)>) {
    match state {
        Some((done, total)) => {
            bar.set_opacity(1.0);
            bar.set_text(Some(&format!("{done}/{total}")));
            bar.set_fraction(done as f64 / total.max(1) as f64);
        }
        None => bar.set_opacity(0.0),
    }
}

/// Color a capacity LevelBar by fullness: normal < 75%, `cap-warn` ≥ 75%,
/// `cap-full` ≥ 90%. The classes are styled in `skin.rs`.
fn set_levelbar_fullness(bar: &gtk4::LevelBar, used: f64) {
    bar.remove_css_class("cap-ok");
    bar.remove_css_class("cap-warn");
    bar.remove_css_class("cap-full");
    // Thresholds are on *free* space: red under 5% free, amber under 15% free,
    // accent/blue otherwise. Exactly one class is set so every capacity bar
    // reads the same color across the sidebar, overview, and detail views.
    let free = 1.0 - used;
    if free < 0.05 {
        bar.add_css_class("cap-full");
    } else if free < 0.15 {
        bar.add_css_class("cap-warn");
    } else {
        bar.add_css_class("cap-ok");
    }
}

/// Toggle a button into a "working" state: a running spinner replaces its label
/// and it goes insensitive, restored to `idle_label` when done. Used so the
/// Sync button shows activity during the (sometimes slow over MTP) device
/// communication before the sync dialog appears.
fn set_button_busy(btn: &Button, busy: bool, idle_label: &str) {
    if busy {
        let spinner = gtk4::Spinner::new();
        spinner.start();
        btn.set_child(Some(&spinner));
        btn.set_sensitive(false);
    } else {
        btn.set_label(idle_label);
        btn.set_sensitive(true);
    }
}

/// Resolve an MTP device's writable **storage root** under its gvfs FUSE mount.
///
/// The mtp:// mount root's children are storage volumes (e.g. "Internal shared
/// storage", "SD card"), and Android rejects files written at the device root —
/// they must live inside a storage. So the device's `mount_path` is set to a
/// storage dir, keeping the flat `Music/<file>` layout valid. Prefers a storage
/// that already has a `Music` folder, then one whose name looks "internal", else
/// the first. Cached per device URI so the poll doesn't `read_dir` every tick;
/// the cache self-heals if the path goes stale (replug).
fn mtp_storage_root(uri: &str, fuse_root: &std::path::Path) -> std::path::PathBuf {
    // Thread-safe cache: this is called from a worker thread (off the UI thread)
    // so the FUSE read_dir never blocks the main loop.
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::path::PathBuf>>,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Some(p) = cache.lock().unwrap().get(uri).cloned() {
        return p;
    }
    let mut chosen = fuse_root.to_path_buf();
    if let Ok(entries) = std::fs::read_dir(fuse_root) {
        let dirs: Vec<std::path::PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .collect();
        chosen = dirs
            .iter()
            .find(|d| d.join("Music").exists())
            .or_else(|| {
                dirs.iter().find(|d| {
                    d.file_name()
                        .map(|n| n.to_string_lossy().to_lowercase().contains("internal"))
                        .unwrap_or(false)
                })
            })
            .or_else(|| dirs.first())
            .cloned()
            .unwrap_or_else(|| fuse_root.to_path_buf());
    }
    // Only cache a real storage — not the device-root fallback (which happens
    // in charge-only mode), so switching the phone to file mode re-resolves.
    if chosen != fuse_root {
        cache.lock().unwrap().insert(uri.to_string(), chosen.clone());
    }
    chosen
}

/// Detect MTP devices (Android phones in File-transfer mode) via gio's
/// `VolumeMonitor`. These are surfaced by gvfs as `mtp://` mounts with a FUSE
/// path under `/run/user/<uid>/gvfs/`. Produces core [`Device`] structs tagged
/// `DeviceBackend::Mtp`; the udisks2 detection path never sees them.
///
/// Must run on the main thread (VolumeMonitor is a GLib main-context object).
/// A device without a local FUSE path is skipped — `PosixIo` can't browse it
/// until the gio IO backend (later phase) lands.
struct MtpRaw {
    uri: String,
    fuse_root: std::path::PathBuf,
    label: String,
    id: String,
    ejectable: bool,
}

/// Enumerate MTP mount *metadata* via gio's VolumeMonitor. Cheap, no filesystem
/// IO (so safe to run on the main thread): only cached GLib mount/volume props
/// and URI→path mapping. The FUSE `read_dir` to find the storage root is done
/// later, off-thread, by [`mtp_raw_to_device`].
fn enumerate_mtp_raw() -> Vec<MtpRaw> {
    let monitor = gio::VolumeMonitor::get();
    // gvfs can expose one MTP device as several mounts sharing the same root URI
    // (a friendly "Pixel 8" plus a generic "mtp"). Dedup by URI, best label wins.
    let mut by_uri: std::collections::HashMap<String, MtpRaw> = std::collections::HashMap::new();
    for mount in monitor.mounts() {
        let root = mount.root();
        let uri = root.uri().to_string();
        if !uri.starts_with("mtp://") && !uri.starts_with("gphoto2://") {
            continue;
        }
        let Some(fuse_root) = root.path() else {
            continue;
        };
        let mount_name = mount.name().to_string();
        let vol_name = mount.volume().map(|v| v.name().to_string()).unwrap_or_default();
        let label = if !mount_name.is_empty() && mount_name != "mtp" {
            mount_name
        } else if !vol_name.is_empty() {
            vol_name
        } else {
            "MTP device".to_string()
        };
        let id = mount
            .uuid()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| uri.clone());
        let raw = MtpRaw {
            uri: uri.clone(),
            fuse_root,
            label,
            id,
            ejectable: mount.can_eject() || mount.can_unmount(),
        };
        match by_uri.get(&uri) {
            Some(existing) if existing.label != "MTP device" => {}
            _ => {
                by_uri.insert(uri, raw);
            }
        }
    }
    by_uri.into_values().collect()
}

/// Resolve one [`MtpRaw`] into a [`Device`]. Runs on a worker thread because it
/// does FUSE `read_dir`s (via [`mtp_storage_root`]) to point `mount_path` at the
/// device's writable storage root.
///
/// Returns `None` for a **dead** mount — a gvfs entry left behind in
/// VolumeMonitor after the phone was unplugged, whose FUSE root can no longer be
/// read. Dropping it keeps a phantom "MTP device" out of the sidebar when
/// nothing is actually connected.
///
/// Returns a device with `fs_visible == false` when the phone is connected but
/// exposes no readable storage volume (file transfer not authorized, or the
/// storage hasn't appeared yet) — the detail view then shows a reconnect banner
/// instead of empty lists.
/// Set true once the main window starts closing. Worker-thread device code
/// checks it before starting any blocking gvfs/MTP FUSE work (directory reads,
/// capacity queries, tag scans): such a read can block in uninterruptible IO on
/// a slow/wedged device, pinning the thread and delaying process exit and
/// Ctrl-C. Not *starting* the read avoids that. (An already in-flight read can't
/// be cancelled — that case is inherent to FUSE.)
static DEVICE_IO_SHUTDOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn device_io_shutting_down() -> bool {
    DEVICE_IO_SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Cached MTP device metadata, filled by the one-time FUSE reads in
/// [`mtp_raw_to_device`] the first time a device URI is seen. Steady-state
/// polling reuses it and NEVER touches the gvfs mount: issuing a blocking,
/// uncancellable FUSE read every 2 s would, on a slow or post-sync-wedged
/// device, hold the mount busy (blocking eject from Sparkamp and GNOME) and pin
/// a worker thread in uninterruptible IO — delaying process exit and Ctrl-C.
/// Invalidated by [`invalidate_mtp_meta`] after any operation that changes the
/// device, so capacity/visibility refresh then rather than on a timer.
struct MtpMeta {
    storage_root: std::path::PathBuf,
    has_storage: bool,
    total_bytes: u64,
    free_bytes: u64,
}

fn mtp_meta_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, MtpMeta>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, MtpMeta>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Drop a device's cached MTP metadata so the next poll re-reads it once — e.g.
/// after a copy/sync/delete changed its contents, or on eject. No-op for
/// non-MTP backend ids (their URIs are never cached here).
fn invalidate_mtp_meta(uri: &str) {
    mtp_meta_cache().lock().unwrap().remove(uri);
}

fn mtp_device_from_meta(raw: &MtpRaw, m: &MtpMeta) -> crate::devices::Device {
    use crate::devices::DeviceBackend;
    crate::devices::Device {
        id: raw.id.clone(),
        label: raw.label.clone(),
        mount_path: m.storage_root.clone(),
        fs_type: "mtp".to_string(),
        total_bytes: m.total_bytes,
        free_bytes: m.free_bytes,
        read_only: false,
        ejectable: raw.ejectable,
        backend_id: raw.uri.clone(),
        backend: DeviceBackend::Mtp,
        fs_visible: m.has_storage,
    }
}

/// Whether a gvfs URI belongs to an Apple device (iPad/iPhone). gphoto2 URIs
/// for Apple hardware embed the vendor, e.g.
/// `gphoto2://Apple_Inc._iPad_00008020.../`.
fn is_apple_device_uri(uri: &str) -> bool {
    uri.to_lowercase().contains("apple")
}

/// Banner text for a device Sparkamp can't sync to. Apple devices get the
/// iOS-specific guidance; everything else on `gphoto2://` is a phone in
/// photo-transfer (PTP) mode that should be switched to file-transfer/MTP.
fn unsupported_device_banner(uri: &str) -> &'static str {
    if is_apple_device_uri(uri) {
        "⚠ iPad / iPhone detected. iOS doesn't allow third-party music transfer — \
         use Apple Music or Finder to add songs. Sparkamp can't sync to this device."
    } else {
        "⚠ Device is in photo-transfer (PTP) mode. Switch it to File Transfer / MTP \
         mode to sync music, then reconnect."
    }
}

fn mtp_raw_to_device(raw: MtpRaw) -> Option<crate::devices::Device> {
    // gphoto2:// mounts are photo-transfer (PTP) interfaces: read-only, camera
    // roll only. Apple devices and Android-in-photo-mode both land here. They
    // are surfaced so the user sees the device is detected, but tagged
    // Unsupported (NullIo) and never offered as a sync target. Built directly,
    // with no FUSE/capacity reads — there is nothing useful to read.
    if raw.uri.starts_with("gphoto2://") {
        use crate::devices::DeviceBackend;
        return Some(crate::devices::Device {
            id: raw.id.clone(),
            label: raw.label.clone(),
            mount_path: raw.fuse_root.clone(),
            fs_type: if is_apple_device_uri(&raw.uri) { "ios" } else { "ptp" }.to_string(),
            total_bytes: 0,
            free_bytes: 0,
            read_only: true,
            ejectable: raw.ejectable,
            backend_id: raw.uri.clone(),
            backend: DeviceBackend::Unsupported,
            fs_visible: false,
        });
    }
    // Cache hit → no FUSE IO at all. This is the steady-state path on every
    // 2 s poll once a device has been seen, so a slow/wedged mount can never
    // block the poll worker or hold the mount busy in the background.
    if let Some(m) = mtp_meta_cache().lock().unwrap().get(&raw.uri) {
        return Some(mtp_device_from_meta(&raw, m));
    }
    // Don't begin first-detect FUSE reads while shutting down.
    if device_io_shutting_down() {
        return None;
    }
    // Cache miss (first sight of this URI): do the one-time FUSE reads.
    // Accessibility gate: an unplugged phone's stale mount still lists in
    // VolumeMonitor, but its FUSE root errors on read — treat as "not
    // connected" and drop it.
    let Ok(entries) = std::fs::read_dir(&raw.fuse_root) else {
        return None;
    };
    // At least one storage-volume directory present? (Internal storage / SD
    // card.) Its absence is the "connected but no visible filesystem" case.
    let has_storage = entries
        .flatten()
        .any(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false));
    let storage_root = mtp_storage_root(&raw.uri, &raw.fuse_root);
    // Capacity via gio (gvfs FUSE rarely reports statvfs). Safe here — this runs
    // on a worker thread, so the blocking query never freezes the UI.
    let (total_bytes, free_bytes) = gio::File::for_uri(&raw.uri)
        .query_filesystem_info("filesystem::size,filesystem::free", gio::Cancellable::NONE)
        .map(|info| {
            (
                info.attribute_uint64("filesystem::size"),
                info.attribute_uint64("filesystem::free"),
            )
        })
        .unwrap_or((0, 0));
    let meta = MtpMeta {
        storage_root,
        has_storage,
        total_bytes,
        free_bytes,
    };
    let dev = mtp_device_from_meta(&raw, &meta);
    mtp_meta_cache().lock().unwrap().insert(raw.uri.clone(), meta);
    Some(dev)
}

/// "N songs · M playlists" with singular/plural agreement.
fn counts_text(songs: usize, playlists: usize) -> String {
    format!(
        "{songs} song{} · {playlists} playlist{}",
        if songs == 1 { "" } else { "s" },
        if playlists == 1 { "" } else { "s" },
    )
}

/// Tooltip shown on the device row / detail for an unsupported filesystem.
const UNSUPPORTED_FS_TOOLTIP: &str =
    "Unsupported filesystem (NTFS/exFAT) — Sparkamp can't reliably read or write this device yet.";

/// Device identity for sync pairs: the volume UUID, or a marker id written now
/// (the first time a file is paired to this device).
fn device_sync_id(dev: &crate::devices::Device) -> String {
    crate::devices::plan::device_sync_id(dev)
}

/// The DB half of [`device_plan_one`]: the recorded sync-pair device relpath for
/// `src` on this device, if any. Frontend shim over
/// [`crate::devices::plan::recorded_relpath`] that pulls the open library.
fn device_recorded_relpath(
    state: &Rc<RefCell<AppState>>,
    device_id: &str,
    src: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let s = state.borrow();
    let lib = s.media_lib.as_ref()?;
    crate::devices::plan::recorded_relpath(lib, device_id, src)
}

/// The filesystem half of [`device_plan_one`]: given the recorded relpath (from
/// [`device_recorded_relpath`]), decide the final relpath and whether the file
/// is already present, using `metadata`/`exists` checks on the device. This is
/// the part that can be slow over a gvfs/MTP FUSE mount, so callers run it on a
/// worker thread.
fn device_plan_fs(
    mount: &std::path::Path,
    src: &std::path::Path,
    recorded: Option<std::path::PathBuf>,
) -> (std::path::PathBuf, bool) {
    crate::devices::plan::device_plan_fs(mount, src, recorded)
}

/// Decide where `src` goes on the device and whether it's already there.
///
/// Resolution order, all yielding the canonical flat `Music/<filename>` layout:
/// 1. A recorded sync pair whose device file still exists *and* matches the
///    current flat layout → reuse it (so editing metadata never duplicates).
/// 2. An identical file (same name, same size) already at `Music/<filename>` →
///    treat as present, so a lost/mismatched pair can't spawn a `-N` duplicate.
/// 3. A *different* file occupying `Music/<filename>` → `-N` collision suffix.
/// 4. Otherwise the free `Music/<filename>` slot.
///
/// Does filesystem IO; on a slow (MTP) device prefer the split
/// [`device_recorded_relpath`] (main thread) + [`device_plan_fs`] (worker).
fn device_plan_one(
    state: &Rc<RefCell<AppState>>,
    mount: &std::path::Path,
    device_id: &str,
    src: &std::path::Path,
) -> (std::path::PathBuf, bool) {
    device_plan_fs(mount, src, device_recorded_relpath(state, device_id, src))
}

/// Record (or refresh) the sync pair for a just-copied file with its REAL tag
/// baseline, so a later sync sees no change until a tag is actually edited.
fn device_record_pair(
    state: &Rc<RefCell<AppState>>,
    device_id: &str,
    src: &std::path::Path,
    relpath: &std::path::Path,
) {
    if let Some(lib) = state.borrow().media_lib.as_ref() {
        crate::devices::plan::record_pair(lib, device_id, src, relpath);
    }
}

/// Sanitize a playlist name into the bare filename stem used for its `.m3u`/
/// `.m3u8` on a device: strip path-hostile characters and surrounding dots/
/// spaces, falling back to "Playlist" when nothing usable remains.
fn safe_playlist_filename(name: &str) -> String {
    crate::devices::plan::safe_playlist_filename(name)
}

/// If a device playlist file is linked to a library playlist — i.e. some
/// library playlist's safe filename equals the device file's stem — return its
/// `(id, name)`. Device-only playlists (no library match) return `None`.
fn linked_library_playlist(
    state: &Rc<RefCell<AppState>>,
    dev_playlist: &std::path::Path,
) -> Option<(i64, String)> {
    let s = state.borrow();
    let lib = s.media_lib.as_ref()?;
    crate::devices::plan::linked_library_playlist(lib, dev_playlist)
}

/// A validated plan for sending a whole playlist to a device: the files to
/// copy (with their on-device paths), the device identity for sync pairs, and
/// where the `.m3u8` will be written on the device.
struct PlaylistSendPlan {
    srcs: Vec<std::path::PathBuf>,
    device_id: String,
    m3u_path: std::path::PathBuf,
}

/// Validate and build a [`PlaylistSendPlan`] for `playlist_id` on `dev`, or a
/// user-facing error (read-only / unsupported device, empty playlist, no space).
fn prepare_playlist_send(
    state: &Rc<RefCell<AppState>>,
    dev: &crate::devices::Device,
    playlist_id: i64,
    playlist_name: &str,
) -> Result<PlaylistSendPlan, String> {
    if dev.read_only {
        let n = if dev.label.is_empty() { "This device" } else { &dev.label };
        return Err(format!("{n} is read-only — can't copy files to it."));
    }
    if device_fs_unsupported(&dev.fs_type) {
        return Err(format!(
            "{} is an unsupported filesystem — can't write to this device yet.",
            dev.fs_type
        ));
    }
    let tracks = {
        let s = state.borrow();
        s.media_lib
            .as_ref()
            .and_then(|lib| {
                lib.playlist_by_id(playlist_id)
                    .ok()
                    .and_then(|pl| lib.load_playlist_tracks(&pl).ok())
            })
            .unwrap_or_default()
    };
    let srcs: Vec<std::path::PathBuf> = tracks
        .iter()
        .map(|t| std::path::PathBuf::from(&t.path))
        .filter(|p| p.exists())
        .collect();
    if srcs.is_empty() {
        return Err("No playable files in this playlist.".to_string());
    }
    let device_id = device_sync_id(dev);
    // Free-space guard — only when capacity is known (0 = unknown, e.g. MTP).
    // Skipping it avoids a whole pass of slow per-file device checks on devices
    // that can't report free space anyway.
    if dev.free_bytes > 0 {
        let mut need = 0u64;
        for src in &srcs {
            if !device_plan_one(state, &dev.mount_path, &device_id, src).1 {
                need += std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
            }
        }
        if need > dev.free_bytes {
            return Err(format!(
                "Not enough space on the device: need {:.1} GB, {:.1} GB free.",
                need as f64 / 1e9,
                dev.free_bytes as f64 / 1e9
            ));
        }
    }
    let safe = safe_playlist_filename(playlist_name);
    let ext = state
        .borrow()
        .config
        .media_library
        .playlist_format
        .extension();
    let m3u_path = dev.mount_path.join(format!("{safe}.{ext}"));
    Ok(PlaylistSendPlan {
        srcs,
        device_id,
        m3u_path,
    })
}

/// Compute the per-pair sync decisions for a device: for each recorded sync
/// pair, hash the current tags on each side and decide the direction.
fn device_sync_plan(
    lib: &crate::media_library::MediaLibrary,
    dev: &crate::devices::Device,
) -> Vec<(crate::media_library::SyncPair, crate::devices::sync::SyncAction)> {
    crate::devices::plan::device_sync_plan(lib, dev)
}

/// Apply one tag-sync direction to a single pair and refresh its baseline.
/// `to_device` true = library→device, false = device→library. Returns ok.
fn apply_tag_pair(
    state: &Rc<RefCell<AppState>>,
    dev: &crate::devices::Device,
    pair: &crate::media_library::SyncPair,
    to_device: bool,
) -> bool {
    match state.borrow().media_lib.as_ref() {
        Some(lib) => crate::devices::plan::apply_tag_pair(lib, dev, pair, to_device),
        None => false,
    }
}

/// Apply a sync plan: propagate the winning side's tags for the unambiguous
/// directions (conflicts are handled separately by the prompt) and refresh each
/// pair's baseline. Returns `(applied, failed)`.
fn apply_device_sync(
    state: &Rc<RefCell<AppState>>,
    dev: &crate::devices::Device,
    plan: &[(crate::media_library::SyncPair, crate::devices::sync::SyncAction)],
) -> (usize, usize) {
    match state.borrow().media_lib.as_ref() {
        Some(lib) => crate::devices::plan::apply_device_sync(lib, dev, plan),
        None => (0, 0),
    }
}

/// Build the two-way playlist sync plan for a device: for each library playlist
/// that is on the device (or was, per a stored baseline), decide whether to
/// push to the device, pull into the library, or flag a conflict.
fn device_playlist_sync_plan(
    lib: &crate::media_library::MediaLibrary,
    dev: &crate::devices::Device,
    ext: &str,
) -> Vec<PlaylistSyncItem> {
    crate::devices::plan::device_playlist_sync_plan(lib, dev, ext)
}

/// Push a library playlist to the device: copy any missing tracks (flat
/// `Music/<file>`, deduped), rewrite the device `.m3u8`, drop the old device
/// file if the playlist was renamed, and refresh the baseline. Audio files for
/// tracks removed from the playlist stay on the device (Deletion Rule).
/// Returns `(files_copied, ok)`.
fn apply_playlist_push(
    state: &Rc<RefCell<AppState>>,
    dev: &crate::devices::Device,
    item: &PlaylistSyncItem,
) -> (usize, bool) {
    match state.borrow().media_lib.as_ref() {
        Some(lib) => crate::devices::plan::apply_playlist_push(lib, dev, item),
        None => (0, false),
    }
}

/// Prompt the user to resolve playlist conflicts one at a time (both sides
/// changed). Each prompt shows how many entries differ; the user keeps the
/// computer's copy (push), the device's copy (pull), or skips. After the last
/// one, `done` runs (refresh + summary).
/// Prompt the user to resolve per-file tag conflicts one at a time. Each prompt
/// lists the differing fields (computer vs device); the user keeps the computer
/// copy (library→device), the device copy (device→library), or skips. After the
/// last one, `done` runs.
fn prompt_tag_conflicts(
    state: Rc<RefCell<AppState>>,
    dev: crate::devices::Device,
    mut conflicts: Vec<TagConflictItem>,
    win_wk: glib::WeakRef<gtk4::Window>,
    done: Rc<dyn Fn()>,
) {
    let Some(item) = conflicts.pop() else {
        (done)();
        return;
    };
    let mut detail = String::new();
    for d in &item.diffs {
        let comp = if d.computer.is_empty() { "(empty)" } else { &d.computer };
        let dev_v = if d.device.is_empty() { "(empty)" } else { &d.device };
        detail.push_str(&format!("{}:\n   This computer: {comp}\n   On device: {dev_v}\n", d.label));
    }
    let dialog = gtk4::AlertDialog::builder()
        .message(format!("\"{}\" changed on both sides", item.song))
        .detail(detail.trim_end().to_string())
        .buttons(vec![
            "Skip".to_string(),
            "Keep device".to_string(),
            "Keep computer".to_string(),
        ])
        .cancel_button(0)
        .default_button(2)
        .modal(true)
        .build();
    dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |res| {
        match res {
            Ok(2) => {
                apply_tag_pair(&state, &dev, &item.pair, true); // keep computer → library→device
            }
            Ok(1) => {
                apply_tag_pair(&state, &dev, &item.pair, false); // keep device → device→library
            }
            _ => {} // Skip — leave both sides, no baseline update.
        }
        prompt_tag_conflicts(state.clone(), dev.clone(), conflicts, win_wk.clone(), done.clone());
    });
}

/// Build the per-file tag-conflict items from a sync plan: for each pair marked
/// `Conflict`, read both sides' tags and compute the differing fields.
fn build_tag_conflicts(
    dev: &crate::devices::Device,
    plan: &[(crate::media_library::SyncPair, crate::devices::sync::SyncAction)],
) -> Vec<TagConflictItem> {
    crate::devices::plan::build_tag_conflicts(dev, plan)
}

fn prompt_playlist_conflicts(
    state: Rc<RefCell<AppState>>,
    dev: crate::devices::Device,
    mut conflicts: Vec<PlaylistSyncItem>,
    win_wk: glib::WeakRef<gtk4::Window>,
    done: Rc<dyn Fn()>,
) {
    let Some(item) = conflicts.pop() else {
        (done)();
        return;
    };
    let dialog = gtk4::AlertDialog::builder()
        .message(format!("\"{}\" changed on both sides", item.library_name))
        .detail(format!(
            "{} file{} differ between this computer and the device. Which copy do you want to keep?",
            item.differ,
            if item.differ == 1 { "" } else { "s" }
        ))
        .buttons(vec![
            "Skip".to_string(),
            "Keep device".to_string(),
            "Keep computer".to_string(),
        ])
        .cancel_button(0)
        .default_button(2)
        .modal(true)
        .build();
    dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |res| {
        match res {
            Ok(2) => {
                apply_playlist_push(&state, &dev, &item);
            }
            Ok(1) => {
                apply_playlist_pull(&state, &item);
            }
            _ => {} // Skip — leave both sides as-is (no baseline update).
        }
        prompt_playlist_conflicts(state.clone(), dev.clone(), conflicts, win_wk.clone(), done.clone());
    });
}

/// Pull a device playlist into the library: rewrite the library playlist file to
/// mirror the device's order/membership (mapping device filenames back to
/// library tracks by filename), then refresh the baseline. Returns ok.
fn apply_playlist_pull(
    state: &Rc<RefCell<AppState>>,
    item: &PlaylistSyncItem,
) -> bool {
    match state.borrow().media_lib.as_ref() {
        Some(lib) => crate::devices::plan::apply_playlist_pull(lib, item),
        None => false,
    }
}

/// Rewrite a device `.m3u`/`.m3u8`, dropping every track line whose filename
/// (basename of the entry, `/` or `\` separated) is in `remove`. Comment/blank
/// lines are preserved. Returns true if the file changed.
fn device_m3u_remove_basenames(
    path: &std::path::Path,
    remove: &std::collections::HashSet<String>,
) -> bool {
    crate::devices::plan::device_m3u_remove_basenames(path, remove)
}

/// Delete files from a device and remove them from every device playlist that
/// referenced them. `paths` are absolute on-device paths. Returns the number of
/// files that couldn't be deleted.
fn device_delete_files(dev: &crate::devices::Device, paths: &[std::path::PathBuf]) -> usize {
    crate::devices::plan::device_delete_files(dev, paths)
}

fn open_media_library_window(
    parent: Option<&gtk4::Window>,
    state: Rc<RefCell<AppState>>,
    rebuild_playlist: Rc<dyn Fn()>,
    set_track: Rc<dyn Fn(&str)>,
    init_width: i32,
    init_height: i32,
) -> gtk4::Window {
    let win = gtk4::Window::new();
    win.set_title(Some("Media Library — Sparkamp"));
    win.set_default_size(init_width, init_height);
    win.set_resizable(true);
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }

    let paned = Paned::new(Orientation::Horizontal);
    paned.set_margin_top(8);
    paned.set_margin_bottom(8);
    paned.set_margin_start(8);
    paned.set_margin_end(8);

    // ── Left sidebar ──────────────────────────────────────────────────────
    // Wrap sidebar in a ScrolledWindow so many playlists don't overflow.
    let sidebar = ListBox::new();
    sidebar.set_selection_mode(gtk4::SelectionMode::Single);
    sidebar.add_css_class("ml-sidebar");
    sidebar.set_vexpand(true);

    // Latest detected devices — declared here (ahead of the sidebar DropTarget,
    // which routes drops onto device rows) and kept current by the poll below.
    let current_devices: Rc<RefCell<Vec<crate::devices::Device>>> =
        Rc::new(RefCell::new(Vec::new()));

    // Per-device (song, playlist) counts for the overview cards, keyed by
    // backend_id. Computed off-thread on first show and cleared whenever a
    // device's contents change (see reload_device_store). `counts_in_flight`
    // guards against spawning the same count walk twice.
    let device_counts: Rc<RefCell<std::collections::HashMap<String, (usize, usize)>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    let counts_in_flight: Rc<RefCell<std::collections::HashSet<String>>> =
        Rc::new(RefCell::new(std::collections::HashSet::new()));

    // Live copy progress per device (backend_id → (done, total)); absent = idle.
    // `device_card_progress` maps a backend_id to its overview card's progress
    // bar (rebuilt each overview render). Together they let a copy show progress
    // on the card and survive a poll-driven rebuild mid-transfer.
    let device_transfers: Rc<RefCell<std::collections::HashMap<String, (usize, usize)>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    let device_card_progress: Rc<RefCell<std::collections::HashMap<String, gtk4::ProgressBar>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));

    // Apply (or clear) a transfer's progress to a card's bar. The bar always
    // occupies its space; idle just makes it transparent so the card never
    // changes size between copying and not.
    let update_card_progress: Rc<dyn Fn(&str, Option<(usize, usize)>)> = {
        let transfers = device_transfers.clone();
        let bars = device_card_progress.clone();
        Rc::new(move |backend: &str, state: Option<(usize, usize)>| {
            match state {
                Some(v) => {
                    transfers.borrow_mut().insert(backend.to_string(), v);
                }
                None => {
                    transfers.borrow_mut().remove(backend);
                }
            }
            if let Some(bar) = bars.borrow().get(backend) {
                apply_card_progress(bar, state);
            }
        })
    };

    // Sidebar DropTarget — accept FileList drags from the active playlist,
    // ML files view, or ML editor and append paths to the saved playlist
    // whose `pl:<id>` row is under the drop coordinate.  Drops landing on
    // the Files/Playlists header rows fall through to no-op.
    // Deferred handle to the playlist-send runner (defined later, in the
    // device-view section). Lets the sidebar drop handler send a playlist
    // dragged onto a device row.
    let send_playlist_holder: Rc<
        RefCell<Option<Rc<dyn Fn(crate::devices::Device, i64, String)>>>,
    > = Rc::new(RefCell::new(None));
    // Deferred handle to the file-copy runner (defined later, with the device
    // detail widgets it needs for the progress bar). Lets the sidebar drop
    // handler copy dragged files to a device asynchronously with progress.
    let copy_files_holder: Rc<
        RefCell<Option<Rc<dyn Fn(crate::devices::Device, Vec<std::path::PathBuf>)>>>,
    > = Rc::new(RefCell::new(None));
    {
        let dt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        dt.set_types(&[gdk::FileList::static_type(), glib::Type::STRING]);
        let sidebar_for_drop = sidebar.clone();
        let state_for_drop   = state.clone();
        let current_devices_drop = current_devices.clone();
        let send_holder_drop = send_playlist_holder.clone();
        let copy_holder_drop = copy_files_holder.clone();
        dt.connect_drop(move |_, value, _x, y| {
            // Locate the sidebar row under the drop coordinate.
            let mut hit: Option<ListBoxRow> = None;
            let mut i = 0i32;
            while let Some(r) = sidebar_for_drop.row_at_index(i) {
                if let Some(b) = r.compute_bounds(&sidebar_for_drop) {
                    if y as f32 >= b.y() && y as f32 <= b.y() + b.height() {
                        hit = Some(r);
                        break;
                    }
                }
                i += 1;
            }
            let Some(row) = hit else { return false };
            let name = row.widget_name().to_string();

            // Resolve the drag payload. A playlist row drags a `pl:<id>`
            // String. Track drags ship a FileList — but when the drop target
            // also advertises STRING (it does, for `pl:`), GTK may instead
            // deliver the FileList as a text/uri-list String. Handle both so a
            // drag from the active playlist works regardless of which format
            // gets negotiated.
            enum Payload {
                Playlist(i64),
                Files(Vec<std::path::PathBuf>),
            }
            let payload = if let Ok(s) = value.get::<String>() {
                if let Some(pid) = s.strip_prefix("pl:").and_then(|n| n.trim().parse::<i64>().ok())
                {
                    Payload::Playlist(pid)
                } else {
                    // A newline-separated uri-list or path-list.
                    let paths: Vec<std::path::PathBuf> = s
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty() && !l.starts_with('#'))
                        .map(|l| {
                            if l.starts_with("file://") {
                                gio::File::for_uri(l)
                                    .path()
                                    .unwrap_or_else(|| std::path::PathBuf::from(l))
                            } else {
                                std::path::PathBuf::from(l)
                            }
                        })
                        .collect();
                    if paths.is_empty() {
                        return false;
                    }
                    Payload::Files(paths)
                }
            } else if let Ok(file_list) = value.get::<gdk::FileList>() {
                let paths: Vec<std::path::PathBuf> =
                    file_list.files().iter().filter_map(|f| f.path()).collect();
                if paths.is_empty() {
                    return false;
                }
                Payload::Files(paths)
            } else {
                return false;
            };

            match payload {
                // Playlist dropped onto a device row → send files + .m3u8.
                Payload::Playlist(pid) => {
                    let Some(backend) = name.strip_prefix("dev:") else {
                        return false;
                    };
                    let Some(dev) = current_devices_drop
                        .borrow()
                        .iter()
                        .find(|d| d.backend_id == backend)
                        .cloned()
                    else {
                        return false;
                    };
                    let plname = state_for_drop
                        .borrow()
                        .media_lib
                        .as_ref()
                        .and_then(|l| l.playlist_by_id(pid).ok())
                        .map(|p| p.name)
                        .unwrap_or_default();
                    if let Some(send) = send_holder_drop.borrow().as_ref() {
                        send(dev, pid, plname);
                        return true;
                    }
                    false
                }
                Payload::Files(srcs) => {
                    // Onto a device row → copy the files (async, with progress).
                    if let Some(backend) = name.strip_prefix("dev:") {
                        let Some(dev) = current_devices_drop
                            .borrow()
                            .iter()
                            .find(|d| d.backend_id == backend)
                            .cloned()
                        else {
                            return false;
                        };
                        if let Some(copy) = copy_holder_drop.borrow().as_ref() {
                            copy(dev, srcs);
                            return true;
                        }
                        return false;
                    }
                    // Onto a saved-playlist row → append the files to it.
                    let Some(pid) =
                        name.strip_prefix("pl:").and_then(|n| n.parse::<i64>().ok())
                    else {
                        return false;
                    };
                    let path_strs: Vec<String> =
                        srcs.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                    if let Some(lib) = state_for_drop.borrow().media_lib.as_ref() {
                        if let Err(e) = lib.append_paths_to_playlist(pid, &path_strs) {
                            eprintln!("append_paths_to_playlist {pid}: {e}");
                            return false;
                        }
                    }
                    notify_playlist_changed(pid);
                    true
                }
            }
        });
        sidebar.add_controller(dt);
    }

    let sidebar_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Automatic)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .child(&sidebar)
        .build();

    // ── "Files" row ───────────────────────────────────────────────────────
    {
        let lbl = Label::builder()
            .label("Files")
            .halign(Align::Start)
            .xalign(0.0)
            .margin_start(10)
            .margin_end(10)
            .margin_top(7)
            .margin_bottom(7)
            .build();
        let row = ListBoxRow::new();
        row.set_widget_name("files");
        row.set_child(Some(&lbl));
        sidebar.append(&row);
    }

    // ── "Playlists" header row (with expand/collapse chevron) ─────────────
    let playlists_expanded = Rc::new(Cell::new(
        state.borrow().config.window.ml_playlists_expanded
    ));

    // Track sub-rows so we can show/hide them on toggle
    let pl_sub_rows: Rc<RefCell<Vec<ListBoxRow>>> = Rc::new(RefCell::new(Vec::new()));

    {
        let pl_header_box = GtkBox::new(Orientation::Horizontal, 0);

        let pl_lbl = Label::builder()
            .label("Playlists")
            .halign(Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .margin_start(10)
            .margin_top(7)
            .margin_bottom(7)
            .build();

        // Chevron label — "▾" expanded, "▸" collapsed
        let chevron_lbl = Label::builder()
            .label(if playlists_expanded.get() { "▾" } else { "▸" })
            .margin_end(8)
            .build();

        pl_header_box.append(&pl_lbl);
        pl_header_box.append(&chevron_lbl);

        let row_playlists = ListBoxRow::new();
        row_playlists.set_widget_name("playlists");
        row_playlists.set_child(Some(&pl_header_box));
        sidebar.append(&row_playlists);

        // Chevron click toggles expansion (separate from navigation)
        let gesture = GestureClick::new();
        let expanded_rc = playlists_expanded.clone();
        let sub_rows_rc  = pl_sub_rows.clone();
        let chev = chevron_lbl.clone();
        gesture.connect_released(move |g, _n, x, _y| {
            // Only handle clicks in the right ~20px (chevron area)
            let widget = g.widget();
            let width  = widget.map(|w| w.width()).unwrap_or(0) as f64;
            if x < width - 24.0 {
                return; // let the row selection handle the left area
            }
            let new_val = !expanded_rc.get();
            expanded_rc.set(new_val);
            chev.set_text(if new_val { "▾" } else { "▸" });
            for r in sub_rows_rc.borrow().iter() {
                r.set_visible(new_val);
            }
        });
        row_playlists.add_controller(gesture);
    }

    // Populate initial playlist sub-rows
    {
        let playlists_initial = state
            .borrow()
            .media_lib
            .as_ref()
            .and_then(|lib| lib.all_playlists().ok())
            .unwrap_or_default();
        let expanded = playlists_expanded.get();
        for pl in &playlists_initial {
            let lbl = Label::builder()
                .label(&pl.name)
                .halign(Align::Start)
                .xalign(0.0)
                .margin_start(24)  // indent
                .margin_end(8)
                .margin_top(4)
                .margin_bottom(4)
                .build();
            let row = ListBoxRow::new();
            row.set_widget_name(&format!("pl:{}", pl.id));
            row.set_child(Some(&lbl));
            row.set_visible(expanded);
            attach_pl_row_drag(&row, pl.id);
            sidebar.append(&row);
            pl_sub_rows.borrow_mut().push(row);
        }
    }

    // ── "Disc Drives" header row (optical drives via crate::disc) ─────────
    // Sits just above Devices. Disc sub-rows are inserted between this header
    // and the Devices header; device rows keep appending to the sidebar end, so
    // the two groups stay separate. Phase 1: detection + audio-CD playback.
    let discs_expanded = Rc::new(Cell::new(true));
    let disc_sub_rows: Rc<RefCell<Vec<ListBoxRow>>> = Rc::new(RefCell::new(Vec::new()));
    let current_drives: Rc<RefCell<Vec<crate::disc::OpticalDrive>>> =
        Rc::new(RefCell::new(Vec::new()));
    let selected_disc_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let current_disc_entries: Rc<RefCell<Vec<crate::disc::DiscTrackEntry>>> =
        Rc::new(RefCell::new(Vec::new()));
    // Phase 2 — per-disc gnudb tags, keyed by freedb id. `disc_tags` is the
    // user's current set (drives titles/artist/album, and rip/submit later);
    // `disc_official` keeps the untouched gnudb match as the submission
    // baseline. Both are seeded from the shared on-disk store so names survive
    // restarts. `pending_disc_matches` parks a multi-match result (discid +
    // candidates) when the user leaves the view before choosing.
    let disc_tags: Rc<RefCell<std::collections::HashMap<String, crate::disc::xmcd::XmcdEntry>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    let disc_official: Rc<
        RefCell<std::collections::HashMap<String, crate::disc::xmcd::XmcdEntry>>,
    > = Rc::new(RefCell::new(std::collections::HashMap::new()));
    {
        let store = crate::disc::tagstore::DiscTagStore::load();
        for (id, rec) in store.discs {
            disc_tags.borrow_mut().insert(id.clone(), rec.user);
            if let Some(o) = rec.official {
                disc_official.borrow_mut().insert(id, o);
            }
        }
    }
    // Phase 3 rip state: a cancel flag shared with the worker thread, and a
    // guard so only one rip runs at a time.
    let rip_cancel: Rc<RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>> =
        Rc::new(RefCell::new(None));
    let rip_active = Rc::new(Cell::new(false));
    // True until the first drive poll finishes, so the overview shows a
    // "Detecting…" hint instead of a premature "No disc drives connected".
    let disc_detecting = Rc::new(Cell::new(true));
    // Spinner shown in the sidebar header while that first poll runs; stopped
    // and hidden by refresh_discs once detection completes.
    let disc_detect_spinner = gtk4::Spinner::new();
    // Sits immediately after the "Disc Drives" label (not far-right, where a wide
    // sidebar would push it off-screen). An unsized spinner in a header slot can
    // render 0×0, so give it an explicit size and center it vertically.
    disc_detect_spinner.set_margin_start(6);
    disc_detect_spinner.set_size_request(16, 16);
    disc_detect_spinner.set_valign(Align::Center);
    disc_detect_spinner.start();
    {
        let hdr = GtkBox::new(Orientation::Horizontal, 0);
        // Label takes only its text width (no hexpand) so the spinner can follow
        // it directly; a hexpanding spacer then keeps the chevron right-aligned.
        let lbl = Label::builder()
            .label("Disc Drives")
            .halign(Align::Start)
            .xalign(0.0)
            .margin_start(10)
            .margin_top(7)
            .margin_bottom(7)
            .build();
        let spacer = Label::new(None);
        spacer.set_hexpand(true);
        let chev = Label::builder()
            .label(if discs_expanded.get() { "▾" } else { "▸" })
            .margin_end(8)
            .build();
        hdr.append(&lbl);
        hdr.append(&disc_detect_spinner);
        hdr.append(&spacer);
        hdr.append(&chev);
        let row = ListBoxRow::new();
        row.set_widget_name("discs");
        row.set_child(Some(&hdr));
        sidebar.append(&row);

        let gesture = GestureClick::new();
        let exp = discs_expanded.clone();
        let subs = disc_sub_rows.clone();
        let chev2 = chev.clone();
        gesture.connect_released(move |g, _n, x, _y| {
            let w = g.widget().map(|w| w.width()).unwrap_or(0) as f64;
            if x < w - 24.0 {
                return; // left of the chevron = navigation, handled elsewhere
            }
            let v = !exp.get();
            exp.set(v);
            chev2.set_text(if v { "▾" } else { "▸" });
            for r in subs.borrow().iter() {
                r.set_visible(v);
            }
        });
        row.add_controller(gesture);
    }

    // ── "Devices" header row (external USB/SD storage via udisks2) ────────
    // Mirrors the Playlists header: an expand/collapse chevron, with device
    // sub-rows populated live by the poll below.
    let devices_expanded = Rc::new(Cell::new(true));
    let dev_sub_rows: Rc<RefCell<Vec<ListBoxRow>>> = Rc::new(RefCell::new(Vec::new()));
    // `current_devices` is declared earlier (before the sidebar DropTarget).
    {
        let hdr = GtkBox::new(Orientation::Horizontal, 0);
        let lbl = Label::builder()
            .label("Devices")
            .halign(Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .margin_start(10)
            .margin_top(7)
            .margin_bottom(7)
            .build();
        let chev = Label::builder()
            .label(if devices_expanded.get() { "▾" } else { "▸" })
            .margin_end(8)
            .build();
        hdr.append(&lbl);
        hdr.append(&chev);
        let row = ListBoxRow::new();
        row.set_widget_name("devices");
        row.set_child(Some(&hdr));
        sidebar.append(&row);

        let gesture = GestureClick::new();
        let exp = devices_expanded.clone();
        let subs = dev_sub_rows.clone();
        let chev2 = chev.clone();
        gesture.connect_released(move |g, _n, x, _y| {
            let w = g.widget().map(|w| w.width()).unwrap_or(0) as f64;
            if x < w - 24.0 {
                return; // left of the chevron = navigation, handled elsewhere
            }
            let v = !exp.get();
            exp.set(v);
            chev2.set_text(if v { "▾" } else { "▸" });
            for r in subs.borrow().iter() {
                r.set_visible(v);
            }
        });
        row.add_controller(gesture);
    }

    // ── Devices content page widgets (added to the stack below) ───────────
    let dev_page = GtkBox::new(Orientation::Vertical, 8);
    dev_page.set_margin_top(8);
    dev_page.set_margin_start(8);
    dev_page.set_margin_end(8);

    // Diagnostics banner — shown only when udisks2 can't be reached.
    let dev_banner = GtkBox::new(Orientation::Horizontal, 8);
    dev_banner.set_visible(false);
    let dev_banner_lbl = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .build();
    dev_banner_lbl.add_css_class("broken");
    let dev_banner_retry = Button::with_label("Retry");
    dev_banner_retry.add_css_class("pl-btn");
    dev_banner.append(&dev_banner_lbl);
    dev_banner.append(&dev_banner_retry);
    dev_page.append(&dev_banner);

    // ── Overview: a live list of all connected devices (shown when the
    // Devices header is selected). ───────────────────────────────────────
    let dev_overview = GtkBox::new(Orientation::Vertical, 6);
    let dev_overview_title = Label::builder()
        .label("Devices")
        .halign(Align::Start)
        .xalign(0.0)
        .build();
    dev_overview_title.add_css_class("ml-section-header");
    dev_overview.append(&dev_overview_title);
    let dev_overview_list = GtkBox::new(Orientation::Vertical, 12);
    dev_overview_list.set_margin_top(6);
    dev_overview.append(&dev_overview_list);
    dev_page.append(&dev_overview);

    // ── Detail: the selected device (hidden until one is picked) ─────────
    let dev_detail = GtkBox::new(Orientation::Vertical, 8);
    dev_detail.set_visible(false);

    // Header band: device icon · name + (filesystem · path) · status badges ·
    // Sync / Eject. Populated by the device-select handler.
    let dev_icon = Image::from_icon_name("drive-removable-media");
    dev_icon.set_pixel_size(40);
    dev_icon.set_valign(Align::Center);

    let dev_title = Label::builder().halign(Align::Start).xalign(0.0).build();
    dev_title.add_css_class("device-detail-name");
    // Filesystem + mount path subtitle (selectable so the path can be copied).
    let dev_path = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .selectable(true)
        .ellipsize(gtk4::pango::EllipsizeMode::Middle)
        .build();
    dev_path.add_css_class("status-label");
    // Unsupported-filesystem tag sits under the "fs · path" line on the left,
    // left-aligned and a touch smaller than the read-only pill.
    let dev_warn_badge = Label::new(Some("⚠ Unsupported"));
    dev_warn_badge.add_css_class("device-badge");
    dev_warn_badge.add_css_class("device-badge-warn");
    dev_warn_badge.add_css_class("device-badge-sm");
    dev_warn_badge.set_halign(Align::Start);
    dev_warn_badge.set_margin_top(4);
    dev_warn_badge.set_tooltip_text(Some(UNSUPPORTED_FS_TOOLTIP));
    dev_warn_badge.set_visible(false);

    let dev_title_box = GtkBox::new(Orientation::Vertical, 0);
    dev_title_box.set_valign(Align::Center);
    dev_title_box.append(&dev_title);
    dev_title_box.append(&dev_path);
    dev_title_box.append(&dev_warn_badge);

    let dev_ro_badge = Label::new(Some("🔒 Read-only"));
    dev_ro_badge.add_css_class("device-badge");
    dev_ro_badge.set_valign(Align::Center);
    dev_ro_badge.set_visible(false);

    let dev_scan = Button::with_label("Scan");
    dev_scan.add_css_class("pl-btn");
    dev_scan.set_valign(Align::Center);
    dev_scan.set_tooltip_text(Some("Re-read tags + duration from the files on this device"));
    dev_scan.set_sensitive(false);
    let dev_sync = Button::with_label("Sync");
    dev_sync.add_css_class("pl-btn");
    dev_sync.set_valign(Align::Center);
    dev_sync.set_sensitive(false);
    let dev_eject = Button::with_label("Eject");
    dev_eject.add_css_class("pl-btn");
    dev_eject.set_valign(Align::Center);
    dev_eject.set_sensitive(false);

    // Capacity meter — capacity bar + used/free/total text. Lives in the header
    // band (between the name/path and the Sync/Eject buttons) to save vertical
    // space, taking the flexible middle column.
    let dev_levelbar = gtk4::LevelBar::new();
    dev_levelbar.set_min_value(0.0);
    dev_levelbar.set_max_value(1.0);
    dev_levelbar.add_css_class("device-capacity");
    dev_levelbar.set_valign(Align::Center);
    let dev_capacity = Label::builder().halign(Align::Start).xalign(0.0).build();
    dev_capacity.add_css_class("status-label");
    dev_capacity.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    // Third row of the capacity area: "X playlists - Y audio files".
    let dev_counts = Label::builder().halign(Align::Start).xalign(0.0).build();
    dev_counts.add_css_class("status-label");
    dev_counts.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    let dev_capacity_box = GtkBox::new(Orientation::Vertical, 2);
    dev_capacity_box.set_hexpand(true);
    dev_capacity_box.set_valign(Align::Center);
    // Triple the breathing room on either side of the capacity bar.
    dev_capacity_box.set_margin_start(30);
    dev_capacity_box.set_margin_end(30);
    dev_capacity_box.append(&dev_levelbar);
    dev_capacity_box.append(&dev_capacity);
    dev_capacity_box.append(&dev_counts);

    let dev_hdr_row = GtkBox::new(Orientation::Horizontal, 10);
    dev_hdr_row.add_css_class("device-detail-header");
    dev_hdr_row.append(&dev_icon);
    dev_hdr_row.append(&dev_title_box);
    dev_hdr_row.append(&dev_capacity_box);
    dev_hdr_row.append(&dev_ro_badge);
    dev_hdr_row.append(&dev_scan);
    dev_hdr_row.append(&dev_sync);
    dev_hdr_row.append(&dev_eject);
    dev_detail.append(&dev_hdr_row);

    // Copy progress bar — shown only while files are being copied to this
    // device; carries an "x/y · filename" label.
    // Thick accent bar matching the capacity bar above; the live "Copying x/y ·
    // filename" text rides in the status bar (`dev_hint`), so the bar itself
    // carries no inline text and can be slim/tall like the capacity meter.
    let dev_progress = gtk4::ProgressBar::new();
    dev_progress.set_show_text(false);
    dev_progress.set_visible(false);
    dev_progress.add_css_class("device-progress");
    dev_detail.append(&dev_progress);

    // Caution banner for a connected device with no readable filesystem (an
    // MTP phone whose storage isn't shared). Shown in place of the playlist and
    // file lists, which are hidden while it is up.
    let dev_nofs_banner = GtkBox::new(Orientation::Horizontal, 8);
    dev_nofs_banner.set_visible(false);
    dev_nofs_banner.set_margin_top(12);
    dev_nofs_banner.set_margin_bottom(12);
    let dev_nofs_lbl = Label::builder()
        .label(
            "⚠ No visible filesystem on this device. Set the phone to file-transfer \
             mode and allow access, or reconnect it, then press Scan.",
        )
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .build();
    dev_nofs_lbl.add_css_class("broken");
    dev_nofs_banner.append(&dev_nofs_lbl);
    dev_detail.append(&dev_nofs_banner);

    // Playlists section header: a "Playlists" label on the left and an always-
    // available "+ New" button on the right that creates a device-only playlist.
    let dev_pl_header_lbl = Label::builder()
        .label("Playlists")
        .halign(Align::Start)
        .xalign(0.0)
        .hexpand(true)
        .build();
    dev_pl_header_lbl.add_css_class("ml-section-header");
    let dev_pl_new = Button::with_label("+ New");
    dev_pl_new.add_css_class("pl-btn");
    let dev_pl_header = GtkBox::new(Orientation::Horizontal, 6);
    dev_pl_header.append(&dev_pl_header_lbl);
    dev_pl_header.append(&dev_pl_new);
    dev_detail.append(&dev_pl_header);
    // Filter chips: "All files" + one toggle per device .m3u/.m3u8 (grouped so
    // exactly one is active, radio-style). Rebuilt per device by
    // reload_dev_playlists; the active chip drives the track filter.
    // Chips wrap onto multiple rows (no horizontal scroll that hid the names).
    let dev_pl_chips = gtk4::FlowBox::builder()
        .orientation(Orientation::Horizontal)
        .selection_mode(gtk4::SelectionMode::None)
        .row_spacing(4)
        .column_spacing(4)
        .min_children_per_line(1)
        .max_children_per_line(64)
        .homogeneous(false)
        .build();
    dev_pl_chips.add_css_class("device-chips");
    dev_pl_chips.set_valign(Align::Start);
    let dev_pl_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        // One chip row when there's a single row; grow as chips wrap, up to
        // ~2.5 rows before a vertical scrollbar appears. (No propagate-natural-
        // height: the FlowBox over-estimates row count and would inflate to the
        // max even for a single row.)
        .min_content_height(34)
        .max_content_height(80)
        .child(&dev_pl_chips)
        .build();
    dev_pl_scroll.set_vexpand(false);
    dev_detail.append(&dev_pl_scroll);

    // Per-playlist management actions — shown only when a specific playlist chip
    // (not "All files") is selected. Click handlers are wired further down, once
    // the device run-closures they depend on exist. A device playlist linked to
    // a library playlist (same safe name) is renamed via the library; a
    // device-only playlist is acted on in place.
    let dev_pl_rename = Button::with_label("Rename");
    let dev_pl_duplicate = Button::with_label("Duplicate");
    let dev_pl_delete = Button::with_label("Delete");
    for b in [&dev_pl_rename, &dev_pl_duplicate, &dev_pl_delete] {
        b.add_css_class("pl-btn");
    }
    dev_pl_delete.add_css_class("destructive");
    let dev_pl_actions = GtkBox::new(Orientation::Horizontal, 6);
    dev_pl_actions.append(&dev_pl_rename);
    dev_pl_actions.append(&dev_pl_duplicate);
    dev_pl_actions.append(&dev_pl_delete);
    dev_pl_actions.set_visible(false);
    dev_detail.append(&dev_pl_actions);
    // The device playlist file the active chip points at (None = "All files").
    let selected_dev_playlist: Rc<RefCell<Option<std::path::PathBuf>>> =
        Rc::new(RefCell::new(None));

    // Delete/Remove button for the device track view, created early so the
    // playlist filter can flip its label. It is placed into the bottom action
    // row further down. Label is "Delete" in the all-files view (delete off the
    // device + drop from every playlist) and "Remove" in a playlist view (drop
    // from that one playlist, keep the file). Disabled until files are selected.
    let dev_file_remove = Button::with_label("Delete");
    dev_file_remove.add_css_class("pl-btn");
    dev_file_remove.add_css_class("destructive");
    dev_file_remove.set_sensitive(false);

    // Live copy status ("Copying x/y · filename"). Empty when idle, so it acts
    // as the flexible spacer in the bottom action row (no dedicated status bar,
    // which left an empty strip at the bottom of the view).
    let dev_hint = Label::builder()
        .label("")
        .halign(Align::Start)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .build();
    dev_hint.add_css_class("status-label");
    // Kept for the selection handler's unsupported-fs note; not shown directly
    // (the title-section badge now carries that), so it stays unparented.
    let dev_warn = Label::builder()
        .halign(Align::End)
        .xalign(1.0)
        .visible(false)
        .build();
    dev_warn.add_css_class("broken");

    // Track view mirroring the files-view columns, populated from device tags.
    // `dev_store` is the *displayed* model: in the all-files view it holds every
    // device file; in a playlist view it holds that playlist's entries in order,
    // duplicates included (a playlist may reference the same file more than
    // once). `dev_all_tracks` caches the full device file list so switching
    // views doesn't re-scan the device.
    let dev_store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let dev_all_tracks: Rc<RefCell<Vec<crate::media_library::LibTrack>>> =
        Rc::new(RefCell::new(Vec::new()));
    // Device file path → the library file it was copied from (its sync pair), for
    // the device view's "Synced from" column so the user can see exactly which
    // computer file each device file is kept in step with. Rebuilt per device by
    // reload_device_store; read live by the column factory.
    let dev_pair_map: Rc<RefCell<std::collections::HashMap<String, String>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    // Per-view search over whatever the store currently shows (all files or
    // one playlist): store → filter → sort → selection, so every fill site
    // stays filter-oblivious and copy/delete still act on the selection.
    let dev_search_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let dev_filter = gtk4::CustomFilter::new({
        let q = dev_search_query.clone();
        move |obj| {
            let Some(boxed) = obj.downcast_ref::<glib::BoxedAnyObject>() else {
                return true;
            };
            lib_track_matches_query(&boxed.borrow::<crate::media_library::LibTrack>(), &q.borrow())
        }
    });
    let dev_filter_model =
        gtk4::FilterListModel::new(Some(dev_store.clone()), Some(dev_filter.clone()));
    // Search filters just this device view's rows (all-files or the shown
    // playlist). Created here so reload_device_store can clear it when a
    // different device opens; packed above the track table below.
    let (dev_search_row, dev_search_entry) =
        make_view_search_row("Search this device — artist, title, album…");
    {
        let q = dev_search_query.clone();
        let filter = dev_filter.clone();
        dev_search_entry.connect_changed(move |e| {
            *q.borrow_mut() = e.text().to_lowercase();
            filter.changed(gtk4::FilterChange::Different);
        });
    }
    let dev_sort_model = SortListModel::new(Some(dev_filter_model), None::<gtk4::Sorter>);
    let dev_selection = MultiSelection::new(Some(dev_sort_model.clone()));
    let dev_col_view = ColumnView::new(Some(dev_selection.clone()));
    dev_col_view.add_css_class("ml-col-view");
    dev_col_view.set_hexpand(true);
    dev_col_view.set_vexpand(true);

    // Playlist-order column (front): shown only while a playlist filter is
    // active, then made the default sort — like the editor's position column.
    let dev_pos_col = {
        let factory = SignalListItemFactory::new();
        factory.connect_setup(|_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            if li.child().is_some() {
                return;
            }
            let lbl = Label::builder()
                .halign(Align::End)
                .xalign(1.0)
                .margin_start(6)
                .margin_end(6)
                .css_classes(["pl-duration"])
                .build();
            li.set_child(Some(&lbl));
        });
        // The playlist view holds entries in order (no sort), so the row's
        // position in the model is its 1-based playlist position. Each duplicate
        // entry is its own row and gets its own number.
        factory.connect_bind(move |_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) else {
                return;
            };
            lbl.set_text(&(li.position() + 1).to_string());
        });
        let col = ColumnViewColumn::new(Some("#"), Some(factory));
        col.set_fixed_width(48);
        col.set_visible(false);
        dev_col_view.append_column(&col);
        col
    };

    // "Synced from" column (device view only): the library file each device file
    // was copied from. Lets the user confirm at a glance which computer file a
    // sync keeps in step, instead of guessing among same-named files. Reads the
    // live per-device pair map keyed by on-device path.
    {
        let pair_map = dev_pair_map.clone();
        let factory = SignalListItemFactory::new();
        factory.connect_setup(|_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            if li.child().is_some() {
                return;
            }
            let lbl = Label::builder()
                .halign(Align::Start)
                .xalign(0.0)
                .margin_start(6)
                .margin_end(6)
                .ellipsize(gtk4::pango::EllipsizeMode::Middle)
                .css_classes(["status-label"])
                .build();
            li.set_child(Some(&lbl));
        });
        factory.connect_bind(move |_, obj| {
            let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
            let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) else {
                return;
            };
            let Some(item) = li.item() else { return };
            let Some(boxed) = item.downcast_ref::<glib::BoxedAnyObject>() else {
                return;
            };
            let path = boxed.borrow::<crate::media_library::LibTrack>().path.clone();
            match pair_map.borrow().get(&path) {
                Some(libp) => {
                    let base = std::path::Path::new(libp)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(libp);
                    lbl.set_text(&gtk_safe(base));
                    lbl.set_tooltip_text(Some(&gtk_safe(libp)));
                }
                None => {
                    lbl.set_text("—");
                    lbl.set_tooltip_text(Some("Not synced from this computer"));
                }
            }
        });
        let col = ColumnViewColumn::new(Some("Synced from"), Some(factory));
        col.set_fixed_width(220);
        col.set_resizable(true);
        dev_col_view.append_column(&col);
    }

    let mut dev_named_cols: Vec<(String, ColumnViewColumn)> = Vec::new();
    // Buttons that already have a click handler wired (artwork "View"), so the
    // device factory connects each button instance only once.
    let dev_connected_artwork: Rc<RefCell<std::collections::HashSet<glib::Object>>> =
        Rc::new(RefCell::new(std::collections::HashSet::new()));
    {
        // Columns that are library bookkeeping, not ID3 tags — irrelevant for a
        // device, so never shown here even if visible in the files view.
        const DEVICE_HIDDEN_COLS: &[&str] = &["play_count", "last_played", "last_scanned"];
        let visible_ids: Vec<String> =
            state.borrow().config.media_library.visible_columns.clone();
        let widths: std::collections::HashMap<String, i32> =
            state.borrow().config.media_library.ml_file_col_widths.clone();
        let order = state.borrow().config.media_library.ml_file_col_order.clone();
        // Build columns in the saved order (unknown/leftover ids appended).
        let ordered: Vec<&MlColumnDef> = {
            let mut v: Vec<&MlColumnDef> = Vec::new();
            for id in &order {
                if let Some(c) = ALL_COLUMNS.iter().find(|c| &c.id == id) {
                    v.push(c);
                }
            }
            for c in ALL_COLUMNS.iter() {
                if !order.iter().any(|id| id == c.id) {
                    v.push(c);
                }
            }
            v
        };
        for c in ordered {
            if DEVICE_HIDDEN_COLS.contains(&c.id) {
                continue;
            }
            let id_str = c.id.to_string();
            let is_art = c.id == "artwork_path";
            let factory = SignalListItemFactory::new();
            factory.connect_setup(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() {
                    return;
                }
                // Artwork column shows a "View" button (mirrors the files view),
                // every other column a plain label.
                let child: gtk4::Widget = if is_art {
                    let btn = Button::with_label("View");
                    btn.add_css_class("link");
                    btn.set_halign(Align::Start);
                    btn.set_margin_start(4);
                    btn.set_margin_end(4);
                    btn.set_visible(false);
                    btn.upcast::<gtk4::Widget>()
                } else {
                    Label::builder()
                        .halign(Align::Start)
                        .xalign(0.0)
                        .margin_start(6)
                        .margin_end(6)
                        .ellipsize(gtk4::pango::EllipsizeMode::End)
                        .css_classes(["ml-col-label"])
                        .build()
                        .upcast::<gtk4::Widget>()
                };
                li.set_child(Some(&child));
            });
            let bind_id = id_str.clone();
            let bind_connected = dev_connected_artwork.clone();
            factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li
                    .item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else {
                    return;
                };
                let t = boxed.borrow::<crate::media_library::LibTrack>();
                if is_art {
                    let Some(btn) = li.child().and_then(|c| c.downcast::<Button>().ok()) else {
                        return;
                    };
                    if let Some(ref art_path) = t.artwork_path {
                        btn.set_visible(true);
                        let btn_obj = btn.clone().upcast::<glib::Object>();
                        if !bind_connected.borrow().contains(&btn_obj) {
                            bind_connected.borrow_mut().insert(btn_obj);
                            let art = art_path.clone();
                            btn.connect_clicked(move |_| open_image_viewer(&art));
                        }
                    } else {
                        btn.set_visible(false);
                    }
                    return;
                }
                let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) else {
                    return;
                };
                lbl.set_text(&gtk_safe(&ml_cell_text(&t, &bind_id)));
            });
            let col = ColumnViewColumn::new(Some(c.header), Some(factory));
            col.set_resizable(true);
            if c.expand {
                col.set_expand(true);
            }
            col.set_visible(visible_ids.contains(&id_str));
            if let Some(&w) = widths.get(&id_str) {
                if w > 0 {
                    col.set_fixed_width(w);
                }
            }
            let sort_id = id_str.clone();
            let sorter = CustomSorter::new(move |a, b| {
                let ka = a
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<crate::media_library::LibTrack>(), &sort_id))
                    .unwrap_or_default();
                let kb = b
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<crate::media_library::LibTrack>(), &sort_id))
                    .unwrap_or_default();
                ka.cmp(&kb).into()
            });
            col.set_sorter(Some(&sorter));
            dev_named_cols.push((id_str.clone(), col.clone()));
            dev_col_view.append_column(&col);
        }
        // Header clicks drive the sort model.
        dev_sort_model.set_sorter(dev_col_view.sorter().as_ref());
    }
    let dev_named_cols = Rc::new(dev_named_cols);

    // Backend object id of the currently shown device (Eject/Sync target).
    let selected_dev_backend: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Reload a device's tracks into the column store (tags re-read on a worker
    // thread). Used on device select and after a sync so changed values show
    // immediately.
    let reload_device_store: Rc<dyn Fn(crate::devices::Device)> = {
        let store = dev_store.clone();
        let all_tracks = dev_all_tracks.clone();
        let hint = dev_hint.clone();
        let counts_lbl = dev_counts.clone();
        let state = state.clone();
        let counts_cache = device_counts.clone();
        let sel_backend = selected_dev_backend.clone();
        let pair_map = dev_pair_map.clone();
        let search = dev_search_entry.clone();
        Rc::new(move |dev: crate::devices::Device| {
            counts_lbl.set_text("Reading device…");
            hint.set_text(""); // clear any stale copy status
            // A previous device's search query must not filter this one.
            search.set_text("");
            store.remove_all();
            pair_map.borrow_mut().clear(); // drop the previous device's pairings
            // Device contents may have changed (copy/send/sync) — drop the
            // cached overview counts so the cards recompute next time shown, and
            // the cached MTP metadata so the next poll refreshes free space once.
            counts_cache.borrow_mut().remove(&dev.backend_id);
            invalidate_mtp_meta(&dev.backend_id);
            let store2 = store.clone();
            let all_tracks2 = all_tracks.clone();
            let counts_lbl2 = counts_lbl.clone();
            let state2 = state.clone();
            let pair_map2 = pair_map.clone();
            let mount = dev.mount_path.clone();
            // Guard against a slow scan landing after the user switched devices:
            // each scan is tagged with its device, and results are applied only
            // if that device is still the one shown (else a stale scan would
            // overwrite the current device's list — the "275 vs 18" flip).
            let backend = dev.backend_id.clone();
            let sel_backend2 = sel_backend.clone();
            // Non-writing device id (don't drop a marker just to browse).
            let device_id = if dev.id.is_empty() {
                crate::devices::marker::read_marker(&dev.mount_path).unwrap_or_default()
            } else {
                dev.id.clone()
            };
            // Backend-specific IO (POSIX today; gio/MTP later) — move it onto the
            // worker thread for the blocking scan.
            let io = crate::devices::io::for_device(&dev);
            glib::spawn_future_local(async move {
                let (mut tracks, pl_count) = gio::spawn_blocking(move || {
                    if device_io_shutting_down() {
                        return (Vec::new(), 0);
                    }
                    let tracks = io
                        .list_audio_files()
                        .iter()
                        .map(|p| crate::devices::browse::read_device_track(p))
                        .collect::<Vec<crate::media_library::LibTrack>>();
                    let pl_count = io.playlist_files().len();
                    (tracks, pl_count)
                })
                .await
                .unwrap_or_default();

                // Stale-scan guard: bail if the user has since switched devices.
                if sel_backend2.borrow().as_deref() != Some(backend.as_str()) {
                    return;
                }

                // Prefill calculated values (duration, bitrate, channels) from
                // the paired library track for files copied from this computer,
                // so device rows match the files view even when the on-device
                // tags don't carry that info.
                if !device_id.is_empty() {
                    let s = state2.borrow();
                    if let Some(lib) = s.media_lib.as_ref() {
                        if let Ok(pairs) = lib.sync_pairs_for_device(&device_id) {
                            // Populate the "Synced from" map: on-device path → the
                            // library file it was copied from.
                            let mut pm = std::collections::HashMap::new();
                            for p in &pairs {
                                pm.insert(
                                    mount.join(&p.device_relpath).to_string_lossy().into_owned(),
                                    p.library_path.clone(),
                                );
                            }
                            *pair_map2.borrow_mut() = pm;
                            for t in tracks.iter_mut() {
                                let tp = std::path::Path::new(&t.path);
                                let Some(pair) = pairs.iter().find(|p| {
                                    mount.join(&p.device_relpath) == tp
                                }) else {
                                    continue;
                                };
                                let Ok(libt) = lib.track_by_path(&pair.library_path) else {
                                    continue;
                                };
                                if t.length_secs.is_none() {
                                    t.length_secs = libt.length_secs;
                                }
                                if t.bitrate.is_none() {
                                    t.bitrate = libt.bitrate;
                                }
                                if t.channels.is_none() {
                                    t.channels = libt.channels;
                                }
                                t.sort_keys = crate::media_library::SortKeys::from_track(t);
                            }
                        }
                    }
                }

                // Cache the full file list (for playlist views) and show all
                // files. A playlist chip selection re-derives its rows from this
                // cache without re-scanning.
                *all_tracks2.borrow_mut() = tracks.clone();
                store2.remove_all();
                for t in &tracks {
                    store2.append(&glib::BoxedAnyObject::new(t.clone()));
                }
                counts_lbl2.set_text(&format!(
                    "{} playlist{} - {} audio file{}",
                    pl_count,
                    if pl_count == 1 { "" } else { "s" },
                    tracks.len(),
                    if tracks.len() == 1 { "" } else { "s" }
                ));
            });
        })
    };

    // Rebuild the device playlist-filter rows ("All files" + each device
    // .m3u/.m3u8) for a mount. Shared by the device-select handler and the
    // playlist-send completion so a just-copied playlist appears immediately.
    // Apply a playlist filter to the device track view by name ("all" clears
    // it; otherwise the device .m3u/.m3u8 path). Shared by every filter chip.
    let apply_pl_filter: Rc<dyn Fn(&str)> = {
        let store = dev_store.clone();
        let all_tracks = dev_all_tracks.clone();
        let sort_model = dev_sort_model.clone();
        let pos_col = dev_pos_col.clone();
        let col_view = dev_col_view.clone();
        let sel_pl = selected_dev_playlist.clone();
        let actions = dev_pl_actions.clone();
        let remove_btn = dev_file_remove.clone();
        Rc::new(move |name: &str| {
            store.remove_all();
            if name == "all" || name.is_empty() {
                *sel_pl.borrow_mut() = None;
                actions.set_visible(false);
                remove_btn.set_label("Delete");
                pos_col.set_visible(false);
                for t in all_tracks.borrow().iter() {
                    store.append(&glib::BoxedAnyObject::new(t.clone()));
                }
                // Restore column-driven sorting for the all-files view.
                sort_model.set_sorter(col_view.sorter().as_ref());
            } else {
                *sel_pl.borrow_mut() = Some(std::path::PathBuf::from(name));
                actions.set_visible(true);
                remove_btn.set_label("Remove");
                pos_col.set_visible(true);
                // Fixed playlist order: index the device files by filename, then
                // emit one row per playlist entry — duplicates included, in order.
                let order =
                    crate::devices::browse::playlist_entry_order(std::path::Path::new(name));
                let by_name: std::collections::HashMap<String, crate::media_library::LibTrack> =
                    all_tracks
                        .borrow()
                        .iter()
                        .map(|t| (t.filename.clone(), t.clone()))
                        .collect();
                // No sort in the playlist view, so insertion order = playlist order.
                sort_model.set_sorter(None::<&gtk4::Sorter>);
                for fname in order {
                    if let Some(t) = by_name.get(&fname) {
                        store.append(&glib::BoxedAnyObject::new(t.clone()));
                    }
                }
            }
        })
    };

    let reload_dev_playlists: Rc<dyn Fn(crate::devices::Device)> = {
        let chips = dev_pl_chips.clone();
        let apply = apply_pl_filter.clone();
        // Generation token: bumped on every call so an in-flight playlist walk
        // (slow over MTP) that finishes after the user switched devices is
        // discarded instead of appending stale chips.
        let generation = Rc::new(Cell::new(0u64));
        Rc::new(move |dev: crate::devices::Device| {
            let gen_id = generation.get().wrapping_add(1);
            generation.set(gen_id);
            while let Some(c) = chips.first_child() {
                chips.remove(&c);
            }
            // "All files" chip + cleared filter are shown immediately so the
            // detail page paints without waiting on the device walk.
            let all = gtk4::ToggleButton::with_label("All files");
            all.add_css_class("device-chip");
            {
                let apply2 = apply.clone();
                all.connect_toggled(move |btn| {
                    if btn.is_active() {
                        apply2("all");
                    }
                });
            }
            chips.insert(&all, -1);
            all.set_active(true);
            apply("all");

            // Walk the device for playlist files off the main thread (a recursive
            // tree walk over a gvfs/MTP FUSE mount would otherwise freeze the UI),
            // then append a chip per playlist if this is still the shown device.
            let chips2 = chips.clone();
            let all2 = all.clone();
            let apply3 = apply.clone();
            let generation2 = generation.clone();
            let io = crate::devices::io::for_device(&dev);
            glib::spawn_future_local(async move {
                let pls = gio::spawn_blocking(move || io.playlist_files())
                    .await
                    .unwrap_or_default();
                if generation2.get() != gen_id {
                    return; // device switched / chips rebuilt since this walk began
                }
                for pl in pls {
                    let nm = pl
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let path_name = pl.to_string_lossy().into_owned();
                    let chip = gtk4::ToggleButton::with_label(&gtk_safe(&nm));
                    chip.add_css_class("device-chip");
                    chip.set_group(Some(&all2));
                    let apply4 = apply3.clone();
                    chip.connect_toggled(move |btn| {
                        if btn.is_active() {
                            apply4(&path_name);
                        }
                    });
                    chips2.insert(&chip, -1);
                }
            });
        })
    };

    // Send a whole playlist (files + .m3u8) to a device, copying on a worker
    // thread with live progress shown on the device's sidebar row and detail.
    let send_playlist_run: Rc<dyn Fn(crate::devices::Device, i64, String)> = {
        let state = state.clone();
        let sidebar = sidebar.clone();
        let hint = dev_hint.clone();
        let progress = dev_progress.clone();
        let reload = reload_device_store.clone();
        let reload_pls = reload_dev_playlists.clone();
        let sel_backend = selected_dev_backend.clone();
        let update_card = update_card_progress.clone();
        let eject = dev_eject.clone();
        let win_wk = win.downgrade();
        Rc::new(move |dev: crate::devices::Device, playlist_id: i64, name: String| {
            let plan = match prepare_playlist_send(&state, &dev, playlist_id, &name) {
                Ok(p) => p,
                Err(e) => {
                    show_alert_parented(win_wk.upgrade().as_ref(), &e);
                    return;
                }
            };
            let backend = dev.backend_id.clone();
            let dname = if dev.label.is_empty() {
                "device".to_string()
            } else {
                dev.label.clone()
            };
            let row_base = format!(
                "{}{}",
                device_glyph_prefix(dev.read_only, &dev.fs_type),
                if dev.label.is_empty() {
                    "Untitled device".to_string()
                } else {
                    dev.label.clone()
                }
            );
            let set_row_label = {
                let sidebar = sidebar.clone();
                let row_name = format!("dev:{backend}");
                move |text: &str| {
                    if let Some(row) = find_row_by_name(&sidebar, &row_name) {
                        if let Some(bx) = row.child().and_then(|c| c.downcast::<GtkBox>().ok()) {
                            if let Some(lbl) =
                                bx.first_child().and_then(|c| c.downcast::<Label>().ok())
                            {
                                lbl.set_text(text);
                            }
                        }
                    }
                }
            };

            let total = plan.srcs.len();
            let srcs = plan.srcs.clone();
            let device_id = plan.device_id.clone();
            let m3u_path = plan.m3u_path.clone();
            let mount = dev.mount_path.clone();
            let dev_for_reload = dev.clone();
            let state2 = state.clone();
            let hint2 = hint.clone();
            let progress2 = progress.clone();
            let reload2 = reload.clone();
            let reload_pls2 = reload_pls.clone();
            let sel2 = sel_backend.clone();
            let update_card2 = update_card.clone();
            let eject2 = eject.clone();
            let dev_ejectable = dev.ejectable;
            let win2 = win_wk.clone();
            glib::spawn_future_local(async move {
                // (device relpath, library source path) pairs so the written
                // .m3u8 carries #EXTINF metadata from the library.
                let mut entries: Vec<(String, String)> = Vec::new();
                let (mut copied, mut skipped, mut failed) = (0usize, 0usize, 0usize);
                let on_dev = sel2.borrow().as_deref() == Some(backend.as_str());
                if on_dev {
                    eject2.set_sensitive(false); // no eject mid-copy
                }
                for (i, src) in srcs.iter().enumerate() {
                    let prog = format!("{}/{}", i + 1, total);
                    set_row_label(&format!("{row_base} — {prog}"));
                    update_card2(&backend, Some((i + 1, total)));
                    if sel2.borrow().as_deref() == Some(backend.as_str()) {
                        let fname = src.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        hint2.set_text(&format!("Copying {prog} · {fname}"));
                        progress2.set_visible(true);
                        progress2.set_text(Some(&format!("{prog} · {fname}")));
                        progress2.set_fraction((i + 1) as f64 / total.max(1) as f64);
                    }
                    // DB lookup on the main thread; FS plan + copy on the worker
                    // so a slow MTP FUSE op never blocks the UI.
                    let recorded = device_recorded_relpath(&state2, &device_id, src);
                    let s = src.clone();
                    let m = mount.clone();
                    let dc = dev_for_reload.clone();
                    let joined = gio::spawn_blocking(move || -> Result<(std::path::PathBuf, bool), ()> {
                        let (rel, present) = device_plan_fs(&m, &s, recorded);
                        if present {
                            return Ok((rel, false)); // already there → skipped
                        }
                        match crate::devices::io::for_device(&dc).copy_to_device(&s, &rel) {
                            Ok(_) => Ok((rel, true)),
                            Err(_) => Err(()),
                        }
                    })
                    .await;
                    match joined {
                        Ok(Ok((rel, copied_now))) => {
                            if copied_now {
                                copied += 1;
                            } else {
                                skipped += 1;
                            }
                            device_record_pair(&state2, &device_id, src, &rel);
                            entries.push((
                                rel.to_string_lossy().replace('\\', "/"),
                                src.to_string_lossy().into_owned(),
                            ));
                        }
                        _ => failed += 1,
                    }
                }
                // Write the playlist file, carrying #EXTINF metadata from the
                // library for each entry.
                let body = state2
                    .borrow()
                    .media_lib
                    .as_ref()
                    .map(|l| l.build_device_m3u(&entries))
                    .unwrap_or_else(|| {
                        format!(
                            "#EXTM3U\n{}\n",
                            entries.iter().map(|(r, _)| r.clone()).collect::<Vec<_>>().join("\n")
                        )
                    });
                let mp = m3u_path.clone();
                let _ = gio::spawn_blocking(move || std::fs::write(&mp, body)).await;
                // Record the playlist sync baseline so a later edit on either
                // side syncs two-way instead of the library silently winning.
                if !device_id.is_empty() {
                    let dev_fname = m3u_path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let basenames: Vec<String> = entries
                        .iter()
                        .map(|(e, _)| e.rsplit(['/', '\\']).next().unwrap_or(e).to_string())
                        .collect();
                    if let Some(lib) = state2.borrow().media_lib.as_ref() {
                        let _ = lib.upsert_playlist_baseline(&crate::media_library::PlaylistBaseline {
                            device_id: device_id.clone(),
                            library_playlist_id: playlist_id,
                            device_filename: dev_fname,
                            entries_hash: crate::devices::sync::entries_hash(&basenames),
                            last_sync_at: Some(crate::timeutil::format_current_timestamp()),
                        });
                    }
                }
                set_row_label(&row_base);
                progress2.set_visible(false);
                update_card2(&backend, None);
                if sel2.borrow().as_deref() == Some(backend.as_str()) {
                    eject2.set_sensitive(dev_ejectable);
                }
                reload2(dev_for_reload.clone());
                // Refresh the playlist filter so the just-written .m3u8 shows
                // immediately, without needing to reselect the device.
                if sel2.borrow().as_deref() == Some(backend.as_str()) {
                    reload_pls2(dev_for_reload.clone());
                }
                show_alert_parented(
                    win2.upgrade().as_ref(),
                    &format!(
                        "Sent to {dname}: {copied} copied, {skipped} skipped, {failed} failed, \
                         plus the playlist."
                    ),
                );
            });
        })
    };
    *send_playlist_holder.borrow_mut() = Some(send_playlist_run.clone());

    // ── Device playlist management actions (New / Rename / Duplicate / Delete) ─
    // Resolve the Device backing the currently-selected device row.
    let current_device_for_actions = {
        let current_devices = current_devices.clone();
        let sel_backend = selected_dev_backend.clone();
        move || -> Option<crate::devices::Device> {
            let backend = sel_backend.borrow().clone()?;
            current_devices
                .borrow()
                .iter()
                .find(|d| d.backend_id == backend)
                .cloned()
        }
    };

    // Rename: rename the device .m3u/.m3u8; if it is linked to a library
    // playlist, rename that too so the link (safe-name match) is preserved.
    {
        let state = state.clone();
        let sel_pl = selected_dev_playlist.clone();
        let get_dev = current_device_for_actions.clone();
        let reload_pls = reload_dev_playlists.clone();
        let reload_store = reload_device_store.clone();
        let win_wk = win.downgrade();
        dev_pl_rename.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            let Some(pl_path) = sel_pl.borrow().clone() else { return };
            if dev.read_only {
                show_alert_parented(win_wk.upgrade().as_ref(), "Device is read-only.");
                return;
            }
            let current_stem = pl_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = pl_path
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| "m3u8".to_string());

            let dialog = gtk4::Window::builder()
                .title("Rename Playlist")
                .modal(true)
                .resizable(false)
                .default_width(300)
                .build();
            if let Some(w) = win_wk.upgrade() {
                dialog.set_transient_for(Some(&w));
            }
            let vbox = GtkBox::new(Orientation::Vertical, 8);
            vbox.set_margin_top(12);
            vbox.set_margin_bottom(12);
            vbox.set_margin_start(12);
            vbox.set_margin_end(12);
            let lbl = Label::builder().label("New name:").halign(Align::Start).build();
            let name_entry = Entry::new();
            name_entry.set_text(&gtk_safe(&current_stem));
            name_entry.set_hexpand(true);
            let dialog_btns = GtkBox::new(Orientation::Horizontal, 6);
            dialog_btns.set_halign(Align::End);
            let cancel_btn = Button::with_label("Cancel");
            let ok_btn = Button::with_label("Rename");
            ok_btn.add_css_class("suggested-action");
            dialog_btns.append(&cancel_btn);
            dialog_btns.append(&ok_btn);
            vbox.append(&lbl);
            vbox.append(&name_entry);
            vbox.append(&dialog_btns);
            dialog.set_child(Some(&vbox));
            let d = dialog.clone();
            cancel_btn.connect_clicked(move |_| d.close());

            let d = dialog.clone();
            let e = name_entry.clone();
            let state2 = state.clone();
            let pl_path2 = pl_path.clone();
            let dev2 = dev.clone();
            let reload_pls2 = reload_pls.clone();
            let reload_store2 = reload_store.clone();
            let win_wk2 = win_wk.clone();
            let ext2 = ext.clone();
            ok_btn.connect_clicked(move |_| {
                let raw = e.text().to_string();
                if raw.trim().is_empty() {
                    return;
                }
                let safe = safe_playlist_filename(&raw);
                let new_path = pl_path2
                    .parent()
                    .map(|p| p.join(format!("{safe}.{ext2}")))
                    .unwrap_or_else(|| pl_path2.clone());
                if new_path != pl_path2 {
                    if let Err(err) = std::fs::rename(&pl_path2, &new_path) {
                        show_alert_parented(
                            win_wk2.upgrade().as_ref(),
                            &format!("Couldn't rename the playlist file: {err}"),
                        );
                        return;
                    }
                }
                // Keep a linked library playlist's name in step.
                if let Some((id, _)) = linked_library_playlist(&state2, &pl_path2) {
                    if let Some(lib) = state2.borrow().media_lib.as_ref() {
                        let _ = lib.rename_playlist(id, raw.trim());
                    }
                }
                reload_pls2(dev2.clone());
                reload_store2(dev2.clone());
                d.close();
            });
            let ok2 = ok_btn.clone();
            name_entry.connect_activate(move |_| {
                ok2.activate();
            });
            dialog.present();
        });
    }

    // Duplicate: copy the selected device .m3u/.m3u8 to a new name on the same
    // device. The copy is a device-only playlist (referencing the same files).
    {
        let sel_pl = selected_dev_playlist.clone();
        let get_dev = current_device_for_actions.clone();
        let reload_pls = reload_dev_playlists.clone();
        let win_wk = win.downgrade();
        dev_pl_duplicate.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            let Some(pl_path) = sel_pl.borrow().clone() else { return };
            if dev.read_only {
                show_alert_parented(win_wk.upgrade().as_ref(), "Device is read-only.");
                return;
            }
            let stem = pl_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = pl_path
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| "m3u8".to_string());

            let dialog = gtk4::Window::builder()
                .title("Duplicate Playlist")
                .modal(true)
                .resizable(false)
                .default_width(300)
                .build();
            if let Some(w) = win_wk.upgrade() {
                dialog.set_transient_for(Some(&w));
            }
            let vbox = GtkBox::new(Orientation::Vertical, 8);
            vbox.set_margin_top(12);
            vbox.set_margin_bottom(12);
            vbox.set_margin_start(12);
            vbox.set_margin_end(12);
            let lbl = Label::builder().label("Name for the copy:").halign(Align::Start).build();
            let name_entry = Entry::new();
            name_entry.set_text(&gtk_safe(&format!("{stem} copy")));
            name_entry.set_hexpand(true);
            let dialog_btns = GtkBox::new(Orientation::Horizontal, 6);
            dialog_btns.set_halign(Align::End);
            let cancel_btn = Button::with_label("Cancel");
            let ok_btn = Button::with_label("Duplicate");
            ok_btn.add_css_class("suggested-action");
            dialog_btns.append(&cancel_btn);
            dialog_btns.append(&ok_btn);
            vbox.append(&lbl);
            vbox.append(&name_entry);
            vbox.append(&dialog_btns);
            dialog.set_child(Some(&vbox));
            let d = dialog.clone();
            cancel_btn.connect_clicked(move |_| d.close());

            let d = dialog.clone();
            let e = name_entry.clone();
            let pl_path2 = pl_path.clone();
            let dev2 = dev.clone();
            let reload_pls2 = reload_pls.clone();
            let win_wk2 = win_wk.clone();
            let ext2 = ext.clone();
            ok_btn.connect_clicked(move |_| {
                let raw = e.text().to_string();
                if raw.trim().is_empty() {
                    return;
                }
                let safe = safe_playlist_filename(&raw);
                let dest = dev2.mount_path.join(format!("{safe}.{ext2}"));
                if dest == pl_path2 {
                    return;
                }
                if dest.exists() {
                    show_alert_parented(
                        win_wk2.upgrade().as_ref(),
                        "A playlist with that name already exists on the device.",
                    );
                    return;
                }
                if let Err(err) = std::fs::copy(&pl_path2, &dest) {
                    show_alert_parented(
                        win_wk2.upgrade().as_ref(),
                        &format!("Couldn't duplicate the playlist: {err}"),
                    );
                    return;
                }
                reload_pls2(dev2.clone());
                d.close();
            });
            let ok2 = ok_btn.clone();
            name_entry.connect_activate(move |_| {
                ok2.activate();
            });
            dialog.present();
        });
    }

    // New: create an empty device-only playlist (a bare .m3u8) on the device.
    // The user then adds device files to it. Always available (not tied to a
    // selected playlist).
    {
        let get_dev = current_device_for_actions.clone();
        let reload_pls = reload_dev_playlists.clone();
        let win_wk = win.downgrade();
        dev_pl_new.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            if dev.read_only {
                show_alert_parented(win_wk.upgrade().as_ref(), "Device is read-only.");
                return;
            }
            if device_fs_unsupported(&dev.fs_type) {
                show_alert_parented(
                    win_wk.upgrade().as_ref(),
                    "This filesystem is unsupported — can't create a playlist on it yet.",
                );
                return;
            }
            let dialog = gtk4::Window::builder()
                .title("New Playlist")
                .modal(true)
                .resizable(false)
                .default_width(300)
                .build();
            if let Some(w) = win_wk.upgrade() {
                dialog.set_transient_for(Some(&w));
            }
            let vbox = GtkBox::new(Orientation::Vertical, 8);
            vbox.set_margin_top(12);
            vbox.set_margin_bottom(12);
            vbox.set_margin_start(12);
            vbox.set_margin_end(12);
            let lbl = Label::builder().label("Playlist name:").halign(Align::Start).build();
            let name_entry = Entry::new();
            name_entry.set_text("New Playlist");
            name_entry.set_hexpand(true);
            let dialog_btns = GtkBox::new(Orientation::Horizontal, 6);
            dialog_btns.set_halign(Align::End);
            let cancel_btn = Button::with_label("Cancel");
            let ok_btn = Button::with_label("Create");
            ok_btn.add_css_class("suggested-action");
            dialog_btns.append(&cancel_btn);
            dialog_btns.append(&ok_btn);
            vbox.append(&lbl);
            vbox.append(&name_entry);
            vbox.append(&dialog_btns);
            dialog.set_child(Some(&vbox));
            let d = dialog.clone();
            cancel_btn.connect_clicked(move |_| d.close());

            let d = dialog.clone();
            let e = name_entry.clone();
            let dev2 = dev.clone();
            let reload_pls2 = reload_pls.clone();
            let win_wk2 = win_wk.clone();
            ok_btn.connect_clicked(move |_| {
                let raw = e.text().to_string();
                if raw.trim().is_empty() {
                    return;
                }
                let safe = safe_playlist_filename(&raw);
                let dest = dev2.mount_path.join(format!("{safe}.m3u8"));
                if dest.exists() {
                    show_alert_parented(
                        win_wk2.upgrade().as_ref(),
                        "A playlist with that name already exists on the device.",
                    );
                    return;
                }
                if let Err(err) = std::fs::write(&dest, "#EXTM3U\n") {
                    show_alert_parented(
                        win_wk2.upgrade().as_ref(),
                        &format!("Couldn't create the playlist: {err}"),
                    );
                    return;
                }
                reload_pls2(dev2.clone());
                d.close();
            });
            let ok2 = ok_btn.clone();
            name_entry.connect_activate(move |_| {
                ok2.activate();
            });
            dialog.present();
        });
    }

    // Delete: remove the .m3u/.m3u8 from the device only. The audio files are
    // kept (they may belong to other playlists), and no library playlist or
    // on-disk music file is touched (Deletion Rule).
    {
        let sel_pl = selected_dev_playlist.clone();
        let get_dev = current_device_for_actions.clone();
        let reload_pls = reload_dev_playlists.clone();
        let reload_store = reload_device_store.clone();
        let win_wk = win.downgrade();
        dev_pl_delete.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            let Some(pl_path) = sel_pl.borrow().clone() else { return };
            let name = pl_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let dialog = gtk4::AlertDialog::builder()
                .message(format!("Remove \"{name}\" from the device?"))
                .detail("Only the playlist file is removed. The songs stay on the device.")
                .buttons(vec!["Cancel".to_string(), "Remove".to_string()])
                .cancel_button(0)
                .default_button(1)
                .modal(true)
                .build();
            let pl_path2 = pl_path.clone();
            let dev2 = dev.clone();
            let reload_pls2 = reload_pls.clone();
            let reload_store2 = reload_store.clone();
            let win_wk2 = win_wk.clone();
            dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |res| {
                if res != Ok(1) {
                    return;
                }
                if let Err(err) = crate::devices::io::for_device(&dev2).delete(&pl_path2) {
                    show_alert_parented(
                        win_wk2.upgrade().as_ref(),
                        &format!("Couldn't remove the playlist file: {err}"),
                    );
                    return;
                }
                reload_pls2(dev2.clone());
                reload_store2(dev2.clone());
            });
        });
    }

    // Copy loose files (drag-drop from a view) onto a device on a worker
    // thread, with the same sidebar "(x/y)" label and detail progress bar the
    // playlist send uses. No .m3u8 is written — these are just files.
    let copy_files_run: Rc<dyn Fn(crate::devices::Device, Vec<std::path::PathBuf>)> = {
        let state = state.clone();
        let sidebar = sidebar.clone();
        let hint = dev_hint.clone();
        let progress = dev_progress.clone();
        let reload = reload_device_store.clone();
        let sel_backend = selected_dev_backend.clone();
        let update_card = update_card_progress.clone();
        let eject = dev_eject.clone();
        let win_wk = win.downgrade();
        Rc::new(move |dev: crate::devices::Device, srcs: Vec<std::path::PathBuf>| {
            if dev.read_only {
                let n = if dev.label.is_empty() { "This device" } else { &dev.label };
                show_alert_parented(
                    win_wk.upgrade().as_ref(),
                    &format!("{n} is read-only — can't copy files to it."),
                );
                return;
            }
            if device_fs_unsupported(&dev.fs_type) {
                show_alert_parented(
                    win_wk.upgrade().as_ref(),
                    &format!(
                        "{} is an unsupported filesystem — can't write to this device yet.",
                        dev.fs_type
                    ),
                );
                return;
            }
            let device_id = device_sync_id(&dev);
            let mount = dev.mount_path.clone();
            let srcs: Vec<std::path::PathBuf> =
                srcs.into_iter().filter(|p| p.exists()).collect();
            if srcs.is_empty() {
                return;
            }
            // Free-space guard — only when capacity is known (skips a pass of
            // slow per-file device checks on devices that can't report it, MTP).
            if dev.free_bytes > 0 {
                let mut need = 0u64;
                for src in &srcs {
                    if !device_plan_one(&state, &mount, &device_id, src).1 {
                        need += std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
                    }
                }
                if need > dev.free_bytes {
                    show_alert_parented(
                        win_wk.upgrade().as_ref(),
                        &format!(
                            "Not enough space on the device: need {:.1} GB, {:.1} GB free.",
                            need as f64 / 1e9,
                            dev.free_bytes as f64 / 1e9
                        ),
                    );
                    return;
                }
            }

            let backend = dev.backend_id.clone();
            let dname = if dev.label.is_empty() {
                "device".to_string()
            } else {
                dev.label.clone()
            };
            let row_base = format!(
                "{}{}",
                device_glyph_prefix(dev.read_only, &dev.fs_type),
                if dev.label.is_empty() {
                    "Untitled device".to_string()
                } else {
                    dev.label.clone()
                }
            );
            let set_row_label = {
                let sidebar = sidebar.clone();
                let row_name = format!("dev:{backend}");
                move |text: &str| {
                    if let Some(row) = find_row_by_name(&sidebar, &row_name) {
                        if let Some(bx) = row.child().and_then(|c| c.downcast::<GtkBox>().ok()) {
                            if let Some(lbl) =
                                bx.first_child().and_then(|c| c.downcast::<Label>().ok())
                            {
                                lbl.set_text(text);
                            }
                        }
                    }
                }
            };

            let total = srcs.len();
            let dev_for_reload = dev.clone();
            let state2 = state.clone();
            let hint2 = hint.clone();
            let progress2 = progress.clone();
            let reload2 = reload.clone();
            let sel2 = sel_backend.clone();
            let update_card2 = update_card.clone();
            let eject2 = eject.clone();
            let dev_ejectable = dev.ejectable;
            let win2 = win_wk.clone();
            glib::spawn_future_local(async move {
                let (mut copied, mut skipped, mut failed) = (0usize, 0usize, 0usize);
                if sel2.borrow().as_deref() == Some(backend.as_str()) {
                    eject2.set_sensitive(false); // no eject mid-copy
                }
                for (i, src) in srcs.iter().enumerate() {
                    let prog = format!("{}/{}", i + 1, total);
                    set_row_label(&format!("{row_base} — {prog}"));
                    update_card2(&backend, Some((i + 1, total)));
                    if sel2.borrow().as_deref() == Some(backend.as_str()) {
                        let fname = src.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        hint2.set_text(&format!("Copying {prog} · {fname}"));
                        progress2.set_visible(true);
                        progress2.set_text(Some(&format!("{prog} · {fname}")));
                        progress2.set_fraction((i + 1) as f64 / total.max(1) as f64);
                    }
                    // DB lookup on the main thread; the FS plan + copy (slow over
                    // MTP) run on the worker so the UI never blocks on FUSE.
                    let recorded = device_recorded_relpath(&state2, &device_id, src);
                    let s = src.clone();
                    let m = mount.clone();
                    let dc = dev_for_reload.clone();
                    let joined = gio::spawn_blocking(move || -> Result<(std::path::PathBuf, bool), ()> {
                        let (rel, present) = device_plan_fs(&m, &s, recorded);
                        if present {
                            return Ok((rel, false)); // already there → skipped
                        }
                        match crate::devices::io::for_device(&dc).copy_to_device(&s, &rel) {
                            Ok(_) => Ok((rel, true)),
                            Err(_) => Err(()),
                        }
                    })
                    .await;
                    match joined {
                        Ok(Ok((rel, copied_now))) => {
                            if copied_now {
                                copied += 1;
                            } else {
                                skipped += 1;
                            }
                            device_record_pair(&state2, &device_id, src, &rel);
                        }
                        _ => failed += 1,
                    }
                }
                set_row_label(&row_base);
                progress2.set_visible(false);
                update_card2(&backend, None);
                if sel2.borrow().as_deref() == Some(backend.as_str()) {
                    eject2.set_sensitive(dev_ejectable);
                }
                reload2(dev_for_reload.clone());
                show_alert_parented(
                    win2.upgrade().as_ref(),
                    &format!("Copied {copied}, skipped {skipped}, failed {failed} to {dname}."),
                );
            });
        })
    };
    *copy_files_holder.borrow_mut() = Some(copy_files_run.clone());

    dev_detail.append(&dev_search_row);

    let dev_tracks_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Automatic)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .child(&dev_col_view)
        .build();
    dev_detail.append(&dev_tracks_scroll);

    // ── Bottom action row for the device track view ──────────────────────────
    // Left: add files to the device + delete/remove the selected files. Right
    // (aligned like the rest of the Media Library): play / enqueue the selection.
    let dev_file_add = Button::with_label("Add Files…");
    let dev_file_play = Button::with_label("Play");
    let dev_file_enqueue = Button::with_label("Enqueue");
    for b in [&dev_file_add, &dev_file_play, &dev_file_enqueue] {
        b.add_css_class("pl-btn");
    }
    let dev_file_actions = GtkBox::new(Orientation::Horizontal, 6);
    dev_file_actions.append(&dev_file_add);
    dev_file_actions.append(&dev_file_remove);
    // dev_hint is the flexible middle element: empty (a spacer) when idle, live
    // copy status while files copy.
    dev_file_actions.append(&dev_hint);
    dev_file_actions.append(&dev_file_play);
    dev_file_actions.append(&dev_file_enqueue);
    dev_detail.append(&dev_file_actions);

    // Collect the currently-selected device track rows (full LibTrack, so
    // already-known metadata like duration carries into the active playlist).
    let selected_device_tracks: Rc<dyn Fn() -> Vec<crate::media_library::LibTrack>> = {
        let sel = dev_selection.clone();
        let model = dev_sort_model.clone();
        Rc::new(move || {
            let mut out = Vec::new();
            for i in 0..model.n_items() {
                if !sel.is_selected(i) {
                    continue;
                }
                if let Some(t) = model.item(i).and_downcast::<glib::BoxedAnyObject>() {
                    out.push(t.borrow::<crate::media_library::LibTrack>().clone());
                }
            }
            out
        })
    };

    // Enable the Delete/Remove button only while one or more files are selected.
    {
        let remove_btn = dev_file_remove.clone();
        let sel_tracks = selected_device_tracks.clone();
        dev_selection.connect_selection_changed(move |_, _, _| {
            remove_btn.set_sensitive(!sel_tracks().is_empty());
        });
    }

    // Add Files…: pick audio files and copy them to the device Music folder.
    {
        let get_dev = current_device_for_actions.clone();
        let copy = copy_files_run.clone();
        let win_wk = win.downgrade();
        dev_file_add.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            let dialog = gtk4::FileDialog::builder().title("Add Files to Device").build();
            let copy2 = copy.clone();
            let dev2 = dev.clone();
            dialog.open_multiple(
                win_wk.upgrade().as_ref(),
                None::<&gio::Cancellable>,
                move |res| {
                    let Ok(files) = res else { return };
                    let paths: Vec<std::path::PathBuf> = (0..files.n_items())
                        .filter_map(|i| files.item(i).and_downcast::<gio::File>())
                        .filter_map(|f| f.path())
                        .collect();
                    if !paths.is_empty() {
                        copy2(dev2.clone(), paths);
                    }
                },
            );
        });
    }

    // Play: replace the active playlist with the selected device files and play
    // from the first one (so "Play" plays just the selection, not whatever was
    // queued before). Built from the device LibTrack so known duration/tags
    // show immediately rather than "-:--" until played.
    {
        let sel_tracks = selected_device_tracks.clone();
        let state = state.clone();
        let rebuild = rebuild_playlist.clone();
        dev_file_play.connect_clicked(move |_| {
            let tracks = sel_tracks();
            if tracks.is_empty() {
                return;
            }
            let _ = state.borrow_mut().player.stop();
            state.borrow_mut().playlist.clear();
            for lt in &tracks {
                state.borrow_mut().playlist.add(crate::model::Track::from(lt));
            }
            if !state.borrow().playlist.is_empty() {
                state.borrow_mut().play_current();
            }
            rebuild();
        });
    }

    // Enqueue: append the selected device files to the active playlist, carrying
    // the device row's known metadata (duration etc.) so it shows immediately.
    {
        let sel_tracks = selected_device_tracks.clone();
        let state = state.clone();
        let rebuild = rebuild_playlist.clone();
        dev_file_enqueue.connect_clicked(move |_| {
            let tracks = sel_tracks();
            if tracks.is_empty() {
                return;
            }
            let was_empty = state.borrow().playlist.is_empty();
            for lt in &tracks {
                state.borrow_mut().playlist.add(crate::model::Track::from(lt));
            }
            if state.borrow().config.behavior.autoplay_on_add && was_empty {
                state.borrow_mut().play_current();
            }
            rebuild();
        });
    }

    // Delete / Remove on the selected device files. Behaviour depends on the
    // active view:
    //   • All files  → "Delete": permanently delete the files from the device
    //     AND drop them from every device playlist (Deletion Rule — allowed from
    //     this Media Library external-device view, after confirmation).
    //   • A playlist → "Remove": drop the files from THAT playlist only; the
    //     files stay on the device and in other playlists.
    {
        let sel_tracks = selected_device_tracks.clone();
        let get_dev = current_device_for_actions.clone();
        let reload_store = reload_device_store.clone();
        let reload_pls = reload_dev_playlists.clone();
        let apply_filter = apply_pl_filter.clone();
        let sel_pl = selected_dev_playlist.clone();
        let win_wk = win.downgrade();
        dev_file_remove.connect_clicked(move |_| {
            let Some(dev) = get_dev() else { return };
            let paths: Vec<std::path::PathBuf> = sel_tracks()
                .iter()
                .map(|t| std::path::PathBuf::from(&t.path))
                .collect();
            if paths.is_empty() {
                return;
            }
            let n = paths.len();
            let in_playlist = sel_pl.borrow().clone();

            let (message, detail, confirm) = if let Some(pl) = &in_playlist {
                let pl_name = pl
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (
                    format!(
                        "Remove {n} file{} from \"{pl_name}\"?",
                        if n == 1 { "" } else { "s" }
                    ),
                    "The file(s) stay on the device and in any other playlist.".to_string(),
                    "Remove".to_string(),
                )
            } else {
                (
                    format!(
                        "Delete {n} file{} from the device?",
                        if n == 1 { "" } else { "s" }
                    ),
                    "The file(s) are permanently deleted from the device and removed from every \
                     playlist. This can't be undone."
                        .to_string(),
                    "Delete".to_string(),
                )
            };

            let dialog = gtk4::AlertDialog::builder()
                .message(message)
                .detail(detail)
                .buttons(vec!["Cancel".to_string(), confirm])
                .cancel_button(0)
                .default_button(0)
                .modal(true)
                .build();
            let reload_store2 = reload_store.clone();
            let reload_pls2 = reload_pls.clone();
            let apply_filter2 = apply_filter.clone();
            let dev2 = dev.clone();
            let win_wk2 = win_wk.clone();
            let in_playlist2 = in_playlist.clone();
            dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |res| {
                if res != Ok(1) {
                    return;
                }
                match &in_playlist2 {
                    Some(pl_path) => {
                        // Remove from this playlist only — rewrite its .m3u8.
                        let basenames: std::collections::HashSet<String> = paths
                            .iter()
                            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
                            .map(|s| s.to_string())
                            .collect();
                        device_m3u_remove_basenames(pl_path, &basenames);
                        // Re-apply the filter so the removed rows disappear.
                        apply_filter2(&pl_path.to_string_lossy());
                    }
                    None => {
                        // Delete off the device + drop from every playlist.
                        let failed = device_delete_files(&dev2, &paths);
                        reload_store2(dev2.clone());
                        reload_pls2(dev2.clone());
                        if failed > 0 {
                            show_alert_parented(
                                win_wk2.upgrade().as_ref(),
                                &format!("{failed} file(s) couldn't be deleted."),
                            );
                        }
                    }
                }
            });
        });
    }

    // Drop target on the device track list: dropping files (from the active
    // playlist, files view, or editor) copies them to the device currently
    // shown in the detail view; dropping a playlist row sends the playlist.
    // Same routing as the sidebar device row, just with a fixed target.
    {
        let dt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        dt.set_types(&[gdk::FileList::static_type(), glib::Type::STRING]);
        let sel_backend_drop = selected_dev_backend.clone();
        let current_devices_drop = current_devices.clone();
        let state_drop = state.clone();
        let copy_holder = copy_files_holder.clone();
        let send_holder = send_playlist_holder.clone();
        dt.connect_drop(move |_, value, _x, _y| {
            // Resolve the device currently shown in the detail view.
            let Some(backend) = sel_backend_drop.borrow().clone() else {
                return false;
            };
            let Some(dev) = current_devices_drop
                .borrow()
                .iter()
                .find(|d| d.backend_id == backend)
                .cloned()
            else {
                return false;
            };

            // A playlist row (`pl:<id>` String) → send the whole playlist.
            if let Ok(s) = value.get::<String>() {
                if let Some(pid) = s.strip_prefix("pl:").and_then(|n| n.trim().parse::<i64>().ok())
                {
                    let plname = state_drop
                        .borrow()
                        .media_lib
                        .as_ref()
                        .and_then(|l| l.playlist_by_id(pid).ok())
                        .map(|p| p.name)
                        .unwrap_or_default();
                    if let Some(send) = send_holder.borrow().as_ref() {
                        send(dev, pid, plname);
                        return true;
                    }
                    return false;
                }
                // Otherwise a uri/path-list String → copy those files.
                let paths: Vec<std::path::PathBuf> = s
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(|l| {
                        if l.starts_with("file://") {
                            gio::File::for_uri(l)
                                .path()
                                .unwrap_or_else(|| std::path::PathBuf::from(l))
                        } else {
                            std::path::PathBuf::from(l)
                        }
                    })
                    .collect();
                if paths.is_empty() {
                    return false;
                }
                if let Some(copy) = copy_holder.borrow().as_ref() {
                    copy(dev, paths);
                    return true;
                }
                return false;
            }

            // A FileList drag → copy the dragged files.
            if let Ok(file_list) = value.get::<gdk::FileList>() {
                let paths: Vec<std::path::PathBuf> =
                    file_list.files().iter().filter_map(|f| f.path()).collect();
                if paths.is_empty() {
                    return false;
                }
                if let Some(copy) = copy_holder.borrow().as_ref() {
                    copy(dev, paths);
                    return true;
                }
            }
            false
        });
        dev_tracks_scroll.add_controller(dt);
    }

    // ── Right-click context menu on device files: View / Edit ID3 ────────────
    // Mirrors the active-playlist menu. The ID3 editor also shows/edits album
    // art, so this one item covers viewing artwork too. Operates on the current
    // selection (like the Play / Enqueue / Delete buttons in this view); the
    // editor binds one file, so the item appears only for a single selection.
    // Gesture + action group live on the ScrolledWindow, not the ColumnView, to
    // dodge the GTK4 bug where a PopoverMenu parented on the view misses hover.
    {
        let ctx_click = GestureClick::new();
        ctx_click.set_button(3); // right mouse button

        let dev_file_action_group = gio::SimpleActionGroup::new();
        dev_tracks_scroll.insert_action_group("dev-file", Some(&dev_file_action_group));

        let action_id3 = gio::SimpleAction::new("edit-id3", None);
        {
            let state_id3 = state.clone();
            let win_id3 = win.downgrade();
            let sel_tracks = selected_device_tracks.clone();
            let reload_store = reload_device_store.clone();
            let current_devices_id3 = current_devices.clone();
            let sel_backend_id3 = selected_dev_backend.clone();
            action_id3.connect_activate(move |_, _| {
                let tracks = sel_tracks();
                let [track] = tracks.as_slice() else { return };
                let path = std::path::PathBuf::from(&track.path);
                // Re-read the edited device file's row so new tags show.
                let reload = reload_store.clone();
                let devices = current_devices_id3.clone();
                let backend = sel_backend_id3.clone();
                let rebuild_cb: Rc<dyn Fn()> = Rc::new(move || {
                    let Some(b) = backend.borrow().clone() else { return };
                    if let Some(dev) =
                        devices.borrow().iter().find(|d| d.backend_id == b).cloned()
                    {
                        reload(dev);
                    }
                });
                open_id3_editor_window(
                    win_id3.upgrade().as_ref(),
                    path,
                    state_id3.clone(),
                    rebuild_cb,
                    None,
                );
            });
        }
        dev_file_action_group.add_action(&action_id3);

        let sel_menu = selected_device_tracks.clone();
        let scroll_menu = dev_tracks_scroll.clone();
        ctx_click.connect_pressed(move |gest, _, x, y| {
            // Only a single-file selection is editable (the editor binds one file).
            if sel_menu().len() != 1 {
                return;
            }
            let menu = gio::Menu::new();
            menu.append_item(&gio::MenuItem::new(
                Some("🎵 View / Edit ID3"),
                Some("dev-file.edit-id3"),
            ));
            let popover = gtk4::PopoverMenu::from_model(Some(&menu));
            popover.set_parent(&scroll_menu);
            // Unparent on close so a right-click doesn't leak a popover per use.
            popover.connect_closed(|p| p.unparent());
            let rect = gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            popover.set_pointing_to(Some(&rect));
            popover.popup();
            gest.set_state(gtk4::EventSequenceState::Claimed);
        });
        dev_tracks_scroll.add_controller(ctx_click);
    }

    dev_page.append(&dev_detail);

    let _vsep_unused = (); // replaced by Paned divider

    // ── "Disc Drives" content page (optical drives; Phase 1: play) ────────
    // Overview (one card per drive) + detail (audio track list + add actions).
    let disc_page = GtkBox::new(Orientation::Vertical, 8);
    disc_page.set_margin_top(8);
    disc_page.set_margin_start(8);
    disc_page.set_margin_end(8);

    // Overview: shown when the Disc Drives header is selected.
    let disc_overview = GtkBox::new(Orientation::Vertical, 6);
    let disc_overview_title = Label::builder()
        .label("Disc Drives")
        .halign(Align::Start)
        .xalign(0.0)
        .build();
    disc_overview_title.add_css_class("ml-section-header");
    disc_overview.append(&disc_overview_title);
    let disc_overview_list = GtkBox::new(Orientation::Vertical, 12);
    disc_overview_list.set_margin_top(6);
    disc_overview.append(&disc_overview_list);
    disc_page.append(&disc_overview);

    // Detail: the selected drive (hidden until one is picked).
    let disc_detail = GtkBox::new(Orientation::Vertical, 8);
    disc_detail.set_visible(false);
    let disc_title = Label::builder().halign(Align::Start).xalign(0.0).build();
    disc_title.add_css_class("ml-section-header");
    disc_detail.append(&disc_title);
    let disc_media_lbl = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .build();
    disc_media_lbl.add_css_class("dim-label");
    disc_detail.append(&disc_media_lbl);
    // "Artist — Album" once the disc has gnudb/edited tags (hidden otherwise).
    let disc_tag_lbl = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .build();
    disc_tag_lbl.add_css_class("ml-section-header");
    disc_tag_lbl.set_visible(false);
    disc_detail.append(&disc_tag_lbl);
    // Banner shown for non-audio media (no disc / blank / data).
    let disc_banner = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .build();
    disc_banner.add_css_class("broken");
    disc_banner.set_visible(false);
    disc_detail.append(&disc_banner);
    // Audio-track list: multi-select rows "Track N — MM:SS".
    let disc_track_list = gtk4::ListBox::new();
    disc_track_list.set_selection_mode(gtk4::SelectionMode::Multiple);
    // Single click only selects (for Add Selected); a double-click activates a
    // row to add just that track — matching the established double-click add.
    disc_track_list.set_activate_on_single_click(false);
    disc_track_list.add_css_class("ml-col-view");
    // Search filters just this disc's tracks. The filter hides rows without
    // reindexing them, so row.index() keeps mapping onto the entries store
    // (Add Selected, double-click add, rip preselection all stay correct).
    let disc_search_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let (disc_search_row, disc_search_entry) =
        make_view_search_row("Search this disc — track title…");
    {
        let q = disc_search_query.clone();
        let entries_store = current_disc_entries.clone();
        disc_track_list.set_filter_func(move |row| {
            let q = q.borrow();
            if q.is_empty() {
                return true;
            }
            let idx = row.index();
            if idx < 0 {
                return true;
            }
            entries_store
                .borrow()
                .get(idx as usize)
                .map(|e| e.title.to_lowercase().contains(q.as_str()))
                .unwrap_or(true)
        });
    }
    {
        let q = disc_search_query.clone();
        let list = disc_track_list.clone();
        disc_search_entry.connect_changed(move |e| {
            *q.borrow_mut() = e.text().to_lowercase();
            list.invalidate_filter();
        });
    }
    disc_detail.append(&disc_search_row);
    let disc_tracks_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Automatic)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .child(&disc_track_list)
        .build();
    disc_detail.append(&disc_tracks_scroll);
    // Add + identify/rip/tag/eject actions. Order matches the macOS drive
    // header (Identify · Rip… · Edit Tags · … · Eject last), with the GTK-only
    // Add buttons in front.
    let disc_add_sel = Button::with_label("Add Selected");
    let disc_add_all = Button::with_label("Add All");
    let disc_identify = Button::with_label("Identify");
    let disc_rip = Button::with_label("Rip…");
    let disc_edit_tags = Button::with_label("Edit Tags");
    // Shown only when the disc is unknown to gnudb or the user's tags differ
    // from the official match (visibility set in populate_disc_detail).
    let disc_submit = Button::with_label("Submit to gnudb");
    let disc_eject = Button::with_label("Eject");
    for b in [
        &disc_add_sel,
        &disc_add_all,
        &disc_identify,
        &disc_rip,
        &disc_edit_tags,
        &disc_submit,
        &disc_eject,
    ] {
        b.add_css_class("pl-btn");
    }
    let disc_actions = GtkBox::new(Orientation::Horizontal, 6);
    disc_actions.append(&disc_add_sel);
    disc_actions.append(&disc_add_all);
    disc_actions.append(&disc_identify);
    disc_actions.append(&disc_rip);
    disc_actions.append(&disc_edit_tags);
    disc_actions.append(&disc_submit);
    disc_actions.append(&disc_eject);
    disc_detail.append(&disc_actions);
    // Rip progress row (hidden unless a rip is running): a bar + Cancel.
    let disc_rip_box = GtkBox::new(Orientation::Horizontal, 6);
    disc_rip_box.set_visible(false);
    let disc_rip_bar = gtk4::ProgressBar::new();
    disc_rip_bar.set_hexpand(true);
    disc_rip_bar.set_show_text(true);
    let disc_rip_cancel = Button::with_label("Cancel");
    disc_rip_cancel.add_css_class("pl-btn");
    disc_rip_box.append(&disc_rip_bar);
    disc_rip_box.append(&disc_rip_cancel);
    disc_detail.append(&disc_rip_box);
    // Transient status for gnudb lookups + rip results.
    let disc_status_lbl = Label::builder()
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .build();
    disc_status_lbl.add_css_class("dim-label");
    disc_detail.append(&disc_status_lbl);
    disc_page.append(&disc_detail);

    // ── Content stack ─────────────────────────────────────────────────────
    let stack = Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_transition_type(StackTransitionType::None);
    stack.add_named(&dev_page, Some("devices"));
    stack.add_named(&disc_page, Some("discs"));

    // Holders so close_request can save Files-tab state (col_view and all_cols are
    // defined inside the Files block scope below).
    let col_view_holder: Rc<RefCell<Option<ColumnView>>> = Rc::new(RefCell::new(None));
    let all_cols_holder: Rc<RefCell<Vec<(String, ColumnViewColumn)>>> =
        Rc::new(RefCell::new(Vec::new()));

    // ── Page: Files ──────────────────────────────────────────────────────
    {
        let files_vbox = GtkBox::new(Orientation::Vertical, 4);

        let search_entry = Entry::new();
        search_entry.set_placeholder_text(Some("Search artist, title, album…"));
        search_entry.set_hexpand(true);

        let search_clear_btn = Button::with_label("✕");
        search_clear_btn.add_css_class("pl-btn");
        {
            let e = search_entry.clone();
            search_clear_btn.connect_clicked(move |_| {
                e.set_text("");
            });
        }

        let search_row = GtkBox::new(Orientation::Horizontal, 4);
        search_row.set_margin_top(4);
        search_row.set_margin_start(4);
        search_row.set_margin_end(4);
        search_row.append(&search_entry);
        search_row.append(&search_clear_btn);
        files_vbox.append(&search_row);

        let track_store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let sort_model = SortListModel::new(Some(track_store.clone()), None::<gtk4::Sorter>);
        let multi_sel = MultiSelection::new(Some(sort_model.clone()));
        let col_view = ColumnView::new(Some(multi_sel.clone()));
        col_view.add_css_class("ml-col-view");
        col_view.set_show_row_separators(false);
        col_view.set_show_column_separators(false);
        col_view.set_hexpand(true);
        col_view.set_vexpand(true);
        col_view.set_reorderable(true);

        // Create action group and actions for ML right-click menu
        let ml_action_group = gio::SimpleActionGroup::new();
        col_view.insert_action_group("ml", Some(&ml_action_group));

        // Store for selected tracks (used by action handlers)
        let ml_selected_tracks: Rc<RefCell<Vec<std::path::PathBuf>>> =
            Rc::new(RefCell::new(Vec::new()));

        // Append to Playlist action
        let ml_action_append_state = state.clone();
        let _ml_action_append_sel = multi_sel.clone();
        let ml_action_append_rebuild = rebuild_playlist.clone();
        let ml_action_append_tracks = ml_selected_tracks.clone();
        let action_append = gio::SimpleAction::new("append", None); // Note: action name without "ml." prefix
        action_append.connect_activate(move |_, _| {
            let tracks: Vec<_> = ml_action_append_tracks.borrow().clone();
            if tracks.is_empty() {
                return;
            }
            let was_empty = ml_action_append_state.borrow().playlist.is_empty();
            for path in tracks {
                let track = crate::model::Track::from_path(&path).ok();
                if let Some(track) = track {
                    ml_action_append_state.borrow_mut().playlist.add(track);
                }
            }
            if ml_action_append_state
                .borrow()
                .config
                .behavior
                .autoplay_on_add
                && was_empty
            {
                ml_action_append_state.borrow_mut().play_current();
            }
            ml_action_append_rebuild();
        });
        ml_action_group.add_action(&action_append);

        // Replace current playlist action
        let ml_action_replace_state = state.clone();
        let ml_action_replace_tracks = ml_selected_tracks.clone();
        let ml_action_replace_rebuild = rebuild_playlist.clone();
        let action_replace = gio::SimpleAction::new("replace", None); // Note: action name without "ml." prefix
        action_replace.connect_activate(move |_, _| {
            let tracks: Vec<_> = ml_action_replace_tracks.borrow().clone();
            if tracks.is_empty() {
                return;
            }
            let _ = ml_action_replace_state.borrow_mut().player.stop();
            ml_action_replace_state.borrow_mut().playlist.clear();
            for path in tracks {
                let track = crate::model::Track::from_path(&path).ok();
                if let Some(track) = track {
                    ml_action_replace_state.borrow_mut().playlist.add(track);
                }
            }
            if ml_action_replace_state
                .borrow()
                .config
                .behavior
                .autoplay_on_add
                && !ml_action_replace_state.borrow().playlist.is_empty()
            {
                ml_action_replace_state.borrow_mut().play_current();
            }
            ml_action_replace_rebuild();
        });
        ml_action_group.add_action(&action_replace);

        // View/Edit ID3 Info action (for single selection)
        let ml_action_id3_state = state.clone();
        let ml_action_id3_tracks = ml_selected_tracks.clone();
        let ml_action_id3_rebuild = rebuild_playlist.clone();
        let action_id3 = gio::SimpleAction::new("edit-id3", None); // Note: action name without "ml." prefix
        action_id3.connect_activate(move |_, _| {
            let tracks: Vec<_> = ml_action_id3_tracks.borrow().clone();
            if tracks.is_empty() {
                return;
            }
            // Only open for the first (single) selected track
            let path = tracks[0].clone();
            open_id3_editor_window(
                None::<&gtk4::Window>,
                path,
                ml_action_id3_state.clone(),
                ml_action_id3_rebuild.clone(),
                None,
            );
        });
        ml_action_group.add_action(&action_id3);

        // Rescan Metadata action
        let ml_action_rescan_state = state.clone();
        let ml_action_rescan_tracks = ml_selected_tracks.clone();
        let action_rescan = gio::SimpleAction::new("rescan", None); // Note: action name without "ml." prefix
        action_rescan.connect_activate(move |_, _| {
            let tracks: Vec<_> = ml_action_rescan_tracks.borrow().clone();
            if tracks.is_empty() {
                return;
            }
            if ml_action_rescan_state.borrow().ml_scan.is_some() {
                return;
            }
            let paths: Vec<String> = tracks
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let total = paths.len();
            let cancel_flag = start_ml_scan(&ml_action_rescan_state, ScanType::AddFiles, total);
            let (progress_tx, progress_rx) = std::sync::mpsc::channel();
            let (result_tx, result_rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let db_path = crate::media_library::MediaLibrary::db_path_pub();
                let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
                    Ok(l) => l,
                    Err(_) => {
                        let _ = result_tx.send(());
                        return;
                    }
                };
                for (i, path) in paths.iter().enumerate() {
                    if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let _ = lib.rescan_track(path);
                    let _ = progress_tx.send(i + 1);
                }
                let _ = result_tx.send(());
            });
            let progress_rx = std::cell::RefCell::new(progress_rx);
            let result_rx = std::cell::RefCell::new(result_rx);
            let state_for_timer = ml_action_rescan_state.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                while let Ok(current) = progress_rx.borrow().try_recv() {
                    update_ml_scan_progress(&state_for_timer, current, total);
                }
                if result_rx.borrow().try_recv().is_ok() {
                    {
                        let mut s = state_for_timer.borrow_mut();
                        s.media_lib = crate::media_library::MediaLibrary::open().ok();
                    }
                    complete_ml_scan(&state_for_timer);
                    if let Some(ref cb) = state_for_timer.borrow().rebuild_ml_callback {
                        cb();
                    }
                    return glib::ControlFlow::Break;
                }
                glib::ControlFlow::Continue
            });
        });
        ml_action_group.add_action(&action_rescan);

        // Remove from Media Library action
        let ml_action_remove_tracks = ml_selected_tracks.clone();
        let ml_action_remove_store = track_store.clone();
        let action_remove = gio::SimpleAction::new("remove", None);
        action_remove.connect_activate(move |_, _| {
            let paths = ml_action_remove_tracks.borrow().clone();
            if paths.is_empty() {
                return;
            }

            let path_set: std::collections::HashSet<String> = paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let paths_owned: Vec<String> = path_set.iter().cloned().collect();

            let db_path = crate::media_library::MediaLibrary::db_path_pub();
            std::thread::spawn(move || {
                if let Ok(lib) = crate::media_library::MediaLibrary::open_at(&db_path) {
                    let _ = lib.soft_delete_tracks_by_paths(&paths_owned);
                    let _ = lib.purge_deleted_tracks();
                }
            });

            let mut rows_to_remove: Vec<u32> = Vec::new();
            for i in 0..ml_action_remove_store.n_items() {
                if let Some(item) = ml_action_remove_store.item(i) {
                    if let Some(boxed) = item.downcast_ref::<glib::BoxedAnyObject>() {
                        let track = boxed.borrow::<crate::media_library::LibTrack>();
                        if path_set.contains(&track.path) {
                            rows_to_remove.push(i);
                        }
                    }
                }
            }

            for idx in rows_to_remove.into_iter().rev() {
                ml_action_remove_store.remove(idx);
            }
        });
        ml_action_group.add_action(&action_remove);

        // Seed a brand new saved playlist from the current ML selection.
        let ml_action_new_state  = state.clone();
        let ml_action_new_tracks = ml_selected_tracks.clone();
        let ml_action_new_win    = win.clone();
        let action_add_to_new    = gio::SimpleAction::new("add-to-new", None);
        action_add_to_new.connect_activate(move |_, _| {
            let paths: Vec<String> = ml_action_new_tracks.borrow()
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            if paths.is_empty() { return }
            let default_stem = glib::DateTime::now_local()
                .ok()
                .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Playlist".to_string());
            let state_cb = ml_action_new_state.clone();
            let paths_cb = paths.clone();
            run_playlist_save_dialog(
                ml_action_new_state.clone(),
                ml_action_new_win.clone(),
                &default_stem,
                move |path, win_cb| {
                    if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                        if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths_cb) {
                            eprintln!("save_playlist_tracks_to_path: {e}");
                            show_playlist_save_error(&win_cb, &path, &e);
                        }
                    }
                },
            );
        });
        ml_action_group.add_action(&action_add_to_new);

        // Add-to-saved-playlist action (parameterised by target playlist id).
        // Append currently selected ML file paths to the chosen saved playlist.
        let ml_action_add_state = state.clone();
        let ml_action_add_tracks = ml_selected_tracks.clone();
        let action_add_to_saved = gio::SimpleAction::new(
            "add-to-saved",
            Some(glib::VariantTy::INT64),
        );
        action_add_to_saved.connect_activate(move |_, param| {
            let Some(pid) = param.and_then(|p| p.get::<i64>()) else { return };
            let paths: Vec<String> = ml_action_add_tracks.borrow()
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            if paths.is_empty() { return }
            let mut ok = false;
            if let Some(lib) = ml_action_add_state.borrow().media_lib.as_ref() {
                match lib.append_paths_to_playlist(pid, &paths) {
                    Ok(_)  => ok = true,
                    Err(e) => eprintln!("append_paths_to_playlist {pid}: {e}"),
                }
            }
            if ok { notify_playlist_changed(pid); }
        });
        ml_action_group.add_action(&action_add_to_saved);

        let col_defs: &[(&str, &str, i32, bool)] = ALL_COLUMNS
            .iter()
            .map(|c| (c.id, c.header, 80, c.expand))
            .collect::<Vec<_>>()
            .leak();

        let visible_ids: Vec<String> = state.borrow().config.media_library.visible_columns.clone();
        let saved_widths: std::collections::HashMap<String, i32> =
            state.borrow().config.media_library.ml_file_col_widths.clone();

        // Track which artwork buttons have been connected to avoid duplicate click handlers
        // (connect_bind fires each time an item is shown after a scroll).
        let connected_artwork: Rc<RefCell<std::collections::HashSet<glib::Object>>> =
            Rc::new(RefCell::new(std::collections::HashSet::new()));

        // Capture store_ref before factory so it's available for the factory's right-click handler
        let store_for_ctx = track_store.clone();

        // ── Unscanned indicator column (always first, always visible) ──────────
        {
            let unscanned_factory = SignalListItemFactory::new();

            unscanned_factory.connect_setup(|_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() {
                    return;
                }
                let lbl = Label::builder()
                    .halign(Align::Center)
                    .valign(Align::Center)
                    .css_classes(["ml-col-label"])
                    .build();
                li.set_child(Some(&lbl));
            });

            unscanned_factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let boxed = li
                    .item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok());
                let Some(boxed) = boxed else {
                    return;
                };
                let t = boxed.borrow::<crate::media_library::LibTrack>();
                let lbl = li.child().and_then(|c| c.downcast::<Label>().ok());
                let Some(lbl) = lbl else {
                    return;
                };
                let path = std::path::Path::new(&t.path);
                // A row can carry a `last_scanned` timestamp yet have no real
                // metadata: `update_last_scanned` runs after every scan pass
                // even when extraction produced nothing (e.g. the duration
                // probe failed). So "scanned" for the status glyph means
                // metadata was actually extracted — duration is the reliable
                // tell — not merely that a timestamp exists.
                //   ❓ never (properly) scanned — no metadata
                //   🔄 scanned, but the file changed since (rescan to refresh)
                //   🔒 read-only
                let scanned = t.length_secs.is_some() && t.last_scanned.is_some();
                if !scanned {
                    lbl.set_label("❓");
                    lbl.set_tooltip_text(Some(
                        "Not scanned yet — metadata loads on the next scan",
                    ));
                } else if crate::media_library::MediaLibrary::needs_metadata_scan(
                    &t.path,
                    t.last_scanned.as_deref(),
                ) {
                    lbl.set_label("🔄");
                    lbl.set_tooltip_text(Some(
                        "File changed since last scan — rescan to refresh its metadata",
                    ));
                } else if crate::media_library::is_read_only(path) {
                    lbl.set_label("🔒");
                    lbl.set_tooltip_text(Some("Read-only file"));
                } else {
                    lbl.set_label("");
                    lbl.set_tooltip_text(None);
                }
            });

            let unscanned_col = ColumnViewColumn::new(Some(""), Some(unscanned_factory));
            unscanned_col.set_fixed_width(24);
            col_view.append_column(&unscanned_col);
        }

        let all_cols: Vec<(String, ColumnViewColumn)> = col_defs
            .iter()
            .map(|(id, header, _min_w, expand)| {
                let factory = SignalListItemFactory::new();
                let id_str = id.to_string();
                let is_artwork = id_str == "artwork_path";
                let connected = connected_artwork.clone();
                let ctx_multi_sel = multi_sel.clone();
                let ctx_col_view = col_view.clone();
                let _ctx_store = store_for_ctx.clone();
                let ml_tracks_gest = ml_selected_tracks.clone();
                let state_for_ctx = state.clone();

                factory.connect_setup(move |_, obj| {
                    let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();

                    // Skip if child already exists (row is being recycled)
                    if li.child().is_some() {
                        return;
                    }

                    let child: gtk4::Widget;

                    if is_artwork {
                        let btn = Button::builder()
                            .label("View")
                            .halign(Align::Start)
                            .margin_start(6)
                            .margin_end(6)
                            .margin_top(3)
                            .margin_bottom(3)
                            .hexpand(true)
                            .vexpand(true)
                            .halign(Align::Fill)
                            .valign(Align::Fill)
                            .build();
                        btn.add_css_class("link");
                        child = btn.upcast::<gtk4::Widget>();
                    } else {
                        let lbl = Label::builder()
                            .margin_start(6)
                            .margin_end(6)
                            .margin_top(3)
                            .margin_bottom(3)
                            .hexpand(true)
                            .vexpand(true)
                            .halign(Align::Fill)
                            .valign(Align::Fill)
                            .xalign(0.0)
                            .ellipsize(gtk4::pango::EllipsizeMode::End)
                            .css_classes(["ml-col-label"])
                            .build();
                        child = lbl.upcast::<gtk4::Widget>();
                    }

                    // Per-cell DragSource — collects every currently-selected
                    // ML row as a FileList content provider so the user can
                    // drag library tracks out to the active playlist's
                    // pl_scroll drop target (which accepts FileList).  Plain
                    // single-track drag works too: if the row under the
                    // pointer is not in the selection it still ships its
                    // own path.
                    {
                        let ds = gtk4::DragSource::new();
                        ds.set_actions(gtk4::gdk::DragAction::COPY);
                        let ds_sel = ctx_multi_sel.clone();
                        let ds_li  = li.clone();
                        ds.connect_prepare(move |_, _, _| {
                            let mut paths: Vec<std::path::PathBuf> = Vec::new();
                            let mut self_path: Option<std::path::PathBuf> = None;
                            if let Some(obj) = ds_li.item()
                                .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            {
                                let t = obj.borrow::<crate::media_library::LibTrack>();
                                self_path = Some(std::path::PathBuf::from(&t.path));
                            }
                            for i in 0..ds_sel.n_items() {
                                if ds_sel.is_selected(i) {
                                    if let Some(obj) = ds_sel.item(i)
                                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                    {
                                        let t = obj.borrow::<crate::media_library::LibTrack>();
                                        paths.push(std::path::PathBuf::from(&t.path));
                                    }
                                }
                            }
                            if paths.is_empty() {
                                if let Some(p) = self_path { paths.push(p); }
                            }
                            if paths.is_empty() { return None }
                            let files: Vec<gio::File> = paths.iter()
                                .map(|p| gio::File::for_path(p))
                                .collect();
                            let fl = gdk::FileList::from_array(&files);
                            Some(gdk::ContentProvider::for_value(&fl.to_value()))
                        });
                        child.add_controller(ds);
                    }

                    // Add right-click gesture to each row.  Capture phase
                    // pre-empts ColumnView's default secondary-button
                    // handler so multi-selection survives long enough for
                    // our is_selected guard to inspect it.
                    let gesture = gtk4::GestureClick::new();
                    gesture.set_button(gtk4::gdk::BUTTON_SECONDARY);
                    gesture.set_propagation_phase(gtk4::PropagationPhase::Capture);
                    let sel_gest = ctx_multi_sel.clone();
                    let col_popup = ctx_col_view.clone();
                    let li_gest = li.clone();
                    let ml_tracks_for_gest = ml_tracks_gest.clone();
                    let state_for_gest = state_for_ctx.clone();
                    gesture.connect_pressed(move |gest, n_press, x, y| {
                        if n_press != 1 {
                            return;
                        }
                        // Get the item directly from the ListItem - no coordinate math needed!
                        let Some(item) = li_gest.item() else {
                            return;
                        };
                        let item_clone = item.clone();

                        // Find the index of the clicked item by checking each item
                        let mut clicked_index: Option<u32> = None;
                        for i in 0..sel_gest.n_items() {
                            if let Some(model_item) = sel_gest.item(i) {
                                if model_item == item_clone {
                                    clicked_index = Some(i);
                                    break;
                                }
                            }
                        }

                        // Only change selection if clicked on non-selected item
                        // This preserves multi-selection when right-clicking on selected items
                        if let Some(idx) = clicked_index {
                            if !sel_gest.is_selected(idx) {
                                sel_gest.unselect_all();
                                sel_gest.select_item(idx, true);
                            }
                        }

                        // Collect selected tracks into shared state for action handlers
                        let mut paths: Vec<std::path::PathBuf> = Vec::new();
                        let mut selected_count = 0usize;
                        for i in 0..sel_gest.n_items() {
                            if sel_gest.is_selected(i) {
                                if let Some(obj) = sel_gest
                                    .item(i)
                                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                {
                                    let t = obj.borrow::<crate::media_library::LibTrack>();
                                    paths.push(std::path::PathBuf::from(&t.path));
                                    selected_count += 1;
                                }
                            }
                        }
                        *ml_tracks_for_gest.borrow_mut() = paths;

                        // Convert coordinates from gesture widget to ColumnView
                        // The gesture gives coords in the child widget's space
                        let child = li_gest.child();
                        let (popup_x, popup_y) = if let Some(child_widget) = child {
                            if let Some((rel_x, rel_y)) =
                                child_widget.translate_coordinates(&col_popup, x, y)
                            {
                                (rel_x, rel_y)
                            } else {
                                (x, y)
                            }
                        } else {
                            (x, y)
                        };

                        // Build menu model
                        let menu = gio::Menu::new();
                        menu.append_item(&gio::MenuItem::new(
                            Some("Append to Playlist"),
                            Some("ml.append"),
                        ));
                        menu.append_item(&gio::MenuItem::new(
                            Some("Replace current playlist"),
                            Some("ml.replace"),
                        ));

                        // Only show View/Edit ID3 for single selection
                        if selected_count == 1 {
                            menu.append_item(&gio::MenuItem::new(
                                Some("View/Edit ID3 Info"),
                                Some("ml.edit-id3"),
                            ));
                        }

                        menu.append_item(&gio::MenuItem::new(
                            Some("Rescan Metadata"),
                            Some("ml.rescan"),
                        ));
                        menu.append_item(&gio::MenuItem::new(
                            Some("Remove from Media Library"),
                            Some("ml.remove"),
                        ));

                        let submenu = build_add_to_playlist_submenu(
                            &state_for_gest,
                            "ml.add-to-new",
                            "ml.add-to-saved",
                        );
                        menu.append_submenu(Some("Add to Playlist"), &submenu);

                        // Create popover menu — NESTED so the "Add to
                        // Playlist" submenu opens as its own popover with
                        // an independent height instead of sliding inside
                        // the parent popover (which would clip it to the
                        // parent's content height).
                        let popover = gtk4::PopoverMenu::from_model_full(
                            &menu,
                            gtk4::PopoverMenuFlags::NESTED,
                        );
                        popover.set_pointing_to(Some(&gtk4::gdk::Rectangle::new(
                            popup_x as i32,
                            popup_y as i32,
                            1,
                            1,
                        )));
                        popover.set_parent(&col_popup);
                        popover.popup();
                        gest.set_state(gtk4::EventSequenceState::Claimed);
                    });
                    child.add_controller(gesture);
                    if li.child().is_none() {
                        li.set_child(Some(&child));
                    }
                });
                factory.connect_bind(move |_, obj| {
                    let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                    let boxed = li
                        .item()
                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok());
                    let Some(boxed) = boxed else {
                        return;
                    };
                    let t = boxed.borrow::<crate::media_library::LibTrack>();

                    if is_artwork {
                        let btn = li.child().and_then(|c| c.downcast::<Button>().ok());
                        if let Some(btn) = btn {
                            let btn_obj = btn.clone().upcast::<glib::Object>();
                            if let Some(ref art_path) = t.artwork_path {
                                btn.set_visible(true);
                                btn.set_sensitive(true);
                                btn.set_label("View");
                                // Only connect once per button instance.
                                if !connected.borrow().contains(&btn_obj) {
                                    let art_clone = art_path.clone();
                                    connected.borrow_mut().insert(btn_obj.clone());
                                    btn.connect_clicked(move |_| {
                                        open_image_viewer(&art_clone);
                                    });
                                }
                            } else {
                                btn.set_visible(false);
                            }
                        }
                        return;
                    }

                    let lbl = li.child().and_then(|c| c.downcast::<Label>().ok());
                    let Some(lbl) = lbl else {
                        return;
                    };
                    let text = match id_str.as_str() {
                        "num" => t.track_num.map(|n| n.to_string()).unwrap_or_default(),
                        "title" => t.title.as_deref().unwrap_or(&t.filename).to_string(),
                        "artist" => t.artist.as_deref().unwrap_or("").to_string(),
                        "album" => t.album.as_deref().unwrap_or("").to_string(),
                        "album_artist" => t.album_artist.as_deref().unwrap_or("").to_string(),
                        "duration" => t
                            .length_secs
                            .map(|s| {
                                let ss = s as u64;
                                format!("{}:{:02}", ss / 60, ss % 60)
                            })
                            .unwrap_or_else(|| "-:--".to_string()),
                        "filename" => t.filename.clone(),
                        "year" => t.year.map(|y| y.to_string()).unwrap_or_default(),
                        "genre" => t.genre.as_deref().unwrap_or("").to_string(),
                        "bitrate" => t.bitrate.map(|b| format!("{b}k")).unwrap_or_default(),
                        "channels" => match t.channels.unwrap_or(0) {
                            1 => "mono".to_string(),
                            2 => "stereo".to_string(),
                            n => format!("{}ch", n),
                        },
                        "path" => t.path.clone(),
                        "play_count" => t.play_count.to_string(),
                        "last_played" => format_last_played(t.last_played.as_deref().unwrap_or("")),
                        "last_scanned" => t.last_scanned.as_deref().unwrap_or("").to_string(),
                        "disc_num" => {
                            let d = t.disc_num.unwrap_or(0);
                            if d == 0 {
                                String::new()
                            } else if let Some(total) = t.disc_total {
                                if total > 0 {
                                    format!("{}/{}", d, total)
                                } else {
                                    d.to_string()
                                }
                            } else {
                                d.to_string()
                            }
                        }
                        "disc_total" => t.disc_total.map(|d| d.to_string()).unwrap_or_default(),
                        "composer" => t.composer.as_deref().unwrap_or("").to_string(),
                        "original_artist" => t.original_artist.as_deref().unwrap_or("").to_string(),
                        "copyright" => t.copyright.as_deref().unwrap_or("").to_string(),
                        "url" => t.url.as_deref().unwrap_or("").to_string(),
                        "encoded_by" => t.encoded_by.as_deref().unwrap_or("").to_string(),
                        "bpm" => t.bpm.as_deref().unwrap_or("").to_string(),
                        "lyric" => {
                            let ly = t.lyric.as_deref().unwrap_or("");
                            if ly.is_empty() {
                                String::new()
                            } else if ly.len() > 30 {
                                format!("{}…", &ly[..30])
                            } else {
                                ly.to_string()
                            }
                        }
                        "comment" => t.comment.as_deref().unwrap_or("").to_string(),
                        "artwork_path" => {
                            if t.artwork_path.is_some() {
                                "Yes".to_string()
                            } else {
                                String::new()
                            }
                        }
                        _ => String::new(),
                    };
                    lbl.set_text(&gtk_safe(&text));
                });

                let col = ColumnViewColumn::new(Some(header), Some(factory));
                col.set_resizable(true);
                if *expand {
                    col.set_expand(true);
                }
                col.set_visible(visible_ids.contains(&id.to_string()));
                if let Some(&w) = saved_widths.get(&id.to_string()) {
                    if w > 0 {
                        col.set_fixed_width(w);
                    }
                }

                let sort_id = id.to_string();
                let sorter = CustomSorter::new(move |a, b| {
                    let a_val = a
                        .downcast_ref::<glib::BoxedAnyObject>()
                        .map(|o| {
                            ml_sort_key(&o.borrow::<crate::media_library::LibTrack>(), &sort_id)
                        })
                        .unwrap_or_default();
                    let b_val = b
                        .downcast_ref::<glib::BoxedAnyObject>()
                        .map(|o| {
                            ml_sort_key(&o.borrow::<crate::media_library::LibTrack>(), &sort_id)
                        })
                        .unwrap_or_default();
                    a_val.cmp(&b_val).into()
                });
                col.set_sorter(Some(&sorter));

                col_view.append_column(&col);
                (id.to_string(), col)
            })
            .collect();
        let all_cols = Rc::new(all_cols);

        // Expose col_view and all_cols for close_request (outside this block scope).
        *col_view_holder.borrow_mut() = Some(col_view.clone());
        *all_cols_holder.borrow_mut() = all_cols.iter().cloned().collect();

        // Restore column order from config (empty list means use default order).
        // The unscanned indicator column is always first (position 0); named
        // columns start at position 1.
        {
            let saved_order = state.borrow().config.media_library.ml_file_col_order.clone();
            if !saved_order.is_empty() {
                // Remove all named columns from their current positions.
                for (_, col) in all_cols.iter() {
                    col_view.remove_column(col);
                }
                // Re-insert in saved order starting after the unscanned column.
                let mut pos = 1u32;
                for col_id in &saved_order {
                    if let Some((_, col)) = all_cols.iter().find(|(id, _)| id == col_id) {
                        col_view.insert_column(pos, col);
                        pos += 1;
                    }
                }
                // Append columns not present in saved_order (e.g. newly added columns).
                for (id, col) in all_cols.iter() {
                    if !saved_order.contains(id) {
                        col_view.insert_column(pos, col);
                        pos += 1;
                    }
                }
            }
        }

        let rebuild_files: Rc<dyn Fn() -> usize> = {
            let state_rc = state.clone();
            let store_ref = track_store.clone();
            let search_ref = search_entry.clone();
            Rc::new(move || {
                // Respect any active search filter so that background rebuilds
                // (rescan, folder add, ID3 save) don't discard the current query.
                let query = search_ref.text().to_lowercase();
                let tracks: Vec<crate::media_library::LibTrack> = state_rc
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|lib| {
                        if query.is_empty() {
                            lib.all_tracks().ok()
                        } else {
                            lib.search_tracks(&query).ok()
                        }
                    })
                    .unwrap_or_default();
                let count = tracks.len();
                let boxed: Vec<glib::BoxedAnyObject> =
                    tracks.into_iter().map(glib::BoxedAnyObject::new).collect();
                store_ref.splice(0, store_ref.n_items(), &boxed);
                count
            })
        };

        rebuild_files();
        sort_model.set_sorter(col_view.sorter().as_ref());

        let track_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Automatic)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .min_content_height(300)
            .child(&col_view)
            .build();
        files_vbox.append(&track_scroll);

        // Live search with 300ms debounce to avoid rebuilding on every keystroke.
        {
            let state_rc = state.clone();
            let store_ref = track_store.clone();
            let pending = Rc::new(RefCell::new(None::<glib::SourceId>));
            search_entry.connect_changed(move |entry| {
                let query = entry.text().to_lowercase();
                // Cancel any pending search.
                if let Some(src) = pending.borrow_mut().take() {
                    src.remove();
                }
                // Schedule a new search after 300ms of inactivity.
                let state_inner = state_rc.clone();
                let store_inner = store_ref.clone();
                let pending_inner = pending.clone();
                let src =
                    glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
                        let tracks: Vec<crate::media_library::LibTrack> = state_inner
                            .borrow()
                            .media_lib
                            .as_ref()
                            .and_then(|lib| {
                                if query.is_empty() {
                                    lib.all_tracks().ok()
                                } else {
                                    lib.search_tracks(&query).ok()
                                }
                            })
                            .unwrap_or_default();
                        let boxed: Vec<glib::BoxedAnyObject> =
                            tracks.into_iter().map(glib::BoxedAnyObject::new).collect();
                        store_inner.splice(0, store_inner.n_items(), &boxed);
                        pending_inner.borrow_mut().take();
                        glib::ControlFlow::Break
                    });
                *pending.borrow_mut() = Some(src);
            });
        }

        let files_status = Label::builder()
            .label("")
            .halign(Align::Start)
            .margin_start(6)
            .margin_end(6)
            .margin_bottom(2)
            .build();
        files_status.add_css_class("status-label");
        files_vbox.append(&files_status);

        // Button row.
        let btn_row = GtkBox::new(Orientation::Horizontal, 4);
        btn_row.set_margin_start(4);
        btn_row.set_margin_end(4);
        btn_row.set_margin_bottom(4);

        let btn_add_to_pl = Button::with_label("▶ Add to Playlist");
        btn_add_to_pl.add_css_class("pl-btn");
        let btn_customize = Button::with_label("⚙ Columns");
        btn_customize.add_css_class("pl-btn");
        let btn_add_folder = Button::with_label("+ Add Folder");
        btn_add_folder.add_css_class("pl-btn");
        let btn_rescan = Button::with_label("⟳ Rescan");
        btn_rescan.add_css_class("pl-btn");
        let btn_cancel = Button::with_label("✕ Cancel Scan");
        btn_cancel.add_css_class("pl-btn");
        btn_cancel.add_css_class("destructive");
        btn_cancel.set_visible(false);
        let btn_rm_from_ml = Button::with_label("✕ Remove");
        btn_rm_from_ml.add_css_class("pl-btn");
        btn_rm_from_ml.add_css_class("destructive");

        // Button row: add-to-playlist on the left, management buttons on the right.
        let spring = GtkBox::new(Orientation::Horizontal, 0);
        spring.set_hexpand(true);
        btn_row.append(&btn_add_to_pl);
        btn_row.append(&spring);
        btn_row.append(&btn_rm_from_ml);
        btn_row.append(&btn_customize);
        btn_row.append(&btn_add_folder);
        btn_row.append(&btn_rescan);
        btn_row.append(&btn_cancel);
        files_vbox.append(&btn_row);

        // Add selected tracks to playlist.
        let add_selected: Rc<dyn Fn()> = {
            let state_rc = state.clone();
            let sel_ref = multi_sel.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let set_track_add = set_track.clone();
            Rc::new(move || {
                let was_empty = state_rc.borrow().playlist.is_empty();
                let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                let should_replace = state_rc.borrow().config.behavior.playlist_add_behavior
                    == crate::config::PlaylistAddBehavior::Replace;
                if should_replace {
                    let _ = state_rc.borrow_mut().player.stop();
                    state_rc.borrow_mut().playlist.clear();
                }
                let mut added = 0usize;
                for i in 0..sel_ref.n_items() {
                    if sel_ref.is_selected(i) {
                        if let Some(obj) = sel_ref
                            .item(i)
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            let t = obj.borrow::<crate::media_library::LibTrack>();
                            let track = crate::model::Track::from(&*t);
                            state_rc.borrow_mut().playlist.add(track);
                            added += 1;
                        }
                    }
                }
                if added > 0 {
                    // Autoplay when replacing (always start fresh) or when the
                    // playlist was empty and a track just arrived.
                    if autoplay && (was_empty || should_replace) {
                        if let Some(display) = state_rc.borrow_mut().play_current() {
                            set_track_add(&display);
                        }
                    }
                    rebuild_pl();
                }
            })
        };

        btn_add_to_pl.connect_clicked({
            let add = add_selected.clone();
            move |_| {
                add();
            }
        });

        // Double-click / Enter to add a single track.
        {
            let state_rc = state.clone();
            let sel_ref = multi_sel.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let set_track_ml = set_track.clone();
            col_view.connect_activate(move |_, pos| {
                if let Some(obj) = sel_ref
                    .item(pos)
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                {
                    let was_empty = state_rc.borrow().playlist.is_empty();
                    let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                    let should_replace = state_rc.borrow().config.behavior.playlist_add_behavior
                        == crate::config::PlaylistAddBehavior::Replace;
                    let t = obj.borrow::<crate::media_library::LibTrack>();
                    let track = crate::model::Track::from(&*t);
                    drop(t);
                    if should_replace {
                        // Stop before clearing so the current track doesn't
                        // keep playing after the playlist is replaced.
                        let _ = state_rc.borrow_mut().player.stop();
                        state_rc.borrow_mut().playlist.clear();
                    }
                    state_rc.borrow_mut().playlist.add(track);
                    // Autoplay when: the playlist was empty (append mode), or
                    // when replacing (the new track should always start playing).
                    if autoplay && (was_empty || should_replace) {
                        if let Some(display) = state_rc.borrow_mut().play_current() {
                            set_track_ml(&display);
                        }
                    }
                    rebuild_pl();
                }
            });
        }

        // Customize columns dialog.
        {
            let state_rc = state.clone();
            let all_cols_rc = all_cols.clone();
            let cv_holder = col_view_holder.clone();
            let ac_holder = all_cols_holder.clone();
            let state_reorder = state.clone();
            let win_wk = win.downgrade();
            btn_customize.connect_clicked(move |_| {
                let cols_for_callback = all_cols_rc.clone();
                let cv_h = cv_holder.clone();
                let ac_h = ac_holder.clone();
                let st_r = state_reorder.clone();
                open_customize_columns_dialog(
                    win_wk.upgrade().as_ref(),
                    state_rc.clone(),
                    "Customize Columns",
                    ColumnCustomizerMode::MediaLibrary,
                    Some(Rc::new(move |id: String, visible: bool| {
                        if let Some((_, col)) =
                            cols_for_callback.iter().find(|(col_id, _)| col_id == &id)
                        {
                            col.set_visible(visible);
                        }
                    }) as Rc<dyn Fn(String, bool)>),
                    Some(Rc::new(move || {
                        let saved_order =
                            st_r.borrow().config.media_library.ml_file_col_order.clone();
                        if saved_order.is_empty() {
                            return;
                        }
                        let cv_opt = cv_h.borrow();
                        let all_cols = ac_h.borrow();
                        if let Some(col_view) = &*cv_opt {
                            for (_, col) in all_cols.iter() {
                                col_view.remove_column(col);
                            }
                            let mut pos = 1u32;
                            for col_id in &saved_order {
                                if let Some((_, col)) =
                                    all_cols.iter().find(|(id, _)| id == col_id)
                                {
                                    col_view.insert_column(pos, col);
                                    pos += 1;
                                }
                            }
                            for (id, col) in all_cols.iter() {
                                if !saved_order.contains(id) {
                                    col_view.insert_column(pos, col);
                                    pos += 1;
                                }
                            }
                        }
                    }) as Rc<dyn Fn()>),
                );
            });
        }

        // Add Folder handler.
        {
            let state_rc = state.clone();
            let win_wk = win.downgrade();
            let rebuild_ref = rebuild_files.clone();
            let status_ref = files_status.clone();
            let cancel_ref = btn_cancel.clone();
            let rescan_ref = btn_rescan.clone();
            btn_add_folder.connect_clicked(move |_| {
                let chooser = gtk4::FileDialog::new();
                chooser.set_title("Add Folder to Media Library");
                let state_inner = state_rc.clone();
                let rebuild_inner = rebuild_ref.clone();
                let status_inner = status_ref.clone();
                let cancel_btn = cancel_ref.clone();
                let rescan_btn = rescan_ref.clone();
                if let Some(w) = win_wk.upgrade() {
                    chooser.select_folder(Some(&w), None::<&gio::Cancellable>, move |result| {
                        let Ok(file) = result else {
                            return;
                        };
                        let Some(folder) = file.path() else {
                            return;
                        };
                        let path_str = folder.to_string_lossy().to_string();

                        let db_path = {
                            let s = state_inner.borrow();
                            s.media_lib
                                .as_ref()
                                .map(|_| crate::media_library::MediaLibrary::db_path_pub())
                        };
                        let Some(db_path) = db_path else {
                            status_inner.set_text("Media library not available");
                            return;
                        };
                        // Refuse to start a second concurrent scan.
                        if state_inner.borrow().ml_scan.is_some() {
                            status_inner.set_text("Scan already in progress — please wait");
                            return;
                        }

                        // Set up scan state: shows cancel button and disables rescan.
                        let cancel_flag = start_ml_scan(&state_inner, ScanType::AddFolder, 0);
                        status_inner.set_text("Reading tags…");
                        cancel_btn.set_visible(true);
                        rescan_btn.set_sensitive(false);

                        // Three channels: fast done, metadata progress, final result.
                        let (fast_tx, fast_rx) =
                            std::sync::mpsc::channel::<Result<usize, String>>();
                        let (progress_tx, progress_rx) =
                            std::sync::mpsc::channel::<(usize, usize)>();
                        let (result_tx, result_rx) =
                            std::sync::mpsc::channel::<Result<usize, String>>();

                        let cancel_thread = cancel_flag.clone();
                        std::thread::spawn(move || {
                            let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
                                Ok(l) => l,
                                Err(e) => {
                                    let _ = fast_tx.send(Err(format!("DB error: {e}")));
                                    return;
                                }
                            };
                            let folder_id = match lib.add_folder(&path_str) {
                                Err(e) => {
                                    let _ = fast_tx
                                        .send(Err(format!("Could not add '{}': {e}", path_str)));
                                    return;
                                }
                                Ok(r) => r.id(),
                            };
                            // Phase 1: insert file paths into DB (fast).
                            if let Err(e) = lib.rescan_folder_fast(folder_id, &path_str) {
                                let _ = fast_tx
                                    .send(Err(format!("Scan error for '{}': {e}", path_str)));
                                return;
                            }
                            let _ = fast_tx.send(Ok(folder_id as usize));
                            // Phase 2: read metadata. Reset tracks with no metadata
                            // first so any missed by a prior scan are re-processed.
                            let _ = lib.reset_unscanned_metadata();
                            let count = lib
                                .scan_folder(folder_id, &cancel_thread, |c, t| {
                                    let _ = progress_tx.send((c, t));
                                })
                                .map(|(scanned, _, _)| scanned)
                                .unwrap_or(0);
                            let _ = result_tx.send(Ok(count));
                        });

                        let fast_rx = std::cell::RefCell::new(fast_rx);
                        let progress_rx = std::cell::RefCell::new(progress_rx);
                        let result_rx = std::cell::RefCell::new(result_rx);
                        let fast_handled = std::cell::Cell::new(false);
                        glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
                            // Handle fast scan completion — rebuild immediately so
                            // tracks appear in the library while metadata loads.
                            if !fast_handled.get() {
                                if let Ok(fast_result) = fast_rx.borrow().try_recv() {
                                    fast_handled.set(true);
                                    {
                                        let mut s = state_inner.borrow_mut();
                                        s.media_lib =
                                            crate::media_library::MediaLibrary::open().ok();
                                    }
                                    if let Err(e) = fast_result {
                                        status_inner.set_text(&e);
                                        complete_ml_scan(&state_inner);
                                        cancel_btn.set_visible(false);
                                        rescan_btn.set_sensitive(true);
                                        return glib::ControlFlow::Break;
                                    }
                                    rebuild_inner();
                                    status_inner.set_text("Reading tags…");
                                }
                            }

                            // Drain metadata progress updates.
                            while let Ok((current, total)) = progress_rx.borrow().try_recv() {
                                update_ml_scan_progress(&state_inner, current, total);
                                status_inner
                                    .set_text(&format!("Reading tags {}/{}…", current, total));
                            }

                            // Check for final completion.
                            if let Ok(result) = result_rx.borrow().try_recv() {
                                {
                                    let mut s = state_inner.borrow_mut();
                                    s.media_lib = crate::media_library::MediaLibrary::open().ok();
                                }
                                complete_ml_scan(&state_inner);
                                match result {
                                    Err(e) => status_inner.set_text(&e),
                                    Ok(_) => {
                                        let count = rebuild_inner();
                                        status_inner
                                            .set_text(&format!("{count} tracks in library"));
                                    }
                                }
                                cancel_btn.set_visible(false);
                                rescan_btn.set_sensitive(true);
                                return glib::ControlFlow::Break;
                            }

                            glib::ControlFlow::Continue
                        });
                    });
                }
            });
        }

        // Rescan handler — runs in a background thread to avoid blocking the UI.
        {
            let state_rc = state.clone();
            let rebuild_ref = rebuild_files.clone();
            let status_ref = files_status.clone();
            let cancel_ref = btn_cancel.clone();
            let rescan_ref = btn_rescan.clone();
            btn_rescan.connect_clicked(move |_| {
                let db_path = {
                    let s = state_rc.borrow();
                    match s.media_lib.as_ref() {
                        None => {
                            status_ref.set_text("Media library not available");
                            return;
                        }
                        Some(_) => crate::media_library::MediaLibrary::db_path_pub(),
                    }
                };

                let cancel_flag = start_ml_scan(&state_rc, ScanType::Rescan, 0);
                status_ref.set_text("Reading tags…");
                cancel_ref.set_visible(true);
                rescan_ref.set_sensitive(false);

                let (progress_tx, progress_rx) = std::sync::mpsc::channel();
                let (result_tx, result_rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let lib = match crate::media_library::MediaLibrary::open_at(&db_path) {
                        Ok(l) => l,
                        Err(e) => {
                            let _ = result_tx.send(Err(format!("DB error: {e}")));
                            return;
                        }
                    };
                    let _ = lib.reset_unscanned_metadata();
                    let result = lib
                        .scan_all_folders(&cancel_flag, |current, total| {
                            let _ = progress_tx.send((current, total));
                        })
                        .map_err(|e| e.to_string());
                    let _ = result_tx.send(result);
                });
                let progress_rx = std::cell::RefCell::new(progress_rx);
                let result_rx = std::cell::RefCell::new(result_rx);
                let state_rc2 = state_rc.clone();
                let rebuild_ref2 = rebuild_ref.clone();
                let status_ref2 = status_ref.clone();
                let cancel_ref2 = cancel_ref.clone();
                let rescan_ref2 = rescan_ref.clone();
                glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                    // Check for progress updates
                    while let Ok((current, total)) = progress_rx.borrow().try_recv() {
                        update_ml_scan_progress(&state_rc2, current, total);
                        status_ref2.set_text(&format!("Reading tags {}/{}…", current, total));
                    }

                    // Check for completion
                    if let Ok(result) = result_rx.borrow().try_recv() {
                        {
                            let mut s = state_rc2.borrow_mut();
                            s.media_lib = crate::media_library::MediaLibrary::open().ok();
                        }
                        complete_ml_scan(&state_rc2);
                        match result {
                            Err(e) => status_ref2.set_text(&format!("Rescan error: {}", e)),
                            Ok(_) => {
                                let count = rebuild_ref2();
                                status_ref2.set_text(&format!("{count} tracks in library"));
                            }
                        }
                        cancel_ref2.set_visible(false);
                        rescan_ref2.set_sensitive(true);
                        return glib::ControlFlow::Break;
                    }

                    glib::ControlFlow::Continue
                });
            });
        }

        // Cancel scan handler
        {
            let state_rc = state.clone();
            let cancel_ref = btn_cancel.clone();
            let rescan_ref = btn_rescan.clone();
            let status_ref = files_status.clone();
            btn_cancel.connect_clicked(move |_| {
                cancel_ml_scan(&state_rc);
                status_ref.set_text("Cancelling…");
                cancel_ref.set_visible(false);
                rescan_ref.set_sensitive(true);
            });
        }

        // Polling timer to sync scan state with UI.
        {
            let state_rc = state.clone();
            let cancel_ref = btn_cancel.clone();
            let rescan_ref = btn_rescan.clone();
            let add_folder_ref = btn_add_folder.clone();
            let status_ref = files_status.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                let scan_state = state_rc.borrow().ml_scan.clone();
                if let Some(scan) = scan_state {
                    cancel_ref.set_visible(true);
                    rescan_ref.set_sensitive(false);
                    // Disable Add Folder so a second concurrent scan cannot be started.
                    add_folder_ref.set_sensitive(false);
                    if scan.total > 0 {
                        status_ref
                            .set_text(&format!("Reading tags {}/{}…", scan.current, scan.total));
                    } else {
                        status_ref.set_text("Reading tags…");
                    }
                } else {
                    cancel_ref.set_visible(false);
                    rescan_ref.set_sensitive(true);
                    add_folder_ref.set_sensitive(true);
                }
                glib::ControlFlow::Continue
            });
        }

        // Remove selected tracks from library.
        {
            let sel_ref = multi_sel.clone();
            let store_ref = track_store.clone();
            let status_ref = files_status.clone();
            btn_rm_from_ml.connect_clicked(move |_| {
                // Collect IDs of every selected item in one pass.
                let mut ids_vec: Vec<i64> = Vec::new();
                for i in 0..sel_ref.n_items() {
                    if sel_ref.is_selected(i) {
                        if let Some(obj) = sel_ref
                            .item(i)
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            ids_vec.push(obj.borrow::<crate::media_library::LibTrack>().id);
                        }
                    }
                }
                if ids_vec.is_empty() {
                    return;
                }
                let ids_set: std::collections::HashSet<i64> =
                    ids_vec.iter().copied().collect();
                let n_items = store_ref.n_items();

                // Build the kept list and splice in one shot — a single
                // items-changed signal instead of one per removed row.
                // This is the same pattern used by rebuild_files/search and
                // avoids blocking the main thread on large selections.
                let kept: Vec<glib::Object> = (0..n_items)
                    .filter_map(|i| store_ref.item(i))
                    .filter(|obj| {
                        obj.downcast_ref::<glib::BoxedAnyObject>()
                            .map(|b| !ids_set.contains(
                                &b.borrow::<crate::media_library::LibTrack>().id,
                            ))
                            .unwrap_or(true)
                    })
                    .collect();
                let removed = n_items as usize - kept.len();
                store_ref.splice(0, n_items, &kept);

                status_ref.set_text(&format!(
                    "Removed {removed} track{}. {} tracks in library",
                    if removed == 1 { "" } else { "s" },
                    kept.len(),
                ));

                // Soft-delete in background, then purge — same pattern as
                // folder removal.  Opens its own DB connection because
                // rusqlite::Connection is not Send.
                let db_path = crate::media_library::MediaLibrary::db_path_pub();
                std::thread::spawn(move || {
                    if let Ok(lib) = crate::media_library::MediaLibrary::open_at(&db_path) {
                        let _ = lib.soft_delete_tracks(&ids_vec);
                        let _ = lib.purge_deleted_tracks();
                    }
                });
            });
        }

        stack.add_named(&files_vbox, Some("files"));
        let rf = rebuild_files.clone();
        state.borrow_mut().rebuild_ml_callback = Some(Rc::new(move || {
            rf();
        }));
    }

    // ── Page: Playlists ──────────────────────────────────────────────────
    //
    // Two sub-pages within the "playlists" stack page:
    //   "pl-manage" – full-width list of saved playlists + New/Rename/Delete
    //   "pl-edit"   – track editor for the selected playlist
    //
    // pl_sub_stack is stored in an Rc so the sidebar wiring can switch pages.
    let pl_sub_stack: Rc<Stack> = Rc::new({
        let s = Stack::new();
        s.set_hexpand(true);
        s.set_vexpand(true);
        s.set_transition_type(StackTransitionType::None);
        s
    });

    // Shared: currently-editing playlist id and LibTrack list
    let editing_tracks: Rc<RefCell<Vec<crate::media_library::LibTrack>>> =
        Rc::new(RefCell::new(Vec::new()));
    let saved_track_ids: Rc<RefCell<Vec<i64>>> = Rc::new(RefCell::new(Vec::new()));
    // The DB row id of the playlist currently open in the editor (-1 = none)
    let editing_pl_id: Rc<Cell<i64>> = Rc::new(Cell::new(-1));

    // Widget handles for pl-manage playlist list (shared with sidebar)
    let pl_manage_list: Rc<ListBox> = Rc::new({
        let lb = ListBox::new();
        lb.add_css_class("playlist");
        lb.set_selection_mode(gtk4::SelectionMode::Single);
        lb.set_vexpand(true);
        lb
    });

    // Canonical play-order index of the row most recently right-clicked
    // in the editor; the ple.edit-id3 / ple.remove actions read this when
    // they need a single row to operate on.  Used instead of LibTrack.id
    // so duplicate entries (same track listed several times in the
    // playlist file) can be disambiguated by position.
    let ctx_canonical_idx: Rc<Cell<i64>> = Rc::new(Cell::new(-1));

    // Canonical play-order indices selected for an in-progress drag from
    // the editor.  Populated by the per-cell DragSource at prepare time
    // and consumed by the editor DropTarget when handling a reorder.
    // Cleared on every new drag prepare so a previous drag's selection
    // can't leak into a subsequent unrelated drop.
    let drag_selection: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    // Path → first canonical slot.  Used by the editor DropTarget when a
    // cross-window drop ships only paths (no canonical indices) and we
    // need to know whether every dropped path is already in the playlist.
    // For duplicates only the first slot is recorded; the drag_selection
    // path is preferred when the drag originated in the editor itself.
    let position_map: Rc<RefCell<std::collections::HashMap<String, usize>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));

    // Wrapper put into the editor's ListStore.  Carrying `canonical_idx`
    // alongside the track lets every cell — even duplicates of the same
    // file in the playlist — bind to its own play-order slot, so the
    // position column reads the correct row instead of all duplicates
    // collapsing onto the last occurrence's index.  Cloned cheaply on
    // splice because `LibTrack` is `Clone` already.
    #[derive(Clone)]
    struct EditorEntry {
        track: crate::media_library::LibTrack,
        canonical_idx: usize,
    }

    // True when the editor's current display sort allows intra-list drag
    // reorder (only the canonical play-order ascending state preserves the
    // bijection between display index and play-order index).  Flipped by
    // a sorter-change handler installed once the ColumnView exists.
    let reorder_allowed: Rc<Cell<bool>> = Rc::new(Cell::new(true));

    // Track editor: ListStore → SortListModel → MultiSelection → ColumnView.
    // Sort lives in the SortListModel so the user's column-header clicks
    // produce a display-only sort.  `editing_tracks` (the canonical play
    // order) is never reordered by sort — Save always writes that order.
    let edit_store: gio::ListStore = gio::ListStore::new::<glib::BoxedAnyObject>();
    // Per-view search over this playlist's rows: store → filter → sort →
    // selection. Rows keep their canonical_idx, so delete/context actions
    // stay correct under a filter; drag-reorder is refused while one is
    // active (display order no longer maps onto play order).
    let pl_edit_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let edit_filter = gtk4::CustomFilter::new({
        let q = pl_edit_query.clone();
        move |obj| {
            let Some(boxed) = obj.downcast_ref::<glib::BoxedAnyObject>() else {
                return true;
            };
            lib_track_matches_query(&boxed.borrow::<EditorEntry>().track, &q.borrow())
        }
    });
    let edit_filter_model =
        gtk4::FilterListModel::new(Some(edit_store.clone()), Some(edit_filter.clone()));
    // Search filters just this playlist's rows (drag-reorder pauses while a
    // query is active — see the drop handler). Created here so
    // load_pl_by_id can clear it when a different playlist opens; packed
    // into the pl-edit page below.
    let (pl_search_row, pl_search_entry) =
        make_view_search_row("Search this playlist — artist, title, album…");
    {
        let q = pl_edit_query.clone();
        let filter = edit_filter.clone();
        pl_search_entry.connect_changed(move |e| {
            *q.borrow_mut() = e.text().to_lowercase();
            filter.changed(gtk4::FilterChange::Different);
        });
    }
    let edit_sort_model = gtk4::SortListModel::new(
        Some(edit_filter_model),
        None::<gtk4::Sorter>,
    );
    let edit_multi_sel: gtk4::MultiSelection =
        gtk4::MultiSelection::new(Some(edit_sort_model.clone()));
    let track_list: Rc<gtk4::ColumnView> = Rc::new({
        let cv = gtk4::ColumnView::new(Some(edit_multi_sel.clone()));
        cv.add_css_class("playlist");
        cv.set_vexpand(true);
        cv.set_show_row_separators(false);
        cv.set_show_column_separators(false);
        cv
    });

    // ── Editor columns: walk ALL_COLUMNS so files view + editor stay in
    //    lock-step on which columns exist and which order they default to.
    // Position column reference is captured here so the sorter-change
    // listener below can detect when the user has selected position-ASC
    // (the only sort that allows intra-list drag-reorder).
    let pos_col_holder: Rc<RefCell<Option<ColumnViewColumn>>> = Rc::new(RefCell::new(None));
    // Editor named columns (skipping the leading status + position pinned
    // columns) — captured so we can apply the files-view saved order so
    // the user only has to arrange columns in one place.
    let mut editor_named_cols: Vec<(String, ColumnViewColumn)> = Vec::new();
    // Holder for the rebuild closure — populated right after the closure
    // is defined.  Cell factories install per-cell drop targets that need
    // to refresh the editor after a successful reorder, but those factory
    // setups live above the rebuild definition in source order.
    type RebuildClosure = Rc<dyn Fn()>;
    let rebuild_track_list_holder: Rc<RefCell<Option<RebuildClosure>>> =
        Rc::new(RefCell::new(None));

    // Holder for the editor's "ple" action group.  Cell factories pop
    // PopoverMenus parented to track_list; the popover's action lookup
    // walks the GTK widget chain back to track_list where the group is
    // also attached, but some GTK4 versions break that walk with the
    // NESTED PopoverMenu flag.  Installing the group directly on each
    // popup makes dispatch reliable regardless of GTK version.
    let ple_action_group_holder: Rc<RefCell<Option<gio::SimpleActionGroup>>> =
        Rc::new(RefCell::new(None));
    // Holder for the editor's ScrolledWindow — populated right after it
    // is built so the cell right-click handler can use it as the popover
    // parent (cell-label parents render invisible on this GTK4 build).
    let track_scroll_holder: Rc<RefCell<Option<gtk4::ScrolledWindow>>> =
        Rc::new(RefCell::new(None));
    {
        let visible_ids: Vec<String> =
            state.borrow().config.media_library.visible_columns.clone();
        let saved_widths: std::collections::HashMap<String, i32> =
            state.borrow().config.media_library.ml_file_col_widths.clone();

        // Leading status-glyph column (⚠/🔒) — playlist-editor-only, mirrors
        // the unscanned-indicator column on the files side.
        {
            let factory = gtk4::SignalListItemFactory::new();
            factory.connect_setup(|_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() { return }
                let lbl = Label::builder()
                    .halign(Align::Center)
                    .valign(Align::Center)
                    .build();
                li.set_child(Some(&lbl));
            });
            factory.connect_bind(|_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li.item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else { return };
                let entry = boxed.borrow::<EditorEntry>();
                let t = &entry.track;
                let path = std::path::Path::new(&t.path);
                // Missing == the file is gone, mirroring the macOS/FFI
                // `file_missing` flag. `id == 0` only means "not catalogued";
                // an uncatalogued file that exists is a normal playable track.
                let missing  = !path.exists();
                let readonly = !missing && crate::media_library::is_read_only(path);
                let glyph = if missing { "⚠" } else if readonly { "🔒" } else { "" };
                if let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) {
                    lbl.set_label(glyph);
                }
            });
            let col = ColumnViewColumn::new(Some(""), Some(factory));
            col.set_fixed_width(24);
            track_list.append_column(&col);
        }

        // Position column (editor-only) — shows the 1-based playlist slot
        // resolved against the canonical play order in `editing_tracks`.
        // Pinned: fixed width, no resize/reorder.  Sorter is installed
        // below so clicking the header toggles position ASC/DESC.
        {
            let pos_factory = gtk4::SignalListItemFactory::new();
            pos_factory.connect_setup(|_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() { return }
                let lbl = Label::builder()
                    .halign(Align::End)
                    .xalign(1.0)
                    .margin_start(6).margin_end(6)
                    .css_classes(["pl-duration"])
                    .build();
                li.set_child(Some(&lbl));
            });
            pos_factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li.item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else { return };
                let entry = boxed.borrow::<EditorEntry>();
                let text = (entry.canonical_idx + 1).to_string();
                if let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok()) {
                    lbl.set_label(&text);
                }
            });
            let pos_col = ColumnViewColumn::new(Some("#"), Some(pos_factory));
            pos_col.set_fixed_width(48);
            pos_col.set_resizable(false);
            // Canonical-order sorter: compare each entry's slot directly.
            let sorter = CustomSorter::new(move |a, b| {
                let pa = a.downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .unwrap_or(usize::MAX);
                let pb = b.downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .unwrap_or(usize::MAX);
                pa.cmp(&pb).into()
            });
            pos_col.set_sorter(Some(&sorter));
            track_list.append_column(&pos_col);
            *pos_col_holder.borrow_mut() = Some(pos_col);
        }

        for c in ALL_COLUMNS.iter() {
            let id_str = c.id.to_string();
            let factory = gtk4::SignalListItemFactory::new();

            let setup_sel        = edit_multi_sel.clone();
            let setup_state      = state.clone();
            let setup_tl         = track_list.clone();
            let setup_ctx_id     = ctx_canonical_idx.clone();
            let setup_et         = editing_tracks.clone();
            let setup_ep_id      = editing_pl_id.clone();
            let setup_drag_sel   = drag_selection.clone();
            let setup_ra         = reorder_allowed.clone();
            // rebuild_track_list isn't yet defined at this point of the
            // outer scope, so capture the Rc via a deferred holder filled
            // immediately after the rebuild closure is created.
            let setup_rebuild    = rebuild_track_list_holder.clone();
            let setup_rebuild_pl = rebuild_playlist.clone();
            let setup_set_track  = set_track.clone();
            let setup_win        = win.clone();
            let setup_scroll     = track_scroll_holder.clone();
            let setup_actgroup   = ple_action_group_holder.clone();
            let setup_id         = id_str.clone();
            let is_artwork_col   = id_str == "artwork_path";
            factory.connect_setup(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                if li.child().is_some() { return }
                // Artwork column gets a "View" Button instead of a Label —
                // matches the files view affordance.  Drag-source / drop-
                // target / right-click gesture attach to the Button just
                // like they would to a Label (both are Widget).
                let child: gtk4::Widget = if setup_id == "artwork_path" {
                    let btn = Button::with_label("View");
                    btn.add_css_class("link");
                    btn.set_margin_start(4);
                    btn.set_margin_end(4);
                    btn.set_halign(Align::Start);
                    btn.set_visible(false);
                    btn.upcast::<gtk4::Widget>()
                } else {
                    let lbl = Label::builder()
                        .margin_start(6).margin_end(6)
                        .margin_top(3).margin_bottom(3)
                        .hexpand(true).vexpand(true)
                        .halign(Align::Fill).valign(Align::Fill)
                        .xalign(0.0)
                        .ellipsize(gtk4::pango::EllipsizeMode::End)
                        .build();
                    lbl.upcast::<gtk4::Widget>()
                };
                let lbl = child.clone();
                let _ = is_artwork_col;

                // Per-cell DropTarget — handles intra-editor reorder.  When
                // the source drag originated in the editor (drag_selection
                // populated) and the current sort allows reorder, splice
                // those canonical rows to this cell's canonical slot.
                // Drops from other windows (drag_selection empty) fall
                // through to the outer track_scroll DropTarget which
                // appends the external paths.
                {
                    let dt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
                    let dt_li      = li.clone();
                    let dt_et      = setup_et.clone();
                    let dt_state   = setup_state.clone();
                    let dt_ep_id   = setup_ep_id.clone();
                    let dt_ra      = setup_ra.clone();
                    let dt_dragsel = setup_drag_sel.clone();
                    let dt_rebuild = setup_rebuild.clone();
                    dt.connect_drop(move |_, value, _, _| {
                        if !dt_ra.get() { return false }
                        // Reject the drop unless the drag originated in
                        // the editor itself — otherwise let the outer
                        // track_scroll DropTarget handle external add.
                        let src_indices: Vec<usize> = dt_dragsel.borrow().clone();
                        if src_indices.is_empty() { return false }
                        // Validate we still received the expected number
                        // of paths (sanity check; not used for indices).
                        if value.get::<gdk::FileList>().is_err() { return false }

                        // Resolve drop slot directly from this cell's
                        // EditorEntry so duplicate paths in the playlist
                        // collapse to the correct row, not the first one.
                        let Some(dst_canon) = dt_li.item()
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                        else { return false };

                        // Splice in canonical order: remove src indices
                        // highest-first, then re-insert in original order
                        // at the adjusted destination.
                        let mut sorted = src_indices.clone();
                        sorted.sort_unstable_by(|a, b| b.cmp(a));
                        let mut adjusted_dst = dst_canon;
                        let mut removed: Vec<crate::media_library::LibTrack> = Vec::new();
                        {
                            let mut et = dt_et.borrow_mut();
                            for src in sorted.iter() {
                                if *src < et.len() {
                                    let t = et.remove(*src);
                                    if *src < adjusted_dst { adjusted_dst -= 1; }
                                    removed.push(t);
                                }
                            }
                            removed.reverse();
                            let cap = et.len();
                            let insert_at = adjusted_dst.min(cap);
                            for (i, t) in removed.into_iter().enumerate() {
                                et.insert(insert_at + i, t);
                            }
                        }

                        // Persist canonical order through the library so
                        // the on-disk M3U8 reflects the reorder immediately.
                        // Rewrites the existing playlist file in place;
                        // `add_playlist_file` upserts the row so registering
                        // the same path again is a no-op.
                        let pid = dt_ep_id.get();
                        if pid >= 0 {
                            let s = dt_state.borrow();
                            if let Some(lib) = s.media_lib.as_ref() {
                                let paths: Vec<String> = dt_et.borrow()
                                    .iter().map(|t| t.path.clone()).collect();
                                if let Ok(pl) = lib.playlist_by_id(pid) {
                                    if let Err(e) = lib.save_playlist_tracks_to_path(
                                        std::path::Path::new(&pl.path),
                                        &paths,
                                    ) {
                                        eprintln!("editor reorder persist {pid}: {e}");
                                    }
                                }
                            }
                        }

                        // Drag completed — clear selection so a stray
                        // subsequent drop (e.g. external) doesn't reorder.
                        dt_dragsel.borrow_mut().clear();

                        // Defer rebuild to next idle tick so we don't
                        // splice the backing ListStore while GTK is still
                        // unwinding the drop event chain — splicing mid-
                        // drop segfaults on some GTK4 versions.
                        if let Some(rb) = dt_rebuild.borrow().as_ref().cloned() {
                            glib::idle_add_local_once(move || rb());
                        }
                        true
                    });
                    lbl.add_controller(dt);
                }

                // Per-cell DragSource — ships every currently-selected editor
                // row as a FileList so the user can drag tracks out of the
                // playlist editor into the active playlist (pl_scroll accepts
                // FileList).  Single-row drag works too: if the row under
                // the pointer is not in the selection it still ships its
                // own path.
                {
                    let ds = gtk4::DragSource::new();
                    ds.set_actions(gtk4::gdk::DragAction::COPY);
                    let ds_sel       = setup_sel.clone();
                    let ds_li        = li.clone();
                    let ds_dragsel   = setup_drag_sel.clone();
                    ds.connect_prepare(move |_, _, _| {
                        // Clear any stale canonical indices from a prior
                        // drag, then record this drag's selection by
                        // canonical_idx so duplicates of the same path
                        // resolve to the correct rows on reorder.
                        ds_dragsel.borrow_mut().clear();
                        let mut paths: Vec<std::path::PathBuf> = Vec::new();
                        let mut indices: Vec<usize> = Vec::new();
                        let mut self_entry: Option<(std::path::PathBuf, usize)> = None;
                        if let Some(obj) = ds_li.item()
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            let entry = obj.borrow::<EditorEntry>();
                            self_entry = Some((
                                std::path::PathBuf::from(&entry.track.path),
                                entry.canonical_idx,
                            ));
                        }
                        for i in 0..ds_sel.n_items() {
                            if ds_sel.is_selected(i) {
                                if let Some(obj) = ds_sel.item(i)
                                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                {
                                    let entry = obj.borrow::<EditorEntry>();
                                    paths.push(std::path::PathBuf::from(&entry.track.path));
                                    indices.push(entry.canonical_idx);
                                }
                            }
                        }
                        if paths.is_empty() {
                            if let Some((p, i)) = self_entry {
                                paths.push(p);
                                indices.push(i);
                            }
                        }
                        if paths.is_empty() { return None }
                        *ds_dragsel.borrow_mut() = indices;
                        let files: Vec<gio::File> = paths.iter()
                            .map(|p| gio::File::for_path(p))
                            .collect();
                        let fl = gdk::FileList::from_array(&files);
                        Some(gdk::ContentProvider::for_value(&fl.to_value()))
                    });
                    lbl.add_controller(ds);
                }

                // Per-cell right-click gesture.  Builds a plain GtkPopover
                // with a vertical box of Buttons rather than a PopoverMenu —
                // each button's connect_clicked fires its action logic
                // directly so dispatch doesn't depend on the GIO action
                // muxer (which proved unreliable for editor menu items in
                // this GTK4 version).
                let gesture = gtk4::GestureClick::new();
                gesture.set_button(gtk4::gdk::BUTTON_SECONDARY);
                let g_sel        = setup_sel.clone();
                let g_state      = setup_state.clone();
                let g_tl         = setup_tl.clone();
                let g_ctx_id     = setup_ctx_id.clone();
                let g_li         = li.clone();
                let g_lbl        = lbl.clone();
                let g_et         = setup_et.clone();
                let g_ep_id      = setup_ep_id.clone();
                let g_rebuild    = setup_rebuild.clone();
                let g_rebuild_pl = setup_rebuild_pl.clone();
                let g_set_track  = setup_set_track.clone();
                let g_win        = setup_win.clone();
                let g_scroll     = setup_scroll.clone();
                let g_act        = setup_actgroup.clone();
                gesture.connect_pressed(move |g, _n, x, y| {
                    let Some(item) = g_li.item() else {
                        return;
                    };
                    let item_clone = item.clone();
                    let mut clicked_idx: Option<u32> = None;
                    for i in 0..g_sel.n_items() {
                        if g_sel.item(i).as_ref() == Some(&item_clone) {
                            clicked_idx = Some(i);
                            break;
                        }
                    }
                    if let Some(idx) = clicked_idx {
                        if !g_sel.is_selected(idx) {
                            g_sel.unselect_all();
                            g_sel.select_item(idx, true);
                        }
                    }
                    // Stash this row's canonical play-order slot so the
                    // single-row actions (edit-id3) operate on the exact
                    // row that was clicked even when the playlist lists
                    // duplicates of the same path.
                    let (cidx, is_lib_track) = item.downcast_ref::<glib::BoxedAnyObject>()
                        .map(|o| {
                            let e = o.borrow::<EditorEntry>();
                            (e.canonical_idx as i64, e.track.id > 0)
                        })
                        .unwrap_or((-1, false));
                    g_ctx_id.set(cidx);

                    let sel_count: usize = (0..g_sel.n_items())
                        .filter(|i| g_sel.is_selected(*i)).count();

                    // Helper closure: gather canonical indices the action
                    // should operate on — selection first, falling back
                    // to the single clicked row when nothing is selected.
                    let sel_for_pick = g_sel.clone();
                    let ctx_for_pick = g_ctx_id.clone();
                    let pick_idxs = Rc::new(move || -> Vec<usize> {
                        let mut idxs: Vec<usize> = (0..sel_for_pick.n_items())
                            .filter(|i| sel_for_pick.is_selected(*i))
                            .filter_map(|i| sel_for_pick.item(i))
                            .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                            .collect();
                        if idxs.is_empty() {
                            let c = ctx_for_pick.get();
                            if c >= 0 { idxs.push(c as usize); }
                        }
                        idxs
                    });

                    // ── Build plain Popover + Box of Buttons ----------
                    // PopoverMenu dispatch path proved unreliable for the
                    // editor — actions never fired even when group was
                    // attached at multiple ancestors.  Plain Popover with
                    // direct connect_clicked closures guarantees action
                    // delivery.  Visual style is matched to the files
                    // view via the `menu` CSS class on both the popover
                    // and the content box, plus a "modelbutton"-style
                    // CSS class on each button (mimics PopoverMenu's
                    // internal GtkModelButtons).
                    let popover = gtk4::Popover::new();
                    popover.set_has_arrow(false);
                    popover.set_position(gtk4::PositionType::Bottom);
                    popover.add_css_class("menu");

                    let vbox = GtkBox::new(Orientation::Vertical, 0);
                    vbox.add_css_class("menu");
                    vbox.set_size_request(240, -1);

                    // Build buttons that look like PopoverMenu items.
                    // Marked "modelbutton" so the GTK4 default theme
                    // applies the same hover/padding/border treatment
                    // PopoverMenu uses internally for its GtkModelButton
                    // entries.
                    let add_btn = |label: &str, vbox: &GtkBox| -> Button {
                        let lbl = Label::builder()
                            .label(label)
                            .halign(Align::Start)
                            .hexpand(true)
                            .xalign(0.0)
                            .build();
                        let b = Button::new();
                        b.set_child(Some(&lbl));
                        b.set_has_frame(false);
                        b.set_hexpand(true);
                        b.add_css_class("flat");
                        b.add_css_class("modelbutton");
                        vbox.append(&b);
                        b
                    };

                    // Add to Active Playlist
                    {
                        let btn = add_btn("Add to Playlist", &vbox);
                        let pop_c = popover.clone();
                        let et_c = g_et.clone();
                        let st_c = g_state.clone();
                        let pi_c = pick_idxs.clone();
                        let rb_pl_c = g_rebuild_pl.clone();
                        let st_track_c = g_set_track.clone();
                        btn.connect_clicked(move |_| {
                            pop_c.popdown();
                            let tracks: Vec<crate::media_library::LibTrack> = {
                                let et_b = et_c.borrow();
                                pi_c().into_iter().filter_map(|i| et_b.get(i).cloned()).collect()
                            };
                            if tracks.is_empty() { return }
                            let was_empty = st_c.borrow().playlist.is_empty();
                            let autoplay = st_c.borrow().config.behavior.autoplay_on_add;
                            {
                                let mut s = st_c.borrow_mut();
                                for lt in &tracks { s.playlist.add(crate::model::Track::from(lt)); }
                            }
                            if autoplay && was_empty {
                                if let Some(d) = st_c.borrow_mut().play_current() { st_track_c(&d); }
                            }
                            rb_pl_c();
                        });
                    }

                    // Replace Active Playlist
                    {
                        let btn = add_btn("Replace Current Playlist", &vbox);
                        let pop_c = popover.clone();
                        let et_c = g_et.clone();
                        let st_c = g_state.clone();
                        let pi_c = pick_idxs.clone();
                        let rb_pl_c = g_rebuild_pl.clone();
                        let st_track_c = g_set_track.clone();
                        btn.connect_clicked(move |_| {
                            pop_c.popdown();
                            let tracks: Vec<crate::media_library::LibTrack> = {
                                let et_b = et_c.borrow();
                                pi_c().into_iter().filter_map(|i| et_b.get(i).cloned()).collect()
                            };
                            if tracks.is_empty() { return }
                            let autoplay = st_c.borrow().config.behavior.autoplay_on_add;
                            {
                                let mut s = st_c.borrow_mut();
                                let _ = s.player.stop();
                                s.playlist = crate::model::Playlist::new();
                                for lt in &tracks { s.playlist.add(crate::model::Track::from(lt)); }
                            }
                            if autoplay {
                                if let Some(d) = st_c.borrow_mut().play_current() { st_track_c(&d); }
                            }
                            rb_pl_c();
                        });
                    }

                    // Edit / View ID3 (single + library only)
                    if is_lib_track && sel_count <= 1 {
                        let btn = add_btn("Edit / View ID3 Tags", &vbox);
                        let pop_c = popover.clone();
                        let et_c = g_et.clone();
                        let st_c = g_state.clone();
                        let rb_pl_c = g_rebuild_pl.clone();
                        let ctx_c = g_ctx_id.clone();
                        btn.connect_clicked(move |_| {
                            pop_c.popdown();
                            let c = ctx_c.get();
                            if c < 0 { return }
                            let path = et_c.borrow().get(c as usize).map(|t| t.path.clone());
                            let Some(path) = path else { return };
                            open_id3_editor_window(
                                None::<&gtk4::Window>,
                                path.into(),
                                st_c.clone(),
                                rb_pl_c.clone(),
                                None,
                            );
                        });
                    }

                    // Remove from Playlist
                    {
                        let btn = add_btn("Remove from Playlist", &vbox);
                        let pop_c = popover.clone();
                        let et_c = g_et.clone();
                        let st_c = g_state.clone();
                        let pi_c = pick_idxs.clone();
                        let ep_c = g_ep_id.clone();
                        let rb_c = g_rebuild.clone();
                        btn.connect_clicked(move |_| {
                            pop_c.popdown();
                            let mut idxs = pi_c();
                            if idxs.is_empty() { return }
                            idxs.sort_unstable_by(|a, b| b.cmp(a));
                            {
                                let mut e = et_c.borrow_mut();
                                for i in idxs.iter() {
                                    if *i < e.len() { e.remove(*i); }
                                }
                            }
                            let pid = ep_c.get();
                            if pid >= 0 {
                                let s = st_c.borrow();
                                if let Some(lib) = s.media_lib.as_ref() {
                                    let paths: Vec<String> = et_c.borrow()
                                        .iter().map(|t| t.path.clone()).collect();
                                    if let Ok(pl) = lib.playlist_by_id(pid) {
                                        let _ = lib.save_playlist_tracks_to_path(
                                            std::path::Path::new(&pl.path),
                                            &paths,
                                        );
                                    }
                                }
                            }
                            if let Some(rb) = rb_c.borrow().as_ref() { rb(); }
                        });
                    }

                    // ── Add to Playlist section ----------------------
                    let sep = gtk4::Separator::new(Orientation::Horizontal);
                    sep.set_margin_top(4);
                    sep.set_margin_bottom(4);
                    vbox.append(&sep);
                    let header = Label::builder()
                        .label("Save to Playlist")
                        .halign(Align::Start)
                        .margin_start(8)
                        .build();
                    header.add_css_class("dim-label");
                    vbox.append(&header);

                    // New Playlist…
                    {
                        let btn = add_btn("  New Playlist…", &vbox);
                        let pop_c = popover.clone();
                        let et_c = g_et.clone();
                        let st_c = g_state.clone();
                        let pi_c = pick_idxs.clone();
                        let win_c = g_win.clone();
                        btn.connect_clicked(move |_| {
                            pop_c.popdown();
                            let paths: Vec<String> = {
                                let et_b = et_c.borrow();
                                pi_c().into_iter()
                                    .filter_map(|i| et_b.get(i))
                                    .map(|t| t.path.clone())
                                    .collect()
                            };
                            if paths.is_empty() { return }
                            let default_stem = glib::DateTime::now_local().ok()
                                .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "Playlist".to_string());
                            let state_cb = st_c.clone();
                            let paths_cb = paths.clone();
                            run_playlist_save_dialog(
                                st_c.clone(),
                                win_c.clone(),
                                &default_stem,
                                move |path, win_cb| {
                                    if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                                        if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths_cb) {
                                            eprintln!("save_playlist_tracks_to_path: {e}");
                                            show_playlist_save_error(&win_cb, &path, &e);
                                        }
                                    }
                                },
                            );
                        });
                    }

                    // Existing saved playlists
                    let playlists: Vec<(i64, String)> = g_state.borrow()
                        .media_lib.as_ref()
                        .and_then(|lib| lib.all_playlists().ok())
                        .map(|v| v.into_iter().map(|p| (p.id, p.name)).collect())
                        .unwrap_or_default();
                    for (pid, name) in playlists {
                        let btn = add_btn(&format!("  {name}"), &vbox);
                        let pop_c = popover.clone();
                        let et_c = g_et.clone();
                        let st_c = g_state.clone();
                        let pi_c = pick_idxs.clone();
                        btn.connect_clicked(move |_| {
                            pop_c.popdown();
                            let paths: Vec<String> = {
                                let et_b = et_c.borrow();
                                pi_c().into_iter()
                                    .filter_map(|i| et_b.get(i))
                                    .map(|t| t.path.clone())
                                    .collect()
                            };
                            if paths.is_empty() { return }
                            let mut ok = false;
                            if let Some(lib) = st_c.borrow().media_lib.as_ref() {
                                match lib.append_paths_to_playlist(pid, &paths) {
                                    Ok(_)  => ok = true,
                                    Err(e) => eprintln!("append_paths_to_playlist {pid}: {e}"),
                                }
                            }
                            if ok { notify_playlist_changed(pid); }
                        });
                    }

                    // Wrap in scrolling container so many playlists fit.
                    let menu_scroll = gtk4::ScrolledWindow::builder()
                        .hscrollbar_policy(PolicyType::Never)
                        .vscrollbar_policy(PolicyType::Automatic)
                        .min_content_width(260)
                        .max_content_height(420)
                        .propagate_natural_height(true)
                        .child(&vbox)
                        .build();
                    popover.set_child(Some(&menu_scroll));

                    // Parent on track_list (ColumnView) — stable.  Plain
                    // popover with size_request renders for both single
                    // and multi-select cases.
                    let parent_widget: gtk4::Widget = (*g_tl).clone().upcast();
                    let (px, py) = g_lbl
                        .translate_coordinates(&parent_widget, x, y)
                        .unwrap_or((x, y));
                    let rect = gtk4::gdk::Rectangle::new(px as i32, py as i32, 1, 1);
                    popover.set_parent(&parent_widget);
                    popover.set_pointing_to(Some(&rect));

                    popover.connect_closed(|p| p.unparent());
                    popover.popup();
                    let _ = g;
                    let _ = g_scroll;
                    let _ = g_act;
                });
                lbl.add_controller(gesture);

                li.set_child(Some(&lbl));
            });

            let bind_id = id_str.clone();
            factory.connect_bind(move |_, obj| {
                let li = obj.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(boxed) = li.item()
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                else { return };
                let entry = boxed.borrow::<EditorEntry>();
                let t = &entry.track;
                // Stash this cell's canonical play-order index on whatever
                // child widget the cell currently holds so the editor-area
                // drop target can resolve a drop coordinate to a canonical
                // insert position via track_list.pick(x, y) → walk_up →
                // parse "pos:<N>".  Works for both Label and Button cells.
                if let Some(c) = li.child() {
                    c.set_widget_name(&format!("pos:{}", entry.canonical_idx));
                }
                // Artwork column gets the Button affordance, mirroring the
                // files view.  Click opens the cached cover-art image.
                if bind_id == "artwork_path" {
                    let Some(btn) = li.child().and_then(|c| c.downcast::<Button>().ok())
                    else { return };
                    if let Some(art_path) = t.artwork_path.clone() {
                        btn.set_visible(true);
                        btn.set_sensitive(true);
                        btn.set_label("View");
                        // Replace any prior click handler so the captured
                        // path always matches the row currently bound to
                        // this recycled cell.
                        let handler = btn.connect_clicked(move |_| {
                            open_image_viewer(&art_path);
                        });
                        // Disconnect previous handler if present to avoid
                        // accumulating across binds on the same widget.
                        unsafe {
                            if let Some(old) = btn.steal_data::<glib::SignalHandlerId>("art-handler") {
                                btn.disconnect(old);
                            }
                            btn.set_data("art-handler", handler);
                        }
                    } else {
                        btn.set_visible(false);
                    }
                    return;
                }
                let Some(lbl) = li.child().and_then(|c| c.downcast::<Label>().ok())
                else { return };
                let text = match bind_id.as_str() {
                    "num" => t.track_num.map(|n| n.to_string()).unwrap_or_default(),
                    "title" => t.title.as_deref().unwrap_or(&t.filename).to_string(),
                    "artist" => t.artist.as_deref().unwrap_or("").to_string(),
                    "album" => t.album.as_deref().unwrap_or("").to_string(),
                    "album_artist" => t.album_artist.as_deref().unwrap_or("").to_string(),
                    "duration" => t.length_secs
                        .map(|s| { let ss = s as u64; format!("{}:{:02}", ss/60, ss%60) })
                        .unwrap_or_else(|| "-:--".to_string()),
                    "filename" => t.filename.clone(),
                    "year" => t.year.map(|y| y.to_string()).unwrap_or_default(),
                    "genre" => t.genre.as_deref().unwrap_or("").to_string(),
                    "bitrate" => t.bitrate.map(|b| format!("{b}k")).unwrap_or_default(),
                    "channels" => match t.channels.unwrap_or(0) {
                        1 => "mono".to_string(),
                        2 => "stereo".to_string(),
                        n => format!("{}ch", n),
                    },
                    "path" => t.path.clone(),
                    "play_count" => t.play_count.to_string(),
                    "last_played" => format_last_played(t.last_played.as_deref().unwrap_or("")),
                    "last_scanned" => t.last_scanned.as_deref().unwrap_or("").to_string(),
                    "disc_num" => {
                        let d = t.disc_num.unwrap_or(0);
                        if d == 0 { String::new() }
                        else if let Some(total) = t.disc_total {
                            if total > 0 { format!("{}/{}", d, total) } else { d.to_string() }
                        } else { d.to_string() }
                    }
                    "disc_total" => t.disc_total.map(|d| d.to_string()).unwrap_or_default(),
                    "composer" => t.composer.as_deref().unwrap_or("").to_string(),
                    "original_artist" => t.original_artist.as_deref().unwrap_or("").to_string(),
                    "copyright" => t.copyright.as_deref().unwrap_or("").to_string(),
                    "url" => t.url.as_deref().unwrap_or("").to_string(),
                    "encoded_by" => t.encoded_by.as_deref().unwrap_or("").to_string(),
                    "bpm" => t.bpm.as_deref().unwrap_or("").to_string(),
                    "lyric" => {
                        let ly = t.lyric.as_deref().unwrap_or("");
                        if ly.is_empty() { String::new() }
                        else if ly.len() > 30 { format!("{}…", &ly[..30]) }
                        else { ly.to_string() }
                    }
                    "comment" => t.comment.as_deref().unwrap_or("").to_string(),
                    "artwork_path" => if t.artwork_path.is_some() { "Yes".to_string() } else { String::new() },
                    _ => String::new(),
                };
                lbl.set_text(&gtk_safe(&text));
                // Unavailable file → broken color, mirroring the macOS
                // editor's red rows for missing files. Existence — not library
                // membership — decides this, so an uncatalogued but present
                // file shows normally.
                let missing = !std::path::Path::new(&t.path).exists();
                if missing {
                    lbl.add_css_class("broken");
                } else {
                    lbl.remove_css_class("broken");
                }
            });

            let col = ColumnViewColumn::new(Some(c.header), Some(factory));
            col.set_resizable(true);
            if c.expand { col.set_expand(true); }
            col.set_visible(visible_ids.contains(&id_str));
            if let Some(&w) = saved_widths.get(&id_str) {
                if w > 0 { col.set_fixed_width(w); }
            }

            // Display-only sorter — sort is applied via SortListModel so
            // `editing_tracks` (canonical play order) is never mutated.
            let sort_id = id_str.clone();
            let sorter = CustomSorter::new(move |a, b| {
                let a_val = a
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<EditorEntry>().track, &sort_id))
                    .unwrap_or_default();
                let b_val = b
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .map(|o| ml_sort_key(&o.borrow::<EditorEntry>().track, &sort_id))
                    .unwrap_or_default();
                a_val.cmp(&b_val).into()
            });
            col.set_sorter(Some(&sorter));
            track_list.append_column(&col);
            editor_named_cols.push((id_str, col));
        }

        // Apply the files-view saved column order so the editor matches
        // it — the user only arranges columns once.  Columns not present
        // in saved_order keep their default position at the tail.
        let saved_order = state.borrow().config.media_library.ml_file_col_order.clone();
        if !saved_order.is_empty() {
            for (_, col) in editor_named_cols.iter() {
                track_list.remove_column(col);
            }
            // Position 0 = status glyph, 1 = position; named columns start at 2.
            let mut pos = 2u32;
            for col_id in &saved_order {
                if let Some((_, col)) = editor_named_cols.iter()
                    .find(|(id, _)| id == col_id)
                {
                    track_list.insert_column(pos, col);
                    pos += 1;
                }
            }
            for (id, col) in editor_named_cols.iter() {
                if !saved_order.contains(id) {
                    track_list.insert_column(pos, col);
                    pos += 1;
                }
            }
        }
    }
    // Allow drag-reorder of editor column headers — same affordance as
    // the files view.  Pinned columns (status + position) remain in
    // place because they aren't reorderable individually; GTK keeps them
    // in their declared positions.
    track_list.set_reorderable(true);

    // Shared closure that re-applies the files-view column state
    // (visibility, widths, order) to the editor's ColumnView.  Called
    // every time a saved playlist is loaded so the editor mirrors the
    // user's latest customization without needing a full ML reopen.
    let editor_cols_rc: Rc<Vec<(String, ColumnViewColumn)>> =
        Rc::new(editor_named_cols);
    let apply_editor_columns: Rc<dyn Fn()> = {
        let cols = editor_cols_rc.clone();
        let state_rc = state.clone();
        let tl = track_list.clone();
        // 2 pinned leading columns: status glyph + position.
        Rc::new(move || apply_ml_columns_to(&tl, cols.as_slice(), &state_rc, 2))
    };

    // Connect the sort model to the ColumnView's column-driven sorter so
    // header clicks produce a display sort.  Then listen for sorter changes
    // and update `reorder_allowed` — true when the active sort is "position
    // ASC" or no sort, false for any other column / order.
    {
        let sorter = track_list.sorter();
        edit_sort_model.set_sorter(sorter.as_ref());
        if let Some(s) = sorter {
            let pos_holder = pos_col_holder.clone();
            let ra = reorder_allowed.clone();
            let update = move |s: &gtk4::Sorter| {
                let pos_col = pos_holder.borrow().clone();
                let allowed = if let Some(cv_sorter) =
                    s.downcast_ref::<gtk4::ColumnViewSorter>()
                {
                    let primary = cv_sorter.primary_sort_column();
                    let order   = cv_sorter.primary_sort_order();
                    match (primary, pos_col) {
                        (None, _) => true, // default sort = canonical
                        (Some(pc), Some(target)) =>
                            pc == target && order == gtk4::SortType::Ascending,
                        _ => false,
                    }
                } else {
                    true
                };
                ra.set(allowed);
            };
            update(&s);
            s.connect_changed(move |s, _| update(s));
        }
    }

    // Rebuild track editor: splice the entire `editing_tracks` Vec into the
    // backing ListStore as `EditorEntry` items so each row carries its
    // canonical slot.  ColumnView recycles visible rows so this stays
    // cheap for big playlists.  Also rebuilds `position_map` for first-
    // occurrence path lookups by the cross-window drop target.
    let rebuild_track_list: Rc<dyn Fn()> = {
        let store    = edit_store.clone();
        let et       = editing_tracks.clone();
        let pos_map  = position_map.clone();
        Rc::new(move || {
            let mut map = pos_map.borrow_mut();
            map.clear();
            let items: Vec<glib::BoxedAnyObject> = et
                .borrow()
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    map.entry(t.path.clone()).or_insert(i);
                    glib::BoxedAnyObject::new(EditorEntry {
                        track: t.clone(),
                        canonical_idx: i,
                    })
                })
                .collect();
            drop(map);
            store.splice(0, store.n_items(), &items);
        })
    };
    // Populate the holder so the column factories' per-cell drop targets
    // can refresh the editor after a successful reorder.
    *rebuild_track_list_holder.borrow_mut() = Some(rebuild_track_list.clone());

    // Error banner shown when a playlist's file can't be read (e.g. the
    // library was scanned in another sandbox and the stored path doesn't
    // resolve here).  Hidden while the playlist loads normally.  Hoisted
    // here so load_pl_by_id below can capture it; packed into the
    // pl-edit page further down.
    let edit_error_label: Label = Label::builder()
        .label("")
        .halign(Align::Start)
        .xalign(0.0)
        .wrap(true)
        .margin_start(8).margin_end(8)
        .margin_top(4).margin_bottom(4)
        .visible(false)
        .build();
    edit_error_label.add_css_class("broken");

    // ── Helper: load a playlist by DB id into editing state ───────────────
    let load_pl_by_id: Rc<dyn Fn(i64)> = {
        let state_rc   = state.clone();
        let et         = editing_tracks.clone();
        let saved      = saved_track_ids.clone();
        let rebuild    = rebuild_track_list.clone();
        let ep_id      = editing_pl_id.clone();
        let apply_cols = apply_editor_columns.clone();
        let err_lbl    = edit_error_label.clone();
        let search     = pl_search_entry.clone();
        Rc::new(move |id: i64| {
            ep_id.set(id);
            // A previous playlist's search query must not filter this one.
            search.set_text("");
            // Re-apply files-view column state so customizations made
            // while the editor was elsewhere take effect immediately.
            apply_cols();
            let loaded = state_rc
                .borrow()
                .media_lib
                .as_ref()
                .map(|lib| {
                    lib.playlist_by_id(id)
                        .and_then(|pl| lib.load_playlist_tracks(&pl))
                });
            let tracks = match loaded {
                Some(Ok(tracks)) => {
                    err_lbl.set_visible(false);
                    tracks
                }
                Some(Err(e)) => {
                    // Playlist entries live only in the .m3u8 file, so an
                    // unreadable file means there is nothing to show — say
                    // why instead of presenting a silently empty playlist.
                    err_lbl.set_text(&gtk_safe(&format!(
                        "This playlist has not been scanned yet and its \
                         file is not accessible from here ({e:#})."
                    )));
                    err_lbl.set_visible(true);
                    Vec::new()
                }
                None => {
                    err_lbl.set_visible(false);
                    Vec::new()
                }
            };
            let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();
            *et.borrow_mut() = tracks;
            *saved.borrow_mut() = ids;
            rebuild();
        })
    };

    // Register the editor-refresh hook so any cross-window add-to-saved
    // action that targets the currently-open playlist reloads the editor.
    {
        let load = load_pl_by_id.clone();
        let ep_id = editing_pl_id.clone();
        let hook: Rc<dyn Fn(i64)> = Rc::new(move |target_pid: i64| {
            if ep_id.get() == target_pid {
                load(target_pid);
            }
        });
        EDITOR_REFRESH_HOOK.with(|h| *h.borrow_mut() = Some(hook));
    }
    // Refresh-current hook: reloads whatever playlist is open in the
    // editor.  Fired after a track is recorded as played so the editor
    // mirrors the files view's updated metadata + unread state.
    {
        let load = load_pl_by_id.clone();
        let ep_id = editing_pl_id.clone();
        let hook: Rc<dyn Fn()> = Rc::new(move || {
            let id = ep_id.get();
            if id >= 0 { load(id); }
        });
        EDITOR_CURRENT_REFRESH_HOOK.with(|h| *h.borrow_mut() = Some(hook));
    }
    // Nav-refresh hook: re-sync the playlist sidebar sub-rows and the
    // manage list with the playlists table after a playlist is created
    // from another window (e.g. active-playlist "Add to new playlist").
    {
        let state_rc     = state.clone();
        let sidebar_ref  = sidebar.clone();
        let sub_rows_ref = pl_sub_rows.clone();
        let expanded_ref = playlists_expanded.clone();
        let manage_ref   = pl_manage_list.clone();
        let hook: Rc<dyn Fn()> = Rc::new(move || {
            let playlists = state_rc
                .borrow()
                .media_lib
                .as_ref()
                .and_then(|lib| lib.all_playlists().ok())
                .unwrap_or_default();

            // Remember the selected sidebar playlist (if any) so the
            // rebuild doesn't visually drop the user's place.
            let selected = sidebar_ref
                .selected_row()
                .map(|r| r.widget_name().to_string());

            // Clear both lists, then rebuild from the playlists table.
            // Sidebar sub-rows are tracked in `pl_sub_rows`, so drain that;
            // the manage list isn't tracked, so empty it by index.
            for row in sub_rows_ref.borrow_mut().drain(..) {
                sidebar_ref.remove(&row);
            }
            while let Some(row) = manage_ref.row_at_index(0) {
                manage_ref.remove(&row);
            }

            // Insert the rebuilt rows right after the Playlists header — not at
            // the sidebar end, which is below the Devices section.
            let mut insert_at = {
                let mut idx = 0i32;
                let mut after = 1i32;
                while let Some(r) = sidebar_ref.row_at_index(idx) {
                    if r.widget_name() == "playlists" {
                        after = idx + 1;
                        break;
                    }
                    idx += 1;
                }
                after
            };

            for pl in &playlists {
                let s_lbl = Label::builder()
                    .label(&pl.name)
                    .halign(Align::Start)
                    .xalign(0.0)
                    .margin_start(24).margin_end(8)
                    .margin_top(4).margin_bottom(4)
                    .build();
                let s_row = ListBoxRow::new();
                s_row.set_widget_name(&format!("pl:{}", pl.id));
                s_row.set_child(Some(&s_lbl));
                s_row.set_visible(expanded_ref.get());
                attach_pl_row_drag(&s_row, pl.id);
                sidebar_ref.insert(&s_row, insert_at);
                insert_at += 1;
                if selected.as_deref() == Some(s_row.widget_name().as_str()) {
                    sidebar_ref.select_row(Some(&s_row));
                }
                sub_rows_ref.borrow_mut().push(s_row);

                let m_lbl = Label::builder()
                    .label(&pl.name)
                    .halign(Align::Start)
                    .margin_start(8).margin_end(8)
                    .margin_top(3).margin_bottom(3)
                    .build();
                let m_row = ListBoxRow::new();
                m_row.set_widget_name(&pl.id.to_string());
                m_row.set_child(Some(&m_lbl));
                attach_pl_row_drag(&m_row, pl.id);
                manage_ref.append(&m_row);
            }
        });
        PLAYLIST_NAV_REFRESH_HOOK.with(|h| *h.borrow_mut() = Some(hook));
    }

    // ── Helper: add a sub-row to both the sidebar and pl_manage_list ──────
    // Returns the sidebar row so the caller can select it.
    let _add_pl_sidebar_row = {
        let sidebar_ref  = sidebar.clone();
        let sub_rows_ref = pl_sub_rows.clone();
        let expanded_ref = playlists_expanded.clone();
        Rc::new(move |id: i64, name: &str| -> ListBoxRow {
            // Sidebar sub-row
            let s_lbl = Label::builder()
                .label(name)
                .halign(Align::Start)
                .xalign(0.0)
                .margin_start(24).margin_end(8)
                .margin_top(4).margin_bottom(4)
                .build();
            let s_row = ListBoxRow::new();
            s_row.set_widget_name(&format!("pl:{}", id));
            s_row.set_child(Some(&s_lbl));
            s_row.set_visible(expanded_ref.get());
            attach_pl_row_drag(&s_row, id);
            sidebar_ref.append(&s_row);
            sub_rows_ref.borrow_mut().push(s_row.clone());
            s_row
        })
    };

    // ── Build "pl-manage" page ────────────────────────────────────────────
    {
        let manage_vbox = GtkBox::new(Orientation::Vertical, 0);

        // Populate the manage list from DB
        let playlists_initial = state
            .borrow()
            .media_lib
            .as_ref()
            .and_then(|lib| lib.all_playlists().ok())
            .unwrap_or_default();
        for pl in &playlists_initial {
            let lbl = Label::builder()
                .label(&pl.name)
                .halign(Align::Start)
                .margin_start(8).margin_end(8)
                .margin_top(3).margin_bottom(3)
                .build();
            let row = ListBoxRow::new();
            row.set_widget_name(&pl.id.to_string());
            row.set_child(Some(&lbl));
            attach_pl_row_drag(&row, pl.id);
            pl_manage_list.append(&row);
        }

        let manage_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .child(&*pl_manage_list)
            .build();
        manage_vbox.append(&manage_scroll);

        // Clicking a manage-list row → select its sidebar sub-row
        {
            let sidebar_ref   = sidebar.clone();
            let pl_sub_ref    = pl_sub_stack.clone();
            pl_manage_list.connect_row_selected(move |_, opt_row| {
                let row = match opt_row { Some(r) => r, None => return };
                let id_str = row.widget_name().to_string();
                // Find matching sidebar "pl:ID" row and select it
                let target = format!("pl:{}", id_str);
                let mut i = 0i32;
                loop {
                    match sidebar_ref.row_at_index(i) {
                        Some(sr) if sr.widget_name() == target => {
                            sidebar_ref.select_row(Some(&sr));
                            break;
                        }
                        Some(_) => { i += 1; }
                        None => break,
                    }
                }
                // Also switch sub-stack directly (sidebar handler may not fire
                // if the row is already selected)
                pl_sub_ref.set_visible_child_name("pl-edit");
            });
        }

        // Manage list bottom buttons: New / Rename / Delete
        let manage_btn_row = GtkBox::new(Orientation::Horizontal, 4);
        manage_btn_row.set_margin_start(4);
        manage_btn_row.set_margin_end(4);
        manage_btn_row.set_margin_top(4);
        manage_btn_row.set_margin_bottom(4);

        let btn_new_pl    = Button::with_label("+ New");
        btn_new_pl.add_css_class("pl-btn");
        let btn_rename_pl = Button::with_label("Rename");
        btn_rename_pl.add_css_class("pl-btn");
        btn_rename_pl.set_sensitive(false);
        let btn_delete_pl = Button::with_label("Delete");
        btn_delete_pl.add_css_class("pl-btn");
        btn_delete_pl.set_sensitive(false);

        manage_btn_row.append(&btn_new_pl);
        manage_btn_row.append(&btn_rename_pl);
        manage_btn_row.append(&btn_delete_pl);
        manage_vbox.append(&manage_btn_row);

        // Enable/disable rename+delete based on manage list selection
        {
            let btn_ren = btn_rename_pl.clone();
            let btn_del = btn_delete_pl.clone();
            pl_manage_list.connect_row_selected(move |_, opt| {
                let has = opt.is_some();
                btn_ren.set_sensitive(has);
                btn_del.set_sensitive(has);
            });
        }

        // ── New playlist ──────────────────────────────────────────────────
        {
            let state_rc      = state.clone();
            let pl_list_ref   = pl_manage_list.clone();
            let sidebar_ref   = sidebar.clone();
            let sub_rows_ref  = pl_sub_rows.clone();
            let expanded_ref  = playlists_expanded.clone();
            let pl_sub_ref    = pl_sub_stack.clone();
            let load          = load_pl_by_id.clone();
            let win_wk        = win.downgrade();
            btn_new_pl.connect_clicked(move |_| {
                let Some(win) = win_wk.upgrade() else { return };
                let state2  = state_rc.clone();
                let pl_ref2 = pl_list_ref.clone();
                let sid2    = sidebar_ref.clone();
                let sub2    = sub_rows_ref.clone();
                let exp2    = expanded_ref.clone();
                let pls2    = pl_sub_ref.clone();
                let load2   = load.clone();
                // Save dialog replaces the previous name-only popup —
                // user picks BOTH the filename and the target folder so
                // the new playlist no longer lands silently in Sparkamp's
                // managed `~/.config/sparkamp/playlists/` directory (which
                // had the side effect of registering itself as a watched
                // folder via `add_playlist_file`).
                run_playlist_save_dialog(state_rc.clone(), win, "New Playlist", move |path, win_cb| {
                    let name = path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Untitled")
                        .to_string();
                    let save_result = state2.borrow().media_lib.as_ref()
                        .map(|lib| lib.save_playlist_tracks_to_path(&path, &[]));
                    let new_id = match save_result {
                        Some(Ok(id)) => id,
                        Some(Err(e)) => {
                            eprintln!("save_playlist_tracks_to_path: {e}");
                            show_playlist_save_error(&win_cb, &path, &e);
                            return;
                        }
                        None => return,
                    };

                    // Add to manage list
                    let row_lbl = Label::builder().label(&name)
                        .halign(Align::Start)
                        .margin_start(8).margin_end(8)
                        .margin_top(3).margin_bottom(3).build();
                    let manage_row = ListBoxRow::new();
                    manage_row.set_widget_name(&new_id.to_string());
                    manage_row.set_child(Some(&row_lbl));
                    attach_pl_row_drag(&manage_row, new_id);
                    pl_ref2.append(&manage_row);
                    pl_ref2.select_row(Some(&manage_row));

                    // Add sidebar sub-row and select it
                    let s_lbl = Label::builder().label(&name)
                        .halign(Align::Start)
                        .xalign(0.0)
                        .margin_start(24).margin_end(8)
                        .margin_top(4).margin_bottom(4).build();
                    let s_row = ListBoxRow::new();
                    s_row.set_widget_name(&format!("pl:{}", new_id));
                    s_row.set_child(Some(&s_lbl));
                    s_row.set_visible(exp2.get());
                    attach_pl_row_drag(&s_row, new_id);
                    sid2.insert(&s_row, sidebar_pl_end_index(&sid2));
                    sub2.borrow_mut().push(s_row.clone());
                    sid2.select_row(Some(&s_row));

                    load2(new_id);
                    pls2.set_visible_child_name("pl-edit");
                });
            });
        }

        // ── Rename playlist ───────────────────────────────────────────────
        {
            let state_rc    = state.clone();
            let pl_list_ref = pl_manage_list.clone();
            let sidebar_ref = sidebar.clone();
            let win_wk      = win.downgrade();
            btn_rename_pl.connect_clicked(move |_| {
                let sel_row = match pl_list_ref.selected_row() { Some(r) => r, None => return };
                let id = match sel_row.widget_name().to_string().parse::<i64>() {
                    Ok(v) => v, Err(_) => return,
                };
                let current = sel_row.child()
                    .and_then(|c| c.downcast::<Label>().ok())
                    .map(|l| l.text().to_string()).unwrap_or_default();

                let dialog = gtk4::Window::builder()
                    .title("Rename Playlist").modal(true).resizable(false).default_width(300)
                    .build();
                if let Some(w) = win_wk.upgrade() { dialog.set_transient_for(Some(&w)); }
                let vbox = GtkBox::new(Orientation::Vertical, 8);
                vbox.set_margin_top(12); vbox.set_margin_bottom(12);
                vbox.set_margin_start(12); vbox.set_margin_end(12);
                let lbl = Label::builder().label("New name:").halign(Align::Start).build();
                let name_entry = Entry::new();
                name_entry.set_text(&gtk_safe(&current));
                name_entry.set_hexpand(true);
                let dialog_btns = GtkBox::new(Orientation::Horizontal, 6);
                dialog_btns.set_halign(Align::End);
                let cancel_btn = Button::with_label("Cancel");
                let ok_btn     = Button::with_label("Rename");
                ok_btn.add_css_class("suggested-action");
                dialog_btns.append(&cancel_btn); dialog_btns.append(&ok_btn);
                vbox.append(&lbl); vbox.append(&name_entry); vbox.append(&dialog_btns);
                dialog.set_child(Some(&vbox));
                let d = dialog.clone();
                cancel_btn.connect_clicked(move |_| { d.close(); });
                let d        = dialog.clone();
                let e        = name_entry.clone();
                let state2   = state_rc.clone();
                let sel2     = sel_row.clone();
                let sid2     = sidebar_ref.clone();
                ok_btn.connect_clicked(move |_| {
                    let name = e.text().to_string();
                    if name.is_empty() { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.rename_playlist(id, &name);
                    }
                    // Update manage-list label
                    if let Some(c) = sel2.child() {
                        if let Ok(l) = c.downcast::<Label>() { l.set_text(&gtk_safe(&name)); }
                    }
                    // Update sidebar sub-row label
                    let target = format!("pl:{}", id);
                    let mut i = 0i32;
                    loop {
                        match sid2.row_at_index(i) {
                            Some(sr) if sr.widget_name() == target => {
                                if let Some(c) = sr.child() {
                                    if let Ok(l) = c.downcast::<Label>() {
                                        l.set_text(&gtk_safe(&name));
                                    }
                                }
                                break;
                            }
                            Some(_) => { i += 1; }
                            None => break,
                        }
                    }
                    d.close();
                });
                let ok2 = ok_btn.clone();
                name_entry.connect_activate(move |_| { ok2.activate(); });
                dialog.present();
            });
        }

        // ── Delete playlist ───────────────────────────────────────────────
        {
            let state_rc    = state.clone();
            let pl_list_ref = pl_manage_list.clone();
            let sidebar_ref = sidebar.clone();
            let sub_rows_ref = pl_sub_rows.clone();
            let pl_sub_ref  = pl_sub_stack.clone();
            let et          = editing_tracks.clone();
            let saved       = saved_track_ids.clone();
            let rebuild     = rebuild_track_list.clone();
            let win_wk      = win.downgrade();
            btn_delete_pl.connect_clicked(move |_| {
                let sel_row = match pl_list_ref.selected_row() { Some(r) => r, None => return };
                let id = match sel_row.widget_name().to_string().parse::<i64>() {
                    Ok(v) => v, Err(_) => return,
                };
                let pl_name = sel_row.child()
                    .and_then(|c| c.downcast::<Label>().ok())
                    .map(|l| l.text().to_string()).unwrap_or_default();

                let dialog = gtk4::AlertDialog::builder()
                    .message(format!("Delete \"{}\"?", pl_name))
                    .detail("The playlist file on disk is not deleted.")
                    .buttons(vec!["Cancel".to_string(), "Delete".to_string()])
                    .cancel_button(0).default_button(1).modal(true).build();

                let state2    = state_rc.clone();
                let pl_ref2   = pl_list_ref.clone();
                let sid2      = sidebar_ref.clone();
                let sub2      = sub_rows_ref.clone();
                let pls2      = pl_sub_ref.clone();
                let sel2      = sel_row.clone();
                let et2       = et.clone();
                let saved2    = saved.clone();
                let rebuild2  = rebuild.clone();
                dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |result| {
                    if result != Ok(1) { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.remove_playlist(id);
                    }
                    // Remove from manage list
                    pl_ref2.remove(&sel2);
                    // Remove sidebar sub-row
                    let target = format!("pl:{}", id);
                    let mut sub = sub2.borrow_mut();
                    sub.retain(|r| {
                        if r.widget_name() == target { sid2.remove(r); false } else { true }
                    });
                    // Go back to manage page
                    et2.borrow_mut().clear();
                    saved2.borrow_mut().clear();
                    rebuild2();
                    pls2.set_visible_child_name("pl-manage");
                });
            });
        }

        pl_sub_stack.add_named(&manage_vbox, Some("pl-manage"));
    }

    // Hoisted: title + rename button + path label (sidebar handler updates
    // the title text on selection change).
    let edit_header: Label = Label::builder()
        .label("Playlist Editor")
        .halign(Align::Start)
        .hexpand(true)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .margin_start(8).margin_top(4).margin_bottom(0)
        .build();
    edit_header.add_css_class("ml-section-header");

    let btn_rename_pl_inline: Button = {
        let b = Button::with_label("Rename");
        b.add_css_class("pl-btn");
        b.set_margin_end(8);
        b.set_margin_top(2);
        b
    };

    // File path bar — shows the .m3u path so the user can see if it is an
    // external playlist (not managed by Sparkamp).
    let edit_path_label: Label = Label::builder()
        .label("")
        .halign(Align::Start)
        .margin_start(8).margin_top(0).margin_bottom(4)
        .ellipsize(gtk4::pango::EllipsizeMode::Middle)
        .selectable(true)
        .build();
    edit_path_label.add_css_class("status-label");

    // Save button (hoisted so the sidebar handler can toggle its sensitivity)
    let btn_save_pl_outer: Button = {
        let b = Button::with_label("Save");
        b.add_css_class("pl-btn");
        b
    };

    // ── Build "pl-edit" page ──────────────────────────────────────────────
    {
        let edit_vbox = GtkBox::new(Orientation::Vertical, 0);

        let header_row = GtkBox::new(Orientation::Horizontal, 4);
        header_row.append(&edit_header);
        header_row.append(&btn_rename_pl_inline);
        edit_vbox.append(&header_row);
        edit_vbox.append(&edit_path_label);
        edit_vbox.append(&edit_error_label);

        edit_vbox.append(&pl_search_row);

        let track_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Automatic)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .hexpand(true)
            .child(&*track_list)
            .build();
        edit_vbox.append(&track_scroll);
        // Expose track_scroll so cell right-click popovers can parent
        // themselves to it (parented-to-leaf popovers don't render).
        *track_scroll_holder.borrow_mut() = Some(track_scroll.clone());

        // Delete key on the editor's ColumnView removes the selected
        // rows from the playlist (canonical play order) and rewrites
        // the on-disk M3U8 — same behavior as the Remove from Playlist
        // menu item.
        {
            let key = EventControllerKey::new();
            let sel    = edit_multi_sel.clone();
            let et     = editing_tracks.clone();
            let ep_id  = editing_pl_id.clone();
            let rb     = rebuild_track_list.clone();
            let st     = state.clone();
            key.connect_key_pressed(move |_, keyval, _keycode, _mods| {
                if keyval != gdk::Key::Delete && keyval != gdk::Key::KP_Delete {
                    return glib::Propagation::Proceed;
                }
                let mut idxs: Vec<usize> = (0..sel.n_items())
                    .filter(|i| sel.is_selected(*i))
                    .filter_map(|i| sel.item(i))
                    .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .collect();
                if idxs.is_empty() { return glib::Propagation::Proceed }
                idxs.sort_unstable_by(|a, b| b.cmp(a));
                {
                    let mut e = et.borrow_mut();
                    for i in idxs.iter() {
                        if *i < e.len() { e.remove(*i); }
                    }
                }
                let pid = ep_id.get();
                if pid >= 0 {
                    let s = st.borrow();
                    if let Some(lib) = s.media_lib.as_ref() {
                        let paths: Vec<String> = et.borrow()
                            .iter().map(|t| t.path.clone()).collect();
                        if let Ok(pl) = lib.playlist_by_id(pid) {
                            let _ = lib.save_playlist_tracks_to_path(
                                std::path::Path::new(&pl.path),
                                &paths,
                            );
                        }
                    }
                }
                rb();
                glib::Propagation::Stop
            });
            track_list.add_controller(key);
        }

        // Editor DropTarget — handles two drop kinds:
        //
        //   1. Reorder (every dropped path already in `editing_tracks`):
        //      splice the rows to the canonical insert position resolved
        //      from the drop coordinate.  Gated by `reorder_allowed` so
        //      drops while a non-position sort is active no-op rather than
        //      adding duplicates at the bottom.
        //   2. External add (any dropped path not in `editing_tracks`):
        //      append the *new* paths to the on-disk M3U8 via
        //      `append_paths_to_playlist` and mirror them into the
        //      editor's in-memory state.
        {
            let dt = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
            let state_drop  = state.clone();
            let et_drop     = editing_tracks.clone();
            let ep_drop     = editing_pl_id.clone();
            let rebuild_drop = rebuild_track_list.clone();
            let _posmap_drop = position_map.clone();
            let ra_drop     = reorder_allowed.clone();
            let query_drop  = pl_edit_query.clone();
            let tl_drop     = track_list.clone();
            let dragsel_drop = drag_selection.clone();
            dt.connect_drop(move |_, value, x, y| {
                let file_list = match value.get::<gdk::FileList>() {
                    Ok(fl) => fl,
                    Err(_) => return false,
                };
                let paths: Vec<String> = file_list.files().iter()
                    .filter_map(|f| f.path())
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                if paths.is_empty() { return false }
                let pid = ep_drop.get();
                let lib_opt_has = state_drop.borrow().media_lib.is_some();
                if !lib_opt_has { return false }

                // Prefer drag_selection (canonical indices captured by
                // our DragSource) so duplicates in the playlist resolve
                // correctly.  If the drag came from another window the
                // selection is empty — treat the paths as external add.
                let drag_src_indices: Vec<usize> = dragsel_drop.borrow().clone();
                let is_internal_reorder = !drag_src_indices.is_empty();

                if is_internal_reorder {
                    // Pure reorder.  Refuse silently when the current sort
                    // doesn't make reorder semantically sensible — avoids
                    // appending duplicates at the bottom in that case.  A
                    // live search filter breaks the display↔play-order
                    // mapping the same way, so it refuses too.
                    if !ra_drop.get() || !query_drop.borrow().is_empty() {
                        dragsel_drop.borrow_mut().clear();
                        return true;
                    }

                    // Resolve the drop coordinate to a canonical insert
                    // position.  First try pick(x, y) + walk up — works
                    // when the cursor is over a cell.  Falls back to a
                    // scan of every visible cell when the cursor lands
                    // between rows (no cell directly under it), inserting
                    // before the first cell whose vertical midpoint is
                    // past the drop y.  Last-resort default is append.
                    let dst_canon: usize = (|| {
                        let mut w = tl_drop.pick(x, y, gtk4::PickFlags::DEFAULT)?;
                        loop {
                            let name = w.widget_name().to_string();
                            if let Some(rest) = name.strip_prefix("pos:") {
                                if let Ok(n) = rest.parse::<usize>() {
                                    return Some(n);
                                }
                            }
                            w = w.parent()?;
                        }
                    })()
                    .or_else(|| {
                        let root_widget: &gtk4::Widget = tl_drop.upcast_ref();
                        let mut cells = editor_cell_positions(root_widget);
                        cells.sort_by(|a, b| a.1.partial_cmp(&b.1)
                            .unwrap_or(std::cmp::Ordering::Equal));
                        let drop_y = y as f32;
                        cells.iter()
                            .find(|c| c.1 + c.2 / 2.0 > drop_y)
                            .map(|c| c.0)
                    })
                    .unwrap_or_else(|| et_drop.borrow().len());

                    let mut sorted = drag_src_indices.clone();
                    sorted.sort_unstable_by(|a, b| b.cmp(a));
                    let mut adjusted_dst = dst_canon;
                    let mut removed: Vec<crate::media_library::LibTrack> = Vec::new();
                    {
                        let mut et = et_drop.borrow_mut();
                        for src in sorted.iter() {
                            if *src < et.len() {
                                let t = et.remove(*src);
                                if *src < adjusted_dst { adjusted_dst -= 1; }
                                removed.push(t);
                            }
                        }
                        removed.reverse();
                        let cap = et.len();
                        let insert_at = adjusted_dst.min(cap);
                        for (i, t) in removed.into_iter().enumerate() {
                            et.insert(insert_at + i, t);
                        }
                    }

                    if pid >= 0 {
                        let s = state_drop.borrow();
                        if let Some(lib) = s.media_lib.as_ref() {
                            let paths_now: Vec<String> = et_drop.borrow()
                                .iter().map(|t| t.path.clone()).collect();
                            if let Ok(pl) = lib.playlist_by_id(pid) {
                                if let Err(e) = lib.save_playlist_tracks_to_path(
                                    std::path::Path::new(&pl.path),
                                    &paths_now,
                                ) {
                                    eprintln!("editor reorder persist {pid}: {e}");
                                }
                            }
                        }
                    }
                    dragsel_drop.borrow_mut().clear();
                    let rb = rebuild_drop.clone();
                    glib::idle_add_local_once(move || rb());
                    return true;
                }

                // External add: append every dropped path; the user's
                // playlist may already contain some of them but treating
                // a cross-window drop as add is the least-surprising
                // semantics (duplicates can be removed afterwards).
                let new_paths: Vec<String> = paths.clone();
                if new_paths.is_empty() { return true }
                // Persist to disk first; only mutate in-memory editor state
                // if the save succeeded so failures don't leave the editor
                // diverged from the file on disk.
                if pid >= 0 {
                    let s = state_drop.borrow();
                    let lib = s.media_lib.as_ref().unwrap();
                    if let Err(e) = lib.append_paths_to_playlist(pid, &new_paths) {
                        eprintln!("editor drop append_paths_to_playlist {pid}: {e}");
                        return false;
                    }
                }
                let paths = new_paths;
                // Mirror the new entries into editing_tracks so the visible
                // ColumnView reflects them without needing a full reload.
                let new_libtracks: Vec<crate::media_library::LibTrack> = {
                    let s = state_drop.borrow();
                    let lib = s.media_lib.as_ref().unwrap();
                    paths.iter()
                        .map(|p| {
                            if let Ok(t) = lib.track_by_path(p) { return t }
                            let filename = std::path::Path::new(p)
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            crate::media_library::LibTrack {
                                id: 0,
                                path: p.clone(),
                                filename,
                                artist: None, title: None, album: None,
                                track_num: None, genre: None, year: None,
                                bpm: None, length_secs: None, bitrate: None,
                                channels: None, filetype: None,
                                play_count: 0, last_played: None,
                                comment: None, album_artist: None,
                                disc_num: None, disc_total: None,
                                composer: None, original_artist: None,
                                copyright: None, url: None, encoded_by: None,
                                lyric: None, artwork_path: None,
                                last_scanned: None,
                                sort_keys: Default::default(),
                            }
                        })
                        .collect()
                };
                et_drop.borrow_mut().extend(new_libtracks);
                let rb = rebuild_drop.clone();
                glib::idle_add_local_once(move || rb());
                true
            });
            track_scroll.add_controller(dt);
        }

        // Track editor controls
        let edit_btn_row = GtkBox::new(Orientation::Horizontal, 4);
        edit_btn_row.set_margin_start(4); edit_btn_row.set_margin_end(4);
        edit_btn_row.set_margin_top(4);  edit_btn_row.set_margin_bottom(4);

        let btn_add_files_pl  = Button::with_label("+ Files");    btn_add_files_pl.add_css_class("pl-btn");
        let btn_add_folder_pl = Button::with_label("+ Folder");   btn_add_folder_pl.add_css_class("pl-btn");
        let btn_remove_tracks = Button::with_label("− Remove");   btn_remove_tracks.add_css_class("pl-btn");
        let btn_delete_pl     = Button::with_label("🗑 Delete Playlist"); btn_delete_pl.add_css_class("pl-btn");
        let spring_pl         = GtkBox::new(Orientation::Horizontal, 0); spring_pl.set_hexpand(true);
        let btn_revert_pl     = Button::with_label("↺ Revert");  btn_revert_pl.add_css_class("pl-btn");
        let btn_save_as_pl    = Button::with_label("Save As…");  btn_save_as_pl.add_css_class("pl-btn");
        let btn_save_pl       = btn_save_pl_outer.clone();
        let btn_enqueue_pl    = Button::with_label("Enqueue"); btn_enqueue_pl.add_css_class("pl-btn");
        let btn_send_dev      = Button::with_label("Send to…"); btn_send_dev.add_css_class("pl-btn");
        let btn_play_pl       = Button::with_label("▶ Play");  btn_play_pl.add_css_class("pl-btn");

        edit_btn_row.append(&btn_add_files_pl);
        edit_btn_row.append(&btn_add_folder_pl);
        edit_btn_row.append(&btn_remove_tracks);
        edit_btn_row.append(&btn_delete_pl);
        edit_btn_row.append(&spring_pl);
        edit_btn_row.append(&btn_revert_pl);
        edit_btn_row.append(&btn_save_as_pl);
        edit_btn_row.append(&btn_save_pl);
        edit_btn_row.append(&btn_enqueue_pl);
        edit_btn_row.append(&btn_send_dev);
        edit_btn_row.append(&btn_play_pl);
        edit_vbox.append(&edit_btn_row);

        // "Send to…" → popover listing connected devices; picking one sends the
        // whole playlist (files + .m3u8) to it.
        {
            let devices = current_devices.clone();
            let ep_id = editing_pl_id.clone();
            let state_rc = state.clone();
            let send = send_playlist_run.clone();
            let win_wk = win.downgrade();
            btn_send_dev.connect_clicked(move |btn| {
                let devs = devices.borrow().clone();
                if devs.is_empty() {
                    show_alert_parented(win_wk.upgrade().as_ref(), "No devices connected.");
                    return;
                }
                let id = ep_id.get();
                if id < 0 {
                    return;
                }
                let name = state_rc
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|l| l.playlist_by_id(id).ok())
                    .map(|p| p.name)
                    .unwrap_or_default();
                let pop = gtk4::Popover::new();
                pop.set_parent(btn);
                pop.connect_closed(|p| p.unparent());
                let vbox = GtkBox::new(Orientation::Vertical, 2);
                for d in devs {
                    let label = if d.label.is_empty() {
                        "Untitled device".to_string()
                    } else {
                        d.label.clone()
                    };
                    let b = Button::with_label(&gtk_safe(&label));
                    b.add_css_class("flat");
                    let send2 = send.clone();
                    let name2 = name.clone();
                    let pop2 = pop.clone();
                    let d2 = d.clone();
                    b.connect_clicked(move |_| {
                        pop2.popdown();
                        send2(d2.clone(), id, name2.clone());
                    });
                    vbox.append(&b);
                }
                pop.set_child(Some(&vbox));
                pop.popup();
            });
        }

        // ── Add Files ─────────────────────────────────────────────────────
        {
            let state_rc = state.clone();
            let et       = editing_tracks.clone();
            let rebuild  = rebuild_track_list.clone();
            let win_wk   = win.downgrade();
            btn_add_files_pl.connect_clicked(move |_| {
                let dialog = gtk4::FileDialog::builder().title("Add Audio Files").build();
                let filter = gtk4::FileFilter::new();
                filter.set_name(Some("Audio files"));
                // add_suffix (not add_mime_type) so files appear even when
                // the desktop has no MIME registration (.ape, .tta, …).
                for ext in crate::model::AUDIO_EXTENSIONS {
                    filter.add_suffix(ext);
                }
                let fs = gio::ListStore::new::<gtk4::FileFilter>();
                fs.append(&filter);
                dialog.set_filters(Some(&fs));
                let state2  = state_rc.clone();
                let et2     = et.clone();
                let rebuild2 = rebuild.clone();
                let parent  = win_wk.upgrade();
                dialog.open_multiple(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                    let Ok(list) = result else { return };
                    let paths: Vec<PathBuf> = (0..list.n_items())
                        .filter_map(|i| list.item(i))
                        .filter_map(|o| o.downcast::<gio::File>().ok())
                        .filter_map(|f| f.path())
                        .collect();
                    if paths.is_empty() { return; }
                    let s = state2.borrow();
                    if let Some(ref lib) = s.media_lib {
                        let existing: std::collections::HashSet<String> =
                            et2.borrow().iter().map(|t| t.path.clone()).collect();
                        for p in &paths {
                            if let Some(p_str) = p.to_str() {
                                if !existing.contains(p_str) {
                                    if let Ok(t) = lib.track_by_path(p_str) {
                                        et2.borrow_mut().push(t);
                                    }
                                }
                            }
                        }
                    }
                    drop(s);
                    rebuild2();
                });
            });
        }

        // ── Add Folder ────────────────────────────────────────────────────
        {
            let state_rc = state.clone();
            let et       = editing_tracks.clone();
            let rebuild  = rebuild_track_list.clone();
            let win_wk   = win.downgrade();
            btn_add_folder_pl.connect_clicked(move |_| {
                let dialog = gtk4::FileDialog::builder().title("Add Folder").build();
                let state2   = state_rc.clone();
                let et2      = et.clone();
                let rebuild2 = rebuild.clone();
                let parent   = win_wk.upgrade();
                dialog.select_folder(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                    let Ok(file) = result else { return };
                    let Some(folder) = file.path() else { return };
                    let Some(folder_str) = folder.to_str() else { return };
                    let s = state2.borrow();
                    if let Some(ref lib) = s.media_lib {
                        let existing: std::collections::HashSet<String> =
                            et2.borrow().iter().map(|t| t.path.clone()).collect();
                        let new_tracks: Vec<_> = lib.all_tracks().unwrap_or_default()
                            .into_iter()
                            .filter(|t| t.path.starts_with(folder_str) && !existing.contains(&t.path))
                            .collect();
                        et2.borrow_mut().extend(new_tracks);
                    }
                    drop(s);
                    rebuild2();
                });
            });
        }

        // ── Remove selected tracks ────────────────────────────────────────
        {
            let sel     = edit_multi_sel.clone();
            let et      = editing_tracks.clone();
            let rebuild = rebuild_track_list.clone();
            btn_remove_tracks.connect_clicked(move |_| {
                // Map display-index selection through EditorEntry so each
                // selected row resolves to its canonical play-order slot.
                // Otherwise duplicates / a non-default sort cause the wrong
                // rows to be removed from `editing_tracks`.
                let mut to_remove: Vec<usize> = (0..sel.n_items())
                    .filter(|i| sel.is_selected(*i))
                    .filter_map(|i| sel.item(i))
                    .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                    .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                    .collect();
                if to_remove.is_empty() { return }
                to_remove.sort_unstable_by(|a, b| b.cmp(a));
                let mut tracks = et.borrow_mut();
                for idx in to_remove.into_iter() {
                    if idx < tracks.len() { tracks.remove(idx); }
                }
                drop(tracks);
                rebuild();
            });
        }

        // ── Revert ────────────────────────────────────────────────────────
        {
            let load    = load_pl_by_id.clone();
            let sidebar_ref = sidebar.clone();
            btn_revert_pl.connect_clicked(move |_| {
                // Find currently-selected sidebar pl: row
                let mut i = 0i32;
                loop {
                    match sidebar_ref.row_at_index(i) {
                        Some(row) => {
                            let name = row.widget_name().to_string();
                            if row.is_selected() {
                                if let Some(id_str) = name.strip_prefix("pl:") {
                                    if let Ok(id) = id_str.parse::<i64>() { load(id); }
                                }
                                break;
                            }
                            i += 1;
                        }
                        None => break,
                    }
                }
            });
        }

        // ── Save As playlist ──────────────────────────────────────────────
        {
            let state_rc     = state.clone();
            let et           = editing_tracks.clone();
            let ep_id        = editing_pl_id.clone();
            let load         = load_pl_by_id.clone();
            let sidebar_ref  = sidebar.clone();
            let pl_ml_ref    = pl_manage_list.clone();
            let win_wk       = win.downgrade();
            btn_save_as_pl.connect_clicked(move |_| {
                let Some(win) = win_wk.upgrade() else { return };
                // Pre-fill the Save dialog with the current playlist's name
                // (or "New Playlist" when the editor has no playlist loaded).
                let initial_stem = if ep_id.get() >= 0 {
                    state_rc.borrow().media_lib.as_ref()
                        .and_then(|lib| lib.playlist_by_id(ep_id.get()).ok())
                        .map(|pl| pl.name)
                        .unwrap_or_else(|| "New Playlist".to_string())
                } else {
                    "New Playlist".to_string()
                };
                let paths: Vec<String> = et.borrow().iter().map(|t| t.path.clone()).collect();
                let state2   = state_rc.clone();
                let ep_id2   = ep_id.clone();
                let load2    = load.clone();
                let sidebar2 = sidebar_ref.clone();
                let pl_ml2   = pl_ml_ref.clone();
                // Native Save dialog replaces the previous name-only popup —
                // user chooses both filename and folder so the new .m3u8
                // doesn't silently land in the managed-playlists dir (which
                // `add_playlist_file` then registered as a watched folder).
                run_playlist_save_dialog(state_rc.clone(), win, &initial_stem, move |path, win_cb| {
                    let new_name = path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Untitled")
                        .to_string();
                    let save_result = state2.borrow().media_lib.as_ref()
                        .map(|lib| lib.save_playlist_tracks_to_path(&path, &paths));
                    let new_id = match save_result {
                        Some(Ok(id)) => id,
                        Some(Err(e)) => {
                            eprintln!("save_playlist_tracks_to_path: {e}");
                            show_playlist_save_error(&win_cb, &path, &e);
                            return;
                        }
                        None => return,
                    };

                    // Add row to manage list + sidebar
                    let lbl = Label::builder()
                        .label(&new_name)
                        .halign(Align::Start)
                        .margin_start(8).margin_end(8)
                        .margin_top(3).margin_bottom(3)
                        .build();
                    let manage_row = ListBoxRow::new();
                    manage_row.set_widget_name(&new_id.to_string());
                    manage_row.set_child(Some(&lbl));
                    attach_pl_row_drag(&manage_row, new_id);
                    pl_ml2.append(&manage_row);

                    let s_lbl = Label::builder()
                        .label(&new_name)
                        .halign(Align::Start)
                        .xalign(0.0)
                        .margin_start(24).margin_end(8)
                        .margin_top(4).margin_bottom(4)
                        .build();
                    let s_row = ListBoxRow::new();
                    s_row.set_widget_name(&format!("pl:{}", new_id));
                    s_row.set_child(Some(&s_lbl));
                    attach_pl_row_drag(&s_row, new_id);
                    sidebar2.insert(&s_row, sidebar_pl_end_index(&sidebar2));
                    sidebar2.select_row(Some(&s_row));

                    ep_id2.set(new_id);
                    load2(new_id);
                });
            });
        }

        // ── Save playlist ─────────────────────────────────────────────────
        {
            let state_rc    = state.clone();
            let et          = editing_tracks.clone();
            let saved       = saved_track_ids.clone();
            let ep_id       = editing_pl_id.clone();
            btn_save_pl.connect_clicked(move |_| {
                let id = ep_id.get();
                if id < 0 { return; }
                let track_ids: Vec<i64> = et.borrow().iter().map(|t| t.id).collect();
                if let Some(ref lib) = state_rc.borrow().media_lib {
                    let _ = lib.save_playlist_tracks(id, &track_ids);
                    *saved.borrow_mut() = track_ids;
                }
            });
        }

        // ── Play (replace active playlist; honour autoplay) ──────────────
        {
            let state_rc   = state.clone();
            let et         = editing_tracks.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let set_track2 = set_track.clone();
            btn_play_pl.connect_clicked(move |_| {
                let tracks: Vec<crate::media_library::LibTrack> = et.borrow().clone();
                if tracks.is_empty() { return; }
                let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                {
                    let mut s = state_rc.borrow_mut();
                    let _ = s.player.stop();
                    s.playlist = crate::model::Playlist::new();
                    for lt in &tracks {
                        s.playlist.add(crate::model::Track::from(lt));
                    }
                }
                if autoplay {
                    if let Some(display) = state_rc.borrow_mut().play_current() {
                        set_track2(&display);
                    }
                }
                rebuild_pl();
            });
        }

        // ── Enqueue (append to active playlist) ──────────────────────────
        {
            let state_rc   = state.clone();
            let et         = editing_tracks.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let set_track2 = set_track.clone();
            btn_enqueue_pl.connect_clicked(move |_| {
                let tracks: Vec<crate::media_library::LibTrack> = et.borrow().clone();
                if tracks.is_empty() { return; }
                let was_empty = state_rc.borrow().playlist.is_empty();
                let autoplay  = state_rc.borrow().config.behavior.autoplay_on_add;
                {
                    let mut s = state_rc.borrow_mut();
                    for lt in &tracks {
                        s.playlist.add(crate::model::Track::from(lt));
                    }
                }
                // Don't interrupt a track the user is already listening to.
                if autoplay && was_empty {
                    if let Some(display) = state_rc.borrow_mut().play_current() {
                        set_track2(&display);
                    }
                }
                rebuild_pl();
            });
        }

        // ── Delete this playlist ─────────────────────────────────────────
        {
            let state_rc      = state.clone();
            let ep_id         = editing_pl_id.clone();
            let pl_list_ref   = pl_manage_list.clone();
            let sidebar_ref   = sidebar.clone();
            let sub_rows_ref  = pl_sub_rows.clone();
            let pl_sub_ref    = pl_sub_stack.clone();
            let et            = editing_tracks.clone();
            let saved         = saved_track_ids.clone();
            let rebuild       = rebuild_track_list.clone();
            let win_wk        = win.downgrade();
            btn_delete_pl.connect_clicked(move |_| {
                let id = ep_id.get();
                if id < 0 { return; }
                let pl_name = state_rc.borrow().media_lib.as_ref()
                    .and_then(|lib| lib.playlist_by_id(id).ok())
                    .map(|pl| pl.name.clone())
                    .unwrap_or_default();

                let dialog = gtk4::AlertDialog::builder()
                    .message(format!("Delete \"{}\"?", pl_name))
                    .detail("The playlist file on disk is not deleted.")
                    .buttons(vec!["Cancel".to_string(), "Delete".to_string()])
                    .cancel_button(0).default_button(1).modal(true).build();

                let state2   = state_rc.clone();
                let ep_id2   = ep_id.clone();
                let pl_ref2  = pl_list_ref.clone();
                let sid2     = sidebar_ref.clone();
                let sub2     = sub_rows_ref.clone();
                let pls2     = pl_sub_ref.clone();
                let et2      = et.clone();
                let saved2   = saved.clone();
                let rebuild2 = rebuild.clone();
                dialog.choose(win_wk.upgrade().as_ref(), None::<&gio::Cancellable>, move |result| {
                    if result != Ok(1) { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.remove_playlist(id);
                    }
                    // Drop the manage-list row whose widget_name == id.
                    let target = id.to_string();
                    let mut i = 0i32;
                    loop {
                        match pl_ref2.row_at_index(i) {
                            Some(r) if r.widget_name() == target => {
                                pl_ref2.remove(&r);
                                break;
                            }
                            Some(_) => i += 1,
                            None => break,
                        }
                    }
                    // Drop the matching sidebar sub-row.
                    let target_s = format!("pl:{}", id);
                    sub2.borrow_mut().retain(|r| {
                        if r.widget_name() == target_s {
                            sid2.remove(r);
                            false
                        } else { true }
                    });
                    // Clear editing state and bounce back to the manage page.
                    ep_id2.set(-1);
                    et2.borrow_mut().clear();
                    saved2.borrow_mut().clear();
                    rebuild2();
                    pls2.set_visible_child_name("pl-manage");
                });
            });
        }

        // ── Rename this playlist (header-row button) ─────────────────────
        {
            let state_rc      = state.clone();
            let ep_id         = editing_pl_id.clone();
            let header_ref    = edit_header.clone();
            let pl_list_ref   = pl_manage_list.clone();
            let sidebar_ref   = sidebar.clone();
            let win_wk        = win.downgrade();
            btn_rename_pl_inline.connect_clicked(move |_| {
                let id = ep_id.get();
                if id < 0 { return; }
                let current = state_rc.borrow().media_lib.as_ref()
                    .and_then(|lib| lib.playlist_by_id(id).ok())
                    .map(|pl| pl.name.clone())
                    .unwrap_or_default();

                let dialog = gtk4::Window::builder()
                    .title("Rename Playlist").modal(true).resizable(false).default_width(300)
                    .build();
                if let Some(w) = win_wk.upgrade() { dialog.set_transient_for(Some(&w)); }
                let vbox = GtkBox::new(Orientation::Vertical, 8);
                vbox.set_margin_top(12); vbox.set_margin_bottom(12);
                vbox.set_margin_start(12); vbox.set_margin_end(12);
                let lbl = Label::builder().label("New name:").halign(Align::Start).build();
                let name_entry = Entry::new();
                name_entry.set_text(&gtk_safe(&current));
                name_entry.set_hexpand(true);
                let btns_box = GtkBox::new(Orientation::Horizontal, 6);
                btns_box.set_halign(Align::End);
                let cancel_btn = Button::with_label("Cancel");
                let ok_btn     = Button::with_label("Rename");
                ok_btn.add_css_class("suggested-action");
                btns_box.append(&cancel_btn); btns_box.append(&ok_btn);
                vbox.append(&lbl); vbox.append(&name_entry); vbox.append(&btns_box);
                dialog.set_child(Some(&vbox));

                let d = dialog.clone();
                cancel_btn.connect_clicked(move |_| { d.close(); });

                let d        = dialog.clone();
                let e        = name_entry.clone();
                let state2   = state_rc.clone();
                let header2  = header_ref.clone();
                let pl_ref2  = pl_list_ref.clone();
                let sid2     = sidebar_ref.clone();
                ok_btn.connect_clicked(move |_| {
                    let name = e.text().to_string();
                    let name = name.trim();
                    if name.is_empty() { return; }
                    if let Some(ref lib) = state2.borrow().media_lib {
                        let _ = lib.rename_playlist(id, name);
                    }
                    header2.set_text(&gtk_safe(name));
                    // Update manage-list row label.
                    let target = id.to_string();
                    let mut i = 0i32;
                    loop {
                        match pl_ref2.row_at_index(i) {
                            Some(r) if r.widget_name() == target => {
                                if let Some(c) = r.child() {
                                    if let Ok(l) = c.downcast::<Label>() {
                                        l.set_text(&gtk_safe(name));
                                    }
                                }
                                break;
                            }
                            Some(_) => i += 1,
                            None => break,
                        }
                    }
                    // Update sidebar sub-row label.
                    let target_s = format!("pl:{}", id);
                    let mut j = 0i32;
                    loop {
                        match sid2.row_at_index(j) {
                            Some(r) if r.widget_name() == target_s => {
                                if let Some(c) = r.child() {
                                    if let Ok(l) = c.downcast::<Label>() {
                                        l.set_text(&gtk_safe(name));
                                    }
                                }
                                break;
                            }
                            Some(_) => j += 1,
                            None => break,
                        }
                    }
                    d.close();
                });
                let ok2 = ok_btn.clone();
                name_entry.connect_activate(move |_| { ok2.activate(); });
                dialog.present();
            });
        }

        // ── Right-click context menu on track rows ───────────────────────
        // Add to / Replace active playlist, Edit ID3 (single only), Remove
        // from Library.  No album-art viewer in GTK so that entry is
        // omitted here.
        {
            // ctx_canonical_idx is now hoisted above the column builder so each
            // editor cell's right-click gesture can record into it.  Reuse
            // the outer binding so action handlers see the same Cell.
            let action_group = gio::SimpleActionGroup::new();

            // Helper: collect the canonical indices the action should
            // operate on — the current multi-selection, falling back to
            // the single right-clicked row when nothing is selected.
            let selected_canonical_indices = {
                let sel = edit_multi_sel.clone();
                let id_ref = ctx_canonical_idx.clone();
                Rc::new(move || -> Vec<usize> {
                    let mut idxs: Vec<usize> = (0..sel.n_items())
                        .filter(|i| sel.is_selected(*i))
                        .filter_map(|i| sel.item(i))
                        .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                        .collect();
                    if idxs.is_empty() {
                        let c = id_ref.get();
                        if c >= 0 { idxs.push(c as usize); }
                    }
                    idxs
                })
            };

            // ─── Append (add to active playlist) ─────────────────────────
            {
                let state_rc   = state.clone();
                let et         = editing_tracks.clone();
                let rebuild_pl = rebuild_playlist.clone();
                let set_track2 = set_track.clone();
                let pick_idxs  = selected_canonical_indices.clone();
                let action     = gio::SimpleAction::new("append", None);
                action.connect_activate(move |_, _| {
                    let tracks: Vec<crate::media_library::LibTrack> = {
                        let et_b = et.borrow();
                        pick_idxs().into_iter()
                            .filter_map(|i| et_b.get(i).cloned())
                            .collect()
                    };
                    if tracks.is_empty() { return }
                    let was_empty = state_rc.borrow().playlist.is_empty();
                    let autoplay  = state_rc.borrow().config.behavior.autoplay_on_add;
                    {
                        let mut s = state_rc.borrow_mut();
                        for lt in &tracks {
                            s.playlist.add(crate::model::Track::from(lt));
                        }
                    }
                    if autoplay && was_empty {
                        if let Some(display) = state_rc.borrow_mut().play_current() {
                            set_track2(&display);
                        }
                    }
                    rebuild_pl();
                });
                action_group.add_action(&action);
            }

            // ─── Replace (active playlist becomes the selection) ─────────
            {
                let state_rc   = state.clone();
                let et         = editing_tracks.clone();
                let rebuild_pl = rebuild_playlist.clone();
                let set_track2 = set_track.clone();
                let pick_idxs  = selected_canonical_indices.clone();
                let action     = gio::SimpleAction::new("replace", None);
                action.connect_activate(move |_, _| {
                    let tracks: Vec<crate::media_library::LibTrack> = {
                        let et_b = et.borrow();
                        pick_idxs().into_iter()
                            .filter_map(|i| et_b.get(i).cloned())
                            .collect()
                    };
                    if tracks.is_empty() { return }
                    let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                    {
                        let mut s = state_rc.borrow_mut();
                        let _ = s.player.stop();
                        s.playlist = crate::model::Playlist::new();
                        for lt in &tracks {
                            s.playlist.add(crate::model::Track::from(lt));
                        }
                    }
                    if autoplay {
                        if let Some(display) = state_rc.borrow_mut().play_current() {
                            set_track2(&display);
                        }
                    }
                    rebuild_pl();
                });
                action_group.add_action(&action);
            }

            // ─── Edit ID3 (single only) ──────────────────────────────────
            {
                let state_rc      = state.clone();
                let id_ref        = ctx_canonical_idx.clone();
                let et            = editing_tracks.clone();
                let rebuild_pl    = rebuild_playlist.clone();
                let action        = gio::SimpleAction::new("edit-id3", None);
                action.connect_activate(move |_, _| {
                    let c = id_ref.get();
                    if c < 0 { return }
                    let path = et.borrow().get(c as usize)
                        .map(|t| t.path.clone());
                    let Some(path) = path else {
                        return;
                    };
                    open_id3_editor_window(
                        None::<&gtk4::Window>,
                        path.into(),
                        state_rc.clone(),
                        rebuild_pl.clone(),
                        None,
                    );
                });
                action_group.add_action(&action);
            }

            // ─── Remove from Playlist (mutate editing_tracks + persist) ──
            // Removes selected rows from the canonical play order and
            // immediately rewrites the on-disk M3U8.  Does NOT delete the
            // track from the media library — the user's library DB is
            // untouched.
            {
                let state_rc = state.clone();
                let et       = editing_tracks.clone();
                let ep_id    = editing_pl_id.clone();
                let rebuild  = rebuild_track_list.clone();
                let pick_idxs = selected_canonical_indices.clone();
                let action   = gio::SimpleAction::new("remove", None);
                action.connect_activate(move |_, _| {
                    let mut idxs = pick_idxs();
                    if idxs.is_empty() { return }
                    idxs.sort_unstable_by(|a, b| b.cmp(a));
                    {
                        let mut e = et.borrow_mut();
                        for i in idxs.iter() {
                            if *i < e.len() { e.remove(*i); }
                        }
                    }
                    let pid = ep_id.get();
                    if pid >= 0 {
                        let s = state_rc.borrow();
                        if let Some(lib) = s.media_lib.as_ref() {
                            let paths: Vec<String> = et.borrow()
                                .iter().map(|t| t.path.clone()).collect();
                            if let Ok(pl) = lib.playlist_by_id(pid) {
                                if let Err(e) = lib.save_playlist_tracks_to_path(
                                    std::path::Path::new(&pl.path),
                                    &paths,
                                ) {
                                    eprintln!("ple.remove persist {pid}: {e}");
                                }
                            }
                        }
                    }
                    rebuild();
                });
                action_group.add_action(&action);
            }

            // ─── Seed a new saved playlist from the editor selection ─────
            {
                let state_rc = state.clone();
                let sel      = edit_multi_sel.clone();
                let et       = editing_tracks.clone();
                let win_atn  = win.clone();
                let action   = gio::SimpleAction::new("add-to-new", None);
                action.connect_activate(move |_, _| {
                    let paths: Vec<String> = {
                        let et_b = et.borrow();
                        // Selection indices are display positions in the
                        // sorted model — map each through EditorEntry to
                        // the canonical play-order slot so duplicates and
                        // non-default sorts both resolve correctly.
                        let mut p: Vec<String> = (0..sel.n_items())
                            .filter(|i| sel.is_selected(*i))
                            .filter_map(|i| sel.item(i))
                            .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                            .filter_map(|c| et_b.get(c))
                            .map(|t| t.path.clone())
                            .collect();
                        if p.is_empty() {
                            p = et_b.iter().map(|t| t.path.clone()).collect();
                        }
                        p
                    };
                    if paths.is_empty() { return }
                    let default_stem = glib::DateTime::now_local()
                        .ok()
                        .and_then(|dt| dt.format("Playlist %Y-%m-%d %H-%M").ok())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "Playlist".to_string());
                    let state_cb = state_rc.clone();
                    let paths_cb = paths.clone();
                    run_playlist_save_dialog(
                        state_rc.clone(),
                        win_atn.clone(),
                        &default_stem,
                        move |path, win_cb| {
                            if let Some(lib) = state_cb.borrow().media_lib.as_ref() {
                                if let Err(e) = lib.save_playlist_tracks_to_path(&path, &paths_cb) {
                                    eprintln!("save_playlist_tracks_to_path: {e}");
                                    show_playlist_save_error(&win_cb, &path, &e);
                                }
                            }
                        },
                    );
                });
                action_group.add_action(&action);
            }

            // ─── Add selection to a saved playlist (parameterised by id) ─
            {
                let state_rc = state.clone();
                let sel      = edit_multi_sel.clone();
                let et       = editing_tracks.clone();
                let action   = gio::SimpleAction::new(
                    "add-to-saved",
                    Some(glib::VariantTy::INT64),
                );
                action.connect_activate(move |_, param| {
                    let Some(pid) = param.and_then(|p| p.get::<i64>()) else { return };
                    let paths: Vec<String> = {
                        let et_borrow = et.borrow();
                        (0..sel.n_items())
                            .filter(|i| sel.is_selected(*i))
                            .filter_map(|i| sel.item(i))
                            .filter_map(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            .map(|o| o.borrow::<EditorEntry>().canonical_idx)
                            .filter_map(|c| et_borrow.get(c))
                            .map(|t| t.path.clone())
                            .collect()
                    };
                    if paths.is_empty() { return }
                    let mut ok = false;
                    if let Some(lib) = state_rc.borrow().media_lib.as_ref() {
                        match lib.append_paths_to_playlist(pid, &paths) {
                            Ok(_)  => ok = true,
                            Err(e) => eprintln!("append_paths_to_playlist {pid}: {e}"),
                        }
                    }
                    if ok { notify_playlist_changed(pid); }
                });
                action_group.add_action(&action);
            }

            track_list.insert_action_group("ple", Some(&action_group));
            if let Some(ref ts) = *track_scroll_holder.borrow() {
                ts.insert_action_group("ple", Some(&action_group));
            }
            win.insert_action_group("ple", Some(&action_group));
            // ALSO attach the actions to the GtkApplication (app-level)
            // under "app-ple-*" names — PopoverMenu dispatch via the
            // app prefix is the reliable code path in GTK4, even when
            // widget-tree action lookup fails for nested popovers.
            if let Some(app) = win.application() {
                let app_action_names = ["append", "replace", "edit-id3",
                                        "remove", "add-to-new", "add-to-saved"];
                for name in app_action_names {
                    if let Some(act) = action_group.lookup_action(name) {
                        let app_name = format!("ple-{name}");
                        let simple = act.downcast_ref::<gio::SimpleAction>();
                        if let Some(sa) = simple {
                            // Build a parallel app-level SimpleAction
                            // that forwards activate to the editor's
                            // group action.  Same parameter type.
                            let app_action = gio::SimpleAction::new(
                                &app_name,
                                sa.parameter_type().as_ref().map(|v| &**v),
                            );
                            let sa_clone = sa.clone();
                            app_action.connect_activate(move |_, param| {
                                eprintln!("[app.{app_name}] forwarding to ple.{name}");
                                sa_clone.activate(param);
                            });
                            app.add_action(&app_action);
                        }
                    }
                }
            }
            *ple_action_group_holder.borrow_mut() = Some(action_group.clone());
            // Per-cell right-click gesture lives inside each column's
            // factory.connect_setup — see the editor column builder at the
            // top of this scope.  Nothing to register here at the row level.

            // Double-click / Enter activates the row: append to the active
            // playlist (matches the ML files view affordance).  Respects
            // the user's playlist_add_behavior preference (Append vs Replace)
            // and autoplay_on_add config.
            {
                let state_rc     = state.clone();
                let et           = editing_tracks.clone();
                let rebuild_pl   = rebuild_playlist.clone();
                let set_track_pe = set_track.clone();
                let sel_act = edit_multi_sel.clone();
                track_list.connect_activate(move |_, pos| {
                    // `pos` is a display position; resolve through the
                    // sorted model to the canonical row in `editing_tracks`.
                    let canon = sel_act.item(pos)
                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        .map(|o| o.borrow::<EditorEntry>().canonical_idx);
                    let Some(canon) = canon else { return };
                    let lt = et.borrow().get(canon).cloned();
                    let Some(lt) = lt else { return };
                    let was_empty = state_rc.borrow().playlist.is_empty();
                    let autoplay = state_rc.borrow().config.behavior.autoplay_on_add;
                    let should_replace = state_rc.borrow().config.behavior.playlist_add_behavior
                        == crate::config::PlaylistAddBehavior::Replace;
                    if should_replace {
                        let _ = state_rc.borrow_mut().player.stop();
                        state_rc.borrow_mut().playlist.clear();
                    }
                    state_rc.borrow_mut().playlist.add(crate::model::Track::from(&lt));
                    if autoplay && (was_empty || should_replace) {
                        if let Some(display) = state_rc.borrow_mut().play_current() {
                            set_track_pe(&display);
                        }
                    }
                    rebuild_pl();
                });
            }
        }

        pl_sub_stack.add_named(&edit_vbox, Some("pl-edit"));
    }

    {
        let pl_vbox = GtkBox::new(Orientation::Vertical, 0);
        pl_vbox.append(&*pl_sub_stack);
        stack.add_named(&pl_vbox, Some("playlists"));
    }

    // Wire sidebar to stack.
    {
        let stack_ref      = stack.clone();
        let pl_sub_ref     = pl_sub_stack.clone();
        let load           = load_pl_by_id.clone();
        let state_rc       = state.clone();
        let expanded_rc    = playlists_expanded.clone();
        let hdr_lbl        = edit_header.clone();
        let path_lbl       = edit_path_label.clone();
        let save_btn       = btn_save_pl_outer.clone();
        sidebar.connect_row_selected(move |_, opt_row| {
            let row = match opt_row { Some(r) => r, None => return };
            let name = row.widget_name().to_string();

            if name == "files" {
                stack_ref.set_visible_child_name("files");
            } else if name == "playlists" {
                stack_ref.set_visible_child_name("playlists");
                pl_sub_ref.set_visible_child_name("pl-manage");
                // Expand sub-rows on navigation
                if !expanded_rc.get() {
                    expanded_rc.set(true);
                }
            } else if let Some(id_str) = name.strip_prefix("pl:") {
                if let Ok(id) = id_str.parse::<i64>() {
                    stack_ref.set_visible_child_name("playlists");
                    load(id);
                    pl_sub_ref.set_visible_child_name("pl-edit");
                    // Update editor header, path bar, and Save sensitivity.
                    if let Some(ref lib) = state_rc.borrow().media_lib {
                        if let Ok(pl) = lib.playlist_by_id(id) {
                            hdr_lbl.set_text(&gtk_safe(&pl.name));
                            path_lbl.set_text(&gtk_safe(&pl.path));
                            // Disable Save for external playlists; user should
                            // use Save As to get a Sparkamp-managed copy.
                            let is_managed = lib.playlist_is_managed(id);
                            save_btn.set_sensitive(is_managed);
                        }
                    }
                }
            }
        });
    }

    // Persist sidebar expansion state on window close (handled in close_request below).


    // ── Device detection: poll udisks2 and keep the sidebar live ──────────
    // A 2 s poll (rather than D-Bus signal wiring) keeps this simple while
    // still updating in place — devices appear/disappear and free space
    // refreshes without reopening the window.
    // Deferred handles to the eject / sync runners (defined further down, once
    // the refresh + reload closures they need exist). The overview rows' Sync
    // and Eject buttons call through these.
    let eject_run_holder: Rc<RefCell<Option<Rc<dyn Fn(String)>>>> =
        Rc::new(RefCell::new(None));
    let sync_run_holder: Rc<RefCell<Option<Rc<dyn Fn(crate::devices::Device, Button)>>>> =
        Rc::new(RefCell::new(None));

    // Rebuild the device overview list (shown when the Devices header is
    // selected) from the latest detection results. Each device is its own row
    // with Sync and Eject buttons on the right.
    let rebuild_overview: Rc<dyn Fn()> = {
        let list = dev_overview_list.clone();
        let current = current_devices.clone();
        let eject_holder = eject_run_holder.clone();
        let sync_holder = sync_run_holder.clone();
        let counts_cache = device_counts.clone();
        let counts_inflight = counts_in_flight.clone();
        let transfers = device_transfers.clone();
        let card_bars = device_card_progress.clone();
        let sidebar_ov = sidebar.clone();
        Rc::new(move || {
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            // Card progress bars are rebuilt below; drop the stale references.
            card_bars.borrow_mut().clear();
            let devs = current.borrow();
            if devs.is_empty() {
                let l = Label::builder()
                    .label("No devices connected.")
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                l.add_css_class("status-label");
                list.append(&l);
                return;
            }
            for d in devs.iter() {
                let name = if d.label.is_empty() {
                    "Untitled device".to_string()
                } else {
                    d.label.clone()
                };

                // ── Card ────────────────────────────────────────────────
                let card = GtkBox::new(Orientation::Vertical, 6);
                card.add_css_class("device-card");

                // Header: icon · name + filesystem · status badges.
                let header = GtkBox::new(Orientation::Horizontal, 10);
                let icon = Image::from_icon_name(device_icon_name(d));
                icon.set_pixel_size(32);
                icon.set_valign(Align::Center);
                header.append(&icon);

                let title_box = GtkBox::new(Orientation::Vertical, 0);
                title_box.set_hexpand(true);
                title_box.set_valign(Align::Center);
                let name_lbl = Label::builder()
                    .label(&gtk_safe(&name))
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                name_lbl.add_css_class("device-card-name");
                let fs_lbl = Label::builder()
                    .label(if d.fs_type.is_empty() { "unknown" } else { &d.fs_type })
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                fs_lbl.add_css_class("status-label");
                title_box.append(&name_lbl);
                title_box.append(&fs_lbl);
                header.append(&title_box);

                let badges = GtkBox::new(Orientation::Horizontal, 4);
                badges.set_valign(Align::Center);
                if d.read_only {
                    let b = Label::new(Some("🔒 Read-only"));
                    b.add_css_class("device-badge");
                    badges.append(&b);
                }
                if device_fs_unsupported(&d.fs_type) {
                    let b = Label::new(Some("⚠ Unsupported"));
                    b.add_css_class("device-badge");
                    b.add_css_class("device-badge-warn");
                    b.set_tooltip_text(Some(UNSUPPORTED_FS_TOOLTIP));
                    badges.append(&b);
                }
                header.append(&badges);
                // Clicking the card's banner (icon + name area) opens that
                // device's detail page by selecting its sidebar row, which the
                // row-selected handler turns into the detail view. The Sync/Eject
                // buttons live in their own row below and claim their own clicks.
                {
                    let click = gtk4::GestureClick::new();
                    let sidebar = sidebar_ov.clone();
                    let row_name = format!("dev:{}", d.backend_id);
                    click.connect_released(move |_, _, _, _| {
                        if let Some(row) = find_row_by_name(&sidebar, &row_name) {
                            sidebar.select_row(Some(&row));
                        }
                    });
                    header.add_controller(click);
                    header.set_cursor_from_name(Some("pointer"));
                }
                card.append(&header);

                // Capacity bar + free/total text.
                let used = if d.total_bytes > 0 {
                    1.0 - (d.free_bytes as f64 / d.total_bytes as f64)
                } else {
                    0.0
                };
                let bar = gtk4::LevelBar::new();
                bar.set_min_value(0.0);
                bar.set_max_value(1.0);
                bar.set_value(used);
                set_levelbar_fullness(&bar, used);
                card.append(&bar);

                let cap_lbl = Label::builder()
                    .label(&format!(
                        "{:.1} GB free of {:.1} GB",
                        d.free_bytes as f64 / 1e9,
                        d.total_bytes as f64 / 1e9,
                    ))
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                cap_lbl.add_css_class("status-label");
                card.append(&cap_lbl);

                // Song / playlist counts — cached, computed off-thread on miss.
                let counts_lbl = Label::builder()
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                counts_lbl.add_css_class("status-label");
                match counts_cache.borrow().get(&d.backend_id).copied() {
                    Some((songs, pls)) => {
                        counts_lbl.set_text(&counts_text(songs, pls));
                    }
                    None => {
                        counts_lbl.set_text("counting…");
                        let backend = d.backend_id.clone();
                        if counts_inflight.borrow_mut().insert(backend.clone()) {
                            let mount = d.mount_path.clone();
                            let cache = counts_cache.clone();
                            let inflight = counts_inflight.clone();
                            let lbl = counts_lbl.clone();
                            glib::spawn_future_local(async move {
                                let res = gio::spawn_blocking(move || {
                                    if device_io_shutting_down() {
                                        return (0, 0);
                                    }
                                    let songs =
                                        crate::devices::browse::list_audio_files(&mount).len();
                                    let pls = crate::devices::browse::device_playlist_files(&mount)
                                        .len();
                                    (songs, pls)
                                })
                                .await
                                .unwrap_or((0, 0));
                                cache.borrow_mut().insert(backend.clone(), res);
                                inflight.borrow_mut().remove(&backend);
                                lbl.set_text(&counts_text(res.0, res.1));
                            });
                        }
                    }
                }
                card.append(&counts_lbl);

                // Copy progress bar — always present (reserves its space) so the
                // card height is identical whether or not a transfer is running.
                // Transparent when idle; the runners drive it via backend_id.
                let prog = gtk4::ProgressBar::new();
                prog.set_show_text(true);
                apply_card_progress(&prog, transfers.borrow().get(&d.backend_id).copied());
                card.append(&prog);
                card_bars.borrow_mut().insert(d.backend_id.clone(), prog);

                // Sync / Eject buttons, right-aligned.
                let btn_row = GtkBox::new(Orientation::Horizontal, 6);
                btn_row.set_halign(Align::End);
                btn_row.set_margin_top(2);

                let sync_btn = Button::with_label("Sync");
                sync_btn.add_css_class("pl-btn");
                {
                    let holder = sync_holder.clone();
                    let dev = d.clone();
                    sync_btn.connect_clicked(move |btn| {
                        if let Some(run) = holder.borrow().as_ref() {
                            run(dev.clone(), btn.clone());
                        }
                    });
                }
                btn_row.append(&sync_btn);

                let eject_btn = Button::with_label("Eject");
                eject_btn.add_css_class("pl-btn");
                // Unavailable while a copy to this device is running.
                eject_btn.set_sensitive(
                    d.ejectable && !transfers.borrow().contains_key(&d.backend_id),
                );
                {
                    let holder = eject_holder.clone();
                    let backend = d.backend_id.clone();
                    eject_btn.connect_clicked(move |btn| {
                        btn.set_sensitive(false);
                        if let Some(run) = holder.borrow().as_ref() {
                            run(backend.clone());
                        }
                    });
                }
                btn_row.append(&eject_btn);
                card.append(&btn_row);

                list.append(&card);
            }
        })
    };

    let refresh_devices: Rc<dyn Fn()> = {
        let sidebar = sidebar.clone();
        let dev_sub_rows = dev_sub_rows.clone();
        let devices_expanded = devices_expanded.clone();
        let current_devices = current_devices.clone();
        let banner = dev_banner.clone();
        let banner_lbl = dev_banner_lbl.clone();
        let rebuild_overview = rebuild_overview.clone();
        // Guard against overlapping polls stacking up.
        let in_flight = Rc::new(Cell::new(false));
        Rc::new(move || {
            if in_flight.get() {
                return;
            }
            in_flight.set(true);
            let sidebar = sidebar.clone();
            let dev_sub_rows = dev_sub_rows.clone();
            let devices_expanded = devices_expanded.clone();
            let current_devices = current_devices.clone();
            let banner = banner.clone();
            let banner_lbl = banner_lbl.clone();
            let rebuild_overview = rebuild_overview.clone();
            let in_flight = in_flight.clone();
            // udisks2 access runs on a worker thread so a stalled D-Bus call
            // can never freeze the UI — a main-thread block previously made
            // the app impossible to quit or eject after a copy.
            glib::spawn_future_local(async move {
                // Enumerate MTP mount metadata on the main thread (cheap, no
                // FUSE IO), then resolve storage roots + list udisks devices on
                // the worker thread so no gvfs filesystem call blocks the UI.
                let mtp_raw = enumerate_mtp_raw();
                let result = gio::spawn_blocking(move || {
                    let udisks = crate::devices::detect::list_devices();
                    let mtp: Vec<crate::devices::Device> =
                        mtp_raw.into_iter().filter_map(mtp_raw_to_device).collect();
                    (udisks, mtp)
                })
                .await;
                in_flight.set(false);
                match result {
                    Ok((Ok(devs), mtp)) => {
                        banner.set_visible(false);
                        // Merge MTP devices, then re-sort by label.
                        let mut devs = devs;
                        // Mounted optical data discs belong to Disc Drives, not
                        // the removable-Devices list — drop them here.
                        devs.retain(|d| !is_optical_fs(&d.fs_type));
                        devs.extend(mtp);
                        devs.sort_by(|a, b| {
                            a.label
                                .to_lowercase()
                                .cmp(&b.label.to_lowercase())
                                .then_with(|| a.mount_path.cmp(&b.mount_path))
                        });
                        let want: Vec<String> =
                            devs.iter().map(|d| format!("dev:{}", d.backend_id)).collect();
                        // Remove rows for devices that went away.
                        dev_sub_rows.borrow_mut().retain(|r| {
                            let keep = want.contains(&r.widget_name().to_string());
                            if !keep {
                                sidebar.remove(r);
                            }
                            keep
                        });
                        // Add rows for new devices; update free-space bars in
                        // place so selection isn't disturbed when unchanged.
                        let expanded = devices_expanded.get();
                        for d in &devs {
                            let name = format!("dev:{}", d.backend_id);
                            let used = if d.total_bytes > 0 {
                                1.0 - (d.free_bytes as f64 / d.total_bytes as f64)
                            } else {
                                0.0
                            };
                            let base = if d.label.is_empty() {
                                "Untitled device".to_string()
                            } else {
                                d.label.clone()
                            };
                            // Status glyphs: ⚠ unsupported fs, 🔒 read-only.
                            let label_text =
                                format!("{}{base}", device_glyph_prefix(d.read_only, &d.fs_type));
                            let existing = dev_sub_rows
                                .borrow()
                                .iter()
                                .find(|r| r.widget_name().as_str() == name)
                                .cloned();
                            match existing {
                                Some(row) => {
                                    if let Some(bx) =
                                        row.child().and_then(|c| c.downcast::<GtkBox>().ok())
                                    {
                                        // Keep the label current (e.g. an MTP
                                        // device whose friendly name resolved
                                        // after the first poll).
                                        if let Some(lbl) = bx
                                            .first_child()
                                            .and_then(|c| c.downcast::<Label>().ok())
                                        {
                                            lbl.set_text(&gtk_safe(&label_text));
                                        }
                                        if let Some(bar) = bx
                                            .last_child()
                                            .and_then(|c| c.downcast::<gtk4::LevelBar>().ok())
                                        {
                                            bar.set_value(used);
                                            set_levelbar_fullness(&bar, used);
                                        }
                                    }
                                }
                                None => {
                                    let bx = GtkBox::new(Orientation::Vertical, 2);
                                    bx.set_margin_start(24);
                                    bx.set_margin_end(8);
                                    bx.set_margin_top(4);
                                    bx.set_margin_bottom(4);
                                    let lbl = Label::builder()
                                        .label(&gtk_safe(&label_text))
                                        .halign(Align::Start)
                                        .xalign(0.0)
                                        .build();
                                    let bar = gtk4::LevelBar::new();
                                    bar.set_min_value(0.0);
                                    bar.set_max_value(1.0);
                                    bar.set_value(used);
                                    set_levelbar_fullness(&bar, used);
                                    bx.append(&lbl);
                                    bx.append(&bar);
                                    let row = ListBoxRow::new();
                                    row.set_widget_name(&name);
                                    row.set_child(Some(&bx));
                                    row.set_visible(expanded);
                                    if device_fs_unsupported(&d.fs_type) {
                                        row.set_tooltip_text(Some(UNSUPPORTED_FS_TOOLTIP));
                                    }
                                    sidebar.append(&row);
                                    dev_sub_rows.borrow_mut().push(row);
                                }
                            }
                        }
                        *current_devices.borrow_mut() = devs;
                    }
                    // udisks failed — MTP (if any) is hidden until it recovers.
                    Ok((Err(e), _mtp)) => {
                        for r in dev_sub_rows.borrow_mut().drain(..) {
                            sidebar.remove(&r);
                        }
                        current_devices.borrow_mut().clear();
                        use crate::devices::diagnostics::{self, Diagnosis};
                        let diag = diagnostics::classify(
                            diagnostics::has_udisks_grant(&diagnostics::read_flatpak_info()),
                            &diagnostics::read_distro_info(),
                            crate::devices::detect::classify_error(&e),
                        );
                        let msg = match diag {
                            Diagnosis::PermissionOff => {
                                "Can't access drives — Sparkamp needs permission to use the system \
                                 disk service. Enable org.freedesktop.UDisks2 under System Bus in \
                                 Flatseal, then Retry."
                            }
                            Diagnosis::NotInstalled => {
                                "Can't access drives — your system's disk service (udisks2) isn't \
                                 installed. Install it, then Retry."
                            }
                            Diagnosis::EjectUnavailable => {
                                "Couldn't reach the disk service. Retry, or manage the device \
                                 through your file browser."
                            }
                        };
                        banner_lbl.set_text(msg);
                        banner.set_visible(true);
                    }
                    Err(_) => {
                        // The worker thread panicked.
                        for r in dev_sub_rows.borrow_mut().drain(..) {
                            sidebar.remove(&r);
                        }
                        current_devices.borrow_mut().clear();
                        banner_lbl.set_text("Couldn't query the device service.");
                        banner.set_visible(true);
                    }
                }
                // Keep the overview list in sync with the latest results.
                rebuild_overview();
            });
        })
    };

    // Initial scan + 2 s poll (stops once the window — hence the sidebar — is gone).
    refresh_devices();
    {
        let refresh = refresh_devices.clone();
        let sidebar_weak = sidebar.downgrade();
        glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
            if sidebar_weak.upgrade().is_none() {
                return glib::ControlFlow::Break;
            }
            refresh();
            glib::ControlFlow::Continue
        });
    }
    {
        let refresh = refresh_devices.clone();
        dev_banner_retry.connect_clicked(move |_| refresh());
    }

    // ── Disc Drives: playlist adds, detail population, overview, poll ────────
    // Turn DiscTrackEntry values into active-playlist rows, honoring the same
    // add-behavior + autoplay rules as the ML double-click path. Phase 1 has no
    // gnudb tags yet, so titles are "Track N" and artist/album stay empty (the
    // " / " sampler split still applies to future matched discs).
    let add_disc_entries: Rc<dyn Fn(&[crate::disc::DiscTrackEntry])> = {
        let state = state.clone();
        let rebuild = rebuild_playlist.clone();
        let disc_tags = disc_tags.clone();
        let selected_disc_id = selected_disc_id.clone();
        let current_drives = current_drives.clone();
        Rc::new(move |entries: &[crate::disc::DiscTrackEntry]| {
            if entries.is_empty() {
                return;
            }
            use crate::config::PlaylistAddBehavior;
            let behavior = state.borrow().config.behavior.playlist_add_behavior.clone();
            let autoplay = state.borrow().config.behavior.autoplay_on_add;
            // Disc-level artist/album for the currently shown drive (empty until
            // identified/edited); used for the non-sampler title case.
            let (disc_artist, disc_album) =
                selected_disc_discid(&selected_disc_id, &current_drives)
                    .and_then(|(_, id)| {
                        disc_tags
                            .borrow()
                            .get(&id)
                            .map(|t| (t.artist.clone(), t.album.clone()))
                    })
                    .unwrap_or_default();
            if behavior == PlaylistAddBehavior::Replace {
                let _ = state.borrow_mut().player.stop();
                let mut s = state.borrow_mut();
                s.playlist.tracks.clear();
                s.playlist.current_index = 0;
                s.last_duration = None;
                s.pending_seek = None;
                s.mute_pending = None;
            }
            let insert_start = state.borrow().playlist.len();
            for e in entries {
                // Sampler discs put the per-track artist in the title.
                let meta = crate::disc::track_meta(&e.title, &disc_artist);
                state.borrow_mut().playlist.tracks.push(crate::model::Track {
                    path: std::path::PathBuf::from(&e.path),
                    title: meta.title,
                    artist: meta.artist,
                    album_artist: String::new(),
                    album: disc_album.clone(),
                    duration: Some(std::time::Duration::from_secs(e.duration_secs as u64)),
                    broken: false,
                    read_only: true, // disc media is never writable in place
                });
            }
            rebuild();
            if autoplay && (behavior == PlaylistAddBehavior::Replace || insert_start == 0) {
                state.borrow_mut().playlist.jump_to(insert_start);
                state.borrow_mut().play_current();
            }
        })
    };

    // Fill the drive detail view for one drive: header, media state, and either
    // the audio-track list or a banner for no-disc/blank/data media.
    let populate_disc_detail: Rc<dyn Fn(&crate::disc::OpticalDrive)> = {
        let title = disc_title.clone();
        let media_lbl = disc_media_lbl.clone();
        let tag_lbl = disc_tag_lbl.clone();
        let banner = disc_banner.clone();
        let track_list = disc_track_list.clone();
        let tracks_scroll = disc_tracks_scroll.clone();
        let actions = disc_actions.clone();
        // Audio-only actions hide on non-audio media; Eject shows whenever a
        // disc is present (mac parity).
        let audio_btns = [
            disc_add_sel.clone(),
            disc_add_all.clone(),
            disc_identify.clone(),
            disc_rip.clone(),
            disc_edit_tags.clone(),
        ];
        let eject_btn = disc_eject.clone();
        let submit_btn = disc_submit.clone();
        let entries_store = current_disc_entries.clone();
        let disc_tags = disc_tags.clone();
        let disc_official = disc_official.clone();
        let search_row = disc_search_row.clone();
        let search_entry = disc_search_entry.clone();
        // Which drive the detail last showed — a switch clears the search
        // (the 10 s poll repopulates the SAME drive and must not).
        let last_drive: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        Rc::new(move |drive: &crate::disc::OpticalDrive| {
            if last_drive.borrow().as_deref() != Some(drive.id.as_str()) {
                *last_drive.borrow_mut() = Some(drive.id.clone());
                search_entry.set_text("");
            }
            title.set_text(&gtk_safe(&drive.label));
            media_lbl.set_text(&drive.media_summary());
            while let Some(child) = track_list.first_child() {
                track_list.remove(&child);
            }
            let mut entries = crate::disc::toc::track_entries(drive);
            // Overlay stored gnudb/edited titles + surface "Artist — Album".
            let discid = drive.toc.as_ref().map(crate::disc::discid::freedb_discid);
            let mut header: Option<String> = None;
            if let Some(id) = &discid {
                if let Some(tags) = disc_tags.borrow().get(id) {
                    for e in &mut entries {
                        if let Some(t) = tags.track_titles.get(e.number as usize - 1) {
                            if !t.is_empty() {
                                e.title = t.clone();
                            }
                        }
                    }
                    if !tags.artist.is_empty() || !tags.album.is_empty() {
                        header = Some(format!("{} — {}", tags.artist, tags.album));
                    }
                }
            }
            match &header {
                Some(h) => {
                    tag_lbl.set_text(&gtk_safe(h));
                    tag_lbl.set_visible(true);
                }
                None => tag_lbl.set_visible(false),
            }
            if drive.media.is_audio_cd && !entries.is_empty() {
                banner.set_visible(false);
                search_row.set_visible(true);
                tracks_scroll.set_visible(true);
                actions.set_visible(true);
                for b in &audio_btns {
                    b.set_visible(true);
                }
                eject_btn.set_visible(true);
                // Submit only makes sense with something to send: the disc is
                // unknown to gnudb, or the tags differ from the official match.
                submit_btn.set_visible(discid.as_ref().is_some_and(|id| {
                    disc::disc_submittable(id, &disc_tags.borrow(), &disc_official.borrow())
                }));
                for e in &entries {
                    let (m, s) = (e.duration_secs / 60, e.duration_secs % 60);
                    // Show the real title once known; otherwise the placeholder.
                    let disp = if e.title == format!("Track {}", e.number) {
                        format!("Track {} — {}:{:02}", e.number, m, s)
                    } else {
                        format!("{}. {} — {}:{:02}", e.number, e.title.replace(" / ", " - "), m, s)
                    };
                    let row_lbl = Label::builder()
                        .label(&gtk_safe(&disp))
                        .halign(Align::Start)
                        .xalign(0.0)
                        .margin_start(8)
                        .margin_end(8)
                        .margin_top(4)
                        .margin_bottom(4)
                        .build();
                    let row = ListBoxRow::new();
                    row.set_child(Some(&row_lbl));
                    track_list.append(&row);
                }
            } else {
                search_row.set_visible(false);
                tracks_scroll.set_visible(false);
                // A loaded non-audio disc still gets Eject; the audio actions
                // make no sense for it.
                actions.set_visible(drive.media.present);
                for b in &audio_btns {
                    b.set_visible(false);
                }
                submit_btn.set_visible(false);
                eject_btn.set_visible(drive.media.present);
                tag_lbl.set_visible(false);
                let msg = if !drive.media.present {
                    "No disc in the drive. Insert an audio CD to play its tracks."
                } else if drive.media.is_blank {
                    "Blank disc. Burning arrives in a later phase."
                } else {
                    "Data disc — no audio tracks to play."
                };
                banner.set_text(msg);
                banner.set_visible(true);
            }
            *entries_store.borrow_mut() = entries;
            // Fresh rows + fresh entries: re-run the search filter over them.
            track_list.invalidate_filter();
        })
    };

    // Store a disc's tags (user set + optional official baseline), persist to
    // the shared store, refresh the detail if it's showing that disc, and push
    // the new titles/artist/album into already-added playlist rows.
    #[allow(clippy::type_complexity)]
    let commit_disc_tags: Rc<
        dyn Fn(String, crate::disc::xmcd::XmcdEntry, Option<crate::disc::xmcd::XmcdEntry>),
    > = {
        let disc_tags = disc_tags.clone();
        let disc_official = disc_official.clone();
        let state = state.clone();
        let selected_disc_id = selected_disc_id.clone();
        let current_drives = current_drives.clone();
        let populate = populate_disc_detail.clone();
        let entries_store = current_disc_entries.clone();
        let rebuild = rebuild_playlist.clone();
        Rc::new(move |discid: String, user: crate::disc::xmcd::XmcdEntry, official| {
            disc_tags.borrow_mut().insert(discid.clone(), user.clone());
            if let Some(o) = official {
                disc_official.borrow_mut().insert(discid.clone(), o);
            }
            // Persist (user set + the untouched official baseline for submit).
            {
                let mut store = crate::disc::tagstore::DiscTagStore::load();
                let off = disc_official.borrow().get(&discid).cloned();
                store.set(&discid, user, off);
                store.save();
            }
            // Only refresh/propagate when the committed disc is on screen.
            let showing = selected_disc_discid(&selected_disc_id, &current_drives)
                .map(|(_, id)| id == discid)
                .unwrap_or(false);
            if !showing {
                return;
            }
            if let Some(id) = selected_disc_id.borrow().clone() {
                if let Some(drive) = current_drives.borrow().iter().find(|d| d.id == id).cloned() {
                    populate(&drive);
                }
            }
            // Path-keyed propagation to already-added playlist rows, using the
            // same sampler " / " split as add_disc_entries.
            let (disc_artist, disc_album) = disc_tags
                .borrow()
                .get(&discid)
                .map(|t| (t.artist.clone(), t.album.clone()))
                .unwrap_or_default();
            let updates: Vec<(String, String, String)> = entries_store
                .borrow()
                .iter()
                .map(|e| {
                    let meta = crate::disc::track_meta(&e.title, &disc_artist);
                    (e.path.clone(), meta.title, meta.artist)
                })
                .collect();
            {
                let mut s = state.borrow_mut();
                for track in &mut s.playlist.tracks {
                    let tp = track.path.display().to_string();
                    if let Some((_, title, artist)) = updates.iter().find(|(p, _, _)| *p == tp) {
                        track.title = title.clone();
                        track.artist = artist.clone();
                        track.album = disc_album.clone();
                    }
                }
            }
            rebuild();
        })
    };

    // Overview cards (one per drive); clicking a card opens that drive's detail.
    let rebuild_disc_overview: Rc<dyn Fn()> = {
        let drives = current_drives.clone();
        let list = disc_overview_list.clone();
        let sidebar_ov = sidebar.clone();
        let detecting = disc_detecting.clone();
        Rc::new(move || {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let ds = drives.borrow();
            if ds.is_empty() {
                if detecting.get() {
                    // Still running the first poll: show a working indicator.
                    let row = GtkBox::new(Orientation::Horizontal, 8);
                    let spinner = gtk4::Spinner::new();
                    spinner.start();
                    let lbl = Label::builder()
                        .label("Detecting disc drives…")
                        .halign(Align::Start)
                        .xalign(0.0)
                        .build();
                    lbl.add_css_class("dim-label");
                    row.append(&spinner);
                    row.append(&lbl);
                    list.append(&row);
                } else {
                    let empty = Label::builder()
                        .label("No disc drives connected")
                        .halign(Align::Start)
                        .xalign(0.0)
                        .build();
                    empty.add_css_class("dim-label");
                    list.append(&empty);
                }
                return;
            }
            for d in ds.iter() {
                let card = GtkBox::new(Orientation::Vertical, 4);
                card.set_margin_top(4);
                card.set_margin_bottom(4);
                let name = Label::builder()
                    .label(&gtk_safe(&d.label))
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                let state_lbl = Label::builder()
                    .label(&d.media_summary())
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                state_lbl.add_css_class("dim-label");
                card.append(&name);
                card.append(&state_lbl);
                if let Some(detail) = disc_overview_detail_line(d) {
                    let dl = Label::builder()
                        .label(&detail)
                        .halign(Align::Start)
                        .xalign(0.0)
                        .build();
                    dl.add_css_class("dim-label");
                    card.append(&dl);
                }
                let gesture = GestureClick::new();
                let sidebar_c = sidebar_ov.clone();
                let target = format!("disc:{}", d.id);
                gesture.connect_released(move |_, _, _, _| {
                    if let Some(r) = find_row_by_name(&sidebar_c, &target) {
                        sidebar_c.select_row(Some(&r));
                    }
                });
                card.add_controller(gesture);
                list.append(&card);
            }
        })
    };

    // Poll every optical drive off the UI thread (detection shells out to
    // cd-info). Diff the sidebar rows in place, keeping selection stable.
    let refresh_discs: Rc<dyn Fn()> = {
        let sidebar = sidebar.clone();
        let disc_sub_rows = disc_sub_rows.clone();
        let discs_expanded = discs_expanded.clone();
        let current_drives = current_drives.clone();
        let selected_disc_id = selected_disc_id.clone();
        let rebuild_overview = rebuild_disc_overview.clone();
        let populate_detail = populate_disc_detail.clone();
        let state = state.clone();
        let disc_detecting = disc_detecting.clone();
        let disc_detect_spinner = disc_detect_spinner.clone();
        let rip_active = rip_active.clone();
        let in_flight = Rc::new(Cell::new(false));
        Rc::new(move || {
            if in_flight.get() {
                return;
            }
            // Never run cd-info on a drive we're actively reading from — cdiocddasrc
            // (playback OR a rip) seeks the same head, and the device only allows
            // one reader, so a concurrent cd-info thrashes it. Skip while a cdda://
            // track plays or a rip is in progress; polling resumes afterwards.
            {
                let s = state.borrow();
                let playing_disc = !matches!(s.player.state(), PlayerState::Stopped)
                    && s
                        .playlist
                        .current()
                        .map(|t| t.path.to_string_lossy().starts_with("cdda://"))
                        .unwrap_or(false);
                if playing_disc || rip_active.get() {
                    // Not detecting right now — clear any spinner a show/map set.
                    disc_detect_spinner.stop();
                    disc_detect_spinner.set_visible(false);
                    return;
                }
            }
            in_flight.set(true);
            let sidebar = sidebar.clone();
            let disc_sub_rows = disc_sub_rows.clone();
            let discs_expanded = discs_expanded.clone();
            let current_drives = current_drives.clone();
            let selected_disc_id = selected_disc_id.clone();
            let rebuild_overview = rebuild_overview.clone();
            let populate_detail = populate_detail.clone();
            let disc_detecting = disc_detecting.clone();
            let disc_detect_spinner = disc_detect_spinner.clone();
            let in_flight = in_flight.clone();
            glib::spawn_future_local(async move {
                // Cached poll: an unchanged loaded disc is answered by the
                // kernel status ioctl and NOT re-probed — the full cd-info
                // probe spins the drive, and a 10 s poll doing that keeps
                // the disc spinning forever.
                let prev = current_drives.borrow().clone();
                let result = gio::spawn_blocking(move || {
                    crate::disc::detect::list_drives_cached(&prev)
                })
                .await;
                in_flight.set(false);
                // First poll finished — drop the "Detecting…" hint + sidebar
                // spinner and show the real state.
                disc_detecting.set(false);
                disc_detect_spinner.stop();
                disc_detect_spinner.set_visible(false);
                let Ok(drives) = result else { return };
                let want: Vec<String> =
                    drives.iter().map(|d| format!("disc:{}", d.id)).collect();
                // Remove rows for drives that went away.
                disc_sub_rows.borrow_mut().retain(|r| {
                    let keep = want.contains(&r.widget_name().to_string());
                    if !keep {
                        sidebar.remove(r);
                    }
                    keep
                });
                let expanded = discs_expanded.get();
                for d in &drives {
                    let name = format!("disc:{}", d.id);
                    let label_text = if d.label.is_empty() {
                        d.id.clone()
                    } else {
                        d.label.clone()
                    };
                    let summary = d.media_summary();
                    let existing = disc_sub_rows
                        .borrow()
                        .iter()
                        .find(|r| r.widget_name().as_str() == name)
                        .cloned();
                    match existing {
                        Some(row) => {
                            // Keep the media-state line current (disc in/out).
                            if let Some(bx) =
                                row.child().and_then(|c| c.downcast::<GtkBox>().ok())
                            {
                                if let Some(lbl) =
                                    bx.last_child().and_then(|c| c.downcast::<Label>().ok())
                                {
                                    lbl.set_text(&summary);
                                }
                            }
                        }
                        None => {
                            let bx = GtkBox::new(Orientation::Vertical, 2);
                            bx.set_margin_start(24);
                            bx.set_margin_end(8);
                            bx.set_margin_top(4);
                            bx.set_margin_bottom(4);
                            let lbl = Label::builder()
                                .label(&gtk_safe(&label_text))
                                .halign(Align::Start)
                                .xalign(0.0)
                                .build();
                            let state_lbl = Label::builder()
                                .label(&summary)
                                .halign(Align::Start)
                                .xalign(0.0)
                                .build();
                            state_lbl.add_css_class("dim-label");
                            bx.append(&lbl);
                            bx.append(&state_lbl);
                            let row = ListBoxRow::new();
                            row.set_widget_name(&name);
                            row.set_child(Some(&bx));
                            row.set_visible(expanded);
                            // Insert between the Disc Drives and Devices headers
                            // so disc rows stay grouped above the device rows.
                            let at = find_row_by_name(&sidebar, "devices")
                                .map(|r| r.index())
                                .unwrap_or(-1);
                            sidebar.insert(&row, at);
                            disc_sub_rows.borrow_mut().push(row);
                        }
                    }
                }
                // Unplug fallback: if the drive being viewed disappeared, return
                // to the discs overview.
                if let Some(sel) = selected_disc_id.borrow().clone() {
                    if !drives.iter().any(|d| d.id == sel) {
                        if let Some(r) = find_row_by_name(&sidebar, "discs") {
                            sidebar.select_row(Some(&r));
                        }
                    }
                }
                // If the drive being viewed changed state (disc ejected,
                // inserted, or swapped), repopulate the open detail view —
                // otherwise it keeps showing the previous disc's tracks.
                // Unchanged drives skip this so the 10 s poll never disturbs
                // the user's row selection.
                let detail_update: Option<crate::disc::OpticalDrive> = selected_disc_id
                    .borrow()
                    .clone()
                    .and_then(|sel| {
                        let new_d = drives.iter().find(|d| d.id == sel).cloned()?;
                        let old_d = current_drives
                            .borrow()
                            .iter()
                            .find(|d| d.id == sel)
                            .cloned();
                        (old_d.as_ref() != Some(&new_d)).then_some(new_d)
                    });
                *current_drives.borrow_mut() = drives;
                rebuild_overview();
                if let Some(d) = detail_update {
                    populate_detail(&d);
                }
            });
        })
    };

    // Selecting a drive (or the Disc Drives header) shows the discs page.
    {
        let stack_ref = stack.clone();
        let drives = current_drives.clone();
        let overview = disc_overview.clone();
        let detail = disc_detail.clone();
        let populate = populate_disc_detail.clone();
        let rebuild_overview = rebuild_disc_overview.clone();
        let sel_id = selected_disc_id.clone();
        let exp = discs_expanded.clone();
        sidebar.connect_row_selected(move |_, opt_row| {
            let Some(row) = opt_row else { return };
            let name = row.widget_name().to_string();
            if name == "discs" {
                stack_ref.set_visible_child_name("discs");
                rebuild_overview();
                overview.set_visible(true);
                detail.set_visible(false);
                *sel_id.borrow_mut() = None;
                if !exp.get() {
                    exp.set(true);
                }
            } else if let Some(id) = name.strip_prefix("disc:") {
                stack_ref.set_visible_child_name("discs");
                if let Some(d) = drives.borrow().iter().find(|d| d.id == id) {
                    overview.set_visible(false);
                    detail.set_visible(true);
                    populate(d);
                    *sel_id.borrow_mut() = Some(id.to_string());
                }
            }
        });
    }

    // Add actions: selected tracks, all tracks, or a double-clicked row.
    {
        let entries = current_disc_entries.clone();
        let track_list = disc_track_list.clone();
        let add = add_disc_entries.clone();
        disc_add_sel.connect_clicked(move |_| {
            let sel = track_list.selected_rows();
            if sel.is_empty() {
                return;
            }
            let all = entries.borrow();
            let picked: Vec<crate::disc::DiscTrackEntry> = sel
                .iter()
                .filter_map(|r| all.get(r.index() as usize).cloned())
                .collect();
            add(&picked);
        });
    }
    {
        let entries = current_disc_entries.clone();
        let add = add_disc_entries.clone();
        disc_add_all.connect_clicked(move |_| {
            let all = entries.borrow().clone();
            add(&all);
        });
    }
    {
        let entries = current_disc_entries.clone();
        let add = add_disc_entries.clone();
        disc_track_list.connect_row_activated(move |_, row| {
            if let Some(e) = entries.borrow().get(row.index() as usize).cloned() {
                add(&[e]);
            }
        });
    }

    // ── gnudb identify + tag override (Phase 2) ─────────────────────────────
    // Fetch one chosen match in the background, parse its xmcd, and commit it as
    // both the user tags and the official (submission-baseline) copy.
    let apply_disc_match: Rc<dyn Fn(String, String, String)> = {
        let state = state.clone();
        let commit = commit_disc_tags.clone();
        let status = disc_status_lbl.clone();
        Rc::new(move |discid: String, category: String, matched_id: String| {
            let email = state.borrow().config.disc.gnudb_email.clone();
            status.set_text("Fetching entry…");
            let commit = commit.clone();
            let status = status.clone();
            glib::spawn_future_local(async move {
                let res = gio::spawn_blocking(move || {
                    match crate::disc::gnudb::read(&category, &matched_id, &email) {
                        Ok(text) => crate::disc::xmcd::parse(&text)
                            .ok_or_else(|| "gnudb entry was unreadable".to_string()),
                        Err(e) => Err(e.to_string()),
                    }
                })
                .await;
                match res {
                    Ok(Ok(entry)) => {
                        let label = format!("{} — {}", entry.artist, entry.album);
                        commit(discid, entry.clone(), Some(entry));
                        status.set_text(&gtk_safe(&label));
                    }
                    Ok(Err(msg)) => status.set_text(&gtk_safe(&msg)),
                    Err(_) => status.set_text("gnudb lookup failed"),
                }
            });
        })
    };

    // Modal picker for an inexact/multi-candidate match list.
    let open_match_picker: Rc<dyn Fn(String, Vec<crate::disc::gnudb::DiscMatch>)> = {
        let apply = apply_disc_match.clone();
        let win_wk = win.downgrade();
        Rc::new(move |discid: String, matches: Vec<crate::disc::gnudb::DiscMatch>| {
            let dialog = gtk4::Window::builder()
                .title("Choose a gnudb match")
                .modal(true)
                .default_width(440)
                .default_height(320)
                .build();
            if let Some(w) = win_wk.upgrade() {
                dialog.set_transient_for(Some(&w));
            }
            let vbox = GtkBox::new(Orientation::Vertical, 8);
            vbox.set_margin_top(12);
            vbox.set_margin_bottom(12);
            vbox.set_margin_start(12);
            vbox.set_margin_end(12);
            let list = gtk4::ListBox::new();
            list.set_selection_mode(gtk4::SelectionMode::Single);
            for m in &matches {
                let text = format!("{}{}", m.title, if m.exact { "  (exact)" } else { "" });
                let lbl = Label::builder()
                    .label(&gtk_safe(&text))
                    .halign(Align::Start)
                    .xalign(0.0)
                    .margin_start(6)
                    .margin_end(6)
                    .margin_top(4)
                    .margin_bottom(4)
                    .build();
                let row = ListBoxRow::new();
                row.set_child(Some(&lbl));
                list.append(&row);
            }
            list.select_row(list.row_at_index(0).as_ref());
            let scroll = ScrolledWindow::builder().vexpand(true).child(&list).build();
            vbox.append(&scroll);
            let btns = GtkBox::new(Orientation::Horizontal, 6);
            btns.set_halign(Align::End);
            let cancel = Button::with_label("Cancel");
            let ok = Button::with_label("Use This");
            ok.add_css_class("suggested-action");
            btns.append(&cancel);
            btns.append(&ok);
            vbox.append(&btns);
            dialog.set_child(Some(&vbox));
            let d = dialog.clone();
            cancel.connect_clicked(move |_| d.close());
            let d = dialog.clone();
            let apply = apply.clone();
            ok.connect_clicked(move |_| {
                let idx = list.selected_row().map(|r| r.index()).unwrap_or(-1);
                if idx >= 0 {
                    if let Some(m) = matches.get(idx as usize) {
                        apply(discid.clone(), m.category.clone(), m.discid.clone());
                    }
                }
                d.close();
            });
            dialog.present();
        })
    };

    // The actual gnudb query, factored out so the email prompt can retry it.
    // Single exact match auto-applies; several open the picker; none points the
    // user at Edit Tags. Never blocks the UI.
    let run_identify: Rc<dyn Fn()> = {
        let selected_disc_id = selected_disc_id.clone();
        let current_drives = current_drives.clone();
        let state = state.clone();
        let status = disc_status_lbl.clone();
        let apply = apply_disc_match.clone();
        let picker = open_match_picker.clone();
        let identify_btn = disc_identify.clone();
        Rc::new(move || {
            let Some((toc, discid)) = selected_disc_discid(&selected_disc_id, &current_drives)
            else {
                status.set_text("No audio disc to identify");
                return;
            };
            let email = state.borrow().config.disc.gnudb_email.clone();
            status.set_text("Asking gnudb…");
            identify_btn.set_sensitive(false);
            let status = status.clone();
            let apply = apply.clone();
            let picker = picker.clone();
            let identify_btn2 = identify_btn.clone();
            glib::spawn_future_local(async move {
                let res =
                    gio::spawn_blocking(move || crate::disc::gnudb::query(&toc, &email)).await;
                identify_btn2.set_sensitive(true);
                match res {
                    Ok(Ok(matches)) if matches.is_empty() => {
                        status.set_text("No gnudb match — use Edit Tags to fill them in.");
                    }
                    Ok(Ok(matches)) if matches.len() == 1 && matches[0].exact => {
                        let m = &matches[0];
                        apply(discid, m.category.clone(), m.discid.clone());
                    }
                    Ok(Ok(matches)) => picker(discid, matches),
                    Ok(Err(e)) => status.set_text(&gtk_safe(&e.to_string())),
                    Err(_) => status.set_text("gnudb lookup failed"),
                }
            });
        })
    };

    // Identify button: gnudb needs an email for its handshake, so collect one
    // (stored in Settings) before the first lookup when it's unset.
    {
        let state = state.clone();
        let status = disc_status_lbl.clone();
        let selected_disc_id = selected_disc_id.clone();
        let current_drives = current_drives.clone();
        let run_identify = run_identify.clone();
        let win_wk = win.downgrade();
        disc_identify.connect_clicked(move |_| {
            if selected_disc_discid(&selected_disc_id, &current_drives).is_none() {
                status.set_text("No audio disc to identify");
                return;
            }
            let email = state.borrow().config.disc.gnudb_email.clone();
            if crate::disc::gnudb::is_unset_email(&email) {
                // Prompt, store, then run the lookup with the entered address.
                prompt_gnudb_email(
                    win_wk.upgrade().as_ref(),
                    state.clone(),
                    run_identify.clone(),
                );
            } else {
                run_identify();
            }
        });
    }

    // Edit Tags: modal editor for disc fields + per-track titles, editable with
    // or without a match. Save commits, persists, overlays, and propagates.
    {
        let selected_disc_id = selected_disc_id.clone();
        let current_drives = current_drives.clone();
        let disc_tags = disc_tags.clone();
        let entries_store = current_disc_entries.clone();
        let commit = commit_disc_tags.clone();
        let status = disc_status_lbl.clone();
        let win_wk = win.downgrade();
        disc_edit_tags.connect_clicked(move |_| {
            let Some((_, discid)) = selected_disc_discid(&selected_disc_id, &current_drives) else {
                status.set_text("No audio disc loaded");
                return;
            };
            let stored = disc_tags.borrow().get(&discid).cloned();
            let entries = entries_store.borrow().clone();
            let dialog = gtk4::Window::builder()
                .title("Edit Disc Tags")
                .modal(true)
                .default_width(460)
                .default_height(500)
                .build();
            if let Some(w) = win_wk.upgrade() {
                dialog.set_transient_for(Some(&w));
            }
            let outer = GtkBox::new(Orientation::Vertical, 8);
            outer.set_margin_top(12);
            outer.set_margin_bottom(12);
            outer.set_margin_start(12);
            outer.set_margin_end(12);
            let mk_field = |label: &str, val: &str| -> (GtkBox, Entry) {
                let row = GtkBox::new(Orientation::Horizontal, 8);
                let l = Label::builder()
                    .label(label)
                    .width_chars(7)
                    .halign(Align::Start)
                    .xalign(0.0)
                    .build();
                let e = Entry::new();
                e.set_hexpand(true);
                e.set_text(&gtk_safe(val));
                row.append(&l);
                row.append(&e);
                (row, e)
            };
            let (artist_row, artist_e) =
                mk_field("Artist", stored.as_ref().map(|s| s.artist.as_str()).unwrap_or(""));
            let (album_row, album_e) =
                mk_field("Album", stored.as_ref().map(|s| s.album.as_str()).unwrap_or(""));
            let (year_row, year_e) =
                mk_field("Year", stored.as_ref().map(|s| s.year.as_str()).unwrap_or(""));
            let (genre_row, genre_e) =
                mk_field("Genre", stored.as_ref().map(|s| s.genre.as_str()).unwrap_or(""));
            outer.append(&artist_row);
            outer.append(&album_row);
            outer.append(&year_row);
            outer.append(&genre_row);
            let sep = Label::builder()
                .label("Track titles (use \"Artist / Title\" for compilations)")
                .halign(Align::Start)
                .xalign(0.0)
                .build();
            sep.add_css_class("dim-label");
            outer.append(&sep);
            let title_box = GtkBox::new(Orientation::Vertical, 4);
            let mut title_entries: Vec<Entry> = Vec::new();
            for e in &entries {
                let idx = e.number as usize - 1;
                let init = stored
                    .as_ref()
                    .and_then(|s| s.track_titles.get(idx).cloned())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| {
                        if e.title == format!("Track {}", e.number) {
                            String::new()
                        } else {
                            e.title.clone()
                        }
                    });
                let row = GtkBox::new(Orientation::Horizontal, 8);
                let l = Label::builder()
                    .label(&format!("{}.", e.number))
                    .width_chars(3)
                    .halign(Align::Start)
                    .build();
                let ent = Entry::new();
                ent.set_hexpand(true);
                ent.set_text(&gtk_safe(&init));
                row.append(&l);
                row.append(&ent);
                title_box.append(&row);
                title_entries.push(ent);
            }
            let scroll = ScrolledWindow::builder().vexpand(true).child(&title_box).build();
            outer.append(&scroll);
            let btns = GtkBox::new(Orientation::Horizontal, 6);
            btns.set_halign(Align::End);
            let cancel = Button::with_label("Cancel");
            let save = Button::with_label("Save");
            save.add_css_class("suggested-action");
            btns.append(&cancel);
            btns.append(&save);
            outer.append(&btns);
            dialog.set_child(Some(&outer));
            let d = dialog.clone();
            cancel.connect_clicked(move |_| d.close());
            let d = dialog.clone();
            let commit = commit.clone();
            save.connect_clicked(move |_| {
                // Base on the stored entry so extd/extt/revision survive edits.
                let mut entry = stored.clone().unwrap_or_default();
                entry.discid = discid.clone();
                entry.artist = artist_e.text().to_string();
                entry.album = album_e.text().to_string();
                entry.year = year_e.text().to_string();
                entry.genre = genre_e.text().to_string();
                entry.track_titles =
                    title_entries.iter().map(|e| e.text().to_string()).collect();
                commit(discid.clone(), entry, None);
                d.close();
            });
            dialog.present();
        });
    }

    // ── Rip to MP3 (Phase 3) ────────────────────────────────────────────────
    // Dialog + worker live in the `disc` module; this wires the buttons to
    // the shared state and the progress widgets on the drive detail view.
    disc::connect_rip_ui(
        disc::DiscRipUi {
            state: state.clone(),
            rip_cancel: rip_cancel.clone(),
            rip_active: rip_active.clone(),
            rip_box: disc_rip_box.clone(),
            rip_bar: disc_rip_bar.clone(),
            status: disc_status_lbl.clone(),
        },
        &disc_rip,
        &disc_rip_cancel,
        &win,
        current_disc_entries.clone(),
        disc_tags.clone(),
        selected_disc_id.clone(),
        current_drives.clone(),
    );

    // Submit to gnudb (Phase 4): category picker + background POST; the
    // button's visibility (unknown disc / tags differ from the official
    // match) is maintained by populate_disc_detail.
    disc::connect_submit(
        &disc_submit,
        state.clone(),
        disc_status_lbl.clone(),
        &win,
        disc_tags.clone(),
        disc_official.clone(),
        selected_disc_id.clone(),
        current_drives.clone(),
    );

    // Eject: blocking subprocess off the UI thread, then re-poll the drives.
    disc::connect_eject(
        &disc_eject,
        state.clone(),
        rip_active.clone(),
        disc_status_lbl.clone(),
        selected_disc_id.clone(),
        refresh_discs.clone(),
    );

    // Initial scan + lazy 10 s poll (stops once the window/sidebar is gone).
    refresh_discs();
    {
        let refresh = refresh_discs.clone();
        let sidebar_weak = sidebar.downgrade();
        glib::timeout_add_local(std::time::Duration::from_secs(10), move || {
            if sidebar_weak.upgrade().is_none() {
                return glib::ControlFlow::Break;
            }
            refresh();
            glib::ControlFlow::Continue
        });
    }
    // Re-detect every time the window is shown (this ML window uses
    // hide-on-close, so it's reused across opens). Spinning the header spinner
    // here means the "detecting…" indicator is actually visible when the user
    // opens the Media Library, not only during the one-off build at startup.
    {
        let refresh = refresh_discs.clone();
        let spinner = disc_detect_spinner.clone();
        win.connect_map(move |_| {
            spinner.set_visible(true);
            spinner.start();
            refresh();
        });
    }

    // Selecting a device (or the Devices header) shows the devices page.
    {
        let stack_ref = stack.clone();
        let current = current_devices.clone();
        let title = dev_title.clone();
        let capacity = dev_capacity.clone();
        let levelbar = dev_levelbar.clone();
        let eject = dev_eject.clone();
        let sel_backend = selected_dev_backend.clone();
        let exp = devices_expanded.clone();
        let path_lbl = dev_path.clone();
        let overview = dev_overview.clone();
        let detail = dev_detail.clone();
        let warn = dev_warn.clone();
        let ro_badge = dev_ro_badge.clone();
        let warn_badge = dev_warn_badge.clone();
        let transfers_sel = device_transfers.clone();
        let rebuild_overview_sel = rebuild_overview.clone();
        let reload_dev_playlists_sel = reload_dev_playlists.clone();
        let reload_device_store_sel = reload_device_store.clone();
        let dev_named_cols_sel = dev_named_cols.clone();
        let dev_col_view_sel = dev_col_view.clone();
        let state_devcols = state.clone();
        let sync_btn = dev_sync.clone();
        let scan_btn = dev_scan.clone();
        // Sections hidden behind the "no filesystem" banner.
        let nofs_banner = dev_nofs_banner.clone();
        let nofs_lbl_sel = dev_nofs_lbl.clone();
        let pl_header_sel = dev_pl_header.clone();
        let pl_scroll_sel = dev_pl_scroll.clone();
        let pl_actions_sel = dev_pl_actions.clone();
        let tracks_scroll_sel = dev_tracks_scroll.clone();
        let file_actions_sel = dev_file_actions.clone();
        let store_sel = dev_store.clone();
        let counts_sel = dev_counts.clone();
        sidebar.connect_row_selected(move |_, opt_row| {
            let Some(row) = opt_row else { return };
            let name = row.widget_name().to_string();
            if name == "devices" {
                // Overview mode: list every connected device.
                stack_ref.set_visible_child_name("devices");
                rebuild_overview_sel();
                overview.set_visible(true);
                detail.set_visible(false);
                *sel_backend.borrow_mut() = None;
                if !exp.get() {
                    exp.set(true);
                }
            } else if let Some(backend) = name.strip_prefix("dev:") {
                stack_ref.set_visible_child_name("devices");
                if let Some(d) = current.borrow().iter().find(|d| d.backend_id == backend) {
                    // Detail mode for the selected device.
                    overview.set_visible(false);
                    detail.set_visible(true);
                    // Re-apply the shared column config so device columns track
                    // changes made in the files view (same as the editor does).
                    apply_ml_columns_to(&dev_col_view_sel, &dev_named_cols_sel, &state_devcols, 1);
                    let base = if d.label.is_empty() {
                        "Untitled device".to_string()
                    } else {
                        d.label.clone()
                    };
                    // Name in the header; status shown as pill badges instead
                    // of inline glyphs.
                    title.set_text(&gtk_safe(&base));
                    path_lbl.set_text(&gtk_safe(&format!(
                        "{} · {}",
                        if d.fs_type.is_empty() { "unknown" } else { &d.fs_type },
                        d.mount_path.to_string_lossy(),
                    )));
                    ro_badge.set_visible(d.read_only);
                    let unsupported = device_fs_unsupported(&d.fs_type);
                    warn_badge.set_visible(unsupported);
                    let used_bytes = d.total_bytes.saturating_sub(d.free_bytes);
                    capacity.set_text(&format!(
                        "{:.1} GB used · {:.1} GB free · {:.1} GB total",
                        used_bytes as f64 / 1e9,
                        d.free_bytes as f64 / 1e9,
                        d.total_bytes as f64 / 1e9,
                    ));
                    if unsupported {
                        warn.set_text("⚠ NTFS/exFAT — limited support");
                        warn.set_tooltip_text(Some(UNSUPPORTED_FS_TOOLTIP));
                        warn.set_visible(true);
                    } else {
                        warn.set_visible(false);
                    }
                    let unsupported_dev =
                        d.backend == crate::devices::DeviceBackend::Unsupported;
                    let used = if d.total_bytes > 0 {
                        1.0 - d.free_bytes as f64 / d.total_bytes as f64
                    } else {
                        0.0
                    };
                    levelbar.set_value(used);
                    set_levelbar_fullness(&levelbar, used);
                    // No capacity is knowable for a photo/iOS mount — hide the bar.
                    levelbar.set_visible(!unsupported_dev);
                    // Eject is unavailable while a copy to this device is running.
                    let busy = transfers_sel.borrow().contains_key(&d.backend_id);
                    eject.set_sensitive(d.ejectable && !busy);
                    sync_btn.set_sensitive(true);
                    scan_btn.set_sensitive(true);
                    *sel_backend.borrow_mut() = Some(d.backend_id.clone());

                    if unsupported_dev {
                        // Apple iOS / PTP photo device: detected, but not a music
                        // sync target. Explain why and disable Sync/Scan. Eject
                        // stays available so the user can disconnect cleanly.
                        warn.set_visible(false);
                        capacity.set_text("Capacity unavailable");
                        nofs_lbl_sel.set_text(unsupported_device_banner(&d.backend_id));
                        nofs_banner.set_visible(true);
                        pl_header_sel.set_visible(false);
                        pl_scroll_sel.set_visible(false);
                        pl_actions_sel.set_visible(false);
                        tracks_scroll_sel.set_visible(false);
                        file_actions_sel.set_visible(false);
                        store_sel.remove_all();
                        counts_sel.set_text("Not a music-sync device");
                        sync_btn.set_sensitive(false);
                        scan_btn.set_sensitive(false);
                    } else if d.fs_visible {
                        // Normal device: show the lists, hide the banner.
                        nofs_banner.set_visible(false);
                        pl_header_sel.set_visible(true);
                        pl_scroll_sel.set_visible(true);
                        tracks_scroll_sel.set_visible(true);
                        file_actions_sel.set_visible(true);
                        sync_btn.set_sensitive(true);
                        scan_btn.set_sensitive(true);

                        // Rebuild the playlist filter rows ("All files" + each
                        // device .m3u/.m3u8); selecting "All files" resets the
                        // filter via the playlist-list handler.
                        reload_dev_playlists_sel(d.clone());

                        // Read device tags off the UI thread, then fill columns.
                        reload_device_store_sel(d.clone());
                    } else {
                        // Connected but no readable filesystem: show the banner
                        // in place of empty lists. Eject stays available so the
                        // user can disconnect; Sync/Scan are pointless here.
                        nofs_lbl_sel.set_text(
                            "⚠ No visible filesystem on this device. Set the phone to \
                             file-transfer mode and allow access, or reconnect it, then \
                             press Scan.",
                        );
                        nofs_banner.set_visible(true);
                        pl_header_sel.set_visible(false);
                        pl_scroll_sel.set_visible(false);
                        pl_actions_sel.set_visible(false);
                        tracks_scroll_sel.set_visible(false);
                        file_actions_sel.set_visible(false);
                        store_sel.remove_all();
                        counts_sel.set_text("No visible filesystem");
                        sync_btn.set_sensitive(false);
                        scan_btn.set_sensitive(false);
                    }
                }
            }
        });
    }

    // Scan: re-read tags + duration from the files on the selected device, and
    // refresh the playlist chips. Same work the device-select does, on demand.
    {
        let devices_scan = current_devices.clone();
        let sel_backend = selected_dev_backend.clone();
        let reload_store = reload_device_store.clone();
        let reload_pls = reload_dev_playlists.clone();
        dev_scan.connect_clicked(move |_| {
            let Some(backend) = sel_backend.borrow().clone() else { return };
            let dev = devices_scan
                .borrow()
                .iter()
                .find(|d| d.backend_id == backend)
                .cloned();
            let Some(dev) = dev else { return };
            reload_pls(dev.clone());
            reload_store(dev);
        });
    }

    // Eject: unmount + power off a device, then refresh the list. Shared by
    // the detail Eject button and each overview row's Eject button.
    let eject_run: Rc<dyn Fn(String)> = {
        let refresh = refresh_devices.clone();
        let sidebar_ej = sidebar.clone();
        let win_wk_ej = win.downgrade();
        Rc::new(move |backend: String| {
            let refresh = refresh.clone();
            let sidebar_ej = sidebar_ej.clone();
            let win_wk = win_wk_ej.clone();
            // MTP devices have no udisks2 block object — unmount through gvfs
            // (gio) on the main thread instead; the unmount itself is async.
            if backend.starts_with("mtp://") || backend.starts_with("gphoto2://") {
                // Forget cached metadata so a later replug of the same URI
                // re-reads the device rather than showing stale capacity.
                invalidate_mtp_meta(&backend);
                let monitor = gio::VolumeMonitor::get();
                let mount = monitor
                    .mounts()
                    .into_iter()
                    .find(|m| m.root().uri() == backend);
                let Some(mount) = mount else {
                    refresh();
                    return;
                };
                let refresh2 = refresh.clone();
                let sidebar2 = sidebar_ej.clone();
                let win2 = win_wk.clone();
                mount.unmount_with_operation(
                    gio::MountUnmountFlags::NONE,
                    None::<&gio::MountOperation>,
                    gio::Cancellable::NONE,
                    move |res| match res {
                        Ok(()) => {
                            refresh2();
                            if let Some(r) = find_row_by_name(&sidebar2, "devices") {
                                sidebar2.select_row(Some(&r));
                            }
                        }
                        Err(e) => {
                            show_alert_parented(
                                win2.upgrade().as_ref(),
                                &format!(
                                    "Couldn't disconnect the device ({e}). Close anything \
                                     using it and try again."
                                ),
                            );
                        }
                    },
                );
                return;
            }
            // Run the unmount/power-off on a worker thread so a busy device
            // can't freeze the UI.
            glib::spawn_future_local(async move {
                let res =
                    gio::spawn_blocking(move || crate::devices::detect::eject(&backend)).await;
                match res {
                    Ok(Ok(())) => {
                        refresh();
                        // The detail view may now show a device that's gone —
                        // return to the Devices overview.
                        if let Some(r) = find_row_by_name(&sidebar_ej, "devices") {
                            sidebar_ej.select_row(Some(&r));
                        }
                    }
                    Ok(Err(e)) => {
                        let dialog = gtk4::AlertDialog::builder()
                            .message("Couldn't eject")
                            .detail(format!(
                                "The device is still busy or couldn't be ejected ({e}). \
                                 Close anything using it and try again, or eject it from \
                                 your file browser."
                            ))
                            .modal(true)
                            .build();
                        dialog.show(win_wk.upgrade().as_ref());
                    }
                    Err(_) => {
                        show_alert_parented(
                            win_wk.upgrade().as_ref(),
                            "Eject failed unexpectedly.",
                        );
                    }
                }
            });
        })
    };
    *eject_run_holder.borrow_mut() = Some(eject_run.clone());
    {
        let sel_backend = selected_dev_backend.clone();
        let eject_run = eject_run.clone();
        dev_eject.connect_clicked(move |btn| {
            let Some(backend) = sel_backend.borrow().clone() else { return };
            btn.set_sensitive(false);
            eject_run(backend);
        });
    }

    // Sync: compare tags on each side of every pair, confirm en masse, apply.
    // Shared by the detail Sync button and each overview row's Sync button.
    let sync_run: Rc<dyn Fn(crate::devices::Device, Button)> = {
        let state_sync = state.clone();
        let win_wk = win.downgrade();
        let reload_sync = reload_device_store.clone();
        Rc::new(move |dev: crate::devices::Device, sync_btn: Button| {
            use crate::devices::sync::{PlaylistSyncDir, SyncAction};
            // Show activity while the device is read/planned (slow over MTP);
            // restored on every exit path below, just before a dialog/alert.
            set_button_busy(&sync_btn, true, "Sync");
            // Compute both sync plans on a worker thread — reading device tags
            // and playlist files over a slow MTP FUSE mount on the UI thread
            // froze the app. A throwaway read-only library handle is opened on
            // that thread (same pattern as the scan workers).
            let ext = state_sync
                .borrow()
                .config
                .media_library
                .playlist_format
                .extension()
                .to_string();
            let db_path = crate::media_library::MediaLibrary::db_path_pub();
            let state_sync = state_sync.clone();
            let win_wk = win_wk.clone();
            let reload_sync = reload_sync.clone();
            glib::spawn_future_local(async move {
                let dev_b = dev.clone();
                let (plan, pl_plan) = gio::spawn_blocking(move || {
                    if device_io_shutting_down() {
                        return (Vec::new(), Vec::new());
                    }
                    match crate::media_library::MediaLibrary::open_at(&db_path) {
                        Ok(lib) => (
                            device_sync_plan(&lib, &dev_b),
                            device_playlist_sync_plan(&lib, &dev_b, &ext),
                        ),
                        Err(_) => (Vec::new(), Vec::new()),
                    }
                })
                .await
                .unwrap_or((Vec::new(), Vec::new()));
            let to_lib = plan
                .iter()
                .filter(|(_, a)| *a == SyncAction::DeviceToLibrary)
                .count();
            let to_dev = plan
                .iter()
                .filter(|(_, a)| *a == SyncAction::LibraryToDevice)
                .count();
            let song_conflict = plan
                .iter()
                .filter(|(_, a)| *a == SyncAction::Conflict)
                .count();
            let pl_push = pl_plan.iter().filter(|i| i.dir == PlaylistSyncDir::Push).count();
            let pl_pull = pl_plan.iter().filter(|i| i.dir == PlaylistSyncDir::Pull).count();
            let pl_conflict = pl_plan
                .iter()
                .filter(|i| i.dir == PlaylistSyncDir::Conflict)
                .count();
            if to_lib == 0
                && to_dev == 0
                && song_conflict == 0
                && pl_push == 0
                && pl_pull == 0
                && pl_conflict == 0
            {
                set_button_busy(&sync_btn, false, "Sync");
                show_alert_parented(
                    win_wk.upgrade().as_ref(),
                    "Already in sync — no tag or playlist changes to apply.",
                );
                return;
            }
            let dname = if dev.label.is_empty() {
                "The device".to_string()
            } else {
                dev.label.clone()
            };
            let mut pl_bits: Vec<String> = Vec::new();
            if song_conflict > 0 {
                pl_bits.push(format!(
                    "{song_conflict} song conflict{} to resolve",
                    if song_conflict == 1 { "" } else { "s" }
                ));
            }
            if pl_push + pl_pull > 0 {
                pl_bits.push(format!(
                    "{} playlist{} to update",
                    pl_push + pl_pull,
                    if pl_push + pl_pull == 1 { "" } else { "s" }
                ));
            }
            if pl_conflict > 0 {
                pl_bits.push(format!(
                    "{pl_conflict} playlist conflict{} to resolve",
                    if pl_conflict == 1 { "" } else { "s" }
                ));
            }
            let pl_line = if pl_bits.is_empty() {
                String::new()
            } else {
                format!(" {}.", pl_bits.join(", "))
            };
            let detail = format!(
                "{dname} has {to_lib} updated song{}, this computer has {to_dev} updated song{}.{pl_line} \
                 Sync all changes?",
                if to_lib == 1 { "" } else { "s" },
                if to_dev == 1 { "" } else { "s" },
            );
            // Planning done — restore the button; the modal dialog now drives
            // the rest of the flow.
            set_button_busy(&sync_btn, false, "Sync");
            let dialog = gtk4::AlertDialog::builder()
                .message("Sync device")
                .detail(detail)
                .buttons(vec!["Cancel".to_string(), "Sync".to_string()])
                .cancel_button(0)
                .default_button(1)
                .modal(true)
                .build();
            let state2 = state_sync.clone();
            let dev2 = dev.clone();
            let plan2 = plan;
            let pl_plan2 = pl_plan;
            let win_wk2 = win_wk.clone();
            let reload2 = reload_sync.clone();
            dialog.choose(
                win_wk.upgrade().as_ref(),
                None::<&gio::Cancellable>,
                move |res| {
                    if res != Ok(1) {
                        return;
                    }
                    let (applied, failed) = apply_device_sync(&state2, &dev2, &plan2);
                    // Auto-apply the unambiguous playlist directions; collect the
                    // both-changed conflicts to prompt for afterwards.
                    let mut pl_updated = 0usize;
                    let mut pl_copied = 0usize;
                    let mut conflicts: Vec<PlaylistSyncItem> = Vec::new();
                    for item in &pl_plan2 {
                        match item.dir {
                            PlaylistSyncDir::Push => {
                                let (c, ok) = apply_playlist_push(&state2, &dev2, item);
                                pl_copied += c;
                                if ok {
                                    pl_updated += 1;
                                }
                            }
                            PlaylistSyncDir::Pull => {
                                if apply_playlist_pull(&state2, item) {
                                    pl_updated += 1;
                                }
                            }
                            PlaylistSyncDir::Conflict => conflicts.push(item.clone()),
                            PlaylistSyncDir::None => {}
                        }
                    }
                    reload2(dev2.clone());

                    let summary = {
                        let tail = if failed > 0 {
                            format!(", {failed} failed")
                        } else {
                            String::new()
                        };
                        let pl_tail = if pl_updated > 0 {
                            format!(
                                "; updated {pl_updated} playlist{} ({pl_copied} new file{} copied)",
                                if pl_updated == 1 { "" } else { "s" },
                                if pl_copied == 1 { "" } else { "s" },
                            )
                        } else {
                            String::new()
                        };
                        format!(
                            "Synced {applied} song{}{pl_tail}{tail}.",
                            if applied == 1 { "" } else { "s" }
                        )
                    };

                    // Per-file tag conflicts (both sides changed a song's tags).
                    let tag_conflicts = build_tag_conflicts(&dev2, &plan2);

                    // Final step: refresh + show the summary.
                    let final_done: Rc<dyn Fn()> = {
                        let reload_done = reload2.clone();
                        let dev_done = dev2.clone();
                        let win_done = win_wk2.clone();
                        Rc::new(move || {
                            reload_done(dev_done.clone());
                            show_alert_parented(win_done.upgrade().as_ref(), &summary);
                        })
                    };
                    // After tag conflicts, resolve playlist conflicts, then finish.
                    let after_tags: Rc<dyn Fn()> = if conflicts.is_empty() {
                        final_done
                    } else {
                        let state_pl = state2.clone();
                        let dev_pl = dev2.clone();
                        let win_pl = win_wk2.clone();
                        Rc::new(move || {
                            prompt_playlist_conflicts(
                                state_pl.clone(),
                                dev_pl.clone(),
                                conflicts.clone(),
                                win_pl.clone(),
                                final_done.clone(),
                            );
                        })
                    };
                    if tag_conflicts.is_empty() {
                        (after_tags)();
                    } else {
                        prompt_tag_conflicts(
                            state2.clone(),
                            dev2.clone(),
                            tag_conflicts,
                            win_wk2.clone(),
                            after_tags,
                        );
                    }
                },
            );
            });
        })
    };
    *sync_run_holder.borrow_mut() = Some(sync_run.clone());
    {
        let devices_sync = current_devices.clone();
        let sel_backend = selected_dev_backend.clone();
        let sync_run = sync_run.clone();
        dev_sync.connect_clicked(move |btn| {
            let Some(backend) = sel_backend.borrow().clone() else { return };
            let dev = devices_sync
                .borrow()
                .iter()
                .find(|d| d.backend_id == backend)
                .cloned();
            let Some(dev) = dev else { return };
            sync_run(dev, btn.clone());
        });
    }

    sidebar.select_row(sidebar.row_at_index(0).as_ref());

    let init_sidebar_width = state.borrow().config.window.ml_sidebar_width;
    paned.set_start_child(Some(&sidebar_scroll));
    paned.set_end_child(Some(&stack));
    paned.set_position(init_sidebar_width);
    win.set_child(Some(&paned));

    win.connect_close_request({
        let state = state.clone();
        let playlists_expanded = playlists_expanded.clone();
        let paned_ref = paned.clone();
        let col_view_holder = col_view_holder.clone();
        let all_cols_holder = all_cols_holder.clone();
        move |w| {
            let (w_size, h_size) = (w.width(), w.height());
            // Capture current column display order before borrowing state.
            let col_order: Vec<String> = col_view_holder
                .borrow()
                .as_ref()
                .map(|cv| {
                    let col_model = cv.columns();
                    let ac = all_cols_holder.borrow();
                    (0..col_model.n_items())
                        .filter_map(|i| col_model.item(i)?.downcast::<ColumnViewColumn>().ok())
                        .filter_map(|col| {
                            ac.iter().find(|(_, c)| c == &col).map(|(id, _)| id.clone())
                        })
                        .collect()
                })
                .unwrap_or_default();
            // Capture current per-column widths.
            let col_widths: std::collections::HashMap<String, i32> = {
                let ac = all_cols_holder.borrow();
                ac.iter()
                    .filter_map(|(id, col)| {
                        let w = col.fixed_width();
                        if w > 0 { Some((id.clone(), w)) } else { None }
                    })
                    .collect()
            };
            {
                let mut s = state.borrow_mut();
                s.config.window.ml_width = w_size;
                s.config.window.ml_height = h_size;
                s.config.window.ml_playlists_expanded = playlists_expanded.get();
                s.config.window.ml_sidebar_width = paned_ref.position();
                s.config.media_library.ml_file_col_order = col_order;
                s.config.media_library.ml_file_col_widths = col_widths;
                s.rebuild_ml_callback = None;
            }
            let _ = state.borrow().config.save();
            state.borrow_mut().ml_window = None;
            // Drop the editor-refresh hooks so we don't pin closed-window
            // Rcs in thread-local storage across an ML reopen.
            EDITOR_REFRESH_HOOK.with(|h| *h.borrow_mut() = None);
            EDITOR_CURRENT_REFRESH_HOOK.with(|h| *h.borrow_mut() = None);
            PLAYLIST_NAV_REFRESH_HOOK.with(|h| *h.borrow_mut() = None);
            glib::Propagation::Proceed
        }
    });

    win.present();
    win
}

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
    fn remove_last_remaining_track_stops_player_and_returns_none() {
        let mut s = state_with_tracks(&["A"]);
        let result = s.remove_track(0);
        assert!(result.is_none());
        assert!(s.playlist.is_empty());
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

    // ── AppState::apply_probed_duration ───────────────────────────────────────

    #[test]
    fn apply_probed_duration_sets_track_duration() {
        let mut s = make_state();
        s.playlist.add(fake_track("Song"));
        let path = s.playlist.tracks[0].path.clone();
        let dur = Duration::from_secs(180);
        let _ = s.apply_probed_duration(&path, dur);
        assert_eq!(s.playlist.tracks[0].duration, Some(dur));
    }

    #[test]
    fn apply_probed_duration_inserts_into_cache() {
        let mut s = make_state();
        s.playlist.add(fake_track("Song"));
        let path = s.playlist.tracks[0].path.clone();
        let _ = s.apply_probed_duration(&path, Duration::from_secs(120));
        assert!(s.duration_cache.dirty);
        assert_eq!(s.duration_cache.get(&path), Some(Duration::from_secs(120)));
    }

    #[test]
    fn apply_probed_duration_updates_last_duration_for_current_stopped_track() {
        let mut s = make_state();
        s.playlist.add(fake_track("Song"));
        let path = s.playlist.tracks[0].path.clone();
        let dur = Duration::from_secs(200);
        let _ = s.apply_probed_duration(&path, dur);
        // Player is Stopped (freshly created), current track matches → last_duration set.
        assert_eq!(s.last_duration, Some(dur));
    }

    #[test]
    fn apply_probed_duration_does_not_update_last_duration_for_non_current_track() {
        let mut s = make_state();
        s.playlist.add(fake_track("A"));
        s.playlist.add(fake_track("B"));
        s.playlist.current_index = 0;
        let path_b = s.playlist.tracks[1].path.clone();
        let _ = s.apply_probed_duration(&path_b, Duration::from_secs(99));
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
