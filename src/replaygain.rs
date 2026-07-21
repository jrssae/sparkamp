//! ReplayGain analysis — album batching, value formatting, and the GStreamer
//! `rganalysis` pipeline that computes track/album gain + peak. The playback
//! side (applying gain via `rgvolume`) lives in `engine.rs`; this module only
//! MEASURES and hands results to the media library / tag write-back.
//!
//! Analysis decodes whole files, so callers run it on a single background
//! worker (never per-track in parallel — decoding is CPU-bound).

use gstreamer as gst;
use gstreamer::prelude::*;

use crate::media_library::LibTrack;

/// One track's ReplayGain result: gains in dB, peaks linear (0..~1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RgResult {
    pub track_gain: f64,
    pub track_peak: f64,
    pub album_gain: f64,
    pub album_peak: f64,
}

/// Format a gain value as Winamp-compatible ReplayGain text, e.g. `-6.20 dB`.
pub fn format_gain_db(gain_db: f64) -> String {
    format!("{:.2} dB", gain_db)
}

/// Format a linear peak value the way ReplayGain tags store it, e.g.
/// `0.988123` (six decimals).
pub fn format_peak(peak: f64) -> String {
    format!("{:.6}", peak)
}

/// The album-grouping key for a track: album + album-artist (falling back to
/// artist), case-insensitive. `None` when the album tag is empty — such tracks
/// analyze alone (a per-track batch), since an "album gain" over unrelated
/// singletons is meaningless.
fn album_key(t: &LibTrack) -> Option<(String, String)> {
    let album = t.album.as_deref().unwrap_or("").trim();
    if album.is_empty() {
        return None;
    }
    let artist = t
        .album_artist
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or(t.artist.as_deref())
        .unwrap_or("")
        .trim();
    Some((album.to_lowercase(), artist.to_lowercase()))
}

/// Group `tracks` into ReplayGain analysis batches (as index lists into the
/// input slice). Tracks sharing an album batch together so album gain is
/// meaningful; album-less tracks each get their own batch. Input order is
/// preserved (a batch appears at the position of its first member).
pub fn album_batches(tracks: &[LibTrack]) -> Vec<Vec<usize>> {
    let mut batches: Vec<Vec<usize>> = Vec::new();
    // Maps an album key to the index of its batch in `batches`.
    let mut by_key: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for (i, t) in tracks.iter().enumerate() {
        match album_key(t) {
            Some(key) => {
                if let Some(&b) = by_key.get(&key) {
                    batches[b].push(i);
                } else {
                    by_key.insert(key, batches.len());
                    batches.push(vec![i]);
                }
            }
            None => batches.push(vec![i]), // album-less → analyze alone
        }
    }
    batches
}

/// `true` when the GStreamer `rganalysis` element is installed. Callers
/// (library actions / auto-analyze) should gate the whole feature on this
/// before offering it, mirroring `Player::rg_available` for playback.
pub fn rg_analysis_available() -> bool {
    let _ = gst::init(); // idempotent; ElementFactory::find needs init first.
    gst::ElementFactory::find("rganalysis").is_some()
}

/// Analyze one album batch (a group of file paths sharing an album, or a
/// single album-less file). Returns one [`RgResult`] per input path, IN THE
/// SAME ORDER.
///
/// Track gain/peak comes from a SEPARATE single-file `rganalysis` pass per
/// file. `concat` merges several files into one continuous stream, so a
/// shared pass emits only ONE computed gain (concat swallows the per-file EOS
/// that would mark a track boundary) — every track after the first otherwise
/// stored a neutral 0.0 dB (the album-batch bug). Album gain/peak: a
/// multi-track batch runs one extra concat pass to measure the whole album's
/// loudness as a single stream; a single-track batch reuses its own pass.
///
/// Runs synchronously on the calling thread (GStreamer elements aren't
/// `Send`, and analysis is CPU-bound decode anyway — callers already run this
/// off a single background worker).
///
/// Returns an error if `rganalysis` isn't installed — callers should gate on
/// [`rg_analysis_available`] first; this is the defensive fallback.
pub fn analyze_batch(paths: &[std::path::PathBuf]) -> anyhow::Result<Vec<RgResult>> {
    let _ = gst::init();
    if gst::ElementFactory::find("rganalysis").is_none() {
        anyhow::bail!("rganalysis element not available (gst-plugins-good missing?)");
    }
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    // Per-track gain/peak — one pass per file, never a shared concat pass.
    let mut track_results: Vec<(f64, f64)> = Vec::with_capacity(paths.len());
    for path in paths {
        let gp = analyze_lump(std::slice::from_ref(path))?.unwrap_or_else(|| {
            eprintln!(
                "replaygain: no computed gain for {}; storing neutral 0.0 dB",
                path.display()
            );
            (0.0, 1.0)
        });
        track_results.push(gp);
    }

    // Album gain/peak — single-track batch reuses its pass; multi-track batch
    // measures the whole album as one concatenated stream.
    let (album_gain, album_peak) = if paths.len() == 1 {
        track_results[0]
    } else {
        analyze_lump(paths)?.unwrap_or((0.0, 1.0))
    };

    Ok(track_results
        .into_iter()
        .map(|(track_gain, track_peak)| RgResult {
            track_gain,
            track_peak,
            album_gain,
            album_peak,
        })
        .collect())
}

/// Run ONE `rganalysis` pass over `paths` (concatenated in input order) and
/// return the single reference-level-stamped (gain, peak) it computes for the
/// whole stream — the track's own value for a single path, or the combined
/// (album) loudness for several. `None` when nothing decodable produced a
/// computed tag. Always tears the pipeline down to `Null` before returning.
///
/// Pipeline shape:
/// ```text
/// filesrc ! decodebin ─┐
/// filesrc ! decodebin ─┼─ concat ! audioconvert ! audioresample ! rganalysis ! fakesink
/// filesrc ! decodebin ─┘
/// ```
/// Each `filesrc ! decodebin` feeds a concat sink pad requested UP FRONT in
/// input order, so stream order through `rganalysis` is deterministic
/// regardless of decode timing.
fn analyze_lump(paths: &[std::path::PathBuf]) -> anyhow::Result<Option<(f64, f64)>> {
    let pipeline = gst::Pipeline::new();
    let concat = gst::ElementFactory::make("concat").build()?;
    let audioconvert = gst::ElementFactory::make("audioconvert").build()?;
    let audioresample = gst::ElementFactory::make("audioresample").build()?;
    let rganalysis = gst::ElementFactory::make("rganalysis").build()?;
    // One computed value for the whole (possibly concatenated) stream.
    rganalysis.set_property("num-tracks", 1i32);
    let fakesink = gst::ElementFactory::make("fakesink").build()?;
    // Analysis has no audience — don't throttle decode to wall-clock playback
    // speed the way a real sink would.
    fakesink.set_property("sync", false);

    pipeline.add_many([&concat, &audioconvert, &audioresample, &rganalysis, &fakesink])?;
    gst::Element::link_many([&concat, &audioconvert, &audioresample, &rganalysis, &fakesink])?;

    // Request concat's sink pads UP FRONT, in input order — concat forwards
    // from its request-ordered sink pads sequentially, so pad i's stream is
    // always track i regardless of which decodebin finishes typefinding
    // first.
    let mut sink_pads = Vec::with_capacity(paths.len());
    for _ in paths {
        let pad = concat
            .request_pad_simple("sink_%u")
            .ok_or_else(|| anyhow::anyhow!("concat: failed to request a sink pad"))?;
        sink_pads.push(pad);
    }

    // One filesrc ! decodebin per file, each wired (once decodebin's async
    // pad-added fires) to its pre-requested concat pad. Mirrors the
    // decodebin pad-added pattern in engine.rs (guard already-linked +
    // filter to audio caps — a file with embedded cover art can make
    // decodebin emit a second, video, pad).
    for (path, sink_pad) in paths.iter().zip(sink_pads.iter()) {
        let filesrc = gst::ElementFactory::make("filesrc").build()?;
        filesrc.set_property("location", path.to_string_lossy().as_ref());
        let decodebin = gst::ElementFactory::make("decodebin").build()?;
        pipeline.add_many([&filesrc, &decodebin])?;
        filesrc.link(&decodebin)?;

        let sink_pad = sink_pad.clone();
        decodebin.connect_pad_added(move |_dbin, src_pad| {
            if sink_pad.is_linked() {
                return;
            }
            let is_audio = src_pad
                .current_caps()
                .map(|c| c.to_string().contains("audio"))
                .unwrap_or(true); // caps not ready yet: try anyway, same as engine.rs
            if is_audio {
                let _ = src_pad.link(&sink_pad);
            }
        });
    }

    // Force a real re-analysis even for files that already carry (possibly
    // wrong) REPLAYGAIN tags — analysis must measure the audio, not trust the
    // file. Default is already true; set it explicitly so a future default
    // change can't silently make us pass stale tags through.
    rganalysis.set_property("forced", true);

    // Read the RECOMPUTED gains from rganalysis's OWN src pad — NOT the bus.
    // The file's pre-existing REPLAYGAIN tags (which may be bogus, e.g. 0.00 dB
    // on a loud track) are also posted to the bus by decodebin; picking those
    // up gave wrong results. rganalysis strips the incoming RG tags and emits
    // its computed track/album gain+peak as downstream tag events on its src
    // pad, one per track in concat order. The probe runs on the streaming
    // thread, so collect into a shared buffer read after EOS.
    #[derive(Default)]
    struct Collected {
        tracks: Vec<(f64, f64)>,
    }
    let collected: std::sync::Arc<std::sync::Mutex<Collected>> =
        std::sync::Arc::new(std::sync::Mutex::new(Collected::default()));
    if let Some(src) = rganalysis.static_pad("src") {
        let collected = collected.clone();
        src.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_pad, info| {
            if let Some(gst::PadProbeData::Event(ev)) = &info.data {
                if let gst::EventView::Tag(tag_ev) = ev.view() {
                    let tags = tag_ev.tag();
                    // rganalysis stamps its OWN computed tag event with the
                    // reference level (89 dB); the file's pass-through original
                    // REPLAYGAIN tags (which arrive first and may be bogus, e.g.
                    // 0.00 dB) do not. Gate on it so we only ever read
                    // rganalysis's freshly-measured values.
                    if tags.get::<gst::tags::ReferenceLevel>().is_none() {
                        return gst::PadProbeReturn::Ok;
                    }
                    let mut c = collected.lock().unwrap();
                    if let Some(g) = tags.get::<gst::tags::TrackGain>() {
                        let peak = tags
                            .get::<gst::tags::TrackPeak>()
                            .map(|v| v.get())
                            .unwrap_or(1.0);
                        c.tracks.push((g.get(), peak));
                    }
                }
            }
            gst::PadProbeReturn::Ok
        });
    }

    pipeline.set_state(gst::State::Playing)?;

    let bus = pipeline
        .bus()
        .ok_or_else(|| anyhow::anyhow!("pipeline has no bus"))?;

    let mut pipeline_err: Option<String> = None;

    // Bus-message watchdog (mirrors disc/rip.rs's stall guard, but keyed on
    // bus activity rather than pipeline position — there's no single
    // "position" that spans multiple concatenated files here). 500ms poll,
    // 60s of total silence means something's wedged. Gains come from the pad
    // probe above; the bus is only for EOS / errors here.
    let mut last_activity = std::time::Instant::now();
    loop {
        match bus.timed_pop(gst::ClockTime::from_mseconds(500)) {
            Some(msg) => {
                last_activity = std::time::Instant::now();
                match msg.view() {
                    gst::MessageView::Eos(..) => break,
                    gst::MessageView::Error(e) => {
                        pipeline_err =
                            Some(format!("{} ({})", e.error(), e.debug().unwrap_or_default()));
                        break;
                    }
                    _ => {}
                }
            }
            None => {
                if last_activity.elapsed() > std::time::Duration::from_secs(60) {
                    pipeline_err = Some("stalled: no bus activity for 60s".to_string());
                    break;
                }
            }
        }
    }

    // Always tear down, success or failure.
    let _ = pipeline.set_state(gst::State::Null);

    if let Some(e) = pipeline_err {
        eprintln!("replaygain: analyze_batch pipeline error: {e}");
    }

    let collected = collected.lock().unwrap();
    // Exactly one reference-level-stamped value for the whole stream (or none,
    // if nothing decoded). Extras shouldn't occur with num-tracks=1.
    Ok(collected.tracks.first().copied())
}

/// Progress snapshot for [`analyze_and_store`], reported after each batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgJobProgress {
    pub done: usize,
    pub total: usize,
}

/// `true` when `t` should be (re-)analyzed: no stored track gain yet, or the
/// file has been modified since the last scan. Both `file_mtime` and
/// `last_scanned` are ISO-8601 strings, which compare lexically — no parsing
/// needed. A pure helper so the "missing OR stale" selection logic (P4-T6's
/// job) has one tested rule to call instead of re-deriving it at each UI
/// entry point.
pub fn needs_analysis(t: &LibTrack) -> bool {
    if t.rg_track_gain.is_none() {
        return true;
    }
    match (&t.file_mtime, &t.last_scanned) {
        (Some(mtime), Some(scanned)) => mtime.as_str() > scanned.as_str(),
        _ => false,
    }
}

/// Analyze `tracks` (already the exact set to process — the caller applies
/// the missing-OR-stale/force filter via [`needs_analysis`]) and store each
/// result via [`crate::media_library::MediaLibrary::set_replaygain`].
///
/// Runs on the CALLER's thread — callers are responsible for spawning a
/// single background worker (analysis is CPU-bound decode; running two in
/// parallel just contends for the same cores). `cancel` is polled between
/// batches (not mid-batch — a batch is one atomic `analyze_batch` call) and,
/// when set, stops early without analyzing remaining batches. `progress` is
/// invoked once per completed batch, whether or not that batch's analysis
/// succeeded.
///
/// Returns the count of tracks actually analyzed (not merely attempted —
/// batches are attempted regardless, but see below: a batch-level pipeline
/// error still yields fallback `RgResult`s that get stored, so "analyzed"
/// here means "a batch containing this track ran", matching what the UI
/// progress bar should count).
pub fn analyze_and_store(
    lib: &crate::media_library::MediaLibrary,
    tracks: &[LibTrack],
    write_tags: bool,
    cancel: &std::sync::atomic::AtomicBool,
    mut progress: impl FnMut(RgJobProgress),
) -> anyhow::Result<usize> {
    use std::sync::atomic::Ordering::Relaxed;

    let batches = album_batches(tracks);
    let total = tracks.len();
    let mut analyzed = 0usize;

    for batch in &batches {
        if cancel.load(Relaxed) {
            break;
        }

        let paths: Vec<std::path::PathBuf> =
            batch.iter().map(|&i| std::path::PathBuf::from(&tracks[i].path)).collect();

        match analyze_batch(&paths) {
            Ok(results) => {
                for (&idx, r) in batch.iter().zip(results.iter()) {
                    let track = &tracks[idx];
                    if let Err(e) =
                        lib.set_replaygain(track.id, r.track_gain, r.track_peak, r.album_gain, r.album_peak)
                    {
                        eprintln!("replaygain: store failed for track {}: {e}", track.id);
                    } else {
                        analyzed += 1;
                    }
                    // Optional MP3 tag write-back (non-MP3 silently skipped).
                    if write_tags {
                        if let Err(e) =
                            write_mp3_replaygain_tags(std::path::Path::new(&track.path), r)
                        {
                            eprintln!("replaygain: tag write-back failed for {}: {e}", track.path);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("replaygain: batch analysis failed: {e}");
            }
        }

        progress(RgJobProgress {
            done: analyzed,
            total,
        });
    }

    Ok(analyzed)
}

/// Outcome of a ReplayGain tag write-back attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteBackOutcome {
    /// TXXX frames written to the MP3.
    Written,
    /// Non-MP3 file — Sparkamp only writes ReplayGain tags to MP3 (id3 path).
    /// Non-MP3 formats keep DB values only (phase-4 known limitation).
    SkippedNonMp3,
}

/// Write the four `REPLAYGAIN_*` TXXX (user-defined text) frames to an MP3,
/// preserving every other frame. Values use the Winamp-compatible formats
/// (`-6.20 dB` / `0.988123`). Existing REPLAYGAIN_* frames with the same
/// description are replaced (not duplicated).
///
/// MP3 ONLY: other formats (M4A/WMA/FLAC/OGG/WAV) return `SkippedNonMp3` and are
/// left untouched — Sparkamp writes tags via the `id3` crate, which is MP3-only.
pub fn write_mp3_replaygain_tags(
    path: &std::path::Path,
    r: &RgResult,
) -> anyhow::Result<WriteBackOutcome> {
    let is_mp3 = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("mp3"))
        .unwrap_or(false);
    if !is_mp3 {
        return Ok(WriteBackOutcome::SkippedNonMp3);
    }

    use id3::frame::ExtendedText;
    use id3::{TagLike, Version};
    let mut tag = id3::Tag::read_from_path(path).unwrap_or_default();
    let pairs = [
        ("REPLAYGAIN_TRACK_GAIN", format_gain_db(r.track_gain)),
        ("REPLAYGAIN_TRACK_PEAK", format_peak(r.track_peak)),
        ("REPLAYGAIN_ALBUM_GAIN", format_gain_db(r.album_gain)),
        ("REPLAYGAIN_ALBUM_PEAK", format_peak(r.album_peak)),
    ];
    for (desc, value) in pairs {
        // Drop any prior frame with this description so we replace, not stack.
        tag.remove_extended_text(Some(desc), None);
        tag.add_frame(ExtendedText {
            description: desc.to_string(),
            value,
        });
    }
    tag.write_to_path(path, Version::Id3v23)
        .map_err(|e| anyhow::anyhow!("write REPLAYGAIN tags to {}: {e}", path.display()))?;
    Ok(WriteBackOutcome::Written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_library::SortKeys;

    fn track(path: &str, album: Option<&str>, album_artist: Option<&str>, artist: Option<&str>) -> LibTrack {
        LibTrack {
            id: 0,
            path: path.to_string(),
            artist: artist.map(String::from),
            title: None,
            album: album.map(String::from),
            track_num: None,
            genre: None,
            year: None,
            bpm: None,
            length_secs: None,
            bitrate: None,
            channels: None,
            filetype: None,
            filename: path.to_string(),
            play_count: 0,
            last_played: None,
            comment: None,
            album_artist: album_artist.map(String::from),
            disc_num: None,
            disc_total: None,
            composer: None,
            original_artist: None,
            copyright: None,
            url: None,
            encoded_by: None,
            lyric: None,
            artwork_path: None,
            last_scanned: None,
            sample_rate: None,
            file_size: None,
            file_mtime: None,
            added_at: None,
            bitrate_mode: None,
            rg_track_gain: None,
            rg_track_peak: None,
            rg_album_gain: None,
            rg_album_peak: None,
            sort_keys: SortKeys::default(),
        }
    }

    #[test]
    fn format_helpers_match_winamp() {
        assert_eq!(format_gain_db(-6.2), "-6.20 dB");
        assert_eq!(format_gain_db(3.4), "3.40 dB");
        assert_eq!(format_gain_db(0.0), "0.00 dB");
        assert_eq!(format_peak(0.988123), "0.988123");
        assert_eq!(format_peak(1.0), "1.000000");
    }

    #[test]
    fn batches_group_by_album_and_artist() {
        let tracks = vec![
            track("/a1.mp3", Some("Album X"), Some("Artist A"), Some("Artist A")),
            track("/b.mp3", Some("Other"), Some("Artist B"), Some("Artist B")),
            track("/a2.mp3", Some("album x"), Some("artist a"), None), // same album, case-insensitive
        ];
        let b = album_batches(&tracks);
        assert_eq!(b, vec![vec![0, 2], vec![1]]);
    }

    #[test]
    fn album_artist_falls_back_to_artist() {
        // Same album, no album_artist → keyed on artist; same artist groups.
        let tracks = vec![
            track("/1.mp3", Some("LP"), None, Some("Band")),
            track("/2.mp3", Some("LP"), None, Some("Band")),
            track("/3.mp3", Some("LP"), None, Some("Other")), // different artist → own batch
        ];
        let b = album_batches(&tracks);
        assert_eq!(b, vec![vec![0, 1], vec![2]]);
    }

    #[test]
    fn albumless_tracks_analyze_alone() {
        let tracks = vec![
            track("/x.mp3", None, None, Some("A")),
            track("/y.mp3", Some(""), None, Some("A")), // empty album == none
            track("/z.mp3", None, None, Some("A")),
        ];
        let b = album_batches(&tracks);
        assert_eq!(b, vec![vec![0], vec![1], vec![2]]);
    }

    // ── needs_analysis ──────────────────────────────────────────────────

    #[test]
    fn needs_analysis_when_never_analyzed() {
        let mut t = track("/x.mp3", None, None, None);
        t.rg_track_gain = None;
        assert!(needs_analysis(&t));
    }

    #[test]
    fn needs_analysis_when_file_touched_after_last_scan() {
        let mut t = track("/x.mp3", None, None, None);
        t.rg_track_gain = Some(-3.0);
        t.last_scanned = Some("2026-01-01T00:00:00Z".to_string());
        t.file_mtime = Some("2026-01-02T00:00:00Z".to_string()); // touched after scan
        assert!(needs_analysis(&t));
    }

    #[test]
    fn no_reanalysis_when_gain_present_and_file_unchanged() {
        let mut t = track("/x.mp3", None, None, None);
        t.rg_track_gain = Some(-3.0);
        t.last_scanned = Some("2026-01-02T00:00:00Z".to_string());
        t.file_mtime = Some("2026-01-01T00:00:00Z".to_string()); // scanned after the file was last touched
        assert!(!needs_analysis(&t));
    }

    // ── analyze_batch (GStreamer end-to-end) ────────────────────────────

    /// A minimal PCM WAV containing a low-amplitude sine tone rather than
    /// silence — `rganalysis` reports gain as ±infinity dB on pure silence
    /// (zero RMS), which would make the "finite" assertions below meaningless.
    /// Mirrors `write_test_wav` in `media_library/tests.rs`, but fills the
    /// data chunk with a tone instead of zeros.
    fn write_tone_wav(path: &std::path::Path, sample_rate: u32, secs: f64, freq: f64) {
        let channels: u16 = 2;
        let bytes_per_frame = channels as u32 * 2;
        let n_frames = (sample_rate as f64 * secs) as u32;
        let data_len = n_frames * bytes_per_frame;
        let byte_rate = sample_rate * bytes_per_frame;
        let block_align = channels * 2;

        let mut buf = Vec::new();
        buf.extend(b"RIFF");
        buf.extend(&(36 + data_len).to_le_bytes());
        buf.extend(b"WAVE");
        buf.extend(b"fmt ");
        buf.extend(&16u32.to_le_bytes());
        buf.extend(&1u16.to_le_bytes()); // PCM
        buf.extend(&channels.to_le_bytes());
        buf.extend(&sample_rate.to_le_bytes());
        buf.extend(&byte_rate.to_le_bytes());
        buf.extend(&block_align.to_le_bytes());
        buf.extend(&16u16.to_le_bytes()); // bits per sample
        buf.extend(b"data");
        buf.extend(&data_len.to_le_bytes());

        // Low amplitude (~25% full scale) so it's audible-loudness-finite
        // without risking clipping-related edge behavior.
        let amp = i16::MAX as f64 * 0.25;
        for n in 0..n_frames {
            let t = n as f64 / sample_rate as f64;
            let sample = (amp * (2.0 * std::f64::consts::PI * freq * t).sin()) as i16;
            buf.extend(&sample.to_le_bytes()); // left
            buf.extend(&sample.to_le_bytes()); // right
        }
        std::fs::write(path, buf).unwrap();
    }

    #[test]
    fn analyze_batch_single_file_returns_finite_result() {
        let _ = gst::init();
        if gst::ElementFactory::find("rganalysis").is_none() {
            eprintln!("skipping: rganalysis element not available");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("tone.wav");
        write_tone_wav(&p, 44100, 1.0, 440.0);

        let results = analyze_batch(&[p]).expect("analysis should succeed");
        assert_eq!(results.len(), 1);
        let r = results[0];
        assert!(r.track_gain.is_finite(), "track_gain = {}", r.track_gain);
        assert!(r.track_peak.is_finite(), "track_peak = {}", r.track_peak);
        assert!(r.album_gain.is_finite(), "album_gain = {}", r.album_gain);
        assert!(r.album_peak.is_finite(), "album_peak = {}", r.album_peak);
    }

    #[test]
    fn analyze_batch_two_files_share_one_album_gain() {
        let _ = gst::init();
        if gst::ElementFactory::find("rganalysis").is_none() {
            eprintln!("skipping: rganalysis element not available");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("a.wav");
        let p2 = dir.path().join("b.wav");
        // Different tones/lengths so the two tracks aren't identical inputs.
        write_tone_wav(&p1, 44100, 1.0, 440.0);
        write_tone_wav(&p2, 44100, 1.5, 220.0);

        let results = analyze_batch(&[p1, p2]).expect("analysis should succeed");
        assert_eq!(results.len(), 2);
        for r in &results {
            assert!(r.track_gain.is_finite());
            assert!(r.track_peak.is_finite());
            assert!(r.album_gain.is_finite());
            assert!(r.album_peak.is_finite());
        }
        // Both results are from the same batch → same album gain/peak.
        assert_eq!(results[0].album_gain, results[1].album_gain);
        assert_eq!(results[0].album_peak, results[1].album_peak);

        // Regression (concat-lump bug): each track gets its OWN analysis pass,
        // so a track's gain is independent of its batch position. Analyzing
        // the 2nd file alone must reproduce its in-batch track gain bit-for-
        // bit. Before the fix, index 1 got a neutral 0.0 fallback because
        // concat emitted only one computed gain for the whole concatenation.
        let alone = analyze_batch(&[dir.path().join("b.wav")]).expect("solo analysis");
        assert_eq!(results[1].track_gain, alone[0].track_gain);
        assert_ne!(results[1].track_gain, 0.0, "index-1 track fell back to 0.0");
    }

    #[test]
    fn mp3_write_back_roundtrips_and_preserves_other_frames() {
        use id3::{TagLike, Version};
        let dir = tempfile::tempdir().unwrap();
        let mp3 = dir.path().join("song.mp3");
        // Seed the file with a title so we can prove it survives write-back.
        std::fs::write(&mp3, b"").unwrap();
        let mut seed = id3::Tag::new();
        seed.set_title("Keep Me");
        seed.write_to_path(&mp3, Version::Id3v23).unwrap();

        let r = RgResult {
            track_gain: -6.20,
            track_peak: 0.988123,
            album_gain: -7.10,
            album_peak: 0.995,
        };
        assert_eq!(
            write_mp3_replaygain_tags(&mp3, &r).unwrap(),
            WriteBackOutcome::Written
        );

        let tag = id3::Tag::read_from_path(&mp3).unwrap();
        assert_eq!(tag.title(), Some("Keep Me"), "existing frames preserved");
        let get = |desc: &str| {
            tag.extended_texts()
                .find(|e| e.description == desc)
                .map(|e| e.value.clone())
        };
        assert_eq!(get("REPLAYGAIN_TRACK_GAIN").as_deref(), Some("-6.20 dB"));
        assert_eq!(get("REPLAYGAIN_TRACK_PEAK").as_deref(), Some("0.988123"));
        assert_eq!(get("REPLAYGAIN_ALBUM_GAIN").as_deref(), Some("-7.10 dB"));
        assert_eq!(get("REPLAYGAIN_ALBUM_PEAK").as_deref(), Some("0.995000"));

        // Re-writing replaces (no duplicate REPLAYGAIN_TRACK_GAIN frames).
        write_mp3_replaygain_tags(&mp3, &r).unwrap();
        let tag2 = id3::Tag::read_from_path(&mp3).unwrap();
        let count = tag2
            .extended_texts()
            .filter(|e| e.description == "REPLAYGAIN_TRACK_GAIN")
            .count();
        assert_eq!(count, 1, "replace, not stack");
    }

    #[test]
    fn write_back_skips_non_mp3_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let flac = dir.path().join("song.flac");
        std::fs::write(&flac, b"not really flac").unwrap();
        let before = std::fs::read(&flac).unwrap();
        let r = RgResult {
            track_gain: -6.2,
            track_peak: 0.9,
            album_gain: -6.2,
            album_peak: 0.9,
        };
        assert_eq!(
            write_mp3_replaygain_tags(&flac, &r).unwrap(),
            WriteBackOutcome::SkippedNonMp3
        );
        assert_eq!(std::fs::read(&flac).unwrap(), before, "non-MP3 left untouched");
    }
}

