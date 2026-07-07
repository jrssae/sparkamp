//! Rip audio-CD tracks to tagged MP3s.
//!
//! One GStreamer pipeline per track: the source differs by platform (macOS
//! decodes the auto-mounted AIFF file; Linux reads the drive directly via
//! `cdiocddasrc`), the tail is shared — `audioconvert ! lamemp3enc !
//! filesink`. Tags are written AFTER encoding with
//! [`crate::id3_editor::write_tag_fields`], so one code path owns tagging
//! (no `id3v2mux` in the pipeline).
//!
//! Everything here is synchronous and per-track: the frontends loop on a
//! background thread/queue, publish per-track progress, and check a cancel
//! flag between tracks (cancel stops after the current track).

use std::path::{Path, PathBuf};

use crate::id3_editor::TagFields;

/// Where one track's audio comes from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RipSource {
    /// macOS: the mounted AIFF file for the track.
    File { path: PathBuf },
    /// Linux: raw CD audio from the drive.
    Cdda { device: String, track: u8 },
}

/// MP3 encoding preset (mirrors `DiscConfig::rip_mp3_quality`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp3Quality {
    /// VBR V0, ~245 kbps.
    VbrV0,
    /// VBR V2, ~190 kbps — the default.
    VbrV2,
    /// 320 kbps CBR.
    Cbr320,
}

impl Mp3Quality {
    /// From the config's preset id (unknown values fall back to V2).
    pub fn from_config(v: u8) -> Self {
        match v {
            0 => Mp3Quality::VbrV0,
            2 => Mp3Quality::Cbr320,
            _ => Mp3Quality::VbrV2,
        }
    }

    /// The `lamemp3enc` property string for this preset.
    fn encoder_props(self) -> &'static str {
        match self {
            Mp3Quality::VbrV0 => "target=quality quality=0",
            Mp3Quality::VbrV2 => "target=quality quality=2",
            Mp3Quality::Cbr320 => "target=bitrate bitrate=320 cbr=true",
        }
    }
}

/// Strip path-hostile characters from a tag value used as a file/dir name,
/// falling back when nothing usable remains (same rules as device playlist
/// filenames).
pub fn safe_component(name: &str, fallback: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    let safe = safe.trim().trim_matches('.').trim();
    if safe.is_empty() {
        fallback.to_string()
    } else {
        safe.to_string()
    }
}

/// Destination file for one ripped track:
/// `<dest_root>/Artist/Album/NN - Title.mp3`, all components sanitized.
/// Empty artist/album become "Unknown Artist"/"Unknown Album"; an empty
/// title becomes "Track NN".
pub fn dest_path(dest_root: &Path, artist: &str, album: &str, number: u8, title: &str) -> PathBuf {
    let artist = safe_component(artist, "Unknown Artist");
    let album = safe_component(album, "Unknown Album");
    let title = safe_component(title, &format!("Track {number:02}"));
    dest_root
        .join(artist)
        .join(album)
        .join(format!("{number:02} - {title}.mp3"))
}

/// The `gst-launch`-style pipeline description for one track. Split out from
/// execution so the string form is unit-testable everywhere (running it needs
/// a drive/file + the LAME plugin).
pub fn pipeline_desc(source: &RipSource, quality: Mp3Quality, out: &Path) -> String {
    let src = match source {
        RipSource::File { path } => format!(
            "filesrc location=\"{}\" ! decodebin",
            path.display().to_string().replace('"', "\\\"")
        ),
        RipSource::Cdda { device, track } => {
            format!("cdiocddasrc track={track} device=\"{device}\"")
        }
    };
    format!(
        "{src} ! audioconvert ! lamemp3enc {} ! filesink location=\"{}\"",
        quality.encoder_props(),
        out.display().to_string().replace('"', "\\\"")
    )
}

/// Rip one track: run the pipeline to EOS (blocking — call on a worker
/// thread), then write the tags onto the fresh MP3. Creates the destination
/// directories. On any error the partial output file is removed.
pub fn rip_track(
    source: &RipSource,
    out: &Path,
    quality: Mp3Quality,
    tags: &TagFields,
) -> Result<(), String> {
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }

    let desc = pipeline_desc(source, quality, out);
    run_pipeline_to_eos(&desc).inspect_err(|_| {
        let _ = std::fs::remove_file(out);
    })?;

    crate::id3_editor::write_tag_fields(out, tags).map_err(|e| format!("tag write: {e}"))?;
    Ok(())
}

/// Build, play, and drain a pipeline until EOS or error. GStreamer must
/// already be initialized (both frontends do it at startup).
fn run_pipeline_to_eos(desc: &str) -> Result<(), String> {
    use gstreamer as gst;
    use gstreamer::prelude::*;

    let pipeline = gst::parse::launch(desc).map_err(|e| format!("pipeline: {e}"))?;
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| format!("start: {e}"))?;

    let bus = pipeline.bus().ok_or("pipeline has no bus")?;
    // Watchdog on PIPELINE POSITION, not bus traffic: a healthy encode posts
    // no bus messages at all between the start and EOS (a slow optical read
    // can run minutes in silence), so only a position that stops advancing
    // means a wedged drive.
    let mut last_pos: Option<gst::ClockTime> = None;
    let mut last_advance = std::time::Instant::now();
    let result = loop {
        match bus.timed_pop(gst::ClockTime::from_seconds(2)) {
            Some(msg) => match msg.view() {
                gst::MessageView::Eos(_) => break Ok(()),
                gst::MessageView::Error(e) => {
                    break Err(format!(
                        "{} ({})",
                        e.error(),
                        e.debug().unwrap_or_default()
                    ));
                }
                _ => {}
            },
            None => {
                let pos = pipeline.query_position::<gst::ClockTime>();
                if pos != last_pos {
                    last_pos = pos;
                    last_advance = std::time::Instant::now();
                } else if last_advance.elapsed() > std::time::Duration::from_secs(30) {
                    break Err("stalled: no read progress for 30 s".to_string());
                }
            }
        }
    };

    let _ = pipeline.set_state(gst::State::Null);
    result
}

/// The [`TagFields`] for one ripped track, from the disc's tag set. The
/// sampler convention ("Artist / Title" inside a track title) yields a
/// per-track artist, with the disc artist as `album_artist` in that case.
pub fn tag_fields_for_track(
    disc_artist: &str,
    album: &str,
    year: &str,
    genre: &str,
    number: u8,
    total: u8,
    raw_title: &str,
) -> TagFields {
    let (artist, title, album_artist) = match raw_title.split_once(" / ") {
        Some((a, t)) => (a.to_string(), t.to_string(), disc_artist.to_string()),
        None => (disc_artist.to_string(), raw_title.to_string(), String::new()),
    };
    TagFields {
        title,
        artist,
        album: album.to_string(),
        album_artist,
        genre: genre.to_string(),
        year: year.to_string(),
        track_number: number.to_string(),
        track_total: total.to_string(),
        disc_number: String::new(),
        disc_total: String::new(),
        bpm: String::new(),
        comment: String::new(),
        artwork_path: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dest_path_sanitizes_and_falls_back() {
        let p = dest_path(Path::new("/music"), "AC/DC", "Back: In Black?", 3, "Hells Bells");
        assert_eq!(
            p,
            Path::new("/music/AC_DC/Back_ In Black_/03 - Hells Bells.mp3")
        );
        let p = dest_path(Path::new("/m"), "", "", 12, "");
        assert_eq!(
            p,
            Path::new("/m/Unknown Artist/Unknown Album/12 - Track 12.mp3")
        );
    }

    #[test]
    fn pipeline_desc_per_source_and_quality() {
        let out = Path::new("/tmp/out.mp3");
        let mac = pipeline_desc(
            &RipSource::File {
                path: PathBuf::from("/Volumes/Audio CD/1 Audio Track.aiff"),
            },
            Mp3Quality::VbrV2,
            out,
        );
        assert!(mac.starts_with(
            "filesrc location=\"/Volumes/Audio CD/1 Audio Track.aiff\" ! decodebin"
        ));
        assert!(mac.contains("lamemp3enc target=quality quality=2"));
        assert!(mac.ends_with("filesink location=\"/tmp/out.mp3\""));

        let linux = pipeline_desc(
            &RipSource::Cdda {
                device: "/dev/sr0".into(),
                track: 4,
            },
            Mp3Quality::Cbr320,
            out,
        );
        assert!(linux.starts_with("cdiocddasrc track=4 device=\"/dev/sr0\""));
        assert!(linux.contains("target=bitrate bitrate=320 cbr=true"));
    }

    #[test]
    fn quality_mapping_from_config() {
        assert_eq!(Mp3Quality::from_config(0), Mp3Quality::VbrV0);
        assert_eq!(Mp3Quality::from_config(1), Mp3Quality::VbrV2);
        assert_eq!(Mp3Quality::from_config(2), Mp3Quality::Cbr320);
        assert_eq!(Mp3Quality::from_config(99), Mp3Quality::VbrV2);
    }

    #[test]
    fn tag_fields_handle_sampler_titles() {
        let plain = tag_fields_for_track("Band", "Album", "2001", "Rock", 3, 8, "Song");
        assert_eq!(plain.artist, "Band");
        assert_eq!(plain.title, "Song");
        assert!(plain.album_artist.is_empty());
        assert_eq!(plain.track_number, "3");
        assert_eq!(plain.track_total, "8");

        let split = tag_fields_for_track("Various", "Comp", "", "", 1, 12, "Guest / Tune");
        assert_eq!(split.artist, "Guest");
        assert_eq!(split.title, "Tune");
        assert_eq!(split.album_artist, "Various");
    }

    /// Live end-to-end rip of track 1 from the real mounted audio CD — run
    /// with `cargo test --lib live_rip -- --ignored --nocapture`. Uses the
    /// real detector, so any disc/volume name works.
    #[test]
    #[ignore]
    fn live_rip_first_track() {
        gstreamer::init().expect("gst init");
        let drives = crate::disc::detect::list_drives();
        let Some(entry) = drives
            .iter()
            .find(|d| d.media.is_audio_cd)
            .map(crate::disc::toc::track_entries)
            .and_then(|entries| entries.into_iter().next())
        else {
            println!("no audio CD mounted — skipping");
            return;
        };
        let aiff = PathBuf::from(&entry.path);
        let dir = std::env::temp_dir().join(format!("sparkamp-rip-{}", std::process::id()));
        let tags = tag_fields_for_track("Live Artist", "Live Album", "2026", "Rock", 1, 8, "Live Test");
        let out = dest_path(&dir, "Live Artist", "Live Album", 1, "Live Test");
        let started = std::time::Instant::now();
        rip_track(
            &RipSource::File { path: aiff },
            &out,
            Mp3Quality::VbrV2,
            &tags,
        )
        .expect("rip");
        let size = std::fs::metadata(&out).expect("output").len();
        println!(
            "ripped to {} — {} bytes in {:.1?}",
            out.display(),
            size,
            started.elapsed()
        );
        assert!(size > 100_000, "suspiciously small MP3");
        let tag = id3::Tag::read_from_path(&out).expect("id3 tag");
        use id3::TagLike;
        assert_eq!(tag.title(), Some("Live Test"));
        assert_eq!(tag.artist(), Some("Live Artist"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
