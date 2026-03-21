//! Application configuration — structures and TOML persistence.
//!
//! Configuration lives at `~/.config/sparkamp/config.toml` (following the XDG
//! Base Directory Specification via the `dirs` crate).  Missing fields fall
//! back to the defaults defined by `Config::default()`, so a partial or
//! absent file is always valid.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
            },
            visualizer: VisualizerConfig {
                mode: VisualizerMode::Bars,
            },
            window: WindowConfig::default(),
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
