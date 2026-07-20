//! Pure builder for MPRIS `Metadata` map values.
//!
//! This module knows nothing about D-Bus, gio, or glib — it only turns
//! discrete track fields into an ordered list of typed values. A later task
//! (the gio D-Bus layer) maps [`MetaValue`] variants onto `glib::Variant`s
//! and publishes them on the MPRIS `org.mpris.MediaPlayer2.Player` interface.
//! Keeping this free of gtk4/gio means it unit-tests without a session bus.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// One MPRIS metadata value, frontend-agnostic. The D-Bus layer maps these to
/// `glib::Variant` ("s", "as", "x", "o", and artUrl as "s").
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum MetaValue {
    /// xesam:title, xesam:album -> "s"
    Str(String),
    /// xesam:artist, xesam:albumArtist, xesam:genre -> "as"
    StrList(Vec<String>),
    /// mpris:length (usecs), xesam:trackNumber -> "x" / "i" (D-Bus layer picks type)
    I64(i64),
    /// mpris:trackid -> "o"
    ObjPath(String),
    /// mpris:artUrl -> "s" (a file:// URL)
    ArtUrl(String),
}

/// Discrete inputs the builder needs. The CALLER (a later task) fills this
/// from `id3_editor::read_tag_fields` + engine length + the now-playing
/// artwork path; this module does no I/O.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct MprisMeta {
    /// The track's filesystem path (used to derive mpris:trackid).
    pub path: String,
    /// Duration in microseconds; 0 when unknown.
    pub length_usecs: i64,
    /// Absolute artwork file path, None when absent.
    pub art_path: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub genre: String,
    pub track_number: Option<i64>,
}

/// Build a valid D-Bus object path for `mpris:trackid` from a track path.
///
/// Object paths may only contain `[A-Za-z0-9_]` between `/` separators and
/// must start with `/`, so the raw filesystem path (which may contain `.`,
/// spaces, unicode, etc.) can't be used directly. Instead we hash the path
/// with `DefaultHasher` and hex-encode the result: stable per path, unique
/// enough for MPRIS's purposes, and always a valid path segment.
#[allow(dead_code)]
fn trackid_for(path: &str) -> String {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    format!("/dev/sparkamp/track/{:016x}", hasher.finish())
}

/// Turn discrete track fields into the ordered list of typed values an MPRIS
/// `Metadata` map needs. Fields whose source is empty/None are omitted,
/// except `mpris:trackid` which is always present.
#[allow(dead_code)]
pub fn build_metadata(m: &MprisMeta) -> Vec<(&'static str, MetaValue)> {
    let mut out = Vec::new();

    out.push((
        "mpris:trackid",
        MetaValue::ObjPath(trackid_for(&m.path)),
    ));

    if m.length_usecs > 0 {
        out.push(("mpris:length", MetaValue::I64(m.length_usecs)));
    }

    if let Some(art_path) = &m.art_path {
        if !art_path.is_empty() {
            out.push((
                "mpris:artUrl",
                MetaValue::ArtUrl(format!("file://{}", art_path)),
            ));
        }
    }

    if !m.title.is_empty() {
        out.push(("xesam:title", MetaValue::Str(m.title.clone())));
    }

    if !m.artist.is_empty() {
        out.push((
            "xesam:artist",
            MetaValue::StrList(vec![m.artist.clone()]),
        ));
    }

    if !m.album.is_empty() {
        out.push(("xesam:album", MetaValue::Str(m.album.clone())));
    }

    if !m.album_artist.is_empty() {
        out.push((
            "xesam:albumArtist",
            MetaValue::StrList(vec![m.album_artist.clone()]),
        ));
    }

    if !m.genre.is_empty() {
        out.push(("xesam:genre", MetaValue::StrList(vec![m.genre.clone()])));
    }

    if let Some(n) = m.track_number {
        out.push(("xesam:trackNumber", MetaValue::I64(n)));
    }

    out
}

// ---------------------------------------------------------------------------
// Command + property mappers (pure — make the D-Bus layer table-testable
// without a session bus).
// ---------------------------------------------------------------------------

/// An MPRIS `org.mpris.MediaPlayer2.Player` / root method resolved to the
/// controller action it should trigger. `Seek`/`SetPosition` carry the raw
/// microsecond argument the method was called with.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum MprisAction {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    /// Player.Seek(x): relative offset in microseconds (may be negative).
    Seek(i64),
    /// Player.SetPosition(o, x): absolute position in microseconds.
    SetPosition(i64),
    /// Root MediaPlayer2.Raise / .Quit.
    Raise,
    Quit,
}

/// Map an MPRIS method name to its action, or `None` for an unknown method.
/// `Seek`/`SetPosition` default their argument to 0 — the D-Bus layer replaces
/// it with the parsed call argument (this keeps the name→action mapping a pure,
/// bus-free table).
#[allow(dead_code)]
pub fn mpris_command_action(method: &str) -> Option<MprisAction> {
    Some(match method {
        "Play" => MprisAction::Play,
        "Pause" => MprisAction::Pause,
        "PlayPause" => MprisAction::PlayPause,
        "Stop" => MprisAction::Stop,
        "Next" => MprisAction::Next,
        "Previous" => MprisAction::Previous,
        "Seek" => MprisAction::Seek(0),
        "SetPosition" => MprisAction::SetPosition(0),
        "Raise" => MprisAction::Raise,
        "Quit" => MprisAction::Quit,
        _ => return None,
    })
}

/// MPRIS `PlaybackStatus` string for an engine [`crate::engine::PlayerState`].
#[allow(dead_code)]
pub fn playback_status_str(state: &crate::engine::PlayerState) -> &'static str {
    use crate::engine::PlayerState;
    match state {
        PlayerState::Playing => "Playing",
        PlayerState::Paused => "Paused",
        PlayerState::Stopped => "Stopped",
    }
}

/// MPRIS `LoopStatus` string → Sparkamp [`crate::shuffle::RepeatMode`], or
/// `None` for an unrecognized value (the setter should then ignore it).
#[allow(dead_code)]
pub fn loop_status_to_repeat(status: &str) -> Option<crate::shuffle::RepeatMode> {
    use crate::shuffle::RepeatMode;
    match status {
        "None" => Some(RepeatMode::Off),
        "Track" => Some(RepeatMode::Song),
        "Playlist" => Some(RepeatMode::Playlist),
        _ => None,
    }
}

/// Sparkamp [`crate::shuffle::RepeatMode`] → MPRIS `LoopStatus` string.
#[allow(dead_code)]
pub fn repeat_to_loop_status(mode: crate::shuffle::RepeatMode) -> &'static str {
    use crate::shuffle::RepeatMode;
    match mode {
        RepeatMode::Off => "None",
        RepeatMode::Song => "Track",
        RepeatMode::Playlist => "Playlist",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_meta() -> MprisMeta {
        MprisMeta {
            path: "/home/user/Music/song.mp3".to_string(),
            length_usecs: 5_000_000,
            art_path: Some("/home/user/.cache/sparkamp/art/abc.jpg".to_string()),
            title: "Song Title".to_string(),
            artist: "Some Artist".to_string(),
            album: "Some Album".to_string(),
            album_artist: "Some Album Artist".to_string(),
            genre: "Rock".to_string(),
            track_number: Some(3),
        }
    }

    #[test]
    fn full_map_has_all_keys_in_order() {
        let meta = full_meta();
        let result = build_metadata(&meta);

        let keys: Vec<&str> = result.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            vec![
                "mpris:trackid",
                "mpris:length",
                "mpris:artUrl",
                "xesam:title",
                "xesam:artist",
                "xesam:album",
                "xesam:albumArtist",
                "xesam:genre",
                "xesam:trackNumber",
            ]
        );

        assert_eq!(
            result[1].1,
            MetaValue::I64(5_000_000)
        );
        assert_eq!(
            result[2].1,
            MetaValue::ArtUrl(
                "file:///home/user/.cache/sparkamp/art/abc.jpg".to_string()
            )
        );
        assert_eq!(result[3].1, MetaValue::Str("Song Title".to_string()));
        assert_eq!(
            result[4].1,
            MetaValue::StrList(vec!["Some Artist".to_string()])
        );
        assert_eq!(result[5].1, MetaValue::Str("Some Album".to_string()));
        assert_eq!(
            result[6].1,
            MetaValue::StrList(vec!["Some Album Artist".to_string()])
        );
        assert_eq!(
            result[7].1,
            MetaValue::StrList(vec!["Rock".to_string()])
        );
        assert_eq!(result[8].1, MetaValue::I64(3));

        match &result[0].1 {
            MetaValue::ObjPath(p) => assert!(p.starts_with("/dev/sparkamp/track/")),
            other => panic!("expected ObjPath, got {other:?}"),
        }
    }

    #[test]
    fn omits_empty_fields() {
        let meta = MprisMeta {
            path: "/home/user/Music/empty.mp3".to_string(),
            length_usecs: 0,
            art_path: None,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            album_artist: String::new(),
            genre: String::new(),
            track_number: None,
        };

        let result = build_metadata(&meta);
        let keys: Vec<&str> = result.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec!["mpris:trackid"]);
    }

    #[test]
    fn art_url_only_when_art_present() {
        let mut meta = full_meta();
        meta.art_path = Some("/some/art.jpg".to_string());
        let result = build_metadata(&meta);
        assert!(result.iter().any(|(k, _)| *k == "mpris:artUrl"));
        match result
            .iter()
            .find(|(k, _)| *k == "mpris:artUrl")
            .map(|(_, v)| v)
            .unwrap()
        {
            MetaValue::ArtUrl(url) => assert_eq!(url, "file:///some/art.jpg"),
            other => panic!("expected ArtUrl, got {other:?}"),
        }

        meta.art_path = None;
        let result = build_metadata(&meta);
        assert!(!result.iter().any(|(k, _)| *k == "mpris:artUrl"));
    }

    #[test]
    fn length_included_only_when_positive() {
        let mut meta = full_meta();
        meta.length_usecs = 0;
        let result = build_metadata(&meta);
        assert!(!result.iter().any(|(k, _)| *k == "mpris:length"));

        meta.length_usecs = 5_000_000;
        let result = build_metadata(&meta);
        let entry = result.iter().find(|(k, _)| *k == "mpris:length");
        assert_eq!(entry.map(|(_, v)| v.clone()), Some(MetaValue::I64(5_000_000)));
    }

    #[test]
    fn trackid_is_valid_object_path() {
        let id_a = trackid_for("/home/user/Music/a.mp3");
        let id_b = trackid_for("/home/user/Music/b.mp3");
        let id_a_again = trackid_for("/home/user/Music/a.mp3");

        assert!(id_a.starts_with('/'));
        assert!(id_a
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '/'));

        assert_ne!(id_a, id_b);
        assert_eq!(id_a, id_a_again);
    }

    #[test]
    fn command_action_maps_every_method_and_rejects_unknown() {
        use MprisAction::*;
        assert_eq!(mpris_command_action("Play"), Some(Play));
        assert_eq!(mpris_command_action("Pause"), Some(Pause));
        assert_eq!(mpris_command_action("PlayPause"), Some(PlayPause));
        assert_eq!(mpris_command_action("Stop"), Some(Stop));
        assert_eq!(mpris_command_action("Next"), Some(Next));
        assert_eq!(mpris_command_action("Previous"), Some(Previous));
        assert_eq!(mpris_command_action("Seek"), Some(Seek(0)));
        assert_eq!(mpris_command_action("SetPosition"), Some(SetPosition(0)));
        assert_eq!(mpris_command_action("Raise"), Some(Raise));
        assert_eq!(mpris_command_action("Quit"), Some(Quit));
        assert_eq!(mpris_command_action("Nonsense"), None);
        assert_eq!(mpris_command_action("play"), None); // case-sensitive
    }

    #[test]
    fn playback_status_strings() {
        use crate::engine::PlayerState;
        assert_eq!(playback_status_str(&PlayerState::Playing), "Playing");
        assert_eq!(playback_status_str(&PlayerState::Paused), "Paused");
        assert_eq!(playback_status_str(&PlayerState::Stopped), "Stopped");
    }

    #[test]
    fn loop_status_repeat_round_trip() {
        use crate::shuffle::RepeatMode;
        assert_eq!(loop_status_to_repeat("None"), Some(RepeatMode::Off));
        assert_eq!(loop_status_to_repeat("Track"), Some(RepeatMode::Song));
        assert_eq!(loop_status_to_repeat("Playlist"), Some(RepeatMode::Playlist));
        assert_eq!(loop_status_to_repeat("bogus"), None);

        for m in [RepeatMode::Off, RepeatMode::Song, RepeatMode::Playlist] {
            assert_eq!(loop_status_to_repeat(repeat_to_loop_status(m)), Some(m));
        }
    }
}
