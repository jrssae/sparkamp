//! TUI behaviour tests driven through `App::handle_key`, split by topic;
//! this module holds the shared app/track builders every submodule uses.

use super::*;
use crate::{
    config::Config,
    model::{Playlist, Track},
};
use std::path::PathBuf;

pub(super) fn make_app() -> App {
    gstreamer::init().expect("GStreamer must be available for tests");
    App::new(Playlist::new(), Config::default()).expect("App::new failed")
}

pub(super) fn fake_track(title: &str) -> Track {
    Track {
        path: PathBuf::from(format!("/fake/{}.mp3", title)),
        title: title.to_string(),
        artist: String::new(),
        album_artist: String::new(),
        album: String::new(),
        duration: None,
        broken: false,
        read_only: false,
    }
}

pub(super) fn named_track(title: &str, artist: &str) -> Track {
    Track {
        path: PathBuf::from(format!("/fake/{}.mp3", title)),
        title: title.to_string(),
        artist: artist.to_string(),
        album_artist: String::new(),
        album: String::new(),
        duration: None,
        broken: false,
        read_only: false,
    }
}

pub(super) fn app_with_tracks(titles: &[&str]) -> App {
    let mut app = make_app();
    for t in titles {
        app.playlist.add(fake_track(t));
    }
    app
}

mod keys_input;
mod playback;
mod views;
mod bindings;
mod engine;
