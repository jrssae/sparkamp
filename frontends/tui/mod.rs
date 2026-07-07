use anyhow::Result;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{
    io,
    path::PathBuf,
    sync::{
        atomic::AtomicBool,
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

use crate::{
    config::{Config, VisualizerMode},
    duration_cache::DurationCache,
    duration_probe,
    engine::{BusEvent, Player},
    id3_editor::{ExtraFrame, TagFields, ID3V1_GENRES},
    model::{Playlist, Track},
    shuffle::ShuffleState,
};

mod id3;
mod keys;
mod media_library;
mod settings_eq;
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
    /// Optical drives: one row per drive, with the loaded disc's track list.
    Discs,
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
    /// Optical drives, refreshed when the Discs tab is entered (subprocess-
    /// backed detection — not polled every frame).
    pub drives: Vec<crate::disc::OpticalDrive>,
    /// Highlighted drive row in the Discs tab.
    pub selected_drive: usize,
    /// Playlist-ready entries of the selected drive's audio disc.
    pub disc_entries: Vec<crate::disc::DiscTrackEntry>,
    /// Highlighted track row in the Discs tab track list.
    pub selected_disc_track: usize,
    /// gnudb match list awaiting a pick (overlay atop the Discs tab):
    /// the proposed matches and the highlighted row.
    pub gnudb_matches: Option<(Vec<crate::disc::gnudb::DiscMatch>, usize)>,
    /// Per-disc tag editor overlay state, when open.
    pub tag_edit: Option<DiscTagEditState>,
    /// Category picker for a gnudb submission: highlighted index into
    /// [`crate::disc::gnudb::CATEGORIES`], when the overlay is open.
    pub submit_category: Option<usize>,
    /// First-submission email capture: the input buffer while the overlay is
    /// open (gnudb requires the submitter's own address; config ships blank).
    pub submit_email: Option<String>,
}

/// State of the disc tag-override editor overlay (Discs tab, `e`).
///
/// A flat field list: rows 0–3 are Artist / Album / Year / Genre, rows 4+
/// are the per-track titles. `editing` routes typed characters into the
/// selected row's value; Enter toggles editing, Esc closes (editing → stop
/// editing; otherwise save + close).
pub struct DiscTagEditState {
    /// freedb id of the disc being edited — the `disc_tags` key.
    pub discid: String,
    pub artist: String,
    pub album: String,
    pub year: String,
    pub genre: String,
    /// One title per track (index 0 = track 1).
    pub titles: Vec<String>,
    /// Selected row: 0..=3 disc fields, 4.. = titles.
    pub selected: usize,
    pub editing: bool,
}

/// Result of a background gnudb lookup, delivered to the tick loop.
pub enum DiscLookupMsg {
    /// Several candidates — the user picks from the overlay.
    Matches(Vec<crate::disc::gnudb::DiscMatch>),
    /// A fetched + parsed entry for the disc with this freedb id.
    Entry(String, crate::disc::xmcd::XmcdEntry),
    /// A submission was accepted (server's message).
    Submitted(String),
    /// Lookup/submission failed (user-facing message).
    Failed(String),
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
/// Four tabs: Behavior, Visualizer, Filetypes, Media Library.  Each tab has
/// between one and three settings.  String-valued settings (Filetypes paths)
/// enter an inline text-edit mode when the user presses Enter.
///
/// Skin / theme selection lives in the GTK frontend's Settings window; the
/// TUI has no visual skinning of its own.
pub struct SettingsState {
    /// Active tab: 0 = Behavior, 1 = Visualizer, 2 = Filetypes, 3 = Media Library.
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
/// Tabs: 0=Behavior, 1=Visualizer, 2=Filetypes, 3=Media Library.
pub(super) fn settings_tab_len(tab: usize) -> usize {
    match tab {
        // Behavior: 2 items (autoplay_on_add, playlist_add_behavior)
        0 => 2,
        // Visualizer: 1 item (mode)
        1 => 1,
        // Media Library: 3 items (rescan_on_startup, periodic_rescan, rescan_interval_mins)
        2 => 3,
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
    /// Media library, opened lazily on first access.
    /// `None` when the DB could not be opened (startup error silenced).
    pub media_lib: Option<crate::media_library::MediaLibrary>,
    /// Active background scan channels, present while a scan is running.
    scan_channels: Option<ScanChannels>,
    /// Tag sets per disc (freedb id → entry): gnudb matches and hand edits.
    /// Overlaid onto the Discs tab titles; feeds rip/submission phases.
    pub disc_tags: std::collections::HashMap<String, crate::disc::xmcd::XmcdEntry>,
    /// The untouched gnudb match per disc — the baseline for "has the user
    /// changed anything worth submitting", and the source of the revision an
    /// update submission must increment.
    pub disc_official: std::collections::HashMap<String, crate::disc::xmcd::XmcdEntry>,
    /// Receiver for an in-flight background gnudb lookup, drained by tick().
    disc_lookup: Option<mpsc::Receiver<DiscLookupMsg>>,
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
            media_lib,
            scan_channels: None,
            disc_tags: std::collections::HashMap::new(),
            disc_official: std::collections::HashMap::new(),
            disc_lookup: None,
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
    pub(super) fn ctrl(&mut self) -> crate::controller::Controller<'_> {
        crate::controller::Controller {
            player: &mut self.player,
            playlist: &mut self.playlist,
            config: &mut self.config,
            shuffle_state: &mut self.shuffle_state,
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
    pub(super) fn play_current_no_record(&mut self) {
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
    pub(super) fn advance_to_next_playable(&mut self) {
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
                self.play_current_no_record();
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
                self.play_current_no_record();
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
    /// Return visualizer data for the current mode.
    ///
    /// Returns frequency data from GStreamer's spectrum analysis when available.
    /// Falls back to minimal bars when spectrum data is not yet received.
    pub fn visualizer_data(&self, count: usize) -> Vec<f64> {
        // Enforce a minimum of 10 data points so the visualizer always looks
        // reasonable even in very narrow terminal windows.
        let count = count.max(10);
        if !self.visualizer_active {
            return vec![0.0; count];
        }

        match self.config.visualizer.mode {
            // Granite has no terminal renderer; fall back to bars in the TUI.
            VisualizerMode::Bars | VisualizerMode::Granite => {
                // Use display_bands from config
                let display_count = self.config.visualizer.display_bands as usize;
                // Get display-ready spectrum bands (logarithmically mapped)
                let display_bands = self.player.get_spectrum_display_bands(display_count as u32);

                // Check if we got real data
                if !display_bands.iter().all(|&v| v == 0.0) {
                    display_bands
                } else {
                    // Spectrum not available: return minimal bars
                    vec![0.05; count]
                }
            }
            VisualizerMode::Waveform => {
                // Get real PCM waveform samples from the audio pipeline.
                // Samples are in [-1, 1] (bipolar, centred); map to [0, 1] for display.
                let raw = self.player.get_waveform_samples(count);
                raw.iter()
                    .map(|&s| (0.5 + s * 0.45).clamp(0.0, 1.0))
                    .collect()
            }
        }
    }

    // -----------------------------------------------------------------------
    // Visualizer mode cycling
    // -----------------------------------------------------------------------

    /// Advance the visualizer to the next mode.
    ///
    /// Cycle order in the TUI: Bars → Waveform → Bars. The shared core cycle
    /// goes Bars → Waveform → Granite → Bars, but Granite is GUI-only
    /// (CPU-rendered RGBA buffer), so when the core cycle lands on Granite the
    /// TUI advances once more to a mode it can render.
    pub(super) fn cycle_visualizer_mode(&mut self) {
        self.ctrl().toggle_visualizer_mode();
        if self.config.visualizer.mode == VisualizerMode::Granite {
            self.ctrl().toggle_visualizer_mode();
        }
        self.visualizer_active = true;
        self.status_message = None;
    }

    // -----------------------------------------------------------------------
    // Playlist editing
    // -----------------------------------------------------------------------

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

        // 4. Deliver background gnudb lookup results (Discs tab).
        let mut lookup_msgs = Vec::new();
        if let Some(rx) = &self.disc_lookup {
            while let Ok(msg) = rx.try_recv() {
                lookup_msgs.push(msg);
            }
        }
        for msg in lookup_msgs {
            self.handle_disc_lookup(msg);
        }

        // 5. Auto-clear transient status messages after STATUS_TICKS ticks.
        if self.status_ticks > 0 {
            self.status_ticks -= 1;
            if self.status_ticks == 0 {
                self.status_message = None;
            }
        }
    }

    /// Set a transient status message that auto-clears after STATUS_TICKS ticks.
    pub(super) fn set_status(&mut self, msg: impl Into<String>) {
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
mod tests;
