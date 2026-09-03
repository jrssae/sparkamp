//! ReplayGain analysis — album batching, value formatting, and the GStreamer
//! `rganalysis` pipeline that computes track/album gain + peak. The playback
//! side (applying gain via `rgvolume`) lives in `engine.rs`; this module only
//! MEASURES and hands results to the media library / tag write-back.
//!
//! Analysis decodes whole files, so callers run it on a single background
//! worker (never per-track in parallel — decoding is CPU-bound).

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

/// Parse a ReplayGain gain string back to dB — the inverse of
/// [`format_gain_db`], but tolerant of what other taggers actually emit:
/// a `dB` suffix in any case, a leading `+`, and surrounding whitespace
/// (`-11.00 dB`, `+2.3 DB`, `-6.2`). Returns `None` for anything unparseable
/// or non-finite, so a junk tag is treated as "no value" rather than poisoning
/// the DB with NaN.
pub fn parse_gain_db(raw: &str) -> Option<f64> {
    let s = raw.trim();
    let s = s
        .strip_suffix("dB")
        .or_else(|| s.strip_suffix("DB"))
        .or_else(|| s.strip_suffix("db"))
        .or_else(|| s.strip_suffix("Db"))
        .unwrap_or(s)
        .trim();
    let s = s.strip_prefix('+').unwrap_or(s);
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Parse a ReplayGain peak string to a linear value — the inverse of
/// [`format_peak`]. Negative or non-finite values are rejected; peaks are
/// magnitudes.
pub fn parse_peak(raw: &str) -> Option<f64> {
    raw.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
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

/// Whether this build can measure ReplayGain.
///
/// Callers gate the whole feature on this before offering it, mirroring
/// `Player::rg_available` for playback — an unavailable analyser is a menu
/// item that is not there, not an action that fails.
pub fn rg_analysis_available() -> bool {
    analysis::available()
}

/// Analyze one album batch (a group of file paths sharing an album, or a
/// single album-less file). Returns one [`RgResult`] per input path, IN THE
/// SAME ORDER.
///
/// Runs synchronously on the calling thread — analysis is CPU-bound decode,
/// and callers already run it off a single background worker.
///
/// Returns an error when this build cannot analyse; callers should gate on
/// [`rg_analysis_available`] first, and this is the defensive fallback.
pub fn analyze_batch(paths: &[std::path::PathBuf]) -> anyhow::Result<Vec<RgResult>> {
    analysis::analyze_batch(paths)
}

/// Apply a hand-edited ReplayGain value from an ID3 editor: write it into the
/// file's `REPLAYGAIN_TRACK_GAIN` tag AND store it in the library, so the two
/// agree afterwards. Returns the parsed gain (`None` when cleared).
///
/// Deliberately independent of the `write_tags` setting — that governs whether
/// *automatic analysis* writes back to files, whereas this is the user
/// explicitly editing the value in front of them.
///
/// `text` is free-form (`-11.00 dB`, `-11`, `+2.3 dB`) and is normalised to the
/// standard tag format before writing; empty/whitespace clears both the frame
/// and the stored value. A container with no ReplayGain representation (WMA,
/// TTA) skips the tag write but still gets the library value, which is what
/// playback reads.
///
/// Shared by every frontend so the mac editor, GTK and the TUI cannot drift.
pub fn apply_manual_gain_edit(
    lib: Option<&crate::media_library::MediaLibrary>,
    path: &std::path::Path,
    text: &str,
) -> Result<Option<f64>, ManualGainError> {
    let trimmed = text.trim();
    let gain = if trimmed.is_empty() {
        None
    } else {
        Some(parse_gain_db(trimmed).ok_or(ManualGainError::Unparseable)?)
    };

    // Every container that has somewhere to put this gets it. A format with
    // no ReplayGain representation still gets the library value below, which
    // is what playback actually reads.
    let frame = format!("{}REPLAYGAIN_TRACK_GAIN", crate::id3_editor::TXXX_PREFIX);
    let value = gain.map(format_gain_db).unwrap_or_default();
    if crate::id3_editor::supports_frame(path, &frame) {
        crate::id3_editor::write_extra_frame(path, &frame, &value)
            .map_err(|_| ManualGainError::WriteFailed)?;
    }

    if let Some(lib) = lib {
        lib.set_track_gain_by_path(&path.to_string_lossy(), gain)
            .map_err(|_| ManualGainError::WriteFailed)?;
    }
    Ok(gain)
}

/// The stored track gain for `path`, formatted for an ID3 editor field
/// (`-11.00 dB`), or an empty string when the file isn't in the library or has
/// never been analyzed.
pub fn manual_gain_field_text(
    lib: Option<&crate::media_library::MediaLibrary>,
    path: &str,
) -> String {
    lib.and_then(|l| l.track_by_path(path).ok())
        .and_then(|t| t.rg_track_gain)
        .map(format_gain_db)
        .unwrap_or_default()
}

/// Resolve the ReplayGain value stored in the library for the file at `path`
/// and hand it to the player as the gain for its next `load()`.
///
/// The single place the DB→playback rule lives, so the frontends can't drift:
/// in album mode prefer the album gain and fall back to the track gain (a
/// single that was never part of an analyzed album has only the latter);
/// in track mode use the track gain. `None` — no library, no row, or nothing
/// analyzed — leaves the user's configured fallback in charge.
///
/// Call immediately before `player.load(...)`; `load` consumes the value.
pub fn prime_player_gain(
    player: &mut crate::engine::Player,
    lib: Option<&crate::media_library::MediaLibrary>,
    path: &str,
    album_mode: bool,
) {
    let gain = lib.and_then(|l| l.track_by_path(path).ok()).and_then(|t| {
        if album_mode {
            t.rg_album_gain.or(t.rg_track_gain)
        } else {
            t.rg_track_gain
        }
    });
    player.set_rg_db_gain(gain);
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
                    // Optional tag write-back. A container with no place for
                    // ReplayGain is skipped, not an error.
                    if write_tags {
                        if let Err(e) =
                            write_replaygain_tags(std::path::Path::new(&track.path), r)
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
    /// The four `REPLAYGAIN_*` values were written.
    Written,
    /// The file's container has nowhere to put them, so it was left exactly
    /// as it was. Not a failure: a WMA in the library is a file with no
    /// ReplayGain representation, not a broken write.
    SkippedUnsupported,
}

/// The four ReplayGain values as text, in the Winamp-compatible formats
/// (`-6.20 dB`, `0.988123`) that every reader in the wild expects.
struct RgTagValues {
    track_gain: String,
    track_peak: String,
    album_gain: String,
    album_peak: String,
}

impl RgTagValues {
    fn of(r: &RgResult) -> Self {
        Self {
            track_gain: format_gain_db(r.track_gain),
            track_peak: format_peak(r.track_peak),
            album_gain: format_gain_db(r.album_gain),
            album_peak: format_peak(r.album_peak),
        }
    }

    /// Paired with the uppercase names ID3 uses as TXXX descriptions.
    fn named(&self) -> [(&'static str, &str); 4] {
        [
            ("REPLAYGAIN_TRACK_GAIN", &self.track_gain),
            ("REPLAYGAIN_TRACK_PEAK", &self.track_peak),
            ("REPLAYGAIN_ALBUM_GAIN", &self.album_gain),
            ("REPLAYGAIN_ALBUM_PEAK", &self.album_peak),
        ]
    }

    /// Paired with lofty's own keys, which it maps to whatever the container
    /// actually uses.
    fn keyed(&self) -> [(lofty::prelude::ItemKey, &str); 4] {
        use lofty::prelude::ItemKey;
        [
            (ItemKey::ReplayGainTrackGain, &self.track_gain),
            (ItemKey::ReplayGainTrackPeak, &self.track_peak),
            (ItemKey::ReplayGainAlbumGain, &self.album_gain),
            (ItemKey::ReplayGainAlbumPeak, &self.album_peak),
        ]
    }
}

/// Write the four `REPLAYGAIN_*` values into `path`, preserving every other
/// tag already there. Re-writing replaces rather than duplicating.
///
/// Each container gets the representation its readers expect: TXXX frames in
/// an ID3 tag for MP3, Vorbis comments for FLAC, Ogg Vorbis and Opus, an
/// iTunes atom for M4A, an ID3 chunk for WAV and AIFF. A container with no
/// ReplayGain representation, or one that cannot be parsed, is left untouched
/// and reported as [`WriteBackOutcome::SkippedUnsupported`].
///
/// This used to be MP3-only, which quietly meant the files Sparkamp itself
/// produces went untagged: macOS rips to FLAC.
pub fn write_replaygain_tags(
    path: &std::path::Path,
    r: &RgResult,
) -> anyhow::Result<WriteBackOutcome> {
    let values = RgTagValues::of(r);
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let outcome = if extension == "mp3" {
        write_id3_replaygain(path, &values)?
    } else {
        write_tagged_replaygain(path, &values)?
    };

    if outcome == WriteBackOutcome::Written {
        // Suppress the watcher: this is Sparkamp's own write, not an external
        // change.
        crate::watch::register_self_write(path);
    }
    Ok(outcome)
}

/// MP3, through the `id3` crate.
///
/// MP3 keeps this stack rather than moving to lofty with everything else,
/// because it is the one [`crate::id3_editor`] already uses for every other
/// MP3 tag Sparkamp writes. Two ID3 writers on one container would mean
/// frames whose version and text encoding depend on which code path last
/// touched the file.
fn write_id3_replaygain(
    path: &std::path::Path,
    values: &RgTagValues,
) -> anyhow::Result<WriteBackOutcome> {
    use id3::frame::ExtendedText;
    use id3::{TagLike, Version};

    let mut tag = id3::Tag::read_from_path(path).unwrap_or_default();
    for (description, value) in values.named() {
        // Drop any prior frame with this description so we replace, not stack.
        tag.remove_extended_text(Some(description), None);
        tag.add_frame(ExtendedText {
            description: description.to_string(),
            value: value.to_string(),
        });
    }
    tag.write_to_path(path, Version::Id3v23)
        .map_err(|e| anyhow::anyhow!("write REPLAYGAIN tags to {}: {e}", path.display()))?;
    Ok(WriteBackOutcome::Written)
}

/// Every other container, through lofty.
///
/// Lofty owns the mapping from "ReplayGain track gain" to whatever the file
/// format spells it as, which is the whole reason to use it here: the four
/// names are the same everywhere, but where they live is not.
fn write_tagged_replaygain(
    path: &std::path::Path,
    values: &RgTagValues,
) -> anyhow::Result<WriteBackOutcome> {
    use lofty::config::WriteOptions;
    use lofty::file::TaggedFileExt;
    use lofty::prelude::TagExt;
    use lofty::tag::Tag;

    // A file lofty cannot parse is not a failed write. It is a format with
    // nowhere to put this, and saying so lets the caller carry on.
    let Ok(mut tagged) = lofty::probe::Probe::open(path).and_then(|p| p.read()) else {
        return Ok(WriteBackOutcome::SkippedUnsupported);
    };
    if tagged.primary_tag_mut().is_none() {
        let kind = tagged.file_type().primary_tag_type();
        tagged.insert_tag(Tag::new(kind));
    }
    let Some(tag) = tagged.primary_tag_mut() else {
        return Ok(WriteBackOutcome::SkippedUnsupported);
    };

    // `insert_text` refuses a key the container has no mapping for. If none of
    // the four can be represented there is nothing to save, and saving anyway
    // would rewrite the file to no purpose.
    let mut wrote_any = false;
    for (key, value) in values.keyed() {
        wrote_any |= tag.insert_text(key, value.to_string());
    }
    if !wrote_any {
        return Ok(WriteBackOutcome::SkippedUnsupported);
    }

    tag.save_to_path(path, WriteOptions::default())
        .map_err(|e| anyhow::anyhow!("write REPLAYGAIN tags to {}: {e}", path.display()))?;
    Ok(WriteBackOutcome::Written)
}

/// Progress snapshot for [`analyze_and_store`], reported after each batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgJobProgress {
    pub done: usize,
    pub total: usize,
}

/// Why a manual ReplayGain edit was rejected.
#[derive(Debug, PartialEq, Eq)]
pub enum ManualGainError {
    /// Non-empty text that isn't a gain value — nothing was written.
    Unparseable,
    /// The tag or database write failed.
    WriteFailed,
}

/// Measuring loudness, which is the one part of ReplayGain that needs an
/// audio stack.
///
/// Everything above this line — the number formats, the tag parsing, the
/// album batching, the manual edits — is arithmetic and text, and is shared.
pub mod coefficients;
pub mod rg1;

mod analysis {
    /// GStreamer's `rganalysis`, which implements ReplayGain 1.0.
    #[cfg(not(target_os = "macos"))]
    mod imp {
        use super::super::RgResult;
        use gstreamer as gst;
        use gstreamer::prelude::*;

        /// `true` when the GStreamer `rganalysis` element is installed. Callers
        /// (library actions / auto-analyze) should gate the whole feature on this
        /// before offering it, mirroring `Player::rg_available` for playback.
        pub fn available() -> bool {
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

    }

    /// AVFoundation decodes, [`super::rg1`] measures.
    ///
    /// `rganalysis` is a GStreamer element and the App Store build ships no
    /// GStreamer, so the algorithm is implemented here instead. It is the same
    /// ReplayGain 1.0 the element implements — verified against it directly,
    /// see `rg1::tests::matches_rganalysis`.
    ///
    /// Only the decode is macOS's. The measuring lives in `rg1`, compiled and
    /// tested on every platform so it cannot rot on the one that does not use
    /// it.
    #[cfg(target_os = "macos")]
    mod imp {
        use std::path::Path;

        use objc2::AllocAnyThread;
        use objc2_avf_audio::{AVAudioFile, AVAudioPCMBuffer};
        use objc2_foundation::{NSString, NSURL};

        use super::super::RgResult;
        use super::super::rg1::Analyzer;

        pub fn available() -> bool {
            true
        }

        pub fn analyze_batch(paths: &[std::path::PathBuf]) -> anyhow::Result<Vec<RgResult>> {
            if paths.is_empty() {
                return Ok(Vec::new());
            }
            // One analyzer for the whole batch. Track gains come out per file;
            // the album histogram keeps accumulating underneath, which is what
            // makes the album figure a measurement of the album rather than an
            // average of its tracks.
            let mut analyzer: Option<Analyzer> = None;
            let mut tracks: Vec<(f64, f64)> = Vec::with_capacity(paths.len());

            for path in paths {
                let rate = decode_into(path, &mut analyzer)?;
                let a = analyzer
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("{} decoded to nothing", path.display()))?;
                let gain = a.finish_track().ok_or_else(|| {
                    anyhow::anyhow!(
                        "{} is shorter than one 50 ms window at {rate} Hz",
                        path.display()
                    )
                })?;
                tracks.push((gain.gain_db, gain.peak));
            }

            let album = analyzer
                .as_ref()
                .and_then(Analyzer::album)
                .ok_or_else(|| anyhow::anyhow!("nothing in the batch could be measured"))?;

            Ok(tracks
                .into_iter()
                .map(|(track_gain, track_peak)| RgResult {
                    track_gain,
                    track_peak,
                    album_gain: album.gain_db,
                    album_peak: album.peak,
                })
                .collect())
        }

        /// Decode one file and feed every frame to the analyzer.
        ///
        /// The analyzer is created on the first file, because its filter
        /// coefficients depend on the sample rate. A batch whose files
        /// disagree about rate is refused rather than measured wrongly — the
        /// histogram would mix windows filtered by different curves, and the
        /// album figure would be meaningless in a way nothing downstream could
        /// detect.
        fn decode_into(path: &Path, analyzer: &mut Option<Analyzer>) -> anyhow::Result<u32> {
            let path_str = path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("path is not UTF-8: {}", path.display()))?;
            if path_str.contains('\0') {
                anyhow::bail!("path contains a NUL: {}", path.display());
            }
            let url = NSURL::fileURLWithPath(&NSString::from_str(path_str));
            // SAFETY: a live file URL; failures come back as `Result`.
            let file = unsafe { AVAudioFile::initForReading_error(AVAudioFile::alloc(), &url) }
                .map_err(|e| anyhow::anyhow!("could not read {}: {e:?}", path.display()))?;
            // SAFETY: `file` is live for the length of this function.
            let (format, rate, channels) = unsafe {
                let f = file.processingFormat();
                (f.clone(), f.sampleRate(), f.channelCount())
            };
            let rate = rate as u32;
            match analyzer {
                Some(_) => {}
                None => {
                    *analyzer = Some(Analyzer::new(rate).ok_or_else(|| {
                        anyhow::anyhow!("ReplayGain defines no filter for {rate} Hz")
                    })?)
                }
            }

            const CHUNK: u32 = 1 << 15;
            loop {
                // SAFETY: a live format and a capacity; the buffer comes back
                // owned or None.
                let buffer = unsafe {
                    AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
                        AVAudioPCMBuffer::alloc(),
                        &format,
                        CHUNK,
                    )
                }
                .ok_or_else(|| anyhow::anyhow!("could not allocate a decode buffer"))?;
                // `readIntoBuffer` throws at the end of the file rather than
                // returning an empty buffer, so an error here is "finished".
                // SAFETY: both objects are live.
                if unsafe { file.readIntoBuffer_error(&buffer) }.is_err() {
                    break;
                }
                // SAFETY: `buffer` is live and in a 32-bit float format, which
                // is what `processingFormat` always is.
                let frames = unsafe { buffer.frameLength() } as usize;
                if frames == 0 {
                    break;
                }
                // SAFETY: the channel pointers are valid for `frames` samples
                // each while the buffer lives.
                let data = unsafe { buffer.floatChannelData() };
                if data.is_null() {
                    anyhow::bail!("{} decoded to no channel data", path.display());
                }
                let mut pcm: Vec<[f64; 2]> = Vec::with_capacity(frames);
                // SAFETY: `data` points at `channels` pointers, each to
                // `frames` floats.
                unsafe {
                    let left = (*data).as_ptr();
                    let right = if channels > 1 { (*data.add(1)).as_ptr() } else { left };
                    for i in 0..frames {
                        pcm.push([f64::from(*left.add(i)), f64::from(*right.add(i))]);
                    }
                }
                if let Some(a) = analyzer.as_mut() {
                    a.feed(&pcm);
                }
            }
            Ok(rate)
        }
    }

    pub use imp::{analyze_batch, available};
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

    // ── parse_gain_db / parse_peak ──────────────────────────────────────

    #[test]
    fn parse_gain_accepts_what_other_taggers_write() {
        assert_eq!(parse_gain_db("-11.00 dB"), Some(-11.0));
        assert_eq!(parse_gain_db("+2.3 DB"), Some(2.3));
        assert_eq!(parse_gain_db("  -6.2  "), Some(-6.2));
        assert_eq!(parse_gain_db("0.00 db"), Some(0.0));
    }

    #[test]
    fn parse_gain_rejects_junk_rather_than_poisoning_the_db() {
        assert_eq!(parse_gain_db(""), None);
        assert_eq!(parse_gain_db("dB"), None);
        assert_eq!(parse_gain_db("loud"), None);
        assert_eq!(parse_gain_db("NaN dB"), None);
        assert_eq!(parse_gain_db("inf"), None);
    }

    #[test]
    fn parse_peak_rejects_negatives_and_junk() {
        assert_eq!(parse_peak("0.988123"), Some(0.988123));
        assert_eq!(parse_peak(" 1.0 "), Some(1.0));
        assert_eq!(parse_peak("-0.5"), None);
        assert_eq!(parse_peak("x"), None);
    }

    #[test]
    fn gain_and_peak_round_trip_through_our_own_formatters() {
        assert_eq!(parse_gain_db(&format_gain_db(-6.2)), Some(-6.20));
        assert_eq!(parse_peak(&format_peak(0.988123)), Some(0.988123));
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

    /// LIVE: this platform's whole analysis path against `rganalysis`, over
    /// real music.
    /// `SPARKAMP_RG_DIR=~/Music cargo test --lib live_rg_matches_rganalysis -- --ignored --nocapture`
    ///
    /// This measures **algorithm plus decoder**, which is a different question
    /// from `rg1::tests::matches_rganalysis`. That one uses lossless audio to
    /// isolate the maths and matches exactly. This one runs on whatever is in
    /// a real library — mostly MP3 — where the two decoders are not bit-exact
    /// with each other, so a small spread is expected and is not a defect in
    /// either.
    ///
    /// It reports the distribution rather than asserting a tight bound,
    /// because the honest tolerance is whatever this measures.
    #[test]
    #[ignore]
    fn live_rg_matches_rganalysis() {
        let Some(dir) = std::env::var_os("SPARKAMP_RG_DIR") else {
            println!("set SPARKAMP_RG_DIR to a directory of audio files");
            return;
        };
        if !rg_analysis_available() {
            println!("no analyser in this build — skipping");
            return;
        }
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if crate::model::is_audio_file(&p) {
                    out.push(p);
                }
            }
        }
        walk(std::path::Path::new(&dir), &mut files);
        files.sort();
        let limit: usize = std::env::var("SPARKAMP_RG_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(40);
        files.truncate(limit);
        if files.is_empty() {
            println!("no audio files found — skipping");
            return;
        }
        println!("comparing {} file(s)", files.len());

        let mut deltas: Vec<f64> = Vec::new();
        let mut peak_deltas: Vec<f64> = Vec::new();
        let mut skipped = 0usize;
        for path in &files {
            let Some((ref_gain, ref_peak)) = rganalysis_reference(path) else {
                skipped += 1;
                continue;
            };
            let mine = match analyze_batch(std::slice::from_ref(path)) {
                Ok(r) if !r.is_empty() => r[0],
                _ => {
                    skipped += 1;
                    continue;
                }
            };
            let d = mine.track_gain - ref_gain;
            let pd = mine.track_peak - ref_peak;
            if d.abs() > 0.5 {
                println!(
                    "  {:>8.3} dB  {:.6} peak  {}",
                    d,
                    pd,
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
            }
            deltas.push(d);
            peak_deltas.push(pd);
        }
        if deltas.is_empty() {
            println!("nothing comparable ({skipped} skipped) — skipping");
            return;
        }
        deltas.sort_by(|a, b| a.abs().partial_cmp(&b.abs()).unwrap());
        peak_deltas.sort_by(|a, b| a.abs().partial_cmp(&b.abs()).unwrap());
        let pct = |v: &[f64], p: f64| v[((v.len() as f64 - 1.0) * p).round() as usize].abs();
        println!(
            "gain |delta| dB: median {:.4}  p90 {:.4}  max {:.4}   ({} compared, {skipped} skipped)",
            pct(&deltas, 0.5),
            pct(&deltas, 0.9),
            pct(&deltas, 1.0),
            deltas.len()
        );
        println!(
            "peak |delta|:    median {:.6}  p90 {:.6}  max {:.6}",
            pct(&peak_deltas, 0.5),
            pct(&peak_deltas, 0.9),
            pct(&peak_deltas, 1.0)
        );
        // Loose on purpose. A whole dB apart would mean the algorithm is
        // wrong; hundredths mean the decoders disagree, which they do.
        assert!(
            pct(&deltas, 0.9) < 0.5,
            "nine files in ten should agree within half a dB"
        );
    }

    /// LIVE: album gain across several real tracks, measured as one stream.
    /// `SPARKAMP_RG_DIR=<dir> cargo test --lib live_rg_album_across_real_tracks -- --ignored --nocapture`
    ///
    /// The synthetic test pins that album gain is not the mean of the track
    /// gains. This checks the same property holds on real material, where the
    /// tracks are mastered close together and the two answers are much nearer
    /// each other — which is the case where a wrong implementation would look
    /// right.
    #[test]
    #[ignore]
    fn live_rg_album_across_real_tracks() {
        let Some(dir) = std::env::var_os("SPARKAMP_RG_DIR") else {
            println!("set SPARKAMP_RG_DIR to a directory of audio files");
            return;
        };
        if !rg_analysis_available() {
            println!("no analyser in this build — skipping");
            return;
        }
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| crate::model::is_audio_file(p))
            .collect();
        files.sort();
        files.truncate(3);
        if files.len() < 2 {
            println!("need at least two tracks — skipping");
            return;
        }
        let results = analyze_batch(&files).expect("analysis should succeed");
        assert_eq!(results.len(), files.len(), "one result per input, in order");

        let tracks: Vec<f64> = results.iter().map(|r| r.track_gain).collect();
        let album = results[0].album_gain;
        let mean = tracks.iter().sum::<f64>() / tracks.len() as f64;
        for (f, r) in files.iter().zip(&results) {
            println!(
                "  {:>7.2} dB  peak {:.6}  {}",
                r.track_gain,
                r.track_peak,
                f.file_name().unwrap_or_default().to_string_lossy()
            );
        }
        println!("album {album:.2} dB (mean of tracks would be {mean:.2})");

        // Every result carries the same album figure — it is a property of the
        // batch, not of a track.
        assert!(
            results.iter().all(|r| (r.album_gain - album).abs() < 1e-9),
            "every track must report the same album gain"
        );
        assert!(
            results.iter().all(|r| r.album_peak >= r.track_peak - 1e-9),
            "album peak must be at least every track's peak"
        );
        // It must land inside the range of the track gains. Outside would mean
        // the histogram is not accumulating what it should.
        let lo = tracks.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = tracks.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(
            album >= lo - 0.5 && album <= hi + 0.5,
            "album {album:.2} sits outside the track range {lo:.2}..{hi:.2}"
        );
    }

    /// `rganalysis`'s answer for one file, via `gst-launch-1.0`.
    ///
    /// A subprocess because this platform no longer links GStreamer at all —
    /// which is the entire reason `rg1` exists. Test-only.
    fn rganalysis_reference(path: &std::path::Path) -> Option<(f64, f64)> {
        let out = std::process::Command::new("gst-launch-1.0")
            .args([
                "-t",
                "filesrc",
                &format!("location={}", path.display()),
                "!",
                "decodebin",
                "!",
                "audioconvert",
                "!",
                "rganalysis",
                "!",
                "fakesink",
            ])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let field = |name: &str| -> Option<f64> {
            text.lines()
                .find(|l| l.contains(name))?
                .rsplit(':')
                .next()?
                .trim()
                .parse()
                .ok()
        };
        Some((
            field("replaygain track gain")?,
            field("replaygain track peak")?,
        ))
    }

    #[test]
    fn analyze_batch_single_file_returns_finite_result() {
        // Through the public gate, which is what a caller asks — and which
        // answers `false` on a platform with no analyser rather than needing
        // this test to know why.
        if !rg_analysis_available() {
            eprintln!("skipping: ReplayGain analysis is not available in this build");
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
        // Through the public gate, which is what a caller asks — and which
        // answers `false` on a platform with no analyser rather than needing
        // this test to know why.
        if !rg_analysis_available() {
            eprintln!("skipping: ReplayGain analysis is not available in this build");
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
            write_replaygain_tags(&mp3, &r).unwrap(),
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
        write_replaygain_tags(&mp3, &r).unwrap();
        let tag2 = id3::Tag::read_from_path(&mp3).unwrap();
        let count = tag2
            .extended_texts()
            .filter(|e| e.description == "REPLAYGAIN_TRACK_GAIN")
            .count();
        assert_eq!(count, 1, "replace, not stack");
    }

    /// A valid WAV, so lofty has a real container to write into. The
    /// analyzer's own fixtures are built the same way.
    fn wav_bytes(frames: usize) -> Vec<u8> {
        let len = frames * 4;
        let mut buf = Vec::with_capacity(44 + len);
        buf.extend(b"RIFF");
        buf.extend(&((36 + len) as u32).to_le_bytes());
        buf.extend(b"WAVE");
        buf.extend(b"fmt ");
        buf.extend(&16u32.to_le_bytes());
        buf.extend(&1u16.to_le_bytes());
        buf.extend(&2u16.to_le_bytes());
        buf.extend(&44_100u32.to_le_bytes());
        buf.extend(&176_400u32.to_le_bytes());
        buf.extend(&4u16.to_le_bytes());
        buf.extend(&16u16.to_le_bytes());
        buf.extend(b"data");
        buf.extend(&(len as u32).to_le_bytes());
        for i in 0..frames {
            let v = ((i as f32 * 0.05).sin() * 8000.0) as i16;
            buf.extend(&v.to_le_bytes());
            buf.extend(&v.to_le_bytes());
        }
        buf
    }

    /// A FLAC carrying nothing but its STREAMINFO block. Enough for a tag
    /// writer, which is all this exercises.
    fn minimal_flac_bytes() -> Vec<u8> {
        let mut f = b"fLaC".to_vec();
        f.push(0x80); // last-metadata-block flag, type 0 (STREAMINFO)
        f.extend_from_slice(&[0, 0, 34]);
        f.extend_from_slice(&[0u8; 34]);
        f
    }

    fn sample_result() -> RgResult {
        RgResult {
            track_gain: -6.20,
            track_peak: 0.988123,
            album_gain: -7.10,
            album_peak: 0.995,
        }
    }

    /// FLAC is the one that matters most: macOS rips to it, so before this
    /// the files Sparkamp produced were the files it could not tag.
    ///
    /// Written through lofty and read back through `metaflac`, deliberately.
    /// A round-trip through one library would pass even if the values were
    /// stored somewhere no other reader looks.
    #[test]
    fn flac_write_back_is_readable_as_vorbis_comments() {
        let dir = tempfile::tempdir().unwrap();
        let flac = dir.path().join("song.flac");
        std::fs::write(&flac, minimal_flac_bytes()).unwrap();

        assert_eq!(
            write_replaygain_tags(&flac, &sample_result()).unwrap(),
            WriteBackOutcome::Written
        );

        let tag = metaflac::Tag::read_from_path(&flac).expect("readable FLAC");
        let comments = tag.vorbis_comments().expect("a Vorbis comment block");
        let one = |k: &str| comments.get(k).and_then(|v| v.first()).cloned();
        assert_eq!(one("REPLAYGAIN_TRACK_GAIN").as_deref(), Some("-6.20 dB"));
        assert_eq!(one("REPLAYGAIN_TRACK_PEAK").as_deref(), Some("0.988123"));
        assert_eq!(one("REPLAYGAIN_ALBUM_GAIN").as_deref(), Some("-7.10 dB"));
        assert_eq!(one("REPLAYGAIN_ALBUM_PEAK").as_deref(), Some("0.995000"));

        // Re-writing replaces rather than stacking, the same rule the MP3
        // path follows.
        write_replaygain_tags(&flac, &sample_result()).unwrap();
        let again = metaflac::Tag::read_from_path(&flac).unwrap();
        assert_eq!(
            again
                .vorbis_comments()
                .unwrap()
                .get("REPLAYGAIN_TRACK_GAIN")
                .map(|v| v.len()),
            Some(1),
            "replace, not stack"
        );
    }

    /// WAV keeps its ReplayGain in an ID3 chunk. Written through lofty, read
    /// back through `id3`, so this is a second cross-library check on a
    /// container that spells things differently again.
    #[test]
    fn wav_write_back_is_readable_as_id3() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("song.wav");
        std::fs::write(&wav, wav_bytes(4_410)).unwrap();

        assert_eq!(
            write_replaygain_tags(&wav, &sample_result()).unwrap(),
            WriteBackOutcome::Written
        );

        let tag = id3::Tag::read_from_path(&wav).expect("an ID3 chunk");
        let gain = tag
            .extended_texts()
            .find(|e| e.description == "REPLAYGAIN_TRACK_GAIN")
            .map(|e| e.value.clone());
        assert_eq!(gain.as_deref(), Some("-6.20 dB"));
    }

    /// A container with no ReplayGain representation is left byte-for-byte
    /// alone, and says so rather than reporting a failure.
    #[test]
    fn unsupported_container_is_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let wma = dir.path().join("song.wma");
        std::fs::write(&wma, b"not really a media file").unwrap();
        let before = std::fs::read(&wma).unwrap();

        assert_eq!(
            write_replaygain_tags(&wma, &sample_result()).unwrap(),
            WriteBackOutcome::SkippedUnsupported
        );
        assert_eq!(
            std::fs::read(&wma).unwrap(),
            before,
            "an unwritable container is not rewritten"
        );
    }
}
