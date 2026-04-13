//! GTK4 graphical user interface entry point.
//!
//! Launched when the user passes `--ui` on the command line.  All widget
//! construction lives in [`window`]; this module's only job is to create the
//! [`gtk4::Application`], hook its `activate` and `open` signals to
//! [`window::build`], and hand control over to the GTK main loop.
//!
//! Single-instance behaviour: GTK's GApplication unique-app mechanism ensures
//! that "Open with Sparkamp" in the file manager routes files to the already-
//! running instance via the `open` signal rather than spawning a new process.

use anyhow::Result;
use gtk4::prelude::*;
use gtk4::Application;

use crate::{config::Config, model::Playlist};

mod window;

/// Initialise the GTK4 application and enter the GTK main loop.
///
/// Returns `Ok(())` after the last window is closed, or an error if the GTK
/// application itself exits with a non-zero status.
pub fn run(playlist: Playlist, config: Config) -> Result<()> {
    use gtk4::gio;

    // Channel for forwarding file paths received via the GApplication `open`
    // signal to the running window's tick loop.
    let (file_tx, file_rx) = std::sync::mpsc::channel::<Vec<std::path::PathBuf>>();

    let app = Application::builder()
        .application_id("dev.sparkamp.Sparkamp")
        // HANDLES_OPEN: tells GLib to route file arguments (and "Open with"
        // activations) to the `open` signal on the primary instance instead of
        // starting a second process.
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    // `open` fires on the primary instance whenever another process passes
    // files to it (file manager "Open with", or `sparkamp file.mp3` while
    // already running).  We forward the paths through the channel; the window's
    // tick loop consumes them and respects playlist_add_behavior / autoplay.
    app.connect_open(move |app, files, _hint| {
        let paths: Vec<std::path::PathBuf> =
            files.iter().filter_map(|f| f.path()).collect();
        if paths.is_empty() {
            return;
        }
        // If no window exists yet (first launch via file association), create
        // one by triggering `activate` before sending through the channel.
        if app.windows().is_empty() {
            app.activate();
        }
        let _ = file_tx.send(paths);
    });

    // `activate` fires for a plain `sparkamp` launch with no file arguments,
    // and also when `connect_open` calls `app.activate()` above.  Guard with
    // `windows().is_empty()` so a second activation call doesn't create a
    // duplicate window.
    let file_rx = std::cell::Cell::new(Some(file_rx));
    app.connect_activate(move |app| {
        if !app.windows().is_empty() {
            return;
        }
        // Take the receiver exactly once; subsequent activate() calls (from
        // connect_open) reach the `return` above so this never panics.
        if let Some(rx) = file_rx.take() {
            window::build(app, playlist.clone(), config.clone(), rx);
        }
    });

    // Pass an empty argv slice so GTK does not consume our own CLI flags.
    let exit = app.run_with_args::<&str>(&[]);

    if exit == gtk4::glib::ExitCode::SUCCESS {
        Ok(())
    } else {
        Err(anyhow::anyhow!("GTK application exited with an error"))
    }
}
