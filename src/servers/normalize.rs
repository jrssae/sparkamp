//! Turning a server song and a local library row into comparable
//! [`Fields`].
//!
//! Without this, every row would look different: Navidrome fills missing
//! tags with placeholders like `[Unknown Artist]`, uses the file name as the
//! title of an untagged file, and some servers send 0 for "no year".

use super::api::ServerSong;
use super::merge::Fields;
use crate::media_library::LibTrack;

/// Navidrome's stand-ins for missing tags.
const PLACEHOLDERS: [&str; 2] = ["[Unknown Artist]", "[Unknown Album]"];

/// The comparable fields of a server copy.
pub fn server_fields(song: &ServerSong) -> Fields {
    let artist = text(&song.artist);
    let album = text(&song.album);
    // An untagged file shows up with placeholder artist and album and its
    // file name as title. Only then is a title equal to the file name treated
    // as missing; a tagged song may well be titled like its file.
    let untagged = artist.is_empty() && album.is_empty();
    let stem = song
        .path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_stem())
        .map(|s| s.to_string_lossy().into_owned());
    let mut title = text(&song.title);
    if untagged && stem.as_deref() == Some(title.as_str()) {
        title.clear();
    }
    Fields {
        title,
        artist,
        album,
        album_artist: text(&song.album_artist),
        genre: text(&song.genre),
        comment: text(&song.comment),
        track: number(song.track),
        disc: number(song.disc),
        year: number(song.year),
        bpm: number(song.bpm),
        rating: song.user_rating.min(5),
    }
}

/// The comparable fields of a local copy. The rating comes from the caller
/// because the library row does not carry one.
pub fn local_fields(track: &LibTrack, rating: u8) -> Fields {
    let opt = |s: &Option<String>| text(s.as_deref().unwrap_or_default());
    Fields {
        title: opt(&track.title),
        artist: opt(&track.artist),
        album: opt(&track.album),
        album_artist: opt(&track.album_artist),
        genre: opt(&track.genre),
        comment: opt(&track.comment),
        track: number(track.track_num),
        disc: number(track.disc_num),
        year: number(track.year),
        bpm: number(track.bpm.as_deref().and_then(|b| b.trim().parse::<f64>().ok()).map(|b| b.round() as i64)),
        rating: rating.min(5),
    }
}

/// Trimmed text, with Navidrome's placeholders read as empty.
fn text(s: &str) -> String {
    let t = s.trim();
    if PLACEHOLDERS.contains(&t) { String::new() } else { t.to_string() }
}

/// A number, with 0 read as absent: servers send 0 for "no year".
fn number(n: Option<i64>) -> Option<i64> {
    n.filter(|v| *v != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untagged_file_on_navidrome_has_no_fields() {
        let song = ServerSong {
            id: "x".into(),
            path: Some("/music/incoming/test.mp3".into()),
            title: "test".into(),
            artist: "[Unknown Artist]".into(),
            album: "[Unknown Album]".into(),
            album_artist: "[Unknown Artist]".into(),
            year: Some(0),
            track: Some(0),
            ..ServerSong::default()
        };
        assert_eq!(server_fields(&song), Fields::default());
    }

    #[test]
    fn a_tagged_song_keeps_its_values_trimmed() {
        let song = ServerSong {
            id: "x".into(),
            path: Some("/music/Miles Davis/Kind of Blue/01 So What.mp3".into()),
            title: " So What ".into(),
            artist: "Miles Davis".into(),
            album: "Kind of Blue".into(),
            album_artist: "Miles Davis".into(),
            genre: "Jazz".into(),
            comment: "remastered ".into(),
            track: Some(1),
            disc: Some(1),
            year: Some(1959),
            bpm: Some(0),
            user_rating: 4,
            ..ServerSong::default()
        };
        assert_eq!(
            server_fields(&song),
            Fields {
                title: "So What".into(),
                artist: "Miles Davis".into(),
                album: "Kind of Blue".into(),
                album_artist: "Miles Davis".into(),
                genre: "Jazz".into(),
                comment: "remastered".into(),
                track: Some(1),
                disc: Some(1),
                year: Some(1959),
                bpm: None,
                rating: 4,
            }
        );
    }

    /// A song really titled like its file name, with other tags present, is
    /// not mistaken for an untagged file.
    #[test]
    fn a_title_equal_to_the_file_name_survives_when_other_tags_exist() {
        let song = ServerSong {
            id: "x".into(),
            path: Some("/music/Björk/Homogenic/Hunter.flac".into()),
            title: "Hunter".into(),
            artist: "Björk".into(),
            album: "Homogenic".into(),
            ..ServerSong::default()
        };
        assert_eq!(server_fields(&song).title, "Hunter");
    }

    #[test]
    fn a_local_row_converts_with_the_given_rating() {
        let track = LibTrack {
            path: "/Users/me/Music/a.mp3".into(),
            title: Some("Song ".into()),
            artist: Some("Artist".into()),
            album: None,
            album_artist: Some("".into()),
            genre: Some("Rock".into()),
            comment: None,
            track_num: Some(3),
            disc_num: None,
            year: Some(0),
            bpm: Some("120".into()),
            ..LibTrack::default()
        };
        assert_eq!(
            local_fields(&track, 5),
            Fields {
                title: "Song".into(),
                artist: "Artist".into(),
                genre: "Rock".into(),
                track: Some(3),
                bpm: Some(120),
                rating: 5,
                ..Fields::default()
            }
        );
    }
}
