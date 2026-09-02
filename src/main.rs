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

mod devices;
mod config;
mod controller;
#[cfg(target_os = "linux")]
mod crash_log;
mod dedupe;
// Display-backend selection is a GTK/GDK concern; the macOS app bundle has no
// such choice to make.
#[cfg(target_os = "linux")]
mod display_backend;
mod disc;
mod duration_cache;
mod duration_probe;
mod file_status;
mod engine;
// Consumed by the GTK frontend (Linux) and the C FFI in the lib target
// (macOS app). In the macOS *bin* neither exists, so the whole module is
// dead there — silence that case only; Linux still checks for real rot.
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod granite;
mod id3_editor;
mod lyrics;
mod media_library;
mod ml_columns;
mod model;
// Pure MPRIS metadata-map builder (Phase 3). The gio D-Bus layer that
// consumes it lands in a later task.
mod mpris_meta;
// Core module for the album-art now-playing panel (Phase 2). Wired up by the
// GTK play-start seam (T5); `thumb_path_for` still awaits its T8 caller.
mod now_playing;
mod replaygain;
// Security-scoped bookmarks. macOS-only in substance; a documented no-op
// elsewhere, and declared here because the binary has its own module tree.
mod sandbox;
mod pathutil;
mod play_stats;
// B3 landed: the Linux GTK frontend (`gtk_ui`, below) now calls into this
// module (`dnd.rs`'s `playlist_add::AddMode`/`add_with_mode`), so the bin
// target no longer needs blanket dead-code suppression there. The allow
// stays for the GTK-less macOS bin build specifically: on `target_os =
// "macos"` `gtk_ui` is the stub at the bottom of this file, which never
// touches `playlist_add`, and the FFI call sites that do (`src/ffi/playlist.rs`)
// live only in the `lib` target, not this bin — so this module's variants
// are genuinely unconstructed in that one configuration.
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod playlist_add;
// Shared active-playlist status-line formatter (phase 7).
mod playlist_ingest;
mod playlist_status;
mod queue;
mod shuffle;
mod skin;
mod tags;
mod technical_probe;
mod textutil;
mod timeutil;
mod watch;

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
    version,
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
  sparkamp --backend=x11            Force X11 for this run\n\
  sparkamp --renderer=cairo         Force software rendering for this run\n\
\n\
DISPLAY BACKEND AND RENDERER:\n\
  Both flags override the saved Settings → Appearance → Graphics choice for a\n\
  single run without changing it, so a setting that leaves you with no window\n\
  can always be escaped from the command line.\n\
    --backend   auto (default) | wayland | x11\n\
    --renderer  auto (default) | gl | vulkan | cairo\n\
  On auto, Sparkamp probes Wayland in a throwaway child process at startup and\n\
  falls back to X11 if this compositor crashes GTK's Wayland backend.\n\
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

    /// Force the GDK display backend for this run, overriding the saved setting.
    #[arg(long, value_name = "BACKEND")]
    backend: Option<config::DisplayBackend>,

    /// Force the GSK renderer for this run, overriding the saved setting.
    #[arg(long, value_name = "RENDERER")]
    renderer: Option<config::RendererChoice>,

    /// Internal: open a GDK display, then exit. Spawned by the parent process
    /// to find out whether this compositor crashes GDK's Wayland backend, so
    /// the crash lands in a throwaway child instead of the real app.
    #[arg(long, hide = true)]
    probe_display: bool,

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

    // The display probe: open a GDK display and exit, nothing else. This is a
    // child of a real Sparkamp process, and the whole point is that it may die
    // on a signal — so it returns before the crash handler is installed, which
    // would otherwise record a crash we asked for.
    #[cfg(target_os = "linux")]
    if args.probe_display {
        display_backend::run_probe_child();
    }

    // Install panic + GLib log capture before anything else so a crash
    // during init or in a GTK/GStreamer callback still leaves a record
    // at ~/.config/sparkamp/crash.log instead of vanishing silently.
    #[cfg(target_os = "linux")]
    crash_log::install();

    let mut config = config::Config::load()?;

    // Pick the display backend and renderer before GStreamer initialises.
    // `configure` writes GDK_BACKEND / GSK_RENDERER, and setting an environment
    // variable is only sound while the process is single-threaded — GStreamer's
    // init is the first thing here that spawns threads. The TUI has no display
    // to choose, so it skips this entirely.
    #[cfg(target_os = "linux")]
    if !args.tui {
        if display_backend::configure(args.backend, args.renderer, &mut config) {
            // Only the probe verdict changed; a failure to persist it costs a
            // probe on the next launch and nothing else.
            let _ = config.save();
        }
    }

    // GStreamer must be initialised before any Player is created, regardless
    // of which UI frontend is used.
    // The GTK and TUI frontends play through GStreamer. macOS's app is the
    // Swift one over the FFI, which plays through AVFoundation and links no
    // GStreamer — this binary still compiles there, so the init is gated
    // rather than the whole entry point.
    #[cfg(not(target_os = "macos"))]
    gstreamer::init()?;
    // Suppress GStreamer's default stderr log handler so its diagnostic output
    // does not corrupt the TUI alternate screen.  Actual errors are captured
    // via the GStreamer message bus and surfaced through the UI instead.
    #[cfg(not(target_os = "macos"))]
    gstreamer::log::set_default_threshold(gstreamer::DebugLevel::None);

    // Build the initial playlist from any files / folders given on the command
    // line.  Each argument may itself be a comma-separated list so that users
    // can write `sparkamp "song.mp3,~/music/jazz"` and have both processed.
    // Folder paths are scanned recursively for audio files.
    let mut playlist = model::Playlist::new();
    if !args.files.is_empty() {
        // Resolve against the library first. A folder it has already scanned
        // then costs one batched query and no file access at all, instead of
        // 27.974 ms per file — about seventeen minutes for a 36k folder.
        let lib = if config.media_library.skip_db_load {
            None
        } else {
            media_library::MediaLibrary::open().ok()
        };
        for raw_arg in &args.files {
            for part in raw_arg.split(',') {
                let part = part.trim();
                if part.is_empty() { continue; }
                let path = std::path::PathBuf::from(part);
                let is_dir = path.is_dir();
                let rows = playlist_ingest::resolve(lib.as_ref(), std::slice::from_ref(&path));
                if is_dir && rows.is_empty() {
                    eprintln!("Warning: no audio files found in {:?}", path);
                }
                for row in rows {
                    if row.needs_tags {
                        // Nothing but the file can describe it, and at startup
                        // there is no UI to keep responsive — so read it now,
                        // exactly as before. Only the library-known case got
                        // faster.
                        match model::Track::from_path(&row.track.path) {
                            Ok(track) => playlist.add(track),
                            Err(e) => {
                                eprintln!("Warning: skipping {:?}: {}", row.track.path, e)
                            }
                        }
                    } else {
                        playlist.add(row.track);
                    }
                }
            }
        }
    }

    // Restore the last saved playlist so the user does not have to re-add
    // their tracks on every launch.
    //
    // When files were also given on the command line, `playlist_add_behavior`
    // decides what happens to the restored one — the same setting that governs
    // a drag-and-drop, a Media Library add, or a file opened from the desktop.
    // Cold start used to be the one path that ignored it: giving any file
    // argument skipped the restore entirely, so the command line was always a
    // replace whatever the user had configured.
    let cli_files_given = !playlist.is_empty();
    if !cli_files_given {
        if let Ok(saved) = model::Playlist::load_last() {
            playlist = saved;
        }
    } else if !playlist_add::should_replace(
        &config.behavior.playlist_add_behavior,
        playlist_add::AddMode::Behavior,
    ) {
        // Append: the restored playlist comes first, the command line's files
        // after it, matching where a drop would have put them.
        if let Ok(saved) = model::Playlist::load_last() {
            let cli_tracks: Vec<model::Track> = playlist.tracks.clone();
            playlist = saved;
            for t in cli_tracks {
                playlist.add(t);
            }
        }
    }

    // Dispatch to the appropriate frontend.
    if args.tui {
        tui::run(playlist, config)
    } else {
        gtk_ui::run(playlist, config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn every_visible_flag_carries_a_description_in_help() {
        let cmd = Args::command();
        for arg in cmd.get_arguments() {
            if arg.is_hide_set() {
                continue;
            }
            let help = arg.get_help().map(|h| h.to_string()).unwrap_or_default();
            assert!(
                !help.trim().is_empty(),
                "`{}` appears in --help with no description; give it a doc comment",
                arg.get_id()
            );
        }
    }

    #[test]
    fn help_lists_every_flag_a_user_is_expected_to_reach_for() {
        let help = Args::command().render_long_help().to_string();
        for flag in ["--tui", "--backend", "--renderer", "--help", "--version"] {
            assert!(help.contains(flag), "--help is missing {flag}:\n{help}");
        }
    }

    #[test]
    fn help_lists_the_accepted_backend_and_renderer_values() {
        let help = Args::command().render_long_help().to_string();
        for value in ["auto", "wayland", "x11", "ngl", "vulkan", "gl", "cairo"] {
            assert!(
                help.contains(value),
                "--help does not list the `{value}` value:\n{help}"
            );
        }
    }

    #[test]
    fn the_internal_probe_flag_stays_out_of_help() {
        let help = Args::command().render_long_help().to_string();
        assert!(
            !help.contains("probe-display"),
            "the probe flag is internal plumbing and must not be advertised"
        );
    }

    #[test]
    fn the_backend_and_renderer_flags_parse_into_the_config_types() {
        let args = Args::parse_from(["sparkamp", "--backend", "x11", "--renderer", "cairo"]);
        assert_eq!(args.backend, Some(config::DisplayBackend::X11));
        assert_eq!(args.renderer, Some(config::RendererChoice::Cairo));
    }

    #[test]
    fn a_misspelled_backend_is_rejected_rather_than_ignored() {
        assert!(Args::try_parse_from(["sparkamp", "--backend", "waylnad"]).is_err());
        assert!(Args::try_parse_from(["sparkamp", "--renderer", "opengl"]).is_err());
    }

    #[test]
    fn omitting_the_flags_leaves_the_saved_settings_in_charge() {
        let args = Args::parse_from(["sparkamp"]);
        assert_eq!(args.backend, None);
        assert_eq!(args.renderer, None);
    }

    #[test]
    fn the_renderer_flag_still_accepts_the_old_ngl_spelling_as_gl() {
        // Kept as an alias, not a listed value: muscle memory and old scripts
        // keep working, while --help stops advertising a name GTK rejects.
        let args = Args::parse_from(["sparkamp", "--renderer", "ngl"]);
        assert_eq!(args.renderer, Some(config::RendererChoice::Gl));
    }

    #[test]
    fn the_renderer_flag_offers_exactly_the_names_gtk_422_accepts() {
        // Checked against the argument's possible values rather than the help
        // text: "ngl" is a substring of "single", which appears in the prose,
        // so a text search answers a different question than the one asked.
        let cmd = Args::command();
        let arg = cmd
            .get_arguments()
            .find(|a| a.get_id() == "renderer")
            .expect("--renderer must exist");
        let values: Vec<String> = arg
            .get_possible_values()
            .iter()
            .map(|v| v.get_name().to_string())
            .collect();

        assert_eq!(values, ["auto", "vulkan", "gl", "cairo"]);
        assert!(
            !values.iter().any(|v| v == "ngl"),
            "ngl is renamed in GTK 4.22 and must not be offered as a value"
        );
    }
}
