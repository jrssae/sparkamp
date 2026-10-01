//! A song's star rating (0 to 5) in its own file's tags.
//!
//! The file is where a rating lives. Changing one writes the file first, then
//! the library, then any server; if the file cannot take it, nothing changes.
//!
//! - MP3: an ID3 `POPM` frame owned by "Sparkamp" (the same frame device
//!   sync writes), 0–255 on the Windows Media Player scale. Reading falls
//!   back to a frame of any owner.
//! - FLAC, Ogg Vorbis, Opus: the `FMPS_RATING` Vorbis comment, 0.0–1.0, as
//!   Strawberry, Clementine, Amarok and Quod Libet use it.
//! - Anything else has no rating tag Sparkamp writes.

use std::path::Path;

/// The owner name on the POPM frame Sparkamp writes.
pub const POPM_OWNER: &str = "Sparkamp";

/// The Vorbis comment holding the rating.
pub const FMPS_KEY: &str = "FMPS_RATING";

/// Why a rating could not be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RatingError {
    /// The format has no rating tag Sparkamp writes (WAV, M4A, …).
    Unsupported(String),
    /// Reading or writing the file failed (read-only, gone, damaged).
    File(String),
}

impl std::fmt::Display for RatingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RatingError::Unsupported(ext) => {
                write!(f, "{ext} files have no rating tag Sparkamp can write")
            }
            RatingError::File(why) => write!(f, "could not write the rating: {why}"),
        }
    }
}

impl std::error::Error for RatingError {}

/// The rating stored in `path`'s tags, if any.
pub fn read_rating(path: &Path) -> Option<u8> {
    match extension(path).as_str() {
        "mp3" => read_popm(path),
        "flac" => {
            let tag = metaflac::Tag::read_from_path(path).ok()?;
            let value = tag.get_vorbis(FMPS_KEY)?.next()?.to_string();
            fmps_to_stars(&value)
        }
        "ogg" | "oga" | "opus" => read_ogg(path).and_then(|v| fmps_to_stars(&v)),
        _ => None,
    }
}

/// Store `stars` (0 to 5; 0 removes the rating) in `path`'s tags.
pub fn write_rating(path: &Path, stars: u8) -> Result<(), RatingError> {
    let stars = stars.min(5);
    let file = |e: &dyn std::fmt::Display| RatingError::File(e.to_string());
    // A read-only file would be refused by the writer anyway, but some
    // writers create a temporary file next to it first; ask up front.
    if std::fs::metadata(path).map_err(|e| file(&e))?.permissions().readonly() {
        return Err(RatingError::File(format!("{} is read-only", path.display())));
    }
    match extension(path).as_str() {
        "mp3" => write_popm(path, stars).map_err(|e| file(&e)),
        "flac" => {
            let mut tag = metaflac::Tag::read_from_path(path).map_err(|e| file(&e))?;
            tag.remove_vorbis(FMPS_KEY);
            if stars > 0 {
                tag.set_vorbis(FMPS_KEY, vec![stars_to_fmps(stars)]);
            }
            tag.save().map_err(|e| file(&e))
        }
        "ogg" | "oga" | "opus" => write_ogg(path, stars).map_err(|e| file(&e)),
        other => Err(RatingError::Unsupported(if other.is_empty() {
            "Extensionless".into()
        } else {
            other.to_uppercase()
        })),
    }
}

fn extension(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// 0.0–1.0 to stars, nearest star.
pub(crate) fn fmps_to_stars(value: &str) -> Option<u8> {
    let v: f64 = value.trim().parse().ok()?;
    let stars = (v.clamp(0.0, 1.0) * 5.0).round() as u8;
    (stars > 0).then_some(stars)
}

fn stars_to_fmps(stars: u8) -> String {
    format!("{:.1}", stars as f64 / 5.0)
}

fn read_popm(path: &Path) -> Option<u8> {
    popm_stars(&id3::Tag::read_from_path(path).ok()?)
}

/// The rating in an already-read ID3 tag: Sparkamp's POPM frame, else any.
pub(crate) fn popm_stars(tag: &id3::Tag) -> Option<u8> {
    let frames: Vec<id3::frame::Popularimeter> =
        tag.frames().filter_map(|f| f.content().popularimeter()).cloned().collect();
    let chosen = frames.iter().find(|p| p.user == POPM_OWNER).or(frames.first())?;
    let stars = crate::devices::sync::popm_to_stars(chosen.rating);
    (stars > 0).then_some(stars)
}

fn write_popm(path: &Path, stars: u8) -> id3::Result<()> {
    use id3::TagLike;
    let mut tag = id3::Tag::read_from_path(path).unwrap_or_default();
    // Keep the play counter on our frame; leave other players' frames alone.
    let counter = tag
        .frames()
        .filter_map(|f| f.content().popularimeter())
        .find(|p| p.user == POPM_OWNER)
        .map(|p| p.counter)
        .unwrap_or(0);
    let others: Vec<id3::frame::Popularimeter> = tag
        .frames()
        .filter_map(|f| f.content().popularimeter())
        .filter(|p| p.user != POPM_OWNER)
        .cloned()
        .collect();
    tag.remove("POPM");
    for p in others {
        tag.add_frame(p);
    }
    if stars > 0 || counter > 0 {
        tag.add_frame(id3::frame::Popularimeter {
            user: POPM_OWNER.to_string(),
            rating: if stars > 0 { crate::devices::sync::stars_to_popm(stars) } else { 0 },
            counter,
        });
    }
    tag.write_to_path(path, id3::Version::Id3v24)
}

fn read_ogg(path: &Path) -> Option<String> {
    use lofty::config::ParseOptions;
    use lofty::file::AudioFile;
    let mut f = std::fs::File::open(path).ok()?;
    let comments = if extension(path) == "opus" {
        lofty::ogg::OpusFile::read_from(&mut f, ParseOptions::new()).ok()?.vorbis_comments().clone()
    } else {
        lofty::ogg::VorbisFile::read_from(&mut f, ParseOptions::new()).ok()?.vorbis_comments().clone()
    };
    comments.get(FMPS_KEY).map(str::to_string)
}

fn write_ogg(path: &Path, stars: u8) -> Result<(), String> {
    use lofty::config::{ParseOptions, WriteOptions};
    use lofty::file::AudioFile;
    use lofty::tag::TagExt;
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    // Start from the comments already there: saving writes the whole set.
    let mut comments = if extension(path) == "opus" {
        lofty::ogg::OpusFile::read_from(&mut f, ParseOptions::new())
            .map_err(|e| e.to_string())?
            .vorbis_comments()
            .clone()
    } else {
        lofty::ogg::VorbisFile::read_from(&mut f, ParseOptions::new())
            .map_err(|e| e.to_string())?
            .vorbis_comments()
            .clone()
    };
    drop(f);
    let _ = comments.remove(FMPS_KEY).count();
    if stars > 0 {
        comments.insert(FMPS_KEY.to_string(), stars_to_fmps(stars));
    }
    comments.save_to_path(path, WriteOptions::default()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use id3::TagLike;

    fn temp(name: &str, bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        (dir, path)
    }

    /// A FLAC carrying nothing but its STREAMINFO block.
    fn minimal_flac() -> Vec<u8> {
        let mut f = b"fLaC".to_vec();
        f.push(0x80);
        f.extend_from_slice(&[0, 0, 34]);
        f.extend_from_slice(&[0u8; 34]);
        f
    }

    fn popm_frames(path: &Path) -> Vec<(String, u8, u64)> {
        let tag = id3::Tag::read_from_path(path).unwrap();
        tag.frames()
            .filter_map(|f| f.content().popularimeter())
            .map(|p| (p.user.clone(), p.rating, p.counter))
            .collect()
    }

    #[test]
    fn an_mp3_rating_goes_in_sparkamps_popm_frame() {
        let (_d, path) = temp("song.mp3", b"not really audio");
        write_rating(&path, 4).unwrap();
        assert_eq!(popm_frames(&path), vec![("Sparkamp".to_string(), 196, 0)]);
        assert_eq!(read_rating(&path), Some(4));
    }

    #[test]
    fn an_mp3_rating_keeps_the_play_counter_and_other_owners_frames() {
        let (_d, path) = temp("song.mp3", b"not really audio");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::frame::Popularimeter { user: "Sparkamp".into(), rating: 64, counter: 12 });
        tag.add_frame(id3::frame::Popularimeter {
            user: "Windows Media Player 9 Series".into(),
            rating: 255,
            counter: 0,
        });
        tag.write_to_path(&path, id3::Version::Id3v24).unwrap();

        write_rating(&path, 3).unwrap();

        let mut frames = popm_frames(&path);
        frames.sort();
        assert_eq!(
            frames,
            vec![
                ("Sparkamp".to_string(), 128, 12),
                ("Windows Media Player 9 Series".to_string(), 255, 0),
            ]
        );
        assert_eq!(read_rating(&path), Some(3), "Sparkamp's own frame wins when reading");
    }

    #[test]
    fn an_mp3_rated_by_another_player_reads_its_rating() {
        let (_d, path) = temp("song.mp3", b"not really audio");
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::frame::Popularimeter {
            user: "Windows Media Player 9 Series".into(),
            rating: 255,
            counter: 0,
        });
        tag.write_to_path(&path, id3::Version::Id3v24).unwrap();
        assert_eq!(read_rating(&path), Some(5));
    }

    #[test]
    fn zero_stars_removes_sparkamps_mp3_rating() {
        let (_d, path) = temp("song.mp3", b"not really audio");
        write_rating(&path, 5).unwrap();
        write_rating(&path, 0).unwrap();
        assert_eq!(read_rating(&path), None);
    }

    #[test]
    fn a_flac_rating_is_an_fmps_rating_comment() {
        let (_d, path) = temp("song.flac", &minimal_flac());
        write_rating(&path, 3).unwrap();
        // Read back through a different library than the one that wrote it.
        let tag = metaflac::Tag::read_from_path(&path).unwrap();
        let values: Vec<&str> = tag.get_vorbis(FMPS_KEY).map(|v| v.collect()).unwrap_or_default();
        assert_eq!(values, vec!["0.6"]);
        assert_eq!(read_rating(&path), Some(3));

        write_rating(&path, 0).unwrap();
        assert_eq!(read_rating(&path), None);
    }

    #[test]
    fn fmps_values_round_to_the_nearest_star() {
        let (_d, path) = temp("song.flac", &minimal_flac());
        let mut tag = metaflac::Tag::read_from_path(&path).unwrap();
        tag.set_vorbis(FMPS_KEY, vec!["0.72"]);
        tag.save().unwrap();
        assert_eq!(read_rating(&path), Some(4));
    }

    #[test]
    fn a_format_without_a_rating_tag_is_refused_and_left_untouched() {
        let (_d, path) = temp("song.wav", b"RIFF....WAVE");
        let before = std::fs::read(&path).unwrap();
        assert!(matches!(write_rating(&path, 4), Err(RatingError::Unsupported(_))));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let (_d2, m4a) = temp("song.m4a", b"....ftyp");
        assert!(matches!(write_rating(&m4a, 4), Err(RatingError::Unsupported(_))));
    }

    #[test]
    fn a_read_only_file_is_refused() {
        let (_d, path) = temp("song.mp3", b"not really audio");
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();
        assert!(matches!(write_rating(&path, 4), Err(RatingError::File(_))));
        assert_eq!(read_rating(&path), None);
    }
}
