//! Song URIs and `#SPARKAMP-SONG` playlist lines.
//!
//! Both name a server copy by server id plus the path the server reports,
//! never by song ID: Navidrome derives song IDs from tags, so they change
//! whenever a file is retagged. The path is the durable key.

/// The scheme prefix of a server song URI.
pub const SCHEME: &str = "subsonic://";

/// The URI for the song at `path` on server `server_id`.
pub fn song_uri(server_id: &str, path: &str) -> String {
    let mut out = format!("{SCHEME}{server_id}/");
    for b in path.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `(server_id, path)` from a song URI, or `None` if `uri` is not one.
pub fn parse_song_uri(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix(SCHEME)?;
    let (id, path) = rest.split_once('/')?;
    if id.is_empty() || path.is_empty() {
        return None;
    }
    Some((id.to_string(), percent_decode(path)?))
}

/// Undo `%XX` escapes. `None` if the result is not UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// A server-only playlist entry as written in a local `.m3u8`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongLine {
    pub server_id: String,
    pub path: String,
    /// For people reading the file; Sparkamp resolves by server and path.
    pub title: String,
    pub artist: String,
}

/// The `#SPARKAMP-SONG:` comment line for `song`. Other players skip `#`
/// lines they do not know, so the playlist still works for them.
pub fn format_song_line(song: &SongLine) -> String {
    format!(
        "{LINE_PREFIX}server={};path={};title={};artist={}",
        escape_field(&song.server_id),
        escape_field(&song.path),
        escape_field(&song.title),
        escape_field(&song.artist)
    )
}

/// Parse a `#SPARKAMP-SONG:` line; `None` for any other line.
pub fn parse_song_line(line: &str) -> Option<SongLine> {
    let body = line.trim_end_matches(['\r', '\n']).strip_prefix(LINE_PREFIX)?;
    let mut song = SongLine {
        server_id: String::new(),
        path: String::new(),
        title: String::new(),
        artist: String::new(),
    };
    for field in body.split(';') {
        let Some((key, value)) = field.split_once('=') else { continue };
        let value = percent_decode(value)?;
        match key {
            "server" => song.server_id = value,
            "path" => song.path = value,
            "title" => song.title = value,
            "artist" => song.artist = value,
            _ => {}
        }
    }
    if song.server_id.is_empty() || song.path.is_empty() {
        return None;
    }
    Some(song)
}

const LINE_PREFIX: &str = "#SPARKAMP-SONG:";

/// Escape only what would break the line's structure, so paths stay readable.
fn escape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' => out.push_str("%25"),
            ';' => out.push_str("%3B"),
            '=' => out.push_str("%3D"),
            '\n' => out.push_str("%0A"),
            '\r' => out.push_str("%0D"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "6f1c2b0e-0000-4000-8000-000000000001";

    #[test]
    fn a_song_uri_encodes_the_path_but_keeps_its_slashes() {
        // An absolute server path keeps its leading slash, hence the `//`.
        assert_eq!(
            song_uri(ID, "/music/AC/DC?/Back in Black/01 #1 100%.mp3"),
            "subsonic://6f1c2b0e-0000-4000-8000-000000000001\
             //music/AC/DC%3F/Back%20in%20Black/01%20%231%20100%25.mp3"
        );
    }

    #[test]
    fn a_song_uri_round_trips_any_path() {
        for path in [
            "/music/Björk/Homogenic/01 Hunter.flac",
            "Artist/Album/01 - Title.mp3",
            "/music/a;b=c/d%20e/f.ogg",
        ] {
            assert_eq!(
                parse_song_uri(&song_uri(ID, path)),
                Some((ID.to_string(), path.to_string())),
                "{path}"
            );
        }
    }

    #[test]
    fn files_and_disc_tracks_are_not_song_uris() {
        assert_eq!(parse_song_uri("/Users/me/Music/a.mp3"), None);
        assert_eq!(parse_song_uri("cdda://1"), None);
        assert_eq!(parse_song_uri("subsonic://"), None);
        assert_eq!(parse_song_uri("subsonic://no-path"), None);
    }

    fn line() -> SongLine {
        SongLine {
            server_id: ID.into(),
            path: "Artist/Album/01 Song.mp3".into(),
            title: "Song".into(),
            artist: "Artist".into(),
        }
    }

    #[test]
    fn a_song_line_stays_readable() {
        assert_eq!(
            format_song_line(&line()),
            "#SPARKAMP-SONG:server=6f1c2b0e-0000-4000-8000-000000000001\
             ;path=Artist/Album/01 Song.mp3;title=Song;artist=Artist"
        );
    }

    #[test]
    fn a_song_line_round_trips_awkward_values() {
        let awkward = SongLine {
            server_id: ID.into(),
            path: "/music/a;b=c/100%/x.mp3".into(),
            title: "Line\nbreak; and = signs".into(),
            artist: "".into(),
        };
        assert_eq!(parse_song_line(&format_song_line(&awkward)), Some(awkward));
    }

    #[test]
    fn other_lines_are_not_song_lines() {
        assert_eq!(parse_song_line("#EXTM3U"), None);
        assert_eq!(parse_song_line("#EXTINF:123,Artist - Title"), None);
        assert_eq!(parse_song_line("/Users/me/Music/a.mp3"), None);
        assert_eq!(parse_song_line("#SPARKAMP-SONG:path=a.mp3"), None, "no server");
        assert_eq!(parse_song_line("#SPARKAMP-SONG:server=x"), None, "no path");
    }
}
