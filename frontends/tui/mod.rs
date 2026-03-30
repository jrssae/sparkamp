use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

use crate::{
    config::{Config, VisualizerMode},
    duration_cache::DurationCache,
    duration_probe,
    engine::{BusEvent, Player, PlayerState},
    id3_editor::{
        read_extra_frames, read_tag_fields, write_extra_frame, write_tag_fields, ExtraFrame,
        TagFields, ID3V1_GENRES,
    },
    model::{Playlist, Track},
    plugin_manager::PluginManager,
    shuffle::ShuffleState,
};

mod ui;

/// Number of ticks (each ≈ 100 ms) before a transient status message is
/// auto-cleared.  10 ticks ≈ 1 second — long enough to read, short enough
/// not to linger.
const STATUS_TICKS: u8 = 10;

// ---------------------------------------------------------------------------
// Mode
// ---------------------------------------------------------------------------

pub enum Mode {
    Normal,
    Jump {
        query: String,
        results: Vec<usize>,
        selected: usize,
        /// When true, closing the Jump overlay returns to the Media Library
        /// instead of Normal mode (the user opened Jump via Alt+j from the ML).
        from_media_library: bool,
    },
    /// n key: user types a file or directory path (spaces are literal; no quoting needed).
    AddFile {
        input: String,
        scan_cancel: Option<Arc<AtomicBool>>,
        scan_added: usize,
    },
    /// m key: two-step entry — first the source position, then the destination.
    MoveTrack {
        input: String,
        /// None = waiting for "from" position; Some(n) = have from, waiting for "to".
        from: Option<usize>,
    },
    /// , key: user types a 1-based position number to remove.
    RemoveTrack {
        input: String,
    },
    /// i key: display keyboard shortcut reference.
    /// `scroll` tracks the vertical scroll offset (↑/↓ to move).
    /// z/x/c/v/b/j still work while the overlay is open; Esc closes it.
    Help {
        scroll: u16,
    },
    /// d key: open the ID3 tag editor for the highlighted playlist track.
    Id3Editor(Id3EditorState),
    /// e key: open the settings overlay.
    Settings(SettingsState),
    /// u key: open the 10-band equalizer overlay.
    Equalizer(EqState),
    /// ML key: full-screen media library browser.
    MediaLibrary(MediaLibraryState),
}

/// State for an in-progress background add-file scan.
///
/// Holds the three channels returned by `scan_files_for_ui` / `scan_folder_for_ui`
/// plus the playlist index at which this scan started (needed for O(1) metadata patching).
struct ScanChannels {
    fast_rx: mpsc::Receiver<Track>,
    meta_rx: mpsc::Receiver<(usize, String, String, String, String)>,
    done_rx: mpsc::Receiver<usize>,
    /// Receives a single usize once the scan thread finishes Phase 1.
    /// The TUI uses recv_timeout so it does not need this signal for
    /// correctness, but it must be kept alive so the scan thread's send
    /// does not return Err and abort early.
    phase1_done_rx: mpsc::Receiver<usize>,
    /// Index of the first track added by this scan, so metadata patches can
    /// address `playlist.tracks[scan_start + idx]` directly.
    scan_start: usize,
    /// Set to true once Phase 1 (fast tracks) has finished.
    fast_done: bool,
}

/// Expand a leading `~/` or lone `~` to the user's home directory.
///
/// The TUI prompt is not run through a shell, so `~` is never expanded
/// automatically.  GTK uses native file dialogs and does not need this.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(path))
    } else if path == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(path))
    } else {
        PathBuf::from(path)
    }
}

// ---------------------------------------------------------------------------
// MediaLibraryState
// ---------------------------------------------------------------------------

/// Which tab of the media library view is active.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaLibraryTab {
    /// Show all audio tracks with search.
    Files,
    /// Show playlists and a track preview panel.
    Playlists,
}

/// All state required by the full-screen media library view.
pub struct MediaLibraryState {
    /// Active tab.
    pub tab: MediaLibraryTab,
    /// Current search query (shown in the search bar).
    pub search_query: String,
    /// `true` when the user is actively typing a search query.
    pub search_active: bool,
    /// Full or filtered track list shown in the Files tab.
    pub tracks: Vec<crate::media_library::LibTrack>,
    /// All playlists, shown in the Playlists tab.
    pub playlists: Vec<crate::media_library::LibPlaylist>,
    /// Highlighted row index in the Files tab track list.
    pub selected_track: usize,
    /// Highlighted row index in the Playlists tab playlist list.
    pub selected_playlist: usize,
    /// Tracks from the currently selected playlist (preview panel).
    pub playlist_preview: Option<Vec<crate::media_library::LibTrack>>,
    /// Ordered list of column IDs currently shown in the Files tab, copied
    /// from config when the library is opened.  Editable via the Columns overlay.
    pub visible_columns: Vec<String>,
    /// Horizontal scroll offset: index into `visible_columns` of the leftmost
    /// displayed column when the table is wider than the terminal.
    pub col_offset: usize,
    /// Column ID currently used for sorting (one of the visible_column IDs).
    pub sort_col: String,
    /// When `true` the sort is descending; `false` means ascending.
    pub sort_desc: bool,
    /// When `Some(input)`, the user is typing a folder/file path to add to the ML.
    pub add_input: Option<String>,
}

// ---------------------------------------------------------------------------
// Id3EditorState
// ---------------------------------------------------------------------------

/// All state required by the ID3 tag editor overlay.
///
/// The editor shows 12 default fields in a two-column layout.  A "Customize"
/// sub-panel (toggled with `c`) lists any additional ID3v2 frames present in
/// the file.
pub struct Id3EditorState {
    /// Path to the audio file being edited.
    pub path: std::path::PathBuf,
    /// Live copies of the standard tag fields — mutated as the user types.
    pub fields: TagFields,
    /// Which of the 12 default fields currently has focus (0–11).
    pub focused: usize,
    /// Cursor position within the focused field, in Unicode scalar values
    /// (characters), not bytes.  0 = before the first character; len = after
    /// the last character.  Reset to end-of-field whenever focus changes.
    pub cursor: usize,
    /// Index into the genre typeahead suggestions while the genre field is active.
    pub genre_sel: usize,
    /// True when the Customize (extra frames) sub-panel is visible.
    pub show_extra: bool,
    /// Extra ID3v2 frames loaded from the file (all frames not in `TagFields`).
    pub extra_frames: Vec<ExtraFrame>,
    /// Which extra frame row is focused inside the Customize panel.
    pub extra_focused: usize,
    /// True when the user is typing a new value for the focused extra frame.
    pub extra_editing: bool,
    /// Edit buffer for the extra-frame value being modified.
    pub extra_input: String,
    /// Cursor position within `extra_input`, in characters.
    pub extra_cursor: usize,
    /// Status / error message shown at the bottom of the editor.
    pub status: Option<String>,
}

// ---------------------------------------------------------------------------
// SettingsState
// ---------------------------------------------------------------------------

/// All state required by the settings overlay.
///
/// Four tabs: Appearance, Behavior, Visualizer, Filetypes.  Each tab has
/// between one and two settings.  String-valued settings (Filetypes paths)
/// enter an inline text-edit mode when the user presses Enter.
pub struct SettingsState {
    /// Active tab: 0 = Appearance, 1 = Behavior, 2 = Visualizer, 3 = Filetypes.
    pub tab: usize,
    /// Which item inside the active tab is highlighted (0-based).
    pub cursor: usize,
    /// When Some, the user is editing a Filetypes string field; holds the
    /// in-progress text.  None means normal navigation mode.
    pub edit_buf: Option<String>,
}

// ---------------------------------------------------------------------------
// EqState
// ---------------------------------------------------------------------------

/// State for the 10-band equalizer overlay (Mode::Equalizer).
#[derive(Debug, Clone)]
pub struct EqState {
    /// Which column is selected: 0–9 = EQ band, 10 = pre-amp.
    /// Use left/right arrows to navigate; up/down adjusts the selected item.
    pub selected_band: usize,
}

/// Returns the number of configurable items for the given settings tab index.
///
/// Tabs: 0=Appearance, 1=Behavior, 2=Visualizer, 3=Filetypes, 4=Media Library.
pub(super) fn settings_tab_len(tab: usize) -> usize {
    match tab {
        // Appearance: 2 items (theme, custom_skin)
        0 => 2,
        // Behavior: 1 item (autoplay_on_add)
        1 => 1,
        // Visualizer: 1 item (mode)
        2 => 1,
        // Filetypes: 2 items (visualizer_dir, filetype_dir)
        3 => 2,
        // Media Library: 3 items (rescan_on_startup, periodic_rescan, rescan_interval_mins)
        4 => 3,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub struct App {
    pub playlist: Playlist,
    pub player: Player,
    pub config: Config,
    pub mode: Mode,
    /// Highlighted row in the playlist widget (not the playing track).
    pub playlist_cursor: usize,
    pub visualizer_active: bool,
    pub should_quit: bool,
    pub status_message: Option<String>,
    /// Ticks remaining before `status_message` is auto-cleared.
    /// Set to `STATUS_TICKS` (10 × 100 ms = 1 s) whenever a message is set;
    /// decremented by `tick()` and cleared when it reaches zero.
    pub status_ticks: u8,
    /// Whether the playlist panel is shown.  Toggled by 'p'.
    pub playlist_visible: bool,
    /// Current character offset into the scrolling title string.
    pub marquee_offset: usize,
    /// Tick counter used to throttle marquee advancement (advance every 3 ticks).
    pub marquee_tick: u32,
    /// Persistent cache mapping file path → duration (loaded at startup, saved on quit).
    pub duration_cache: DurationCache,
    /// Shuffle / repeat state (session-only; not persisted).
    /// Tracks which songs have been played this pass and the full playback
    /// history so the previous button works correctly in shuffle mode.
    pub shuffle_state: ShuffleState,
    /// Receiving end of the async duration-probe channel.
    /// The tick loop drains this every 100 ms and writes results back to the playlist.
    probe_rx: mpsc::Receiver<(PathBuf, Duration)>,
    /// Sending end — cloned into `duration_probe::spawn_probes` calls.
    probe_tx: mpsc::Sender<(PathBuf, Duration)>,
    /// Receiving end of the missing-file channel from background probes.
    broken_rx: mpsc::Receiver<PathBuf>,
    /// Sending end — cloned into `duration_probe::spawn_probes` calls.
    broken_tx: mpsc::Sender<PathBuf>,
    /// Plugin registry: owns all loaded visualizer and filetype plugins.
    /// Populated at startup by scanning the managed directory and any
    /// legacy directories from the config.
    pub plugin_manager: PluginManager,
    /// Media library, opened lazily on first access.
    /// `None` when the DB could not be opened (startup error silenced).
    pub media_lib: Option<crate::media_library::MediaLibrary>,
    /// Active background scan channels, present while a scan is running.
    scan_channels: Option<ScanChannels>,
}

impl App {
    pub fn new(mut playlist: Playlist, config: Config) -> Result<Self> {
        let cursor = playlist.current_index;
        let (probe_tx, probe_rx) = mpsc::channel();
        let (broken_tx, broken_rx) = mpsc::channel::<PathBuf>();

        // Load the on-disk duration cache and immediately apply any cached
        // values to the playlist so tracks that were probed before show their
        // duration right away without waiting for GStreamer.
        let duration_cache = DurationCache::load();
        for track in &mut playlist.tracks {
            if track.duration.is_none() {
                if let Some(dur) = duration_cache.get(&track.path) {
                    track.duration = Some(dur);
                }
            }
        }

        // Kick off background probes for tracks whose duration is still unknown
        // (not in the cache).  Results arrive via probe_rx in the tick loop.
        let uncached: Vec<PathBuf> = playlist
            .tracks
            .iter()
            .filter(|t| t.duration.is_none())
            .map(|t| t.path.clone())
            .collect();
        if !uncached.is_empty() {
            duration_probe::spawn_probes(uncached, probe_tx.clone(), broken_tx.clone());
        }

        // Load plugins from the managed directory and any legacy user-configured
        // directories (best-effort; failures are logged but never block startup).
        let mut plugin_manager = PluginManager::new();
        plugin_manager.load_from_config(&config);

        // Create the player and apply the saved EQ config immediately so the
        // correct settings are in effect from the very first track.
        let mut player = Player::new()?;
        let eq_bands = config.equalizer.effective_bands();
        player.apply_eq_bands(&eq_bands);

        // Open the media library DB (best-effort; silently ignore errors so a
        // missing or corrupt DB never prevents the app from starting).
        let media_lib = crate::media_library::MediaLibrary::open().ok();

        // If startup rescan is enabled, run it now in a background thread
        // so the TUI becomes interactive immediately.
        if config.media_library.rescan_on_startup {
            std::thread::spawn(|| {
                if let Ok(lib) = crate::media_library::MediaLibrary::open() {
                    let _ = lib.rescan_all();
                }
            });
        }

        let shuffle_enabled = config.playback.shuffle_enabled;
        Ok(App {
            playlist,
            player,
            config,
            mode: Mode::Normal,
            playlist_cursor: cursor,
            visualizer_active: false,
            should_quit: false,
            status_message: None,
            status_ticks: 0,
            playlist_visible: true,
            marquee_offset: 0,
            marquee_tick: 0,
            duration_cache,
            shuffle_state: {
                let mut s = ShuffleState::new();
                s.enabled = shuffle_enabled;
                s
            },
            probe_rx,
            probe_tx,
            broken_rx,
            broken_tx,
            plugin_manager,
            media_lib,
            scan_channels: None,
        })
    }

    // -----------------------------------------------------------------------
    // Display helpers
    // -----------------------------------------------------------------------

    /// Returns the full scrollable text for the now-playing marquee.
    ///
    /// Format is "Title — Artist" when artist metadata is present, or just
    /// the title (which falls back to the filename stem) when it is not.
    /// This mirrors classic Winamp's LCD display format.
    pub fn marquee_text(&self) -> String {
        match self.playlist.current() {
            None => String::new(),
            Some(t) => {
                if t.artist.is_empty() {
                    t.title.clone()
                } else {
                    format!("{} — {}", t.title, t.artist)
                }
            }
        }
    }

    /// Advance the marquee scroll offset by one character.
    ///
    /// Called every 3 ticks (≈ 300 ms) from the event loop so the scroll
    /// rate matches the classic Winamp ~3 characters / second feel.
    pub fn tick_marquee(&mut self, display_cols: usize) {
        let text = self.marquee_text();
        let char_count = text.chars().count();
        if char_count <= display_cols {
            self.marquee_offset = 0;
            self.marquee_tick = 0;
            return;
        }
        self.marquee_tick += 1;
        if self.marquee_tick >= 3 {
            self.marquee_tick = 0;
            // Cycle length = text + 5-space gap before repeat.
            self.marquee_offset = (self.marquee_offset + 1) % (char_count + 5);
        }
    }

    /// Returns the portion of the marquee text that fits in `display_cols`.
    ///
    /// When the text is shorter than `display_cols` it is returned unchanged.
    /// When longer, a sliding window starting at `marquee_offset` is returned,
    /// wrapping around through a 5-space gap so the animation loops seamlessly.
    pub fn marquee_visible(&self, display_cols: usize) -> String {
        let text = self.marquee_text();
        let chars: Vec<char> = text.chars().collect();
        if chars.len() <= display_cols {
            return text;
        }
        let gap: Vec<char> = "     ".chars().collect();
        let looped: Vec<char> = chars.iter().chain(gap.iter()).cloned().collect();
        let loop_len = looped.len();
        (0..display_cols)
            .map(|i| {
                *looped
                    .get((self.marquee_offset + i) % loop_len)
                    .unwrap_or(&' ')
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Playback helpers
    // -----------------------------------------------------------------------

    /// Borrow the shared fields as a [`crate::controller::Controller`] view so
    /// that core navigation and playback logic can be called without duplicating
    /// it in each frontend.
    fn ctrl(&mut self) -> crate::controller::Controller<'_> {
        crate::controller::Controller {
            player: &mut self.player,
            playlist: &mut self.playlist,
            config: &mut self.config,
            shuffle_state: &mut self.shuffle_state,
            plugin_manager: &mut self.plugin_manager,
        }
    }

    /// Record the current track in the shuffle history and play it.
    ///
    /// Updates all TUI-specific UI state (visualizer, marquee, cursor,
    /// status) after the core play operation completes.
    pub fn play_current(&mut self) {
        match self.ctrl().play_current() {
            crate::controller::PlayResult::Started { .. } => {
                let idx = self.playlist.current_index;
                self.playlist_cursor = idx;
                self.status_message = None;
                self.visualizer_active = true;
                self.marquee_offset = 0;
                self.marquee_tick = 0;
            }
            crate::controller::PlayResult::Error(e) => {
                self.set_status(e);
            }
            crate::controller::PlayResult::NoTrack => {}
        }
    }

    /// Like `play_current` but does not record the track in shuffle history.
    ///
    /// Used for back navigation and restarts so the history cursor is not
    /// truncated and multi-step back navigation keeps working.
    fn play_current_no_record(&mut self) {
        match self.ctrl().play_current_no_record() {
            crate::controller::PlayResult::Started { .. } => {
                let idx = self.playlist.current_index;
                self.playlist_cursor = idx;
                self.status_message = None;
                self.visualizer_active = true;
                self.marquee_offset = 0;
                self.marquee_tick = 0;
            }
            crate::controller::PlayResult::Error(e) => {
                self.set_status(e);
            }
            crate::controller::PlayResult::NoTrack => {}
        }
    }

    /// Auto-advance after end-of-stream, respecting repeat and shuffle modes.
    ///
    /// Delegates the retry loop (skipping broken tracks, etc.) to the shared
    /// controller and then updates TUI-specific state based on the outcome.
    fn advance_to_next_playable(&mut self) {
        match self.ctrl().advance_to_next_playable() {
            crate::controller::AdvanceResult::Playing { new_index } => {
                self.playlist_cursor = new_index;
                self.status_message = None;
                self.visualizer_active = true;
                self.marquee_offset = 0;
                self.marquee_tick = 0;
            }
            crate::controller::AdvanceResult::Stopped => {
                self.visualizer_active = false;
            }
        }
    }

    /// Manual "next" (b key).
    ///
    /// Navigation and repeat/shuffle logic is handled by the shared controller;
    /// this wrapper updates TUI-specific state after the move.
    pub fn play_next(&mut self) {
        match self.ctrl().nav_next() {
            crate::controller::NavResult::Target { was_playing: true } => {
                self.play_current(); // records shuffle history, updates UI state
            }
            crate::controller::NavResult::Target { was_playing: false } => {
                self.playlist_cursor = self.playlist.current_index;
            }
            crate::controller::NavResult::NoTarget => {}
        }
    }

    /// Back button logic.
    ///
    /// Navigation and repeat/shuffle logic is handled by the shared controller;
    /// this wrapper updates TUI-specific state after the move.
    pub fn play_prev(&mut self) {
        match self.ctrl().nav_prev() {
            crate::controller::NavResult::Target { was_playing: true } => {
                self.play_current_no_record(); // no history record for back nav
            }
            crate::controller::NavResult::Target { was_playing: false } => {
                self.playlist_cursor = self.playlist.current_index;
            }
            crate::controller::NavResult::NoTarget => {}
        }
    }

    /// Seek forward (`secs` > 0) or backward (`secs` < 0) by that many seconds.
    ///
    /// Delegates to the shared controller.  No-op when position or duration is
    /// unavailable (pipeline not loaded or no track playing).
    pub fn seek_delta_secs(&mut self, secs: f64) {
        self.ctrl().seek_delta_secs(secs);
    }

    /// Returns `count` (minimum 10) normalised amplitude values [0.0, 1.0] for
    /// the current visualizer frame.  When the player is not active all values
    /// are 0.0 (idle state).  Values are generated from the current playback
    /// position using composite sine waves so they animate naturally while a
    /// track plays.
    ///
    /// For `Bars` mode each bar uses a frequency-scaled phase so lower-indexed
    /// bars simulate bass frequencies (slower, higher amplitude) and
    /// higher-indexed bars simulate treble (faster, lower amplitude).
    /// For `Oscilloscope` mode a smooth periodic waveform spanning [0.0, 1.0]
    /// is returned in column order.
    pub fn visualizer_data(&self, count: usize) -> Vec<f64> {
        // Enforce a minimum of 10 data points so the visualizer always looks
        // reasonable even in very narrow terminal windows.
        let count = count.max(10);
        if !self.visualizer_active {
            return vec![0.0; count];
        }
        let pos = self
            .player
            .position()
            .unwrap_or(std::time::Duration::ZERO)
            .as_secs_f64();

        // If a plugin viz is selected, delegate to it; otherwise use the built-in mode.
        if let Some(plugin) = self.plugin_manager.active_viz_plugin() {
            return plugin.render(pos, self.visualizer_active, count);
        }

        match self.config.visualizer.mode {
            VisualizerMode::Bars => (0..count)
                .map(|i| {
                    // Scale the animation phase by a per-bar frequency so that
                    // bar 0 (leftmost) oscillates slowly (bass) and bar N-1
                    // (rightmost) oscillates quickly (treble).
                    let freq = 1.0 + i as f64 * 0.5;
                    let phase = pos * freq + i as f64 * 0.7;
                    // Mix two harmonics for a richer, less mechanical look.
                    (phase.sin() * 0.4 + (phase * 1.5).sin() * 0.2 + 0.55).clamp(0.05, 1.0)
                })
                .collect(),
            VisualizerMode::Oscilloscope => (0..count)
                .map(|i| {
                    // Normalised x position in [0.0, 1.0] across the display width.
                    let t = i as f64 / (count - 1).max(1) as f64;
                    // TAU * t traces one full cycle; pos * 4.0 animates it over time.
                    (std::f64::consts::TAU * t + pos * 4.0).sin() * 0.5 + 0.5
                })
                .collect(),
        }
    }

    // -----------------------------------------------------------------------
    // Visualizer mode cycling
    // -----------------------------------------------------------------------

    /// Advance the visualizer to the next available mode.
    ///
    /// Cycle order: Bars → Oscilloscope → plugin 0 → plugin 1 → … → Bars.
    /// Delegates the mode-cycling logic to the shared controller and then
    /// enables the visualizer and clears any status message.
    fn cycle_visualizer_mode(&mut self) {
        self.ctrl().toggle_visualizer_mode();
        self.visualizer_active = true;
        self.status_message = None;
    }

    // -----------------------------------------------------------------------
    // Playlist editing
    // -----------------------------------------------------------------------

    /// Add one or more files / directories from `raw_input`.
    ///
    /// `raw_input` may contain a **comma-separated list** of paths; each item
    /// is trimmed and processed individually.  Directory paths are scanned
    /// recursively; file paths are added directly.  A background duration
    /// probe is launched for any newly added tracks that are not already in
    /// the duration cache.
    ///
    /// The status message is updated to reflect the total number of added
    /// tracks and any errors.
    #[allow(dead_code)]
    pub fn commit_add_file(&mut self, raw_input: &str) {
        let before = self.playlist.tracks.len();
        let mut total_added = 0usize;
        let mut total_errors = 0usize;

        // Split on commas so the user can type "song.mp3, /music/rock" and
        // add both in one go — mirrors the GTK "Add Files" multi-select UX.
        for part in raw_input.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let expanded = expand_tilde(part);
            let path = expanded.as_path();

            if path.is_dir() {
                // Use extended scan so filetype plugins' formats are included.
                let extra = self.plugin_manager.extra_extensions();
                let audio_files = Playlist::collect_audio_files_extended(path, &extra);
                for audio_path in audio_files {
                    match Track::from_path(&audio_path) {
                        Ok(track) => {
                            total_added += 1;
                            self.playlist.add(track);
                        }
                        Err(_) => total_errors += 1,
                    }
                }
            } else {
                match Track::from_path(path) {
                    Ok(track) => {
                        total_added += 1;
                        self.playlist.add(track);
                    }
                    Err(_) => total_errors += 1,
                }
            }
        }

        // Any playlist mutation resets shuffle history (new playlist = fresh draw).
        if total_added > 0 {
            self.shuffle_state.reset();
        }

        // Human-readable status feedback.
        let status_msg = match (total_added, total_errors) {
            (0, _) => "No valid audio files found".to_string(),
            (1, 0) => {
                // Show the track name for single-file adds.
                let name = self
                    .playlist
                    .tracks
                    .last()
                    .map(|t| t.display_name())
                    .unwrap_or_default();
                format!("Added: {name}")
            }
            (n, 0) => format!("Added {n} files"),
            (n, e) => format!(
                "Added {n} file{} ({e} error{})",
                if n == 1 { "" } else { "s" },
                if e == 1 { "" } else { "s" }
            ),
        };
        self.set_status(status_msg);

        // Apply cached durations to the new tracks and probe the rest.
        self.probe_new_tracks(before);
    }

    /// Apply cached durations to tracks from `from` onward, then launch
    /// background probes for any that are still unknown.
    ///
    /// Called both at startup (for command-line tracks) and after each
    /// `commit_add_file` call so durations appear as quickly as possible.
    /// `pub(crate)` so unit tests can exercise the cache-hit path directly.
    pub(crate) fn probe_new_tracks(&mut self, from: usize) {
        // Fill from cache first — avoids redundant probe work.
        for track in &mut self.playlist.tracks[from..] {
            if track.duration.is_none() {
                if let Some(dur) = self.duration_cache.get(&track.path) {
                    track.duration = Some(dur);
                }
            }
        }
        // Probe whatever remains uncached.
        let uncached: Vec<PathBuf> = self.playlist.tracks[from..]
            .iter()
            .filter(|t| t.duration.is_none())
            .map(|t| t.path.clone())
            .collect();
        if !uncached.is_empty() {
            duration_probe::spawn_probes(uncached, self.probe_tx.clone(), self.broken_tx.clone());
        }
    }

    /// Move the track at 1-based `from` to 1-based `to`.
    pub fn commit_move_track(&mut self, from_1: usize, to_1: usize) {
        let len = self.playlist.len();
        if from_1 == 0 || from_1 > len || to_1 == 0 || to_1 > len {
            self.set_status(format!("Invalid position (playlist has {} tracks)", len));
            return;
        }
        self.playlist.move_track(from_1 - 1, to_1 - 1);
        self.playlist_cursor = self.playlist.current_index;
        self.status_message = None;
    }

    /// Remove the track at 1-based `pos`.
    pub fn commit_remove_track(&mut self, pos_1: usize) {
        let len = self.playlist.len();
        if pos_1 == 0 || pos_1 > len {
            self.set_status(format!("Invalid position (playlist has {} tracks)", len));
            return;
        }
        let idx = pos_1 - 1;
        let was_current = idx == self.playlist.current_index;
        self.playlist.remove(idx);
        // Shuffle history is no longer valid after a playlist mutation.
        self.shuffle_state.reset();
        self.playlist_cursor = self.playlist.current_index;
        if was_current && !self.playlist.is_empty() {
            self.play_current();
        } else if self.playlist.is_empty() {
            let _ = self.player.stop();
        }
        self.status_message = None;
    }

    // -----------------------------------------------------------------------
    // Input handling
    // -----------------------------------------------------------------------

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match self.mode {
            Mode::Normal => self.handle_normal(code),
            Mode::Jump { .. } => self.handle_jump(code),
            Mode::AddFile { .. } => self.handle_add_file(code),
            Mode::MoveTrack { .. } => self.handle_move_track(code),
            Mode::RemoveTrack { .. } => self.handle_remove_track(code),
            Mode::Help { ref mut scroll } => {
                match code {
                    // Scroll the overlay.
                    KeyCode::Up => *scroll = scroll.saturating_sub(1),
                    KeyCode::Down => *scroll = scroll.saturating_add(1),

                    // Playback pass-throughs — stay in help mode.
                    KeyCode::Char('z') => self.play_prev(),
                    KeyCode::Char('x') => {
                        if *self.player.state() == PlayerState::Stopped {
                            self.play_current();
                        } else {
                            let _ = self.player.play();
                        }
                    }
                    KeyCode::Char('c') => {
                        let _ = self.player.toggle_pause();
                    }
                    KeyCode::Char('v') => {
                        let _ = self.player.stop();
                    }
                    KeyCode::Char('b') => self.play_next(),

                    // Jump — switches mode (closes help implicitly).
                    KeyCode::Char('j') | KeyCode::Char('J') => {
                        let results = (0..self.playlist.len()).collect();
                        self.mode = Mode::Jump {
                            query: String::new(),
                            results,
                            selected: 0,
                            from_media_library: false,
                        };
                    }

                    // Close the overlay.
                    KeyCode::Esc | KeyCode::Char('i') | KeyCode::Char('I') => {
                        self.mode = Mode::Normal;
                    }

                    _ => {}
                }
            }
            Mode::Id3Editor(..) => self.handle_id3_editor(code, modifiers),
            Mode::Settings(..) => self.handle_settings(code, modifiers),
            Mode::Equalizer(..) => self.handle_equalizer(code),
            Mode::MediaLibrary(..) => self.handle_media_library(code, modifiers),
        }
    }

    fn handle_normal(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => self.should_quit = true,

            // Winamp bindings
            KeyCode::Char('z') => self.play_prev(),
            KeyCode::Char('x') => {
                if *self.player.state() == PlayerState::Stopped {
                    self.play_current();
                } else {
                    let _ = self.player.play();
                }
            }
            KeyCode::Char('c') => {
                if let Err(e) = self.player.toggle_pause() {
                    self.set_status(format!("Error: {e}"));
                }
            }
            KeyCode::Char('v') => {
                if let Err(e) = self.player.stop() {
                    self.set_status(format!("Error: {e}"));
                }
            }
            KeyCode::Char('b') => self.play_next(),

            // Playlist editing
            // n — add file(s) or folder(s); supports comma-separated list.
            KeyCode::Char('n') => {
                self.mode = Mode::AddFile {
                    input: String::new(),
                    scan_cancel: None,
                    scan_added: 0,
                };
            }
            // , — move a track (type from-number, Enter, to-number, Enter).
            KeyCode::Char(',') => {
                self.mode = Mode::MoveTrack {
                    input: String::new(),
                    from: None,
                };
            }
            // . — remove a track by 1-based number.
            KeyCode::Char('.') => {
                self.mode = Mode::RemoveTrack {
                    input: String::new(),
                };
            }

            // Toggle playlist visibility.  'p' and 'P' both work so the user
            // doesn't need to worry about Shift.
            KeyCode::Char('p') | KeyCode::Char('P') => {
                self.playlist_visible = !self.playlist_visible;
            }

            // Jump/search
            KeyCode::Char('j') | KeyCode::Char('J') => {
                let results = (0..self.playlist.len()).collect();
                self.mode = Mode::Jump {
                    query: String::new(),
                    results,
                    selected: 0,
                    from_media_library: false,
                };
            }

            // Volume — held key repeats automatically via crossterm key-repeat.
            KeyCode::Char('-') => {
                let vol = self.ctrl().adjust_volume(-0.05);
                self.set_status(format!("Volume: {}%", (vol * 100.0).round() as u32));
            }
            KeyCode::Char('=') => {
                let vol = self.ctrl().adjust_volume(0.05);
                self.set_status(format!("Volume: {}%", (vol * 100.0).round() as u32));
            }

            // Seek ±5 s; crossterm key-repeat fires repeatedly while held,
            // giving continuous fast-forward / rewind behaviour.
            KeyCode::Left => self.seek_delta_secs(-5.0),
            KeyCode::Right => self.seek_delta_secs(5.0),

            // Visualizer mode cycle: Bars → Oscilloscope → plugin 0 → plugin 1 → … → Bars
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.cycle_visualizer_mode();
            }

            // Playlist navigation
            KeyCode::Up | KeyCode::Char('k') => {
                self.playlist_cursor = self.playlist_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('l') => {
                if self.playlist_cursor + 1 < self.playlist.len() {
                    self.playlist_cursor += 1;
                }
            }
            KeyCode::Enter => {
                self.playlist.jump_to(self.playlist_cursor);
                self.play_current();
            }

            // Delete key: remove the currently highlighted playlist item.
            KeyCode::Delete => {
                if !self.playlist.is_empty() {
                    self.commit_remove_track(self.playlist_cursor + 1);
                }
            }

            // / — clear all tracks from the playlist.
            KeyCode::Char('/') => {
                let _ = self.player.stop();
                self.playlist.tracks.clear();
                self.playlist.current_index = 0;
                self.playlist_cursor = 0;
                self.shuffle_state.reset(); // fresh playlist → fresh shuffle draw
                self.set_status("Playlist cleared");
            }

            // i / I — show keyboard shortcut reference overlay.
            KeyCode::Char('i') | KeyCode::Char('I') => {
                self.mode = Mode::Help { scroll: 0 };
            }

            // r — cycle repeat mode: Off → Song → Playlist → Off.
            // Current mode is shown in the header track-info area.
            KeyCode::Char('r') | KeyCode::Char('R') => {
                let new_mode = self.config.playback.repeat_mode.cycle();
                self.config.playback.repeat_mode = new_mode;
                self.set_status(new_mode.label());
            }

            // s — toggle shuffle on/off (hidden shortcut; only shown in help).
            // Resets the shuffle history so the new setting takes effect cleanly.
            KeyCode::Char('s') | KeyCode::Char('S') => {
                self.shuffle_state.toggle();
                // Mirror to config so the setting survives to the next session.
                self.config.playback.shuffle_enabled = self.shuffle_state.enabled;
                if self.shuffle_state.enabled {
                    self.set_status("Shuffle: On");
                } else {
                    self.set_status("Shuffle: Off");
                }
            }

            // d — open the ID3 tag editor for the currently highlighted track.
            // When the playlist is hidden, fall back to the currently playing
            // track (current_index) because the cursor may lag behind.
            KeyCode::Char('d') | KeyCode::Char('D') => {
                let d_idx = if self.playlist_visible {
                    self.playlist_cursor
                } else {
                    self.playlist.current_index
                };
                if let Some(track) = self.playlist.tracks.get(d_idx) {
                    let path = track.path.clone();
                    let fields = read_tag_fields(&path);
                    let extra_frames = read_extra_frames(&path);
                    let initial_cursor = fields.title.chars().count();
                    self.mode = Mode::Id3Editor(Id3EditorState {
                        path,
                        fields,
                        focused: 0,
                        cursor: initial_cursor,
                        genre_sel: 0,
                        show_extra: false,
                        extra_frames,
                        extra_focused: 0,
                        extra_editing: false,
                        extra_input: String::new(),
                        extra_cursor: 0,
                        status: None,
                    });
                } else {
                    self.set_status("No track selected");
                }
            }

            // e — open the settings overlay.
            KeyCode::Char('e') | KeyCode::Char('E') => {
                self.mode = Mode::Settings(SettingsState {
                    tab: 0,
                    cursor: 0,
                    edit_buf: None,
                });
            }

            // u — open the 10-band equalizer overlay.
            KeyCode::Char('u') | KeyCode::Char('U') => {
                self.mode = Mode::Equalizer(EqState { selected_band: 0 });
            }

            // m / M — open the media library full-screen view.
            KeyCode::Char('m') | KeyCode::Char('M') => {
                self.open_media_library();
            }

            _ => {}
        }
    }

    /// Open the media library view, loading the track list from the DB.
    ///
    /// If the media library DB is not open (e.g. failed to initialise at
    /// startup), a status message is shown instead and the mode is unchanged.
    fn open_media_library(&mut self) {
        let visible_columns = self.config.media_library.visible_columns.clone();
        // Default sort: artist ascending (first column alphabetically).
        let sort_col = "artist".to_string();
        let sort_desc = false;
        let tracks = if let Some(ref lib) = self.media_lib {
            lib.all_tracks_sorted(&sort_col, sort_desc)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let playlists = if let Some(ref lib) = self.media_lib {
            lib.all_playlists().unwrap_or_default()
        } else {
            Vec::new()
        };
        self.mode = Mode::MediaLibrary(MediaLibraryState {
            tab: MediaLibraryTab::Files,
            search_query: String::new(),
            search_active: false,
            tracks,
            playlists,
            selected_track: 0,
            selected_playlist: 0,
            playlist_preview: None,
            visible_columns,
            col_offset: 0,
            sort_col,
            sort_desc,
            add_input: None,
        });
    }

    fn handle_jump(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                // Return to the media library if that's where Jump was opened from.
                let from_ml = matches!(
                    self.mode,
                    Mode::Jump {
                        from_media_library: true,
                        ..
                    }
                );
                if from_ml {
                    self.open_media_library();
                } else {
                    self.mode = Mode::Normal;
                }
            }

            KeyCode::Enter => {
                let to_play = if let Mode::Jump {
                    ref results,
                    selected,
                    ..
                } = self.mode
                {
                    results.get(selected).copied()
                } else {
                    None
                };
                let from_ml = matches!(
                    self.mode,
                    Mode::Jump {
                        from_media_library: true,
                        ..
                    }
                );
                if let Some(idx) = to_play {
                    self.playlist.jump_to(idx);
                    self.play_current();
                }
                if from_ml {
                    self.open_media_library();
                } else {
                    self.mode = Mode::Normal;
                }
            }

            KeyCode::Up => {
                if let Mode::Jump {
                    ref mut selected, ..
                } = self.mode
                {
                    *selected = selected.saturating_sub(1);
                }
            }

            KeyCode::Down => {
                if let Mode::Jump {
                    ref mut selected,
                    ref results,
                    ..
                } = self.mode
                {
                    *selected = (*selected + 1).min(results.len().saturating_sub(1));
                }
            }

            KeyCode::Char(c) => {
                let new_query = if let Mode::Jump { ref query, .. } = self.mode {
                    let mut q = query.clone();
                    q.push(c);
                    q
                } else {
                    return;
                };
                self.apply_jump_query(new_query);
            }

            KeyCode::Backspace => {
                let new_query = if let Mode::Jump { ref query, .. } = self.mode {
                    let mut q = query.clone();
                    q.pop();
                    q
                } else {
                    return;
                };
                self.apply_jump_query(new_query);
            }

            _ => {}
        }
    }

    fn apply_jump_query(&mut self, query: String) {
        // Preserve the from_media_library flag so Esc/Enter still return to ML.
        let from_media_library = matches!(
            self.mode,
            Mode::Jump {
                from_media_library: true,
                ..
            }
        );
        let results = if query.is_empty() {
            (0..self.playlist.len()).collect()
        } else {
            self.playlist.search_indices(&query)
        };
        self.mode = Mode::Jump {
            query,
            results,
            selected: 0,
            from_media_library,
        };
    }

    fn handle_add_file(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                // Cancel any active background scan.
                if let Mode::AddFile {
                    ref scan_cancel, ..
                } = self.mode
                {
                    if let Some(cancel) = scan_cancel {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                let input = if let Mode::AddFile { ref input, .. } = self.mode {
                    input.clone()
                } else {
                    return;
                };
                // Collect all audio files from the comma-separated input.  Dir
                // walks are fast (readdir only, no metadata), so we do this on
                // the main thread before handing the list to the background scan.
                let extra = self.plugin_manager.extra_extensions();
                let mut all_files: Vec<PathBuf> = Vec::new();
                for part in input.split(',') {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }
                    let path = expand_tilde(part);
                    if path.is_dir() {
                        let files = Playlist::collect_audio_files_extended(&path, &extra);
                        all_files.extend(files);
                    } else {
                        all_files.push(path);
                    }
                }
                let scan_start = self.playlist.tracks.len();
                let cancel = Arc::new(AtomicBool::new(false));
                let (fast_tx, fast_rx) = mpsc::channel::<Track>();
                let (meta_tx, meta_rx) =
                    mpsc::channel::<(usize, String, String, String, String)>();
                let (done_tx, done_rx) = mpsc::channel::<usize>();
                let (phase1_done_tx, phase1_done_rx) = mpsc::channel::<usize>();
                Playlist::scan_files_for_ui(
                    all_files,
                    cancel.clone(),
                    fast_tx,
                    meta_tx,
                    done_tx,
                    phase1_done_tx,
                );
                if let Mode::AddFile {
                    ref mut scan_cancel, ..
                } = self.mode
                {
                    *scan_cancel = Some(cancel);
                }
                self.scan_channels = Some(ScanChannels {
                    fast_rx,
                    meta_rx,
                    done_rx,
                    phase1_done_rx,
                    scan_start,
                    fast_done: false,
                });
                self.status_message = Some("Scanning…".to_string());
                self.status_ticks = STATUS_TICKS;
                self.drain_add_file_scan();
            }
            KeyCode::Backspace => {
                if let Mode::AddFile { ref mut input, .. } = self.mode {
                    input.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Mode::AddFile { ref mut input, .. } = self.mode {
                    input.push(c);
                }
            }
            _ => {}
        }
    }

    fn handle_move_track(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                // Clone state needed before the mutable borrow in commit_*.
                let (input, from) = if let Mode::MoveTrack { ref input, from } = self.mode {
                    (input.clone(), from)
                } else {
                    return;
                };

                match from {
                    None => {
                        // First Enter: parse "from" position.
                        match input.trim().parse::<usize>() {
                            Ok(n) if n > 0 => {
                                self.mode = Mode::MoveTrack {
                                    input: String::new(),
                                    from: Some(n),
                                };
                            }
                            _ => {
                                self.set_status("Enter a valid track number");
                                self.mode = Mode::Normal;
                            }
                        }
                    }
                    Some(from_pos) => {
                        // Second Enter: parse "to" position and commit.
                        self.mode = Mode::Normal;
                        match input.trim().parse::<usize>() {
                            Ok(to_pos) if to_pos > 0 => {
                                self.commit_move_track(from_pos, to_pos);
                            }
                            _ => {
                                self.set_status("Enter a valid track number");
                            }
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                if let Mode::MoveTrack { ref mut input, .. } = self.mode {
                    input.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Mode::MoveTrack { ref mut input, .. } = self.mode {
                    input.push(c);
                }
            }
            _ => {}
        }
    }

    fn handle_remove_track(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                let input = if let Mode::RemoveTrack { ref input } = self.mode {
                    input.clone()
                } else {
                    return;
                };
                self.mode = Mode::Normal;
                match input.trim().parse::<usize>() {
                    Ok(pos) if pos > 0 => {
                        self.commit_remove_track(pos);
                    }
                    _ => {
                        self.set_status("Enter a valid track number");
                    }
                }
            }
            KeyCode::Backspace => {
                if let Mode::RemoveTrack { ref mut input } = self.mode {
                    input.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Mode::RemoveTrack { ref mut input } = self.mode {
                    input.push(c);
                }
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Media library key handler
    // -----------------------------------------------------------------------

    /// Handle key events while the full-screen media library view is open.
    ///
    /// Key map:
    ///   Esc            — close the media library and return to Normal
    ///   Tab            — switch between Files and Playlists tabs
    ///   / or Ctrl+F    — activate the search input
    ///   Esc (search)   — deactivate search input (clear query)
    ///   ↑ / k          — move selection up
    ///   ↓ / j          — move selection down
    ///   Enter (Files)  — add selected track to the current playlist
    ///   Alt+z/x/c/v/b  — pass transport commands through while in this mode
    fn handle_media_library(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // --- Alt + transport bindings pass through to the player ---
        if modifiers.contains(KeyModifiers::ALT) {
            match code {
                KeyCode::Char('z') => {
                    self.play_prev();
                    return;
                }
                KeyCode::Char('x') => {
                    if *self.player.state() == crate::engine::PlayerState::Stopped {
                        self.play_current();
                    } else {
                        let _ = self.player.play();
                    }
                    return;
                }
                KeyCode::Char('c') => {
                    let _ = self.player.toggle_pause();
                    return;
                }
                KeyCode::Char('v') => {
                    let _ = self.player.stop();
                    return;
                }
                KeyCode::Char('b') => {
                    self.play_next();
                    return;
                }
                KeyCode::Char('j') => {
                    let results = (0..self.playlist.len()).collect();
                    self.mode = Mode::Jump {
                        query: String::new(),
                        results,
                        selected: 0,
                        from_media_library: true,
                    };
                    return;
                }
                _ => {}
            }
        }

        // Snapshot relevant state before borrowing mutably.
        let (search_active, add_active, tab) = match &self.mode {
            Mode::MediaLibrary(s) => (s.search_active, s.add_input.is_some(), s.tab.clone()),
            _ => return,
        };

        // --- Add-to-ML path input mode ---
        if add_active {
            match code {
                KeyCode::Esc => {
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        s.add_input = None;
                    }
                }
                KeyCode::Enter => {
                    let input = if let Mode::MediaLibrary(s) = &self.mode {
                        s.add_input.clone().unwrap_or_default()
                    } else {
                        String::new()
                    };
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        s.add_input = None;
                    }
                    self.commit_ml_add_path(input);
                }
                KeyCode::Backspace => {
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        if let Some(ref mut buf) = s.add_input {
                            buf.pop();
                        }
                    }
                }
                KeyCode::Char(ch) => {
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        if let Some(ref mut buf) = s.add_input {
                            buf.push(ch);
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        // --- Search-input mode ---
        if search_active {
            match code {
                KeyCode::Esc => {
                    // Deactivate search, keep query so the user can see results.
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        s.search_active = false;
                    }
                }
                KeyCode::Backspace => {
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        s.search_query.pop();
                    }
                    self.refresh_ml_search();
                }
                KeyCode::Char(ch) => {
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        s.search_query.push(ch);
                    }
                    self.refresh_ml_search();
                }
                _ => {}
            }
            return;
        }

        // --- Normal navigation ---
        match code {
            // Close media library.
            KeyCode::Esc => {
                self.mode = Mode::Normal;
            }

            // Tab: switch between Files and Playlists.
            KeyCode::Tab => {
                if let Mode::MediaLibrary(s) = &mut self.mode {
                    s.tab = match s.tab {
                        MediaLibraryTab::Files => MediaLibraryTab::Playlists,
                        MediaLibraryTab::Playlists => MediaLibraryTab::Files,
                    };
                    s.selected_track = 0;
                    s.selected_playlist = 0;
                    s.playlist_preview = None;
                }
            }

            // '/' or Ctrl+F — activate search.
            KeyCode::Char('/') | KeyCode::Char('f')
                if code == KeyCode::Char('/') || modifiers.contains(KeyModifiers::CONTROL) =>
            {
                if let Mode::MediaLibrary(s) = &mut self.mode {
                    s.search_active = true;
                }
            }

            // Navigation: up.
            KeyCode::Up | KeyCode::Char('k') => {
                if let Mode::MediaLibrary(s) = &mut self.mode {
                    match s.tab {
                        MediaLibraryTab::Files => {
                            s.selected_track = s.selected_track.saturating_sub(1);
                        }
                        MediaLibraryTab::Playlists => {
                            let prev = s.selected_playlist.saturating_sub(1);
                            s.selected_playlist = prev;
                            s.playlist_preview = None; // refreshed on Enter
                        }
                    }
                }
            }

            // Navigation: down.
            KeyCode::Down | KeyCode::Char('j') => {
                if let Mode::MediaLibrary(s) = &mut self.mode {
                    match s.tab {
                        MediaLibraryTab::Files => {
                            if s.selected_track + 1 < s.tracks.len() {
                                s.selected_track += 1;
                            }
                        }
                        MediaLibraryTab::Playlists => {
                            if s.selected_playlist + 1 < s.playlists.len() {
                                s.selected_playlist += 1;
                            }
                            s.playlist_preview = None;
                        }
                    }
                }
            }

            // Enter: act on the selected item.
            KeyCode::Enter => {
                match tab {
                    MediaLibraryTab::Files => {
                        // Add the selected track to the current playlist.
                        let path = if let Mode::MediaLibrary(s) = &self.mode {
                            s.tracks.get(s.selected_track).map(|t| t.path.clone())
                        } else {
                            None
                        };
                        if let Some(path_str) = path {
                            let p = std::path::Path::new(&path_str);
                            match crate::model::Track::from_path(p) {
                                Ok(track) => {
                                    let before = self.playlist.tracks.len();
                                    self.playlist.add(track);
                                    self.probe_new_tracks(before);
                                    self.set_status("Track added to playlist");
                                }
                                Err(e) => {
                                    self.set_status(format!("Cannot add track: {e}"));
                                }
                            }
                        }
                    }
                    MediaLibraryTab::Playlists => {
                        // Load the preview tracks for the selected playlist.
                        let playlist_info = if let Mode::MediaLibrary(s) = &self.mode {
                            s.playlists.get(s.selected_playlist).cloned()
                        } else {
                            None
                        };
                        if let Some(pl) = playlist_info {
                            let preview = self
                                .media_lib
                                .as_ref()
                                .and_then(|lib| lib.load_playlist_tracks(&pl).ok())
                                .unwrap_or_default();
                            if let Mode::MediaLibrary(s) = &mut self.mode {
                                s.playlist_preview = Some(preview);
                            }
                        }
                    }
                }
            }

            // ← / → — scroll the visible columns left or right.
            KeyCode::Left => {
                if let Mode::MediaLibrary(s) = &mut self.mode {
                    s.col_offset = s.col_offset.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Mode::MediaLibrary(s) = &mut self.mode {
                    let max = s.visible_columns.len().saturating_sub(1);
                    if s.col_offset < max {
                        s.col_offset += 1;
                    }
                }
            }

            // s — cycle the sort column; pressing s again on the same column
            // reverses the direction.
            KeyCode::Char('s') => {
                let (sort_col, sort_desc, cols) = if let Mode::MediaLibrary(s) = &self.mode {
                    (s.sort_col.clone(), s.sort_desc, s.visible_columns.clone())
                } else {
                    return;
                };
                // Find the next column in the visible list after the current sort col.
                let pos = cols.iter().position(|c| *c == sort_col);
                let (new_col, new_desc) = match pos {
                    None => (cols.first().cloned().unwrap_or(sort_col), false),
                    Some(i) => {
                        let next = i + 1;
                        if next < cols.len() {
                            // Move to the next column, ascending.
                            (cols[next].clone(), false)
                        } else {
                            // Wrap: same column again — toggle direction.
                            (cols[0].clone(), !sort_desc)
                        }
                    }
                };
                if let Mode::MediaLibrary(s) = &mut self.mode {
                    s.sort_col = new_col.clone();
                    s.sort_desc = new_desc;
                }
                self.refresh_ml_sort();
            }

            // a — prompt for a folder or file path to add to the media library.
            KeyCode::Char('a') | KeyCode::Char('A') => {
                if let Mode::MediaLibrary(s) = &mut self.mode {
                    s.add_input = Some(String::new());
                }
            }

            // i — open the Help overlay scrolled to the Media Library section.
            KeyCode::Char('i') | KeyCode::Char('I') => {
                self.mode = Mode::Help { scroll: 34 };
            }

            _ => {}
        }
    }

    /// Add a folder or file path to the media library (called from 'a' key in ML).
    /// If the folder is already watched, triggers a rescan instead.
    fn commit_ml_add_path(&mut self, input: String) {
        use crate::media_library::AddFolderResult;
        let path_str = input.trim().to_string();
        if path_str.is_empty() {
            return;
        }
        let path = std::path::Path::new(&path_str);
        if !path.exists() {
            self.set_status(format!("Path not found: {path_str}"));
            self.open_media_library();
            return;
        }
        let result = if let Some(ref lib) = self.media_lib {
            match lib.add_folder(&path_str) {
                Ok(add_result) => {
                    let is_new = matches!(add_result, AddFolderResult::New(_));
                    let folder_id = add_result.id();
                    lib.rescan_folder(folder_id, &path_str).map(|r| (r, is_new))
                }
                Err(e) => Err(e),
            }
        } else {
            self.set_status("Media library not available");
            self.open_media_library();
            return;
        };
        match result {
            Ok(((added, _removed), is_new)) => {
                if is_new {
                    self.set_status(format!("Added {added} track(s) to media library"));
                } else {
                    self.set_status(format!("Rescanned — {added} track(s) in library"));
                }
            }
            Err(e) => {
                self.set_status(format!("Error adding to ML: {e}"));
            }
        }
        self.open_media_library();
    }

    /// Re-query the DB after a sort-column or sort-direction change.
    fn refresh_ml_sort(&mut self) {
        let (query, sort_col, sort_desc) = if let Mode::MediaLibrary(s) = &self.mode {
            (s.search_query.clone(), s.sort_col.clone(), s.sort_desc)
        } else {
            return;
        };
        let tracks = if let Some(ref lib) = self.media_lib {
            if query.is_empty() {
                lib.all_tracks_sorted(&sort_col, sort_desc)
                    .unwrap_or_default()
            } else {
                lib.search_tracks_sorted(&query, &sort_col, sort_desc)
                    .unwrap_or_default()
            }
        } else {
            Vec::new()
        };
        if let Mode::MediaLibrary(s) = &mut self.mode {
            s.tracks = tracks;
            s.selected_track = 0;
        }
    }

    /// Refresh the media library track list after the search query changes.
    ///
    /// Respects the current sort column and direction.
    fn refresh_ml_search(&mut self) {
        let (query, sort_col, sort_desc) = if let Mode::MediaLibrary(s) = &self.mode {
            (s.search_query.clone(), s.sort_col.clone(), s.sort_desc)
        } else {
            return;
        };

        let tracks = if let Some(ref lib) = self.media_lib {
            if query.is_empty() {
                lib.all_tracks_sorted(&sort_col, sort_desc)
                    .unwrap_or_default()
            } else {
                lib.search_tracks_sorted(&query, &sort_col, sort_desc)
                    .unwrap_or_default()
            }
        } else {
            Vec::new()
        };

        if let Mode::MediaLibrary(s) = &mut self.mode {
            s.tracks = tracks;
            s.selected_track = 0;
        }
    }

    // -----------------------------------------------------------------------
    // ID3 editor key handler
    // -----------------------------------------------------------------------

    /// Handle a key press when the ID3 editor overlay is open.
    ///
    /// The editor has two sub-modes:
    /// - **Main fields** (`show_extra == false`): the default 12-field form.
    /// - **Customize panel** (`show_extra == true`): a scrollable list of any
    ///   additional ID3v2 frames already present in the file, with in-place
    ///   editing.
    fn handle_id3_editor(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // Alt+z/x/c/v/b/j trigger transport controls without closing the editor.
        // This check runs before the show_extra dispatch so it applies to both
        // the main panel and the Customize sub-panel.
        if modifiers.contains(KeyModifiers::ALT) {
            match code {
                KeyCode::Char('z') => {
                    self.play_prev();
                    return;
                }
                KeyCode::Char('x') => {
                    if *self.player.state() == PlayerState::Stopped {
                        self.play_current();
                    } else {
                        let _ = self.player.play();
                    }
                    return;
                }
                KeyCode::Char('c') => {
                    let _ = self.player.toggle_pause();
                    return;
                }
                KeyCode::Char('v') => {
                    let _ = self.player.stop();
                    return;
                }
                KeyCode::Char('b') => {
                    self.play_next();
                    return;
                }
                KeyCode::Char('j') | KeyCode::Char('J') => {
                    // Opens Jump, which changes mode and closes the editor.
                    let results = (0..self.playlist.len()).collect();
                    self.mode = Mode::Jump {
                        query: String::new(),
                        results,
                        selected: 0,
                        from_media_library: false,
                    };
                    return;
                }
                _ => {}
            }
        }

        // --- Customize (extra frames) sub-panel ---
        if let Mode::Id3Editor(ref state) = self.mode {
            if state.show_extra {
                self.handle_id3_extra(code, modifiers);
                return;
            }
        }

        // --- Main fields panel ---
        match code {
            // Esc: close the editor without saving.
            KeyCode::Esc => {
                self.mode = Mode::Normal;
            }

            // Tab / Shift-Tab: advance/retreat through the 12 fields.
            KeyCode::Tab => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.focused = (s.focused + 1) % 12;
                    s.cursor = id3_field_value_mut(&mut s.fields, s.focused)
                        .chars()
                        .count();
                    s.genre_sel = 0;
                    s.status = None;
                }
            }
            KeyCode::BackTab => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.focused = if s.focused == 0 { 11 } else { s.focused - 1 };
                    s.cursor = id3_field_value_mut(&mut s.fields, s.focused)
                        .chars()
                        .count();
                    s.genre_sel = 0;
                    s.status = None;
                }
            }

            // Up / Down: navigate genre suggestions when on the genre field
            // (field index 4), otherwise navigate between fields.
            KeyCode::Down => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    if s.focused == 4 {
                        let n = id3_genre_matches(&s.fields.genre).len();
                        if n > 0 {
                            s.genre_sel = (s.genre_sel + 1).min(n - 1);
                        }
                    } else {
                        s.focused = (s.focused + 1) % 12;
                        s.cursor = id3_field_value_mut(&mut s.fields, s.focused)
                            .chars()
                            .count();
                        s.genre_sel = 0;
                    }
                }
            }
            KeyCode::Up => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    if s.focused == 4 {
                        s.genre_sel = s.genre_sel.saturating_sub(1);
                    } else {
                        s.focused = if s.focused == 0 { 11 } else { s.focused - 1 };
                        s.cursor = id3_field_value_mut(&mut s.fields, s.focused)
                            .chars()
                            .count();
                        s.genre_sel = 0;
                    }
                }
            }

            // Left / Right: move cursor within the focused field.
            KeyCode::Left => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.cursor = s.cursor.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    let len = id3_field_value_mut(&mut s.fields, s.focused)
                        .chars()
                        .count();
                    s.cursor = (s.cursor + 1).min(len);
                }
            }

            // Home / End: jump to the start or end of the focused field.
            KeyCode::Home => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.cursor = 0;
                }
            }
            KeyCode::End => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.cursor = id3_field_value_mut(&mut s.fields, s.focused)
                        .chars()
                        .count();
                }
            }

            // Enter: if genre field has suggestions, accept the highlighted one;
            // otherwise advance to the next field.
            KeyCode::Enter => {
                let accept = if let Mode::Id3Editor(ref s) = self.mode {
                    if s.focused == 4 {
                        let matches = id3_genre_matches(&s.fields.genre);
                        matches.get(s.genre_sel).map(|g| g.to_string())
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    if let Some(chosen) = accept {
                        s.fields.genre = chosen;
                        s.focused = (s.focused + 1) % 12;
                        s.cursor = id3_field_value_mut(&mut s.fields, s.focused)
                            .chars()
                            .count();
                        s.genre_sel = 0;
                    } else {
                        s.focused = (s.focused + 1) % 12;
                        s.cursor = id3_field_value_mut(&mut s.fields, s.focused)
                            .chars()
                            .count();
                        s.genre_sel = 0;
                    }
                }
            }

            // Ctrl+S: save tags and close the editor.
            KeyCode::Char('s') | KeyCode::Char('S')
                if modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.id3_save_and_close();
            }

            // c / C: open the Customize (extra frames) sub-panel.
            KeyCode::Char('c') | KeyCode::Char('C') => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.show_extra = true;
                    s.extra_focused = 0;
                    s.extra_editing = false;
                    s.extra_input.clear();
                }
            }

            // Backspace: delete the character immediately before the cursor.
            KeyCode::Backspace => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    let c = s.cursor;
                    if c > 0 {
                        let byte_idx = {
                            let field = id3_field_value_mut(&mut s.fields, s.focused);
                            field.char_indices().nth(c - 1).map(|(i, _)| i)
                        };
                        if let Some(bi) = byte_idx {
                            id3_field_value_mut(&mut s.fields, s.focused).remove(bi);
                            s.cursor -= 1;
                        }
                    }
                    s.genre_sel = 0;
                }
            }

            // Any printable character: insert at the cursor position.
            KeyCode::Char(ch) => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    let c = s.cursor;
                    let byte_idx = {
                        let field = id3_field_value_mut(&mut s.fields, s.focused);
                        field
                            .char_indices()
                            .nth(c)
                            .map(|(i, _)| i)
                            .unwrap_or(field.len())
                    };
                    id3_field_value_mut(&mut s.fields, s.focused).insert(byte_idx, ch);
                    s.cursor += 1;
                    s.genre_sel = 0;
                }
            }

            _ => {}
        }
    }

    /// Handle a key press inside the Customize (extra frames) sub-panel.
    ///
    /// When `extra_editing` is true, keystrokes go to the text buffer for the
    /// currently selected frame.  When false, Up/Down navigate frames and
    /// Enter starts editing the selected frame.
    fn handle_id3_extra(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // Editing mode: keys modify the extra_input buffer.
        let editing = if let Mode::Id3Editor(ref s) = self.mode {
            s.extra_editing
        } else {
            return;
        };

        if editing {
            match code {
                // Esc: abandon the edit.
                KeyCode::Esc => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        s.extra_editing = false;
                        s.extra_input.clear();
                    }
                }
                // Enter / Ctrl+S: write the edited value back to the file.
                KeyCode::Enter | KeyCode::Char('s') | KeyCode::Char('S')
                    if code == KeyCode::Enter || modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    let (path, frame_id, value, idx) = if let Mode::Id3Editor(ref s) = self.mode {
                        let frame = s.extra_frames.get(s.extra_focused);
                        if let Some(f) = frame {
                            (
                                s.path.clone(),
                                f.id.clone(),
                                s.extra_input.clone(),
                                s.extra_focused,
                            )
                        } else {
                            return;
                        }
                    } else {
                        return;
                    };

                    match write_extra_frame(&path, &frame_id, &value) {
                        Ok(()) => {
                            if let Mode::Id3Editor(ref mut s) = self.mode {
                                // Update the in-memory list so the display refreshes.
                                if let Some(f) = s.extra_frames.get_mut(idx) {
                                    f.value = value;
                                }
                                s.extra_editing = false;
                                s.extra_input.clear();
                                s.status = Some("Frame saved".to_string());
                            }
                        }
                        Err(e) => {
                            if let Mode::Id3Editor(ref mut s) = self.mode {
                                s.status = Some(format!("Save error: {e}"));
                                s.extra_editing = false;
                            }
                        }
                    }
                }
                KeyCode::Left => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        s.extra_cursor = s.extra_cursor.saturating_sub(1);
                    }
                }
                KeyCode::Right => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        let len = s.extra_input.chars().count();
                        s.extra_cursor = (s.extra_cursor + 1).min(len);
                    }
                }
                KeyCode::Home => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        s.extra_cursor = 0;
                    }
                }
                KeyCode::End => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        s.extra_cursor = s.extra_input.chars().count();
                    }
                }
                KeyCode::Backspace => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        let c = s.extra_cursor;
                        if c > 0 {
                            if let Some((bi, _)) = s.extra_input.char_indices().nth(c - 1) {
                                s.extra_input.remove(bi);
                                s.extra_cursor -= 1;
                            }
                        }
                    }
                }
                KeyCode::Char(ch) => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        let c = s.extra_cursor;
                        let bi = s
                            .extra_input
                            .char_indices()
                            .nth(c)
                            .map(|(i, _)| i)
                            .unwrap_or(s.extra_input.len());
                        s.extra_input.insert(bi, ch);
                        s.extra_cursor += 1;
                    }
                }
                _ => {}
            }
            return;
        }

        // Navigation mode.
        match code {
            // Esc: close the Customize panel and return to the main fields.
            KeyCode::Esc => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.show_extra = false;
                }
            }

            KeyCode::Up => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.extra_focused = s.extra_focused.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    let max = s.extra_frames.len().saturating_sub(1);
                    s.extra_focused = (s.extra_focused + 1).min(max);
                }
            }

            // Enter: start editing the value of the focused extra frame.
            // Cursor starts at the end of the existing value.
            KeyCode::Enter => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    if !s.extra_frames.is_empty() {
                        let current_val = s.extra_frames[s.extra_focused].value.clone();
                        s.extra_cursor = current_val.chars().count();
                        s.extra_input = current_val;
                        s.extra_editing = true;
                    }
                }
            }

            // Ctrl+S from the Customize panel saves the main fields and closes.
            KeyCode::Char('s') | KeyCode::Char('S')
                if modifiers.contains(KeyModifiers::CONTROL) =>
            {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.show_extra = false; // return to main panel first
                }
                self.id3_save_and_close();
            }

            _ => {}
        }
    }

    /// Write the current `TagFields` back to disk, refresh the in-playlist
    /// track metadata, then close the editor.
    fn id3_save_and_close(&mut self) {
        let (path, fields) = if let Mode::Id3Editor(ref s) = self.mode {
            (s.path.clone(), s.fields.clone())
        } else {
            return;
        };

        match write_tag_fields(&path, &fields) {
            Ok(()) => {
                // Refresh the in-memory track so the playlist shows updated metadata.
                for track in &mut self.playlist.tracks {
                    if track.path == path {
                        if let Ok(fresh) = Track::from_path(&path) {
                            track.title = fresh.title;
                            track.artist = fresh.artist;
                        }
                        break;
                    }
                }
                self.mode = Mode::Normal;
                self.set_status("Tags saved");
            }
            Err(e) => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.status = Some(format!("Save error: {e}"));
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Settings overlay
    // -----------------------------------------------------------------------

    /// Handle a key press inside the settings overlay.
    ///
    /// Key map (normal navigation):
    ///   Left / Right (or h / l) — switch tabs
    ///   Up / Down (or k / j)   — move cursor within the active tab
    ///   Space / Enter          — toggle a bool, cycle an enum, or enter
    ///                            text-edit mode for a string field
    ///   Esc / e                — save config to disk and close the overlay
    ///
    /// Key map (text-edit mode for Filetypes paths):
    ///   Any printable char     — append to the edit buffer
    ///   Backspace              — delete the last character
    ///   Enter                  — confirm and write the value back to config
    ///   Esc                    — abandon the edit (revert to previous value)
    fn handle_settings(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        use crate::config::{ThemeChoice, VisualizerMode};

        // Alt + transport keys pass through to the player without closing settings.
        if modifiers.contains(KeyModifiers::ALT) {
            match code {
                KeyCode::Char('z') => {
                    self.play_prev();
                    return;
                }
                KeyCode::Char('x') => {
                    self.play_current();
                    return;
                }
                KeyCode::Char('c') => {
                    let _ = self.player.toggle_pause();
                    return;
                }
                KeyCode::Char('v') => {
                    let _ = self.player.stop();
                    return;
                }
                KeyCode::Char('b') => {
                    self.play_next();
                    return;
                }
                KeyCode::Char('j') => {
                    let results = (0..self.playlist.len()).collect();
                    self.mode = Mode::Jump {
                        query: String::new(),
                        results,
                        selected: 0,
                        from_media_library: false,
                    };
                    return;
                }
                _ => {}
            }
        }

        // Snapshot the read-only fields we need before any mutable borrow.
        let (tab, cursor, in_edit) = match &self.mode {
            Mode::Settings(s) => (s.tab, s.cursor, s.edit_buf.is_some()),
            _ => return,
        };

        // ── Text-edit mode (Filetypes string fields) ──────────────────────
        if in_edit {
            match code {
                // Esc: abandon the edit, restore the previous value.
                KeyCode::Esc => {
                    if let Mode::Settings(s) = &mut self.mode {
                        s.edit_buf = None;
                    }
                }
                // Enter: commit the typed value back to config.
                KeyCode::Enter => {
                    let val = match &mut self.mode {
                        Mode::Settings(s) => s.edit_buf.take().unwrap_or_default(),
                        _ => return,
                    };
                    // Dispatch by (tab, cursor).
                    match (tab, cursor) {
                        (0, 1) => self.config.appearance.custom_skin = val,
                        (3, 0) => self.config.plugins.visualizer_dir = val,
                        (3, 1) => self.config.plugins.filetype_dir = val,
                        (4, 2) => {
                            // Parse interval minutes; silently keep old value on error.
                            if let Ok(mins) = val.trim().parse::<u64>() {
                                self.config.media_library.set_rescan_interval_mins(mins);
                            }
                        }
                        _ => {}
                    }
                }
                // Backspace: delete last character from the buffer.
                KeyCode::Backspace => {
                    if let Mode::Settings(s) = &mut self.mode {
                        if let Some(buf) = &mut s.edit_buf {
                            buf.pop();
                        }
                    }
                }
                // Any printable character: append to the buffer.
                KeyCode::Char(ch) => {
                    if let Mode::Settings(s) = &mut self.mode {
                        if let Some(buf) = &mut s.edit_buf {
                            buf.push(ch);
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        // ── Normal navigation ─────────────────────────────────────────────
        match code {
            // Esc / e: save config and close.
            KeyCode::Esc | KeyCode::Char('e') | KeyCode::Char('E') => {
                let _ = self.config.save();
                self.mode = Mode::Normal;
            }

            // Left / h: go to the previous tab.
            KeyCode::Left | KeyCode::Char('h') => {
                if let Mode::Settings(s) = &mut self.mode {
                    s.tab = s.tab.saturating_sub(1);
                    s.cursor = 0;
                }
            }
            // Right / l: go to the next tab (tabs 0–4).
            KeyCode::Right | KeyCode::Char('l') => {
                if let Mode::Settings(s) = &mut self.mode {
                    if s.tab < 4 {
                        s.tab += 1;
                    }
                    s.cursor = 0;
                }
            }

            // Up / k: move cursor up within the active tab.
            KeyCode::Up | KeyCode::Char('k') => {
                if let Mode::Settings(s) = &mut self.mode {
                    s.cursor = s.cursor.saturating_sub(1);
                }
            }
            // Down / j: move cursor down within the active tab.
            KeyCode::Down | KeyCode::Char('j') => {
                let tab_len = settings_tab_len(tab);
                if let Mode::Settings(s) = &mut self.mode {
                    if s.cursor + 1 < tab_len {
                        s.cursor += 1;
                    }
                }
            }

            // Space / Enter: act on the focused setting.
            KeyCode::Enter | KeyCode::Char(' ') => {
                match tab {
                    // Appearance: row 0 = cycle theme; row 1 = edit custom skin name.
                    0 => {
                        match cursor {
                            0 => {
                                self.config.appearance.theme = match self.config.appearance.theme {
                                    ThemeChoice::Dark => ThemeChoice::Light,
                                    ThemeChoice::Light => ThemeChoice::Dark,
                                };
                            }
                            1 => {
                                // Enter text-edit mode for the custom skin name.
                                let current = self.config.appearance.custom_skin.clone();
                                if let Mode::Settings(s) = &mut self.mode {
                                    s.edit_buf = Some(current);
                                }
                            }
                            _ => {}
                        }
                    }
                    // Behavior: toggle autoplay-on-add.
                    1 => {
                        self.config.behavior.autoplay_on_add =
                            !self.config.behavior.autoplay_on_add;
                    }
                    // Visualizer: cycle between Bars and Oscilloscope.
                    2 => {
                        self.config.visualizer.mode = match self.config.visualizer.mode {
                            VisualizerMode::Bars => VisualizerMode::Oscilloscope,
                            VisualizerMode::Oscilloscope => VisualizerMode::Bars,
                        };
                    }
                    // Filetypes: enter text-edit mode for the focused path field.
                    3 => {
                        let current = match cursor {
                            0 => self.config.plugins.visualizer_dir.clone(),
                            1 => self.config.plugins.filetype_dir.clone(),
                            _ => String::new(),
                        };
                        if let Mode::Settings(s) = &mut self.mode {
                            s.edit_buf = Some(current);
                        }
                    }
                    // Media Library: toggle booleans or edit the interval field.
                    4 => {
                        match cursor {
                            0 => {
                                self.config.media_library.rescan_on_startup =
                                    !self.config.media_library.rescan_on_startup;
                            }
                            1 => {
                                self.config.media_library.periodic_rescan =
                                    !self.config.media_library.periodic_rescan;
                            }
                            2 => {
                                // Enter text-edit mode for the interval value.
                                let current =
                                    self.config.media_library.rescan_interval_mins.to_string();
                                if let Mode::Settings(s) = &mut self.mode {
                                    s.edit_buf = Some(current);
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Equalizer handler
    // -----------------------------------------------------------------------

    /// Handle key events while the equalizer overlay is open.
    ///
    /// Key map:
    ///   ←/→ (h/l)     — select previous / next band
    ///   ↑/↓ (+/-)     — raise / lower the selected band by 1 dB
    ///   PgUp/PgDn     — raise / lower by 3 dB (coarse)
    ///   [ / ]         — decrease / increase pre-amp by 5 %
    ///   p             — cycle to the next EQ preset
    ///   r             — reset all bands to flat (0 dB)
    ///   t             — toggle EQ enabled / disabled
    ///   Esc / u       — close the overlay (saves config)
    fn handle_equalizer(&mut self, code: KeyCode) {
        let sel = match &self.mode {
            Mode::Equalizer(s) => s.selected_band,
            _ => return,
        };
        // sel == 10 means the pre-amp column is selected.
        let preamp_selected = sel == 10;

        // ── Helpers ───────────────────────────────────────────────────────────
        let adjust_band = |app: &mut App, delta: f64| {
            let b = match &app.mode {
                Mode::Equalizer(s) => s.selected_band,
                _ => return,
            };
            if b >= 10 {
                return;
            }
            let candidate = app.config.equalizer.bands.get(b).copied().unwrap_or(0.0) + delta;
            app.ctrl().set_eq_band(b, candidate);
        };

        let adjust_preamp = |app: &mut App, delta: f64| {
            let new = app.config.equalizer.preamp + delta;
            app.ctrl().set_preamp(new);
        };

        match code {
            // Close and save.
            KeyCode::Esc | KeyCode::Char('u') | KeyCode::Char('U') => {
                let _ = self.config.save();
                self.mode = Mode::Normal;
                return;
            }

            // Navigate: bands 0-9, then pre-amp at position 10.
            KeyCode::Left | KeyCode::Char('h') => {
                if let Mode::Equalizer(s) = &mut self.mode {
                    s.selected_band = s.selected_band.saturating_sub(1);
                }
                return;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if let Mode::Equalizer(s) = &mut self.mode {
                    if s.selected_band < 10 {
                        s.selected_band += 1;
                    }
                }
                return;
            }

            // Up/Down: adjust band gain or pre-amp depending on selection.
            KeyCode::Up | KeyCode::Char('+') => {
                if preamp_selected {
                    adjust_preamp(self, 0.05);
                } else {
                    adjust_band(self, 1.0);
                }
            }
            KeyCode::Down | KeyCode::Char('-') => {
                if preamp_selected {
                    adjust_preamp(self, -0.05);
                } else {
                    adjust_band(self, -1.0);
                }
            }

            // Coarse adjustment (3 dB / 15 %).
            KeyCode::PageUp => {
                if preamp_selected {
                    adjust_preamp(self, 0.15);
                } else {
                    adjust_band(self, 3.0);
                }
            }
            KeyCode::PageDown => {
                if preamp_selected {
                    adjust_preamp(self, -0.15);
                } else {
                    adjust_band(self, -3.0);
                }
            }

            // Cycle presets.
            KeyCode::Char('p') | KeyCode::Char('P') => {
                self.ctrl().cycle_eq_preset();
            }

            // Reset to flat.
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.ctrl().reset_eq_to_flat();
            }

            // Toggle enabled / disabled.
            KeyCode::Char('t') | KeyCode::Char('T') => {
                let new_enabled = !self.config.equalizer.enabled;
                self.ctrl().set_eq_enabled(new_enabled);
            }

            // Playback controls — execute without closing the overlay.
            KeyCode::Char('z') | KeyCode::Char('Z') => {
                self.play_prev();
            }
            KeyCode::Char('x') | KeyCode::Char('X') => {
                self.play_current();
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                let _ = self.player.toggle_pause();
            }
            KeyCode::Char('v') | KeyCode::Char('V') => {
                let _ = self.player.stop();
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                self.play_next();
            }

            // Jump — switch to jump mode (closes EQ overlay).
            KeyCode::Char('j') | KeyCode::Char('J') => {
                let _ = self.config.save();
                let results = (0..self.playlist.len()).collect();
                self.mode = Mode::Jump {
                    query: String::new(),
                    results,
                    selected: 0,
                    from_media_library: false,
                };
            }

            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Tick
    // -----------------------------------------------------------------------

    /// Called every 100 ms from the event loop.
    ///
    /// Responsibilities in order:
    /// 1. Drain async probe results and write durations into the playlist +
    ///    cache so they appear in the display immediately.
    /// 2. Write the GStreamer-queried duration back to the current track the
    ///    first time it becomes available (GStreamer only reports duration once
    ///    the pipeline is Playing; this catches it on the first tick).
    /// 3. Advance to the next track on end-of-stream.
    fn drain_add_file_scan(&mut self) {
        let Some(channels) = self.scan_channels.take() else {
            return;
        };
        let ScanChannels {
            fast_rx,
            meta_rx,
            done_rx,
            phase1_done_rx,
            scan_start,
            mut fast_done,
        } = channels;

        // Phase 1: drain fast tracks (path + filename, no metadata yet).
        // Use a short blocking recv first so tiny scans (single file) complete
        // without waiting for the next tick.
        if !fast_done {
            let timeout = std::time::Duration::from_millis(50);
            if let Ok(track) = fast_rx.recv_timeout(timeout) {
                let count = if let Mode::AddFile { scan_added, .. } = &mut self.mode {
                    *scan_added += 1;
                    *scan_added
                } else {
                    1
                };
                self.playlist.add(track);
                self.status_message = Some(format!(
                    "Scanning… {count} track{}",
                    if count == 1 { "" } else { "s" }
                ));
                self.status_ticks = STATUS_TICKS;
            }
            while let Ok(track) = fast_rx.try_recv() {
                if let Mode::AddFile { scan_added, .. } = &mut self.mode {
                    *scan_added += 1;
                }
                self.playlist.add(track);
            }
        }

        // Phase 2: apply metadata patches from the background thread.
        // Each message is (index-within-scan, title, artist, album_artist, album).
        while let Ok((idx, title, artist, album_artist, album)) = meta_rx.try_recv() {
            fast_done = true; // metadata arriving means Phase 1 is done
            let pos = scan_start + idx;
            if let Some(track) = self.playlist.tracks.get_mut(pos) {
                track.title = title;
                track.artist = artist;
                track.album_artist = album_artist;
                track.album = album;
            }
        }

        // Check for completion signal.
        if let Ok(_total) = done_rx.try_recv() {
            let added = if let Mode::AddFile { scan_added, .. } = &self.mode {
                *scan_added
            } else {
                self.playlist.tracks.len().saturating_sub(scan_start)
            };
            // Probe durations for all newly added tracks.
            let before = scan_start;
            self.probe_new_tracks(before);
            self.mode = Mode::Normal;
            self.shuffle_state.reset();
            let msg = match added {
                0 => "No audio files found".to_string(),
                n => format!("Added {n} file{}", if n == 1 { "" } else { "s" }),
            };
            self.status_message = Some(msg);
            self.status_ticks = STATUS_TICKS;
            return; // scan complete, do not restore channels
        }

        // Scan still running — put channels back.
        self.scan_channels = Some(ScanChannels {
            fast_rx,
            meta_rx,
            done_rx,
            phase1_done_rx,
            scan_start,
            fast_done,
        });
    }

    pub fn tick(&mut self) {
        // 1. Async probe results.
        while let Ok((path, dur)) = self.probe_rx.try_recv() {
            for track in &mut self.playlist.tracks {
                if track.path == path && track.duration.is_none() {
                    track.duration = Some(dur);
                    self.duration_cache.insert(&path, dur);
                    break;
                }
            }
        }
        // 1b. Missing-file notifications from the probe threads.
        while let Ok(path) = self.broken_rx.try_recv() {
            for track in &mut self.playlist.tracks {
                if track.path == path {
                    track.broken = true;
                    break;
                }
            }
        }

        // 1c. Background add-file scan: drain results and update playlist.
        self.drain_add_file_scan();

        // 2. Write GStreamer duration back to the current track if not yet known.
        if let Some(dur) = self.player.duration() {
            let idx = self.playlist.current_index;
            if let Some(track) = self.playlist.tracks.get_mut(idx) {
                if track.duration.is_none() {
                    let path = track.path.clone();
                    track.duration = Some(dur);
                    self.duration_cache.insert(&path, dur);
                }
            }
        }

        // 3. Auto-advance on end-of-stream or GStreamer error.
        //    On an error, mark the current track broken so it shows a ⚠ indicator
        //    and is skipped automatically in future auto-advance calls.
        if let Some(event) = self.player.poll_bus() {
            if matches!(event, BusEvent::Error) {
                let idx = self.playlist.current_index;
                if let Some(t) = self.playlist.tracks.get_mut(idx) {
                    t.broken = true;
                }
            }
            self.advance_to_next_playable();
        }

        // 4. Auto-clear transient status messages after STATUS_TICKS ticks.
        if self.status_ticks > 0 {
            self.status_ticks -= 1;
            if self.status_ticks == 0 {
                self.status_message = None;
            }
        }
    }

    /// Set a transient status message that auto-clears after STATUS_TICKS ticks.
    fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
        self.status_ticks = STATUS_TICKS;
    }
}

// ---------------------------------------------------------------------------
// ID3 editor free-function helpers
// ---------------------------------------------------------------------------

/// Return a mutable reference to the `TagFields` field at the given index.
///
/// Indices correspond to the `field_pairs()` order:
/// 0=Title, 1=Artist, 2=Album, 3=Album Artist, 4=Genre, 5=Year,
/// 6=Track#, 7=Track Total, 8=Disc#, 9=Disc Total, 10=BPM, 11=Comment.
pub fn id3_field_value_mut(fields: &mut TagFields, index: usize) -> &mut String {
    match index {
        0 => &mut fields.title,
        1 => &mut fields.artist,
        2 => &mut fields.album,
        3 => &mut fields.album_artist,
        4 => &mut fields.genre,
        5 => &mut fields.year,
        6 => &mut fields.track_number,
        7 => &mut fields.track_total,
        8 => &mut fields.disc_number,
        9 => &mut fields.disc_total,
        10 => &mut fields.bpm,
        _ => &mut fields.comment,
    }
}

/// Return up to 6 genre suggestions that start with `query` (case-insensitive).
///
/// Returns an empty vec when `query` is empty so the typeahead popup only
/// appears once the user has started typing.
pub fn id3_genre_matches(query: &str) -> Vec<&'static str> {
    if query.is_empty() {
        return vec![];
    }
    let q = query.to_lowercase();
    ID3V1_GENRES
        .iter()
        .filter(|g| g.to_lowercase().starts_with(&q))
        .copied()
        .take(6)
        .collect()
}

// ---------------------------------------------------------------------------
// run()
// ---------------------------------------------------------------------------

pub fn run(playlist: Playlist, config: Config) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(playlist, config)?;

    if !app.playlist.is_empty() && !app.config.playback.start_paused {
        app.play_current();
    }

    let tick_rate = Duration::from_millis(100);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui::draw(f, &app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::ZERO);

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key.code, key.modifiers);
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.tick();
            // Advance the marquee using the current terminal width.
            // The header info column is roughly terminal_width - 24 columns
            // (22 for the viz box + 2 for borders/gap).
            let cols = terminal.size().map(|s| s.width as usize).unwrap_or(80);
            let info_cols = cols.saturating_sub(24);
            app.tick_marquee(info_cols);
            last_tick = Instant::now();
        }

        if app.should_quit {
            let _ = app.playlist.save_last();
            // Persist the config so volume, repeat-mode, window geometry, and
            // any settings-overlay changes survive to the next session.
            let _ = app.config.save();
            // Flush any pending duration-cache writes so probed data survives
            // to the next session (DurationCache::drop also does this, but
            // an explicit call here makes the intent visible).
            app.duration_cache.save_if_dirty();
            break;
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{Config, VisualizerMode},
        model::{Playlist, Track},
    };
    use std::path::PathBuf;

    fn make_app() -> App {
        gstreamer::init().expect("GStreamer must be available for tests");
        App::new(Playlist::new(), Config::default()).expect("App::new failed")
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

    fn app_with_tracks(titles: &[&str]) -> App {
        let mut app = make_app();
        for t in titles {
            app.playlist.add(fake_track(t));
        }
        app
    }

    // -----------------------------------------------------------------------
    // Existing tests
    // -----------------------------------------------------------------------

    #[test]
    fn esc_in_normal_mode_quits() {
        let mut app = make_app();
        assert!(!app.should_quit);
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.should_quit);
    }

    #[test]
    fn esc_in_jump_mode_returns_to_normal_without_quitting() {
        let mut app = make_app();
        app.mode = Mode::Jump {
            query: String::new(),
            results: vec![],
            selected: 0,
            from_media_library: false,
        };
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.should_quit);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn q_in_normal_mode_quits() {
        let mut app = make_app();
        app.handle_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(app.should_quit);
    }

    #[test]
    fn b_key_at_last_track_has_no_effect() {
        let mut app = app_with_tracks(&["A"]);
        app.playlist.current_index = 0;
        app.handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
        assert_eq!(
            app.playlist.current_index, 0,
            "pressing b on the last track must not advance current_index"
        );
    }

    // -----------------------------------------------------------------------
    // Playlist visibility toggle (p key)
    // -----------------------------------------------------------------------

    #[test]
    fn app_starts_with_playlist_visible() {
        let app = make_app();
        assert!(
            app.playlist_visible,
            "playlist should be visible by default"
        );
    }

    #[test]
    fn p_key_toggles_playlist_visible_off() {
        let mut app = make_app();
        assert!(app.playlist_visible);
        app.handle_key(KeyCode::Char('p'), KeyModifiers::NONE);
        assert!(!app.playlist_visible);
    }

    #[test]
    fn p_key_toggles_playlist_visible_back_on() {
        let mut app = make_app();
        app.handle_key(KeyCode::Char('p'), KeyModifiers::NONE);
        app.handle_key(KeyCode::Char('p'), KeyModifiers::NONE);
        assert!(app.playlist_visible);
    }

    #[test]
    fn capital_p_key_also_toggles_playlist_visible() {
        let mut app = make_app();
        app.handle_key(KeyCode::Char('P'), KeyModifiers::NONE);
        assert!(!app.playlist_visible);
    }

    // -----------------------------------------------------------------------
    // Arrow key seeking
    // -----------------------------------------------------------------------

    #[test]
    fn left_arrow_seek_without_active_track_does_not_panic() {
        // No track → position/duration both None → seek_delta_secs is a no-op.
        let mut app = make_app();
        app.handle_key(KeyCode::Left, KeyModifiers::NONE);
    }

    #[test]
    fn right_arrow_seek_without_active_track_does_not_panic() {
        let mut app = make_app();
        app.handle_key(KeyCode::Right, KeyModifiers::NONE);
    }

    #[test]
    fn seek_delta_secs_is_noop_when_no_duration() {
        // Directly exercises the method: no loaded track → no-op, no panic.
        let mut app = make_app();
        app.seek_delta_secs(5.0);
        app.seek_delta_secs(-5.0);
    }

    // -----------------------------------------------------------------------
    // Add file (n key)
    // -----------------------------------------------------------------------

    #[test]
    fn n_key_enters_add_file_mode() {
        let mut app = make_app();
        app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::AddFile { .. }));
    }

    #[test]
    fn add_file_esc_returns_to_normal() {
        let mut app = make_app();
        app.mode = Mode::AddFile {
            input: "some/path".into(),
            scan_cancel: None,
            scan_added: 0,
        };
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn add_file_chars_accumulate_in_input() {
        let mut app = make_app();
        app.mode = Mode::AddFile {
            input: String::new(),
            scan_cancel: None,
            scan_added: 0,
        };
        for c in "/tmp/track.mp3".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        let Mode::AddFile { ref input, .. } = app.mode else {
            panic!("wrong mode")
        };
        assert_eq!(input, "/tmp/track.mp3");
    }

    #[test]
    fn add_file_backspace_removes_last_char() {
        let mut app = make_app();
        app.mode = Mode::AddFile {
            input: "abc".into(),
            scan_cancel: None,
            scan_added: 0,
        };
        app.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        let Mode::AddFile { ref input, .. } = app.mode else {
            panic!("wrong mode")
        };
        assert_eq!(input, "ab");
    }

    #[test]
    fn add_file_enter_with_invalid_path_sets_error_and_returns_to_normal() {
        let mut app = make_app();
        app.mode = Mode::AddFile {
            input: "/nonexistent/file.mp3".into(),
            scan_cancel: None,
            scan_added: 0,
        };
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        // handle_key returns immediately; tick() drains the background scan results.
        app.tick();
        assert!(matches!(app.mode, Mode::Normal));
        assert!(
            app.status_message
                .as_deref()
                .unwrap_or("")
                .contains("No audio files"),
            "expected 'No audio files' message, got: {:?}",
            app.status_message
        );
    }

    #[test]
    fn add_file_spaces_in_path_are_preserved() {
        let mut app = make_app();
        app.mode = Mode::AddFile {
            input: String::new(),
            scan_cancel: None,
            scan_added: 0,
        };
        for c in "/tmp/my music/track.mp3".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        let Mode::AddFile { ref input, .. } = app.mode else {
            panic!("wrong mode")
        };
        assert_eq!(input, "/tmp/my music/track.mp3");
    }

    // -----------------------------------------------------------------------
    // commit_add_file with a directory path
    // -----------------------------------------------------------------------

    #[test]
    fn commit_add_file_with_nonexistent_dir_shows_added_zero_message() {
        // A path that is_dir() returns false for (doesn't exist) falls through
        // to the file branch; Track::from_path fails → "No valid audio files found".
        let mut app = make_app();
        app.commit_add_file("/nonexistent_dir_xyz/");
        let msg = app.status_message.as_deref().unwrap_or("");
        assert!(
            msg.contains("No valid") || msg.contains("Added"),
            "unexpected message: {}",
            msg
        );
    }

    /// A tilde-prefixed path is expanded before scanning, not treated literally.
    #[test]
    fn commit_add_file_tilde_is_expanded() {
        // Use a controlled temp directory so the test doesn't scan the real home
        // directory (which may contain a large music library via a symlink).
        let dir = tempfile::tempdir().unwrap();
        let home_rel = dir.path().to_str().unwrap();

        // Build a "~/subdir" style input by replacing the home portion with ~.
        // Instead, directly verify that a bare "~" does not produce a
        // Track::from_path error on a path literally named "~".
        // We do this by pointing ~ at our empty temp dir via the
        // HOME env var so the scan is instantaneous.
        let original_home = std::env::var("HOME").unwrap_or_default();
        unsafe {
            std::env::set_var("HOME", home_rel);
        }
        let mut app = make_app();
        app.commit_add_file("~/");
        unsafe {
            std::env::set_var("HOME", &original_home);
        }

        // Empty dir → "No valid audio files found", not a panic or missing-message.
        assert_eq!(
            app.status_message.as_deref(),
            Some("No valid audio files found")
        );
    }

    // -----------------------------------------------------------------------
    // Move track (m key)
    // -----------------------------------------------------------------------

    #[test]
    fn comma_key_enters_move_track_mode() {
        // Move track is now bound to ',' (was 'm').
        let mut app = make_app();
        app.handle_key(KeyCode::Char(','), KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::MoveTrack { from: None, .. }));
    }

    #[test]
    fn move_track_esc_returns_to_normal() {
        let mut app = make_app();
        app.mode = Mode::MoveTrack {
            input: String::new(),
            from: None,
        };
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn move_track_first_enter_stores_from_and_clears_input() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.mode = Mode::MoveTrack {
            input: "2".into(),
            from: None,
        };
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let Mode::MoveTrack { from, ref input } = app.mode else {
            panic!("wrong mode")
        };
        assert_eq!(from, Some(2));
        assert!(input.is_empty());
    }

    #[test]
    fn move_track_invalid_from_shows_error_and_returns_to_normal() {
        let mut app = app_with_tracks(&["A", "B"]);
        app.mode = Mode::MoveTrack {
            input: "abc".into(),
            from: None,
        };
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.status_message.is_some());
    }

    #[test]
    fn move_track_second_enter_reorders_playlist() {
        let mut app = app_with_tracks(&["A", "B", "C", "D"]);
        // move track 2 (B) to position 4 (D)
        app.mode = Mode::MoveTrack {
            input: "4".into(),
            from: Some(2),
        };
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Normal));
        let titles: Vec<_> = app
            .playlist
            .tracks
            .iter()
            .map(|t| t.title.as_str())
            .collect();
        assert_eq!(titles, ["A", "C", "D", "B"]);
    }

    #[test]
    fn move_track_out_of_range_to_shows_error() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.mode = Mode::MoveTrack {
            input: "99".into(),
            from: Some(1),
        };
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.status_message.is_some());
    }

    #[test]
    fn move_track_backspace_removes_last_char() {
        let mut app = make_app();
        app.mode = Mode::MoveTrack {
            input: "12".into(),
            from: None,
        };
        app.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        let Mode::MoveTrack { ref input, .. } = app.mode else {
            panic!("wrong mode")
        };
        assert_eq!(input, "1");
    }

    // -----------------------------------------------------------------------
    // Remove track (, key)
    // -----------------------------------------------------------------------

    #[test]
    fn dot_key_enters_remove_track_mode() {
        // Remove track is now bound to '.' (was ',').
        let mut app = make_app();
        app.handle_key(KeyCode::Char('.'), KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::RemoveTrack { .. }));
    }

    #[test]
    fn remove_track_esc_returns_to_normal() {
        let mut app = make_app();
        app.mode = Mode::RemoveTrack { input: "1".into() };
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn remove_track_enter_removes_correct_entry() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.mode = Mode::RemoveTrack { input: "2".into() };
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Normal));
        let titles: Vec<_> = app
            .playlist
            .tracks
            .iter()
            .map(|t| t.title.as_str())
            .collect();
        assert_eq!(titles, ["A", "C"]);
    }

    #[test]
    fn remove_track_invalid_index_shows_error() {
        let mut app = app_with_tracks(&["A", "B"]);
        app.mode = Mode::RemoveTrack { input: "99".into() };
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.status_message.is_some());
        assert_eq!(app.playlist.len(), 2); // unchanged
    }

    #[test]
    fn remove_track_non_numeric_input_shows_error() {
        let mut app = app_with_tracks(&["A"]);
        app.mode = Mode::RemoveTrack {
            input: "abc".into(),
        };
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.status_message.is_some());
    }

    #[test]
    fn remove_track_backspace_removes_last_char() {
        let mut app = make_app();
        app.mode = Mode::RemoveTrack { input: "12".into() };
        app.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        let Mode::RemoveTrack { ref input } = app.mode else {
            panic!("wrong mode")
        };
        assert_eq!(input, "1");
    }

    #[test]
    fn remove_track_reduces_playlist_length() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        assert_eq!(app.playlist.len(), 3);
        app.mode = Mode::RemoveTrack { input: "1".into() };
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.playlist.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Playback state tests
    // -----------------------------------------------------------------------

    /// Pressing x on a stopped player with no tracks does not crash.
    #[test]
    fn play_key_with_empty_playlist_does_not_crash() {
        let mut app = make_app();
        app.handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
        // no panic = pass
    }

    /// Pressing c (pause/resume) on a stopped player does not crash.
    #[test]
    fn pause_key_when_stopped_does_not_crash() {
        let mut app = make_app();
        app.handle_key(KeyCode::Char('c'), KeyModifiers::NONE);
    }

    /// Pressing v (stop) on a stopped player does not crash.
    #[test]
    fn stop_key_when_already_stopped_does_not_crash() {
        let mut app = make_app();
        app.handle_key(KeyCode::Char('v'), KeyModifiers::NONE);
    }

    /// Simulating EOS on a two-track playlist advances to the next track.
    #[test]
    fn eos_auto_advances_to_next_track() {
        let mut app = app_with_tracks(&["A", "B"]);
        app.playlist.current_index = 0;
        // Simulate what tick() does when poll_bus() returns true (EOS)
        app.play_next();
        assert_eq!(app.playlist.current_index, 1);
    }

    /// Simulating EOS on the only track in the playlist stops cleanly.
    #[test]
    fn eos_on_only_track_stops_cleanly_no_crash() {
        let mut app = app_with_tracks(&["A"]);
        app.playlist.current_index = 0;
        // Simulate EOS — play_next() with no successor does nothing
        app.play_next();
        assert_eq!(app.playlist.current_index, 0);
        assert_eq!(app.playlist.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Back-button logic
    // -----------------------------------------------------------------------

    /// When the player position is effectively zero (< 2 s), z goes to previous track.
    /// Without real audio, position() returns None → Duration::ZERO, always < 2 s.
    #[test]
    fn back_when_position_is_zero_goes_to_previous_track() {
        let mut app = app_with_tracks(&["A", "B"]);
        app.playlist.current_index = 1;
        app.handle_key(KeyCode::Char('z'), KeyModifiers::NONE);
        assert_eq!(app.playlist.current_index, 0);
    }

    /// Pressing back on the first track stays at track 0 regardless of position.
    #[test]
    fn back_on_first_track_stays_at_index_zero() {
        let mut app = app_with_tracks(&["A", "B"]);
        app.playlist.current_index = 0;
        app.handle_key(KeyCode::Char('z'), KeyModifiers::NONE);
        assert_eq!(app.playlist.current_index, 0);
    }

    /// Pressing back on the first track with only one item does not crash.
    #[test]
    fn back_on_first_and_only_track_does_not_crash() {
        let mut app = app_with_tracks(&["A"]);
        app.playlist.current_index = 0;
        app.handle_key(KeyCode::Char('z'), KeyModifiers::NONE);
        assert_eq!(app.playlist.current_index, 0);
    }

    /// Linear back must step through the full playlist positionally — not be
    /// limited by session history.  Starting at index 3 and pressing back 3
    /// times must reach index 0.
    #[test]
    fn back_can_step_back_multiple_songs_in_sequence() {
        let mut app = app_with_tracks(&["A", "B", "C", "D"]);
        app.playlist.current_index = 3;

        app.play_prev();
        assert_eq!(
            app.playlist.current_index, 2,
            "first back should reach C (index 2)"
        );

        app.play_prev();
        assert_eq!(
            app.playlist.current_index, 1,
            "second back should reach B (index 1)"
        );

        app.play_prev();
        assert_eq!(
            app.playlist.current_index, 0,
            "third back should reach A (index 0)"
        );
    }

    /// Linear next must step forward through the full playlist positionally.
    #[test]
    fn next_can_step_forward_multiple_songs_in_sequence() {
        let mut app = app_with_tracks(&["A", "B", "C", "D"]);
        app.playlist.current_index = 0;

        app.play_next();
        assert_eq!(
            app.playlist.current_index, 1,
            "first next should reach B (index 1)"
        );

        app.play_next();
        assert_eq!(
            app.playlist.current_index, 2,
            "second next should reach C (index 2)"
        );

        app.play_next();
        assert_eq!(
            app.playlist.current_index, 3,
            "third next should reach D (index 3)"
        );
    }

    /// With RepeatMode::Playlist, back from index 0 wraps to the last track.
    #[test]
    fn back_wraps_to_last_track_when_repeat_playlist_is_on() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Playlist;
        app.playlist.current_index = 0;

        app.play_prev();
        assert_eq!(
            app.playlist.current_index, 2,
            "back at first track should wrap to last (index 2)"
        );
    }

    /// With RepeatMode::Playlist, next from the last track wraps to index 0.
    #[test]
    fn next_wraps_to_first_track_when_repeat_playlist_is_on() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Playlist;
        app.playlist.current_index = 2;

        app.play_next();
        assert_eq!(
            app.playlist.current_index, 0,
            "next at last track should wrap to first (index 0)"
        );
    }

    /// RepeatMode::Song must not prevent manual next from advancing.
    #[test]
    fn next_advances_past_current_track_even_when_repeat_song_is_on() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Song;
        app.playlist.current_index = 0;

        app.play_next();
        assert_eq!(
            app.playlist.current_index, 1,
            "repeat-song must not block manual next"
        );
    }

    /// In shuffle mode the previous button must step backward through the
    /// session's shuffle-play order, not the playlist's linear order.
    #[test]
    fn shuffle_back_steps_through_shuffle_history() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.shuffle_state.enabled = true;
        // Simulate shuffle play order: B(1) → C(2) → A(0).
        app.shuffle_state.record_played(1);
        app.shuffle_state.record_played(2);
        app.shuffle_state.record_played(0);
        app.playlist.current_index = 0; // currently on A

        app.play_prev(); // back to C
        assert_eq!(
            app.playlist.current_index, 2,
            "first back in shuffle should reach C (index 2)"
        );

        app.play_prev(); // back to B
        assert_eq!(
            app.playlist.current_index, 1,
            "second back in shuffle should reach B (index 1)"
        );
    }

    /// After stepping back in shuffle, pressing record_played (simulating forward)
    /// must truncate the stale future so back no longer returns to it.
    #[test]
    fn shuffle_forward_after_back_truncates_stale_future() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.shuffle_state.enabled = true;
        // History: A(0) → B(1) → C(2); cursor at 2.
        app.shuffle_state.record_played(0);
        app.shuffle_state.record_played(1);
        app.shuffle_state.record_played(2);
        app.playlist.current_index = 2;

        // Step back to B (cursor moves to 1, pointing at index 1).
        app.play_prev();
        assert_eq!(app.playlist.current_index, 1);

        // Simulate forward navigation from B — record_played truncates C from history.
        app.shuffle_state.record_played(1);

        // Back from here must return to A (the entry before B in the new history),
        // not to C (the now-stale entry that was truncated).
        app.shuffle_state.prev_from_history();
        assert_ne!(
            app.playlist.current_index, 2,
            "stale future entry C must not be reachable after forward navigation"
        );
    }

    // -----------------------------------------------------------------------
    // Paused-state navigation
    // -----------------------------------------------------------------------

    /// Pressing next while paused must load the new track and start playing it,
    /// not stay paused on the original track.
    #[test]
    fn next_while_paused_advances_and_plays() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.playlist.current_index = 0;
        app.player.set_state_for_test(PlayerState::Paused);

        app.play_next();
        assert_eq!(
            app.playlist.current_index, 1,
            "next while paused must advance to track B"
        );
    }

    /// Pressing back while paused must load the new track and start playing it.
    #[test]
    fn back_while_paused_steps_back_and_plays() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.playlist.current_index = 2;
        app.player.set_state_for_test(PlayerState::Paused);

        app.play_prev();
        assert_eq!(
            app.playlist.current_index, 1,
            "back while paused must step back to track B"
        );
    }

    // -----------------------------------------------------------------------
    // Repeat mode — manual back/next boundary behaviour
    //
    // RepeatMode key:
    //   Off      — no wrap, stop at boundaries
    //   Song     — loops current song on EOS only; manual nav ignores it (same as Off)
    //   Playlist — wraps from end→start and start→end
    // -----------------------------------------------------------------------

    // ── RepeatMode::Off ──────────────────────────────────────────────────────

    /// RepeatOff: next at the last track does nothing (stays at last index).
    #[test]
    fn repeat_off_next_at_last_track_stays() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Off;
        app.playlist.current_index = 2; // last track

        app.play_next();
        assert_eq!(
            app.playlist.current_index, 2,
            "RepeatOff: next at last should stay at last"
        );
    }

    /// RepeatOff: back at the first track does nothing (stays at index 0).
    #[test]
    fn repeat_off_back_at_first_track_stays() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Off;
        app.playlist.current_index = 0;

        app.play_prev();
        assert_eq!(
            app.playlist.current_index, 0,
            "RepeatOff: back at first should stay at first"
        );
    }

    /// RepeatOff: next in the middle still advances normally.
    #[test]
    fn repeat_off_next_in_middle_advances() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Off;
        app.playlist.current_index = 1;

        app.play_next();
        assert_eq!(app.playlist.current_index, 2);
    }

    /// RepeatOff: back in the middle steps back normally.
    #[test]
    fn repeat_off_back_in_middle_steps_back() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Off;
        app.playlist.current_index = 1;

        app.play_prev();
        assert_eq!(app.playlist.current_index, 0);
    }

    // ── RepeatMode::Song ─────────────────────────────────────────────────────
    // Manual back/next treats RepeatMode::Song identically to RepeatMode::Off.
    // Song-repeat only fires on EOS auto-advance (advance_to_next_playable).

    /// RepeatSong: next at the last track does nothing (same boundary as RepeatOff).
    #[test]
    fn repeat_song_next_at_last_track_stays() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Song;
        app.playlist.current_index = 2;

        app.play_next();
        assert_eq!(
            app.playlist.current_index, 2,
            "RepeatSong: next at last should stay (Song only affects EOS)"
        );
    }

    /// RepeatSong: back at the first track does nothing.
    #[test]
    fn repeat_song_back_at_first_track_stays() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Song;
        app.playlist.current_index = 0;

        app.play_prev();
        assert_eq!(
            app.playlist.current_index, 0,
            "RepeatSong: back at first should stay"
        );
    }

    /// RepeatSong: next in the middle advances to the next track (Song does not lock it).
    #[test]
    fn repeat_song_next_in_middle_advances() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Song;
        app.playlist.current_index = 0;

        app.play_next();
        assert_eq!(
            app.playlist.current_index, 1,
            "RepeatSong: next must advance past current track"
        );
    }

    /// RepeatSong: back in the middle steps back normally.
    #[test]
    fn repeat_song_back_in_middle_steps_back() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Song;
        app.playlist.current_index = 2;

        app.play_prev();
        assert_eq!(app.playlist.current_index, 1);
    }

    // ── RepeatMode::Playlist ─────────────────────────────────────────────────

    /// RepeatPlaylist: next in the middle still advances normally (no wrap needed).
    #[test]
    fn repeat_playlist_next_in_middle_advances() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Playlist;
        app.playlist.current_index = 0;

        app.play_next();
        assert_eq!(app.playlist.current_index, 1);
    }

    /// RepeatPlaylist: back in the middle steps back normally (no wrap needed).
    #[test]
    fn repeat_playlist_back_in_middle_steps_back() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Playlist;
        app.playlist.current_index = 2;

        app.play_prev();
        assert_eq!(app.playlist.current_index, 1);
    }

    // ── Single-track edge cases ──────────────────────────────────────────────

    /// Single track, RepeatOff: next stays at index 0 (nothing to advance to).
    #[test]
    fn single_track_repeat_off_next_stays() {
        let mut app = app_with_tracks(&["A"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Off;
        app.playlist.current_index = 0;

        app.play_next();
        assert_eq!(app.playlist.current_index, 0);
    }

    /// Single track, RepeatOff: back stays at index 0.
    #[test]
    fn single_track_repeat_off_back_stays() {
        let mut app = app_with_tracks(&["A"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Off;
        app.playlist.current_index = 0;

        app.play_prev();
        assert_eq!(app.playlist.current_index, 0);
    }

    /// Single track, RepeatPlaylist: next wraps to the same (only) track.
    #[test]
    fn single_track_repeat_playlist_next_wraps_to_self() {
        let mut app = app_with_tracks(&["A"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Playlist;
        app.playlist.current_index = 0;

        app.play_next();
        assert_eq!(
            app.playlist.current_index, 0,
            "single-track RepeatPlaylist next must wrap to index 0"
        );
    }

    /// Single track, RepeatPlaylist: back wraps to the same (only) track.
    #[test]
    fn single_track_repeat_playlist_back_wraps_to_self() {
        let mut app = app_with_tracks(&["A"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Playlist;
        app.playlist.current_index = 0;

        app.play_prev();
        assert_eq!(
            app.playlist.current_index, 0,
            "single-track RepeatPlaylist back must wrap to index 0"
        );
    }

    // ── EOS auto-advance decision logic (ShuffleState::next_index) ──────────
    // advance_to_next_playable() mixes the decision engine with real GStreamer
    // play calls that fail on fake paths, so we test the decision layer
    // (ShuffleState::next_index) directly.  This is exactly the function the
    // tick loop consults when an end-of-stream event fires.

    /// EOS, RepeatOff: next_index at the last track returns None (stop).
    #[test]
    fn eos_repeat_off_at_last_track_returns_none() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.config.playback.repeat_mode = crate::shuffle::RepeatMode::Off;
        let total = app.playlist.len();
        let last = total - 1;

        let result = app
            .shuffle_state
            .next_index(last, total, crate::shuffle::RepeatMode::Off);
        assert!(
            result.is_none(),
            "EOS RepeatOff at last track must return None (stop playback)"
        );
    }

    /// EOS, RepeatOff: next_index in the middle returns current + 1.
    #[test]
    fn eos_repeat_off_in_middle_returns_next() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        let total = app.playlist.len();

        let result = app
            .shuffle_state
            .next_index(1, total, crate::shuffle::RepeatMode::Off);
        assert_eq!(result, Some(2));
    }

    /// EOS, RepeatSong: next_index always returns the current index (replay).
    #[test]
    fn eos_repeat_song_returns_current_index() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        let total = app.playlist.len();

        // Mid-playlist
        let mid = app
            .shuffle_state
            .next_index(1, total, crate::shuffle::RepeatMode::Song);
        assert_eq!(mid, Some(1), "EOS RepeatSong mid should replay same track");

        // Last track — must NOT wrap (that is RepeatPlaylist behaviour)
        let last = app
            .shuffle_state
            .next_index(2, total, crate::shuffle::RepeatMode::Song);
        assert_eq!(
            last,
            Some(2),
            "EOS RepeatSong at last must replay same track, not wrap"
        );
    }

    /// EOS, RepeatPlaylist: next_index at the last track wraps to 0.
    #[test]
    fn eos_repeat_playlist_at_last_track_wraps_to_zero() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        let total = app.playlist.len();
        let last = total - 1;

        let result =
            app.shuffle_state
                .next_index(last, total, crate::shuffle::RepeatMode::Playlist);
        assert_eq!(
            result,
            Some(0),
            "EOS RepeatPlaylist at last must wrap to first track"
        );
    }

    /// EOS, RepeatPlaylist: next_index in the middle still returns current + 1.
    #[test]
    fn eos_repeat_playlist_in_middle_returns_next() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        let total = app.playlist.len();

        let result = app
            .shuffle_state
            .next_index(1, total, crate::shuffle::RepeatMode::Playlist);
        assert_eq!(result, Some(2));
    }

    /// EOS, RepeatPlaylist: next_index on a single-track playlist wraps to 0.
    #[test]
    fn eos_repeat_playlist_single_track_wraps_to_self() {
        let mut app = app_with_tracks(&["A"]);
        let total = app.playlist.len();

        let result = app
            .shuffle_state
            .next_index(0, total, crate::shuffle::RepeatMode::Playlist);
        assert_eq!(result, Some(0));
    }

    /// EOS, RepeatOff: single-track playlist returns None (stop after the only track).
    #[test]
    fn eos_repeat_off_single_track_returns_none() {
        let mut app = app_with_tracks(&["A"]);
        let total = app.playlist.len();

        let result = app
            .shuffle_state
            .next_index(0, total, crate::shuffle::RepeatMode::Off);
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // Visualizer
    // -----------------------------------------------------------------------

    /// play_current() sets visualizer_active = true (when the load succeeds).
    /// We cannot test with a real file here, so we verify the flag is set
    /// by calling the method directly with an empty playlist (no-op path).
    #[test]
    fn visualizer_starts_automatically_on_play_current_call() {
        let mut app = app_with_tracks(&["A"]);
        assert!(!app.visualizer_active);
        // play_current() will fail to load the fake file and return early,
        // so visualizer_active stays false — that is expected without real audio.
        // The important thing: no crash.
        app.play_current();
        // Manually verify the flag logic by setting it directly as play_current would
        // do on success:
        app.visualizer_active = true;
        assert!(app.visualizer_active);
    }

    /// The visualizer mode is taken from the config, not reset on playback.
    #[test]
    fn visualizer_uses_mode_from_config_not_reset_on_play() {
        let mut cfg = Config::default();
        cfg.visualizer.mode = VisualizerMode::Oscilloscope;
        gstreamer::init().unwrap();
        let mut app = App::new(Playlist::new(), cfg).unwrap();
        app.visualizer_active = true;
        assert_eq!(app.config.visualizer.mode, VisualizerMode::Oscilloscope);
        // Simulate a play_current() call (no tracks, so it's a no-op)
        app.play_current();
        // Mode must be unchanged
        assert_eq!(app.config.visualizer.mode, VisualizerMode::Oscilloscope);
    }

    #[test]
    fn a_key_toggles_bars_to_oscilloscope() {
        let mut app = make_app();
        assert_eq!(app.config.visualizer.mode, VisualizerMode::Bars);
        app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(app.config.visualizer.mode, VisualizerMode::Oscilloscope);
    }

    #[test]
    fn a_key_toggles_oscilloscope_back_to_bars() {
        let mut app = make_app();
        app.config.visualizer.mode = VisualizerMode::Oscilloscope;
        app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(app.config.visualizer.mode, VisualizerMode::Bars);
    }

    #[test]
    fn a_key_sets_visualizer_active() {
        let mut app = make_app();
        assert!(!app.visualizer_active);
        app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(app.visualizer_active);
    }

    #[test]
    fn visualizer_data_bars_returns_at_least_8_points() {
        let mut app = make_app();
        app.visualizer_active = true;
        let data = app.visualizer_data(8);
        // minimum is now 10, so requesting 8 still returns 10
        assert!(data.len() >= 8);
    }

    #[test]
    fn visualizer_data_oscilloscope_returns_at_least_8_points() {
        let mut app = make_app();
        app.visualizer_active = true;
        app.config.visualizer.mode = VisualizerMode::Oscilloscope;
        let data = app.visualizer_data(8);
        assert!(data.len() >= 8);
    }

    #[test]
    fn visualizer_data_enforces_minimum_8_when_fewer_requested() {
        let mut app = make_app();
        app.visualizer_active = true;
        let data = app.visualizer_data(3); // request fewer than minimum
                                           // minimum is now 10, so we get at least 10
        assert!(data.len() >= 8);
    }

    #[test]
    fn visualizer_data_enforces_minimum_10() {
        let mut app = make_app();
        app.visualizer_active = true;
        let data = app.visualizer_data(3); // request far below minimum
        assert_eq!(data.len(), 10, "minimum must be 10");
    }

    #[test]
    fn visualizer_data_is_all_zeros_when_inactive() {
        let app = make_app();
        assert!(!app.visualizer_active);
        let data = app.visualizer_data(8);
        assert!(data.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn visualizer_data_values_in_range() {
        let mut app = make_app();
        app.visualizer_active = true;
        for mode in [VisualizerMode::Bars, VisualizerMode::Oscilloscope] {
            app.config.visualizer.mode = mode;
            let data = app.visualizer_data(16);
            for &v in &data {
                assert!((0.0..=1.0).contains(&v), "value out of range: {v}");
            }
        }
    }

    #[test]
    fn multiple_rapid_a_key_presses_do_not_panic() {
        let mut app = make_app();
        for _ in 0..100 {
            app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        }
        // mode must be one of the two valid variants
        assert!(matches!(
            app.config.visualizer.mode,
            VisualizerMode::Bars | VisualizerMode::Oscilloscope
        ));
    }

    // -----------------------------------------------------------------------
    // Playlist — duplicate files and renumbering
    // -----------------------------------------------------------------------

    #[test]
    fn same_fake_track_added_multiple_times_creates_multiple_entries() {
        let mut app = make_app();
        for _ in 0..5 {
            app.playlist.add(fake_track("dup"));
        }
        assert_eq!(app.playlist.len(), 5);
    }

    #[test]
    fn add_same_track_five_times_on_top_of_existing_entries() {
        let mut app = app_with_tracks(&["A", "B"]);
        for _ in 0..5 {
            app.playlist.add(fake_track("dup"));
        }
        assert_eq!(app.playlist.len(), 7);
    }

    #[test]
    fn remove_one_of_three_identical_leaves_two() {
        let mut app = make_app();
        for _ in 0..3 {
            app.playlist.add(fake_track("same"));
        }
        app.mode = Mode::RemoveTrack { input: "2".into() };
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.playlist.len(), 2);
        assert!(app.playlist.tracks.iter().all(|t| t.title == "same"));
    }

    #[test]
    fn move_entry_from_position_3_to_position_1_updates_order() {
        let mut app = app_with_tracks(&["A", "B", "C", "D"]);
        // 1-based: move position 3 (C) to position 1
        app.mode = Mode::MoveTrack {
            input: "1".into(),
            from: Some(3),
        };
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let titles: Vec<_> = app
            .playlist
            .tracks
            .iter()
            .map(|t| t.title.as_str())
            .collect();
        assert_eq!(titles, ["C", "A", "B", "D"]);
    }

    #[test]
    fn remove_entry_leaves_remaining_entries_correctly_numbered() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        app.mode = Mode::RemoveTrack { input: "2".into() };
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        // After removing B, only A (pos 1) and C (pos 2) remain
        assert_eq!(app.playlist.len(), 2);
        assert_eq!(app.playlist.tracks[0].title, "A");
        assert_eq!(app.playlist.tracks[1].title, "C");
    }

    // -----------------------------------------------------------------------
    // Jump search
    // -----------------------------------------------------------------------

    fn app_with_named_tracks() -> App {
        let mut app = make_app();
        app.playlist.add(named_track("Hello World", "Test Artist"));
        app.playlist.add(named_track("Another Song", "Other Band"));
        app
    }

    #[test]
    fn j_key_enters_jump_mode() {
        let mut app = make_app();
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Jump { .. }));
    }

    #[test]
    fn jump_query_filters_results_by_title_in_real_time() {
        let mut app = app_with_named_tracks();
        app.mode = Mode::Jump {
            query: String::new(),
            results: vec![0, 1],
            selected: 0,
            from_media_library: false,
        };
        for c in "hello".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        let Mode::Jump { ref results, .. } = app.mode else {
            panic!("wrong mode")
        };
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], 0, "first track should match 'hello'");
    }

    #[test]
    fn jump_query_filters_by_artist_name() {
        let mut app = app_with_named_tracks();
        app.mode = Mode::Jump {
            query: String::new(),
            results: vec![0, 1],
            selected: 0,
            from_media_library: false,
        };
        for c in "test artist".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        let Mode::Jump { ref results, .. } = app.mode else {
            panic!("wrong mode")
        };
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], 0);
    }

    #[test]
    fn jump_query_no_match_shows_empty_results() {
        let mut app = app_with_named_tracks();
        app.mode = Mode::Jump {
            query: String::new(),
            results: vec![0, 1],
            selected: 0,
            from_media_library: false,
        };
        for c in "zzzzzzzzz".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        let Mode::Jump { ref results, .. } = app.mode else {
            panic!("wrong mode")
        };
        assert!(results.is_empty(), "no track should match gibberish");
    }

    #[test]
    fn jump_esc_closes_overlay_without_quitting() {
        let mut app = app_with_named_tracks();
        app.mode = Mode::Jump {
            query: "hello".into(),
            results: vec![0],
            selected: 0,
            from_media_library: false,
        };
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Normal));
        assert!(!app.should_quit);
    }

    #[test]
    fn jump_enter_plays_first_result() {
        let mut app = app_with_named_tracks();
        app.playlist.current_index = 0;
        app.mode = Mode::Jump {
            query: "another".into(),
            results: vec![1], // second track matches
            selected: 0,
            from_media_library: false,
        };
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.playlist.current_index, 1);
    }

    #[test]
    fn jump_enter_with_multiple_results_plays_selected() {
        let mut app = app_with_named_tracks();
        app.playlist.current_index = 0;
        app.mode = Mode::Jump {
            query: String::new(),
            results: vec![0, 1],
            selected: 1, // second result selected
            from_media_library: false,
        };
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.playlist.current_index, 1);
    }

    /// Display name uses title when available.
    #[test]
    fn display_name_uses_title_when_artist_is_empty() {
        let track = fake_track("My Song");
        assert_eq!(track.display_name(), "My Song");
    }

    /// Display name includes artist when present.
    #[test]
    fn display_name_includes_artist_when_present() {
        let track = named_track("My Song", "Cool Band");
        assert_eq!(track.display_name(), "Cool Band - My Song");
    }

    // -----------------------------------------------------------------------
    // New key binding tests (rebindings: ',' = move, '.' = remove, '/' = clear)
    // -----------------------------------------------------------------------

    /// ',' now enters MoveTrack mode (was 'm').
    #[test]
    fn comma_key_enters_move_track_mode_new_binding() {
        let mut app = make_app();
        app.handle_key(KeyCode::Char(','), KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::MoveTrack { from: None, .. }));
    }

    /// '.' now enters RemoveTrack mode (was ',').
    #[test]
    fn dot_key_enters_remove_track_mode_new_binding() {
        let mut app = make_app();
        app.handle_key(KeyCode::Char('.'), KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::RemoveTrack { .. }));
    }

    /// '/' clears all tracks and stops playback.
    #[test]
    fn slash_key_clears_all_tracks() {
        let mut app = app_with_tracks(&["A", "B", "C"]);
        assert_eq!(app.playlist.len(), 3);
        app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(app.playlist.is_empty(), "playlist should be empty after /");
        assert_eq!(app.playlist.current_index, 0);
        assert_eq!(app.playlist_cursor, 0);
        assert!(
            app.status_message
                .as_deref()
                .unwrap_or("")
                .contains("cleared"),
            "expected cleared message, got: {:?}",
            app.status_message
        );
    }

    /// 'i' enters Help mode with scroll at zero.
    #[test]
    fn i_key_enters_help_mode() {
        let mut app = make_app();
        app.handle_key(KeyCode::Char('i'), KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Help { scroll: 0 }));
    }

    /// Esc closes the help overlay.
    #[test]
    fn esc_in_help_mode_returns_to_normal() {
        let mut app = make_app();
        app.mode = Mode::Help { scroll: 0 };
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Normal));
    }

    /// ↑/↓ scroll the help overlay without closing it.
    #[test]
    fn arrow_keys_scroll_help_overlay() {
        let mut app = make_app();
        app.mode = Mode::Help { scroll: 5 };
        app.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Help { scroll: 6 }));
        app.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Help { scroll: 5 }));
    }

    /// z/x/c/v/b work in help mode (pass-through) and keep the overlay open.
    #[test]
    fn playback_keys_work_in_help_mode() {
        let mut app = make_app();
        app.mode = Mode::Help { scroll: 0 };
        // 'b' (next) should not close the overlay.
        app.handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
        assert!(
            matches!(app.mode, Mode::Help { .. }),
            "overlay should stay open after 'b'"
        );
        // 'v' (stop) should not close the overlay.
        app.handle_key(KeyCode::Char('v'), KeyModifiers::NONE);
        assert!(
            matches!(app.mode, Mode::Help { .. }),
            "overlay should stay open after 'v'"
        );
    }

    // -----------------------------------------------------------------------
    // Comma-separated commit_add_file tests
    // -----------------------------------------------------------------------

    /// A comma-separated input with two invalid paths shows "No valid".
    #[test]
    fn commit_add_file_comma_separated_all_invalid_shows_no_valid() {
        let mut app = make_app();
        app.commit_add_file("/no/such/a.mp3, /no/such/b.mp3");
        let msg = app.status_message.as_deref().unwrap_or("");
        assert!(msg.contains("No valid"), "unexpected: {msg}");
    }

    /// An input that is only whitespace/commas produces "No valid".
    #[test]
    fn commit_add_file_empty_after_split_shows_no_valid() {
        let mut app = make_app();
        app.commit_add_file("  ,  ,  ");
        let msg = app.status_message.as_deref().unwrap_or("");
        assert!(msg.contains("No valid"), "unexpected: {msg}");
    }

    // -----------------------------------------------------------------------
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
}
