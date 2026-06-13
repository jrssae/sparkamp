//! Sparkamp — a Winamp-style audio player for Linux / GNOME.
//!
//! ## Entry points
//!
//! | Command | Behaviour |
//! |---------|-----------|
//! | `sparkamp` | Launch the GTK4 graphical UI |
//! | `sparkamp --tui` | Launch the terminal UI (TUI) |
//! | `sparkamp file1.mp3 …` | Pre-load files into the playlist, then open the GTK4 UI |
//! | `sparkamp --tui file1.mp3 …` | Pre-load files into the playlist, then open the TUI |
//!
//! GStreamer is initialised once here, before either UI is entered, so that
//! both frontends can assume the library is ready.

use anyhow::Result;
use clap::Parser;

mod config;
mod controller;
#[cfg(target_os = "linux")]
mod crash_log;
mod dedupe;
mod duration_cache;
mod duration_probe;
mod engine;
mod filetype_plugin;
// Consumed by the GTK frontend (Linux) and the C FFI in the lib target
// (macOS app). In the macOS *bin* neither exists, so the whole module is
// dead there — silence that case only; Linux still checks for real rot.
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod granite;
mod id3_editor;
mod loaded_plugin;
mod media_library;
mod model;
mod plugin_abi;
mod plugin_manager;
mod plugin_settings;
mod shuffle;
mod skin;
mod tags;
mod textutil;
mod timeutil;
mod viz_plugin;

// GTK4 frontend — Linux only. On macOS the SwiftUI app bundle replaces it.
#[cfg(target_os = "linux")]
#[path = "../frontends/gtk/mod.rs"]
mod gtk_ui;

#[cfg(target_os = "macos")]
mod gtk_ui {
    pub fn run(
        _playlist: crate::model::Playlist,
        _config: crate::config::Config,
    ) -> anyhow::Result<()> {
        eprintln!("Use the Sparkamp.app bundle for the GUI on macOS.");
        std::process::exit(1);
    }
}

#[path = "../frontends/tui/mod.rs"]
mod tui;

/// Command-line arguments parsed by [`clap`].
#[derive(Parser)]
#[command(
    name = "sparkamp",
    about = "A Winamp-style audio player for Linux/GNOME",
    long_about = "Sparkamp — a Winamp-style audio player for Linux/GNOME.\n\
\n\
USAGE EXAMPLES:\n\
  sparkamp                          Launch the GTK4 graphical UI\n\
  sparkamp --tui                    Launch the terminal UI\n\
  sparkamp file1.mp3 file2.flac     Load files, then open the GTK4 UI\n\
  sparkamp ~/music/                 Load a folder recursively, then open the GTK4 UI\n\
  sparkamp --tui ~/music/*.mp3      Shell-glob expansion into the TUI\n\
  sparkamp \"song.mp3,~/albums/rock\" Comma-separated file and folder in one argument\n\
\n\
FILES:\n\
  Pass any number of audio files or folders as positional arguments.\n\
  Comma-separated lists inside a single quoted argument are also accepted.\n\
  Folders are scanned recursively for audio files.\n\
  Relative and absolute paths are both accepted.\n\
  Unreadable or unsupported files are skipped with a warning.\n\
  If nothing is given, the last saved playlist is restored automatically.\n\
\n\
Press 'i' inside the app to view all keyboard shortcuts."
)]
struct Args {
    /// Open the terminal UI instead of the GTK4 graphical interface.
    #[arg(long)]
    tui: bool,

    /// Audio files or folders to load at startup.
    ///
    /// Each argument may be a single file path, a folder path (scanned
    /// recursively), or a comma-separated list of either.  Relative and
    /// absolute paths are both accepted.  Unreadable or unrecognised files
    /// are skipped with a warning.  If nothing is given the last saved
    /// playlist is restored automatically.
    files: Vec<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Install panic + GLib log capture before anything else so a crash
    // during init or in a GTK/GStreamer callback still leaves a record
    // at ~/.config/sparkamp/crash.log instead of vanishing silently.
    #[cfg(target_os = "linux")]
    crash_log::install();

    // GStreamer must be initialised before any Player is created, regardless
    // of which UI frontend is used.
    gstreamer::init()?;
    // Suppress GStreamer's default stderr log handler so its diagnostic output
    // does not corrupt the TUI alternate screen.  Actual errors are captured
    // via the GStreamer message bus and surfaced through the UI instead.
    gstreamer::log::set_default_threshold(gstreamer::DebugLevel::None);

    let config = config::Config::load()?;

    // Build the initial playlist from any files / folders given on the command
    // line.  Each argument may itself be a comma-separated list so that users
    // can write `sparkamp "song.mp3,~/music/jazz"` and have both processed.
    // Folder paths are scanned recursively for audio files.
    let mut playlist = model::Playlist::new();
    for raw_arg in &args.files {
        for part in raw_arg.split(',') {
            let part = part.trim();
            if part.is_empty() { continue; }
            let path = std::path::Path::new(part);
            if path.is_dir() {
                let (added, errors) = playlist.add_paths(&[path]);
                if added == 0 {
                    eprintln!("Warning: no audio files found in {:?}", path);
                }
                for e in errors {
                    eprintln!("Warning: {}", e);
                }
            } else {
                match model::Track::from_path(path) {
                    Ok(track) => playlist.add(track),
                    Err(e)    => eprintln!("Warning: skipping {:?}: {}", path, e),
                }
            }
        }
    }

    // If no files were given, restore the last saved playlist so the user
    // does not have to re-add their tracks on every launch.
    if playlist.is_empty() {
        if let Ok(saved) = model::Playlist::load_last() {
            playlist = saved;
        }
    }

    // Dispatch to the appropriate frontend.
    if args.tui {
        tui::run(playlist, config)
    } else {
        gtk_ui::run(playlist, config)
    }
}
