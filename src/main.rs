//! Sparkamp — a Winamp-style audio player for Linux / GNOME.
//!
//! ## Entry points
//!
//! | Command | Behaviour |
//! |---------|-----------|
//! | `sparkamp` | Launch the terminal UI (TUI) |
//! | `sparkamp --ui` | Launch the GTK4 graphical UI |
//! | `sparkamp file1.mp3 …` | Pre-load files into the playlist, then open the TUI |
//! | `sparkamp --ui file1.mp3 …` | Pre-load files into the playlist, then open the GTK4 UI |
//!
//! GStreamer is initialised once here, before either UI is entered, so that
//! both frontends can assume the library is ready.

use anyhow::Result;
use clap::Parser;

mod config;
mod controller;
mod duration_cache;
mod duration_probe;
mod engine;
mod filetype_plugin;
#[path = "../frontends/gtk/mod.rs"]
mod gtk_ui;
mod id3_editor;
mod loaded_plugin;
mod media_library;
mod model;
mod plugin_abi;
mod plugin_manager;
mod plugin_settings;
mod shuffle;
mod skin;
#[path = "../frontends/tui/mod.rs"]
mod tui;
mod viz_plugin;

/// Command-line arguments parsed by [`clap`].
#[derive(Parser)]
#[command(
    name = "sparkamp",
    about = "A Winamp-style audio player for Linux/GNOME",
    long_about = "Sparkamp — a Winamp-style audio player for Linux/GNOME.\n\
\n\
USAGE EXAMPLES:\n\
  sparkamp                          Launch the terminal UI\n\
  sparkamp --ui                     Launch the GTK4 graphical UI\n\
  sparkamp file1.mp3 file2.flac     Load files, then open the TUI\n\
  sparkamp ~/music/                 Load a folder recursively, then open the TUI\n\
  sparkamp --ui ~/music/*.mp3       Shell-glob expansion into the GTK4 UI\n\
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
    /// Open the GTK4 graphical interface instead of the terminal UI.
    #[arg(long)]
    ui: bool,

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
    if args.ui {
        gtk_ui::run(playlist, config)
    } else {
        tui::run(playlist, config)
    }
}
