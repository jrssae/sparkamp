//! CD-TEXT for audio burns, written as a Sony v07t definition sheet that
//! cdrskin consumes via `input_sheet_v07t=<path>`. Field names verified
//! against `man cdrskin` (dev-box) — the "purpose specifier" table under
//! `input_sheet_v07t=`: session-level `Album Title` / `Artist Name`,
//! per-track `Track NN Title` / `Track NN Artist`. Titles come from the
//! queue's display lines ("Artist - Title", or the whole string when
//! untagged), matching the display logic everywhere else in the app.

use crate::disc::burnlist::BurnItem;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DiscMeta {
    pub artist: String,
    pub album: String,
}

/// Sanitize tag text by replacing line breaks with spaces. The v07t sheet is
/// line-oriented (parsed line-by-line by cdrskin); untrusted tag values
/// (from ID3 metadata) containing embedded `\r` or `\n` could inject new
/// directive lines (e.g., redefining Album Title). This function collapses
/// all line-break sequences to a single space and trims the result.
fn sanitize(s: &str) -> String {
    s.replace('\r', " ")
        .replace('\n', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Split one queue display line into (performer, title).
fn split_display(display: &str, disc_artist: &str) -> (String, String) {
    match display.split_once(" - ") {
        Some((a, t)) => (a.trim().to_string(), t.trim().to_string()),
        None => (disc_artist.to_string(), display.trim().to_string()),
    }
}

/// Defaults: artist = the common track artist when every tagged track
/// agrees, else "Various Artists"; album = "Sparkamp Disc YYYY-MM-DD".
pub fn default_disc_meta(items: &[BurnItem]) -> DiscMeta {
    let mut artists = items.iter().filter_map(|i| {
        i.display.split_once(" - ").map(|(a, _)| a.trim().to_string())
    });
    let artist = match artists.next() {
        Some(first)
            if artists.all(|a| a == first)
                && items.iter().all(|i| i.display.contains(" - ")) =>
        {
            first
        }
        _ => "Various Artists".to_string(),
    };
    let today = chrono_free_today(); // no new crate
    DiscMeta { artist, album: format!("Sparkamp Disc {today}") }
}

/// YYYY-MM-DD from the system clock without adding a date crate: seconds
/// since epoch → civil date (Howard Hinnant's algorithm).
fn chrono_free_today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Sony v07t CD-TEXT definition sheet (one line per field; cdrskin's
/// `input_sheet_v07t=`). Only the fields hardware players read: album
/// title/artist + per-track title/artist. Field names are the documented
/// "purpose specifier" strings from `man cdrskin` — NOT guesses: session
/// fields are bare (`Album Title`, `Artist Name`), track fields carry the
/// two-digit track number *before* the field name (`Track 01 Title`,
/// `Track 01 Artist`), unlike a naive `Track 01 = ` / `Performer 01 = `
/// scheme.
pub fn build_v07t(meta: &DiscMeta, items: &[BurnItem]) -> String {
    render_v07t(&CdTextSheet::from_queue(meta, items))
}

/// Render a derived sheet as v07t. Split from [`build_v07t`] because the
/// Linux burn has the sheet already and must not re-derive it: two
/// derivations are two chances to disagree.
pub fn render_v07t(sheet: &CdTextSheet) -> String {
    let mut s = String::new();
    s.push_str("Input Sheet Version = 0.7T\n");
    s.push_str(&format!("Album Title = {}\n", sheet.album));
    s.push_str(&format!("Artist Name = {}\n", sheet.artist));
    for (i, track) in sheet.tracks.iter().enumerate() {
        s.push_str(&format!("Track {:02} Title = {}\n", i + 1, track.title));
        s.push_str(&format!("Track {:02} Artist = {}\n", i + 1, track.performer));
    }
    s
}

/// The CD-TEXT a burn writes, in the shape both backends want.
///
/// Two backends, two serializations: Linux hands `cdrskin` a v07t sheet on
/// disk, macOS builds a `DRCDTextBlockRef` in memory. Neither is the source of
/// truth — this is, and it is already sanitized, already split into performer
/// and title, and already in track order, so a backend only has to render it.
///
/// Deriving it once also keeps the two platforms from disagreeing about what
/// "the artist of track 3" means, which is exactly the kind of drift that
/// shows up as a disc that reads differently depending on who burned it.
#[derive(Debug, Clone, PartialEq)]
pub struct CdTextSheet {
    pub album: String,
    pub artist: String,
    /// One entry per track, in track order. Track *numbers* are 1-based and
    /// implied by position; CD-TEXT itself indexes the disc at 0 and track N
    /// at N, which is a backend's problem, not this type's.
    pub tracks: Vec<TrackText>,
}

/// One track's CD-TEXT.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackText {
    pub performer: String,
    pub title: String,
}

impl CdTextSheet {
    /// Derive the sheet from the burn queue: the disc metadata plus one
    /// display line per track, split the same way every other surface in the
    /// app splits it.
    pub fn from_queue(meta: &DiscMeta, items: &[BurnItem]) -> Self {
        Self {
            album: sanitize(&meta.album),
            artist: sanitize(&meta.artist),
            tracks: items
                .iter()
                .map(|item| {
                    let (performer, title) = split_display(&item.display, &meta.artist);
                    TrackText {
                        performer: sanitize(&performer),
                        title: sanitize(&title),
                    }
                })
                .collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Reading CD-TEXT back off a disc (so a burned/commercial disc with no gnudb
// match still shows real track/album names instead of "Track N").
// ---------------------------------------------------------------------------

/// CD-TEXT read from a loaded disc.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CdText {
    pub album: Option<String>,
    pub artist: Option<String>,
    /// (track number, title) — 1-based track numbers.
    pub track_titles: Vec<(u32, String)>,
}

impl CdText {
    pub fn is_empty(&self) -> bool {
        self.album.is_none() && self.artist.is_none() && self.track_titles.is_empty()
    }

    /// Synthesize a gnudb-style entry so the disc detail can overlay CD-TEXT
    /// exactly like a database match (album/artist header + per-track titles).
    /// Display-only — the caller keeps this in memory, not the tag store.
    pub fn to_xmcd(&self, discid: &str) -> crate::disc::xmcd::XmcdEntry {
        let n = self
            .track_titles
            .iter()
            .map(|(t, _)| *t as usize)
            .max()
            .unwrap_or(0);
        let mut titles = vec![String::new(); n];
        for (t, title) in &self.track_titles {
            if *t >= 1 && (*t as usize) <= n {
                titles[*t as usize - 1] = title.clone();
            }
        }
        crate::disc::xmcd::XmcdEntry {
            discid: discid.to_string(),
            artist: self.artist.clone().unwrap_or_default(),
            album: self.album.clone().unwrap_or_default(),
            track_titles: titles,
            ..Default::default()
        }
    }
}

/// Parse the Sony v07t sheet `cdrskin cdtext_to_v07t=-` prints (same field
/// names as [`build_v07t`]) into a [`CdText`]. Ignores the "Artist"/performer
/// lines and any header/remark lines — only album/artist and per-track titles
/// drive the track list.
pub fn parse_v07t_readback(text: &str) -> CdText {
    let mut out = CdText::default();
    for line in text.lines() {
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let (key, val) = (key.trim(), val.trim());
        if val.is_empty() {
            continue;
        }
        match key {
            "Album Title" => out.album = Some(val.to_string()),
            "Artist Name" => out.artist = Some(val.to_string()),
            k => {
                if let Some(rest) = k.strip_prefix("Track ") {
                    if let Some(num) = rest.strip_suffix(" Title") {
                        if let Ok(n) = num.trim().parse::<u32>() {
                            out.track_titles.push((n, val.to_string()));
                        }
                    }
                }
            }
        }
    }
    out
}

/// The external program [`read_cdtext`] shells out to on Linux. Named so a
/// failure can say which tool is missing instead of leaving the user to
/// guess. macOS reads CD-TEXT through DiscRecording and spawns nothing.
#[cfg(target_os = "linux")]
pub const CDTEXT_TOOL: &str = "cdrskin";

/// Why a CD-TEXT read produced no tags.
///
/// This distinction exists because the two cases look identical from the
/// outside and mean opposite things to a user. A disc with no CD-TEXT is
/// normal and there is nothing to do about it. A missing reader tool means
/// **no disc will ever show CD-TEXT** until it is installed — and before
/// 2026-08-09 both simply returned `None`, so the UI showed "Track 1…N" and
/// said nothing. That cost a real debugging session: a disc whose CD-TEXT
/// `cdrskin` reads perfectly looked, in the app, exactly like a disc that had
/// none, because the host had no `cdrskin` at all.
///
/// macOS reads through DiscRecording, so it has no tool to be missing:
/// `ToolMissing` is unreachable there and a failed read (no media, the raw
/// device refusing to open) arrives as `ToolFailed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CdTextMiss {
    /// The read worked and the disc genuinely carries no CD-TEXT.
    Absent,
    /// The reader program is not installed. Carries its name. Linux only.
    ToolMissing(&'static str),
    /// The read itself failed (permissions, drive busy, no media).
    ToolFailed(String),
}

impl CdTextMiss {
    /// A short line fit for a status bar, or `None` when the miss is the
    /// unremarkable one (the disc simply has no CD-TEXT) and the UI should
    /// stay quiet.
    pub fn user_message(&self) -> Option<String> {
        match self {
            CdTextMiss::Absent => None,
            CdTextMiss::ToolMissing(t) => {
                Some(format!("CD-TEXT unavailable — '{t}' is not installed"))
            }
            CdTextMiss::ToolFailed(e) => Some(format!("CD-TEXT read failed — {e}")),
        }
    }
}

/// Read CD-TEXT off the loaded disc via `cdrskin cdtext_to_v07t=-`.
/// READS THE DISC — the caller MUST hold the exclusive-read guard
/// (drive-contention rule). See [`CdTextMiss`] for the error cases.
///
/// # An empty read is not proof
///
/// A drive sometimes returns no CD-TEXT packs for a disc that has them, and
/// `cdrskin` reports that as success: exit status 0 and a v07t sheet with a
/// header and nothing in it. Measured on a disc with confirmed CD-TEXT
/// ("Bespoke Bounce", 15 tracks): 207 bytes instead of 1714, on roughly one
/// read in ten to one in thirty, with no error anywhere. Checking the exit
/// status does not help — it is 0 either way — and 207 bytes is also exactly
/// what a disc with genuinely no CD-TEXT returns, so the two cases are
/// indistinguishable from the output alone.
///
/// Retrying was tried and reverted. One call takes 2.9–4.8 s on a disc without
/// CD-TEXT, so three attempts cost ~14 s before concluding `Absent` — paid by
/// the *majority* of discs, on a path that holds the exclusive-read guard and
/// therefore freezes disc detection for the whole time. Trading a rare wrong
/// answer for a guaranteed stall on the common case is the worse deal.
///
/// Fixing this properly needs a way to tell the two apart — the drive's own
/// "has CD-TEXT" bit, or a re-read only when a previous read of the same
/// discid succeeded — rather than another attempt at the same ambiguous
/// question.
#[cfg(target_os = "linux")]
pub fn read_cdtext(drive_id: &str) -> Result<CdText, CdTextMiss> {
    let out = std::process::Command::new(CDTEXT_TOOL)
        .args([&format!("dev={drive_id}"), "cdtext_to_v07t=-"])
        .output()
        .map_err(|e| classify_spawn_error(e, CDTEXT_TOOL))?;
    let cd = parse_v07t_readback(&String::from_utf8_lossy(&out.stdout));
    if cd.is_empty() {
        Err(CdTextMiss::Absent)
    } else {
        Ok(cd)
    }
}

/// Read CD-TEXT off the loaded disc on macOS, through DiscRecording.
/// `drive_id` is the drive's enumeration index (`OpticalDrive::id`), the same
/// value the mac burn/rip paths pass. READS THE DISC — the caller MUST hold
/// the exclusive-read guard (drive-contention rule).
///
/// The empty-read caveat above applies here too, and for the same reason: it
/// is the drive that intermittently reports no PACKs, not the tool that used
/// to ask it.
#[cfg(target_os = "macos")]
pub fn read_cdtext(drive_id: &str) -> Result<CdText, CdTextMiss> {
    let device = crate::disc::discrecording::device_at_id(drive_id)
        .ok_or_else(|| CdTextMiss::ToolFailed(format!("no drive {drive_id}")))?;
    let node = device
        .status()
        .device_node
        .ok_or_else(|| CdTextMiss::ToolFailed("no disc in the drive".to_string()))?;
    let blocks = crate::disc::discrecording::cdtext_blocks(&node).map_err(CdTextMiss::ToolFailed)?;
    let cd = cdtext_from_blocks(&blocks);
    if cd.is_empty() {
        Err(CdTextMiss::Absent)
    } else {
        Ok(cd)
    }
}

/// Split a failed `Command::output()` into "the tool isn't there" and
/// everything else. `NotFound` is the case worth naming: it is not a property
/// of the disc, and it will not fix itself on the next disc.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn classify_spawn_error(e: std::io::Error, tool: &'static str) -> CdTextMiss {
    if e.kind() == std::io::ErrorKind::NotFound {
        CdTextMiss::ToolMissing(tool)
    } else {
        CdTextMiss::ToolFailed(e.to_string())
    }
}

/// One entry of a CD-TEXT block's track array as [`CdText`] needs it: index 0
/// describes the disc, index N describes track N.
///
/// macOS-only, and honestly so: this is the shape `DRCDTextBlockGetTrackDictionaries`
/// hands back. Linux reads CD-TEXT as `cdrskin` v07t text and folds it with
/// [`parse_v07t_readback`] instead, so there is nothing shared to lift out.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockTrack {
    pub title: Option<String>,
    pub performer: Option<String>,
}

/// Fold DiscRecording's CD-TEXT blocks into a [`CdText`].
///
/// Index 0 of each block's array describes the DISC and index N describes
/// track N — the indexing `DRCDTextBlockGetTrackDictionaries` documents, not
/// an off-by-one. Blocks after the first are the same disc in other
/// languages, so every field is first-wins: block 0 (English on essentially
/// every commercial disc) names the disc and a later block can only fill a
/// gap it left. Per-track performers are read but discarded, matching the
/// Linux v07t readback path — the overlay is titles plus a disc-level artist.
///
/// Tolerant by construction: an empty value is skipped and a disc whose
/// blocks carry nothing comes back empty, which the caller treats as "no
/// CD-TEXT".
#[cfg(target_os = "macos")]
pub fn cdtext_from_blocks(blocks: &[Vec<BlockTrack>]) -> CdText {
    let mut out = CdText::default();
    for block in blocks {
        for (index, entry) in block.iter().enumerate() {
            let title = entry.title.as_deref().filter(|s| !s.is_empty());
            let performer = entry.performer.as_deref().filter(|s| !s.is_empty());
            if index == 0 {
                if let Some(t) = title {
                    out.album.get_or_insert_with(|| t.to_string());
                }
                if let Some(p) = performer {
                    out.artist.get_or_insert_with(|| p.to_string());
                }
                continue;
            }
            let number = index as u32;
            if let Some(t) = title {
                if !out.track_titles.iter().any(|(n, _)| *n == number) {
                    out.track_titles.push((number, t.to_string()));
                }
            }
        }
    }
    out.track_titles.sort_by_key(|(n, _)| *n);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(display: &str) -> BurnItem {
        BurnItem {
            path: format!("/m/{display}.mp3").into(),
            display: display.into(),
            duration_secs: Some(60),
            bytes: 1,
        }
    }

    #[test]
    fn v07t_readback_parses_album_artist_and_titles() {
        // Real cdrskin cdtext_to_v07t output (captured from a burned disc):
        // header/remark/performer lines present and must be ignored.
        let sheet = "\
Input Sheet Version = 0.7T
Remarks             = Libburn report of CD-TEXT Block 0
Album Title         = Sparkamp CDTEXT Live
Artist Name         = Sparkamp Test
Track 01 Title      = I Found A Million Dollar Baby
Track 01 Artist     = 0. Adolf Ginsburg tan orch
Track 02 Title      = Boom Clap
Track 02 Artist     = 34. Charli Xcx
";
        let cd = parse_v07t_readback(sheet);
        assert_eq!(cd.album.as_deref(), Some("Sparkamp CDTEXT Live"));
        assert_eq!(cd.artist.as_deref(), Some("Sparkamp Test"));
        assert_eq!(cd.track_titles.len(), 2);
        assert_eq!(cd.track_titles[0], (1, "I Found A Million Dollar Baby".into()));

        // Round-trip into a gnudb-style entry (index 0 = track 1).
        let x = cd.to_xmcd("deadbeef");
        assert_eq!(x.artist, "Sparkamp Test");
        assert_eq!(x.album, "Sparkamp CDTEXT Live");
        assert_eq!(x.track_titles[1], "Boom Clap");

        // A disc with no CD-TEXT parses empty.
        assert!(parse_v07t_readback("Input Sheet Version = 0.7T\n").is_empty());
    }

    /// A disc with no CD-TEXT must stay quiet; a missing tool must not. These
    /// two were the same `None` until 2026-08-09, which is how a host without
    /// `cdrskin` looked exactly like a disc without CD-TEXT.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn only_a_tool_problem_produces_a_user_message() {
        assert_eq!(CdTextMiss::Absent.user_message(), None);

        let missing = CdTextMiss::ToolMissing("cdrskin").user_message().unwrap();
        assert!(missing.contains("cdrskin"), "message must name the tool: {missing}");
        assert!(missing.contains("not installed"), "{missing}");

        let failed = CdTextMiss::ToolFailed("permission denied".into())
            .user_message()
            .unwrap();
        assert!(failed.contains("permission denied"), "{failed}");
    }

    /// Only `NotFound` means "install something". Anything else is a run-time
    /// failure of a tool that IS present, and saying "not installed" for a
    /// permissions error would send the user off fixing the wrong thing.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn spawn_errors_split_missing_from_broken() {
        use std::io::{Error, ErrorKind};
        assert_eq!(
            classify_spawn_error(Error::from(ErrorKind::NotFound), "toolname"),
            CdTextMiss::ToolMissing("toolname")
        );
        assert!(matches!(
            classify_spawn_error(Error::from(ErrorKind::PermissionDenied), "toolname"),
            CdTextMiss::ToolFailed(_)
        ));
    }

    /// An empty/garbage readback is `Absent`, not a tool problem — the tool ran
    /// fine, the disc just had nothing on it.
    #[test]
    fn empty_readback_is_absent_not_a_tool_problem() {
        assert!(parse_v07t_readback("Input Sheet Version = 0.7T\n").is_empty());
        assert!(parse_v07t_readback("").is_empty());
    }

    #[test]
    fn defaults_common_artist_else_various() {
        let same = [item("Foo - One"), item("Foo - Two")];
        assert_eq!(default_disc_meta(&same).artist, "Foo");
        let mixed = [item("Foo - One"), item("Bar - Two")];
        assert_eq!(default_disc_meta(&mixed).artist, "Various Artists");
        let untagged = [item("justafilename")];
        assert_eq!(default_disc_meta(&untagged).artist, "Various Artists");
        assert!(default_disc_meta(&same).album.starts_with("Sparkamp Disc 2"));
    }

    #[test]
    fn v07t_sheet_carries_album_and_tracks() {
        let meta = DiscMeta { artist: "Foo".into(), album: "My Disc".into() };
        let items = [item("Foo - One"), item("justafilename")];
        let sheet = build_v07t(&meta, &items);
        assert!(sheet.contains("Album Title = My Disc"), "{sheet}");
        assert!(sheet.contains("Artist Name = Foo"), "{sheet}");
        assert!(sheet.contains("Track 01 Title = One"), "{sheet}");
        assert!(sheet.contains("Track 01 Artist = Foo"), "{sheet}");
        // No " - " separator: whole display becomes the title, disc artist
        // fills the per-track Artist field.
        assert!(sheet.contains("Track 02 Title = justafilename"), "{sheet}");
        assert!(sheet.contains("Track 02 Artist = Foo"), "{sheet}");
    }

    #[test]
    fn v07t_strips_line_breaks_from_tag_text() {
        let meta = DiscMeta {
            artist: "A\nAlbum Title = HACKED".into(),
            album: "B\r\nArtist Name = X".into(),
        };
        let items = [item("Evil\nTrack 02 Title = Nope - T")];
        let sheet = build_v07t(&meta, &items);
        // No injected directive lines: newlines are replaced with spaces,
        // so attempted injections like "Album Title = HACKED" on their own
        // line cannot exist.
        let lines: Vec<&str> = sheet.lines().collect();
        assert!(
            !lines.iter().any(|l| l.starts_with("Album Title = HACKED")),
            "injected Album Title directive found: {sheet}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("Artist Name = X")),
            "injected Artist Name directive found: {sheet}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("Track 02 Title = Nope")),
            "injected Track 02 Title directive found: {sheet}"
        );
        // Sanitized text keeps the readable parts (newlines replaced with spaces).
        assert!(sheet.contains("Album Title = B Artist Name = X"), "{sheet}");
        assert!(sheet.contains("Artist Name = A Album Title = HACKED"), "{sheet}");
    }








    /// Live read off a real disc. Ignored by default (like the other `live_*`
    /// disc tests) — requires a CD-TEXT-bearing audio disc in the drive.
    /// Point `SPARKAMP_TEST_DRIVE` at the optical device (default `/dev/sr0`).
    /// Run: `cargo test --lib live_cdtext_read -- --ignored --nocapture`.
    #[test]
    #[ignore = "requires a real disc with CD-TEXT in the drive; human-run"]
    #[cfg(target_os = "linux")]
    fn live_cdtext_read() {
        let dev = std::env::var("SPARKAMP_TEST_DRIVE").unwrap_or_else(|_| "/dev/sr0".into());
        let cd = read_cdtext(&dev);
        println!("CD-TEXT read from {dev}: {cd:?}");
        assert!(cd.is_ok(), "no CD-TEXT read from {dev}");
    }

    /// Live CD-TEXT read through DiscRecording, replacing the `drutil` dump
    /// test the port deleted. Same purpose: prove a real disc's names come
    /// back, and print what arrived when they do not.
    ///
    /// `SPARKAMP_TEST_DRIVE` is the drive id from `list_drives`, default `1`.
    /// Run: `cargo test --lib live_cdtext_read -- --ignored --nocapture`
    #[test]
    #[ignore = "requires a real disc with CD-TEXT in the drive; human-run"]
    #[cfg(target_os = "macos")]
    fn live_cdtext_read() {
        let drive = std::env::var("SPARKAMP_TEST_DRIVE").unwrap_or_else(|_| "1".into());
        // Reading CD-TEXT reads the disc, so the drive-contention rule applies:
        // without the guard a detection poll can open the device mid-read and
        // the raw read comes back EIO. Measured, not theoretical.
        crate::disc::detect::begin_exclusive_read();
        let result = read_cdtext(&drive);
        crate::disc::detect::end_exclusive_read();
        match result {
            Ok(cd) => {
                println!("--- CD-TEXT from drive {drive} ---\n{cd:#?}");
                assert!(!cd.is_empty(), "read_cdtext returned Ok but empty");
            }
            Err(CdTextMiss::Absent) => {
                println!("drive {drive}: disc carries no CD-TEXT, which is common");
            }
            Err(e) => panic!("CD-TEXT read failed on drive {drive}: {e:?}"),
        }
    }

    /// The fold has real rules and until now nothing tested them: index 0 is
    /// the disc rather than track 1, later blocks are other languages of the
    /// same disc and may only fill gaps, empty strings are not values, and
    /// per-track performers are deliberately dropped.
    #[cfg(target_os = "macos")]
    mod fold {
        use super::super::{cdtext_from_blocks, BlockTrack};

        fn e(title: &str, performer: &str) -> BlockTrack {
            BlockTrack {
                title: (!title.is_empty()).then(|| title.to_string()),
                performer: (!performer.is_empty()).then(|| performer.to_string()),
            }
        }

        #[test]
        fn index_zero_is_the_disc_and_index_n_is_track_n() {
            let cd = cdtext_from_blocks(&[vec![e("Kind of Blue", "Miles Davis"), e("So What", "")]]);
            assert_eq!(cd.album.as_deref(), Some("Kind of Blue"));
            assert_eq!(cd.artist.as_deref(), Some("Miles Davis"));
            assert_eq!(cd.track_titles, vec![(1, "So What".to_string())]);
        }

        #[test]
        fn a_later_language_block_may_only_fill_a_gap() {
            let cd = cdtext_from_blocks(&[
                vec![e("Disc EN", ""), e("Track EN", "")],
                vec![e("Disc JP", "Artist JP"), e("Track JP", "")],
            ]);
            assert_eq!(cd.album.as_deref(), Some("Disc EN"), "first block wins");
            assert_eq!(
                cd.artist.as_deref(),
                Some("Artist JP"),
                "a later block fills what the first left empty"
            );
            assert_eq!(cd.track_titles, vec![(1, "Track EN".to_string())]);
        }

        /// `Some("")` rather than `None`: a drive really does hand back empty
        /// strings, and the fold filters them. Building these with the helper
        /// would produce `None` and test nothing, which is how the first
        /// version of this passed while the filter was mutated away.
        #[test]
        fn an_empty_string_is_not_a_value() {
            let blank = BlockTrack {
                title: Some(String::new()),
                performer: Some(String::new()),
            };
            let cd = cdtext_from_blocks(&[vec![blank.clone(), blank]]);
            assert!(cd.is_empty(), "a block of empties reads as no CD-TEXT: {cd:?}");
        }

        #[test]
        fn a_per_track_performer_never_becomes_the_disc_artist() {
            let cd = cdtext_from_blocks(&[vec![e("Disc", ""), e("Track", "Guest Vocalist")]]);
            assert_eq!(cd.artist, None, "track performers are dropped, not promoted");
        }
    }
}
