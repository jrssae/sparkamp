//! Shared audio-tag reading.
//!
//! One ID3-first / Symphonia-fallback reader serving both the playlist model
//! (which needs only title/artist/album-artist/album) and the media library
//! (which stores the full tag set).  Keeping a single Symphonia probe here
//! prevents the two callers drifting apart in how they parse tags.

use std::path::Path;

use crate::textutil::sanitize;

/// Raw tag data extracted from an audio file.
#[derive(Default)]
pub(crate) struct TrackTags {
    pub(crate) title: Option<String>,
    pub(crate) artist: Option<String>,
    pub(crate) album: Option<String>,
    pub(crate) track_num: Option<i64>,
    pub(crate) genre: Option<String>,
    pub(crate) year: Option<i64>,
    pub(crate) bpm: Option<String>,
    pub(crate) bitrate: Option<i64>,
    pub(crate) channels: Option<i64>,
    pub(crate) comment: Option<String>,
    pub(crate) album_artist: Option<String>,
    pub(crate) disc_num: Option<i64>,
    pub(crate) disc_total: Option<i64>,
    pub(crate) composer: Option<String>,
    pub(crate) original_artist: Option<String>,
    pub(crate) copyright: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) encoded_by: Option<String>,
    pub(crate) lyric: Option<String>,
    pub(crate) artwork_path: Option<String>,
}

/// Read metadata from an audio file.
///
/// Tries ID3 tags first (works well for MP3), then falls back to Symphonia's
/// generic reader (Vorbis Comments for OGG/FLAC/Opus, etc.).  Returns a
/// best-effort [`TrackTags`] even when no tags are present.
///
/// Side effect: when the ID3 tag embeds album art (APIC), the image is
/// written to the Sparkamp cache directory and `artwork_path` points at it.
pub(crate) fn read_track_tags(path: &Path) -> TrackTags {
    use id3::TagLike;

    // Strategy 1: ID3 (MP3 and some other formats).
    if let Ok(tag) = id3::Tag::read_from_path(path) {
        let get_text = |frame_id: &str| -> Option<String> {
            tag.get(frame_id)
                .and_then(|f| f.content().text())
                .map(|s| sanitize(&s))
        };
        // Prefer the empty-description COMM frame — that's the "main" user
        // comment our editor reads and writes. Files often also carry tool /
        // release COMM frames with a non-empty description (e.g. "PMEDIA
        // NETWORK"); picking the first frame regardless would surface those
        // instead of the value shown and edited in the UI.
        let get_first_comment = || -> Option<String> {
            let comments: Vec<&id3::frame::Comment> = tag.comments().collect();
            comments
                .iter()
                .find(|c| c.description.is_empty())
                .or_else(|| comments.first())
                .map(|c| sanitize(&c.text))
        };
        let disc = tag.disc();
        let (disc_num, disc_total) = if let Some(d) = disc {
            (Some(d as i64), tag.total_discs().map(|t| t as i64))
        } else {
            (None, None)
        };
        let lyric_text = tag.lyrics().next().map(|l| sanitize(&l.text));

        // Look for APIC (album art) and save it to the cache dir.
        let artwork_path = tag.pictures().next().map(|pic| {
            let cache_dir = dirs::cache_dir()
                .unwrap_or_else(|| std::env::temp_dir())
                .join("sparkamp");
            let _ = std::fs::create_dir_all(&cache_dir);
            // Use a hash of the path as the filename to avoid collisions.
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            path.hash(&mut h);
            let hash = h.finish();
            let ext = match pic.mime_type.as_str() {
                "image/png" => "png",
                "image/jpeg" | "image/jpg" => "jpg",
                _ => "bin",
            };
            let art_path = cache_dir.join(format!("{:016x}.{}", hash, ext));
            if !art_path.exists() {
                let _ = std::fs::write(&art_path, &pic.data);
            }
            art_path.to_string_lossy().into_owned()
        });

        TrackTags {
            title: tag.title().map(|s| sanitize(&s)),
            artist: tag.artist().map(|s| sanitize(&s)),
            album: tag.album().map(|s| sanitize(&s)),
            track_num: tag.track().map(|n| n as i64),
            genre: tag.genre().map(|s| sanitize(&s)),
            year: tag.year().map(|y| y as i64),
            bpm: get_text("TBPM"),
            bitrate: None,
            channels: None,
            comment: get_first_comment(),
            album_artist: tag.album_artist().map(|s| sanitize(&s)),
            disc_num,
            disc_total,
            composer: get_text("TCOM"),
            original_artist: get_text("TOPE"),
            copyright: get_text("TCOP"),
            url: get_text("WXXX"),
            encoded_by: get_text("TENC"),
            lyric: lyric_text,
            artwork_path,
        }
    } else {
        // Strategy 2: Symphonia generic (Vorbis Comments, FLAC, Opus, etc.).
        if let Some(meta) = read_symphonia_tags(path) {
            return meta;
        }
        // Fallback: no tags at all.
        TrackTags::default()
    }
}

/// Read metadata using Symphonia's generic reader.
///
/// Handles formats that don't use ID3 tags: OGG/Vorbis, FLAC, Opus.
/// Returns `None` when the file cannot be opened or the format is unrecognised.
/// No side effects (does not extract artwork).
pub(crate) fn read_symphonia_tags(path: &Path) -> Option<TrackTags> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::{MetadataOptions, StandardTagKey, Value};
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;

    let mut title: Option<String> = None;
    let mut artist: Option<String> = None;
    let mut album: Option<String> = None;
    let mut album_artist: Option<String> = None;
    let mut track_num: Option<i64> = None;
    let mut genre: Option<String> = None;
    let mut year: Option<i64> = None;

    // Read from the format reader's own metadata (Vorbis Comments, etc.).
    if let Some(rev) = probed.format.metadata().current() {
        for tag in rev.tags() {
            let text = match &tag.value {
                Value::String(s) => s.clone(),
                _ => continue,
            };
            // Sanitize to remove NUL bytes that can crash GTK.
            let safe_text = sanitize(&text);
            match tag.std_key {
                Some(StandardTagKey::TrackTitle) => title = Some(safe_text),
                Some(StandardTagKey::Artist) => artist = Some(safe_text),
                Some(StandardTagKey::Album) => album = Some(safe_text),
                Some(StandardTagKey::AlbumArtist) => album_artist = Some(safe_text),
                Some(StandardTagKey::TrackNumber) => {
                    // Track number may be "5" or "5/12" — parse the first part.
                    track_num = safe_text
                        .split('/')
                        .next()
                        .and_then(|n| n.trim().parse::<i64>().ok());
                }
                Some(StandardTagKey::Genre) => genre = Some(safe_text),
                Some(StandardTagKey::Date) => {
                    // Date can be "2003", "2003-04-15", etc. — take the year.
                    year = safe_text
                        .split('-')
                        .next()
                        .and_then(|y| y.trim().parse::<i64>().ok());
                }
                _ => {}
            }
        }
    }

    // Collect channel count from codec parameters.
    let channels = probed
        .format
        .tracks()
        .first()
        .and_then(|t| t.codec_params.channels)
        .map(|c| c.count() as i64);

    Some(TrackTags {
        title,
        artist,
        album,
        track_num,
        genre,
        year,
        bpm: None,
        bitrate: None,
        channels,
        comment: None,
        album_artist,
        disc_num: None,
        disc_total: None,
        composer: None,
        original_artist: None,
        copyright: None,
        url: None,
        encoded_by: None,
        lyric: None,
        artwork_path: None,
    })
}

/// Title / artist / album-artist / album via Symphonia, for the playlist
/// model's lightweight path.  Returns `None` when the file cannot be probed
/// or carries no (non-empty) title — same contract the model has always had.
pub(crate) fn read_symphonia_basic(path: &Path) -> Option<(String, String, String, String)> {
    let t = read_symphonia_tags(path)?;
    let title = t.title.unwrap_or_default();
    if title.is_empty() {
        return None;
    }
    Some((
        title,
        t.artist.unwrap_or_default(),
        t.album_artist.unwrap_or_default(),
        t.album.unwrap_or_default(),
    ))
}
