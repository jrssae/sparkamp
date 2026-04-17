//! Application configuration — structures and TOML persistence.
//!
//! Configuration lives at `~/.config/sparkamp/config.toml` (following the XDG
//! Base Directory Specification via the `dirs` crate).  Missing fields fall
//! back to the defaults defined by `Config::default()`, so a partial or
//! absent file is always valid.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::shuffle::RepeatMode;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Root configuration struct.  Every sub-section has its own type so that
/// the TOML file is organised under `[display]`, `[playback]`, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub display: DisplayConfig,
    pub playback: PlaybackConfig,
    pub visualizer: VisualizerConfig,
    pub window: WindowConfig,
    /// Visual appearance settings (theme choice etc.).
    #[serde(default)]
    pub appearance: AppearanceConfig,
    /// Behaviour tweaks that do not belong under playback or visualizer.
    #[serde(default)]
    pub behavior: BehaviorConfig,
    /// Paths searched for dynamic plugin libraries (`.so` files).
    #[serde(default)]
    pub plugins: PluginsConfig,
    /// 10-band parametric equalizer settings.
    #[serde(default)]
    pub equalizer: EqConfig,
    /// Media library scanning and database settings.
    #[serde(default)]
    pub media_library: MediaLibraryConfig,
}

// ---------------------------------------------------------------------------
// DisplayConfig
// ---------------------------------------------------------------------------

/// Controls what the time counter shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayConfig {
    /// `"elapsed"` shows time from the start of the track; `"remaining"`
    /// counts down to zero.  Defaults to `"elapsed"`.
    pub time_mode: String,
}

// ---------------------------------------------------------------------------
// PlaybackConfig
// ---------------------------------------------------------------------------

/// Controls audio output behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackConfig {
    /// Initial volume in the range `[0.0, 1.0]`.  Applied to the GStreamer
    /// pipeline on startup.  Defaults to `0.8`.
    pub volume: f64,
    /// When `true`, the player loads the playlist but does not begin playing
    /// automatically.  Useful for launching the app in the background.
    pub start_paused: bool,
    /// Repeat mode: off, song, or playlist.  Persisted so the user's last
    /// setting is restored on the next launch.
    #[serde(default)]
    pub repeat_mode: RepeatMode,
    /// Whether shuffle was active when the app last exited.  Persisted so the
    /// user's last setting is restored on the next launch.
    #[serde(default)]
    pub shuffle_enabled: bool,
}

// ---------------------------------------------------------------------------
// VisualizerMode / VisualizerConfig
// ---------------------------------------------------------------------------

/// Which visualizer animation to show while a track plays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VisualizerMode {
    /// Animated frequency-bar display.  Default.
    #[default]
    Bars,
    /// Real-audio waveform display (center-line oscilloscope style).
    /// Serialised as "waveform"; the legacy "oscilloscope" value is accepted
    /// on load for backward compatibility.
    Waveform,
}

/// How the waveform trace is rendered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WaveformStyle {
    /// Draw only the waveform stroke; each segment coloured by zone.
    #[default]
    Lines,
    /// Fill the area between the waveform and the centre baseline; coloured by zone.
    Filled,
}

/// Wraps [`VisualizerMode`] so it lives under its own `[visualizer]` section
/// in the TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualizerConfig {
    pub mode: VisualizerMode,
    /// Number of frequency bands for spectrum analysis (8, 16, 32, or 64).
    /// Only used when a real audio source is connected.
    #[serde(default = "VisualizerConfig::default_spectrum_bands")]
    pub spectrum_bands: u32,
    /// Number of display bars for the Bars visualizer.
    /// Uses logarithmic frequency mapping from 30 Hz to 15000 Hz.
    #[serde(default = "VisualizerConfig::default_display_bands")]
    pub display_bands: u32,
    /// Whether to show a mirror effect for bars visualizer.
    /// The bar extends both above and below the center line.
    #[serde(default = "VisualizerConfig::default_bars_mirror")]
    pub bars_mirror: bool,
    /// Number of color zones for bars (1-6). Each zone shows a different color.
    #[serde(default = "VisualizerConfig::default_color_zones")]
    pub color_zones: u8,
    /// Per-zone colors as hex strings (e.g., "#006600"). Index 0 is bottom zone.
    #[serde(default = "VisualizerConfig::default_zone_colors")]
    pub zone_colors: Vec<String>,
    /// Number of color zones for the waveform (1-6).
    #[serde(default = "VisualizerConfig::default_waveform_color_zones")]
    pub waveform_color_zones: u8,
    /// Per-zone colors for the waveform as hex strings. Index 0 is bottom zone.
    #[serde(default = "VisualizerConfig::default_waveform_zone_colors")]
    pub waveform_zone_colors: Vec<String>,
    /// Whether the waveform is drawn as a stroke line or a filled shape.
    #[serde(default)]
    pub waveform_style: WaveformStyle,
}

impl VisualizerConfig {
    fn default_spectrum_bands() -> u32 {
        64
    }
    fn default_display_bands() -> u32 {
        16
    }
    fn default_bars_mirror() -> bool {
        true
    }
    fn default_color_zones() -> u8 {
        5
    }
    fn default_zone_colors() -> Vec<String> {
        vec![
            "#006600".to_string(), // dark green
            "#00cc00".to_string(), // light green
            "#cccc00".to_string(), // yellow
            "#cc8000".to_string(), // orange
            "#cc3300".to_string(), // red
            "#ff0000".to_string(), // bright red
        ]
    }
    fn default_waveform_color_zones() -> u8 {
        5
    }
    fn default_waveform_zone_colors() -> Vec<String> {
        vec![
            "#006600".to_string(), // dark green
            "#00cc00".to_string(), // light green
            "#cccc00".to_string(), // yellow
            "#cc8000".to_string(), // orange
            "#cc3300".to_string(), // red
            "#ff0000".to_string(), // bright red
        ]
    }
}

impl Default for VisualizerConfig {
    fn default() -> Self {
        Self {
            mode: VisualizerMode::default(),
            spectrum_bands: Self::default_spectrum_bands(),
            display_bands: Self::default_display_bands(),
            bars_mirror: true,
            color_zones: Self::default_color_zones(),
            zone_colors: Self::default_zone_colors(),
            waveform_color_zones: Self::default_waveform_color_zones(),
            waveform_zone_colors: Self::default_waveform_zone_colors(),
            waveform_style: WaveformStyle::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// WindowConfig
// ---------------------------------------------------------------------------

/// Window geometry and playlist-panel state.
///
/// All geometry fields are saved on every close and restored on the next
/// launch so the window returns to the user's preferred layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowConfig {
    /// Window width when the playlist panel is hidden.
    #[serde(default = "WindowConfig::default_player_width")]
    pub player_width: i32,
    /// Window height (shared by both views).
    #[serde(default = "WindowConfig::default_player_height")]
    pub player_height: i32,
    /// Playlist window width.
    #[serde(default = "WindowConfig::default_playlist_width")]
    pub playlist_width: i32,
    /// Playlist window height.
    #[serde(default = "WindowConfig::default_playlist_height")]
    pub playlist_height: i32,
    /// Whether the playlist window was open when the application last exited.
    #[serde(default)]
    pub playlist_visible: bool,
    /// Whether the media library window was open when the application last exited.
    #[serde(default)]
    pub ml_visible: bool,
    /// Media library window width.
    #[serde(default = "WindowConfig::default_ml_width")]
    pub ml_width: i32,
    /// Media library window height.
    #[serde(default = "WindowConfig::default_ml_height")]
    pub ml_height: i32,
    /// Whether the playlist sub-section in the ML sidebar is expanded.
    #[serde(default = "WindowConfig::default_ml_playlists_expanded")]
    pub ml_playlists_expanded: bool,
    /// Width of the ML sidebar (left navigation panel).
    #[serde(default = "WindowConfig::default_ml_sidebar_width")]
    pub ml_sidebar_width: i32,
}

impl Default for WindowConfig {
    fn default() -> Self {
        WindowConfig {
            player_width: Self::default_player_width(),
            player_height: Self::default_player_height(),
            playlist_width: Self::default_playlist_width(),
            playlist_height: Self::default_playlist_height(),
            playlist_visible: false,
            ml_visible: false,
            ml_width: Self::default_ml_width(),
            ml_height: Self::default_ml_height(),
            ml_playlists_expanded: true,
            ml_sidebar_width: Self::default_ml_sidebar_width(),
        }
    }
}

impl WindowConfig {
    pub fn default_player_width() -> i32 {
        520
    }
    pub fn default_player_height() -> i32 {
        200
    }
    pub fn default_playlist_width() -> i32 {
        400
    }
    pub fn default_playlist_height() -> i32 {
        500
    }
    pub fn default_ml_playlists_expanded() -> bool {
        true
    }
    pub fn default_ml_width() -> i32 {
        1000
    }
    pub fn default_ml_height() -> i32 {
        520
    }
    pub fn default_ml_sidebar_width() -> i32 {
        165
    }
}

// ---------------------------------------------------------------------------
// AppearanceConfig
// ---------------------------------------------------------------------------

/// Which colour theme the UI should use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    /// Dark theme (default — matches the classic Winamp look).
    #[default]
    Dark,
    /// Light theme for bright-environment use.
    Light,
}

/// Accent/highlight color choices.
/// GNOME provides these built-in accent colors that match the desktop theme.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "type", content = "value")]
pub enum AccentColorChoice {
    /// Use the system accent color from GNOME settings.
    System,
    /// GNOME blue (default).
    Blue,
    /// GNOME green.
    Green,
    /// GNOME purple.
    Purple,
    /// GNOME red/pink.
    Red,
    /// GNOME orange.
    Orange,
    /// GNOME yellow.
    Yellow,
    /// GNOME white.
    White,
    /// GNOME grey.
    Grey,
    /// Custom hex color (e.g., "#ff5500").
    Custom(String),
}

impl Default for AccentColorChoice {
    fn default() -> Self {
        AccentColorChoice::System
    }
}

impl AccentColorChoice {
    /// Return the hex color string for this accent choice.
    /// Returns None for System (to be resolved from gsettings).
    /// Returns Some(hex) for built-in and custom colors.
    pub fn hex(&self) -> Option<&str> {
        match self {
            AccentColorChoice::System => None,
            AccentColorChoice::Blue => Some("#3584e4"),
            AccentColorChoice::Green => Some("#3a944a"),
            AccentColorChoice::Purple => Some("#9141ac"),
            AccentColorChoice::Red => Some("#e01b24"),
            AccentColorChoice::Orange => Some("#ff7800"),
            AccentColorChoice::Yellow => Some("#f6d32d"),
            AccentColorChoice::White => Some("#ffffff"),
            AccentColorChoice::Grey => Some("#77767b"),
            AccentColorChoice::Custom(hex) => Some(hex),
        }
    }
}

/// Visual-appearance preferences that live under `[appearance]` in the TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceConfig {
    /// Which built-in colour theme to apply when `custom_skin` is empty.
    /// Defaults to [`ThemeChoice::Dark`].
    #[serde(default)]
    pub theme: ThemeChoice,

    /// Accent/highlight color for the UI.
    /// Defaults to [`AccentColorChoice::System`] (reads from GNOME settings).
    #[serde(default)]
    pub accent_color: AccentColorChoice,

    /// Name of a user-provided skin to load from
    /// `~/.config/sparkamp/skins/<name>.css`.  When non-empty this overrides
    /// the `theme` field.  Empty string means "use the built-in theme".
    #[serde(default)]
    pub custom_skin: String,

    /// Skin names that have been removed from the Skins picker.
    ///
    /// These skins are hidden in the UI but their `.css` files are not
    /// deleted — they can be re-added via "Add Skin…" at any time.
    /// Built-in skins (`"dark"`, `"light"`) are never added here.
    #[serde(default)]
    pub hidden_skins: Vec<String>,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        AppearanceConfig {
            theme: ThemeChoice::Dark,
            accent_color: AccentColorChoice::default(),
            custom_skin: String::new(),
            hidden_skins: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// BehaviorConfig
// ---------------------------------------------------------------------------

/// How tracks from the media library are added to the playlist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PlaylistAddBehavior {
    /// Append tracks to the end of the current playlist (default).
    #[default]
    Append,
    /// Clear the playlist and add only the selected tracks.
    Replace,
}

/// Miscellaneous behaviour tweaks under `[behavior]` in the TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorConfig {
    /// When `true`, start playing as soon as a file is added to the playlist.
    /// Defaults to `false`.
    #[serde(default)]
    pub autoplay_on_add: bool,

    /// How tracks from the media library are added to the playlist.
    /// Defaults to [`PlaylistAddBehavior::Append`].
    #[serde(default)]
    pub playlist_add_behavior: PlaylistAddBehavior,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        BehaviorConfig {
            autoplay_on_add: false,
            playlist_add_behavior: PlaylistAddBehavior::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// PluginsConfig
// ---------------------------------------------------------------------------

/// Plugin-search-path configuration under `[plugins]` in the TOML.
///
/// Both fields hold file-system paths that sparkamp scans for `.so` files
/// at startup.  Empty strings mean "don't scan any extra directory".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginsConfig {
    /// Directory searched for visualizer plugin libraries.
    #[serde(default)]
    pub visualizer_dir: String,
    /// Directory searched for filetype / decoder plugin libraries.
    #[serde(default)]
    pub filetype_dir: String,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        PluginsConfig {
            visualizer_dir: String::new(),
            filetype_dir: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// EqConfig
// ---------------------------------------------------------------------------

/// Standard 10-band EQ center frequencies in Hz (display only; the
/// GStreamer element's actual centre frequencies are fixed and match these).
pub const EQ_BAND_FREQS: [&str; 10] = [
    "29", "59", "119", "237", "474", "947", "1.9k", "3.8k", "7.5k", "15k",
];

/// Named EQ presets as (name, [band0..band9]) pairs.  Band gains are in dB.
/// All values are in the range accepted by `equalizer-10bands` (−24 to +12).
pub const EQ_PRESETS: &[(&str, [f64; 10])] = &[
    ("Flat", [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    (
        "Rock",
        [-1.0, 0.0, 2.0, 4.0, -2.0, -3.0, 0.0, 2.0, 5.0, 5.0],
    ),
    (
        "Pop",
        [-1.0, -1.0, 0.0, 2.0, 4.0, 4.0, 2.0, 0.0, -1.0, -1.0],
    ),
    ("Jazz", [0.0, 0.0, 0.0, 2.0, 4.0, 4.0, 3.0, 2.0, 2.0, 3.0]),
    (
        "Classical",
        [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -2.0, -3.0, -3.0, -4.0],
    ),
    (
        "Bass Boost",
        [6.0, 5.0, 4.0, 3.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    ),
    (
        "Treble Boost",
        [0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 3.0, 4.0, 5.0, 6.0],
    ),
];

/// 10-band equalizer configuration under `[equalizer]` in the TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EqConfig {
    /// Whether the equalizer is active.  When `false` all bands are effectively
    /// at 0 dB (flat) even if non-zero values are stored in `bands`.
    #[serde(default = "EqConfig::default_enabled")]
    pub enabled: bool,

    /// Name of the active preset (e.g. `"Rock"`), or an empty string when the
    /// user has set custom per-band gains.
    #[serde(default)]
    pub preset: String,

    /// Per-band gain in dB, indices 0–9 (29 Hz → 15 kHz).
    /// Each value is clamped to `[-12.0, +12.0]` before being applied.
    /// GStreamer's `equalizer-10bands` element supports up to -24/+12, but we
    /// use a symmetric range for a more predictable user experience.
    /// Defaults to ten zeros (flat response).
    #[serde(default = "EqConfig::default_bands")]
    pub bands: Vec<f64>,

    /// Pre-amplifier gain multiplier applied before the EQ bands.
    ///
    /// Stored as a linear multiplier in `[0.5, 1.5]` (50 % – 150 %).
    /// Only active when `enabled` is `true`; the engine resets it to 1.0
    /// when the EQ is disabled.  Defaults to 1.0 (no change).
    #[serde(default = "EqConfig::default_preamp")]
    pub preamp: f64,
}

impl EqConfig {
    fn default_enabled() -> bool {
        true
    }
    fn default_preamp() -> f64 {
        1.0
    }

    /// Default bands: ten zeros (flat response).
    pub fn default_bands() -> Vec<f64> {
        vec![0.0; 10]
    }

    /// Return the effective band gains as an array.
    ///
    /// When `enabled` is `false` all gains are returned as 0.0.  When the
    /// `bands` vec is shorter than 10, missing entries default to 0.0.
    pub fn effective_bands(&self) -> [f64; 10] {
        let mut arr = [0.0f64; 10];
        if self.enabled {
            for (i, &v) in self.bands.iter().take(10).enumerate() {
                arr[i] = v;
            }
        }
        arr
    }

    /// Return the effective pre-amp multiplier.
    ///
    /// Returns the stored value when EQ is enabled, or 1.0 (neutral) when
    /// the EQ is disabled so the pre-amp does not colour the signal.
    pub fn effective_preamp(&self) -> f64 {
        if self.enabled {
            self.preamp.clamp(0.5, 1.5)
        } else {
            1.0
        }
    }
}

impl Default for EqConfig {
    fn default() -> Self {
        EqConfig {
            enabled: true,
            preset: String::new(),
            bands: EqConfig::default_bands(),
            preamp: EqConfig::default_preamp(),
        }
    }
}

// ---------------------------------------------------------------------------
// MediaLibraryConfig
// ---------------------------------------------------------------------------

/// Media library behaviour settings under `[media_library]` in the TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaLibraryConfig {
    /// When `true`, rescan all watched folders on every application startup.
    /// Defaults to `false` (scan must be triggered manually or periodically).
    #[serde(default)]
    pub rescan_on_startup: bool,

    /// When `true`, rescan watched folders on a timer while the app is running.
    /// The interval is controlled by [`rescan_interval_mins`].
    #[serde(default)]
    pub periodic_rescan: bool,

    /// How often (in minutes) to perform an automatic rescan.
    /// Only used when `periodic_rescan` is `true`.  Defaults to 30 minutes.
    #[serde(default = "MediaLibraryConfig::default_interval_mins")]
    pub rescan_interval_mins: u64,

    /// Ordered list of column IDs shown in the Files view.
    /// Available IDs: "num", "title", "artist", "album", "duration",
    /// "filename", "year", "genre", "bitrate".
    #[serde(default = "MediaLibraryConfig::default_visible_columns")]
    pub visible_columns: Vec<String>,

    /// Ordered list of column IDs shown in the ID3 tag editor window.
    /// Read-only fields (filename, path, etc.) are always shown regardless of this list.
    #[serde(default = "MediaLibraryConfig::default_id3_visible_columns")]
    pub id3_visible_columns: Vec<String>,

    /// Which column (left/right) each field belongs to in the ID3 editor.
    /// Format: "left" or "right". Used for 2-column layout.
    #[serde(default)]
    pub id3_column_position: std::collections::HashMap<String, String>,

    /// Column display order in the Files view (list of column IDs in left-to-right order).
    /// Empty means use the default order defined by ALL_COLUMNS.
    #[serde(default)]
    pub ml_file_col_order: Vec<String>,
}

impl MediaLibraryConfig {
    /// Default rescan interval: 30 minutes.
    pub fn default_interval_mins() -> u64 {
        30
    }

    /// Default column set shown in the Files view.
    pub fn default_visible_columns() -> Vec<String> {
        ["title", "artist", "album", "duration"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// Default column set shown in the ID3 tag editor window.
    pub fn default_id3_visible_columns() -> Vec<String> {
        [
            "path",
            "filename",
            "title",
            "artist",
            "album",
            "year",
            "genre",
            "track_num",
            "track_total",
            "comment",
            "artwork_path",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    /// Default column positions for ID3 editor (left/right split).
    pub fn default_id3_column_position() -> std::collections::HashMap<String, String> {
        let mut map = std::collections::HashMap::new();
        let left_fields = ["title", "artist", "album", "year", "genre"];
        let right_fields = ["track_num", "track_total", "comment", "artwork_path"];
        for f in left_fields {
            map.insert(f.to_string(), "left".to_string());
        }
        for f in right_fields {
            map.insert(f.to_string(), "right".to_string());
        }
        map
    }
}

impl Default for MediaLibraryConfig {
    fn default() -> Self {
        MediaLibraryConfig {
            rescan_on_startup: false,
            periodic_rescan: false,
            rescan_interval_mins: Self::default_interval_mins(),
            visible_columns: Self::default_visible_columns(),
            id3_visible_columns: Self::default_id3_visible_columns(),
            id3_column_position: Self::default_id3_column_position(),
            ml_file_col_order: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Business-logic helpers on config structs
// ---------------------------------------------------------------------------

impl PlaybackConfig {
    /// Adjust volume by `delta` (positive to increase, negative to decrease),
    /// clamped to [0.0, 1.0].  Updates `self.volume` and returns the new value
    /// so the caller can pass it straight to `player.set_volume()`.
    pub fn adjust_volume(&mut self, delta: f64) -> f64 {
        self.volume = (self.volume + delta).clamp(0.0, 1.0);
        self.volume
    }
}

impl EqConfig {
    /// Set the gain for band `index` (0–9) to `new_gain` dB, clamped to
    /// [-12.0, +12.0].  Marks the EQ as "custom" by clearing `self.preset`.
    /// Returns the clamped gain so the caller can pass it to
    /// `player.set_eq_band()`.  Resizes `bands` to 10 if necessary.
    pub fn set_band_gain(&mut self, index: usize, new_gain: f64) -> f64 {
        if self.bands.len() < 10 {
            self.bands.resize(10, 0.0);
        }
        let clamped = new_gain.clamp(-12.0, 12.0);
        self.bands[index] = clamped;
        self.preset.clear();
        clamped
    }

    /// Advance to the next EQ preset in `EQ_PRESETS`, wrapping after the last
    /// one.  When the current preset name is not found (custom state), selects
    /// the first preset.  Updates `self.preset` and `self.bands`, and returns
    /// a reference to the new band gains so the caller can pass them to
    /// `player.apply_eq_bands()`.
    pub fn cycle_preset(&mut self) -> &[f64] {
        let idx = EQ_PRESETS
            .iter()
            .position(|(n, _)| *n == self.preset.as_str());
        let next_idx = match idx {
            Some(i) => (i + 1) % EQ_PRESETS.len(),
            None => 0,
        };
        let (name, bands) = EQ_PRESETS[next_idx];
        self.preset = name.to_string();
        self.bands = bands.to_vec();
        &self.bands
    }
}

impl MediaLibraryConfig {
    /// Set the rescan interval, enforcing a minimum of 1 minute.
    pub fn set_rescan_interval_mins(&mut self, mins: u64) {
        self.rescan_interval_mins = mins.max(1);
    }
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

impl Default for Config {
    fn default() -> Self {
        Config {
            display: DisplayConfig {
                time_mode: "elapsed".to_string(),
            },
            playback: PlaybackConfig {
                volume: 0.8,
                start_paused: false,
                repeat_mode: RepeatMode::Off,
                shuffle_enabled: false,
            },
            visualizer: VisualizerConfig::default(),
            window: WindowConfig::default(),
            appearance: AppearanceConfig::default(),
            behavior: BehaviorConfig::default(),
            plugins: PluginsConfig::default(),
            equalizer: EqConfig::default(),
            media_library: MediaLibraryConfig::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

impl Config {
    /// Return the canonical path to the config file:
    /// `$XDG_CONFIG_HOME/sparkamp/config.toml` (defaults to
    /// `~/.config/sparkamp/config.toml` on Linux).
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sparkamp")
            .join("config.toml")
    }

    /// Load configuration from disk, falling back to [`Config::default()`] if
    /// the file does not exist.  Returns an error only if the file exists but
    /// cannot be parsed (e.g., corrupted TOML).
    ///
    /// On the first run after the rename from GnomAmp → Sparkamp, migrates
    /// the existing config file from `~/.config/gnomamp/` to
    /// `~/.config/sparkamp/` so the user's settings are preserved.
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            migrate_legacy_file(
                &dirs::config_dir()
                    .unwrap_or_default()
                    .join("gnomamp")
                    .join("config.toml"),
                &path,
            );
        }
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            Ok(toml::from_str(&content)?)
        } else {
            Ok(Config::default())
        }
    }

    /// Persist the current configuration to disk.
    ///
    /// Creates the parent directory if it does not already exist.  Any
    /// existing file at the same path is replaced atomically (via a write
    /// followed by a rename at the OS level, courtesy of `std::fs::write`).
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// One-time migration helper (GnomAmp → Sparkamp rename)
// ---------------------------------------------------------------------------

/// Copy `old` to `new` (creating the destination directory) if `old` exists
/// and `new` does not.  Silently ignores all errors — migration is best-effort
/// and should never block startup.
///
/// Called once per data file on the first launch after the rename so that
/// users who already had GnomAmp installed keep their settings and playlist.
pub(crate) fn migrate_legacy_file(old: &std::path::Path, new: &std::path::Path) {
    if old.exists() {
        if let Some(parent) = new.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::copy(old, new);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── PlaybackConfig::adjust_volume ─────────────────────────────────────────

    #[test]
    fn adjust_volume_clamps_to_zero() {
        let mut cfg = Config::default();
        cfg.playback.volume = 0.02;
        let v = cfg.playback.adjust_volume(-0.05);
        assert_eq!(v, 0.0);
        assert_eq!(cfg.playback.volume, 0.0);
    }

    #[test]
    fn adjust_volume_clamps_to_one() {
        let mut cfg = Config::default();
        cfg.playback.volume = 0.98;
        let v = cfg.playback.adjust_volume(0.05);
        assert_eq!(v, 1.0);
        assert_eq!(cfg.playback.volume, 1.0);
    }

    #[test]
    fn adjust_volume_midrange() {
        let mut cfg = Config::default();
        cfg.playback.volume = 0.5;
        let v = cfg.playback.adjust_volume(0.05);
        assert!((v - 0.55).abs() < 1e-10);
        assert_eq!(cfg.playback.volume, v);
    }

    // ── EqConfig::set_band_gain ───────────────────────────────────────────────

    #[test]
    fn set_band_gain_clamps_above_12() {
        let mut cfg = EqConfig::default();
        let v = cfg.set_band_gain(0, 15.0);
        assert_eq!(v, 12.0);
        assert_eq!(cfg.bands[0], 12.0);
        assert!(cfg.preset.is_empty());
    }

    #[test]
    fn set_band_gain_clamps_below_minus_12() {
        let mut cfg = EqConfig::default();
        let v = cfg.set_band_gain(0, -20.0);
        assert_eq!(v, -12.0);
        assert_eq!(cfg.bands[0], -12.0);
        assert!(cfg.preset.is_empty());
    }

    #[test]
    fn set_band_gain_clears_preset_name() {
        let mut cfg = EqConfig::default();
        cfg.preset = "Rock".to_string();
        cfg.set_band_gain(3, 4.0);
        assert!(cfg.preset.is_empty());
    }

    #[test]
    fn set_band_gain_resizes_short_bands_vec() {
        let mut cfg = EqConfig::default();
        cfg.bands.clear();
        cfg.set_band_gain(5, 3.0);
        assert_eq!(cfg.bands.len(), 10);
        assert_eq!(cfg.bands[5], 3.0);
    }

    // ── EqConfig::cycle_preset ────────────────────────────────────────────────

    #[test]
    fn cycle_preset_from_custom_goes_to_first() {
        let mut cfg = EqConfig::default(); // preset is ""
        cfg.cycle_preset();
        assert_eq!(cfg.preset, EQ_PRESETS[0].0);
        assert_eq!(cfg.bands, EQ_PRESETS[0].1.to_vec());
    }

    #[test]
    fn cycle_preset_advances_through_presets() {
        let mut cfg = EqConfig::default();
        cfg.preset = EQ_PRESETS[0].0.to_string();
        cfg.cycle_preset();
        assert_eq!(cfg.preset, EQ_PRESETS[1].0);
        assert_eq!(cfg.bands, EQ_PRESETS[1].1.to_vec());
    }

    #[test]
    fn cycle_preset_wraps_from_last_to_first() {
        let mut cfg = EqConfig::default();
        let last = EQ_PRESETS.last().unwrap();
        cfg.preset = last.0.to_string();
        cfg.bands = last.1.to_vec();
        cfg.cycle_preset();
        assert_eq!(cfg.preset, EQ_PRESETS[0].0);
    }

    // ── MediaLibraryConfig::set_rescan_interval_mins ─────────────────────────

    #[test]
    fn set_rescan_interval_mins_enforces_minimum_of_1() {
        let mut cfg = MediaLibraryConfig::default();
        cfg.set_rescan_interval_mins(0);
        assert_eq!(cfg.rescan_interval_mins, 1);
    }

    #[test]
    fn set_rescan_interval_mins_accepts_valid_value() {
        let mut cfg = MediaLibraryConfig::default();
        cfg.set_rescan_interval_mins(60);
        assert_eq!(cfg.rescan_interval_mins, 60);
    }

    // ── AppearanceConfig ──────────────────────────────────────────────────────

    #[test]
    fn appearance_config_default_has_empty_hidden_skins() {
        let cfg = AppearanceConfig::default();
        assert!(cfg.hidden_skins.is_empty());
        assert_eq!(cfg.custom_skin, "");
        assert_eq!(cfg.theme, ThemeChoice::Dark);
    }

    #[test]
    fn appearance_config_hidden_skins_roundtrips_through_toml() {
        let mut cfg = AppearanceConfig::default();
        cfg.hidden_skins.push("my-skin".to_string());
        cfg.hidden_skins.push("old-theme".to_string());

        // Serialize to a mini TOML table and deserialize back.
        let toml_str = toml::to_string(&cfg).expect("serialize");
        let back: AppearanceConfig = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(back.hidden_skins, vec!["my-skin", "old-theme"]);
    }

    #[test]
    fn appearance_config_without_hidden_skins_key_deserializes_with_empty_vec() {
        // A config written before hidden_skins was added should deserialize cleanly.
        let toml_str = r#"custom_skin = "dark""#;
        let cfg: AppearanceConfig = toml::from_str(toml_str).expect("deserialize");
        assert!(cfg.hidden_skins.is_empty());
        assert_eq!(cfg.custom_skin, "dark");
    }

    #[test]
    fn window_config_default_ml_dimensions() {
        let cfg = WindowConfig::default();
        assert_eq!(cfg.ml_width, WindowConfig::default_ml_width());
        assert_eq!(cfg.ml_height, WindowConfig::default_ml_height());
    }

    #[test]
    fn window_config_ml_size_roundtrips_through_toml() {
        let mut cfg = WindowConfig::default();
        cfg.ml_width = 1200;
        cfg.ml_height = 800;

        let toml_str = toml::to_string(&cfg).expect("serialize");
        let back: WindowConfig = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(back.ml_width, 1200);
        assert_eq!(back.ml_height, 800);
    }

    #[test]
    fn window_config_without_ml_size_deserializes_with_defaults() {
        // A config written before ml_width/ml_height were added should deserialize cleanly.
        let toml_str = r#"
player_width = 600
player_height = 400
playlist_width = 500
playlist_height = 600
"#;
        let cfg: WindowConfig = toml::from_str(toml_str).expect("deserialize");
        assert_eq!(cfg.ml_width, WindowConfig::default_ml_width());
        assert_eq!(cfg.ml_height, WindowConfig::default_ml_height());
        // Other fields are set from TOML.
        assert_eq!(cfg.player_width, 600);
        assert_eq!(cfg.playlist_width, 500);
    }

    // ── MediaLibraryConfig::visible_columns ────────────────────────────────

    #[test]
    fn visible_columns_default_is_title_artist_album_duration() {
        let cfg = MediaLibraryConfig::default();
        assert_eq!(
            cfg.visible_columns,
            vec!["title", "artist", "album", "duration"]
        );
    }

    #[test]
    fn visible_columns_roundtrips_through_toml() {
        let mut cfg = MediaLibraryConfig::default();
        cfg.visible_columns = vec!["num".to_string(), "title".to_string(), "artist".to_string()];

        let toml_str = toml::to_string(&cfg).expect("serialize");
        let back: MediaLibraryConfig = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(back.visible_columns, vec!["num", "title", "artist"]);
    }

    #[test]
    fn visible_columns_without_key_deserializes_with_defaults() {
        // A config written before visible_columns was added should deserialize cleanly.
        let toml_str = r#"
rescan_on_startup = true
rescan_interval_mins = 60
"#;
        let cfg: MediaLibraryConfig = toml::from_str(toml_str).expect("deserialize");
        assert_eq!(
            cfg.visible_columns,
            vec!["title", "artist", "album", "duration"]
        );
        assert!(cfg.rescan_on_startup);
        assert_eq!(cfg.rescan_interval_mins, 60);
    }

    // ── MediaLibraryConfig::id3_visible_columns ──────────────────────────────

    #[test]
    fn id3_visible_columns_default_has_basic_fields() {
        let cfg = MediaLibraryConfig::default();
        assert_eq!(
            cfg.id3_visible_columns,
            vec![
                "path",
                "filename",
                "title",
                "artist",
                "album",
                "year",
                "genre",
                "track_num",
                "track_total",
                "comment",
                "artwork_path",
            ]
        );
    }

    #[test]
    fn id3_visible_columns_roundtrips_through_toml() {
        let mut cfg = MediaLibraryConfig::default();
        cfg.id3_visible_columns = vec![
            "title".to_string(),
            "artist".to_string(),
            "year".to_string(),
        ];

        let toml_str = toml::to_string(&cfg).expect("serialize");
        let back: MediaLibraryConfig = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(back.id3_visible_columns, vec!["title", "artist", "year"]);
    }

    #[test]
    fn id3_visible_columns_without_key_deserializes_with_defaults() {
        // A config written before id3_visible_columns was added should deserialize cleanly.
        let toml_str = r#"
rescan_on_startup = true
rescan_interval_mins = 60
"#;
        let cfg: MediaLibraryConfig = toml::from_str(toml_str).expect("deserialize");
        assert_eq!(
            cfg.id3_visible_columns,
            vec![
                "path",
                "filename",
                "title",
                "artist",
                "album",
                "year",
                "genre",
                "track_num",
                "track_total",
                "comment",
                "artwork_path",
            ]
        );
    }

    #[test]
    fn id3_column_position_default_assigns_left_right() {
        let cfg = MediaLibraryConfig::default();
        assert_eq!(
            cfg.id3_column_position.get("title"),
            Some(&"left".to_string())
        );
        assert_eq!(
            cfg.id3_column_position.get("artist"),
            Some(&"left".to_string())
        );
        assert_eq!(
            cfg.id3_column_position.get("track_num"),
            Some(&"right".to_string())
        );
    }

    #[test]
    fn id3_column_position_roundtrips_through_toml() {
        let mut cfg = MediaLibraryConfig::default();
        cfg.id3_column_position
            .insert("title".to_string(), "right".to_string());
        cfg.id3_column_position
            .insert("artist".to_string(), "left".to_string());

        let toml_str = toml::to_string(&cfg).expect("serialize");
        let back: MediaLibraryConfig = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(
            back.id3_column_position.get("title"),
            Some(&"right".to_string())
        );
        assert_eq!(
            back.id3_column_position.get("artist"),
            Some(&"left".to_string())
        );
    }
}
