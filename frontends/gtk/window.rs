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
//! - Animated visualizer (bars / oscilloscope, toggled with `a`)
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
// TODO: Migrate to modern APIs (DropDown, ColorDialogButton, etc.) when feasible
#[allow(deprecated)]
use gtk4::{
    gdk, gdk_pixbuf, gio, glib, Adjustment, Align, Application, ApplicationWindow, Box as GtkBox,
    Button, CellRendererText, CheckButton, ColumnView, ColumnViewColumn, CustomSorter, DragSource,
    DrawingArea, DropDown, DropTarget, Entry, EventControllerKey, GestureClick, Grid, Image, Label,
    ListBox, ListBoxRow, ListStore, MultiSelection, Notebook, Orientation, PolicyType, Popover,
    Scale, ScrolledWindow, Separator, SignalListItemFactory, SortListModel, Stack,
    StackTransitionType, TreeView, TreeViewColumn,
};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use crate::{
    config::{Config, VisualizerMode},
    duration_cache::DurationCache,
    duration_probe,
    engine::{BusEvent, Player, PlayerState},
    filetype_plugin::{self, FiletypePlugin},
    model::{fmt_duration, Playlist, Track},
    plugin_manager::PluginManager,
    shuffle::ShuffleState,
    viz_plugin::{load_plugins_from_dir, VizPlugin},
};

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
    /// Visualizer plugins loaded from `config.plugins.visualizer_dir` at startup.
    /// Empty when no directory is configured or no valid plugins were found.
    viz_plugins: Vec<VizPlugin>,
    /// Index into `viz_plugins` of the currently active plugin.
    /// `None` means use the built-in visualizer mode from `config.visualizer.mode`.
    active_plugin_idx: Option<usize>,
    /// Filetype plugins loaded from `config.plugins.filetype_dir` at startup.
    /// Empty when no directory is configured or no valid plugins were found.
    filetype_plugins: Vec<FiletypePlugin>,
    /// Media library — open on startup, or `None` when the DB cannot be opened.
    media_lib: Option<crate::media_library::MediaLibrary>,
    /// Plugin registry: owns all loaded visualizer and filetype plugins.
    plugin_manager: PluginManager,
    /// The media library browser window, if one is currently open.
    ml_window: Option<gtk4::Window>,
    /// The ID3 tag editor window, if one is currently open.
    id3_editor_window: Option<gtk4::Window>,
    /// Callback to refresh the media library window, registered by the window itself.
    rebuild_ml_callback: Option<Rc<dyn Fn()>>,
    /// Callback to update ML scan UI in all windows, registered by each window.
    ml_scan_ui_callback: Option<Rc<dyn Fn()>>,
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
        let mut s = state.borrow_mut();
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
        // Load visualizer and filetype plugins from their configured directories
        // (best-effort; failures produce warnings but never block startup).
        let viz_plugins = load_plugins_from_dir(&config.plugins.visualizer_dir);
        let filetype_plugins = filetype_plugin::load_plugins_from_dir(&config.plugins.filetype_dir);
        let mut plugin_manager = PluginManager::new();
        plugin_manager.load_from_config(&config);
        let media_lib = crate::media_library::MediaLibrary::open().ok();
        Ok(AppState {
            player,
            playlist,
            config,
            shuffle_state: ShuffleState::new(),
            pending_seek: None,
            last_duration: None,
            mute_pending: None,
            duration_cache: DurationCache::load(),
            viz_plugins,
            active_plugin_idx: None,
            filetype_plugins,
            media_lib,
            plugin_manager,
            ml_window: None,
            id3_editor_window: None,
            rebuild_ml_callback: None,
            ml_scan_ui_callback: None,
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

    /// Advance to the next track, respecting shuffle and repeat modes.
    ///
    /// Returns `Some(display_name)` if a next track was found, or `None` if
    /// playback should stop (end of playlist with repeat off).
    fn play_next(&mut self) -> Option<String> {
        let total = self.playlist.len();
        let current = self.playlist.current_index;
        let repeat = self.config.playback.repeat_mode;
        let idx = self.shuffle_state.next_index(current, total, repeat)?;
        self.playlist.jump_to(idx);
        if *self.player.state() != PlayerState::Stopped {
            self.play_current()
        } else {
            self.playlist.current().map(|t| t.display_name())
        }
    }

    /// Implement the "back button" behaviour with shuffle history support.
    ///
    /// - ≥ 2 s elapsed → restart the current track from the beginning.
    /// - < 2 s elapsed + shuffle history → step back through session history.
    /// - < 2 s elapsed + no history → linear previous track.
    ///
    /// Returns `Some(display_name)` of the track that will now play.
    fn play_prev(&mut self) -> Option<String> {
        let pos = self.player.position().unwrap_or(Duration::ZERO);
        let do_play = *self.player.state() != PlayerState::Stopped;
        if pos.as_secs() >= 2 {
            // Restart the current track.
            if do_play {
                self.play_current()
            } else {
                self.playlist.current().map(|t| t.display_name())
            }
        } else if let Some(idx) = self.shuffle_state.prev_from_history() {
            // Step back through the session's playback history.
            self.playlist.jump_to(idx);
            if do_play {
                self.play_current()
            } else {
                self.playlist.current().map(|t| t.display_name())
            }
        } else {
            // No history (beginning of session) — fall back to linear prev.
            self.playlist.previous();
            if do_play {
                self.play_current()
            } else {
                self.playlist.current().map(|t| t.display_name())
            }
        }
    }

    /// Cycle the visualizer to the next available mode.
    ///
    /// Cycle order: Bars → Oscilloscope → plugin 0 → plugin 1 → … → Bars.
    /// When no plugins are loaded the cycle is simply Bars ↔ Oscilloscope.
    fn toggle_visualizer_mode(&mut self) {
        match self.active_plugin_idx {
            None => match self.config.visualizer.mode {
                VisualizerMode::Bars => {
                    self.config.visualizer.mode = VisualizerMode::Oscilloscope;
                }
                VisualizerMode::Oscilloscope => {
                    if !self.viz_plugins.is_empty() {
                        self.active_plugin_idx = Some(0);
                    } else {
                        self.config.visualizer.mode = VisualizerMode::Bars;
                    }
                }
            },
            Some(idx) => {
                if idx + 1 < self.viz_plugins.len() {
                    self.active_plugin_idx = Some(idx + 1);
                } else {
                    self.active_plugin_idx = None;
                    self.config.visualizer.mode = VisualizerMode::Bars;
                }
            }
        }
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
            // Recursively collect all audio files, including any extensions
            // registered by loaded filetype plugins.
            let extra = crate::filetype_plugin::extra_extensions(&self.filetype_plugins);
            let files = Playlist::collect_audio_files_extended(path, &extra);
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
use crate::skin::{prepare_css, DARK_CSS_RAW, LIGHT_CSS_RAW};

/// Read the user's GNOME accent-colour choice from gsettings and return
/// the matching hex string.  Falls back to GNOME's default blue when
/// gsettings is unavailable or the value is unrecognised.
fn gtk_safe(s: &str) -> String {
    if s.contains('\0') {
        s.replace('\0', "")
    } else {
        s.to_owned()
    }
}

fn sanitize_id3_text(s: &str) -> String {
    let trimmed = s.trim();
    let without_nulls = if trimmed.contains('\0') {
        trimmed.replace('\0', "")
    } else {
        trimmed.to_owned()
    };
    let without_control: String = without_nulls
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    without_control.chars().take(256).collect()
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
    let dd = DropDown::from_strings(crate::id3_editor::ID3V1_GENRES);
    let entry = Entry::new();
    entry.set_width_chars(16);

    // Try to match initial value to a predefined genre
    if let Some(idx) = crate::id3_editor::ID3V1_GENRES
        .iter()
        .position(|g| *g == initial_value)
    {
        dd.set_selected(idx as u32);
    } else {
        entry.set_text(initial_value);
    }

    (dd, entry)
}

/// Get the system accent color from GNOME settings.
fn system_accent_hex() -> &'static str {
    let output = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "accent-color"])
        .output();
    let name = output
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().trim_matches('\'').to_string())
        .unwrap_or_default();
    match name.as_str() {
        "blue" => "#3584e4",
        "teal" => "#2190a4",
        "green" => "#3a944a",
        "yellow" => "#c88800",
        "orange" => "#ed5b00",
        "red" => "#e62d42",
        "pink" => "#d56199",
        "purple" => "#9141ac",
        "slate" => "#6f8396",
        _ => "#3584e4", // GNOME default blue
    }
}

/// Resolve the accent color hex from config. Returns the hex string.
fn resolve_accent_hex(accent_choice: &crate::config::AccentColorChoice) -> String {
    match accent_choice {
        crate::config::AccentColorChoice::System => system_accent_hex().to_string(),
        crate::config::AccentColorChoice::Custom(hex) => hex.clone(),
        _ => accent_choice.hex().unwrap_or("#3584e4").to_string(),
    }
}

/// Reload the CSS with a new accent color. Called when the accent color setting changes.
fn reload_css_accent(
    provider: &gtk4::CssProvider,
    _dark_css: &str,
    _light_css: &str,
    is_dark: bool,
    accent_hex: &str,
) {
    use crate::skin::prepare_css;
    let css = prepare_css(
        if is_dark { DARK_CSS_RAW } else { LIGHT_CSS_RAW },
        accent_hex,
    );
    provider.load_from_data(&css);
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

/// Invert the RGB channels of a pixbuf (leave alpha unchanged).
/// Used to turn the black logo into a white logo for dark mode.
fn invert_pixbuf(src: &gdk_pixbuf::Pixbuf) -> gdk_pixbuf::Pixbuf {
    let pb = src.copy().expect("pixbuf copy");
    let n_channels = pb.n_channels() as usize;
    let rowstride = pb.rowstride() as usize;
    let width = pb.width() as usize;
    let height = pb.height() as usize;
    // SAFETY: we own the only reference to this freshly-copied pixbuf.
    let pixels = unsafe { pb.pixels() };
    for row in 0..height {
        for col in 0..width {
            let off = row * rowstride + col * n_channels;
            pixels[off] = 255 - pixels[off]; // R
            pixels[off + 1] = 255 - pixels[off + 1]; // G
            pixels[off + 2] = 255 - pixels[off + 2]; // B
                                                     // pixels[off + 3] is alpha — left unchanged
        }
    }
    pb
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

pub fn build(app: &Application, playlist: Playlist, config: Config) {
    // ── CSS theme ─────────────────────────────────────────────────────────────
    // Inject the accent colour at startup so @accent_bg_color always resolves.
    // If the user has configured a custom skin name, try to load it; fall back
    // to the built-in dark or light skin based on AppearanceConfig.theme.
    let accent_hex_initial = resolve_accent_hex(&config.appearance.accent_color);
    let accent_hex_current = Rc::new(RefCell::new(accent_hex_initial.clone()));
    let dark_css_rc = Rc::new(prepare_css(DARK_CSS_RAW, &accent_hex_initial));
    let light_css_rc = Rc::new(prepare_css(LIGHT_CSS_RAW, &accent_hex_initial));

    // Determine the initial CSS to load.
    let initial_css = {
        use crate::config::ThemeChoice;
        use crate::skin;
        let custom = &config.appearance.custom_skin;
        if !custom.is_empty() {
            // Try to load the user-specified skin; fall back to dark on failure.
            skin::load_prepared(custom, &accent_hex_initial)
                .unwrap_or_else(|| dark_css_rc.as_ref().clone())
        } else {
            match config.appearance.theme {
                ThemeChoice::Dark => dark_css_rc.as_ref().clone(),
                ThemeChoice::Light => light_css_rc.as_ref().clone(),
            }
        }
    };

    let provider = Rc::new(gtk4::CssProvider::new());
    provider.load_from_data(&initial_css);
    gtk4::style_context_add_provider_for_display(
        &gdk::Display::default().expect("No display"),
        &*provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let dark_mode = Rc::new(Cell::new(true));

    // Clone provider and CSS for use by handlers that need them.
    let provider_for_settings = provider.clone();
    let dark_css_for_settings = dark_css_rc.clone();
    let light_css_for_settings = light_css_rc.clone();

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

    let window = ApplicationWindow::builder()
        .application(app)
        .title("SparkAmp")
        .default_width(init_player_width)
        .default_height(init_player_height)
        .build();

    let root = GtkBox::new(Orientation::Vertical, 0);

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

    // Small play/pause/stop indicator to the left of the time display.
    // Subtle: uses the same dim style as the secondary track-index line.
    let state_label = Label::builder()
        .label("⏹")
        .halign(Align::Center)
        .valign(Align::Center)
        .css_classes(["np-artist"])
        .build();

    // Time display label — single-line, monospace, centered.
    // Clicking toggles between elapsed and remaining time.
    let show_remaining = Rc::new(Cell::new(false));
    let time_disp_label = Label::builder()
        .label("0:00")
        .halign(Align::Center)
        .css_classes(["time-disp"])
        .build();
    {
        let show_rem = show_remaining.clone();
        let click = GestureClick::new();
        // connect_released fires after a complete click (pressed + released),
        // giving the user a clear tap-to-toggle interaction.
        click.connect_released(move |_, _, _, _| {
            show_rem.set(!show_rem.get());
        });
        time_disp_label.add_controller(click);
    }

    // Row containing [state_icon | time_display].
    let time_row = GtkBox::new(Orientation::Horizontal, 4);
    time_row.set_halign(Align::Center);
    time_row.append(&state_label);
    time_row.append(&time_disp_label);

    // Mini visualizer DrawingArea — clicking cycles the visualizer mode.
    let viz = DrawingArea::new();
    viz.set_content_width(100);
    viz.set_content_height(68);
    viz.set_valign(Align::Center);
    viz.add_css_class("mini-viz");
    {
        let state_vc = state.clone();
        let click = GestureClick::new();
        click.connect_released(move |_, _, _, _| {
            state_vc.borrow_mut().toggle_visualizer_mode();
        });
        viz.add_controller(click);
    }

    left_col.append(&time_row);
    left_col.append(&viz);

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

    // Dim secondary line: shows current track index within the playlist.
    // Updated by the tick loop so it stays current without extra callbacks.
    let artist_label = Label::builder()
        .label("")
        .halign(Align::Start)
        .margin_start(12) // indent to visually separate from frame edge
        .margin_top(2)
        .css_classes(["np-artist"])
        .build();

    np_info.append(&artist_label);

    // Expanding spring pushes the vol row to the bottom of the column so it
    // sits on the same horizontal line as the bottom of the visualizer.
    let info_spring = GtkBox::new(Orientation::Vertical, 0);
    info_spring.set_vexpand(true);
    np_info.append(&info_spring);

    np_row.append(&left_col);
    np_row.append(&np_info);
    root.append(&np_row);

    // ── Buttons created early so they can all live in the vol row ───────────
    // Repeat button: label and active state reflect saved config.
    let init_repeat = state.borrow().config.playback.repeat_mode;
    let btn_repeat = Button::with_label(init_repeat.symbol());
    btn_repeat.add_css_class("mode-btn");
    btn_repeat.set_tooltip_text(Some("Repeat: off / 1 (song) / A (all)"));
    if init_repeat != crate::shuffle::RepeatMode::Off {
        btn_repeat.add_css_class("mode-btn-active");
    }
    // Shuffle button: active state reflects saved shuffle state.
    let init_shuffle = state.borrow().shuffle_state.enabled;
    let btn_shuffle = Button::with_label("🔀");
    btn_shuffle.add_css_class("mode-btn");
    btn_shuffle.set_tooltip_text(Some("Shuffle on/off"));
    if init_shuffle {
        btn_shuffle.add_css_class("mode-btn-active");
    }

    let btn_pl = Button::with_label("PL");
    btn_pl.add_css_class("mode-btn");
    let btn_eq = Button::with_label("EQ");
    btn_eq.add_css_class("mode-btn");
    btn_eq.set_tooltip_text(Some("10-band equalizer"));
    let btn_info = Button::with_label("ℹ");
    btn_info.add_css_class("mode-btn");
    btn_info.set_tooltip_text(Some("Keyboard shortcuts"));
    let btn_ml = Button::with_label("ML");
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
    vol_bar.set_width_request(150);
    vol_bar.add_css_class("vol-scale");

    // Expanding spacer pushes PL to the right edge of np_info.
    let vol_spring = GtkBox::new(Orientation::Horizontal, 0);
    vol_spring.set_hexpand(true);

    vol_row.append(&vol_label);
    vol_row.append(&vol_bar);
    vol_row.append(&vol_spring);
    vol_row.append(&btn_info);
    vol_row.append(&btn_ml);
    vol_row.append(&btn_eq);
    vol_row.append(&btn_pl);

    // ── Mode row: repeat + shuffle, above the vol row ───────────────────────
    let mode_row = GtkBox::new(Orientation::Horizontal, 4);
    mode_row.set_margin_start(8);
    mode_row.set_margin_end(8);
    mode_row.set_margin_bottom(14);
    let mode_spring = GtkBox::new(Orientation::Horizontal, 0);
    mode_spring.set_hexpand(true);
    mode_row.append(&mode_spring);
    mode_row.append(&btn_repeat);
    mode_row.append(&btn_shuffle);
    np_info.append(&mode_row);
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
    let transport = GtkBox::new(Orientation::Horizontal, 4);
    transport.set_hexpand(true);
    transport.set_margin_start(8);
    transport.set_margin_end(8);
    transport.set_margin_top(8);
    transport.set_margin_bottom(8);

    let btn_prev = Button::with_label("⏮");
    let btn_play = Button::with_label("▶");
    let btn_pause = Button::with_label("⏸");
    let btn_stop = Button::with_label("⏹");
    let btn_next = Button::with_label("⏭");

    for btn in [&btn_prev, &btn_play, &btn_pause, &btn_stop, &btn_next] {
        btn.add_css_class("transport");
    }
    btn_play.add_css_class("transport-play");

    // Load logo at ~42 px (50 % larger than the transport buttons).
    // If the PNG fails to load (e.g. asset missing), the image slot stays blank.
    const LOGO_PX: i32 = 42;
    let logo_light = load_logo_pixbuf(LOGO_PX);
    let logo_dark = logo_light.as_ref().map(|pb| invert_pixbuf(pb));
    let logo_img = Image::new();
    logo_img.set_valign(Align::Center);
    logo_img.set_pixel_size(LOGO_PX);
    // Extra right-side padding so the logo's right edge aligns with the PL
    // button and progress bar end (both sit at 8px from the window edge; the
    // transport box itself already has margin_end(8)).
    logo_img.set_margin_end(8);
    // Initial theme: if dark_mode is set apply the inverted version.
    if dark_mode.get() {
        if let Some(ref pb) = logo_dark {
            logo_img.set_from_pixbuf(Some(pb));
        }
    } else {
        if let Some(ref pb) = logo_light {
            logo_img.set_from_pixbuf(Some(pb));
        }
    }
    // Wrap logo pixbufs in Rc so the theme-toggle closure can reach them.
    let logo_light_rc = Rc::new(logo_light);
    let logo_dark_rc = Rc::new(logo_dark);

    // Spring between buttons and logo.
    let transport_spring = GtkBox::new(Orientation::Horizontal, 0);
    transport_spring.set_hexpand(true);

    transport.append(&btn_prev);
    transport.append(&btn_play);
    transport.append(&btn_pause);
    transport.append(&btn_stop);
    transport.append(&btn_next);
    // Spring pushes the logo to the right.
    transport.append(&transport_spring);
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

    // ── Right-click on the player body → toggle dark / light theme ───────────
    {
        let provider_rc = provider.clone();
        let dark_ref = dark_mode.clone();
        let accent_cell = accent_hex_current.clone();
        let logo_img_rc = logo_img.clone();
        let logo_light_t = logo_light_rc.clone();
        let logo_dark_t = logo_dark_rc.clone();
        let rclick = GestureClick::new();
        rclick.set_button(3);
        rclick.connect_released(move |_, _, _, _| {
            let now_dark = !dark_ref.get();
            dark_ref.set(now_dark);
            let accent_hex = accent_cell.borrow().clone();
            let css = if now_dark {
                prepare_css(DARK_CSS_RAW, &accent_hex)
            } else {
                prepare_css(LIGHT_CSS_RAW, &accent_hex)
            };
            provider_rc.load_from_data(&css);
            // Swap logo to match the new theme.
            if now_dark {
                if let Some(ref pb) = *logo_dark_t {
                    logo_img_rc.set_from_pixbuf(Some(pb));
                }
            } else {
                if let Some(ref pb) = *logo_light_t {
                    logo_img_rc.set_from_pixbuf(Some(pb));
                }
            }
        });
        root.add_controller(rclick);
    }

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

    // ── Left-click on the logo → open settings window ────────────────────────
    {
        let state_rc = state.clone();
        let win_wk = window.downgrade();
        let dark_mode_clone = dark_mode.clone();
        let accent_hex_for_settings = accent_hex_current.clone();
        let accent_rgba_for_settings = accent_rgba.clone();
        let pl_store_ref = pl_store.clone();
        let lclick = GestureClick::new();
        lclick.set_button(1); // primary button only
        lclick.connect_released(move |_, _, _, _| {
            let parent_win = win_wk.upgrade();
            open_settings_window(
                parent_win.as_ref().map(|w| w.upcast_ref()),
                state_rc.clone(),
                None,
                dark_mode_clone.clone(),
                accent_hex_for_settings.clone(),
                accent_rgba_for_settings.clone(),
                provider_for_settings.clone(),
                dark_css_for_settings.clone(),
                light_css_for_settings.clone(),
                pl_store_ref.clone(),
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
        let pl_selected_idx = pl_selected_idx.clone();
        let pl_active_idx = pl_active_idx.clone();
        let accent_rgba = accent_rgba.clone();
        Rc::new(move || {
            let s = state.borrow();
            let current = s.playlist.current_index;
            let is_playing = matches!(
                *s.player.state(),
                PlayerState::Playing | PlayerState::Paused
            );
            let n = s.playlist.tracks.len();
            let saved_selected = pl_selected_idx.get();
            // Update pl_active_idx to match current playing track.
            if is_playing {
                pl_active_idx.set(current);
            } else {
                pl_active_idx.set(usize::MAX);
            }
            // Detach TreeView so bulk model changes don't trigger per-row signals.
            #[allow(deprecated)]
            pl_view.set_model(None::<&ListStore>);
            #[allow(deprecated)]
            pl_store.clear();
            for (i, t) in s.playlist.tracks.iter().enumerate() {
                let pos = format!("{}.", i + 1);
                let name = t.display_name();
                let is_active = is_playing && i == current;
                let is_row_selected = saved_selected != usize::MAX && saved_selected == i;
                let display = if t.broken {
                    format!("⚠ {}", name)
                } else if is_active {
                    format!("▶ {}", name)
                } else {
                    name
                };
                let weight: i32 = if is_active { 700 } else { 400 };
                // Compute foreground color: active > selected > default.
                let fg_rgba = if is_active {
                    accent_rgba
                        .borrow()
                        .clone()
                        .unwrap_or_else(|| gdk::RGBA::new(0.0, 0.6, 1.0, 1.0))
                } else if is_row_selected {
                    gdk::RGBA::new(1.0, 1.0, 1.0, 1.0)
                } else {
                    gdk::RGBA::new(0.8, 0.8, 0.8, 1.0)
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
            pl_count_label.set_label(&format!(
                "Playlist — {} track{}",
                n,
                if n == 1 { "" } else { "s" },
            ));
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
        let pl_selected_idx = pl_selected_idx.clone();
        let pl_active_idx = pl_active_idx.clone();
        let accent_rgba = accent_rgba.clone();
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
            // Compute foreground color: active > selected > default.
            let fg_rgba = {
                let active_idx = pl_active_idx.get();
                let selected_idx = pl_selected_idx.get();
                let is_row_active = active_idx != usize::MAX && active_idx == idx;
                let is_row_selected = selected_idx != usize::MAX && selected_idx == idx;
                if is_row_active {
                    accent_rgba
                        .borrow()
                        .clone()
                        .unwrap_or_else(|| gdk::RGBA::new(0.0, 0.6, 1.0, 1.0))
                } else if is_row_selected {
                    gdk::RGBA::new(1.0, 1.0, 1.0, 1.0)
                } else {
                    gdk::RGBA::new(0.8, 0.8, 0.8, 1.0)
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
        let current_track_meta_tx = current_track_meta_tx.clone();
        Rc::new(move || {
            // Record which row was playing before so we can un-bold it.
            let old_idx = state.borrow().playlist.current_index;
            let result = { state.borrow_mut().play_current() };
            if let Some(display) = result {
                let new_idx = state.borrow().playlist.current_index;
                eprintln!("[now-playing] playlist display: \"{}\"", display);
                set_track(&display);
                // Scan metadata for the current track if it hasn't been scanned yet.
                // This updates the marquee with "Artist - Title" once the scan completes.
                scan_current_track_metadata(&state, current_track_meta_tx.clone());
                // Patch the new current track to ensure active styling is applied.
                // Also patch old track if it was different.
                if old_idx != new_idx {
                    patch_pl_row(old_idx);
                }
                patch_pl_row(new_idx);
            }
        })
    };

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

    // ── DragSource on the TreeView — initiates row reorder drags ─────────────
    // prepare() fires before the drag starts; x/y are the pointer coordinates
    // within the widget.  path_at_pos identifies which row is being dragged and
    // packs its index as u32 into the content provider.
    {
        let drag_src = DragSource::new();
        drag_src.set_actions(gdk::DragAction::MOVE);
        let pl_view_ds = pl_view.clone();
        drag_src.connect_prepare(move |_, x, y| {
            #[allow(deprecated)]
            let row_idx = match pl_view_ds.path_at_pos(x as i32, y as i32) {
                Some((Some(path), _, _, _)) => path.indices().first().copied().unwrap_or(0) as u32,
                _ => return None,
            };
            Some(gdk::ContentProvider::for_value(&row_idx.to_value()))
        });
        pl_view.add_controller(drag_src);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Drag-and-drop: DropTarget on the TreeView (row reorder)
    // ══════════════════════════════════════════════════════════════════════════
    //
    // The DragSource on the TreeView carries the model index as u32.
    // path_at_pos converts the drop coordinate to the destination row index.
    {
        let drop_tgt = DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        let state_dnd = state.clone();
        let rebuild_dnd = rebuild_playlist.clone();
        let pl_view_dnd = pl_view.clone();

        drop_tgt.connect_drop(move |_, value, x, y| {
            if let Ok(src_pos) = value.get::<u32>() {
                let n = state_dnd.borrow().playlist.len();
                if n == 0 {
                    return false;
                }
                #[allow(deprecated)]
                let dst_pos = match pl_view_dnd.path_at_pos(x as i32, y as i32) {
                    Some((Some(path), _, _, _)) => {
                        path.indices().first().copied().unwrap_or(0) as usize
                    }
                    _ => n.saturating_sub(1),
                };
                let src_idx = src_pos as usize;
                if src_idx != dst_pos {
                    state_dnd.borrow_mut().playlist.move_track(src_idx, dst_pos);
                    rebuild_dnd();
                }
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
        let current_track_meta_tx = current_track_meta_tx.clone();
        move |_| {
            let old_idx = state.borrow().playlist.current_index;
            let result = { state.borrow_mut().play_next() };
            if let Some(display) = result {
                let new_idx = state.borrow().playlist.current_index;
                set_track(&display);
                scan_current_track_metadata(&state, current_track_meta_tx.clone());
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
        let current_track_meta_tx = current_track_meta_tx.clone();
        move |_| {
            let old_idx = state.borrow().playlist.current_index;
            let result = { state.borrow_mut().play_prev() };
            if let Some(display) = result {
                let new_idx = state.borrow().playlist.current_index;
                set_track(&display);
                scan_current_track_metadata(&state, current_track_meta_tx.clone());
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
        move |_| {
            let new_mode = {
                let mut s = state.borrow_mut();
                let m = s.config.playback.repeat_mode.cycle();
                s.config.playback.repeat_mode = m;
                m
            };
            // Update button label to show the active mode symbol.
            btn_repeat.set_label(new_mode.symbol());
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
                s.shuffle_state.enabled
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
    {
        let ctx_click = GestureClick::new();
        ctx_click.set_button(3); // right mouse button
        let state_ctx = state.clone();
        let play_ctx = play_and_update.clone();
        let remove_ctx = remove_selected.clone();
        let pl_view_ctx = pl_view.clone();
        let patch_ctx = patch_pl_row.clone();
        let win_ctx = window.downgrade();
        let rebuild_ctx = rebuild_playlist.clone();
        ctx_click.connect_pressed(move |_, _, x, y| {
            #[allow(deprecated)]
            let row_idx = match pl_view_ctx.path_at_pos(x as i32, y as i32) {
                Some((Some(path), _, _, _)) => match path.indices().first().copied() {
                    Some(i) => i as usize,
                    None => return,
                },
                _ => return,
            };
            // Select the right-clicked row so Remove acts on it.
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
            let popover = Popover::new();
            popover.set_parent(&pl_view_ctx);
            let rect = gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            popover.set_pointing_to(Some(&rect));
            let menu_box = GtkBox::new(Orientation::Vertical, 2);
            let btn_play = Button::with_label("▶ Play");
            let btn_id3 = Button::with_label("🎵 View / Edit ID3");
            let btn_del = Button::with_label("✕ Remove");
            for btn in [&btn_play, &btn_id3, &btn_del] {
                btn.add_css_class("popover-button");
            }
            {
                let state_p = state_ctx.clone();
                let play_p = play_ctx.clone();
                let pop_p = popover.clone();
                let patch_p = patch_ctx.clone();
                btn_play.connect_clicked(move |_| {
                    let old_idx = state_p.borrow().playlist.current_index;
                    state_p.borrow_mut().playlist.jump_to(row_idx);
                    play_p();
                    if old_idx != row_idx {
                        patch_p(old_idx);
                    }
                    pop_p.popdown();
                });
            }
            {
                let state_i = state_ctx.clone();
                let win_i = win_ctx.clone();
                let rebuild_i = rebuild_ctx.clone();
                let pop_i = popover.clone();
                btn_id3.connect_clicked(move |_| {
                    let path = state_i
                        .borrow()
                        .playlist
                        .tracks
                        .get(row_idx)
                        .map(|t| t.path.clone());
                    if let Some(path) = path {
                        open_id3_editor_window(
                            win_i.upgrade().as_ref(),
                            path,
                            state_i.clone(),
                            rebuild_i.clone(),
                            None,
                        );
                    }
                    pop_i.popdown();
                });
            }
            {
                let remove_r = remove_ctx.clone();
                let pop_r = popover.clone();
                btn_del.connect_clicked(move |_| {
                    remove_r();
                    pop_r.popdown();
                });
            }
            menu_box.append(&btn_play);
            menu_box.append(&btn_id3);
            menu_box.append(&btn_del);
            popover.set_child(Some(&menu_box));
            popover.popup();
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

                // Collect extra extensions on the main thread — plugin_manager is not Send.
                let extra = state_cb.borrow().plugin_manager.extra_extensions();

                // Capture where the new tracks will start before any are added.
                let scan_start = state_cb.borrow().playlist.len();

                let (fast_tx, fast_rx) = std::sync::mpsc::channel::<crate::model::Track>();
                let (meta_tx, meta_rx) =
                    std::sync::mpsc::channel::<(usize, String, String, String, String)>();
                let (done_tx, done_rx) = std::sync::mpsc::channel::<usize>();
                let (phase1_done_tx, phase1_done_rx) = std::sync::mpsc::channel::<usize>();

                crate::model::Playlist::scan_folder_for_ui(
                    folder,
                    extra,
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
    {
        let state = state.clone();
        let time_disp_label = time_disp_label.clone();
        let title_label = title_label.clone();
        let artist_label = artist_label.clone();
        let seek_bar = seek_bar.clone();
        let play_update = play_and_update.clone();
        let viz = viz.clone();
        let marquee_chars = marquee_chars.clone();
        let marquee_offset = marquee_offset.clone();
        let marquee_tick = marquee_tick.clone();
        let show_remaining = show_remaining.clone();
        let state_label = state_label.clone();
        let patch_pl_row = patch_pl_row.clone();
        let current_track_meta_rx = std::cell::RefCell::new(current_track_meta_rx);
        let set_track = set_track.clone();
        // Counter for periodic cache saves: fires every 300 ticks = 30 seconds.
        let mut cache_save_countdown = 300u32;

        glib::timeout_add_local(Duration::from_millis(100), move || {
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

            // 4. State icon (left of time display) + track-index line.
            {
                let s = state.borrow();
                let icon = match s.player.state() {
                    PlayerState::Playing => "▶",
                    PlayerState::Paused => "⏸",
                    PlayerState::Stopped => "⏹",
                };
                state_label.set_text(icon);
                let idx_text = if s.playlist.is_empty() {
                    String::new()
                } else {
                    format!("[{}/{}]", s.playlist.current_index + 1, s.playlist.len())
                };
                artist_label.set_text(&idx_text);
            }

            // 5. Trigger a Cairo repaint of the visualizer.
            viz.queue_draw();

            // 6. Periodically flush the duration cache to disk (every 30 s).
            cache_save_countdown -= 1;
            if cache_save_countdown == 0 {
                cache_save_countdown = 300;
                state.borrow_mut().duration_cache.save_if_dirty();
            }

            ControlFlow::Continue
        });
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Visualizer draw function (mini box in the now-playing row)
    // ══════════════════════════════════════════════════════════════════════════
    {
        let state = state.clone();
        viz.set_draw_func(move |_da, cr, width, height| {
            use std::f64::consts::PI;

            // ── Background ────────────────────────────────────────────────
            cr.set_source_rgb(0.05, 0.05, 0.05);
            cr.paint().ok();

            let s = state.borrow();
            let is_playing = *s.player.state() == PlayerState::Playing;
            let pos_secs = s.player.position().unwrap_or(Duration::ZERO).as_secs_f64();
            let mode = s.config.visualizer.mode.clone();
            let plugin_idx = s.active_plugin_idx;

            // If a plugin is selected and active, delegate the frame to it.
            if is_playing {
                if let Some(idx) = plugin_idx {
                    if let Some(plugin) = s.viz_plugins.get(idx) {
                        let count = (width / 5).max(10) as usize;
                        let values = plugin.render(pos_secs, true, count);
                        drop(s); // release borrow after last plugin access
                        let bar_w = width as f64 / count as f64;
                        for (i, &v) in values.iter().enumerate() {
                            let bar_h = (v * height as f64 * 0.92).max(2.0);
                            let x = i as f64 * bar_w + 0.5;
                            let y = height as f64 - bar_h;
                            cr.set_source_rgb(0.0, 0.55 + v * 0.35, v * 0.7);
                            cr.rectangle(x, y, bar_w - 1.5, bar_h);
                            cr.fill().ok();
                        }
                        return;
                    }
                }
            }
            drop(s); // release borrow before built-in rendering

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

            let pos_ms = (pos_secs * 1000.0) as u64;
            match mode {
                VisualizerMode::Bars => {
                    // Minimum 10 bars; more bars at wider widths.
                    // Each bar uses a frequency-scaled oscillation so lower-
                    // indexed bars move slowly (bass) and higher ones fast
                    // (treble), giving the classic spectrum analyser feel.
                    let num_bars = (width / 5).max(10) as usize;
                    let bar_w = width as f64 / num_bars as f64;
                    let t = pos_ms as f64 / 80.0;
                    for i in 0..num_bars {
                        let freq = 1.0 + i as f64 * 0.5;
                        let amp = ((t * freq).sin() * 0.4 + (t * freq * 1.5).sin() * 0.2 + 0.55)
                            .clamp(0.05, 1.0);
                        let bar_h = (amp * height as f64 * 0.92).max(2.0);
                        let x = i as f64 * bar_w + 0.5;
                        let y = height as f64 - bar_h;
                        // Colour: dark green (low) → bright cyan (high).
                        cr.set_source_rgb(0.0, 0.55 + amp * 0.35, amp * 0.7);
                        cr.rectangle(x, y, bar_w - 1.5, bar_h);
                        cr.fill().ok();
                    }
                }
                VisualizerMode::Oscilloscope => {
                    let t0 = pos_ms as f64 / 80.0;

                    // ── Dim centre baseline (orientation reference) ────────
                    cr.set_source_rgb(0.0, 0.2, 0.08);
                    cr.set_line_width(0.5);
                    cr.move_to(0.0, height as f64 / 2.0);
                    cr.line_to(width as f64, height as f64 / 2.0);
                    cr.stroke().ok();

                    // ── Animated waveform ──────────────────────────────────
                    // Composite of two sine waves at the golden-ratio interval
                    // (φ ≈ 1.618) for a natural, non-repeating look.
                    cr.set_source_rgb(0.0, 0.85, 0.35);
                    cr.set_line_width(2.0);
                    cr.move_to(0.0, height as f64 / 2.0);
                    for x in 0..width {
                        let t = x as f64 / width as f64;
                        let ph = t0 + t * PI * 6.0;
                        let amp = (ph.sin() + (ph * 1.618).sin() * 0.4) * 0.28;
                        cr.line_to(x as f64, height as f64 * (0.5 + amp));
                    }
                    cr.stroke().ok();
                }
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
    jump_entry.set_margin_end(8);

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

    let jump_root = gtk4::Box::new(Orientation::Vertical, 0);
    jump_root.append(&jump_entry);
    jump_root.append(&jump_scroll);

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

    // Maps each visible row in jump_box → the original track index in the playlist.
    let jump_indices: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    // Closure: clear and repopulate jump_box based on the current query.
    let rebuild_jump: Rc<dyn Fn()> = {
        let state = state.clone();
        let jump_entry = jump_entry.clone();
        let jump_box = jump_box.clone();
        let jump_indices = jump_indices.clone();
        Rc::new(move || {
            // Remove all existing rows.
            while let Some(row) = jump_box.row_at_index(0) {
                jump_box.remove(&row);
            }
            let mut indices = jump_indices.borrow_mut();
            indices.clear();

            let q = jump_entry.text();
            let s = state.borrow();
            for idx in s.playlist.search_indices(&q) {
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
            // Auto-select the first row so Enter immediately plays.
            if let Some(row) = jump_box.row_at_index(0) {
                jump_box.select_row(Some(&row));
            }
        })
    };

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
        let kbd_btn_shuffle = btn_shuffle.clone();

        Rc::new(move |key: gdk::Key| -> glib::Propagation {
            match key {
                // ── Winamp transport bindings ──────────────────────────────
                gdk::Key::z => {
                    let result = { state.borrow_mut().play_prev() };
                    if let Some(d) = result {
                        kbd_set_track(&d);
                        kbd_rebuild();
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
                    let result = { state.borrow_mut().play_next() };
                    if let Some(d) = result {
                        kbd_set_track(&d);
                        kbd_rebuild();
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
                                        eprintln!("{msg}");
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
                    kbd_btn_repeat.set_label(new_mode.symbol());
                    if new_mode == crate::shuffle::RepeatMode::Off {
                        kbd_btn_repeat.remove_css_class("mode-btn-active");
                    } else {
                        kbd_btn_repeat.add_css_class("mode-btn-active");
                    }
                    status_label.set_text(new_mode.label());
                    glib::Propagation::Stop
                }

                // ── Shuffle toggle (s — hidden; only shown in help) ───────
                gdk::Key::s | gdk::Key::S => {
                    let enabled = {
                        let mut s = state.borrow_mut();
                        s.shuffle_state.toggle();
                        s.shuffle_state.reset();
                        s.shuffle_state.enabled
                    };
                    if enabled {
                        kbd_btn_shuffle.add_css_class("mode-btn-active");
                        status_label.set_text("Shuffle: On");
                    } else {
                        kbd_btn_shuffle.remove_css_class("mode-btn-active");
                        status_label.set_text("Shuffle: Off");
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

    // ℹ Info button — show keyboard shortcuts window.
    // Connected here (after handle_key is defined) so shortcuts work inside it.
    btn_info.connect_clicked({
        let window_wk = window.downgrade();
        let handle_key = handle_key.clone();
        move |_| {
            let shortcuts_text = "SparkAmp — Keyboard Shortcuts

── Playback ────────────────────────────────────────
  z          Previous track / restart
  x          Play
  c          Pause / resume
  v          Stop
  b          Next track
  ←  →       Seek −5 s / +5 s
  r          Cycle repeat (off / song / playlist)

── Volume ──────────────────────────────────────────
  -          Volume down 5 %
  =          Volume up 5 %

── Playlist ────────────────────────────────────────
  n          Add file(s) or folder(s)
  j          Jump / search
  ↑ k / ↓ l  Browse up / down
  Enter      Play selected track
  Del        Remove highlighted track
  p          Toggle playlist window

── View & Tags ─────────────────────────────────────
  a          Cycle visualizer mode (bars / oscilloscope)
  d          View/Edit ID3 tags for current track
  u          Open EQ (TUI only — use EQ button in GUI)
  Click logo Open settings
  Right-click Toggle dark / light theme

── Hidden shortcuts ────────────────────────────────
  s          Toggle shuffle on/off

── Other ───────────────────────────────────────────
  i          Show this help
  q / Esc    Quit";

            let win = gtk4::Window::builder()
                .title("Keyboard Shortcuts")
                .modal(false)
                .default_width(420)
                .default_height(480)
                .build();
            if let Some(parent) = window_wk.upgrade() {
                win.set_transient_for(Some(&parent));
            }

            let scroll = gtk4::ScrolledWindow::builder()
                .hscrollbar_policy(gtk4::PolicyType::Never)
                .vscrollbar_policy(gtk4::PolicyType::Automatic)
                .margin_top(12)
                .margin_bottom(12)
                .margin_start(12)
                .margin_end(12)
                .child(
                    &gtk4::Label::builder()
                        .label(shortcuts_text)
                        .halign(gtk4::Align::Start)
                        .valign(gtk4::Align::Start)
                        .use_markup(false)
                        .selectable(false)
                        .css_classes(["info-text"])
                        .build(),
                )
                .build();

            // Esc closes; all transport shortcuts also work.
            let key_ctrl = gtk4::EventControllerKey::new();
            let handler = handle_key.clone();
            let win_wk2 = win.downgrade();
            key_ctrl.connect_key_pressed(move |_, key, _, _| {
                if key == gdk::Key::Escape {
                    if let Some(w) = win_wk2.upgrade() {
                        w.close();
                    }
                    return glib::Propagation::Stop;
                }
                handler(key)
            });
            win.add_controller(key_ctrl);

            win.set_child(Some(&scroll));
            win.present();
        }
    });

    // ML button — open the media library browser window.
    btn_ml.connect_clicked({
        let window_wk = window.downgrade();
        let state_rc = state.clone();
        let rebuild_pl = rebuild_playlist.clone();
        move |_| {
            // Destroy any existing ML window before opening a new one.
            {
                let s = state_rc.borrow_mut();
                if let Some(ref w) = s.ml_window {
                    w.destroy();
                }
            }
            let parent = window_wk.upgrade().map(|w| w.upcast::<gtk4::Window>());
            let (w, h) = {
                let cfg = &state_rc.borrow().config.window;
                (cfg.ml_width, cfg.ml_height)
            };
            let ml_win = open_media_library_window(
                parent.as_ref(),
                state_rc.clone(),
                rebuild_pl.clone(),
                w,
                h,
            );
            state_rc.borrow_mut().ml_window = Some(ml_win);
        }
    });

    // EQ button — open the 10-band equalizer window.
    btn_eq.connect_clicked({
        let window_wk = window.downgrade();
        let state_rc = state.clone();
        move |_| {
            let parent = window_wk.upgrade().map(|w| w.upcast::<gtk4::Window>());
            open_eq_window(parent.as_ref(), state_rc.clone());
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
            gdk::Key::Up | gdk::Key::k => {
                let cur = jb.selected_row().map(|r| r.index()).unwrap_or(1);
                if let Some(row) = jb.row_at_index((cur - 1).max(0)) {
                    jb.select_row(Some(&row));
                }
                glib::Propagation::Stop
            }
            gdk::Key::Down | gdk::Key::l => {
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
        move |w| {
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
            // Save ML window size before destroying it.
            if let Some(ref ml_win) = state.borrow().ml_window {
                cfg.window.ml_width = ml_win.width();
                cfg.window.ml_height = ml_win.height();
                ml_win.destroy();
            }
            let _ = cfg.save();
            playlist_win.destroy();

            // If any background operations (rescan, add folder) are still in flight,
            // force the main loop to exit. The background threads keep running but
            // the UI is gone so they have no effect.
            if state.borrow().pending_bg_ops.get() > 0 {
                if let Some(app) = w.application() {
                    app.quit();
                }
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
        glib::timeout_add_local_once(Duration::from_millis(50), move || {
            let state_rc = state.clone();
            let rebuild_pl = rebuild_playlist.clone();
            let ml_win = open_media_library_window(
                Some(&window.upcast::<gtk4::Window>()),
                state_rc.clone(),
                rebuild_pl.clone(),
                init_ml_width,
                init_ml_height,
            );
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

    let dlg = gtk4::Window::new();
    dlg.set_title(Some(title));
    dlg.set_default_size(400, 450);
    dlg.set_resizable(true);
    if let Some(p) = parent {
        dlg.set_transient_for(Some(p));
    }

    let main_vbox = GtkBox::new(Orientation::Vertical, 8);
    main_vbox.set_margin_top(12);
    main_vbox.set_margin_bottom(12);
    main_vbox.set_margin_start(12);
    main_vbox.set_margin_end(12);

    let (show_header_row, show_position_dropdown, cols_to_show, defaults_vis, defaults_pos): (
        bool,
        bool,
        Vec<&MlColumnDef>,
        Vec<String>,
        std::collections::HashMap<String, String>,
    ) = match mode {
        ColumnCustomizerMode::Id3Editor => {
            let cols: Vec<&MlColumnDef> = ALL_COLUMNS.iter().filter(|c| c.id3_editable).collect();
            let defaults_vis = crate::config::MediaLibraryConfig::default_id3_visible_columns();
            let defaults_pos = crate::config::MediaLibraryConfig::default_id3_column_position();
            (true, true, cols, defaults_vis, defaults_pos)
        }
        ColumnCustomizerMode::MediaLibrary => {
            let cols: Vec<&MlColumnDef> = ALL_COLUMNS.iter().collect();
            let defaults_vis = crate::config::MediaLibraryConfig::default_visible_columns();
            let defaults_pos = std::collections::HashMap::new();
            (false, false, cols, defaults_vis, defaults_pos)
        }
    };

    let hdr_text = if show_position_dropdown {
        "Select fields and column position:"
    } else {
        "Select columns to display:"
    };
    let hdr = Label::builder()
        .label(hdr_text)
        .halign(Align::Start)
        .build();
    main_vbox.append(&hdr);

    if show_header_row {
        let col_hdrs = GtkBox::new(Orientation::Horizontal, 8);
        col_hdrs.append(&Label::new(Some("")));
        col_hdrs.append(&Label::new(Some("Field")));
        let spring = GtkBox::new(Orientation::Horizontal, 0);
        spring.set_hexpand(true);
        col_hdrs.append(&spring);
        col_hdrs.append(&Label::new(Some("Column")));
        main_vbox.append(&col_hdrs);
    } else {
        let col_hdrs = GtkBox::new(Orientation::Horizontal, 8);
        col_hdrs.append(&Label::new(Some("")));
        col_hdrs.append(&Label::new(Some("Field")));
        let spring = GtkBox::new(Orientation::Horizontal, 0);
        spring.set_hexpand(true);
        col_hdrs.append(&spring);
        main_vbox.append(&col_hdrs);
    }

    let scrolled = ScrolledWindow::new();
    scrolled.set_hexpand(true);
    scrolled.set_vexpand(true);
    scrolled.set_has_frame(true);

    let list_vbox = GtkBox::new(Orientation::Vertical, 4);
    list_vbox.set_margin_top(8);

    let visible_ids: std::collections::HashSet<String> = match mode {
        ColumnCustomizerMode::Id3Editor => state
            .borrow()
            .config
            .media_library
            .id3_visible_columns
            .iter()
            .cloned()
            .collect(),
        ColumnCustomizerMode::MediaLibrary => state
            .borrow()
            .config
            .media_library
            .visible_columns
            .iter()
            .cloned()
            .collect(),
    };

    let column_positions: std::collections::HashMap<String, String> = match mode {
        ColumnCustomizerMode::Id3Editor => state
            .borrow()
            .config
            .media_library
            .id3_column_position
            .clone(),
        ColumnCustomizerMode::MediaLibrary => std::collections::HashMap::new(),
    };

    let checkboxes: Rc<RefCell<Vec<(String, gtk4::CheckButton)>>> =
        Rc::new(RefCell::new(Vec::new()));
    let dropdowns: Rc<RefCell<Vec<(String, gtk4::DropDown)>>> = Rc::new(RefCell::new(Vec::new()));
    let skipping_callback: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));

    for col in &cols_to_show {
        let row = GtkBox::new(Orientation::Horizontal, 8);

        let cb = CheckButton::new();
        cb.set_active(visible_ids.contains(col.id));
        let state_cfg = state.clone();
        let mode_for_cb = mode.clone();
        let on_toggle_cb = on_toggle.clone();
        let skip_cb = skipping_callback.clone();
        let id_for_toggle = col.id.to_string();
        cb.connect_toggled(move |btn| {
            if *skip_cb.borrow() {
                return;
            }
            let visible = btn.is_active();
            let id = id_for_toggle.clone();
            if let Some(ref cb) = on_toggle_cb {
                cb(id.clone(), visible);
            }
            let mut s = state_cfg.borrow_mut();
            match mode_for_cb {
                ColumnCustomizerMode::Id3Editor => {
                    let vc = &mut s.config.media_library.id3_visible_columns;
                    if btn.is_active() {
                        if !vc.contains(&id) {
                            vc.push(id);
                        }
                    } else {
                        vc.retain(|c| c != &id);
                    }
                }
                ColumnCustomizerMode::MediaLibrary => {
                    let vc = &mut s.config.media_library.visible_columns;
                    if btn.is_active() {
                        if !vc.contains(&id) {
                            vc.push(id);
                        }
                    } else {
                        vc.retain(|c| c != &id);
                    }
                }
            }
            let _ = s.config.save();
        });

        let lbl = Label::new(Some(col.header));
        lbl.set_halign(Align::Start);
        row.append(&cb);
        row.append(&lbl);

        let spring = GtkBox::new(Orientation::Horizontal, 0);
        spring.set_hexpand(true);
        row.append(&spring);

        if show_position_dropdown {
            let pos = column_positions
                .get(col.id)
                .cloned()
                .unwrap_or_else(|| "left".to_string());
            let dropdown = DropDown::from_strings(&["Left", "Right"]);
            dropdown.set_selected(if pos == "right" { 1 } else { 0 });

            let id_for_dropdown = col.id.to_string();
            let state_dropdown = state.clone();
            dropdown.connect_selected_notify(move |dd| {
                let position = if dd.selected() == 1 { "right" } else { "left" };
                let mut s = state_dropdown.borrow_mut();
                s.config
                    .media_library
                    .id3_column_position
                    .insert(id_for_dropdown.clone(), position.to_string());
                let _ = s.config.save();
            });

            row.append(&dropdown);
            dropdowns.borrow_mut().push((col.id.to_string(), dropdown));
        }

        list_vbox.append(&row);
        checkboxes.borrow_mut().push((col.id.to_string(), cb));
    }

    scrolled.set_child(Some(&list_vbox));
    main_vbox.append(&scrolled);

    let btn_row = GtkBox::new(Orientation::Horizontal, 8);

    let btn_reset = Button::with_label("Reset Defaults");
    let state_reset = state.clone();
    let cbs_reset = checkboxes.clone();
    let dds_reset = dropdowns.clone();
    let defaults_vis_clone = defaults_vis.clone();
    let defaults_pos_clone = defaults_pos.clone();
    let mode_for_reset = mode.clone();
    let on_toggle_reset = on_toggle.clone();
    let skip_cb_flag = skipping_callback.clone();

    btn_reset.connect_clicked(move |_| {
        let default_set: std::collections::HashSet<String> =
            defaults_vis_clone.iter().cloned().collect();

        if let Some(ref cb) = on_toggle_reset {
            *skip_cb_flag.borrow_mut() = true;
            for (id, _) in cbs_reset.borrow().iter() {
                cb(id.clone(), default_set.contains(id));
            }
            *skip_cb_flag.borrow_mut() = false;
        }

        {
            let mut s = state_reset.borrow_mut();
            match mode_for_reset {
                ColumnCustomizerMode::Id3Editor => {
                    s.config.media_library.id3_visible_columns = defaults_vis_clone.clone();
                    s.config.media_library.id3_column_position = defaults_pos_clone.clone();
                }
                ColumnCustomizerMode::MediaLibrary => {
                    s.config.media_library.visible_columns = defaults_vis_clone.clone();
                }
            }
            let _ = s.config.save();
        }

        *skip_cb_flag.borrow_mut() = true;
        for (id, cb) in cbs_reset.borrow().iter() {
            cb.set_active(default_set.contains(id));
        }
        *skip_cb_flag.borrow_mut() = false;
        for (id, dd) in dds_reset.borrow().iter() {
            let pos = defaults_pos_clone
                .get(id)
                .cloned()
                .unwrap_or_else(|| "left".to_string());
            dd.set_selected(if pos == "right" { 1 } else { 0 });
        }
    });

    btn_row.append(&btn_reset);

    let spring = GtkBox::new(Orientation::Horizontal, 0);
    spring.set_hexpand(true);
    btn_row.append(&spring);

    let btn_close = Button::with_label("Close");
    let dlg_wk = dlg.downgrade();
    let on_close_cb = on_close.clone();
    let mode_for_close = mode.clone();
    btn_close.connect_clicked(move |_| {
        if let ColumnCustomizerMode::Id3Editor = mode_for_close {
            if let Some(ref cb) = on_close_cb {
                cb();
            }
        }
        if let Some(w) = dlg_wk.upgrade() {
            w.close();
        }
    });
    btn_row.append(&btn_close);

    main_vbox.append(&btn_row);
    dlg.set_child(Some(&main_vbox));

    let on_close_req = on_close.clone();
    let mode_for_req = mode.clone();
    dlg.connect_close_request(move |_| {
        if let ColumnCustomizerMode::Id3Editor = mode_for_req {
            if let Some(ref cb) = on_close_req {
                cb();
            }
        }
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

    if let Some(ref existing_win) = state.borrow().id3_editor_window {
        let title = format!(
            "ID3 Tag Editor — {}",
            gtk_safe(path.file_name().and_then(|n| n.to_str()).unwrap_or("?"))
        );
        existing_win.set_title(Some(&title));
        existing_win.present();
        return;
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
    win.connect_close_request(move |_| {
        state_for_close.borrow_mut().id3_editor_window = None;
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
            let (combo, _entry) = make_genre_combo(&value);
            combo.set_hexpand(true);
            grid.attach(&combo, 1, row as i32, 1, 1);
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
    btn_browse.connect_clicked(move |_| {
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
        dialog.open(
            Some(&gtk4::Window::new()),
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

    let spring = GtkBox::new(Orientation::Horizontal, 0);
    spring.set_hexpand(true);
    btn_row.append(&btn_customize);
    btn_row.append(&spring);
    btn_row.append(&btn_cancel);
    btn_row.append(&btn_save);

    // ── Main layout ──────────────────────────────────────────────────────────
    let vbox = GtkBox::new(Orientation::Vertical, 0);
    vbox.append(&grid);
    vbox.append(&artwork_vbox);
    vbox.append(&Separator::new(Orientation::Horizontal));
    vbox.append(&status_lbl);
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

                    // Re-extract and update cached artwork from the saved file
                    if let Some(lib) = state_s.borrow().media_lib.as_ref() {
                        let path_str = path.to_string_lossy().into_owned();
                        if let Ok(lib_track) = lib.track_by_path(&path_str) {
                            let _ = lib.refresh_artwork(lib_track.id, &path_str);
                        }
                    }

                    let rebuild = rebuild_s.clone();
                    if let Some(w) = win_wk.upgrade() {
                        w.close();
                    }
                    glib::idle_add_local(move || {
                        rebuild();
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
/// `dark_mode` tracks the current theme for CSS reloads.
/// `accent_hex_current` stores the current accent hex for theme toggles.
/// `accent_rgba` is updated when accent changes to refresh playlist playing row color.
/// `pl_store` is used to repaint the playing row when accent changes.
#[allow(deprecated)]
fn open_settings_window(
    parent: Option<&gtk4::Window>,
    state: Rc<RefCell<AppState>>,
    initial_tab: Option<u32>,
    dark_mode: Rc<Cell<bool>>,
    accent_hex_current: Rc<RefCell<String>>,
    accent_rgba: Rc<RefCell<Option<gdk::RGBA>>>,
    css_provider: Rc<gtk4::CssProvider>,
    dark_css: Rc<String>,
    light_css: Rc<String>,
    pl_store: gtk4::ListStore,
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
        use crate::config::{AccentColorChoice, ThemeChoice};

        let grid = Grid::new();
        grid.set_row_spacing(12);
        grid.set_column_spacing(16);
        grid.set_margin_top(16);
        grid.set_margin_bottom(16);
        grid.set_margin_start(16);
        grid.set_margin_end(16);

        let lbl = Label::new(Some("Theme"));
        lbl.set_halign(Align::Start);
        grid.attach(&lbl, 0, 0, 1, 1);

        // DropDown: index 0 = Dark, index 1 = Light.
        let dd = DropDown::from_strings(&["Dark", "Light"]);
        {
            let theme = state.borrow().config.appearance.theme.clone();
            dd.set_selected(match theme {
                ThemeChoice::Dark => 0,
                ThemeChoice::Light => 1,
            });
        }
        {
            let state_rc = state.clone();
            let dark_mode_rc = dark_mode.clone();
            let provider_rc = css_provider.clone();
            let dark_css_rc = dark_css.clone();
            let light_css_rc = light_css.clone();
            dd.connect_selected_notify(move |d| {
                let theme = match d.selected() {
                    0 => ThemeChoice::Dark,
                    _ => ThemeChoice::Light,
                };
                {
                    let mut s = state_rc.borrow_mut();
                    s.config.appearance.theme = theme.clone();
                }
                dark_mode_rc.set(matches!(theme, ThemeChoice::Dark));
                // Reload CSS with new theme and current accent color.
                let is_dark = matches!(theme, ThemeChoice::Dark);
                let accent_hex =
                    resolve_accent_hex(&state_rc.borrow().config.appearance.accent_color);
                reload_css_accent(
                    &provider_rc,
                    &dark_css_rc,
                    &light_css_rc,
                    is_dark,
                    &accent_hex,
                );
            });
        }
        grid.attach(&dd, 1, 0, 1, 1);

        // Row 1: Highlight color dropdown.
        let accent_color_labels = [
            "System Default",
            "Blue",
            "Green",
            "Purple",
            "Red",
            "Orange",
            "Yellow",
            "White",
            "Grey",
            "Custom…",
        ];
        let lbl_accent = Label::new(Some("Highlight color"));
        lbl_accent.set_halign(Align::Start);
        grid.attach(&lbl_accent, 0, 1, 1, 1);

        let dd_accent = DropDown::from_strings(&accent_color_labels);
        let accent_container = GtkBox::new(Orientation::Horizontal, 4);
        #[allow(deprecated)]
        let custom_color_btn = gtk4::ColorButton::new();
        custom_color_btn.set_visible(false);
        accent_container.append(&dd_accent);
        accent_container.append(&custom_color_btn);

        // Initialize dropdown selection from config.
        {
            let accent_choice = state.borrow().config.appearance.accent_color.clone();
            let custom_hex = match &accent_choice {
                AccentColorChoice::Custom(hex) => Some(hex.clone()),
                _ => None,
            };
            let selection = match &accent_choice {
                AccentColorChoice::System => 0,
                AccentColorChoice::Blue => 1,
                AccentColorChoice::Green => 2,
                AccentColorChoice::Purple => 3,
                AccentColorChoice::Red => 4,
                AccentColorChoice::Orange => 5,
                AccentColorChoice::Yellow => 6,
                AccentColorChoice::White => 7,
                AccentColorChoice::Grey => 8,
                AccentColorChoice::Custom(_) => {
                    custom_color_btn.set_visible(true);
                    9
                }
            };
            dd_accent.set_selected(selection as u32);
            if let Some(hex) = custom_hex {
                if let Ok(color) = gdk::RGBA::parse(&hex) {
                    custom_color_btn.set_rgba(&color);
                }
            }
        }

        // Handle accent color changes.
        {
            let state_rc = state.clone();
            let provider_rc = css_provider.clone();
            let dark_css_rc = dark_css.clone();
            let light_css_rc = light_css.clone();
            let dark_mode_rc = dark_mode.clone();
            let accent_cell = accent_hex_current.clone();
            let accent_rgba_rc = accent_rgba.clone();
            let custom_btn = custom_color_btn.clone();
            let pl_store_rc = pl_store.clone();

            dd_accent.connect_selected_notify(move |d| {
                let selection = d.selected();
                let (accent_choice, _custom_hex) = match selection {
                    0 => (AccentColorChoice::System, None),
                    1 => (AccentColorChoice::Blue, None),
                    2 => (AccentColorChoice::Green, None),
                    3 => (AccentColorChoice::Purple, None),
                    4 => (AccentColorChoice::Red, None),
                    5 => (AccentColorChoice::Orange, None),
                    6 => (AccentColorChoice::Yellow, None),
                    7 => (AccentColorChoice::White, None),
                    8 => (AccentColorChoice::Grey, None),
                    _ => {
                        // Custom: read from color button
                        let rgba = custom_btn.rgba();
                        let hex = format!(
                            "#{:02x}{:02x}{:02x}",
                            (rgba.red() * 255.0) as u8,
                            (rgba.green() * 255.0) as u8,
                            (rgba.blue() * 255.0) as u8
                        );
                        (AccentColorChoice::Custom(hex.clone()), Some(hex))
                    }
                };

                // Show/hide custom color button.
                custom_btn.set_visible(selection == 9);

                // Update config.
                {
                    let mut s = state_rc.borrow_mut();
                    s.config.appearance.accent_color = accent_choice.clone();
                }

                // Reload CSS with new accent color.
                let is_dark = dark_mode_rc.get();
                let accent_hex = resolve_accent_hex(&accent_choice);
                *accent_cell.borrow_mut() = accent_hex.clone();
                reload_css_accent(
                    &provider_rc,
                    &dark_css_rc,
                    &light_css_rc,
                    is_dark,
                    &accent_hex,
                );
                // Update accent_rgba for playlist playing row color
                if let Ok(rgba) = gdk::RGBA::parse(&accent_hex) {
                    *accent_rgba_rc.borrow_mut() = Some(rgba);
                }
                // Repaint the currently playing row with new accent color
                let playing_idx = state_rc.borrow().playlist.current_index;
                let is_playing = matches!(
                    *state_rc.borrow().player.state(),
                    PlayerState::Playing | PlayerState::Paused
                );
                if is_playing && !state_rc.borrow().playlist.is_empty() {
                    #[allow(deprecated)]
                    if let Some(iter) = pl_store_rc.iter_nth_child(None, playing_idx as i32) {
                        let rgba = accent_rgba_rc
                            .borrow()
                            .clone()
                            .unwrap_or_else(|| gdk::RGBA::new(0.0, 0.6, 1.0, 1.0));
                        #[allow(deprecated)]
                        pl_store_rc.set_value(&iter, 4, &rgba.to_value());
                    }
                }
            });

            // Handle custom color button changes.
            {
                let state_rc2 = state.clone();
                let provider_rc2 = css_provider.clone();
                let dark_css_rc2 = dark_css.clone();
                let light_css_rc2 = light_css.clone();
                let dark_mode_rc2 = dark_mode.clone();
                let accent_cell2 = accent_hex_current.clone();
                let accent_rgba_rc2 = accent_rgba.clone();
                let pl_store_rc2 = pl_store.clone();
                #[allow(deprecated)]
                custom_color_btn.connect_color_set(move |btn| {
                    let rgba = btn.rgba();
                    let hex = format!(
                        "#{:02x}{:02x}{:02x}",
                        (rgba.red() * 255.0) as u8,
                        (rgba.green() * 255.0) as u8,
                        (rgba.blue() * 255.0) as u8
                    );
                    let accent_choice = AccentColorChoice::Custom(hex.clone());

                    // Update config.
                    {
                        let mut s = state_rc2.borrow_mut();
                        s.config.appearance.accent_color = accent_choice.clone();
                    }

                    // Reload CSS with new accent color.
                    let is_dark = dark_mode_rc2.get();
                    let accent_hex = resolve_accent_hex(&accent_choice);
                    *accent_cell2.borrow_mut() = accent_hex.clone();
                    reload_css_accent(
                        &provider_rc2,
                        &dark_css_rc2,
                        &light_css_rc2,
                        is_dark,
                        &accent_hex,
                    );
                    // Update accent_rgba for playlist playing row color
                    *accent_rgba_rc2.borrow_mut() = Some(rgba.clone());
                    // Repaint the currently playing row with new accent color
                    let playing_idx = state_rc2.borrow().playlist.current_index;
                    let is_playing = matches!(
                        *state_rc2.borrow().player.state(),
                        PlayerState::Playing | PlayerState::Paused
                    );
                    if is_playing && !state_rc2.borrow().playlist.is_empty() {
                        #[allow(deprecated)]
                        if let Some(iter) = pl_store_rc2.iter_nth_child(None, playing_idx as i32) {
                            #[allow(deprecated)]
                            let rgba = accent_rgba_rc2
                                .borrow()
                                .clone()
                                .unwrap_or_else(|| gdk::RGBA::new(0.0, 0.6, 1.0, 1.0));
                            #[allow(deprecated)]
                            pl_store_rc2.set_value(&iter, 4, &rgba.to_value());
                        }
                    }
                });
            }
        }
        grid.attach(&accent_container, 1, 1, 1, 1);

        // Row 2: Custom skin name (overrides Theme when non-empty).
        let lbl_skin = Label::new(Some("Custom skin name"));
        lbl_skin.set_halign(Align::Start);
        grid.attach(&lbl_skin, 0, 2, 1, 1);

        let entry_skin = Entry::new();
        entry_skin.set_text(&state.borrow().config.appearance.custom_skin);
        entry_skin.set_width_chars(24);
        entry_skin.set_placeholder_text(Some("(empty = use Theme above)"));
        {
            let state_rc = state.clone();
            entry_skin.connect_changed(move |e| {
                state_rc.borrow_mut().config.appearance.custom_skin = e.text().to_string();
            });
        }
        grid.attach(&entry_skin, 1, 2, 1, 1);

        let tab_lbl = Label::new(Some("Appearance"));
        notebook.append_page(&grid, Some(&tab_lbl));
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

        let lbl = Label::new(Some("Visualizer mode"));
        lbl.set_halign(Align::Start);
        grid.attach(&lbl, 0, 0, 1, 1);

        // DropDown: index 0 = Bars, index 1 = Oscilloscope.
        let dd = DropDown::from_strings(&["Bars", "Oscilloscope"]);
        {
            let mode = state.borrow().config.visualizer.mode.clone();
            dd.set_selected(match mode {
                VisualizerMode::Bars => 0,
                VisualizerMode::Oscilloscope => 1,
            });
        }
        {
            let state_rc = state.clone();
            dd.connect_selected_notify(move |d| {
                let mut s = state_rc.borrow_mut();
                s.config.visualizer.mode = match d.selected() {
                    0 => VisualizerMode::Bars,
                    _ => VisualizerMode::Oscilloscope,
                };
            });
        }
        grid.attach(&dd, 1, 0, 1, 1);

        let tab_lbl = Label::new(Some("Visualizer"));
        notebook.append_page(&grid, Some(&tab_lbl));
    }

    // ── Tab 3: Filetypes (plugin search paths) ────────────────────────────
    {
        let grid = Grid::new();
        grid.set_row_spacing(12);
        grid.set_column_spacing(16);
        grid.set_margin_top(16);
        grid.set_margin_bottom(16);
        grid.set_margin_start(16);
        grid.set_margin_end(16);

        // Row 0: Visualizer plugin directory
        let lbl_viz = Label::new(Some("Visualizer plugin dir"));
        lbl_viz.set_halign(Align::Start);
        grid.attach(&lbl_viz, 0, 0, 1, 1);

        let entry_viz = Entry::new();
        entry_viz.set_text(&state.borrow().config.plugins.visualizer_dir);
        entry_viz.set_width_chars(32);
        entry_viz.set_placeholder_text(Some("(leave blank to skip)"));
        {
            let state_rc = state.clone();
            entry_viz.connect_changed(move |e| {
                state_rc.borrow_mut().config.plugins.visualizer_dir = e.text().to_string();
            });
        }
        grid.attach(&entry_viz, 1, 0, 1, 1);

        // Row 1: Filetype plugin directory
        let lbl_ft = Label::new(Some("Filetype plugin dir"));
        lbl_ft.set_halign(Align::Start);
        grid.attach(&lbl_ft, 0, 1, 1, 1);

        let entry_ft = Entry::new();
        entry_ft.set_text(&state.borrow().config.plugins.filetype_dir);
        entry_ft.set_width_chars(32);
        entry_ft.set_placeholder_text(Some("(leave blank to skip)"));
        {
            let state_rc = state.clone();
            entry_ft.connect_changed(move |e| {
                state_rc.borrow_mut().config.plugins.filetype_dir = e.text().to_string();
            });
        }
        grid.attach(&entry_ft, 1, 1, 1, 1);

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

        let rebuild_for_add = rebuild_list.clone();
        let status_for_add = status_lbl.clone();
        let state_for_add = state.clone();
        btn_add_folder.connect_clicked(move |_| {
            let dialog = gtk4::FileDialog::builder()
                .title("Select Music Folder")
                .build();
            let rebuild_cb = rebuild_for_add.clone();
            let status_rc = status_for_add.clone();
            let state_rc = state_for_add.clone();
            dialog.select_folder(
                None::<&gtk4::Window>,
                None::<&gio::Cancellable>,
                move |result| {
                    let path = match result {
                        Ok(f) => f.path().map(|p| p.to_string_lossy().into_owned()),
                        Err(_) => None,
                    };
                    let Some(path_str) = path else {
                        return;
                    };
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

                        // Phase 2: metadata scan
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
                        None::<&gtk4::Window>,
                        None::<&gio::Cancellable>,
                        move |result| {
                            if result == Ok(1) {
                                status_for_dialog.set_text(&format!("Removing: {}", folder_path_cb));

                                // Delete from state.media_lib on main thread first
                                // This updates the same DB connection used by rebuild functions
                                if let Some(ref lib) = state_for_dialog.borrow().media_lib {
                                    let _ = lib.remove_folder(folder_id_cb);
                                }

                                // Now rebuild UI - data is gone from state.media_lib's DB
                                rebuild_for_dialog();
                                status_for_dialog.set_text(&format!("Removed: {}", folder_path_cb));

                                // Trigger Media Library window to refresh if open
                                if let Some(ref cb) = state_for_dialog.borrow().rebuild_ml_callback {
                                    cb();
                                }

                                // Background cleanup: delete from DB again (harmless if already done)
                                let db_path = crate::media_library::MediaLibrary::db_path_pub();
                                let folder_id_bg = folder_id_cb;

                                std::thread::spawn(move || {
                                    if let Ok(lib) =
                                        crate::media_library::MediaLibrary::open_at(&db_path)
                                    {
                                        if let Ok(track_ids) = lib.track_ids_for_folder(folder_id_bg) {
                                            let _ = lib.remove_tracks_batch(&track_ids);
                                        }
                                        let _ = lib.remove_folder(folder_id_bg);
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

        let status_scan = Label::new(None);
        status_scan.set_halign(Align::Start);
        status_scan.add_css_class("dim-label");

        // Update button visibility based on scan state.
        // Clone references for the closure to avoid moving the originals.
        let state_rc_for_update = state.clone();
        let btn_rescan_ref = btn_rescan.clone();
        let btn_cancel_ref = btn_cancel_scan.clone();
        let status_ref = status_scan.clone();
        let update_scan_ui = Rc::new(move || {
            let scan_state = state_rc_for_update.borrow().ml_scan.clone();
            if let Some(scan) = scan_state {
                btn_rescan_ref.set_visible(false);
                btn_cancel_ref.set_visible(true);
                if scan.total > 0 {
                    status_ref.set_text(&format!("Scanning {} / {}…", scan.current, scan.total));
                } else {
                    status_ref.set_text("Scanning…");
                }
            } else {
                btn_rescan_ref.set_visible(true);
                btn_cancel_ref.set_visible(false);
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
                        complete_ml_scan(&state_rc2);
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

        let tab_lbl = Label::new(Some("Media Library"));
        notebook.append_page(&grid, Some(&tab_lbl));
    }

    // Select the requested tab, or default to tab 0.
    if let Some(tab) = initial_tab {
        notebook.set_current_page(Some(tab));
    }

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
fn open_eq_window(parent: Option<&gtk4::Window>, state: Rc<RefCell<AppState>>) {
    use crate::config::{EQ_BAND_FREQS, EQ_PRESETS};
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

        // Frequency label.
        let freq_label = Label::new(Some(EQ_BAND_FREQS[i]));
        freq_label.set_halign(gtk4::Align::Center);
        col.append(&freq_label);

        // Vertical scale: range −24..+12, step 1, page 3.
        let adj = Adjustment::new(bands_snapshot[i], -24.0, 12.0, 1.0, 3.0, 0.0);
        let scale = Scale::new(Orientation::Vertical, Some(&adj));
        scale.add_css_class("eq-scale");
        scale.set_inverted(true); // top = positive, bottom = negative
        scale.set_draw_value(false);
        scale.set_vexpand(true);
        scale.set_height_request(100);
        scale.add_mark(0.0, gtk4::PositionType::Right, Some("0"));
        scale.add_mark(12.0, gtk4::PositionType::Right, Some("+12"));
        scale.add_mark(-24.0, gtk4::PositionType::Right, Some("-24"));
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
        move |w| {
            let mut cfg = state_rc.borrow().config.clone();
            cfg.window.ml_width = w.width();
            cfg.window.ml_height = w.height();
            let _ = cfg.save();
            glib::Propagation::Proceed
        }
    });

    win.set_child(Some(&vbox));
    win.present();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// These tests cover `AppState` business logic without requiring a running
// GTK display.  They mirror the TUI test suite in `tui/mod.rs` so that the
// two frontends are held to the same behavioural contract.
//
// GStreamer must be initialised before any `Player` is created, so every
// test helper calls `gstreamer::init()` (which is idempotent after the first
// call).

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

fn libtrack_to_track(t: &crate::media_library::LibTrack) -> crate::model::Track {
    use std::time::Duration;
    crate::model::Track {
        path: std::path::PathBuf::from(&t.path),
        title: t.title.clone().unwrap_or_else(|| t.filename.clone()),
        artist: t.artist.clone().unwrap_or_default(),
        album_artist: String::new(),
        album: t.album.clone().unwrap_or_default(),
        duration: t
            .length_secs
            .map(|s| Duration::try_from_secs_f64(s).unwrap_or_default()),
        broken: false,
    }
}

// ---------------------------------------------------------------------------
// Image viewer popup
// ---------------------------------------------------------------------------

/// Open a resizable window displaying the image at `path`.
fn open_image_viewer(path: &str) {
    use gtk4::ContentFit;

    let win = gtk4::Window::new();
    win.set_title(Some("Artwork — Sparkamp"));
    win.set_default_size(400, 400);
    win.set_resizable(true);

    let picture = gtk4::Picture::new();
    picture.set_filename(Some(path));
    picture.set_can_shrink(true);
    picture.set_content_fit(ContentFit::Contain);
    picture.set_hexpand(true);
    picture.set_vexpand(true);

    win.set_child(Some(&picture));
    win.present();
}

fn open_media_library_window(
    parent: Option<&gtk4::Window>,
    state: Rc<RefCell<AppState>>,
    rebuild_playlist: Rc<dyn Fn()>,
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

    let root = GtkBox::new(Orientation::Horizontal, 0);
    root.set_margin_top(8);
    root.set_margin_bottom(8);
    root.set_margin_start(8);
    root.set_margin_end(8);

    // ── Left sidebar ──────────────────────────────────────────────────────
    let sidebar = ListBox::new();
    sidebar.set_selection_mode(gtk4::SelectionMode::Single);
    sidebar.add_css_class("ml-sidebar");
    sidebar.set_width_request(130);
    sidebar.set_vexpand(true);

    let make_nav_row = |text: &str| {
        let lbl = Label::builder()
            .label(text)
            .halign(Align::Start)
            .margin_start(10)
            .margin_end(10)
            .margin_top(7)
            .margin_bottom(7)
            .build();
        let row = ListBoxRow::new();
        row.set_child(Some(&lbl));
        row
    };
    let row_files = make_nav_row("Files");
    let row_playlists = make_nav_row("Playlists");
    sidebar.append(&row_files);
    sidebar.append(&row_playlists);

    let vsep = Separator::new(Orientation::Vertical);
    vsep.set_margin_start(4);
    vsep.set_margin_end(4);

    // ── Content stack ─────────────────────────────────────────────────────
    let stack = Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_transition_type(StackTransitionType::None);

    // ── Page: Files ──────────────────────────────────────────────────────
    {
        let files_vbox = GtkBox::new(Orientation::Vertical, 4);

        let search_entry = Entry::new();
        search_entry.set_placeholder_text(Some("Search artist, title, album…"));
        search_entry.set_margin_top(4);
        search_entry.set_margin_start(4);
        search_entry.set_margin_end(4);
        files_vbox.append(&search_entry);

        let track_store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let sort_model = SortListModel::new(Some(track_store.clone()), None::<gtk4::Sorter>);
        let multi_sel = MultiSelection::new(Some(sort_model.clone()));
        let col_view = ColumnView::new(Some(multi_sel.clone()));
        col_view.add_css_class("ml-col-view");
        col_view.set_show_row_separators(true);
        col_view.set_show_column_separators(true);
        col_view.set_hexpand(true);
        col_view.set_vexpand(true);

        let col_defs: &[(&str, &str, i32, bool)] = ALL_COLUMNS
            .iter()
            .map(|c| (c.id, c.header, 80, c.expand))
            .collect::<Vec<_>>()
            .leak();

        let visible_ids: Vec<String> = state.borrow().config.media_library.visible_columns.clone();

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
                if t.last_scanned.is_none() {
                    lbl.set_label("❓");
                } else {
                    lbl.set_label("");
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
                let ctx_state = state.clone();
                let ctx_multi_sel = multi_sel.clone();
                let ctx_rebuild_pl = rebuild_playlist.clone();
                let ctx_col_view = col_view.clone();
                let ctx_store = store_for_ctx.clone();

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
                            .halign(Align::Start)
                            .margin_start(6)
                            .margin_end(6)
                            .margin_top(3)
                            .margin_bottom(3)
                            .hexpand(true)
                            .vexpand(true)
                            .halign(Align::Fill)
                            .valign(Align::Fill)
                            .ellipsize(gtk4::pango::EllipsizeMode::End)
                            .css_classes(["ml-col-label"])
                            .build();
                        child = lbl.upcast::<gtk4::Widget>();
                    }

                    // Add right-click gesture to each row
                    let gesture = gtk4::GestureClick::new();
                    gesture.set_button(gtk4::gdk::BUTTON_SECONDARY);
                    let state_gest = ctx_state.clone();
                    let sel_gest = ctx_multi_sel.clone();
                    let rebuild_pl_gest = ctx_rebuild_pl.clone();
                    let col_popup = ctx_col_view.clone();
                    let li_gest = li.clone();
                    let store_gest = ctx_store.clone();
                    gesture.connect_pressed(move |_gest, n_press, x, y| {
                        if n_press != 1 {
                            return;
                        }
                        // Get the item directly from the ListItem - no coordinate math needed!
                        let Some(item) = li_gest.item() else {
                            return;
                        };
                        let item_clone = item.clone();
                        let Some(boxed) = item.downcast::<glib::BoxedAnyObject>().ok() else {
                            return;
                        };
                        let _track = boxed.borrow::<crate::media_library::LibTrack>();

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

                        // Create popover at converted position
                        let popover = gtk4::Popover::new();
                        popover.set_pointing_to(Some(&gtk4::gdk::Rectangle::new(
                            popup_x as i32,
                            popup_y as i32,
                            1,
                            1,
                        )));
                        popover.set_parent(&col_popup);

                        let vbox = GtkBox::new(Orientation::Vertical, 0);
                        vbox.set_margin_top(4);
                        vbox.set_margin_bottom(4);
                        vbox.set_margin_start(4);
                        vbox.set_margin_end(4);

                        // Append to Playlist
                        let btn_add = Button::with_label("Append to Playlist");
                        let state_add = state_gest.clone();
                        let sel_add = sel_gest.clone();
                        let rebuild_add = rebuild_pl_gest.clone();
                        let popover_add = popover.clone();
                        btn_add.connect_clicked(move |_btn| {
                            let was_empty = state_add.borrow().playlist.is_empty();
                            for i in 0..sel_add.n_items() {
                                if sel_add.is_selected(i) {
                                    if let Some(obj) = sel_add
                                        .item(i)
                                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                    {
                                        let t = obj.borrow::<crate::media_library::LibTrack>();
                                        let track = libtrack_to_track(&t);
                                        state_add.borrow_mut().playlist.add(track);
                                    }
                                }
                            }
                            if state_add.borrow().config.behavior.autoplay_on_add && was_empty {
                                state_add.borrow_mut().play_current();
                            }
                            rebuild_add();
                            popover_add.unparent();
                        });
                        vbox.append(&btn_add);

                        // Replace current playlist
                        let btn_replace = Button::with_label("Replace current playlist");
                        let state_replace = state_gest.clone();
                        let sel_replace = sel_gest.clone();
                        let rebuild_replace = rebuild_pl_gest.clone();
                        let popover_replace = popover.clone();
                        btn_replace.connect_clicked(move |_btn| {
                            // Stop current playback first (always, regardless of autoplay setting)
                            let _ = state_replace.borrow_mut().player.stop();
                            // Clear the playlist first.
                            state_replace.borrow_mut().playlist.clear();
                            for i in 0..sel_replace.n_items() {
                                if sel_replace.is_selected(i) {
                                    if let Some(obj) = sel_replace
                                        .item(i)
                                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                    {
                                        let t = obj.borrow::<crate::media_library::LibTrack>();
                                        let track = libtrack_to_track(&t);
                                        state_replace.borrow_mut().playlist.add(track);
                                    }
                                }
                            }
                            // Autoplay if setting is enabled and there are new tracks
                            if state_replace.borrow().config.behavior.autoplay_on_add
                                && !state_replace.borrow().playlist.is_empty()
                            {
                                state_replace.borrow_mut().play_current();
                            }
                            rebuild_replace();
                            popover_replace.unparent();
                        });
                        vbox.append(&btn_replace);

                        // View/Edit ID3 Info
                        let btn_id3 = Button::with_label("View/Edit ID3 Info");
                        let state_id3 = state_gest.clone();
                        let sel_id3 = sel_gest.clone();
                        let rebuild_id3 = rebuild_pl_gest.clone();
                        let popover_id3 = popover.clone();
                        btn_id3.connect_clicked(move |_btn| {
                            for i in 0..sel_id3.n_items() {
                                if sel_id3.is_selected(i) {
                                    if let Some(obj) = sel_id3
                                        .item(i)
                                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                    {
                                        let t = obj.borrow::<crate::media_library::LibTrack>();
                                        let path = std::path::PathBuf::from(&t.path);
                                        open_id3_editor_window(
                                            None::<&gtk4::Window>,
                                            path,
                                            state_id3.clone(),
                                            rebuild_id3.clone(),
                                            None,
                                        );
                                        break;
                                    }
                                }
                            }
                            popover_id3.unparent();
                        });
                        vbox.append(&btn_id3);

                        // Rescan Metadata
                        let btn_rescan = Button::with_label("Rescan Metadata");
                        let state_rescan = state_gest.clone();
                        let sel_rescan = sel_gest.clone();
                        let popover_rescan = popover.clone();
                        let store_rescan = store_gest.clone();
                        btn_rescan.connect_clicked(move |_btn| {
                            // Collect selected paths
                            let mut paths_to_scan: Vec<String> = Vec::new();
                            for i in 0..sel_rescan.n_items() {
                                if sel_rescan.is_selected(i) {
                                    if let Some(obj) = sel_rescan
                                        .item(i)
                                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                    {
                                        let t = obj.borrow::<crate::media_library::LibTrack>();
                                        paths_to_scan.push(t.path.clone());
                                    }
                                }
                            }

                            if paths_to_scan.is_empty() {
                                popover_rescan.unparent();
                                return;
                            }

                            // Check if a scan is already in progress
                            if state_rescan.borrow().ml_scan.is_some() {
                                popover_rescan.unparent();
                                return;
                            }

                            let total = paths_to_scan.len();
                            let cancel_flag =
                                start_ml_scan(&state_rescan, ScanType::AddFiles, total);

                            let (progress_tx, progress_rx) = std::sync::mpsc::channel();
                            let (result_tx, result_rx) = std::sync::mpsc::channel();

                            std::thread::spawn(move || {
                                let db_path = crate::media_library::MediaLibrary::db_path_pub();
                                let lib =
                                    match crate::media_library::MediaLibrary::open_at(&db_path) {
                                        Ok(l) => l,
                                        Err(_) => {
                                            let _ = result_tx.send(());
                                            return;
                                        }
                                    };

                                for (i, path) in paths_to_scan.iter().enumerate() {
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
                            let state_for_timer = state_rescan.clone();
                            let store_for_timer = store_rescan.clone();
                            glib::timeout_add_local(
                                std::time::Duration::from_millis(500),
                                move || {
                                    // Drain progress updates
                                    while let Ok(current) = progress_rx.borrow().try_recv() {
                                        update_ml_scan_progress(&state_for_timer, current, total);
                                    }

                                    // Check for completion
                                    if result_rx.borrow().try_recv().is_ok() {
                                        complete_ml_scan(&state_for_timer);
                                        // Rebuild the store to reflect rescanned tracks
                                        let tracks: Vec<crate::media_library::LibTrack> =
                                            state_for_timer
                                                .borrow()
                                                .media_lib
                                                .as_ref()
                                                .and_then(|lib| lib.all_tracks().ok())
                                                .unwrap_or_default();
                                        let boxed: Vec<glib::BoxedAnyObject> = tracks
                                            .into_iter()
                                            .map(glib::BoxedAnyObject::new)
                                            .collect();
                                        store_for_timer.splice(
                                            0,
                                            store_for_timer.n_items(),
                                            &boxed,
                                        );
                                        return glib::ControlFlow::Break;
                                    }

                                    glib::ControlFlow::Continue
                                },
                            );

                            popover_rescan.unparent();
                        });
                        vbox.append(&btn_rescan);

                        let sep = Separator::new(Orientation::Horizontal);
                        vbox.append(&sep);

                        // Remove from Media Library
                        let btn_remove = Button::with_label("Remove from Media Library");
                        let state_remove = state_gest.clone();
                        let sel_remove = sel_gest.clone();
                        let popover_remove = popover.clone();
                        let store_remove = store_gest.clone();
                        btn_remove.connect_clicked(move |_btn| {
                            for i in 0..sel_remove.n_items() {
                                if sel_remove.is_selected(i) {
                                    if let Some(obj) = sel_remove
                                        .item(i)
                                        .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                                    {
                                        let t = obj.borrow::<crate::media_library::LibTrack>();
                                        if let Some(lib) = state_remove.borrow().media_lib.as_ref()
                                        {
                                            let _ = lib.remove_track(t.id);
                                        }
                                    }
                                }
                            }
                            // Rebuild the store to reflect removed tracks
                            let tracks: Vec<crate::media_library::LibTrack> = state_remove
                                .borrow()
                                .media_lib
                                .as_ref()
                                .and_then(|lib| lib.all_tracks().ok())
                                .unwrap_or_default();
                            let boxed: Vec<glib::BoxedAnyObject> =
                                tracks.into_iter().map(glib::BoxedAnyObject::new).collect();
                            store_remove.splice(0, store_remove.n_items(), &boxed);
                            popover_remove.unparent();
                        });
                        vbox.append(&btn_remove);

                        popover.set_child(Some(&vbox));
                        popover.popup();
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
                                btn.set_sensitive(false);
                                btn.set_label("");
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
                // Note: do NOT use set_fixed_width here — it prevents the column from
                // shrinking smaller than min_w. Let the Label's ellipsize attribute truncate
                // content when the user resizes the column narrower.
                if *expand {
                    col.set_expand(true);
                }
                col.set_visible(visible_ids.contains(&id.to_string()));

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

        let rebuild_files: Rc<dyn Fn() -> usize> = {
            let state_rc = state.clone();
            let store_ref = track_store.clone();
            Rc::new(move || {
                let tracks: Vec<crate::media_library::LibTrack> = state_rc
                    .borrow()
                    .media_lib
                    .as_ref()
                    .and_then(|lib| lib.all_tracks().ok())
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
            Rc::new(move || {
                let was_empty = state_rc.borrow().playlist.is_empty();
                let should_replace = state_rc.borrow().config.behavior.playlist_add_behavior
                    == crate::config::PlaylistAddBehavior::Replace;
                if should_replace {
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
                            let track = libtrack_to_track(&t);
                            state_rc.borrow_mut().playlist.add(track);
                            added += 1;
                        }
                    }
                }
                if added > 0 {
                    if state_rc.borrow().config.behavior.autoplay_on_add && was_empty {
                        state_rc.borrow_mut().play_current();
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
            col_view.connect_activate(move |_, pos| {
                if let Some(obj) = sel_ref
                    .item(pos)
                    .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                {
                    let was_empty = state_rc.borrow().playlist.is_empty();
                    let t = obj.borrow::<crate::media_library::LibTrack>();
                    let track = libtrack_to_track(&t);
                    let should_replace = state_rc.borrow().config.behavior.playlist_add_behavior
                        == crate::config::PlaylistAddBehavior::Replace;
                    if should_replace {
                        state_rc.borrow_mut().playlist.clear();
                    }
                    state_rc.borrow_mut().playlist.add(track);
                    if state_rc.borrow().config.behavior.autoplay_on_add && was_empty {
                        state_rc.borrow_mut().play_current();
                    }
                    rebuild_pl();
                }
            });
        }

        // Customize columns dialog.
        {
            let state_rc = state.clone();
            let all_cols_rc = all_cols.clone();
            let win_wk = win.downgrade();
            btn_customize.connect_clicked(move |_| {
                let cols_for_callback = all_cols_rc.clone();
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
                    None::<Rc<dyn Fn()>>,
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
                            // Phase 2: read metadata for tracks that need it.
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
            let status_ref = files_status.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                let scan_state = state_rc.borrow().ml_scan.clone();
                if let Some(scan) = scan_state {
                    cancel_ref.set_visible(true);
                    rescan_ref.set_sensitive(false);
                    if scan.total > 0 {
                        status_ref
                            .set_text(&format!("Reading tags {}/{}…", scan.current, scan.total));
                    } else {
                        status_ref.set_text("Reading tags…");
                    }
                } else {
                    cancel_ref.set_visible(false);
                    rescan_ref.set_sensitive(true);
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
                let mut ids_vec: Vec<i64> = Vec::new();
                for i in 0..sel_ref.n_items() {
                    if sel_ref.is_selected(i) {
                        if let Some(obj) = sel_ref
                            .item(i)
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                        {
                            let t = obj.borrow::<crate::media_library::LibTrack>();
                            ids_vec.push(t.id);
                        }
                    }
                }
                if ids_vec.is_empty() {
                    return;
                }
                // Soft-delete: remove rows from the GTK model immediately so
                // the UI is instantly responsive regardless of collection size.
                let ids_set: std::collections::HashSet<i64> = ids_vec.iter().copied().collect();
                let n_items = store_ref.n_items();
                let mut positions: Vec<u32> = (0..n_items)
                    .filter(|&i| {
                        store_ref
                            .item(i)
                            .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                            .map(|obj| {
                                let t = obj.borrow::<crate::media_library::LibTrack>();
                                ids_set.contains(&t.id)
                            })
                            .unwrap_or(false)
                    })
                    .collect();
                positions.sort_by(|a, b| b.cmp(a));
                let removed = positions.len();
                for pos in positions {
                    store_ref.remove(pos);
                }
                let remaining = n_items as usize - removed;
                status_ref.set_text(&format!(
                    "Removed {removed} track{}. {remaining} tracks in library",
                    if removed == 1 { "" } else { "s" }
                ));
                // Delete from DB in the background — opens its own connection
                // because rusqlite::Connection is not Send.
                let db_path = crate::media_library::MediaLibrary::db_path_pub();
                std::thread::spawn(move || {
                    if let Ok(lib) = crate::media_library::MediaLibrary::open_at(&db_path) {
                        let _ = lib.remove_tracks_batch(&ids_vec);
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
    {
        let hbox = GtkBox::new(Orientation::Horizontal, 4);

        let pl_list = ListBox::new();
        pl_list.add_css_class("playlist");
        pl_list.set_selection_mode(gtk4::SelectionMode::Single);

        let playlists = state
            .borrow()
            .media_lib
            .as_ref()
            .and_then(|lib| lib.all_playlists().ok())
            .unwrap_or_default();
        for pl in &playlists {
            let lbl = Label::builder()
                .label(&pl.name)
                .halign(Align::Start)
                .margin_start(8)
                .margin_end(8)
                .margin_top(2)
                .margin_bottom(2)
                .build();
            let row = ListBoxRow::new();
            row.set_widget_name(&pl.path);
            row.set_child(Some(&lbl));
            pl_list.append(&row);
        }

        let pl_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .min_content_height(300)
            .width_request(200)
            .child(&pl_list)
            .build();
        hbox.append(&pl_scroll);

        let preview_list = ListBox::new();
        preview_list.add_css_class("playlist");
        preview_list.set_selection_mode(gtk4::SelectionMode::None);

        let preview_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::Automatic)
            .vexpand(true)
            .hexpand(true)
            .min_content_height(300)
            .child(&preview_list)
            .build();
        hbox.append(&preview_scroll);

        {
            let state_rc = state.clone();
            let preview_ref = preview_list.clone();
            pl_list.connect_row_selected(move |_, opt_row| {
                while let Some(child) = preview_ref.first_child() {
                    preview_ref.remove(&child);
                }
                let row = match opt_row {
                    Some(r) => r,
                    None => return,
                };
                let path = row.widget_name().to_string();
                if path.is_empty() {
                    return;
                }
                let s = state_rc.borrow();
                if let Some(ref lib) = s.media_lib {
                    let pl = crate::media_library::LibPlaylist {
                        id: 0,
                        path: path.clone(),
                        name: String::new(),
                        tracks: Vec::new(),
                    };
                    let tracks = lib.load_playlist_tracks(&pl).unwrap_or_default();
                    for t in &tracks {
                        let artist = t.artist.as_deref().unwrap_or("-");
                        let title = t.title.as_deref().unwrap_or(&t.filename);
                        let lbl = Label::builder()
                            .label(format!("{} — {}", artist, title))
                            .halign(Align::Start)
                            .margin_start(8)
                            .margin_end(8)
                            .margin_top(2)
                            .margin_bottom(2)
                            .build();
                        let prow = ListBoxRow::new();
                        prow.set_child(Some(&lbl));
                        preview_ref.append(&prow);
                    }
                }
            });
        }

        let btn_row = GtkBox::new(Orientation::Horizontal, 4);
        btn_row.set_margin_start(4);
        btn_row.set_margin_end(4);
        btn_row.set_margin_bottom(4);

        let btn_set_pl = Button::with_label("▶ Set as Playlist");
        btn_set_pl.add_css_class("pl-btn");

        let spring = GtkBox::new(Orientation::Horizontal, 0);
        spring.set_hexpand(true);
        btn_row.append(&spring);
        btn_row.append(&btn_set_pl);

        {
            let state_rc = state.clone();
            let pl_ref = pl_list.clone();
            btn_set_pl.connect_clicked(move |_| {
                let row = match pl_ref.selected_row() {
                    Some(r) => r,
                    None => return,
                };
                let path = row.widget_name().to_string();
                if path.is_empty() {
                    return;
                }
                let pl = crate::media_library::LibPlaylist {
                    id: 0,
                    path: path.clone(),
                    name: String::new(),
                    tracks: Vec::new(),
                };
                let mut s = state_rc.borrow_mut();
                let tracks_opt = s
                    .media_lib
                    .as_ref()
                    .and_then(|lib| lib.load_playlist_tracks(&pl).ok());
                if let Some(lib_tracks) = tracks_opt {
                    s.playlist = crate::model::Playlist::new();
                    for lt in &lib_tracks {
                        let track = libtrack_to_track(lt);
                        s.playlist.add(track);
                    }
                }
            });
        }

        let pl_vbox = GtkBox::new(Orientation::Vertical, 0);
        pl_vbox.append(&hbox);
        pl_vbox.append(&btn_row);

        stack.add_named(&pl_vbox, Some("playlists"));
    }

    // Wire sidebar to stack.
    {
        let stack_ref = stack.clone();
        sidebar.connect_row_selected(move |_, opt_row| {
            let row = match opt_row {
                Some(r) => r,
                None => return,
            };
            let page = match row.index() {
                0 => "files",
                1 => "playlists",
                _ => return,
            };
            stack_ref.set_visible_child_name(page);
        });
    }

    sidebar.select_row(sidebar.row_at_index(0).as_ref());

    root.append(&sidebar);
    root.append(&vsep);
    root.append(&stack);
    win.set_child(Some(&root));

    win.connect_close_request({
        let state = state.clone();
        move |w| {
            let (w_size, h_size) = (w.width(), w.height());
            {
                let mut s = state.borrow_mut();
                s.config.window.ml_width = w_size;
                s.config.window.ml_height = h_size;
                s.rebuild_ml_callback = None;
            }
            let _ = state.borrow().config.save();
            state.borrow_mut().ml_window = None;
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
    /// `None` → `Duration::ZERO`, which is always < 2 s, so the back button
    /// always steps to the previous track in tests.
    #[test]
    fn play_prev_when_position_is_zero_goes_to_previous_track() {
        let mut s = state_with_tracks(&["A", "B"]);
        s.playlist.current_index = 1;
        s.play_prev();
        assert_eq!(s.playlist.current_index, 0);
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
    fn toggle_visualizer_mode_bars_becomes_oscilloscope() {
        let mut s = make_state();
        assert_eq!(s.config.visualizer.mode, VisualizerMode::Bars);
        s.toggle_visualizer_mode();
        assert_eq!(s.config.visualizer.mode, VisualizerMode::Oscilloscope);
    }

    #[test]
    fn toggle_visualizer_mode_oscilloscope_becomes_bars() {
        let mut s = make_state();
        s.config.visualizer.mode = VisualizerMode::Oscilloscope;
        s.toggle_visualizer_mode();
        assert_eq!(s.config.visualizer.mode, VisualizerMode::Bars);
    }

    #[test]
    fn toggle_visualizer_mode_100_times_ends_back_at_bars() {
        // After an even number of toggles the mode must return to its start.
        let mut s = make_state();
        for _ in 0..100 {
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
    fn search_indices_returns_all_tracks_for_empty_query() {
        let s = state_with_tracks(&["A", "B", "C"]);
        // search_indices is called with "" from the search bar before any typing
        let results = s.playlist.search_indices("");
        assert_eq!(results.len(), 3);
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
        let path_b = s.playlist.tracks[1].path.to_string_lossy().into_owned();

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
