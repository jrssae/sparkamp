//! ID3 tag reading and writing utilities shared between the TUI and GTK editors.
//!
//! This module provides:
//! - [`TagFields`] — the set of fields shown in the default two-column editor view.
//! - [`read_tag_fields`] — populate a `TagFields` from a file path.
//! - [`write_tag_fields`] — write a `TagFields` back to a file, preserving all
//!   frames that are not represented in `TagFields`.
//! - [`ID3V1_GENRES`] — the canonical 192 ID3v1 genre strings used for the
//!   typeahead dropdown.
//! - [`ExtraFrame`] — a raw ID3v2 frame (ID + value) for the "customize" panel.
//! - [`read_extra_frames`] — read all frames from a file that are *not* in
//!   the default field set.
//! - [`write_extra_frame`] — write a single extra frame back to the tag.
//!
//! Neither the GTK widgets nor TUI rendering code lives here; only the data
//! and I/O logic.  Both UI layers depend on this module to stay in sync.

use anyhow::{Context, Result};
use id3::{Tag, TagLike, Version};
use std::path::Path;

// ---------------------------------------------------------------------------
// Genre list
// ---------------------------------------------------------------------------

/// All 192 genres defined by ID3v1 (Winamp extended set included).
///
/// Used as the source for the genre typeahead / dropdown in both UIs.
/// The user may also type a genre that is not in this list; the editor
/// accepts free text — this array is only for autocompletion suggestions.
pub const ID3V1_GENRES: &[&str] = &[
    "Blues",
    "Classic Rock",
    "Country",
    "Dance",
    "Disco",
    "Funk",
    "Grunge",
    "Hip-Hop",
    "Jazz",
    "Metal",
    "New Age",
    "Oldies",
    "Other",
    "Pop",
    "R&B",
    "Rap",
    "Reggae",
    "Rock",
    "Techno",
    "Industrial",
    "Alternative",
    "Ska",
    "Death Metal",
    "Pranks",
    "Soundtrack",
    "Euro-Techno",
    "Ambient",
    "Trip-Hop",
    "Vocal",
    "Jazz+Funk",
    "Fusion",
    "Trance",
    "Classical",
    "Instrumental",
    "Acid",
    "House",
    "Game",
    "Sound Clip",
    "Gospel",
    "Noise",
    "AlternRock",
    "Bass",
    "Soul",
    "Punk",
    "Space",
    "Meditative",
    "Instrumental Pop",
    "Instrumental Rock",
    "Ethnic",
    "Gothic",
    "Darkwave",
    "Techno-Industrial",
    "Electronic",
    "Pop-Folk",
    "Eurodance",
    "Dream",
    "Southern Rock",
    "Comedy",
    "Cult",
    "Gangsta",
    "Top 40",
    "Christian Rap",
    "Pop/Funk",
    "Jungle",
    "Native American",
    "Cabaret",
    "New Wave",
    "Psychedelic",
    "Rave",
    "Showtunes",
    "Trailer",
    "Lo-Fi",
    "Tribal",
    "Acid Punk",
    "Acid Jazz",
    "Polka",
    "Retro",
    "Musical",
    "Rock & Roll",
    "Hard Rock",
    "Folk",
    "Folk-Rock",
    "National Folk",
    "Swing",
    "Fast Fusion",
    "Bebop",
    "Latin",
    "Revival",
    "Celtic",
    "Bluegrass",
    "Avantgarde",
    "Gothic Rock",
    "Progressive Rock",
    "Psychedelic Rock",
    "Symphonic Rock",
    "Slow Rock",
    "Big Band",
    "Chorus",
    "Easy Listening",
    "Acoustic",
    "Humour",
    "Speech",
    "Chanson",
    "Opera",
    "Chamber Music",
    "Sonata",
    "Symphony",
    "Booty Bass",
    "Primus",
    "Porn Groove",
    "Satire",
    "Slow Jam",
    "Club",
    "Tango",
    "Samba",
    "Folklore",
    "Ballad",
    "Power Ballad",
    "Rhythmic Soul",
    "Freestyle",
    "Duet",
    "Punk Rock",
    "Drum Solo",
    "A Cappella",
    "Euro-House",
    "Dance Hall",
    "Goa",
    "Drum & Bass",
    "Club-House",
    "Hardcore",
    "Terror",
    "Indie",
    "BritPop",
    "Negerpunk",
    "Polsk Punk",
    "Beat",
    "Christian Gangsta Rap",
    "Heavy Metal",
    "Black Metal",
    "Crossover",
    "Contemporary Christian",
    "Christian Rock",
    "Merengue",
    "Salsa",
    "Thrash Metal",
    "Anime",
    "JPop",
    "Synthpop",
    "Abstract",
    "Art Rock",
    "Baroque",
    "Bhangra",
    "Big Beat",
    "Breakbeat",
    "Chillout",
    "Downtempo",
    "Dub",
    "EBM",
    "Eclectic",
    "Electro",
    "Electroclash",
    "Emo",
    "Experimental",
    "Garage",
    "Global",
    "IDM",
    "Illbient",
    "Industro-Goth",
    "Jam Band",
    "Krautrock",
    "Leftfield",
    "Lounge",
    "Math Rock",
    "New Romantic",
    "Nu-Breakz",
    "Post-Punk",
    "Post-Rock",
    "Psytrance",
    "Shoegaze",
    "Space Rock",
    "Trop Rock",
    "World Music",
    "Neoclassical",
    "Audiobook",
    "Audio Theatre",
    "Neue Deutsche Welle",
    "Podcast",
    "Indie-Rock",
    "G-Funk",
    "Dubstep",
    "Garage Rock",
    "Psybient",
];

// ---------------------------------------------------------------------------
// TagFields — the default view
// ---------------------------------------------------------------------------

/// All fields displayed in the default two-column ID3 editor view.
///
/// This struct is intentionally flat (no nesting) so both UIs can iterate
/// over `(label, &mut String)` pairs generically when laying out the form.
///
/// Numeric fields (`year`, `track_number`, `track_total`, `disc_number`,
/// `disc_total`, `bpm`) are stored as `String` so the editor can display and
/// edit them as text without lossy conversions.  `write_tag_fields` converts
/// them back to integers when saving.
#[derive(Debug, Clone, Default)]
pub struct TagFields {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub genre: String,
    pub year: String,         // stored as String for display/editing
    pub track_number: String, // "x" part of "x/y"
    pub track_total: String,  // "y" part of "x/y"
    pub disc_number: String,  // "x" part of "x/y"
    pub disc_total: String,   // "y" part of "x/y"
    pub bpm: String,
    pub comment: String,      // default comment (no content description)
    pub artwork_path: String, // path to artwork file (not embedded in tag)
}

impl TagFields {
    /// Return an ordered list of `(label, field_value)` pairs for rendering
    /// a two-column form.  The left column ends at the midpoint so callers
    /// can split at `len()/2` for a balanced two-column layout.
    ///
    /// Each label is a short human-readable string; the value is a clone of
    /// the field at the time of the call.  Callers that need mutable access
    /// should edit the struct fields directly.
    pub fn field_pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("Title", self.title.clone()),
            ("Artist", self.artist.clone()),
            ("Album", self.album.clone()),
            ("Album Artist", self.album_artist.clone()),
            ("Genre", self.genre.clone()),
            ("Year", self.year.clone()),
            ("Track #", self.track_number.clone()),
            ("Track Total", self.track_total.clone()),
            ("Disc #", self.disc_number.clone()),
            ("Disc Total", self.disc_total.clone()),
            ("BPM", self.bpm.clone()),
            ("Comment", self.comment.clone()),
        ]
    }
}

// ---------------------------------------------------------------------------
// ExtraFrame — custom / additional ID3v2 frames
// ---------------------------------------------------------------------------

/// A raw ID3v2 text frame that is not represented in [`TagFields`].
///
/// Used by the "Customize" panel to let the user add arbitrary frames.
/// Only text frames (IDs starting with 'T') and COMM/USLT are handled;
/// binary frames (cover art, etc.) are read-only in this version.
#[derive(Debug, Clone)]
pub struct ExtraFrame {
    /// The four-character ID3v2 frame identifier (e.g. `"TCOM"`, `"TCOP"`).
    pub id: String,
    /// Human-readable description for frames the editor knows about, or the
    /// raw frame ID for unknown frames.
    pub label: String,
    /// The string value of the frame (decoded from UTF-8 / Latin-1).
    pub value: String,
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Read the default editor fields from the ID3 tag of `path`.
///
/// Returns an empty `TagFields` (all strings empty) if the file has no
/// ID3 tag — the user can then fill in the fields and save to create one.
pub fn read_tag_fields(path: &Path) -> TagFields {
    let tag = match Tag::read_from_path(path) {
        Ok(t) => t,
        Err(_) => return TagFields::default(),
    };

    // Helper: parse "x/y" track/disc notation into separate number strings.
    fn split_x_of_y(s: &str) -> (String, String) {
        if let Some((a, b)) = s.split_once('/') {
            (a.trim().to_string(), b.trim().to_string())
        } else {
            (s.trim().to_string(), String::new())
        }
    }

    let (track_number, track_total) = tag
        .get("TRCK")
        .and_then(|f| f.content().text())
        .map(split_x_of_y)
        .unwrap_or_default();

    let (disc_number, disc_total) = tag
        .get("TPOS")
        .and_then(|f| f.content().text())
        .map(split_x_of_y)
        .unwrap_or_default();

    // COMM frames have a content description; we take the first one whose
    // description is empty (the canonical "plain comment").
    let comment = tag
        .comments()
        .find(|c| c.description.is_empty())
        .map(|c| c.text.clone())
        .unwrap_or_default();

    TagFields {
        title: tag.title().unwrap_or("").to_string(),
        artist: tag.artist().unwrap_or("").to_string(),
        album: tag.album().unwrap_or("").to_string(),
        album_artist: tag.album_artist().unwrap_or("").to_string(),
        genre: tag.genre().unwrap_or("").to_string(),
        year: tag.year().map(|y| y.to_string()).unwrap_or_default(),
        track_number,
        track_total,
        disc_number,
        disc_total,
        bpm: tag
            .get("TBPM")
            .and_then(|f| f.content().text())
            .unwrap_or("")
            .to_string(),
        comment,
        artwork_path: String::new(),
    }
}

/// Read all text frames from the tag that are **not** in the default field set.
///
/// Used by the "Customize" panel to show additional ID3v2 frames the user
/// can optionally add to their editor view.  Binary frames (APIC, etc.) and
/// frames already covered by [`TagFields`] are excluded.
pub fn read_extra_frames(path: &Path) -> Vec<ExtraFrame> {
    // Frame IDs covered by the default TagFields view — exclude these.
    const DEFAULT_IDS: &[&str] = &[
        "TIT2", "TPE1", "TALB", "TPE2", "TCON", "TDRC", "TRCK", "TPOS", "TBPM", "COMM",
    ];

    let tag = match Tag::read_from_path(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    tag.frames()
        .filter(|f| {
            let id = f.id();
            // Only show text frames and known extended text frames.
            (id.starts_with('T') || id == "USLT") && !DEFAULT_IDS.contains(&id)
        })
        .map(|f| {
            let value = f.content().text().unwrap_or("").to_string();
            ExtraFrame {
                label: frame_label(f.id()).to_string(),
                id: f.id().to_string(),
                value,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Write `fields` back to the ID3v2 tag of `path`.
///
/// Reads the existing tag first so that frames not covered by `TagFields`
/// (e.g. cover art, lyrics, extra text frames) are preserved.  Creates a
/// new tag if the file has none.
///
/// Uses ID3v2.3 (`Version::Id3v23`), which is the most broadly compatible
/// version and is the default written by Winamp and most other players.
pub fn write_tag_fields(path: &Path, fields: &TagFields) -> Result<()> {
    // Read the existing tag (or start from a blank one) so we don't clobber
    // frames like APIC (cover art) that aren't part of our editor UI.
    let mut tag = Tag::read_from_path(path).unwrap_or_default();

    // Helper: set or remove a simple text frame.
    macro_rules! set_text {
        ($frame:expr, $value:expr) => {
            if $value.is_empty() {
                tag.remove($frame);
            } else {
                tag.set_text($frame, $value);
            }
        };
    }

    set_text!("TIT2", &fields.title);
    set_text!("TPE1", &fields.artist);
    set_text!("TALB", &fields.album);
    set_text!("TPE2", &fields.album_artist);
    set_text!("TCON", &fields.genre);
    set_text!("TBPM", &fields.bpm);

    // Year — stored in TDRC (ID3v2.4) but we write TYER for v2.3 compatibility.
    if fields.year.is_empty() {
        tag.remove("TDRC");
        tag.remove("TYER");
    } else {
        tag.set_text("TDRC", &fields.year);
        tag.set_text("TYER", &fields.year);
    }

    // Track number: "x" or "x/y".
    let trck = match (
        fields.track_number.is_empty(),
        fields.track_total.is_empty(),
    ) {
        (true, _) => String::new(),
        (false, true) => fields.track_number.clone(),
        (false, false) => format!("{}/{}", fields.track_number, fields.track_total),
    };
    set_text!("TRCK", &trck);

    // Disc number: "x" or "x/y".
    let tpos = match (fields.disc_number.is_empty(), fields.disc_total.is_empty()) {
        (true, _) => String::new(),
        (false, true) => fields.disc_number.clone(),
        (false, false) => format!("{}/{}", fields.disc_number, fields.disc_total),
    };
    set_text!("TPOS", &tpos);

    // Comment: write as a default-language empty-description COMM frame.
    // Remove any existing COMM frame with an empty description first.
    let existing_comms: Vec<id3::frame::Comment> = tag.comments().cloned().collect();
    for c in &existing_comms {
        if c.description.is_empty() {
            tag.remove_comment(None, None);
            break;
        }
    }
    if !fields.comment.is_empty() {
        tag.add_frame(id3::frame::Comment {
            lang: "eng".to_string(),
            description: String::new(),
            text: fields.comment.clone(),
        });
    }

    // Artwork: embed image from artwork_path as APIC frame
    if fields.artwork_path.is_empty() {
        // Remove existing pictures if artwork_path is cleared
        tag.remove_all_pictures();
    } else {
        // Expand tilde to home directory if present
        let art_path = if fields.artwork_path.starts_with('~') {
            if let Some(home) = dirs::home_dir() {
                home.join(&fields.artwork_path[2..])
            } else {
                std::path::PathBuf::from(&fields.artwork_path)
            }
        } else {
            std::path::PathBuf::from(&fields.artwork_path)
        };
        if let Ok(img_data) = std::fs::read(&art_path) {
            let mime = if fields.artwork_path.ends_with(".png") {
                "image/png"
            } else {
                "image/jpeg"
            };
            tag.add_frame(id3::frame::Picture {
                mime_type: mime.to_string(),
                picture_type: id3::frame::PictureType::CoverFront,
                description: String::new(),
                data: img_data,
            });
        }
    }

    // Write to disk using ID3v2.3 for broad compatibility.
    tag.write_to_path(path, Version::Id3v23)
        .with_context(|| format!("Failed to write ID3 tag to {}", path.display()))
}

/// Write a single extra frame (from the "Customize" panel) to the tag.
///
/// Reads, modifies, and re-writes the tag so all other frames are preserved.
pub fn write_extra_frame(path: &Path, frame_id: &str, value: &str) -> Result<()> {
    let mut tag = Tag::read_from_path(path).unwrap_or_default();
    if value.is_empty() {
        tag.remove(frame_id);
    } else {
        tag.set_text(frame_id, value);
    }
    tag.write_to_path(path, Version::Id3v23)
        .with_context(|| format!("Failed to write frame {} to {}", frame_id, path.display()))
}

// ---------------------------------------------------------------------------
// Frame label lookup
// ---------------------------------------------------------------------------

/// Return a human-readable label for a known ID3v2 frame identifier.
///
/// Falls back to returning the raw four-character ID for unrecognised frames.
pub fn frame_label<'a>(id: &'a str) -> &'a str {
    match id {
        "TIT1" => "Content Group",
        "TIT2" => "Title",
        "TIT3" => "Subtitle",
        "TALB" => "Album",
        "TOAL" => "Original Album",
        "TRCK" => "Track Number",
        "TPOS" => "Disc Number",
        "TSST" => "Set Subtitle",
        "TSRC" => "ISRC",
        "TPE1" => "Artist",
        "TPE2" => "Album Artist",
        "TPE3" => "Conductor",
        "TPE4" => "Interpreted By",
        "TOPE" => "Original Artist",
        "TCOM" => "Composer",
        "TEXT" => "Lyricist",
        "TOLY" => "Original Lyricist",
        "TMCL" => "Musician Credits",
        "TIPL" => "Involved People",
        "TENC" => "Encoded By",
        "TBPM" => "BPM",
        "TLEN" => "Length (ms)",
        "TKEY" => "Initial Key",
        "TLAN" => "Language",
        "TCON" => "Genre",
        "TFLT" => "File Type",
        "TMED" => "Media Type",
        "TMOO" => "Mood",
        "TCOP" => "Copyright",
        "TPRO" => "Produced Notice",
        "TPUB" => "Publisher",
        "TOWN" => "File Owner",
        "TRSN" => "Radio Station Name",
        "TRSO" => "Radio Station Owner",
        "TOFN" => "Original Filename",
        "TDLY" => "Playlist Delay",
        "TDEN" => "Encoding Time",
        "TDOR" => "Original Release Time",
        "TDRC" => "Recording Time",
        "TDRL" => "Release Time",
        "TDTG" => "Tagging Time",
        "TSSE" => "Software/Hardware",
        "TSOA" => "Album Sort Order",
        "TSOP" => "Artist Sort Order",
        "TSOT" => "Title Sort Order",
        "TYER" => "Year (legacy)",
        "TRDA" => "Recording Dates (legacy)",
        "TXXX" => "User-Defined Text",
        "USLT" => "Unsynchronised Lyrics",
        "WCOM" => "Commercial Info URL",
        "WCOP" => "Copyright URL",
        "WOAF" => "Official Audio File URL",
        "WOAR" => "Official Artist URL",
        "WOAS" => "Official Audio Source URL",
        "WORS" => "Official Radio Station URL",
        "WPAY" => "Payment URL",
        "WPUB" => "Publisher URL",
        "WXXX" => "User-Defined URL",
        _ => id, // unknown — show the raw frame ID
    }
}

/// Return all "extra" (non-default) text frame IDs that Sparkamp knows about,
/// paired with their human-readable label.  Used to populate the "Customize"
/// panel's "add frame" picker.
#[allow(dead_code)]
pub fn all_extra_frame_ids() -> Vec<(&'static str, &'static str)> {
    vec![
        ("TIT1", "Content Group"),
        ("TIT3", "Subtitle"),
        ("TOAL", "Original Album"),
        ("TSRC", "ISRC"),
        ("TPE3", "Conductor"),
        ("TPE4", "Interpreted By"),
        ("TOPE", "Original Artist"),
        ("TCOM", "Composer"),
        ("TEXT", "Lyricist"),
        ("TOLY", "Original Lyricist"),
        ("TMCL", "Musician Credits"),
        ("TIPL", "Involved People"),
        ("TENC", "Encoded By"),
        ("TLEN", "Length (ms)"),
        ("TKEY", "Initial Key"),
        ("TLAN", "Language"),
        ("TFLT", "File Type"),
        ("TMED", "Media Type"),
        ("TMOO", "Mood"),
        ("TCOP", "Copyright"),
        ("TPUB", "Publisher"),
        ("TOWN", "File Owner"),
        ("TRSN", "Radio Station Name"),
        ("TOFN", "Original Filename"),
        ("TSSE", "Software/Hardware"),
        ("TSOA", "Album Sort Order"),
        ("TSOP", "Artist Sort Order"),
        ("TSOT", "Title Sort Order"),
        ("USLT", "Unsynchronised Lyrics"),
        ("TXXX", "User-Defined Text"),
        ("WXXX", "User-Defined URL"),
    ]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Create a temporary MP3-style file with an ID3v2 tag and return its path.
    fn make_tagged_mp3(title: &str, artist: &str, album: &str) -> NamedTempFile {
        // Write a minimal ID3v2.3 tag followed by a fake (silent) MPEG frame.
        // The id3 crate's write_to_path only needs a writable file path — it
        // does not validate the audio payload — so the fake frame is enough
        // for our read/write tests.
        let mut f = NamedTempFile::with_suffix(".mp3").unwrap();

        // Write a 4-byte placeholder so the file is not empty (some ID3
        // implementations check for an existing file before writing).
        f.write_all(&[0xFFu8, 0xFB, 0x90, 0x00]).unwrap();
        f.flush().unwrap();

        let path = f.path().to_path_buf();

        // Build a tag and write it.
        let mut tag = Tag::new();
        tag.set_title(title);
        tag.set_artist(artist);
        tag.set_album(album);
        tag.write_to_path(&path, Version::Id3v23).unwrap();

        f
    }

    // -----------------------------------------------------------------------
    // read_tag_fields
    // -----------------------------------------------------------------------

    #[test]
    fn read_basic_fields() {
        let file = make_tagged_mp3("Test Title", "Test Artist", "Test Album");
        let fields = read_tag_fields(file.path());
        assert_eq!(fields.title, "Test Title");
        assert_eq!(fields.artist, "Test Artist");
        assert_eq!(fields.album, "Test Album");
    }

    #[test]
    fn read_missing_tag_returns_defaults() {
        // A file with no ID3 tag — from_path will fail, defaulting all fields.
        let mut f = NamedTempFile::with_suffix(".mp3").unwrap();
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        let fields = read_tag_fields(f.path());
        assert!(fields.title.is_empty());
        assert!(fields.artist.is_empty());
    }

    #[test]
    fn read_track_x_of_y() {
        let mut f = NamedTempFile::with_suffix(".mp3").unwrap();
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        let mut tag = Tag::new();
        tag.set_text("TRCK", "3/12");
        tag.write_to_path(f.path(), Version::Id3v23).unwrap();

        let fields = read_tag_fields(f.path());
        assert_eq!(fields.track_number, "3");
        assert_eq!(fields.track_total, "12");
    }

    #[test]
    fn read_track_number_only() {
        let mut f = NamedTempFile::with_suffix(".mp3").unwrap();
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        let mut tag = Tag::new();
        tag.set_text("TRCK", "7");
        tag.write_to_path(f.path(), Version::Id3v23).unwrap();

        let fields = read_tag_fields(f.path());
        assert_eq!(fields.track_number, "7");
        assert!(fields.track_total.is_empty());
    }

    // -----------------------------------------------------------------------
    // write_tag_fields
    // -----------------------------------------------------------------------

    #[test]
    fn write_then_read_roundtrip() {
        let file = make_tagged_mp3("Old Title", "Old Artist", "Old Album");
        let new_fields = TagFields {
            title: "New Title".into(),
            artist: "New Artist".into(),
            album: "New Album".into(),
            album_artist: "New Album Artist".into(),
            genre: "Electronic".into(),
            year: "2024".into(),
            track_number: "5".into(),
            track_total: "10".into(),
            disc_number: "1".into(),
            disc_total: "2".into(),
            bpm: "128".into(),
            comment: "Test comment".into(),
            artwork_path: String::new(),
        };

        write_tag_fields(file.path(), &new_fields).unwrap();
        let read_back = read_tag_fields(file.path());

        assert_eq!(read_back.title, "New Title");
        assert_eq!(read_back.artist, "New Artist");
        assert_eq!(read_back.album, "New Album");
        assert_eq!(read_back.album_artist, "New Album Artist");
        assert_eq!(read_back.genre, "Electronic");
        assert_eq!(read_back.year, "2024");
        assert_eq!(read_back.track_number, "5");
        assert_eq!(read_back.track_total, "10");
        assert_eq!(read_back.disc_number, "1");
        assert_eq!(read_back.disc_total, "2");
        assert_eq!(read_back.bpm, "128");
        assert_eq!(read_back.comment, "Test comment");
    }

    #[test]
    fn write_preserves_unrelated_frames() {
        let mut f = NamedTempFile::with_suffix(".mp3").unwrap();
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        let mut tag = Tag::new();
        tag.set_title("Original");
        tag.set_text("TCOM", "A Composer"); // not in default fields
        tag.write_to_path(f.path(), Version::Id3v23).unwrap();

        // Write default fields (no TCOM).
        let fields = TagFields {
            title: "Updated".into(),
            ..Default::default()
        };
        write_tag_fields(f.path(), &fields).unwrap();

        // TCOM should still be present.
        let tag_after = Tag::read_from_path(f.path()).unwrap();
        let composer = tag_after
            .get("TCOM")
            .and_then(|f| f.content().text())
            .unwrap_or("");
        assert_eq!(composer, "A Composer");
        assert_eq!(tag_after.title().unwrap_or(""), "Updated");
    }

    #[test]
    fn write_empty_field_removes_frame() {
        let file = make_tagged_mp3("Title", "Artist", "Album");
        let fields = TagFields {
            title: "Title".into(),
            artist: String::new(), // clear the artist
            ..Default::default()
        };
        write_tag_fields(file.path(), &fields).unwrap();
        let tag = Tag::read_from_path(file.path()).unwrap();
        assert!(tag.artist().is_none() || tag.artist().unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // field_pairs
    // -----------------------------------------------------------------------

    #[test]
    fn field_pairs_returns_12_entries() {
        let fields = TagFields::default();
        assert_eq!(fields.field_pairs().len(), 12);
    }

    // -----------------------------------------------------------------------
    // frame_label
    // -----------------------------------------------------------------------

    #[test]
    fn frame_label_known() {
        assert_eq!(frame_label("TIT2"), "Title");
        assert_eq!(frame_label("TPE1"), "Artist");
        assert_eq!(frame_label("TALB"), "Album");
    }

    #[test]
    fn frame_label_unknown_returns_id() {
        assert_eq!(frame_label("XXXX"), "XXXX");
    }

    // -----------------------------------------------------------------------
    // ID3V1_GENRES
    // -----------------------------------------------------------------------

    #[test]
    fn genres_list_not_empty() {
        assert!(!ID3V1_GENRES.is_empty());
    }

    #[test]
    fn genres_contains_classic_entries() {
        assert!(ID3V1_GENRES.contains(&"Rock"));
        assert!(ID3V1_GENRES.contains(&"Jazz"));
        assert!(ID3V1_GENRES.contains(&"Electronic"));
    }
}
