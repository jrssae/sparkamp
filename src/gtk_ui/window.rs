//! GTK4 main window — widget layout, callbacks, and application logic.
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
use gtk4::prelude::*;
use gtk4::{
    gdk, gdk_pixbuf, gio, glib,
    Adjustment, Align, Application, ApplicationWindow,
    Box as GtkBox, Button, DrawingArea, DragSource, DropTarget,
    EventControllerKey, GestureClick, Image, Label, ListBox, ListBoxRow,
    Orientation, PolicyType, Scale, ScrolledWindow,
    Separator,
};
use glib::ControlFlow;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use crate::{
    config::{Config, VisualizerMode},
    duration_cache::DurationCache,
    duration_probe,
    engine::{BusEvent, Player, PlayerState},
    model::{fmt_duration, Playlist, Track},
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
        Ok(AppState {
            player,
            playlist,
            config,
            pending_seek: None,
            last_duration: None,
            mute_pending: None,
            duration_cache: DurationCache::load(),
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

    /// Advance `current_index` by one and play the next track.
    ///
    /// Returns `Some(display_name)` if there was a next track, or `None`
    /// if we are already at the last track (no wrap-around).
    fn play_next(&mut self) -> Option<String> {
        self.playlist.next()?;
        self.play_current()
    }

    /// Implement the PRD "back button" behaviour.
    ///
    /// - ≥ 2 s elapsed → restart the current track from the beginning.
    /// - < 2 s elapsed → step to the previous track.
    ///
    /// Returns `Some(display_name)` of the track that will now play.
    fn play_prev(&mut self) -> Option<String> {
        let pos = self.player.position().unwrap_or(Duration::ZERO);
        if pos.as_secs() >= 2 {
            self.play_current()
        } else {
            self.playlist.previous();
            self.play_current()
        }
    }

    /// Toggle the visualizer mode between `Bars` and `Oscilloscope`.
    fn toggle_visualizer_mode(&mut self) {
        self.config.visualizer.mode = match self.config.visualizer.mode {
            VisualizerMode::Bars         => VisualizerMode::Oscilloscope,
            VisualizerMode::Oscilloscope => VisualizerMode::Bars,
        };
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
        let dur = match self.player.duration()
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

    fn apply_probed_duration(&mut self, path: &std::path::PathBuf, dur: Duration) {
        for track in &mut self.playlist.tracks {
            if &track.path == path {
                track.duration = Some(dur);
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
        let dur = self.player.duration()
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
            // Recursively collect all audio files beneath the directory.
            let files = Playlist::collect_audio_files(path);
            let total = files.len();
            if total == 0 {
                return Err(format!(
                    "No audio files found in '{}'",
                    path.display()
                ));
            }
            let mut added = 0usize;
            for file in files {
                if let Ok(track) = Track::from_path(&file) {
                    self.playlist.add(track);
                    added += 1;
                }
            }
            Ok(format!("Added {} / {} files from '{}'", added, total, path.display()))
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
/// Raw CSS embedded at compile time; accent colour is injected at runtime.
const DARK_CSS_RAW:  &str = include_str!("style_dark.css");
const LIGHT_CSS_RAW: &str = include_str!("style_light.css");

/// Read the user's GNOME accent-colour choice from gsettings and return
/// the matching hex string.  Falls back to GNOME's default blue when
/// gsettings is unavailable or the value is unrecognised.
fn accent_hex() -> &'static str {
    let output = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "accent-color"])
        .output();
    let name = output
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().trim_matches('\'').to_string())
        .unwrap_or_default();
    match name.as_str() {
        "blue"   => "#3584e4",
        "teal"   => "#2190a4",
        "green"  => "#3a944a",
        "yellow" => "#c88800",
        "orange" => "#ed5b00",
        "red"    => "#e62d42",
        "pink"   => "#d56199",
        "purple" => "#9141ac",
        "slate"  => "#6f8396",
        _        => "#3584e4",   // GNOME default blue
    }
}

/// Prepend @define-color declarations so the accent colour is always
/// resolved, regardless of whether the GTK theme exports it.
fn make_css(raw: &str, accent: &str) -> String {
    format!(
        "@define-color accent_bg_color {accent};\n\
         @define-color accent_fg_color #ffffff;\n\
         {raw}"
    )
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
    let rowstride  = pb.rowstride() as usize;
    let width      = pb.width() as usize;
    let height     = pb.height() as usize;
    // SAFETY: we own the only reference to this freshly-copied pixbuf.
    let pixels = unsafe { pb.pixels() };
    for row in 0..height {
        for col in 0..width {
            let off = row * rowstride + col * n_channels;
            pixels[off]     = 255 - pixels[off];       // R
            pixels[off + 1] = 255 - pixels[off + 1];   // G
            pixels[off + 2] = 255 - pixels[off + 2];   // B
            // pixels[off + 3] is alpha — left unchanged
        }
    }
    pb
}

pub fn build(app: &Application, playlist: Playlist, config: Config) {
    // ── CSS theme ─────────────────────────────────────────────────────────────
    // Inject the accent colour at startup so @accent_bg_color always resolves.
    let accent       = accent_hex();
    let dark_css_rc  = Rc::new(make_css(DARK_CSS_RAW,  accent));
    let light_css_rc = Rc::new(make_css(LIGHT_CSS_RAW, accent));

    let provider = Rc::new(gtk4::CssProvider::new());
    provider.load_from_data(&**dark_css_rc);
    gtk4::style_context_add_provider_for_display(
        &gdk::Display::default().expect("No display"),
        &*provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let dark_mode = Rc::new(Cell::new(true));

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
    let (probe_tx, probe_rx) =
        std::sync::mpsc::channel::<(std::path::PathBuf, Duration)>();
    let (broken_tx, broken_rx) =
        std::sync::mpsc::channel::<std::path::PathBuf>();

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
    let init_playlist_visible  = state.borrow().config.window.playlist_visible;
    let mut init_player_width  = state.borrow().config.window.player_width;
    let mut init_player_height = state.borrow().config.window.player_height;
    let mut init_pl_width      = state.borrow().config.window.playlist_width;
    let mut init_pl_height     = state.borrow().config.window.playlist_height;

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
                init_player_width  = WindowConfig::default_player_width();
                init_player_height = WindowConfig::default_player_height();
            }
            if init_pl_width > max_w || init_pl_height > max_h {
                init_pl_width  = WindowConfig::default_playlist_width();
                init_pl_height = WindowConfig::default_playlist_height();
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
    let marquee_tick   = Rc::new(Cell::new(0u32));

    // Helper: called whenever the playing track changes.  Updates the marquee
    // state and resets the scroll position to the beginning.
    let set_track: Rc<dyn Fn(&str)> = {
        let chars_ref = marquee_chars.clone();
        let off_ref   = marquee_offset.clone();
        let tick_ref  = marquee_tick.clone();
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
        click.connect_released(move |_, _, _, _| { show_rem.set(!show_rem.get()); });
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
        .xalign(0.0)           // text left-aligned within the full-width label
        .hexpand(true)
        .margin_start(8)       // aligns with the VOL label start in the row below
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
        .margin_start(12)      // indent to visually separate from frame edge
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

    // ── PL and info buttons (created early so they can live in the vol row) ──
    let btn_pl   = Button::with_label("PL");
    btn_pl.add_css_class("mode-btn");
    let btn_info = Button::with_label("ℹ");
    btn_info.add_css_class("mode-btn");
    btn_info.set_tooltip_text(Some("Keyboard shortcuts"));

    // ── Vol row: [VOL] [vol_bar(half-width)] [spring] [ℹ] [PL] ─────────────
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
    let transport = GtkBox::new(Orientation::Horizontal, 4);
    transport.set_hexpand(true);
    transport.set_margin_start(8);
    transport.set_margin_end(8);
    transport.set_margin_top(8);
    transport.set_margin_bottom(8);

    let btn_prev  = Button::with_label("⏮");
    let btn_play  = Button::with_label("▶");
    let btn_pause = Button::with_label("⏸");
    let btn_stop  = Button::with_label("⏹");
    let btn_next  = Button::with_label("⏭");

    for btn in [&btn_prev, &btn_play, &btn_pause, &btn_stop, &btn_next] {
        btn.add_css_class("transport");
    }
    btn_play.add_css_class("transport-play");

    // Load logo at ~42 px (50 % larger than the transport buttons).
    // If the PNG fails to load (e.g. asset missing), the image slot stays blank.
    const LOGO_PX: i32 = 42;
    let logo_light = load_logo_pixbuf(LOGO_PX);
    let logo_dark  = logo_light.as_ref().map(|pb| invert_pixbuf(pb));
    let logo_img = Image::new();
    logo_img.set_valign(Align::Center);
    logo_img.set_pixel_size(LOGO_PX);
    // Extra right-side padding so the logo's right edge aligns with the PL
    // button and progress bar end (both sit at 8px from the window edge; the
    // transport box itself already has margin_end(8)).
    logo_img.set_margin_end(8);
    // Initial theme: if dark_mode is set apply the inverted version.
    if dark_mode.get() {
        if let Some(ref pb) = logo_dark  { logo_img.set_from_pixbuf(Some(pb)); }
    } else {
        if let Some(ref pb) = logo_light { logo_img.set_from_pixbuf(Some(pb)); }
    }
    // Wrap logo pixbufs in Rc so the theme-toggle closure can reach them.
    let logo_light_rc = Rc::new(logo_light);
    let logo_dark_rc  = Rc::new(logo_dark);

    // Spring between buttons and logo.
    let transport_spring = GtkBox::new(Orientation::Horizontal, 0);
    transport_spring.set_hexpand(true);

    transport.append(&btn_prev);
    transport.append(&btn_play);
    transport.append(&btn_pause);
    transport.append(&btn_stop);
    transport.append(&btn_next);
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

    window.set_child(Some(&root));

    // ── Right-click on the player body → toggle dark / light theme ───────────
    {
        let provider_rc  = provider.clone();
        let dark_ref     = dark_mode.clone();
        let dark_css     = dark_css_rc.clone();
        let light_css    = light_css_rc.clone();
        let logo_img_rc  = logo_img.clone();
        let logo_light_t = logo_light_rc.clone();
        let logo_dark_t  = logo_dark_rc.clone();
        let rclick = GestureClick::new();
        rclick.set_button(3);
        rclick.connect_released(move |_, _, _, _| {
            let now_dark = !dark_ref.get();
            dark_ref.set(now_dark);
            provider_rc.load_from_data(if now_dark { &**dark_css } else { &**light_css });
            // Swap logo to match the new theme.
            if now_dark {
                if let Some(ref pb) = *logo_dark_t  { logo_img_rc.set_from_pixbuf(Some(pb)); }
            } else {
                if let Some(ref pb) = *logo_light_t { logo_img_rc.set_from_pixbuf(Some(pb)); }
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
    let btn_add_files  = Button::with_label("+ Files");   // one or more audio files
    let btn_add_dir    = Button::with_label("+ Folder");  // directory (recursive scan)
    let btn_remove     = Button::with_label("✕ Remove");  // remove selected row(s)
    let btn_clear_all  = Button::with_label("✕ All");     // clear entire playlist

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

    // ── Playlist ListBox: multi-select, with drag-and-drop reordering ──────────
    // `SelectionMode::Multiple` lets the user select a contiguous or
    // discontiguous set of rows and remove them all in one Remove click.
    // (Search/jump lives in its own window, not here — see jump_win below.)
    let playlist_box = ListBox::new();
    playlist_box.add_css_class("playlist");
    playlist_box.set_selection_mode(gtk4::SelectionMode::Multiple);

    let pl_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .min_content_height(350)
        .child(&playlist_box)
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
                s.config.window.playlist_width  = w;
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

    // rebuild_playlist — clear and recreate all ListBox rows.
    //
    // Called after any playlist modification.  Attaches a DragSource to each
    // row so that rows can be dragged to a new position.  Also updates the
    // pl_count_label so the playlist window header stays current.
    //
    // Borrow discipline: we extract all data from `state` into local Vecs
    // while holding the borrow, then drop the borrow before touching GTK
    // widgets (widget operations can trigger callbacks that need to borrow
    // state themselves — overlapping borrows would panic the RefCell).
    let rebuild_playlist = {
        let state         = state.clone();
        let playlist_box  = playlist_box.clone();
        let pl_count_label = pl_count_label.clone();
        Rc::new(move || {
            // ── Collect track data while holding the borrow ────────────────
            let (rows_data, current_idx, n_tracks) = {
                let s = state.borrow();
                // (label, duration, is_current, is_broken)
                let data: Vec<(String, String, bool, bool)> = s.playlist.tracks
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let label      = format!("{:2}. {}", i + 1, t.display_name());
                        let dur        = fmt_duration(t.duration);
                        let is_current = i == s.playlist.current_index;
                        (label, dur, is_current, t.broken)
                    })
                    .collect();
                (data, s.playlist.current_index, s.playlist.len())
            };
            // Borrow dropped here — safe to call GTK methods now.

            // ── Update the count label ─────────────────────────────────────
            pl_count_label.set_label(&format!(
                "Playlist — {} track{}",
                n_tracks,
                if n_tracks == 1 { "" } else { "s" }
            ));

            // ── Rebuild the ListBox rows ───────────────────────────────────
            while let Some(child) = playlist_box.first_child() {
                playlist_box.remove(&child);
            }

            for (i, (label_text, dur_text, is_current, is_broken)) in rows_data.iter().enumerate() {
                let row     = ListBoxRow::new();
                let row_box = GtkBox::new(Orientation::Horizontal, 0);

                // Prefix broken rows with ⚠ so the user can see which files
                // could not be found / played.
                let display_label = if *is_broken {
                    format!("⚠ {}", label_text.trim_start())
                } else {
                    label_text.clone()
                };
                let lbl = Label::builder()
                    .label(&display_label)
                    .halign(Align::Start)
                    .hexpand(true)
                    .ellipsize(gtk4::pango::EllipsizeMode::End)
                    .build();
                let dur_lbl = Label::builder()
                    .label(dur_text)
                    .halign(Align::End)
                    .css_classes(["pl-dur-label"])
                    .build();
                row_box.append(&lbl);
                row_box.append(&dur_lbl);
                row.set_child(Some(&row_box));
                if *is_current {
                    row.add_css_class("playing");
                }
                if *is_broken {
                    row.add_css_class("broken");
                }

                // ── Drag source for reordering ─────────────────────────────
                // Each row carries its 0-based index as the drag payload so
                // the DropTarget can call move_track(src, dst).
                let idx = i as i32;
                let drag_src = DragSource::new();
                drag_src.set_actions(gdk::DragAction::MOVE);

                // Prepare: return a ContentProvider wrapping the row index.
                drag_src.connect_prepare(move |src, _, _| {
                    src.set_state(gtk4::EventSequenceState::Claimed);
                    Some(gdk::ContentProvider::for_value(&idx.to_value()))
                });

                // Dim the row while it is being dragged.
                let row_weak = row.downgrade();
                drag_src.connect_drag_begin(move |_, _| {
                    if let Some(r) = row_weak.upgrade() {
                        r.add_css_class("dragging");
                    }
                });

                // Remove the dim class when the drag ends.
                let row_weak2 = row.downgrade();
                drag_src.connect_drag_end(move |_, _, _| {
                    if let Some(r) = row_weak2.upgrade() {
                        r.remove_css_class("dragging");
                    }
                });

                row.add_controller(drag_src);
                playlist_box.append(&row);
            }

            // Scroll the currently playing track into view.
            if n_tracks > 0 {
                if let Some(row) = playlist_box.row_at_index(current_idx as i32) {
                    playlist_box.select_row(Some(&row));
                }
            }
        })
    };

    // play_and_update — play the current track and refresh the UI labels.
    //
    // All "start playing" paths (buttons, keyboard, auto-advance) funnel
    // through here so the marquee and playlist stay in sync.  Label text is
    // NOT set directly here; the 100 ms tick loop renders the marquee window
    // each frame so the scrolling starts immediately after track change.
    let play_and_update = {
        let state            = state.clone();
        let set_track        = set_track.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        Rc::new(move || {
            let result = { state.borrow_mut().play_current() };
            if let Some(display) = result {
                set_track(&display);
                rebuild_playlist();
            }
        })
    };

    // remove_selected — remove every currently selected playlist row.
    //
    // Indices are sorted highest-first before removal so that earlier removes
    // do not shift the positions of later ones.  Does not delete files from
    // disk; only removes the entries from the in-memory playlist.
    let remove_selected = {
        let state           = state.clone();
        let playlist_box_rm = playlist_box.clone();
        let rebuild_rm      = rebuild_playlist.clone();
        let set_track_rm    = set_track.clone();
        Rc::new(move || {
            // Collect selected indices before modifying the playlist.
            let mut indices: Vec<usize> = playlist_box_rm
                .selected_rows()
                .iter()
                .map(|r| r.index() as usize)
                .collect();
            if indices.is_empty() { return; }

            // Highest first so earlier removes don't invalidate later indices.
            indices.sort_unstable_by(|a, b| b.cmp(a));

            let mut last_nowplaying: Option<String> = None;
            for idx in indices {
                // remove_track handles current_index adjustment and auto-advance.
                if let Some(display) = { state.borrow_mut().remove_track(idx) } {
                    last_nowplaying = Some(display);
                }
            }
            // If auto-advance happened, push the new track into the marquee.
            if let Some(display) = last_nowplaying {
                set_track_rm(&display);
            }
            rebuild_rm();
        })
    };

    // ── Initial state ─────────────────────────────────────────────────────────
    // Single click selects (highlights) a row; double click or Enter plays it.
    // GTK's default for ListBox is activate-on-single-click = true, which would
    // fire row-activated on the first click and immediately start playback.
    playlist_box.set_activate_on_single_click(false);

    rebuild_playlist();
    {
        let s = state.borrow();
        if let Some(t) = s.playlist.current() {
            set_track(&t.display_name());
        }
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Drag-and-drop: DropTarget on the ListBox (row reorder)
    // ══════════════════════════════════════════════════════════════════════════
    //
    // Set up ONCE here (not inside rebuild_playlist) so the target persists
    // across rebuilds.  The per-row DragSource is re-created each rebuild.
    //
    // A live drop-indicator line (thin accent border on `drop-target` class) is
    // shown on the row currently under the cursor so the user can see exactly
    // where the dragged entry will land.
    {
        let drop_tgt = DropTarget::new(i32::static_type(), gdk::DragAction::MOVE);
        let state_dnd        = state.clone();
        let rebuild_dnd      = rebuild_playlist.clone();
        let playlist_box_dnd = playlist_box.clone();
        // Track which row currently carries the drop-target indicator (-1 = none).
        let hover_idx = Rc::new(Cell::new(-1i32));

        // Motion: move the indicator line to the row under the cursor.
        drop_tgt.connect_motion({
            let pb = playlist_box_dnd.clone();
            let hi = hover_idx.clone();
            move |_, _x, y| {
                let prev = hi.get();
                if prev >= 0 {
                    if let Some(r) = pb.row_at_index(prev) {
                        r.remove_css_class("drop-target");
                    }
                }
                if let Some(row) = pb.row_at_y(y as i32) {
                    let idx = row.index();
                    row.add_css_class("drop-target");
                    hi.set(idx);
                } else {
                    hi.set(-1);
                }
                gdk::DragAction::MOVE
            }
        });

        // Leave: clear the indicator when the drag exits the playlist area.
        drop_tgt.connect_leave({
            let pb = playlist_box_dnd.clone();
            let hi = hover_idx.clone();
            move |_| {
                let prev = hi.get();
                if prev >= 0 {
                    if let Some(r) = pb.row_at_index(prev) {
                        r.remove_css_class("drop-target");
                    }
                    hi.set(-1);
                }
            }
        });

        drop_tgt.connect_drop(move |_, value, _x, y| {
            // The drag payload is the source row index packed as i32.
            if let Ok(src_idx) = value.get::<i32>() {
                let src_idx = src_idx as usize;
                // Determine destination row from the drop y-coordinate.
                if let Some(dst_row) = playlist_box_dnd.row_at_y(y as i32) {
                    let dst_idx = dst_row.index() as usize;
                    if src_idx != dst_idx {
                        // move_track keeps current_index pointing at the same
                        // logical track even when the row order changes.
                        state_dnd.borrow_mut().playlist.move_track(src_idx, dst_idx);
                        rebuild_dnd(); // rebuilds all rows, indicator gone
                    }
                }
            }
            true // signal: drop was handled
        });

        playlist_box.add_controller(drop_tgt);
    }

    // ── Drop target: accept files dragged from an external file manager ───────
    // Handles gdk::FileList drops (the standard type produced by GNOME Files
    // and most GTK4-aware file managers).  Files are appended to the playlist;
    // directories are scanned recursively.  Attached to the ScrolledWindow so
    // the full visible playlist area is a valid drop zone.
    {
        let file_drop  = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let state_fd    = state.clone();
        let rebuild_fd  = rebuild_playlist.clone();
        let status_fd   = pl_status_label.clone();
        let probe_tx_fd  = probe_tx.clone();
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
                PlayerState::Paused  => { let _ = state.borrow_mut().player.toggle_pause(); }
                PlayerState::Playing => {}
            }
        }
    });

    // ⏸ Pause / resume toggle.
    btn_pause.connect_clicked({
        let state = state.clone();
        move |_| { let _ = state.borrow_mut().player.toggle_pause(); }
    });

    // ⏹ Stop.
    btn_stop.connect_clicked({
        let state    = state.clone();
        let seek_bar = seek_bar.clone();
        move |_| {
            let _ = state.borrow_mut().player.stop();
            seek_bar.set_value(0.0);
        }
    });

    // ⏭ Next track.
    btn_next.connect_clicked({
        let state            = state.clone();
        let set_track        = set_track.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        move |_| {
            let result = { state.borrow_mut().play_next() };
            if let Some(display) = result {
                set_track(&display);
                rebuild_playlist();
            }
        }
    });

    // ⏮ Previous / restart (PRD back-button logic).
    btn_prev.connect_clicked({
        let state            = state.clone();
        let set_track        = set_track.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        move |_| {
            let result = { state.borrow_mut().play_prev() };
            if let Some(display) = result {
                set_track(&display);
                rebuild_playlist();
            }
        }
    });

    // PL — toggle the playlist window.
    btn_pl.connect_clicked({
        let playlist_win = playlist_win.clone();
        move |_| {
            playlist_win.set_visible(!playlist_win.is_visible());
        }
    });

    // ℹ Info button — connected after handle_key is defined (see below).

    // ══════════════════════════════════════════════════════════════════════════
    // Playlist ListBox interactions
    // ══════════════════════════════════════════════════════════════════════════

    // Double-click / Enter on a row: jump to that track and play it.
    playlist_box.connect_row_activated({
        let state            = state.clone();
        let play_and_update  = play_and_update.clone();
        move |_, row| {
            state.borrow_mut().playlist.jump_to(row.index() as usize);
            play_and_update();
        }
    });

    // Single-click on a broken row: show a plain-language explanation.
    playlist_box.connect_row_selected({
        let state      = state.clone();
        let pl_status  = pl_status_label.clone();
        move |_, row| {
            let Some(row) = row else { return; };
            let idx = row.index() as usize;
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
        let state            = state.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let set_track        = set_track.clone();
        move |_| {
            {
                let mut s = state.borrow_mut();
                let _ = s.player.stop();
                s.playlist.tracks.clear();
                s.playlist.current_index = 0;
                s.last_duration = None;
                s.pending_seek  = None;
                s.mute_pending  = None;
            }
            set_track("No track loaded");
            rebuild_playlist();
        }
    });

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
            "audio/mpeg", "audio/flac", "audio/ogg", "audio/opus",
            "audio/wav",  "audio/x-wav", "audio/aac", "audio/mp4",
            "audio/x-m4a", "audio/x-ms-wma",
        ] {
            f.add_mime_type(mime);
        }
        // Extension patterns as fallback for systems without full MIME support.
        for pat in &[
            "*.mp3", "*.flac", "*.ogg", "*.opus", "*.wav",
            "*.aac", "*.m4a", "*.wma", "*.ape", "*.aiff",
        ] {
            f.add_pattern(pat);
        }
        f
    };

    // [+ Files]: open the desktop file browser to pick one or more audio files.
    btn_add_files.connect_clicked({
        let state            = state.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let pl_status        = pl_status_label.clone();
        let window_wk        = playlist_win.downgrade();
        let make_filt        = make_audio_filter.clone();
        let probe_tx         = probe_tx.clone();
        let broken_tx        = broken_tx.clone();
        move |_| {
            let dialog = gtk4::FileDialog::builder()
                .title("Add Audio Files")
                .build();
            let filter_store = gio::ListStore::new::<gtk4::FileFilter>();
            filter_store.append(&make_filt());
            dialog.set_filters(Some(&filter_store));

            let state_cb     = state.clone();
            let rebuild_cb   = rebuild_playlist.clone();
            let status_cb    = pl_status.clone();
            let probe_tx_cb  = probe_tx.clone();
            let broken_tx_cb = broken_tx.clone();
            let parent      = window_wk.upgrade();
            dialog.open_multiple(
                parent.as_ref(),
                None::<&gio::Cancellable>,
                move |result| {
                    if let Ok(list) = result {
                        let before = state_cb.borrow().playlist.tracks.len();
                        let mut added = 0usize;
                        let n = list.n_items();
                        for i in 0..n {
                            if let Some(obj) = list.item(i) {
                                if let Ok(file) = obj.downcast::<gio::File>() {
                                    if let Some(path) = file.path() {
                                        let ok = state_cb.borrow_mut().add_path(&path).is_ok();
                                        if ok { added += 1; }
                                    }
                                }
                            }
                        }
                        if added > 0 {
                            status_cb.set_text(&format!("Added {} file{}", added, if added == 1 { "" } else { "s" }));
                            rebuild_cb();
                            let paths = state_cb.borrow().uncached_paths_from(before);
                            if !paths.is_empty() {
                                duration_probe::spawn_probes(paths, probe_tx_cb.clone(), broken_tx_cb.clone());
                            }
                        }
                    }
                },
            );
        }
    });

    // [+ Folder]: open the desktop folder browser; recursively add all audio files.
    btn_add_dir.connect_clicked({
        let state            = state.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let pl_status        = pl_status_label.clone();
        let window_wk        = playlist_win.downgrade();
        let probe_tx         = probe_tx.clone();
        let broken_tx        = broken_tx.clone();
        move |_| {
            let dialog = gtk4::FileDialog::builder()
                .title("Add Folder (all audio files, including subfolders)")
                .build();

            let state_cb     = state.clone();
            let rebuild_cb   = rebuild_playlist.clone();
            let status_cb    = pl_status.clone();
            let probe_tx_cb  = probe_tx.clone();
            let broken_tx_cb = broken_tx.clone();
            let parent      = window_wk.upgrade();
            dialog.select_folder(
                parent.as_ref(),
                None::<&gio::Cancellable>,
                move |result| {
                    if let Ok(folder) = result {
                        if let Some(path) = folder.path() {
                            let before  = state_cb.borrow().playlist.tracks.len();
                            let outcome = state_cb.borrow_mut().add_path(&path);
                            match outcome {
                                Ok(msg) => {
                                    status_cb.set_text(&msg);
                                    rebuild_cb();
                                    let paths = state_cb.borrow().uncached_paths_from(before);
                                    if !paths.is_empty() {
                                        duration_probe::spawn_probes(paths, probe_tx_cb.clone(), broken_tx_cb.clone());
                                    }
                                }
                                Err(msg) => { status_cb.set_text(&msg); eprintln!("{msg}"); }
                            }
                        }
                    }
                },
            );
        }
    });

    // ══════════════════════════════════════════════════════════════════════════
    // Volume slider
    // ══════════════════════════════════════════════════════════════════════════

    // connect_change_value fires only on user-driven changes, avoiding a loop.
    vol_bar.connect_change_value({
        let state = state.clone();
        move |_, _, value| {
            state.borrow_mut().player.set_volume(value);
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
        let state          = state.clone();
        let time_lbl       = time_disp_label.clone();
        let show_rem       = show_remaining.clone();
        move |_, _, value| {
            // Update the time display immediately so the user sees the correct
            // offset while scrubbing (stopped or paused), without waiting for
            // the next 100 ms tick.
            if let Some(text) = state.borrow().time_display_for_fraction(value, show_rem.get()) {
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
        let state            = state.clone();
        let time_disp_label  = time_disp_label.clone();
        let title_label      = title_label.clone();
        let artist_label     = artist_label.clone();
        let seek_bar         = seek_bar.clone();
        let play_update      = play_and_update.clone();
        let viz              = viz.clone();
        let marquee_chars    = marquee_chars.clone();
        let marquee_offset   = marquee_offset.clone();
        let marquee_tick     = marquee_tick.clone();
        let show_remaining   = show_remaining.clone();
        let state_label      = state_label.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        // Counter for periodic cache saves: fires every 300 ticks = 30 seconds.
        let mut cache_save_countdown = 300u32;

        glib::timeout_add_local(Duration::from_millis(100), move || {
            // 0. Drain probe results from background threads.
            // Rebuild the playlist once per tick if any durations arrived so
            // the new "M:SS" values appear without an extra user action.
            let mut any_probed = false;
            while let Ok((path, dur)) = probe_rx.try_recv() {
                state.borrow_mut().apply_probed_duration(&path, dur);
                any_probed = true;
            }
            // 0b. Drain missing-file notifications; mark those tracks broken.
            while let Ok(path) = broken_rx.try_recv() {
                for track in &mut state.borrow_mut().playlist.tracks {
                    if track.path == path {
                        track.broken = true;
                        any_probed = true;
                        break;
                    }
                }
            }
            if any_probed {
                rebuild_playlist();
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
                // On error, mark the current track broken so it shows a
                // warning indicator and is skipped in future auto-advances.
                if matches!(event, BusEvent::Error) {
                    let mut s = state.borrow_mut();
                    let idx = s.playlist.current_index;
                    if let Some(t) = s.playlist.tracks.get_mut(idx) {
                        t.broken = true;
                    }
                }
                // Advance past broken tracks to the next playable one.
                let advanced = {
                    let mut s = state.borrow_mut();
                    let total = s.playlist.len();
                    let mut found = false;
                    for _ in 0..total {
                        if s.playlist.next().is_none() { break; }
                        let idx = s.playlist.current_index;
                        if !s.playlist.tracks.get(idx).map(|t| t.broken).unwrap_or(false) {
                            found = true;
                            break;
                        }
                    }
                    found
                };
                if advanced {
                    play_update();
                    rebuild_playlist();
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
                    } else { false }
                } else { false }
            } else { false };
            if gst_dur_written { rebuild_playlist(); }

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
                        if let Some(text) = state.borrow()
                            .time_display_for_fraction(fraction, show_rem)
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
                            let rs  = rem.as_secs();
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
                let display_cols = if label_w > 0 { (label_w / 8).max(10) as usize } else { 30 };

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
                    PlayerState::Paused  => "⏸",
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

            let s         = state.borrow();
            let is_playing = *s.player.state() == PlayerState::Playing;
            let pos_ms    = s.player.position().unwrap_or(Duration::ZERO).as_millis() as u64;
            let mode      = s.config.visualizer.mode.clone();
            drop(s); // release borrow before any GTK re-entry risk

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
                    // Minimum 10 bars; more bars at wider widths.
                    // Each bar uses a frequency-scaled oscillation so lower-
                    // indexed bars move slowly (bass) and higher ones fast
                    // (treble), giving the classic spectrum analyser feel.
                    let num_bars = (width / 5).max(10) as usize;
                    let bar_w   = width  as f64 / num_bars as f64;
                    let t       = pos_ms as f64 / 80.0;
                    for i in 0..num_bars {
                        let freq = 1.0 + i as f64 * 0.5;
                        let amp  = ((t * freq).sin() * 0.4
                            + (t * freq * 1.5).sin() * 0.2
                            + 0.55)
                            .clamp(0.05, 1.0);
                        let bar_h = (amp * height as f64 * 0.92).max(2.0);
                        let x    = i as f64 * bar_w + 0.5;
                        let y    = height as f64 - bar_h;
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
                        let t   = x as f64 / width as f64;
                        let ph  = t0 + t * PI * 6.0;
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
        let state        = state.clone();
        let jump_entry   = jump_entry.clone();
        let jump_box     = jump_box.clone();
        let jump_indices = jump_indices.clone();
        Rc::new(move || {
            // Remove all existing rows.
            while let Some(row) = jump_box.row_at_index(0) {
                jump_box.remove(&row);
            }
            let mut indices = jump_indices.borrow_mut();
            indices.clear();

            let q = jump_entry.text().to_lowercase();
            let s = state.borrow();
            for (idx, track) in s.playlist.tracks.iter().enumerate() {
                if !q.is_empty()
                    && !track.title.to_lowercase().contains(&q)
                    && !track.artist.to_lowercase().contains(&q)
                    && !track.album.to_lowercase().contains(&q)
                {
                    continue;
                }
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
        let state            = state.clone();
        let play_and_update  = play_and_update.clone();
        let rebuild_playlist = rebuild_playlist.clone();
        let status_label     = status_label.clone();
        let pl_status        = pl_status_label.clone();
        let kbd_set_track    = set_track.clone();
        let kbd_rebuild      = rebuild_playlist.clone();
        let kbd_vol_bar      = vol_bar.clone();
        let kbd_seek_bar     = seek_bar.clone();
        let playlist_win_wk  = playlist_win.downgrade();
        // Strong reference: keeps the window alive even when hidden, so
        // repeated open/close cycles work without recreating the widget tree.
        let kbd_jump_win     = jump_win.clone();
        let window_weak      = window.downgrade();
        let remove_sel       = remove_selected.clone();
        let kbd_probe_tx     = probe_tx.clone();
        let kbd_broken_tx    = broken_tx.clone();
        let kbd_rebuild_jump = rebuild_jump.clone();
        let kbd_jump_entry   = jump_entry.clone();
        let kbd_btn_info     = btn_info.clone();

        Rc::new(move |key: gdk::Key| -> glib::Propagation {

            match key {
                // ── Winamp transport bindings ──────────────────────────────
                gdk::Key::z => {
                    let result = { state.borrow_mut().play_prev() };
                    if let Some(d) = result { kbd_set_track(&d); kbd_rebuild(); }
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
                    if let Some(d) = result { kbd_set_track(&d); kbd_rebuild(); }
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
                    { let mut s = state.borrow_mut(); s.config.playback.volume = new_vol; s.player.set_volume(new_vol); }
                    kbd_vol_bar.set_value(new_vol);
                    glib::Propagation::Stop
                }
                gdk::Key::equal | gdk::Key::plus => {
                    let new_vol = {
                        let s = state.borrow();
                        (s.config.playback.volume + 0.05).clamp(0.0, 1.0)
                    };
                    { let mut s = state.borrow_mut(); s.config.playback.volume = new_vol; s.player.set_volume(new_vol); }
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
                    for mime in &["audio/mpeg","audio/flac","audio/ogg","audio/opus",
                                  "audio/wav","audio/aac","audio/mp4","audio/x-m4a"] {
                        filter.add_mime_type(mime);
                    }
                    for pat in &["*.mp3","*.flac","*.ogg","*.opus","*.wav","*.aac","*.m4a"] {
                        filter.add_pattern(pat);
                    }
                    let filters = gio::ListStore::new::<gtk4::FileFilter>();
                    filters.append(&filter);

                    let dialog = gtk4::FileDialog::builder().title("Add Audio File").build();
                    dialog.set_filters(Some(&filters));

                    let state_cb     = state.clone();
                    let rebuild_cb   = rebuild_playlist.clone();
                    let status_cb    = status_label.clone();
                    let pl_stat_cb   = pl_status.clone();
                    let probe_tx_cb  = kbd_probe_tx.clone();
                    let broken_tx_cb = kbd_broken_tx.clone();
                    let parent      = window_weak.upgrade();
                    dialog.open(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                        if let Ok(file) = result {
                            if let Some(path) = file.path() {
                                let before  = state_cb.borrow().playlist.tracks.len();
                                let outcome = state_cb.borrow_mut().add_path(&path);
                                match outcome {
                                    Ok(msg)  => {
                                        status_cb.set_text(&msg);
                                        pl_stat_cb.set_text(&msg);
                                        rebuild_cb();
                                        let paths = state_cb.borrow().uncached_paths_from(before);
                                        if !paths.is_empty() {
                                            duration_probe::spawn_probes(paths, probe_tx_cb.clone(), broken_tx_cb.clone());
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
        let handler  = handle_key.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, _| handler(key));
        window.add_controller(key_ctrl);
    }

    // Attach the same handler to the playlist window so all shortcuts work
    // even when the playlist window has keyboard focus.  Use Capture phase so
    // the ListBox cannot swallow keys (e.g. 'j') before they reach this handler.
    {
        let key_ctrl = EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let handler  = handle_key.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, _| handler(key));
        playlist_win.add_controller(key_ctrl);
    }

    // ℹ Info button — show keyboard shortcuts window.
    // Connected here (after handle_key is defined) so shortcuts work inside it.
    btn_info.connect_clicked({
        let window_wk  = window.downgrade();
        let handle_key = handle_key.clone();
        move |_| {
            let shortcuts_text =
"SparkAmp — Keyboard Shortcuts

── Playback ────────────────────────────────────────
  z          Previous track / restart
  x          Play
  c          Pause / resume
  v          Stop
  b          Next track
  ←  →       Seek −5 s / +5 s

── Volume ──────────────────────────────────────────
  -          Volume down 5 %
  =          Volume up 5 %

── Playlist ────────────────────────────────────────
  n          Add file(s) or folder(s)  (comma-separated list ok)
  ,          Move track (enter from → to positions)
  .          Remove track by number
  /          Clear all tracks
  j          Jump / search
  ↑ k        Browse playlist up
  ↓ l        Browse playlist down
  Enter      Play selected track
  Del        Remove highlighted track
  p          Toggle playlist window

── View ────────────────────────────────────────────
  a          Cycle visualizer mode (bars / oscilloscope)
  Right-click Toggle dark / light theme

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
                .child(&gtk4::Label::builder()
                    .label(shortcuts_text)
                    .halign(gtk4::Align::Start)
                    .valign(gtk4::Align::Start)
                    .use_markup(false)
                    .selectable(false)
                    .css_classes(["info-text"])
                    .build())
                .build();

            // Esc closes; all transport shortcuts also work.
            let key_ctrl = gtk4::EventControllerKey::new();
            let handler  = handle_key.clone();
            let win_wk2  = win.downgrade();
            key_ctrl.connect_key_pressed(move |_, key, _, _| {
                if key == gdk::Key::Escape {
                    if let Some(w) = win_wk2.upgrade() { w.close(); }
                    return glib::Propagation::Stop;
                }
                handler(key)
            });
            win.add_controller(key_ctrl);

            win.set_child(Some(&scroll));
            win.present();
        }
    });

    // ══════════════════════════════════════════════════════════════════════════
    // Jump window callbacks (wired after handle_key so the key controller can
    // delegate transport shortcuts to it).
    // ══════════════════════════════════════════════════════════════════════════

    // Typing in the jump entry: immediately refilter results.
    jump_entry.connect_changed({
        let rebuild_jump = rebuild_jump.clone();
        move |_| { rebuild_jump(); }
    });

    // Enter: play the selected (or first) result and close the window.
    jump_entry.connect_activate({
        let state           = state.clone();
        let play_and_update = play_and_update.clone();
        let jump_box        = jump_box.clone();
        let jump_indices    = jump_indices.clone();
        let jump_win_wk     = jump_win.downgrade();
        move |_| {
            let sel_row_idx = jump_box.selected_row().map(|r| r.index() as usize);
            if let Some(list_pos) = sel_row_idx {
                if let Some(&track_idx) = jump_indices.borrow().get(list_pos) {
                    state.borrow_mut().playlist.jump_to(track_idx);
                    play_and_update();
                }
            }
            if let Some(w) = jump_win_wk.upgrade() { w.close(); }
        }
    });

    // SearchEntry emits stop-search (and consumes Escape) before window-level
    // key controllers see it.  Wire the signal directly so Escape always closes.
    jump_entry.connect_stop_search({
        let jw = jump_win.clone();
        move |_| { jw.close(); }
    });

    // Key controller for the jump window: ↑↓ navigate rows; Escape as a
    // fallback in case focus is on the list box rather than the entry.
    // PropagationPhase::Capture ensures we intercept before child widgets.
    {
        let key_ctrl = EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let jb       = jump_box.clone();
        let jw_wk    = jump_win.downgrade();
        key_ctrl.connect_key_pressed(move |_, key, _, _| {
            match key {
                gdk::Key::Escape => {
                    if let Some(w) = jw_wk.upgrade() { w.close(); }
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
            }
        });
        jump_win.add_controller(key_ctrl);
    }

    // Double-clicking a result plays it immediately.
    jump_box.connect_row_activated({
        let state           = state.clone();
        let play_and_update = play_and_update.clone();
        let jump_indices    = jump_indices.clone();
        let jump_win_wk     = jump_win.downgrade();
        move |_, row| {
            let list_pos = row.index() as usize;
            if let Some(&track_idx) = jump_indices.borrow().get(list_pos) {
                state.borrow_mut().playlist.jump_to(track_idx);
                play_and_update();
            }
            if let Some(w) = jump_win_wk.upgrade() { w.close(); }
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
        let state        = state.clone();
        let playlist_win = playlist_win.clone();
        move |w| {
            let _ = state.borrow().playlist.save_last();

            let mut cfg = state.borrow().config.clone();
            cfg.window.player_width     = w.width();
            cfg.window.player_height    = w.height();
            cfg.window.playlist_visible = playlist_win.is_visible();
            // If the playlist window is currently visible, capture its live
            // size.  If it was already hidden, its size was already written to
            // cfg by playlist_win.connect_close_request, so we leave it alone.
            if playlist_win.is_visible() {
                cfg.window.playlist_width  = playlist_win.width();
                cfg.window.playlist_height = playlist_win.height();
            }
            let _ = cfg.save();

            playlist_win.destroy();
            glib::Propagation::Proceed
        }
    });

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
        assert_eq!(s.time_display_for_fraction(0.75, false), Some("3:00".to_string()));
    }

    #[test]
    fn time_display_remaining_at_75_percent_of_4_minute_track() {
        // 75 % elapsed → 25 % remaining = 60 s → "-1:00".
        let s = state_with_last_duration(240);
        assert_eq!(s.time_display_for_fraction(0.75, true), Some("-1:00".to_string()));
    }

    #[test]
    fn time_display_elapsed_at_start() {
        let s = state_with_last_duration(120);
        assert_eq!(s.time_display_for_fraction(0.0, false), Some("0:00".to_string()));
    }

    #[test]
    fn time_display_elapsed_at_end() {
        let s = state_with_last_duration(120);
        assert_eq!(s.time_display_for_fraction(1.0, false), Some("2:00".to_string()));
    }

    #[test]
    fn time_display_remaining_at_start() {
        // 0 % elapsed → full duration remaining = 120 s → "-2:00".
        let s = state_with_last_duration(120);
        assert_eq!(s.time_display_for_fraction(0.0, true), Some("-2:00".to_string()));
    }

    #[test]
    fn time_display_fraction_clamps_above_one() {
        let s = state_with_last_duration(60);
        assert_eq!(s.time_display_for_fraction(1.5, false), Some("1:00".to_string()));
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
        let err = s.add_track_from_path("  /nonexistent/file.mp3  ").unwrap_err();
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
        let dur  = Duration::from_secs(180);
        s.apply_probed_duration(&path, dur);
        assert_eq!(s.playlist.tracks[0].duration, Some(dur));
    }

    #[test]
    fn apply_probed_duration_inserts_into_cache() {
        let mut s = make_state();
        s.playlist.add(fake_track("Song"));
        let path = s.playlist.tracks[0].path.clone();
        s.apply_probed_duration(&path, Duration::from_secs(120));
        assert!(s.duration_cache.dirty);
        assert_eq!(s.duration_cache.get(&path), Some(Duration::from_secs(120)));
    }

    #[test]
    fn apply_probed_duration_updates_last_duration_for_current_stopped_track() {
        let mut s = make_state();
        s.playlist.add(fake_track("Song"));
        let path = s.playlist.tracks[0].path.clone();
        let dur  = Duration::from_secs(200);
        s.apply_probed_duration(&path, dur);
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
        s.apply_probed_duration(&path_b, Duration::from_secs(99));
        // Track B is not current → last_duration unchanged.
        assert_eq!(s.last_duration, None);
    }

    // ── AppState::apply_cached_durations ─────────────────────────────────────

    #[test]
    fn apply_cached_durations_fills_from_cache() {
        let mut s = make_state();
        s.playlist.add(fake_track("Song"));
        let path = s.playlist.tracks[0].path.clone();
        let dur  = Duration::from_secs(240);
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
        assert_eq!(s.playlist.tracks[0].duration, Some(Duration::from_secs(100)));
    }
}
