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
    pub comment: String, // default comment (no content description)
    pub composer: String,        // TCOM
    pub original_artist: String, // TOPE
    pub copyright: String,       // TCOP
    pub url: String,             // WXXX — a link frame, not a text frame
    pub encoded_by: String,      // TENC
    pub lyric: String,           // USLT — unsynchronised lyrics content
    pub artwork_path: String,    // path to artwork file (not embedded in tag)
}

impl TagFields {
    /// Return an ordered list of `(label, field_value)` pairs for rendering
    /// a two-column form.  The left column ends at the midpoint so callers
    /// can split at `len()/2` for a balanced two-column layout.
    ///
    /// Each label is a short human-readable string; the value is a clone of
    /// the field at the time of the call.  Callers that need mutable access
    /// should edit the struct fields directly.
    /// The field named by `field_id`, or `None` for an id this build does not
    /// know.
    ///
    /// Keyed by id rather than by position, because position is what the TUI
    /// used and it tied "which field" to "which row". Both frontends and the
    /// tag layer name fields the same way now, and this is where that name is
    /// resolved.
    pub fn value(&self, field_id: &str) -> Option<&str> {
        Some(match field_id {
            "title" => &self.title,
            "artist" => &self.artist,
            "album" => &self.album,
            "album_artist" => &self.album_artist,
            "genre" => &self.genre,
            "year" => &self.year,
            "track_num" => &self.track_number,
            "track_total" => &self.track_total,
            "disc_num" => &self.disc_number,
            "disc_total" => &self.disc_total,
            "bpm" => &self.bpm,
            "comment" => &self.comment,
            "composer" => &self.composer,
            "original_artist" => &self.original_artist,
            "copyright" => &self.copyright,
            "url" => &self.url,
            "encoded_by" => &self.encoded_by,
            "lyric" => &self.lyric,
            _ => return None,
        })
    }

    /// [`Self::value`], mutably.
    pub fn value_mut(&mut self, field_id: &str) -> Option<&mut String> {
        Some(match field_id {
            "title" => &mut self.title,
            "artist" => &mut self.artist,
            "album" => &mut self.album,
            "album_artist" => &mut self.album_artist,
            "genre" => &mut self.genre,
            "year" => &mut self.year,
            "track_num" => &mut self.track_number,
            "track_total" => &mut self.track_total,
            "disc_num" => &mut self.disc_number,
            "disc_total" => &mut self.disc_total,
            "bpm" => &mut self.bpm,
            "comment" => &mut self.comment,
            "composer" => &mut self.composer,
            "original_artist" => &mut self.original_artist,
            "copyright" => &mut self.copyright,
            "url" => &mut self.url,
            "encoded_by" => &mut self.encoded_by,
            "lyric" => &mut self.lyric,
            _ => return None,
        })
    }

    /// The editor field ids for [`Self::field_pairs`], in the same order.
    ///
    /// `field_pairs` carries labels for people; these are the ids the rest of
    /// the app keys fields by, and what [`supports_field`] takes. They live
    /// next to each other so the pairing stays checkable, which a test does.
    pub fn field_ids() -> [&'static str; 18] {
        [
            "title",
            "artist",
            "album",
            "album_artist",
            "genre",
            "year",
            "track_num",
            "track_total",
            "disc_num",
            "disc_total",
            "bpm",
            "comment",
            "composer",
            "original_artist",
            "copyright",
            "url",
            "encoded_by",
            "lyric",
        ]
    }

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
            ("Composer", self.composer.clone()),
            ("Original Artist", self.original_artist.clone()),
            ("Copyright", self.copyright.clone()),
            ("URL", self.url.clone()),
            ("Encoded By", self.encoded_by.clone()),
            ("Lyric", self.lyric.clone()),
        ]
    }
}

// ---------------------------------------------------------------------------
// ExtraFrame — custom / additional ID3v2 frames
// ---------------------------------------------------------------------------

/// Marks an [`ExtraFrame::id`] as addressing a TXXX frame by description
/// rather than by frame ID (`TXXX:REPLAYGAIN_TRACK_GAIN`). Shared by
/// [`read_extra_frames`] and [`write_extra_frame`] so the two never drift.
pub const TXXX_PREFIX: &str = "TXXX:";

/// A raw ID3v2 text frame that is not represented in [`TagFields`].
///
/// Used by the "Customize" panel to let the user add arbitrary frames.
/// Only text frames (IDs starting with 'T') and COMM/USLT are handled;
/// binary frames (cover art, etc.) are read-only in this version.
#[derive(Debug, Clone)]
#[allow(dead_code)]
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
// ---------------------------------------------------------------------------
// Container routing
//
// MP3 keeps the `id3` crate. It is what every other MP3 tag write in Sparkamp
// uses, and two ID3 writers on one container would produce frames whose
// version and text encoding depend on which code path last touched the file.
//
// Everything else goes through lofty, which knows where each container keeps
// its metadata: Vorbis comments in FLAC, Ogg and Opus, an ilst atom in MP4,
// APEv2 in Monkey's Audio, Musepack and WavPack, an ID3 chunk in WAV and AIFF.
//
// Writing ID3 frames into those was not merely useless. `id3` writes a tag by
// prepending an ID3v2 header, so editing a FLAC left a file that no longer
// began with `fLaC`, and the real Vorbis comments were never touched. The
// edit appeared to save and changed nothing. Measured across every format
// Sparkamp lists: only MP3 read its own tags back, and eight of eleven had a
// foreign ID3 header prepended.
// ---------------------------------------------------------------------------

/// Lowercase extension, or an empty string.
fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// MP3 is the one container this module tags with the `id3` crate.
fn is_mpeg(path: &Path) -> bool {
    extension_of(path) == "mp3"
}

/// The editor's fields paired with the key lofty stores them under. Lofty maps
/// each to whatever the target container actually calls it.
///
/// `url` is absent on purpose: it is ID3's `WXXX`, a user-defined link frame
/// with no equivalent in any other tag format, so it stays MP3-only rather
/// than being forced into an unrelated key.
/// The lofty keys a field may be stored under, best first.
///
/// A list rather than one key, because the right key depends on the container
/// and lofty is deliberate about the difference. `IntegerBpm` is documented as
/// ID3v2 and MP4 only, so a Vorbis comment needs `Bpm`; ID3v2 maps `USLT` to
/// `UnsyncLyrics` and does not support `Lyrics` at all, because ID3 overloads
/// synchronized and unsynchronized lyrics, while a Vorbis comment has both.
///
/// Asking for one key and hoping is what silently dropped BPM on every Vorbis
/// container and lyrics on every ID3-in-a-non-MP3 container. The caller walks
/// the list and takes the first key the target tag type actually maps.
///
/// `url` is absent: it is ID3's `WXXX`, a user-defined link frame with no
/// equivalent elsewhere and no lofty key, so it stays MP3-only through the
/// `id3` crate rather than being forced into an unrelated field.
fn item_keys_for_field(field_id: &str) -> &'static [lofty::prelude::ItemKey] {
    use lofty::prelude::ItemKey;
    match field_id {
        "title" => &[ItemKey::TrackTitle],
        "artist" => &[ItemKey::TrackArtist],
        "album" => &[ItemKey::AlbumTitle],
        "album_artist" => &[ItemKey::AlbumArtist],
        "genre" => &[ItemKey::Genre],
        "year" => &[ItemKey::RecordingDate, ItemKey::Year],
        "track_num" => &[ItemKey::TrackNumber],
        "track_total" => &[ItemKey::TrackTotal],
        "disc_num" => &[ItemKey::DiscNumber],
        "disc_total" => &[ItemKey::DiscTotal],
        "bpm" => &[ItemKey::IntegerBpm, ItemKey::Bpm],
        "comment" => &[ItemKey::Comment],
        "composer" => &[ItemKey::Composer],
        "original_artist" => &[ItemKey::OriginalArtist],
        "copyright" => &[ItemKey::CopyrightMessage],
        "encoded_by" => &[ItemKey::EncodedBy],
        "lyric" => &[ItemKey::Lyrics, ItemKey::UnsyncLyrics],
        _ => &[],
    }
}

/// The first key in `field_id`'s list that `kind` can actually store.
fn item_key_in(field_id: &str, kind: lofty::tag::TagType) -> Option<lofty::prelude::ItemKey> {
    item_keys_for_field(field_id)
        .iter()
        .find(|k| k.map_key(kind).is_some())
        .copied()
}

/// Each editable field paired with its value, keyed by field id. The key each
/// container stores it under is resolved per tag type by the writer.
fn lofty_field_pairs(fields: &TagFields) -> Vec<(&'static str, &str)> {
    TagFields::field_ids()
        .into_iter()
        .filter(|id| !item_keys_for_field(id).is_empty())
        .map(|id| (id, fields.value(id).unwrap_or_default()))
        .collect()
}

/// Read the editor's fields from any container lofty understands.
fn read_lofty_fields(path: &Path) -> Option<TagFields> {
    use lofty::file::TaggedFileExt;

    let tagged = lofty::probe::Probe::open(path).ok()?.read().ok()?;
    // A file can carry more than one tag. A WAV with both a RIFF INFO chunk
    // and an ID3 chunk is ordinary. The primary is the one the format
    // prefers; falling back to the first means a file tagged only in the
    // other form still reads rather than coming back blank.
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    // Read through the same candidate list the writer stores under, and take
    // the first key that holds anything. A file tagged by another program may
    // have used the other candidate: a Vorbis comment can carry LYRICS or
    // UNSYNCEDLYRICS, and reading only one of them loses the other.
    let get = |field_id: &str| {
        item_keys_for_field(field_id)
            .iter()
            .find_map(|k| tag.get_string(*k))
            .unwrap_or_default()
            .to_string()
    };

    let mut fields = TagFields {
        // `url` has no lofty key; it is ID3's WXXX and reaches only MP3.
        url: String::new(),
        artwork_path: String::new(),
        ..TagFields::default()
    };
    for id in TagFields::field_ids() {
        if let Some(slot) = fields.value_mut(id) {
            if !item_keys_for_field(id).is_empty() {
                *slot = get(id);
            }
        }
    }
    Some(fields)
}

/// Write the editor's fields into any container lofty understands.
fn write_lofty_fields(path: &Path, fields: &TagFields) -> Result<()> {
    write_lofty_items(path, &lofty_field_pairs(fields), &artwork_change_for(fields))
}

/// Read the editor's fields, whatever the container.
pub fn read_tag_fields(path: &Path) -> TagFields {
    if is_mpeg(path) {
        read_id3_fields(path)
    } else {
        read_lofty_fields(path).unwrap_or_default()
    }
}

/// Write the editor's fields, whatever the container.
pub fn write_tag_fields(path: &Path, fields: &TagFields) -> Result<()> {
    if is_mpeg(path) {
        write_id3_fields(path, fields)
    } else {
        write_lofty_fields(path, fields)
    }
}

fn read_id3_fields(path: &Path) -> TagFields {
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

    let get_extended = |frame_id: &str| -> String {
        tag.get(frame_id)
            .and_then(|f| f.content().text())
            .unwrap_or("")
            .to_string()
    };
    // WXXX carries ExtendedLink content — pull the link out explicitly
    // rather than relying on Content::text() covering link frames.
    let url = tag
        .get("WXXX")
        .map(|f| match f.content() {
            id3::Content::ExtendedLink(e) => e.link.clone(),
            c => c.text().unwrap_or("").to_string(),
        })
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
        composer: get_extended("TCOM"),
        original_artist: get_extended("TOPE"),
        copyright: get_extended("TCOP"),
        url,
        encoded_by: get_extended("TENC"),
        lyric: tag.lyrics().next().map(|l| l.text.clone()).unwrap_or_default(),
        artwork_path: String::new(),
    }
}

/// Read all text frames from the tag that are **not** in the default field set.
///
/// Used by the "Customize" panel to show additional ID3v2 frames the user
/// can optionally add to their editor view.  Binary frames (APIC, etc.) and
/// frames already covered by [`TagFields`] are excluded.
/// What a save should do with the file's embedded cover art.
enum ArtworkChange {
    /// Leave whatever is there. Used by the extra-frames writer, which has no
    /// business touching pictures.
    Leave,
    /// Remove every embedded picture: the editor's artwork field was cleared.
    Clear,
    /// Replace the art with this image.
    Set { mime: String, data: Vec<u8> },
}

/// Resolve the editor's `artwork_path` into the bytes and MIME type to embed.
///
/// Shared so both tag stacks expand `~` the same way and agree on the MIME
/// type, which is taken from the extension.
fn artwork_change_for(fields: &TagFields) -> ArtworkChange {
    if fields.artwork_path.is_empty() {
        return ArtworkChange::Clear;
    }
    let art_path = if let Some(rest) = fields.artwork_path.strip_prefix("~/") {
        match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => std::path::PathBuf::from(&fields.artwork_path),
        }
    } else {
        std::path::PathBuf::from(&fields.artwork_path)
    };

    let mime = match art_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        // jpg/jpeg and anything unrecognised keep the old default, so
        // behaviour only changes where it was wrong.
        _ => "image/jpeg",
    };

    match std::fs::read(&art_path) {
        Ok(data) => ArtworkChange::Set {
            mime: mime.to_string(),
            data,
        },
        Err(e) => {
            eprintln!("Failed to read artwork file '{}': {e}", art_path.display());
            ArtworkChange::Leave
        }
    }
}

/// The embedded cover art, whatever the container.
///
/// MP3 keeps its APIC frame; every other container is asked through lofty,
/// which knows a FLAC PICTURE block and an MP4 `covr` atom are the same idea.
// Reached only from `src/ffi`, which `src/main.rs` does not declare, so this
// is unreachable in the bin crate while staying live in the lib.
#[allow(dead_code)]
pub fn read_artwork(path: &Path) -> Option<Vec<u8>> {
    if is_mpeg(path) {
        return Tag::read_from_path(path)
            .ok()
            .and_then(|tag| tag.pictures().next().map(|p| p.data.clone()));
    }
    use lofty::file::TaggedFileExt;
    let tagged = lofty::probe::Probe::open(path).ok()?.read().ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    tag.pictures().first().map(|p| p.data().to_vec())
}

/// Frame IDs the main form already owns, so the extra-frames view must not
/// repeat them.
const DEFAULT_IDS: &[&str] = &[
    "TIT2", "TPE1", "TALB", "TPE2", "TCON", "TDRC", "TRCK", "TPOS", "TBPM", "COMM", "TCOM",
    "TOPE", "TCOP", "WXXX", "TENC", "USLT",
];

/// The tag format this container uses, or `None` when Sparkamp cannot tag it
/// at all.
///
/// WMA and TTA land here: neither has a tag format lofty can write, so the
/// honest answer is that there is nowhere to put anything, rather than
/// writing something the file cannot carry.
fn lofty_tag_type(path: &Path) -> Option<lofty::tag::TagType> {
    use lofty::file::TaggedFileExt;
    let tagged = lofty::probe::Probe::open(path).ok()?.read().ok()?;
    Some(tagged.primary_tag_type())
}

/// Whether this file can carry tags at all.
///
/// False for WMA and TTA, and for anything unreadable. The editor uses it to
/// say so plainly instead of presenting a form whose Save cannot work.
// Reached only from `src/ffi`, which `src/main.rs` does not declare, so this
// is unreachable in the bin crate while staying live in the lib.
#[allow(dead_code)]
pub fn is_taggable(path: &Path) -> bool {
    is_mpeg(path) || lofty_tag_type(path).is_some()
}

/// Whether this file's container can carry `frame_id`.
///
/// The editor speaks ID3 frame IDs, but most containers do not: there is no
/// place in a FLAC for `WXXX`, ID3's user-defined link frame. The UI asks
/// this so it can offer the fields that mean something for the file in front
/// of it, rather than all of them with most silently dropped on save.
pub fn supports_frame(path: &Path, frame_id: &str) -> bool {
    use lofty::prelude::ItemKey;
    use lofty::tag::TagType;
    if is_mpeg(path) {
        return true;
    }
    let Some(kind) = lofty_tag_type(path) else {
        return false;
    };
    let lookup = frame_id.strip_prefix(TXXX_PREFIX).unwrap_or(frame_id);
    ItemKey::from_key(TagType::Id3v2, lookup)
        .and_then(|key| key.map_key(kind))
        .is_some()
}

/// The ID3 frame an editor field id addresses.
///
/// The editor and the Media Library name fields in snake case; the tag layer
/// speaks ID3 frame IDs. `None` for an id this build does not know, so a
/// caller's typo hides the field rather than offering one that cannot save.
///
/// Track and disc totals share `TRCK` and `TPOS` with their numbers, because
/// ID3 packs both halves into one `n/m` frame. Containers that keep them apart
/// answer for either half through the same question.
fn frame_for_field(field_id: &str) -> Option<&'static str> {
    Some(match field_id {
        "title" => "TIT2",
        "artist" => "TPE1",
        "album" => "TALB",
        "album_artist" => "TPE2",
        "year" => "TDRC",
        "genre" => "TCON",
        "track_num" | "track_total" => "TRCK",
        "disc_num" | "disc_total" => "TPOS",
        "bpm" => "TBPM",
        "comment" => "COMM",
        "composer" => "TCOM",
        "original_artist" => "TOPE",
        "copyright" => "TCOP",
        "url" => "WXXX",
        "encoded_by" => "TENC",
        "lyric" => "USLT",
        // ReplayGain is not a frame of its own. It rides on whatever tag the
        // file carries, so the question is whether that TXXX value fits.
        "replaygain" => "TXXX:REPLAYGAIN_TRACK_GAIN",
        _ => return None,
    })
}

/// Whether this file's container can carry the editor field `field_id`.
///
/// [`supports_frame`] in the vocabulary the GTK and TUI editors use. The macOS
/// editor is frame-id driven and asks `supports_frame` through the FFI; these
/// two name their fields, so they ask here and both end at the same answer.
pub fn supports_field(path: &Path, field_id: &str) -> bool {
    // MP3 is one case among many now, rather than the vocabulary the others
    // are described in. It answers first because it does not go through lofty
    // at all: the `id3` crate writes it, and ID3 has a frame for every field
    // the editor offers, `url`'s WXXX included.
    if is_mpeg(path) {
        return frame_for_field(field_id).is_some();
    }
    // Everything else is asked about its own tag format directly. Routing this
    // through an ID3 frame is what hid BPM on every Vorbis container: ID3 keys
    // it as an integer frame that a Vorbis comment has no equivalent for, even
    // though `BPM` is a perfectly standard Vorbis field.
    let Some(kind) = lofty_tag_type(path) else {
        return false;
    };
    item_key_in(field_id, kind).is_some()
}

/// Apply `pairs` to every tag the file carries and save.
///
/// Shared by the main form and the extra frames so both follow the same rule:
/// update each tag present rather than only the preferred one. A WAV can hold
/// a RIFF INFO chunk and an ID3 chunk at once, and writing just one leaves the
/// file claiming two different titles.
/// Write one already-resolved key, for callers that speak keys rather than
/// field ids. The extra-frame editor is the only one: it looks its key up from
/// an ID3 frame id, which the main form's fields no longer do.
fn write_lofty_item_key(
    path: &Path,
    key: lofty::prelude::ItemKey,
    value: &str,
) -> Result<()> {
    write_lofty_with(path, &ArtworkChange::Leave, |tag, tag_type| {
        if key.map_key(tag_type).is_none() {
            return;
        }
        tag.remove_key(key);
        if !value.is_empty() {
            tag.insert_text(key, value.to_string());
        }
    })
}

fn write_lofty_items(
    path: &Path,
    pairs: &[(&str, &str)],
    artwork: &ArtworkChange,
) -> Result<()> {
    write_lofty_with(path, artwork, |tag, tag_type| {
        for (field_id, value) in pairs {
            // Which key this container stores the field under is decided here,
            // per tag type, because the answer differs: an ID3v2 chunk inside a
            // WAV wants UnsyncLyrics where the Vorbis comment in a FLAC wants
            // Lyrics. A field this container cannot represent at all is skipped
            // rather than failing the save; the editor does not offer it.
            let Some(key) = item_key_in(field_id, tag_type) else {
                continue;
            };
            // Clear every candidate, not just the one being written, so a value
            // left behind under the other key cannot outlive the edit and be
            // read back in its place.
            for candidate in item_keys_for_field(field_id) {
                tag.remove_key(*candidate);
            }
            if !value.is_empty() {
                tag.insert_text(key, (*value).to_string());
            }
        }
    })
}

/// Apply `edit` to every tag the file carries, plus the one its format
/// prefers, then save each.
fn write_lofty_with(
    path: &Path,
    artwork: &ArtworkChange,
    edit: impl Fn(&mut lofty::tag::Tag, lofty::tag::TagType),
) -> Result<()> {
    use lofty::config::WriteOptions;
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::{Tag as LoftyTag, TagType};

    let mut tagged = lofty::probe::Probe::open(path)
        .and_then(|p| p.read())
        .map_err(|e| anyhow::anyhow!("{} cannot be tagged: {e}", path.display()))?;

    // Every tag the file already carries, plus the one the format prefers.
    //
    // Both halves matter. Existing tags are updated so a WAV holding a RIFF
    // INFO chunk and an ID3 chunk cannot end up claiming two different
    // titles. The preferred tag is added even when others exist because a
    // secondary tag may have nowhere to put the value: an AIFF carrying only
    // its native text chunks has no place for an ISRC, so without this the
    // write silently did nothing.
    let mut targets: Vec<TagType> = tagged.tags().iter().map(|t| t.tag_type()).collect();
    let primary = tagged.primary_tag_type();
    if !targets.contains(&primary) {
        targets.push(primary);
    }

    for tag_type in targets {
        if tagged.tag(tag_type).is_none() {
            tagged.insert_tag(LoftyTag::new(tag_type));
        }
        let Some(tag) = tagged.tag_mut(tag_type) else {
            continue;
        };
        edit(tag, tag_type);
        match artwork {
            ArtworkChange::Leave => {}
            ArtworkChange::Clear => {
                while !tag.pictures().is_empty() {
                    tag.remove_picture(0);
                }
            }
            ArtworkChange::Set { mime, data } => {
                while !tag.pictures().is_empty() {
                    tag.remove_picture(0);
                }
                tag.push_picture(
                    lofty::picture::Picture::unchecked(data.clone())
                        .pic_type(lofty::picture::PictureType::CoverFront)
                        .mime_type(lofty::picture::MimeType::from_str(mime))
                        .build(),
                );
            }
        }
    }
    // One save for the file, after every tag it carries has been edited.
    //
    // This used to call `Tag::save_to_path` inside the loop, so a file with
    // both a RIFF INFO chunk and an ID3 chunk was opened and rewritten twice,
    // and a failure on the second left the first already written while the
    // caller saw only an error. `AudioFile::save_to_path` still writes tag by
    // tag, and says in its own TODO that it would rather not, but it does so
    // against one handle and leaves the partial-write window to lofty rather
    // than widening it here.
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| anyhow::anyhow!("write tags to {}: {e}", path.display()))?;
    crate::watch::register_self_write(path);
    Ok(())
}

/// Extra frames from any container lofty understands.
fn read_lofty_extra_frames(path: &Path) -> Vec<ExtraFrame> {
    use lofty::file::TaggedFileExt;
    use lofty::tag::TagType;

    let Ok(tagged) = lofty::probe::Probe::open(path).and_then(|p| p.read()) else {
        return Vec::new();
    };
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Vec::new();
    };
    tag.items()
        .filter_map(|item| {
            // Reported in the ID3 vocabulary the editor and its UI speak, so a
            // FLAC's ARTISTSORT arrives as TSOP and lands in the row it would
            // occupy for an MP3.
            let mapped = item.key().map_key(TagType::Id3v2)?;
            if DEFAULT_IDS.contains(&mapped) {
                return None;
            }
            let value = item.value().text()?;
            if value.is_empty() {
                return None;
            }
            // Anything that is not a four-character frame id is one of ID3's
            // user-defined names, and the MP3 path reports those with the
            // `TXXX:` prefix. Matching it keeps one vocabulary across formats.
            let id = if mapped.len() == 4 {
                mapped.to_string()
            } else {
                format!("{TXXX_PREFIX}{mapped}")
            };
            Some(ExtraFrame {
                label: frame_label(&id).to_string(),
                id,
                value: value.to_string(),
            })
        })
        .collect()
}

/// Write one extra frame into any container lofty understands.
fn write_lofty_extra_frame(path: &Path, frame_id: &str, value: &str) -> Result<()> {
    use lofty::prelude::ItemKey;
    use lofty::tag::TagType;
    // `TXXX:DESCRIPTION` is ID3's user-defined text frame. Several of those
    // descriptions are standard names other formats have a real home for,
    // REPLAYGAIN_TRACK_GAIN among them, so the description is what gets
    // looked up, not the literal "TXXX".
    let lookup = frame_id.strip_prefix(TXXX_PREFIX).unwrap_or(frame_id);
    let key = ItemKey::from_key(TagType::Id3v2, lookup).ok_or_else(|| {
        anyhow::anyhow!("{frame_id} has no equivalent outside an ID3 tag")
    })?;
    write_lofty_item_key(path, key, value)
}

/// Extra frames, whatever the container.
pub fn read_extra_frames(path: &Path) -> Vec<ExtraFrame> {
    if is_mpeg(path) {
        read_id3_extra_frames(path)
    } else {
        read_lofty_extra_frames(path)
    }
}

/// Write one extra frame, whatever the container.
pub fn write_extra_frame(path: &Path, frame_id: &str, value: &str) -> Result<()> {
    if is_mpeg(path) {
        write_id3_extra_frame(path, frame_id, value)
    } else {
        write_lofty_extra_frame(path, frame_id, value)
    }
}

fn read_id3_extra_frames(path: &Path) -> Vec<ExtraFrame> {
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
            // TXXX (user-defined text) carries its own description, and that
            // description — not the frame ID — is what identifies it. This is
            // where REPLAYGAIN_TRACK_GAIN and friends live. `content().text()`
            // returns None for TXXX, so without this arm a file with the four
            // REPLAYGAIN_* frames showed four blank rows all labelled "TXXX",
            // and the values were invisible in the Customize panel.
            if let Some(ext) = f.content().extended_text() {
                return ExtraFrame {
                    label: ext.description.clone(),
                    id: format!("{TXXX_PREFIX}{}", ext.description),
                    value: ext.value.clone(),
                };
            }
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
fn write_id3_fields(path: &Path, fields: &TagFields) -> Result<()> {
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
    set_text!("TCOM", &fields.composer);
    set_text!("TOPE", &fields.original_artist);
    set_text!("TCOP", &fields.copyright);
    set_text!("TENC", &fields.encoded_by);

    // WXXX is a link frame — set_text would serialize it as a malformed
    // text frame, so build the ExtendedLink content explicitly.
    tag.remove("WXXX");
    if !fields.url.is_empty() {
        tag.add_frame(id3::Frame::with_content(
            "WXXX",
            id3::Content::ExtendedLink(id3::frame::ExtendedLink {
                description: String::new(),
                link: fields.url.clone(),
            }),
        ));
    }

    // USLT likewise carries Lyrics content rather than plain text.
    tag.remove("USLT");
    if !fields.lyric.is_empty() {
        tag.add_frame(id3::frame::Lyrics {
            lang: "eng".to_string(),
            description: String::new(),
            text: fields.lyric.clone(),
        });
    }

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

        match std::fs::read(&art_path) {
            Ok(img_data) => {
                let mime = match art_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .as_deref()
                {
                    Some("png") => "image/png",
                    Some("gif") => "image/gif",
                    Some("webp") => "image/webp",
                    // jpg/jpeg and anything unrecognized — keep the old
                    // default so behavior only changes where it was wrong.
                    _ => "image/jpeg",
                };
                tag.add_frame(id3::frame::Picture {
                    mime_type: mime.to_string(),
                    picture_type: id3::frame::PictureType::CoverFront,
                    description: String::new(),
                    data: img_data,
                });
            }
            Err(e) => {
                eprintln!(
                    "Failed to read artwork file '{}': {}",
                    art_path.display(),
                    e
                );
            }
        }
    }

    // Write to disk using ID3v2.3 for broad compatibility.
    tag.write_to_path(path, Version::Id3v23)
        .with_context(|| format!("Failed to write ID3 tag to {}", path.display()))?;

    // Suppress the watcher: this is Sparkamp's own write, not an external
    // change. Covers both tag edits and APIC artwork embedded above.
    crate::watch::register_self_write(path);

    Ok(())
}

/// Write a single extra frame (from the "Customize" panel) to the tag.
///
/// Reads, modifies, and re-writes the tag so all other frames are preserved.
///
/// A `frame_id` of `TXXX:DESCRIPTION` addresses one user-defined text frame by
/// its description (the form `read_extra_frames` hands out) — needed because
/// every TXXX frame shares the same four-character ID, so `set_text("TXXX", …)`
/// could not say *which* one to write.
fn write_id3_extra_frame(path: &Path, frame_id: &str, value: &str) -> Result<()> {
    let mut tag = Tag::read_from_path(path).unwrap_or_default();
    if let Some(desc) = frame_id.strip_prefix(TXXX_PREFIX) {
        // Always drop the old frame first so a write replaces rather than
        // stacks a second frame with the same description.
        tag.remove_extended_text(Some(desc), None);
        if !value.is_empty() {
            tag.add_frame(id3::frame::ExtendedText {
                description: desc.to_string(),
                value: value.to_string(),
            });
        }
    } else if value.is_empty() {
        tag.remove(frame_id);
    } else {
        tag.set_text(frame_id, value);
    }
    tag.write_to_path(path, Version::Id3v23)
        .with_context(|| format!("Failed to write frame {} to {}", frame_id, path.display()))?;

    // Suppress the watcher: this is Sparkamp's own write, not an external change.
    crate::watch::register_self_write(path);

    Ok(())
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
    // TCOM, TOPE, TCOP, TENC, USLT and WXXX are excluded: they're managed
    // fields the main editor already owns (composer, original_artist,
    // copyright, encoded_by, lyric, url), so a future "add frame" UI must
    // not offer to add them a second time as extra frames.
    vec![
        ("TIT1", "Content Group"),
        ("TIT3", "Subtitle"),
        ("TOAL", "Original Album"),
        ("TSRC", "ISRC"),
        ("TPE3", "Conductor"),
        ("TPE4", "Interpreted By"),
        ("TEXT", "Lyricist"),
        ("TOLY", "Original Lyricist"),
        ("TMCL", "Musician Credits"),
        ("TIPL", "Involved People"),
        ("TLEN", "Length (ms)"),
        ("TKEY", "Initial Key"),
        ("TLAN", "Language"),
        ("TFLT", "File Type"),
        ("TMED", "Media Type"),
        ("TMOO", "Mood"),
        ("TPUB", "Publisher"),
        ("TOWN", "File Owner"),
        ("TRSN", "Radio Station Name"),
        ("TOFN", "Original Filename"),
        ("TSSE", "Software/Hardware"),
        ("TSOA", "Album Sort Order"),
        ("TSOP", "Artist Sort Order"),
        ("TSOT", "Title Sort Order"),
        ("TXXX", "User-Defined Text"),
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

    /// Every container Sparkamp lists, round-tripped through the editor.
    ///
    /// Needs real files, which is why it is ignored by default. Generate them
    /// with ffmpeg and point `SPARKAMP_FIXTURES` at the directory:
    ///
    /// ```text
    /// ffmpeg -f lavfi -i "sine=frequency=440:duration=2" -ac 2 src.wav
    /// ffmpeg -i src.wav -c:a flac song.flac      # and so on per format
    /// SPARKAMP_FIXTURES=/path cargo test --lib editor_round_trips -- --ignored
    /// ```
    ///
    /// What this pins down is the thing unit tests could not: that the value
    /// goes into the container's own tag rather than an ID3 header bolted to
    /// the front. Before the routing existed, only MP3 read its own tags back
    /// and eight of eleven formats were left with a foreign ID3v2 tag
    /// prepended, leaving a FLAC that no longer began with `fLaC`.
    #[test]
    #[ignore]
    fn editor_round_trips_every_container() {
        let Ok(dir) = std::env::var("SPARKAMP_FIXTURES") else {
            println!("set SPARKAMP_FIXTURES to a directory of song.<ext> files");
            return;
        };
        // Neither has a tag format lofty can write, so both must be refused
        // rather than damaged.
        const UNTAGGABLE: &[&str] = &["wma", "tta"];

        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .expect("fixtures directory")
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().starts_with("song."))
                    .unwrap_or(false)
            })
            .collect();
        files.sort();
        assert!(!files.is_empty(), "no song.<ext> fixtures in {dir}");

        for path in files {
            let ext = path.extension().unwrap().to_string_lossy().to_string();
            let magic_before = std::fs::read(&path).unwrap()[..4].to_vec();

            if UNTAGGABLE.contains(&ext.as_str()) {
                assert!(!is_taggable(&path), "{ext} should report as untaggable");
                let before = std::fs::read(&path).unwrap();
                assert!(
                    write_tag_fields(&path, &TagFields::default()).is_err(),
                    "{ext} must refuse a write rather than damage the file"
                );
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    before,
                    "{ext} was modified despite refusing the write"
                );
                continue;
            }

            assert!(is_taggable(&path), "{ext} should be taggable");

            let mut fields = read_tag_fields(&path);
            fields.title = format!("Title {ext}");
            fields.artist = format!("Artist {ext}");
            write_tag_fields(&path, &fields)
                .unwrap_or_else(|e| panic!("{ext}: writing fields failed: {e}"));

            let back = read_tag_fields(&path);
            assert_eq!(back.title, format!("Title {ext}"), "{ext} title");
            assert_eq!(back.artist, format!("Artist {ext}"), "{ext} artist");

            // An extra frame, through the same ID3 vocabulary the UI speaks.
            if supports_frame(&path, "TSRC") {
                write_extra_frame(&path, "TSRC", "ISRC-VALUE")
                    .unwrap_or_else(|e| panic!("{ext}: writing TSRC failed: {e}"));
                assert!(
                    read_extra_frames(&path)
                        .iter()
                        .any(|f| f.id == "TSRC" && f.value == "ISRC-VALUE"),
                    "{ext} did not read back the extra frame it just wrote"
                );
            }

            // ReplayGain reaches every container through the TXXX name, which
            // is how the manual gain edit writes it.
            assert!(
                supports_frame(&path, "TXXX:REPLAYGAIN_TRACK_GAIN"),
                "{ext} should have somewhere for ReplayGain"
            );

            // MP3 rewrites its ID3 header, so its first four bytes may
            // legitimately differ. Every other container must be structurally
            // untouched: a changed magic means a foreign tag was prepended.
            if ext != "mp3" && ext != "aac" {
                assert_eq!(
                    std::fs::read(&path).unwrap()[..4],
                    magic_before[..],
                    "{ext} lost its container magic, a foreign tag was prepended"
                );
            }
            println!("  {ext}: fields, extra frames and ReplayGain all round-trip");
        }
    }

    /// Every field is reachable by id, both to read and to write.
    ///
    /// The frontends each grew their own id-to-field match, which is how they
    /// drifted: the TUI indexed by position and the GTK editor by name. One
    /// accessor beside the field list is what stops a third from appearing.
    #[test]
    fn fields_round_trip_through_their_ids() {
        let mut f = TagFields::default();
        for (i, id) in TagFields::field_ids().into_iter().enumerate() {
            *f.value_mut(id).expect("every id has a field") = format!("v{i}");
        }
        for (i, id) in TagFields::field_ids().into_iter().enumerate() {
            assert_eq!(f.value(id), Some(format!("v{i}").as_str()), "{id}");
        }
        // The values landed on the fields the labels claim, not merely on
        // eighteen distinct strings.
        assert_eq!(f.title, "v0");
        assert_eq!(f.url, "v15");
        assert_eq!(f.lyric, "v17");
    }

    #[test]
    fn an_unknown_field_id_reaches_nothing() {
        let mut f = TagFields::default();
        assert!(f.value("not_a_field").is_none());
        assert!(f.value_mut("not_a_field").is_none());
    }

    /// `field_ids` and `field_pairs` describe the same rows in the same order.
    ///
    /// They are read together: a frontend zips the labels from one with the
    /// ids from the other to ask which rows the file can hold. Drift between
    /// them would silently mislabel every row after the point they diverge.
    #[test]
    fn field_ids_line_up_with_field_pairs() {
        let pairs = TagFields::default().field_pairs();
        let ids = TagFields::field_ids();
        assert_eq!(ids.len(), pairs.len(), "one id per rendered field");
        // Hand-checked anchors at both ends and either side of the middle.
        assert_eq!(ids[0], "title");
        assert_eq!(pairs[0].0, "Title");
        assert_eq!(ids[11], "comment");
        assert_eq!(pairs[11].0, "Comment");
        assert_eq!(ids[15], "url");
        assert_eq!(pairs[15].0, "URL");
        assert_eq!(ids[17], "lyric");
        assert_eq!(pairs[17].0, "Lyric");
    }

    /// Every id the editor renders resolves to a frame, so `supports_field`
    /// answers about the field rather than falling through its unknown arm and
    /// hiding a row that the container can hold perfectly well.
    #[test]
    fn every_editor_field_id_maps_to_a_frame() {
        for id in TagFields::field_ids() {
            assert!(
                frame_for_field(id).is_some(),
                "{id} has no frame, so it would be hidden everywhere"
            );
        }
    }

    /// Print which native tag key each editor field resolves to, per container.
    ///
    /// Reference rather than assertion: it pins nothing, it just shows the
    /// mapping the code actually computes, against the committed tones. Run it
    /// when the field set or the key candidates change, and when someone asks
    /// why a field is missing from one format.
    ///
    ///   cargo test --lib print_field_matrix -- --ignored --nocapture
    #[test]
    #[ignore]
    fn print_field_matrix() {
        use lofty::file::TaggedFileExt;
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let exts = [
            "mp3", "flac", "ogg", "opus", "m4a", "aac", "wav", "aiff", "tta", "wv", "wma",
        ];
        print!("{:<16}", "field");
        for e in exts {
            print!("{e:<14}");
        }
        println!();
        print!("{:<16}", "[tag format]");
        for e in exts {
            let p = dir.join(format!("tone.{e}"));
            // MP3 never reaches lofty: the `id3` crate writes it.
            let ty = if e == "mp3" {
                "ID3v2".to_string()
            } else {
                match lofty::probe::Probe::open(&p).and_then(|x| x.read()) {
                    Ok(t) => format!("{:?}", t.primary_tag_type()),
                    Err(_) => "none".to_string(),
                }
            };
            print!("{ty:<14}");
        }
        println!();
        for id in TagFields::field_ids() {
            print!("{id:<16}");
            for e in exts {
                let p = dir.join(format!("tone.{e}"));
                let cell = if e == "mp3" {
                    frame_for_field(id).unwrap_or("-").to_string()
                } else {
                    lofty_tag_type(&p)
                        .and_then(|kind| {
                            item_key_in(id, kind).and_then(|k| k.map_key(kind))
                        })
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "-".to_string())
                };
                print!("{cell:<14}");
            }
            println!();
        }
    }

    /// A FLAC header with a STREAMINFO block and no audio, which is enough
    /// for lofty to identify the container and answer for its tag type.
    fn minimal_flac(dir: &std::path::Path) -> std::path::PathBuf {
        let mut f = b"fLaC".to_vec();
        f.push(0x80); // last-metadata-block flag, type 0 (STREAMINFO)
        f.extend_from_slice(&[0, 0, 34]);
        f.extend_from_slice(&[0u8; 34]);
        let p = dir.join("probe.flac");
        std::fs::write(&p, f).unwrap();
        p
    }

    /// The editor asks per field so it can hide what the container cannot
    /// hold, rather than offering all of them and dropping most on save.
    ///
    /// URL is the field that proves it: it is ID3's `WXXX`, a user-defined
    /// link frame with no equivalent in a Vorbis comment, which is why
    /// `lofty_field_pairs` omits it.
    #[test]
    fn supports_field_hides_url_on_a_flac_and_keeps_it_on_an_mp3() {
        let dir = tempfile::tempdir().unwrap();
        let flac = minimal_flac(dir.path());
        assert!(supports_field(&flac, "title"), "a FLAC holds a title");
        assert!(supports_field(&flac, "comment"), "a FLAC holds a comment");
        assert!(!supports_field(&flac, "url"), "a FLAC has no WXXX");

        let mp3 = make_tagged_mp3("t", "a", "b");
        assert!(
            supports_field(mp3.path(), "url"),
            "an MP3 does have somewhere for a URL"
        );
    }

    /// An unknown field id is not silently treated as supported, which would
    /// make a typo in a caller show a field that cannot be saved.
    #[test]
    fn supports_field_refuses_a_field_it_does_not_know() {
        let dir = tempfile::tempdir().unwrap();
        let flac = minimal_flac(dir.path());
        assert!(!supports_field(&flac, "not_a_field"));
    }

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
            composer: String::new(),
            original_artist: String::new(),
            copyright: String::new(),
            url: String::new(),
            encoded_by: String::new(),
            lyric: String::new(),
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
        // TSRC (ISRC) isn't part of TagFields, so it must survive a write
        // that doesn't mention it — unlike TCOM et al., which Task 1 moved
        // into TagFields and are now managed (cleared when empty).
        tag.set_text("TSRC", "US-ABC-12-34567");
        tag.write_to_path(f.path(), Version::Id3v23).unwrap();

        // Write default fields (no TSRC).
        let fields = TagFields {
            title: "Updated".into(),
            ..Default::default()
        };
        write_tag_fields(f.path(), &fields).unwrap();

        // TSRC should still be present.
        let tag_after = Tag::read_from_path(f.path()).unwrap();
        let isrc = tag_after
            .get("TSRC")
            .and_then(|f| f.content().text())
            .unwrap_or("");
        assert_eq!(isrc, "US-ABC-12-34567");
        assert_eq!(tag_after.title().unwrap_or(""), "Updated");
    }

    #[test]
    fn write_tag_fields_registers_self_write() {
        let file = make_tagged_mp3("Old Title", "Old Artist", "Old Album");
        let fields = TagFields {
            title: "New Title".into(),
            ..Default::default()
        };

        write_tag_fields(file.path(), &fields).unwrap();

        assert!(crate::watch::is_path_suppressed(file.path()));
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

    #[test]
    fn extended_fields_roundtrip() {
        // The six fields the GTK editor used to drop (B1) must survive a
        // write/read cycle, including the two non-text frames (WXXX, USLT).
        let path = std::env::temp_dir().join("sparkamp_ext_fields_test.mp3");
        std::fs::write(&path, b"").unwrap();

        let fields = TagFields {
            title: "T".into(),
            composer: "A Composer".into(),
            original_artist: "Orig Artist".into(),
            copyright: "(c) 2026".into(),
            url: "https://example.com/a".into(),
            encoded_by: "Sparkamp".into(),
            lyric: "la la\nla".into(),
            ..TagFields::default()
        };
        write_tag_fields(&path, &fields).unwrap();

        let back = read_tag_fields(&path);
        assert_eq!(back.composer, "A Composer");
        assert_eq!(back.original_artist, "Orig Artist");
        assert_eq!(back.copyright, "(c) 2026");
        assert_eq!(back.url, "https://example.com/a");
        assert_eq!(back.encoded_by, "Sparkamp");
        assert_eq!(back.lyric, "la la\nla");

        // Clearing a field must remove its frame.
        let mut cleared = back.clone();
        cleared.lyric = String::new();
        cleared.url = String::new();
        write_tag_fields(&path, &cleared).unwrap();
        let back2 = read_tag_fields(&path);
        assert_eq!(back2.lyric, "");
        assert_eq!(back2.url, "");

        std::fs::remove_file(&path).ok();
    }

    // -----------------------------------------------------------------------
    // field_pairs
    // -----------------------------------------------------------------------

    #[test]
    fn field_pairs_returns_18_entries() {
        let fields = TagFields::default();
        assert_eq!(fields.field_pairs().len(), 18);
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
    // TXXX (user-defined text) extra frames — where REPLAYGAIN_* values live
    // -----------------------------------------------------------------------

    #[test]
    fn extra_frames_expose_txxx_by_description() {
        let f = make_tagged_mp3("T", "A", "Al");
        let path = f.path();

        // Write two TXXX frames the same way ReplayGain analysis does.
        let mut tag = Tag::read_from_path(path).unwrap();
        for (desc, value) in [
            ("REPLAYGAIN_TRACK_GAIN", "-11.00 dB"),
            ("REPLAYGAIN_TRACK_PEAK", "0.988123"),
        ] {
            tag.add_frame(id3::frame::ExtendedText {
                description: desc.to_string(),
                value: value.to_string(),
            });
        }
        tag.write_to_path(path, Version::Id3v23).unwrap();

        let extras = read_extra_frames(path);
        let gain = extras
            .iter()
            .find(|e| e.label == "REPLAYGAIN_TRACK_GAIN")
            .expect("TXXX frame surfaced by its description, not as a bare \"TXXX\" row");
        // The bug this guards: TXXX has no `Content::text()`, so both the
        // label and the value used to come back empty.
        assert_eq!(gain.value, "-11.00 dB");
        assert_eq!(gain.id, "TXXX:REPLAYGAIN_TRACK_GAIN");
        assert!(extras.iter().any(|e| e.label == "REPLAYGAIN_TRACK_PEAK"));
    }

    #[test]
    fn write_extra_frame_round_trips_txxx() {
        let f = make_tagged_mp3("T", "A", "Al");
        let path = f.path();

        write_extra_frame(path, "TXXX:REPLAYGAIN_TRACK_GAIN", "-6.20 dB").unwrap();
        let read_back = |p: &Path| {
            read_extra_frames(p)
                .into_iter()
                .find(|e| e.label == "REPLAYGAIN_TRACK_GAIN")
                .map(|e| e.value)
        };
        assert_eq!(read_back(path).as_deref(), Some("-6.20 dB"));

        // Rewriting replaces rather than stacking a duplicate description.
        write_extra_frame(path, "TXXX:REPLAYGAIN_TRACK_GAIN", "-3.10 dB").unwrap();
        let matches: Vec<_> = read_extra_frames(path)
            .into_iter()
            .filter(|e| e.label == "REPLAYGAIN_TRACK_GAIN")
            .collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].value, "-3.10 dB");

        // Empty value removes the frame.
        write_extra_frame(path, "TXXX:REPLAYGAIN_TRACK_GAIN", "").unwrap();
        assert_eq!(read_back(path), None);
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

    // -----------------------------------------------------------------------
    // artwork_mime_matches_extension
    // -----------------------------------------------------------------------

    #[test]
    fn artwork_mime_matches_extension() {
        // Embedding a .gif/.webp must not claim image/jpeg (B5) — players
        // decode by the declared mime and render garbage otherwise.
        let art = std::env::temp_dir().join("sparkamp_mime_test.GIF");
        std::fs::write(&art, b"GIF89a fake").unwrap();
        let song = std::env::temp_dir().join("sparkamp_mime_test.mp3");
        std::fs::write(&song, b"").unwrap();

        let fields = TagFields {
            artwork_path: art.to_string_lossy().into_owned(),
            ..TagFields::default()
        };
        write_tag_fields(&song, &fields).unwrap();

        let tag = id3::Tag::read_from_path(&song).unwrap();
        let pic = tag.pictures().next().unwrap();
        assert_eq!(pic.mime_type, "image/gif");

        std::fs::remove_file(&art).ok();
        std::fs::remove_file(&song).ok();
    }

    // -----------------------------------------------------------------------
    // APIC picture type
    // -----------------------------------------------------------------------

    #[test]
    fn embedded_artwork_is_cover_front() {
        // Players/OSes pick the "front cover" APIC frame for thumbnails;
        // tagging it as anything else (or leaving PictureType::Other) makes
        // some players ignore it. Pin the type write_tag_fields uses.
        let art = std::env::temp_dir().join("sparkamp_cover_type_test.jpg");
        std::fs::write(&art, b"fake jpeg bytes").unwrap();
        let song = std::env::temp_dir().join("sparkamp_cover_type_test.mp3");
        std::fs::write(&song, b"").unwrap();

        let fields = TagFields {
            artwork_path: art.to_string_lossy().into_owned(),
            ..TagFields::default()
        };
        write_tag_fields(&song, &fields).unwrap();

        let tag = id3::Tag::read_from_path(&song).unwrap();
        let pic = tag.pictures().next().unwrap();
        assert_eq!(pic.picture_type, id3::frame::PictureType::CoverFront);

        std::fs::remove_file(&art).ok();
        std::fs::remove_file(&song).ok();
    }
}
