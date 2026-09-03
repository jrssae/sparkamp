//! xmcd (CDDB database entry) parse + build.
//!
//! The format is `KEY=value` lines with `#` comments; a repeated key
//! continues its value (long titles wrap across lines). Parsing collects
//! `DISCID`, `DTITLE` ("artist / album"), `DYEAR`, `DGENRE`, `TTITLEn`,
//! `EXTD`, `EXTTn`. Building emits the same shape — used by the Phase-4
//! submission — including the offset/length comment header gnudb validates.

use super::{discid, toc, DiscToc};
use serde::{Deserialize, Serialize};

/// One parsed (or to-be-submitted) database entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct XmcdEntry {
    pub discid: String,
    pub artist: String,
    pub album: String,
    pub year: String,
    pub genre: String,
    /// Track titles in track order (index 0 = track 1).
    pub track_titles: Vec<String>,
    /// Disc-level extended data (liner notes).
    pub extd: String,
    /// Per-track extended data, same indexing as `track_titles`.
    pub extt: Vec<String>,
    /// The entry's revision from the `# Revision:` header comment (0 when
    /// absent). A submission updating an existing entry must send the old
    /// revision + 1 or the server rejects it as stale.
    #[serde(default)]
    pub revision: u32,
}

/// Parse an xmcd body (as returned by `cddb read`, header/terminator already
/// stripped). Returns `None` when the text has no `DISCID`/`DTITLE` at all.
pub fn parse(text: &str) -> Option<XmcdEntry> {
    // Repeated keys continue the value, so accumulate strings per key and
    // per track index.
    let mut discid_val = String::new();
    let mut dtitle = String::new();
    let mut year = String::new();
    let mut genre = String::new();
    let mut extd = String::new();
    let mut titles: Vec<(u32, String)> = Vec::new();
    let mut extts: Vec<(u32, String)> = Vec::new();
    let mut revision: u32 = 0;

    for raw in text.lines() {
        let line = raw.trim_end();
        if line.starts_with('#') || line.is_empty() {
            // The revision lives in a comment: "# Revision: 3".
            if let Some(rest) = line.trim_start_matches('#').trim().strip_prefix("Revision:") {
                if let Ok(r) = rest.trim().parse() {
                    revision = r;
                }
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "DISCID" => {
                // Entries can list several comma-separated ids; keep the first.
                if discid_val.is_empty() {
                    discid_val = value.split(',').next().unwrap_or("").trim().to_string();
                }
            }
            "DTITLE" => dtitle.push_str(value),
            "DYEAR" => year.push_str(value),
            "DGENRE" => genre.push_str(value),
            "EXTD" => extd.push_str(value),
            _ => {
                if let Some(n) = key.strip_prefix("TTITLE").and_then(|n| n.parse().ok()) {
                    append_indexed(&mut titles, n, value);
                } else if let Some(n) = key.strip_prefix("EXTT").and_then(|n| n.parse().ok()) {
                    append_indexed(&mut extts, n, value);
                }
            }
        }
    }

    if discid_val.is_empty() && dtitle.is_empty() {
        return None;
    }

    // "Artist / Album" — the separator is " / " per spec; a title without it
    // is both artist and album (self-titled convention).
    let (artist, album) = match dtitle.split_once(" / ") {
        Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
        None => (dtitle.trim().to_string(), dtitle.trim().to_string()),
    };

    Some(XmcdEntry {
        discid: discid_val,
        artist,
        album,
        year: year.trim().to_string(),
        genre: genre.trim().to_string(),
        track_titles: to_dense(titles),
        extd,
        extt: to_dense(extts),
        revision,
    })
}

/// Check an entry against gnudb's submission rules: disc artist + album set,
/// **every** track genuinely titled (the "Track N" placeholders don't count).
/// Returns the user-facing reason when not submittable.
pub fn validate_for_submit(entry: &XmcdEntry, disc_toc: &DiscToc) -> Result<(), String> {
    if entry.artist.trim().is_empty() {
        return Err("Disc artist is empty".to_string());
    }
    if entry.album.trim().is_empty() {
        return Err("Album title is empty".to_string());
    }
    let n = disc_toc.tracks.len();
    let mut untitled: Vec<String> = Vec::new();
    for i in 0..n {
        let title = entry.track_titles.get(i).map(String::as_str).unwrap_or("");
        let placeholder = format!("Track {}", i + 1);
        if title.trim().is_empty() || title.trim() == placeholder {
            untitled.push((i + 1).to_string());
        }
    }
    if !untitled.is_empty() {
        return Err(format!(
            "Track{} {} still untitled",
            if untitled.len() == 1 { "" } else { "s" },
            untitled.join(", ")
        ));
    }
    Ok(())
}

fn append_indexed(store: &mut Vec<(u32, String)>, n: u32, value: &str) {
    if let Some((_, existing)) = store.iter_mut().find(|(i, _)| *i == n) {
        existing.push_str(value);
    } else {
        store.push((n, value.to_string()));
    }
}

/// Indexed pairs → dense vec ordered by index (missing indices become "").
fn to_dense(mut pairs: Vec<(u32, String)>) -> Vec<String> {
    pairs.sort_by_key(|(i, _)| *i);
    let len = pairs.last().map(|(i, _)| *i as usize + 1).unwrap_or(0);
    let mut out = vec![String::new(); len];
    for (i, v) in pairs {
        out[i as usize] = v;
    }
    out
}

/// Build a submission-ready xmcd body for this entry + TOC: offset/length
/// comment header (gnudb validates it against the DISCID), the entry fields,
/// and one TTITLE/EXTT per track. `revision` is 0 for a new entry and the
/// previous revision + 1 for an update.
// Consumed by the gnudb submission flow (Phase 4); round-trip-tested now so
// the writer can't rot before then.
#[allow(dead_code)]
pub fn build(entry: &XmcdEntry, disc_toc: &DiscToc, revision: u32) -> String {
    let mut out = String::new();
    out.push_str("# xmcd\n#\n# Track frame offsets:\n");
    for t in &disc_toc.tracks {
        out.push_str(&format!("#\t{}\n", t.start_frame));
    }
    // The howto's required comment set: offsets, length, revision, and BOTH
    // "Processed by" and "Submitted via" lines.
    out.push_str(&format!(
        "#\n# Disc length: {} seconds\n#\n# Revision: {revision}\n# Processed by: Sparkamp {ver}\n# Submitted via: Sparkamp {ver}\n#\n",
        disc_toc.leadout_frame / 75,
        ver = env!("CARGO_PKG_VERSION"),
    ));
    out.push_str(&format!("DISCID={}\n", discid::freedb_discid(disc_toc)));
    out.push_str(&format!("DTITLE={} / {}\n", entry.artist, entry.album));
    out.push_str(&format!("DYEAR={}\n", entry.year));
    out.push_str(&format!("DGENRE={}\n", entry.genre));
    for (i, _) in disc_toc.tracks.iter().enumerate() {
        let title = entry.track_titles.get(i).map(String::as_str).unwrap_or("");
        out.push_str(&format!("TTITLE{i}={title}\n"));
    }
    out.push_str(&format!("EXTD={}\n", entry.extd));
    for (i, _) in disc_toc.tracks.iter().enumerate() {
        let extt = entry.extt.get(i).map(String::as_str).unwrap_or("");
        out.push_str(&format!("EXTT{i}={extt}\n"));
    }
    out.push_str("PLAYORDER=\n");
    out
}

/// Total disc seconds — re-exported shape check so `build` and the query args
/// can never drift apart. (Kept private; the public math lives in `toc`.)
#[allow(dead_code)]
fn total_secs(disc_toc: &DiscToc) -> u32 {
    toc::total_secs(disc_toc)
}

impl XmcdEntry {
    /// Combine two descriptions of the same disc, field by field.
    ///
    /// A disc can describe itself twice over — CD-TEXT burned into the
    /// lead-in, and a gnudb lookup — and the two are not redundant. gnudb
    /// carries genre and year, which CD-TEXT has no field for; CD-TEXT is on
    /// the disc, so it is right even for a pressing gnudb has never seen. Any
    /// scheme that picks one source wholesale throws away whatever the other
    /// knew.
    ///
    /// `self` wins where both have something; `other` fills every gap,
    /// including per-track, because a partial track list is common in both
    /// sources and a blank title is worse than a title from the other one.
    ///
    /// Empty is the only test for "absent". Neither source distinguishes an
    /// unset field from a blank one, so neither can this.
    pub fn merged_with(&self, other: &XmcdEntry) -> XmcdEntry {
        fn pick(mine: &str, theirs: &str) -> String {
            if mine.trim().is_empty() {
                theirs.to_string()
            } else {
                mine.to_string()
            }
        }
        let tracks = self.track_titles.len().max(other.track_titles.len());
        XmcdEntry {
            // The disc id is an identity, not a description: taking one from a
            // source that had none would claim a match that was never made.
            discid: pick(&self.discid, &other.discid),
            artist: pick(&self.artist, &other.artist),
            album: pick(&self.album, &other.album),
            year: pick(&self.year, &other.year),
            genre: pick(&self.genre, &other.genre),
            track_titles: (0..tracks)
                .map(|i| {
                    pick(
                        self.track_titles.get(i).map(String::as_str).unwrap_or(""),
                        other.track_titles.get(i).map(String::as_str).unwrap_or(""),
                    )
                })
                .collect(),
            extd: pick(&self.extd, &other.extd),
            // Trailing blanks are trimmed rather than padded out to the
            // track count. Extended data is sparse by nature — most discs
            // have none — and padding turns "no liner notes" into a list of
            // empty ones, which is a different value that reads the same.
            extt: {
                let mut extt: Vec<String> = (0..tracks)
                    .map(|i| {
                        pick(
                            self.extt.get(i).map(String::as_str).unwrap_or(""),
                            other.extt.get(i).map(String::as_str).unwrap_or(""),
                        )
                    })
                    .collect();
                while extt.last().is_some_and(|e| e.is_empty()) {
                    extt.pop();
                }
                extt
            },
            // A revision belongs to the gnudb entry it came from. Merging two
            // would produce a number that identifies neither, and submitting
            // it back would be rejected as stale.
            revision: self.revision.max(other.revision),
        }
    }

    /// Whether this describes the disc at all, as opposed to being the empty
    /// shell a failed lookup returns.
    // Nothing in the app reads this; the tests below are its only callers.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.artist.trim().is_empty()
            && self.album.trim().is_empty()
            && self.track_titles.iter().all(|t| t.trim().is_empty())
    }
}

#[cfg(test)]
mod tests {

    /// gnudb first, CD-TEXT filling the gaps — the rule the rip window is
    /// prepopulated by. Each source knows something the other does not: gnudb
    /// carries genre and year, which CD-TEXT has no field for, and the disc
    /// carries titles for pressings gnudb has never seen.
    #[test]
    fn merge_prefers_the_primary_and_fills_from_the_secondary() {
        let gnudb = XmcdEntry {
            discid: "abc".into(),
            artist: "Gnudb Artist".into(),
            album: String::new(),
            year: "1994".into(),
            genre: "Rock".into(),
            track_titles: vec!["One".into(), String::new()],
            ..XmcdEntry::default()
        };
        let cdtext = XmcdEntry {
            discid: String::new(),
            artist: "Disc Artist".into(),
            album: "Disc Album".into(),
            year: String::new(),
            genre: String::new(),
            track_titles: vec!["Disc One".into(), "Disc Two".into(), "Disc Three".into()],
            ..XmcdEntry::default()
        };
        let merged = gnudb.merged_with(&cdtext);
        assert_eq!(merged.artist, "Gnudb Artist", "the primary wins where it has a value");
        assert_eq!(merged.album, "Disc Album", "the secondary fills a gap");
        assert_eq!(merged.year, "1994", "only gnudb carries a year");
        assert_eq!(merged.genre, "Rock", "only gnudb carries a genre");
        assert_eq!(
            merged.track_titles,
            vec!["One", "Disc Two", "Disc Three"],
            "per track, and the longer list survives"
        );
        assert_eq!(merged.discid, "abc");
    }

    /// A whitespace-only field is absent. Both sources produce them, and a
    /// title of " " shows up in a file browser as a track with no name.
    #[test]
    fn merge_treats_blank_as_absent() {
        let a = XmcdEntry {
            artist: "   ".into(),
            track_titles: vec!["  ".into()],
            ..XmcdEntry::default()
        };
        let b = XmcdEntry {
            artist: "Real".into(),
            track_titles: vec!["Real Title".into()],
            ..XmcdEntry::default()
        };
        let merged = a.merged_with(&b);
        assert_eq!(merged.artist, "Real");
        assert_eq!(merged.track_titles, vec!["Real Title"]);
    }

    /// Merging with nothing must change nothing — the case where one source
    /// simply was not available.
    #[test]
    fn merge_with_an_empty_entry_is_the_identity() {
        let a = XmcdEntry {
            artist: "A".into(),
            album: "B".into(),
            track_titles: vec!["T".into()],
            ..XmcdEntry::default()
        };
        assert_eq!(a.merged_with(&XmcdEntry::default()), a);
        assert!(XmcdEntry::default().is_empty());
        assert!(!a.is_empty());
    }
    use super::*;
    use crate::disc::TocTrack;

    fn sample_toc() -> DiscToc {
        DiscToc {
            tracks: (0..3)
                .map(|i| TocTrack {
                    number: i as u8 + 1,
                    start_frame: 150 + i * 15000,
                    is_audio: true,
                })
                .collect(),
            leadout_frame: 45000,
        }
    }

    const SAMPLE: &str = "\
# xmcd
#
# Track frame offsets:
#\t150
#\t15150
#\t30150
#
# Disc length: 600 seconds
#
DISCID=08025603
DTITLE=The Artists / The Album
DYEAR=1997
DGENRE=Rock
TTITLE0=Opening Song
TTITLE1=Middle Song
TTITLE2=Closer With A Very Long Name Th
TTITLE2=at Wraps Across Lines
EXTD=Liner notes here
EXTT0=
EXTT1=
EXTT2=
PLAYORDER=";

    #[test]
    fn parses_fields_and_wrapped_titles() {
        let e = parse(SAMPLE).expect("entry");
        assert_eq!(e.discid, "08025603");
        assert_eq!(e.artist, "The Artists");
        assert_eq!(e.album, "The Album");
        assert_eq!(e.year, "1997");
        assert_eq!(e.genre, "Rock");
        assert_eq!(e.track_titles.len(), 3);
        assert_eq!(e.track_titles[0], "Opening Song");
        assert_eq!(
            e.track_titles[2],
            "Closer With A Very Long Name That Wraps Across Lines"
        );
        assert_eq!(e.extd, "Liner notes here");
    }

    #[test]
    fn self_titled_dtitle_without_separator() {
        let e = parse("DISCID=00000003\nDTITLE=Selftitled\n").expect("entry");
        assert_eq!(e.artist, "Selftitled");
        assert_eq!(e.album, "Selftitled");
    }

    #[test]
    fn rejects_empty_body() {
        assert!(parse("# xmcd\n#\n").is_none());
    }

    #[test]
    fn parses_revision_comment() {
        let e = parse("# Revision: 7\nDISCID=00000003\nDTITLE=A / B\n").expect("entry");
        assert_eq!(e.revision, 7);
        // Absent → 0.
        let e = parse("DISCID=00000003\nDTITLE=A / B\n").expect("entry");
        assert_eq!(e.revision, 0);
    }

    #[test]
    fn validate_rejects_placeholders_and_blanks() {
        let toc = sample_toc();
        let mut e = XmcdEntry {
            artist: "A".into(),
            album: "B".into(),
            track_titles: vec!["One".into(), "Track 2".into(), String::new()],
            ..XmcdEntry::default()
        };
        let err = validate_for_submit(&e, &toc).unwrap_err();
        assert!(err.contains("2, 3"), "{err}");
        e.track_titles = vec!["One".into(), "Two".into(), "Three".into()];
        assert!(validate_for_submit(&e, &toc).is_ok());
        e.artist.clear();
        assert!(validate_for_submit(&e, &toc).is_err());
    }

    #[test]
    fn build_parse_round_trip() {
        let entry = XmcdEntry {
            discid: String::new(), // build derives it from the TOC
            artist: "The Artists".into(),
            album: "The Album".into(),
            year: "1997".into(),
            genre: "Rock".into(),
            track_titles: vec!["One".into(), "Two".into(), "Three".into()],
            extd: "notes".into(),
            extt: vec![String::new(), String::new(), String::new()],
            revision: 0,
        };
        // Offsets 150/15150/30150 → start seconds 2/202/402, digit sums
        // 2+4+6 = 12 = 0x0c; total 598 = 0x256 → discid 0c025603.
        let text = build(&entry, &sample_toc(), 0);
        assert!(text.contains("# Disc length: 600 seconds"));
        assert!(text.contains("# Revision: 0"));
        assert!(text.contains("# Processed by: Sparkamp "));
        assert!(text.contains("# Submitted via: Sparkamp "));
        assert!(text.contains("DISCID=0c025603"));

        let parsed = parse(&text).expect("round-trip parse");
        assert_eq!(parsed.artist, entry.artist);
        assert_eq!(parsed.album, entry.album);
        assert_eq!(parsed.year, entry.year);
        assert_eq!(parsed.genre, entry.genre);
        assert_eq!(parsed.track_titles, entry.track_titles);
        assert_eq!(parsed.extd, entry.extd);
        assert_eq!(parsed.discid, "0c025603");
    }
}
