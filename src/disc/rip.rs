//! Rip audio-CD tracks to tagged MP3s.
//!
//! One GStreamer pipeline per track: the source differs by platform (macOS
//! decodes the auto-mounted AIFF file; Linux reads the drive directly via
//! `cdiocddasrc`), the tail is shared — `audioconvert ! lamemp3enc !
//! filesink`. Tags are written AFTER encoding with
//! [`crate::id3_editor::write_tag_fields`], so one code path owns tagging
//! (no `id3v2mux` in the pipeline).
//!
//! Everything here is synchronous: [`run_job`] rips a whole selection on the
//! caller's (worker) thread, publishing per-track progress through a callback
//! and checking a cancel flag between tracks (cancel stops after the current
//! track). The GTK and TUI frontends call it directly; the FFI exposes the
//! per-track [`rip_track`] for the Swift loop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

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
#[allow(dead_code)] // the frontends go through run_job; the FFI (lib only) rips per track
pub fn rip_track(
    source: &RipSource,
    out: &Path,
    quality: Mp3Quality,
    tags: &TagFields,
) -> Result<(), String> {
    rip_track_observed(source, out, quality, tags, |_| {})
}

/// [`rip_track`], reporting the pipeline position (seconds into the track)
/// as the encode advances — the within-track progress feed for [`run_job`].
pub fn rip_track_observed(
    source: &RipSource,
    out: &Path,
    quality: Mp3Quality,
    tags: &TagFields,
    on_position: impl FnMut(f64),
) -> Result<(), String> {
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }

    let desc = pipeline_desc(source, quality, out);
    run_pipeline_observed(&desc, on_position).inspect_err(|_| {
        let _ = std::fs::remove_file(out);
    })?;

    crate::id3_editor::write_tag_fields(out, tags).map_err(|e| format!("tag write: {e}"))?;
    Ok(())
}

/// Build, play, and drain a pipeline until EOS or error. GStreamer must
/// already be initialized (both frontends do it at startup). Shared with the
/// burn module's Red Book WAV preparation.
pub(crate) fn run_pipeline_to_eos(desc: &str) -> Result<(), String> {
    run_pipeline_observed(desc, |_| {})
}

/// [`run_pipeline_to_eos`] with a position feed: `on_position` gets the
/// pipeline position in seconds roughly twice a second while the pipeline
/// runs (nothing on the EOS/error path).
pub(crate) fn run_pipeline_observed(
    desc: &str,
    mut on_position: impl FnMut(f64),
) -> Result<(), String> {
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
    // means a wedged drive. The 500 ms pop timeout doubles as the position
    // sampling cadence for `on_position`.
    let mut last_pos: Option<gst::ClockTime> = None;
    let mut last_advance = std::time::Instant::now();
    let result = loop {
        match bus.timed_pop(gst::ClockTime::from_mseconds(500)) {
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
                if let Some(p) = pos {
                    on_position(p.seconds_f64());
                }
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
/// per-track artist, with the disc artist as `album_artist` in that case
/// (one shared rule: [`crate::disc::track_meta`]).
pub fn tag_fields_for_track(
    disc_artist: &str,
    album: &str,
    year: &str,
    genre: &str,
    number: u8,
    total: u8,
    raw_title: &str,
) -> TagFields {
    let meta = crate::disc::track_meta(raw_title, disc_artist);
    TagFields {
        title: meta.title,
        artist: meta.artist,
        album: album.to_string(),
        album_artist: meta.album_artist,
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

/// Where a disc entry's audio comes from: `cdda://N?device=…` pseudo-URIs
/// (Linux) become a [`RipSource::Cdda`]; anything else is a plain file path
/// (macOS's mounted AIFF).
pub fn source_for_entry(entry: &crate::disc::DiscTrackEntry) -> RipSource {
    match crate::disc::parse_cdda_uri(&entry.path) {
        Some((track, device)) => RipSource::Cdda {
            device: device.unwrap_or_default().to_string(),
            track: track.parse().unwrap_or(entry.number),
        },
        None => RipSource::File {
            path: PathBuf::from(&entry.path),
        },
    }
}

/// What a finished (or cancelled) rip run produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RipOutcome {
    /// Paths of the successfully written MP3s, in rip order.
    pub ripped: Vec<String>,
    /// One "N: error" line per failed track.
    pub failures: Vec<String>,
    /// The cancel flag fired (the run stopped after the then-current track).
    pub cancelled: bool,
}

impl RipOutcome {
    /// The one-line result every frontend shows, given how many of the ripped
    /// files the library import actually registered (import only accepts
    /// files under watched folders). Failures include their reason — a bare
    /// count told the user nothing when e.g. the destination was read-only.
    pub fn status_message(&self, imported: usize) -> String {
        let mut msg = format!(
            "Ripped {} track{}",
            self.ripped.len(),
            if self.ripped.len() == 1 { "" } else { "s" }
        );
        if self.cancelled {
            msg.push_str(" · cancelled");
        }
        if !self.ripped.is_empty() && imported == 0 {
            msg.push_str(" · not in library (destination isn't a watched folder)");
        } else if imported < self.ripped.len() {
            msg.push_str(&format!(" · only {imported} added to the library"));
        }
        if !self.failures.is_empty() {
            msg.push_str(&format!(
                " · {} failed — {}",
                self.failures.len(),
                truncate_reason(&self.failures.join("; "), 160)
            ));
        }
        msg
    }
}

/// Cap a failure blob for a one-line status (full reasons stay in the
/// outcome for anyone who wants to log them).
fn truncate_reason(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

/// Verify the rip destination is actually writable before any drive work:
/// create it (and parents) if needed, then probe with a real file create.
/// Returns the human-readable reason when it isn't.
fn check_dest_writable(dest_root: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest_root)
        .map_err(|e| format!("can't create {}: {e}", dest_root.display()))?;
    let probe = dest_root.join(format!(".sparkamp-write-test-{}", std::process::id()));
    std::fs::File::create(&probe)
        .map_err(|e| format!("{} isn't writable: {e}", dest_root.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Rip a whole selection, blocking — call on a worker thread. Reports
/// progress through the callback as `(track index, track count, title,
/// within-track fraction 0.0–1.0)` — at each track start and then as the
/// pipeline position advances, so the UI bar moves *within* a track (a
/// one-track rip used to sit at 0% for the whole encode). Checks `cancel`
/// between tracks (a cancel stops after the current track) and derives each
/// track's source, tags, and destination from the entry + the disc's tag
/// set. This is the one job runner shared by the frontends.
pub fn run_job(
    entries: &[crate::disc::DiscTrackEntry],
    dest_root: &Path,
    quality: Mp3Quality,
    tags: &crate::disc::xmcd::XmcdEntry,
    total_on_disc: u8,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize, usize, &str, f64),
) -> RipOutcome {
    let mut outcome = RipOutcome::default();
    // A read-only destination would fail every track with the same reason —
    // catch it before touching the drive and report it once, clearly.
    if let Err(reason) = check_dest_writable(dest_root) {
        outcome.failures.push(reason);
        return outcome;
    }
    // The rip's streaming reads own the drive: keep every detection poll
    // (even status ioctls) off the device for the whole run.
    crate::disc::detect::set_exclusive_read(true);
    let n = entries.len();
    for (i, entry) in entries.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            break;
        }
        progress(i, n, &entry.title, 0.0);
        let source = source_for_entry(entry);
        let track_tags = tag_fields_for_track(
            &tags.artist,
            &tags.album,
            &tags.year,
            &tags.genre,
            entry.number,
            total_on_disc,
            &entry.title,
        );
        let out = dest_path(
            dest_root,
            &tags.artist,
            &tags.album,
            entry.number,
            &track_tags.title,
        );
        let dur = entry.duration_secs.max(1) as f64;
        let result = rip_track_observed(&source, &out, quality, &track_tags, |pos_secs| {
            progress(i, n, &entry.title, (pos_secs / dur).clamp(0.0, 1.0));
        });
        match result {
            Ok(()) => outcome.ripped.push(out.display().to_string()),
            Err(e) => outcome.failures.push(format!("{}: {e}", entry.number)),
        }
    }
    crate::disc::detect::set_exclusive_read(false);
    outcome
}

/// Whether a rip destination sits under one of the watched library folders —
/// outside every one, the import step skips the ripped files (library policy:
/// importing never creates watch folders). Comparison is component-wise (so
/// `/music-other` never matches a watched `/music`) on canonicalized paths
/// when they exist (so symlinked watch folders still count; a
/// not-yet-created destination falls back to its literal path).
pub fn dest_is_watched(dest: &str, watched_folders: &[String]) -> bool {
    let dest = Path::new(dest);
    let dest = dest.canonicalize().unwrap_or_else(|_| dest.to_path_buf());
    watched_folders.iter().any(|folder| {
        let folder = Path::new(folder);
        let folder = folder.canonicalize().unwrap_or_else(|_| folder.to_path_buf());
        dest.starts_with(&folder)
    })
}

/// Default rip destination: the configured directory, else the first watched
/// library folder, else `~/Music`. (The frontends pass their config value and
/// folder list; the choice the user makes in the rip dialog is written back
/// to config.)
pub fn default_dest(configured: Option<&Path>, first_watched: Option<&str>) -> String {
    if let Some(dir) = configured {
        return dir.display().to_string();
    }
    if let Some(folder) = first_watched {
        return folder.to_string();
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    format!("{home}/Music")
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

    #[test]
    fn source_for_entry_maps_uris() {
        let cdda = crate::disc::DiscTrackEntry {
            number: 3,
            path: "cdda://3?device=/dev/sr0".into(),
            title: "Track 3".into(),
            duration_secs: 200,
        };
        assert_eq!(
            source_for_entry(&cdda),
            RipSource::Cdda {
                device: "/dev/sr0".into(),
                track: 3
            }
        );
        // Unparseable track part falls back to the entry's number.
        let odd = crate::disc::DiscTrackEntry {
            number: 7,
            path: "cdda://x?device=/dev/sr1".into(),
            ..cdda.clone()
        };
        assert_eq!(
            source_for_entry(&odd),
            RipSource::Cdda {
                device: "/dev/sr1".into(),
                track: 7
            }
        );
        let file = crate::disc::DiscTrackEntry {
            number: 1,
            path: "/Volumes/Audio CD/1 Audio Track.aiff".into(),
            title: "Track 1".into(),
            duration_secs: 100,
        };
        assert_eq!(
            source_for_entry(&file),
            RipSource::File {
                path: PathBuf::from("/Volumes/Audio CD/1 Audio Track.aiff")
            }
        );
    }

    #[test]
    fn outcome_status_messages() {
        let mut o = RipOutcome {
            ripped: vec!["a.mp3".into(), "b.mp3".into()],
            failures: vec![],
            cancelled: false,
        };
        assert_eq!(o.status_message(2), "Ripped 2 tracks");
        assert_eq!(
            o.status_message(0),
            "Ripped 2 tracks · not in library (destination isn't a watched folder)"
        );
        assert_eq!(o.status_message(1), "Ripped 2 tracks · only 1 added to the library");
        o.cancelled = true;
        o.failures.push("4: stalled".into());
        assert_eq!(
            o.status_message(2),
            "Ripped 2 tracks · cancelled · 1 failed — 4: stalled"
        );
        let one = RipOutcome {
            ripped: vec!["a.mp3".into()],
            ..Default::default()
        };
        assert_eq!(one.status_message(1), "Ripped 1 track");
        let none = RipOutcome::default();
        assert_eq!(none.status_message(0), "Ripped 0 tracks");
    }

    #[test]
    fn run_job_honors_preset_cancel() {
        // Cancel already set: the loop must exit before touching GStreamer,
        // reporting cancelled with no progress callbacks.
        let entries = vec![crate::disc::DiscTrackEntry {
            number: 1,
            path: "cdda://1?device=/dev/sr0".into(),
            title: "Track 1".into(),
            duration_secs: 100,
        }];
        let cancel = AtomicBool::new(true);
        let mut calls = 0;
        let outcome = run_job(
            &entries,
            &std::env::temp_dir(),
            Mp3Quality::VbrV2,
            &crate::disc::xmcd::XmcdEntry::default(),
            1,
            &cancel,
            |_, _, _, _| calls += 1,
        );
        assert!(outcome.cancelled);
        assert!(outcome.ripped.is_empty() && outcome.failures.is_empty());
        assert_eq!(calls, 0);
    }

    #[test]
    fn run_job_fails_fast_on_unwritable_dest() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("sparkamp-ro-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o555)).unwrap();

        let entries = vec![crate::disc::DiscTrackEntry {
            number: 1,
            path: "cdda://1?device=/dev/sr0".into(),
            title: "Track 1".into(),
            duration_secs: 100,
        }];
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        // A subdirectory of the read-only dir: create_dir_all must fail.
        let outcome = run_job(
            &entries,
            &base.join("rips"),
            Mp3Quality::VbrV2,
            &crate::disc::xmcd::XmcdEntry::default(),
            1,
            &cancel,
            |_, _, _, _| calls += 1,
        );
        assert_eq!(calls, 0, "must fail before any drive work");
        assert!(outcome.ripped.is_empty());
        assert_eq!(outcome.failures.len(), 1);
        assert!(
            outcome.failures[0].contains("can't create"),
            "{:?}",
            outcome.failures
        );
        // And the shared status line carries the reason.
        assert!(outcome.status_message(0).contains("failed — can't create"));

        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reason_truncation() {
        assert_eq!(truncate_reason("short", 160), "short");
        let long = "x".repeat(200);
        let cut = truncate_reason(&long, 160);
        assert_eq!(cut.chars().count(), 161);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn dest_is_watched_needs_a_path_boundary() {
        let base = std::env::temp_dir().join(format!("sparkamp-watch-{}", std::process::id()));
        let music = base.join("Music");
        let sibling = base.join("MusicOther");
        let sub = music.join("Rips");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let watched = vec![music.display().to_string()];

        assert!(dest_is_watched(&music.display().to_string(), &watched));
        assert!(dest_is_watched(&sub.display().to_string(), &watched));
        // The old starts_with-on-strings bug: a sibling sharing the prefix.
        assert!(!dest_is_watched(&sibling.display().to_string(), &watched));
        // A destination that doesn't exist yet still resolves by prefix.
        assert!(dest_is_watched(
            &music.join("New Album").display().to_string(),
            &watched
        ));
        assert!(!dest_is_watched("/somewhere/else", &watched));
        assert!(!dest_is_watched("/anywhere", &[]));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn default_dest_chain() {
        assert_eq!(
            default_dest(Some(Path::new("/cfg/rips")), Some("/watched")),
            "/cfg/rips"
        );
        assert_eq!(default_dest(None, Some("/watched")), "/watched");
        let fallback = default_dest(None, None);
        assert!(fallback.ends_with("/Music"), "{fallback}");
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
