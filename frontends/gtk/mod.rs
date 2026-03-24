//! GTK4 graphical user interface entry point.
//!
//! Launched when the user passes `--ui` on the command line.  All widget
//! construction lives in [`window`]; this module's only job is to create the
//! [`gtk4::Application`], hook its `activate` signal to [`window::build`], and
//! hand control over to the GTK main loop.

use anyhow::Result;
use gtk4::prelude::*;
use gtk4::Application;

use crate::{config::Config, model::Playlist};

mod window;

/// Initialise the GTK4 application and enter the GTK main loop.
///
/// `playlist` and `config` are cloned into the `activate` closure so that
/// the closure can be called multiple times (the GTK spec allows it, though
/// for a single-instance desktop app it typically fires exactly once).
///
/// Returns `Ok(())` after the last window is closed, or an error if the GTK
/// application itself exits with a non-zero status.
pub fn run(playlist: Playlist, config: Config) -> Result<()> {
    let app = Application::builder()
        .application_id("dev.sparkamp.Sparkamp")
        .build();

    app.connect_activate(move |app| {
        window::build(app, playlist.clone(), config.clone());
    });

    // Pass an empty argv slice so GTK does not consume our own CLI flags.
    let exit = app.run_with_args::<&str>(&[]);

    if exit == gtk4::glib::ExitCode::SUCCESS {
        Ok(())
    } else {
        Err(anyhow::anyhow!("GTK application exited with an error"))
    }
}
