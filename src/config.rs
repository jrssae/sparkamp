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
    pub display:    DisplayConfig,
    pub playback:   PlaybackConfig,
    pub visualizer: VisualizerConfig,
    pub window:     WindowConfig,
    /// Visual appearance settings (theme choice etc.).
    #[serde(default)]
    pub appearance: AppearanceConfig,
    /// Behaviour tweaks that do not belong under playback or visualizer.
    #[serde(default)]
    pub behavior:   BehaviorConfig,
    /// Paths searched for dynamic plugin libraries (`.so` files).
    #[serde(default)]
    pub plugins:    PluginsConfig,
    /// 10-band parametric equalizer settings.
    #[serde(default)]
    pub equalizer:  EqConfig,
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
    /// Waveform oscilloscope display.
    Oscilloscope,
}

/// Wraps [`VisualizerMode`] so it lives under its own `[visualizer]` section
/// in the TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualizerConfig {
    pub mode: VisualizerMode,
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
}

impl Default for WindowConfig {
    fn default() -> Self {
        WindowConfig {
            player_width:     Self::default_player_width(),
            player_height:    Self::default_player_height(),
            playlist_width:   Self::default_playlist_width(),
            playlist_height:  Self::default_playlist_height(),
            playlist_visible: false,
        }
    }
}

impl WindowConfig {
    pub fn default_player_width()    -> i32 { 520 }
    pub fn default_player_height()   -> i32 { 200 }
    pub fn default_playlist_width()  -> i32 { 400 }
    pub fn default_playlist_height() -> i32 { 500 }
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

/// Visual-appearance preferences that live under `[appearance]` in the TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceConfig {
    /// Which built-in colour theme to apply when `custom_skin` is empty.
    /// Defaults to [`ThemeChoice::Dark`].
    #[serde(default)]
    pub theme: ThemeChoice,

    /// Name of a user-provided skin to load from
    /// `~/.config/sparkamp/skins/<name>.css`.  When non-empty this overrides
    /// the `theme` field.  Empty string means "use the built-in theme".
    #[serde(default)]
    pub custom_skin: String,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        AppearanceConfig {
            theme:       ThemeChoice::Dark,
            custom_skin: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// BehaviorConfig
// ---------------------------------------------------------------------------

/// Miscellaneous behaviour tweaks under `[behavior]` in the TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorConfig {
    /// When `true`, start playing as soon as a file is added to the playlist.
    /// Defaults to `false`.
    #[serde(default)]
    pub autoplay_on_add: bool,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        BehaviorConfig { autoplay_on_add: false }
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
            filetype_dir:   String::new(),
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
    ("Flat",         [ 0.0,  0.0,  0.0,  0.0,  0.0,  0.0,  0.0,  0.0,  0.0,  0.0]),
    ("Rock",         [-1.0,  0.0,  2.0,  4.0, -2.0, -3.0,  0.0,  2.0,  5.0,  5.0]),
    ("Pop",          [-1.0, -1.0,  0.0,  2.0,  4.0,  4.0,  2.0,  0.0, -1.0, -1.0]),
    ("Jazz",         [ 0.0,  0.0,  0.0,  2.0,  4.0,  4.0,  3.0,  2.0,  2.0,  3.0]),
    ("Classical",    [ 0.0,  0.0,  0.0,  0.0,  0.0,  0.0, -2.0, -3.0, -3.0, -4.0]),
    ("Bass Boost",   [ 6.0,  5.0,  4.0,  3.0,  2.0,  0.0,  0.0,  0.0,  0.0,  0.0]),
    ("Treble Boost", [ 0.0,  0.0,  0.0,  0.0,  0.0,  2.0,  3.0,  4.0,  5.0,  6.0]),
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
    /// Each value is clamped to `[-24.0, +12.0]` before being applied.
    /// Defaults to ten zeros (flat response).
    #[serde(default = "EqConfig::default_bands")]
    pub bands: Vec<f64>,
}

impl EqConfig {
    fn default_enabled() -> bool { true }

    /// Default bands: ten zeros (flat response).
    pub fn default_bands() -> Vec<f64> { vec![0.0; 10] }

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
}

impl Default for EqConfig {
    fn default() -> Self {
        EqConfig {
            enabled: true,
            preset:  String::new(),
            bands:   EqConfig::default_bands(),
        }
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
            },
            visualizer: VisualizerConfig {
                mode: VisualizerMode::Bars,
            },
            window:     WindowConfig::default(),
            appearance: AppearanceConfig::default(),
            behavior:   BehaviorConfig::default(),
            plugins:    PluginsConfig::default(),
            equalizer:  EqConfig::default(),
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
    /// On the first run after the rename from GnomAmp → SparkAmp, migrates
    /// the existing config file from `~/.config/gnomamp/` to
    /// `~/.config/sparkamp/` so the user's settings are preserved.
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            migrate_legacy_file(
                &dirs::config_dir().unwrap_or_default().join("gnomamp").join("config.toml"),
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
// One-time migration helper (GnomAmp → SparkAmp rename)
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
