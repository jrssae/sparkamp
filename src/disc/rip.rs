//! Rip audio-CD tracks to tagged MP3s.
//!
//! One GStreamer pipeline per track: the source differs by platform (macOS
//! decodes the auto-mounted AIFF file; Linux reads the drive directly via
//! `cdparanoiasrc`), the tail is shared — `audioconvert ! lamemp3enc !
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

use crate::disc::transcode::RipFormat;
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
    /// Read by the GStreamer encoder adapter, which is the only thing that
    /// speaks `lamemp3enc` — and which is not compiled on macOS, where FLAC
    /// through AVFoundation is the whole of what can be written.
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn encoder_props(self) -> &'static str {
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
/// `<dest_root>/Artist/Album/NN - Title.<ext>`, all components sanitized.
/// Empty artist/album become "Unknown Artist"/"Unknown Album"; an empty
/// title becomes "Track NN".
///
/// The extension comes from the format rather than being written here,
/// because it is not the same on both platforms — macOS rips to FLAC — and a
/// hardcoded `.mp3` was how that difference would have leaked out.
pub fn dest_path(
    dest_root: &Path,
    artist: &str,
    album: &str,
    number: u8,
    title: &str,
    format: RipFormat,
) -> PathBuf {
    let artist = safe_component(artist, "Unknown Artist");
    let album = safe_component(album, "Unknown Album");
    let title = safe_component(title, &format!("Track {number:02}"));
    dest_root
        .join(artist)
        .join(album)
        .join(format!("{number:02} - {title}.{}", format.extension()))
}


/// Rip one track: run the pipeline to EOS (blocking — call on a worker
/// thread), then write the tags onto the fresh MP3. Creates the destination
/// directories. On any error the partial output file is removed.
#[allow(dead_code)] // the frontends go through run_job; the FFI (lib only) rips per track
/// **The caller must hold an exclusive-read scope** for the whole call when
/// `source` is [`RipSource::Cdda`] — [`crate::disc::detect::begin_exclusive_read`]
/// / `end_exclusive_read`. [`run_job`] does this for a whole run; anything
/// calling a single track directly has to do it itself.
///
/// Without it the detector's drive polling opens the device mid-read and
/// libcdio fails partway through the track with "cdio_read_audio_sector …
/// No such device". The pipeline is fine; it just loses the drive underneath
/// itself, and the error names neither the poll nor the cause.
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
///
/// Carries the same exclusive-read requirement as [`rip_track`].
pub fn rip_track_observed(
    source: &RipSource,
    out: &Path,
    quality: Mp3Quality,
    tags: &TagFields,
    mut on_position: impl FnMut(f64),
) -> Result<(), String> {
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }

    let (written, ()) = crate::disc::transcode::encode(
        source,
        out,
        RipFormat::Mp3(quality),
        &mut on_position,
    )?;
    write_track_tags(out, written, tags)
}

/// Write `tags` onto a freshly ripped file, in whatever container it uses.
///
/// ID3 and Vorbis comments are not interchangeable, and neither encoder writes
/// tags itself. Keeping the choice here, keyed off the format that was
/// actually written, is what stops a FLAC being handed an ID3 tag no FLAC
/// reader looks for.
fn write_track_tags(out: &Path, format: RipFormat, tags: &TagFields) -> Result<(), String> {
    if format.tags_are_id3() {
        return crate::id3_editor::write_tag_fields(out, tags)
            .map_err(|e| format!("tag write: {e}"));
    }
    write_vorbis_comments(out, tags).map_err(|e| format!("tag write: {e}"))
}

/// Every field a Vorbis comment block has a home for.
///
/// The field names are the documented Xiph ones, which is what makes the
/// result readable by anything else — a rip nobody else can read the tags of
/// is a rip with no tags.
fn write_vorbis_comments(out: &Path, tags: &TagFields) -> Result<(), String> {
    let mut flac = metaflac::Tag::read_from_path(out).map_err(|e| e.to_string())?;
    {
        let c = flac.vorbis_comments_mut();
        for (key, value) in [
            ("TITLE", &tags.title),
            ("ARTIST", &tags.artist),
            ("ALBUM", &tags.album),
            ("ALBUMARTIST", &tags.album_artist),
            ("GENRE", &tags.genre),
            ("DATE", &tags.year),
            ("TRACKNUMBER", &tags.track_number),
            ("TRACKTOTAL", &tags.track_total),
            ("DISCNUMBER", &tags.disc_number),
            ("DISCTOTAL", &tags.disc_total),
            ("COMPOSER", &tags.composer),
            ("COMMENT", &tags.comment),
            ("COPYRIGHT", &tags.copyright),
            ("BPM", &tags.bpm),
        ] {
            // An empty value is not a tag. Writing one would leave every
            // reader showing a blank field where it should show nothing.
            if !value.is_empty() {
                c.set(key, vec![value.clone()]);
            }
        }
    }
    flac.write_to_path(out).map_err(|e| e.to_string())?;
    Ok(())
}

/// Build, play, and drain a pipeline until EOS or error, reporting the
/// pipeline position in seconds roughly twice a second while it runs
/// (nothing on the EOS/error path) via `on_position`. GStreamer must already
/// be initialized (both frontends do it at startup). Shared with the burn
/// module's Red Book WAV preparation (`burn::prepare_wav`/`prepare_wav_observed`).
#[cfg(not(target_os = "macos"))]
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
        composer: String::new(),
        original_artist: String::new(),
        copyright: String::new(),
        url: String::new(),
        encoded_by: String::new(),
        lyric: String::new(),
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
/// How many times to attempt a single track before giving up on it. A scratched
/// or dirty disc often yields a transient read error that a re-read clears, so
/// each track is retried automatically rather than making the user babysit the
/// rip with a prompt. Unrecoverable tracks are then skipped and reported; the
/// whole run can still be aborted (cancel) between attempts.
const RIP_MAX_ATTEMPTS: u32 = 2;

/// Attempt one track up to [`RIP_MAX_ATTEMPTS`] times, stopping on the first
/// success or once cancelled. `try_once(attempt)` runs a single rip attempt
/// (1-based). Split out so the retry policy is unit-testable without a drive.
fn rip_with_retries(
    cancel: &AtomicBool,
    mut try_once: impl FnMut(u32) -> Result<(), String>,
) -> Result<(), String> {
    let mut last = Err("not attempted".to_string());
    for attempt in 1..=RIP_MAX_ATTEMPTS {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        last = try_once(attempt);
        if last.is_ok() {
            break;
        }
    }
    last
}

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
    crate::disc::detect::begin_exclusive_read();
    // `tags` is taken as given. It is what the rip window holds, and the rip
    // window is the last word: prepopulated from the disc's own metadata (see
    // `xmcd::XmcdEntry::merged_with`) and then whatever the user made of it.
    // Topping it up from the disc here would quietly undo a field they cleared
    // on purpose.
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
        // The format this platform will actually write, asked once per track
        // so the filename and the tag container agree with what comes out.
        let format = if crate::disc::transcode::can_write(RipFormat::Mp3(quality)) {
            RipFormat::Mp3(quality)
        } else {
            crate::disc::transcode::default_rip_format()
        };
        let out = dest_path(
            dest_root,
            &tags.artist,
            &tags.album,
            entry.number,
            &track_tags.title,
            format,
        );
        let dur = entry.duration_secs.max(1) as f64;
        let result = rip_with_retries(cancel, |attempt| {
            // Show the retry in the progress label so a re-read is visible.
            let label = if attempt == 1 {
                entry.title.clone()
            } else {
                format!("{} — retry {}", entry.title, attempt - 1)
            };
            progress(i, n, &label, 0.0);
            rip_track_observed(&source, &out, quality, &track_tags, |pos_secs| {
                progress(i, n, &label, (pos_secs / dur).clamp(0.0, 1.0));
            })
        });
        // A cancel that landed during (or just before) this track ends the run
        // without recording a phantom failure — `rip_with_retries` returns a
        // sentinel error when cancelled before its first attempt, which must not
        // reach the user as a failed track. A track that still completed counts.
        if cancel.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            if result.is_ok() {
                outcome.ripped.push(out.display().to_string());
            }
            break;
        }
        match result {
            Ok(()) => outcome.ripped.push(out.display().to_string()),
            Err(e) => outcome.failures.push(format!("{}: {e}", entry.number)),
        }
    }
    crate::disc::detect::end_exclusive_read();
    outcome
}

/// Whether a rip destination sits under one of the watched library folders —
/// outside every one, the import step skips the ripped files (library policy:
/// importing never creates watch folders). Comparison is component-wise (so
/// `/music-other` never matches a watched `/music`) on canonicalized paths so
/// symlinked watch folders still count.
pub fn dest_is_watched(dest: &str, watched_folders: &[String]) -> bool {
    let dest = crate::pathutil::canonicalize_lenient(Path::new(dest));
    watched_folders.iter().any(|folder| {
        let folder = crate::pathutil::canonicalize_lenient(Path::new(folder));
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
        let mp3 = RipFormat::Mp3(Mp3Quality::VbrV2);
        let p = dest_path(
            Path::new("/music"),
            "AC/DC",
            "Back: In Black?",
            3,
            "Hells Bells",
            mp3,
        );
        assert_eq!(
            p,
            Path::new("/music/AC_DC/Back_ In Black_/03 - Hells Bells.mp3")
        );
        let p = dest_path(Path::new("/m"), "", "", 12, "", mp3);
        assert_eq!(
            p,
            Path::new("/m/Unknown Artist/Unknown Album/12 - Track 12.mp3")
        );
    }

    /// The extension follows the format, not the platform's habits. macOS rips
    /// to FLAC, and a path that still said `.mp3` would be a file whose name
    /// lies about its contents.
    #[test]
    fn dest_path_takes_its_extension_from_the_format() {
        let flac = dest_path(Path::new("/m"), "A", "B", 1, "T", RipFormat::Flac);
        assert_eq!(flac, Path::new("/m/A/B/01 - T.flac"));
        let mp3 = dest_path(
            Path::new("/m"),
            "A",
            "B",
            1,
            "T",
            RipFormat::Mp3(Mp3Quality::VbrV0),
        );
        assert_eq!(mp3, Path::new("/m/A/B/01 - T.mp3"));
    }

    /// Which container the tags go in follows the format too. A FLAC handed an
    /// ID3 tag has, as far as every FLAC reader is concerned, no tags at all.
    #[test]
    fn tag_container_follows_the_format() {
        assert!(RipFormat::Mp3(Mp3Quality::VbrV2).tags_are_id3());
        assert!(!RipFormat::Flac.tags_are_id3());
    }

    /// A rip must come out as a real file of the format it claims, with the
    /// tags in the container that format uses.
    /// `SPARKAMP_FORMAT_DIR=<dir of t.<ext> samples> cargo test --lib \
    ///   rips_to_the_platform_format -- --ignored --nocapture`
    ///
    /// `#[ignore]` because it needs a sample file the repository does not
    /// carry. It stands in for a mounted CD track, which on macOS is exactly
    /// what a rip source is.
    #[test]
    #[ignore]
    fn rips_to_the_platform_format() {
        let Some(dir) = std::env::var_os("SPARKAMP_FORMAT_DIR") else {
            println!("set SPARKAMP_FORMAT_DIR to a directory of t.<ext> samples");
            return;
        };
        let src = std::path::PathBuf::from(dir).join("t.wav");
        if !src.exists() {
            println!("no t.wav sample — skipping");
            return;
        }
        let format = crate::disc::transcode::default_rip_format();
        println!("this platform rips to {format:?}");

        let out_dir = std::env::temp_dir().join(format!("sparkamp-rip-{}", std::process::id()));
        let out = dest_path(&out_dir, "Test Artist", "Test Album", 7, "Test Title", format);
        assert_eq!(
            out.extension().and_then(|e| e.to_str()),
            Some(format.extension())
        );

        let tags = tag_fields_for_track(
            "Test Artist",
            "Test Album",
            "1999",
            "Jazz",
            7,
            12,
            "Test Title",
        );
        let mut ticks = 0usize;
        let result = rip_track_observed(
            &RipSource::File { path: src },
            &out,
            Mp3Quality::VbrV2,
            &tags,
            |_| ticks += 1,
        );
        match result {
            Ok(()) => {
                let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                println!("wrote {} ({bytes} bytes, {ticks} tick(s))", out.display());
                assert!(bytes > 0, "the rip wrote an empty file");
                // The magic, not the extension: a file named .flac that is
                // not FLAC fails everywhere except a filename check.
                let head = std::fs::read(&out).unwrap();
                if format == crate::disc::transcode::RipFormat::Flac {
                    assert_eq!(&head[0..4], b"fLaC", "not a FLAC stream");
                    let flac = metaflac::Tag::read_from_path(&out).expect("read back the tags");
                    let c = flac.vorbis_comments().expect("no vorbis comment block");
                    let get = |k: &str| c.get(k).and_then(|v| v.first()).cloned().unwrap_or_default();
                    println!(
                        "  TITLE={:?} ARTIST={:?} ALBUM={:?} DATE={:?} GENRE={:?} TRACKNUMBER={:?}",
                        get("TITLE"), get("ARTIST"), get("ALBUM"),
                        get("DATE"), get("GENRE"), get("TRACKNUMBER")
                    );
                    assert_eq!(get("TITLE"), "Test Title");
                    assert_eq!(get("ARTIST"), "Test Artist");
                    assert_eq!(get("ALBUM"), "Test Album");
                    assert_eq!(get("DATE"), "1999");
                    assert_eq!(get("GENRE"), "Jazz");
                    assert_eq!(get("TRACKNUMBER"), "7");
                    assert_eq!(get("TRACKTOTAL"), "12");
                    // A field with nothing in it is not written at all.
                    assert!(c.get("BPM").is_none(), "an empty field must not be written");
                }
                assert!(ticks > 0, "the rip reported no progress");
            }
            Err(e) => panic!("rip failed: {e}"),
        }
        let _ = std::fs::remove_dir_all(&out_dir);
    }

    /// LIVE: rip the first two tracks off the loaded audio CD.
    /// `cargo test --lib live_rip_from_disc -- --ignored --nocapture`
    ///
    /// The end-to-end shape a user gets: a mounted disc track in, a tagged
    /// file of this platform's format out, named from the metadata. Reads the
    /// disc; writes only to a temp directory.
    #[test]
    #[ignore]
    fn live_rip_from_disc() {
        let drives = crate::disc::detect::list_drives();
        let Some(drive) = drives.iter().find(|d| d.media.is_audio_cd) else {
            println!("no audio CD loaded — skipping");
            return;
        };
        let entries = crate::disc::toc::track_entries(drive);
        if entries.is_empty() {
            println!("no track entries — skipping");
            return;
        }
        let format = crate::disc::transcode::default_rip_format();
        println!("{} track(s), ripping the first two as {format:?}", entries.len());

        let tags = crate::disc::xmcd::XmcdEntry {
            artist: "Live Rip Artist".into(),
            album: "Live Rip Album".into(),
            year: "2026".into(),
            genre: "Jazz".into(),
            track_titles: entries.iter().map(|e| e.title.clone()).collect(),
            ..Default::default()
        };
        let dest = std::env::temp_dir().join(format!("sparkamp-liverip-{}", std::process::id()));
        let total = entries.len() as u8;

        crate::disc::detect::begin_exclusive_read();
        let mut wrote = Vec::new();
        for entry in entries.iter().take(2) {
            let track_tags = tag_fields_for_track(
                &tags.artist,
                &tags.album,
                &tags.year,
                &tags.genre,
                entry.number,
                total,
                &entry.title,
            );
            let out = dest_path(
                &dest,
                &tags.artist,
                &tags.album,
                entry.number,
                &track_tags.title,
                format,
            );
            let started = std::time::Instant::now();
            let mut ticks = 0usize;
            let r = rip_track_observed(
                &source_for_entry(entry),
                &out,
                Mp3Quality::VbrV2,
                &track_tags,
                |_| ticks += 1,
            );
            match r {
                Ok(()) => {
                    let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                    println!(
                        "  track {} -> {} ({bytes} bytes, {ticks} ticks, {:.1?})",
                        entry.number,
                        out.file_name().unwrap().to_string_lossy(),
                        started.elapsed()
                    );
                    assert!(bytes > 0, "track {} ripped to an empty file", entry.number);
                    wrote.push(out);
                }
                Err(e) => {
                    crate::disc::detect::end_exclusive_read();
                    panic!("track {} failed: {e}", entry.number);
                }
            }
        }
        crate::disc::detect::end_exclusive_read();

        for out in &wrote {
            let head = std::fs::read(out).unwrap();
            if format == crate::disc::transcode::RipFormat::Flac {
                assert_eq!(&head[0..4], b"fLaC", "{} is not a FLAC stream", out.display());
                let flac = metaflac::Tag::read_from_path(out).expect("read tags back");
                let c = flac.vorbis_comments().expect("no vorbis comments");
                let get =
                    |k: &str| c.get(k).and_then(|v| v.first()).cloned().unwrap_or_default();
                println!(
                    "    TITLE={:?} ARTIST={:?} ALBUM={:?} TRACKNUMBER={:?}/{:?}",
                    get("TITLE"),
                    get("ARTIST"),
                    get("ALBUM"),
                    get("TRACKNUMBER"),
                    get("TRACKTOTAL")
                );
                assert_eq!(get("ALBUM"), "Live Rip Album");
                assert_eq!(get("TRACKTOTAL"), total.to_string());
                assert!(!get("TITLE").is_empty(), "a ripped track must carry a title");
            }
        }
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// LIVE: the rip window has the last word.
    /// `cargo test --lib live_rip_window_overrides_the_disc -- --ignored --nocapture`
    ///
    /// Walks the whole precedence chain against a real disc — gnudb where
    /// there is one, the disc's own CD-TEXT filling its gaps, and then a user
    /// edit on top — and rips two tracks to prove which one reached the file.
    ///
    /// The edit is the point. Everything else in this module can be right
    /// while a value the user typed is quietly replaced by one from the disc,
    /// and that failure looks exactly like success until you read the tags.
    #[test]
    #[ignore]
    fn live_rip_window_overrides_the_disc() {
        let drives = crate::disc::detect::list_drives();
        let Some(drive) = drives.iter().find(|d| d.media.is_audio_cd) else {
            println!("no audio CD loaded — skipping");
            return;
        };
        let Some(toc) = drive.toc.as_ref() else {
            println!("no TOC — skipping");
            return;
        };
        let discid = crate::disc::discid::freedb_discid(toc);
        let mut entries = crate::disc::toc::track_entries(drive);
        if entries.len() < 2 {
            println!("need at least two tracks — skipping");
            return;
        }

        // 1. The disc's own CD-TEXT.
        crate::disc::detect::begin_exclusive_read();
        let cdtext = crate::disc::cdtext::read_cdtext(&drive.id).ok();
        crate::disc::detect::end_exclusive_read();
        let from_disc = cdtext.map(|cd| cd.to_xmcd(&discid)).unwrap_or_default();
        println!(
            "CD-TEXT: album={:?} artist={:?} {} title(s)",
            from_disc.album,
            from_disc.artist,
            from_disc.track_titles.len()
        );

        // 2. gnudb, where there is one. Not looked up here — this test must
        //    not depend on the network — so it stands in as empty, which is
        //    also the case the merge has to handle: no gnudb entry means the
        //    prepopulation is CD-TEXT alone.
        let gnudb = crate::disc::xmcd::XmcdEntry::default();

        // 3. What the rip window is prepopulated with.
        let prepopulated = gnudb.merged_with(&from_disc);
        assert!(
            !prepopulated.is_empty(),
            "this disc offered no metadata at all — load one with CD-TEXT"
        );
        println!(
            "prepopulated: album={:?} artist={:?}",
            prepopulated.album, prepopulated.artist
        );

        // 4. The user edits the window: a new album, and a new title for
        //    track 1 only. Track 2 is left as the disc had it.
        const EDITED_ALBUM: &str = "Edited In The Rip Window";
        const EDITED_TITLE: &str = "A Title The Disc Never Had";
        let disc_title_2 = prepopulated
            .track_titles
            .get(1)
            .cloned()
            .unwrap_or_default();
        assert!(
            !disc_title_2.is_empty(),
            "track 2 must have a disc title for this test to mean anything"
        );
        assert_ne!(
            disc_title_2, EDITED_TITLE,
            "the edit must differ from what the disc says, or it proves nothing"
        );

        let mut window = prepopulated.clone();
        window.album = EDITED_ALBUM.to_string();
        window.track_titles[0] = EDITED_TITLE.to_string();
        // The frontends apply the window's titles onto the entries, which is
        // what `run_job` reads for each track's title.
        for (entry, title) in entries.iter_mut().zip(&window.track_titles) {
            entry.title = title.clone();
        }

        // 5. Rip the two tracks the edit is about.
        let format = crate::disc::transcode::default_rip_format();
        let dest = std::env::temp_dir().join(format!("sparkamp-winrip-{}", std::process::id()));
        let total = entries.len() as u8;
        crate::disc::detect::begin_exclusive_read();
        let mut wrote = Vec::new();
        for entry in entries.iter().take(2) {
            let track_tags = tag_fields_for_track(
                &window.artist,
                &window.album,
                &window.year,
                &window.genre,
                entry.number,
                total,
                &entry.title,
            );
            let out = dest_path(
                &dest,
                &window.artist,
                &window.album,
                entry.number,
                &track_tags.title,
                format,
            );
            let r = rip_track_observed(
                &source_for_entry(entry),
                &out,
                Mp3Quality::VbrV2,
                &track_tags,
                |_| {},
            );
            if let Err(e) = r {
                crate::disc::detect::end_exclusive_read();
                panic!("track {} failed: {e}", entry.number);
            }
            println!("  wrote {}", out.display());
            wrote.push(out);
        }
        crate::disc::detect::end_exclusive_read();

        // The edited album must be on both files, and in the path.
        for out in &wrote {
            assert!(
                out.to_string_lossy().contains(EDITED_ALBUM),
                "the edited album must name the directory: {}",
                out.display()
            );
        }
        // Track 1 carries the edit; track 2 carries what the disc said.
        assert!(
            wrote[0].to_string_lossy().contains(EDITED_TITLE),
            "the edited title must name the file: {}",
            wrote[0].display()
        );

        if format == crate::disc::transcode::RipFormat::Flac {
            let title_of = |p: &std::path::Path| {
                let flac = metaflac::Tag::read_from_path(p).expect("read tags");
                let c = flac.vorbis_comments().expect("no vorbis comments");
                let get =
                    |k: &str| c.get(k).and_then(|v| v.first()).cloned().unwrap_or_default();
                (get("TITLE"), get("ALBUM"))
            };
            let (t1, a1) = title_of(&wrote[0]);
            let (t2, a2) = title_of(&wrote[1]);
            println!("  track 1: TITLE={t1:?} ALBUM={a1:?}");
            println!("  track 2: TITLE={t2:?} ALBUM={a2:?}");
            assert_eq!(t1, EDITED_TITLE, "the window's title must win over the disc's");
            assert_eq!(a1, EDITED_ALBUM, "the window's album must win over the disc's");
            assert_eq!(a2, EDITED_ALBUM, "the edit applies to every track");
            assert_eq!(
                t2, disc_title_2,
                "an untouched track keeps what the disc said"
            );
        }
        let _ = std::fs::remove_dir_all(&dest);
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
        // `run_job` enters an exclusive-read scope, so this must not overlap
        // the tests asserting on that counter.
        let _guard = crate::disc::detect::exclusive_read_test_guard();
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
    fn rip_with_retries_recovers_gives_up_and_respects_cancel() {
        let no_cancel = AtomicBool::new(false);

        // A transient failure on the first read clears on retry → success.
        let mut calls = 0;
        let r = rip_with_retries(&no_cancel, |attempt| {
            calls += 1;
            if attempt < 2 {
                Err("read glitch".into())
            } else {
                Ok(())
            }
        });
        assert!(r.is_ok());
        assert_eq!(calls, 2);

        // A hard-bad track fails every attempt → Err after RIP_MAX_ATTEMPTS.
        let mut calls: u32 = 0;
        let r = rip_with_retries(&no_cancel, |_| {
            calls += 1;
            Err::<(), String>("scratched".into())
        });
        assert!(r.unwrap_err().contains("scratched"));
        assert_eq!(calls, RIP_MAX_ATTEMPTS);

        // Cancel already set: abort before any attempt.
        let cancel = AtomicBool::new(true);
        let mut calls = 0;
        let r = rip_with_retries(&cancel, |_| {
            calls += 1;
            Ok::<(), String>(())
        });
        assert!(r.is_err());
        assert_eq!(calls, 0);
    }

    #[test]
    fn run_job_fails_fast_on_unwritable_dest() {
        // Same reason as above: `run_job` holds the exclusive-read guard.
        let _guard = crate::disc::detect::exclusive_read_test_guard();
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
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().expect("gst init");
        let drives = crate::disc::detect::list_drives();
        let Some(drive) = drives.iter().find(|d| d.media.is_audio_cd) else {
            println!("no audio CD in any drive — skipping");
            return;
        };
        let Some(entry) = crate::disc::toc::track_entries(drive).into_iter().next() else {
            println!("audio CD has no track entries — skipping");
            return;
        };

        // `DiscTrackEntry::path` is platform-shaped: a mounted AIFF file on
        // macOS, a `cdda://N?device=…` pseudo-URI on Linux, where audio CDs do
        // not mount. This test used to wrap it in `RipSource::File`
        // unconditionally and failed with `filesrc … No such file
        // "cdda://1?device=/dev/sr0"`. `source_for_entry` is the mapping
        // production already uses, so going through it tests that too rather
        // than a second copy of the same branch.
        let source = source_for_entry(&entry);
        let dir = std::env::temp_dir().join(format!("sparkamp-rip-{}", std::process::id()));
        let tags = tag_fields_for_track("Live Artist", "Live Album", "2026", "Rock", 1, 8, "Live Test");
        let out = dest_path(
            &dir,
            "Live Artist",
            "Live Album",
            1,
            "Live Test",
            crate::disc::transcode::default_rip_format(),
        );
        let started = std::time::Instant::now();
        // Hold the drive for the duration, exactly as the live burn tests do.
        // Without it the detector's polling can open the device mid-read and
        // libcdio fails with "cdio_read_audio_sector … No such device" partway
        // through the track — the pipeline itself is fine, it just loses the
        // drive underneath it.
        crate::disc::detect::begin_exclusive_read();
        let ripped = rip_track(&source, &out, Mp3Quality::VbrV2, &tags);
        crate::disc::detect::end_exclusive_read();
        ripped.expect("rip");
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
