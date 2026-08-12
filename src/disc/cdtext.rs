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
    let mut s = String::new();
    s.push_str("Input Sheet Version = 0.7T\n");
    s.push_str(&format!("Album Title = {}\n", sanitize(&meta.album)));
    s.push_str(&format!("Artist Name = {}\n", sanitize(&meta.artist)));
    for (i, item) in items.iter().enumerate() {
        let (performer, title) = split_display(&item.display, &meta.artist);
        s.push_str(&format!("Track {:02} Title = {}\n", i + 1, sanitize(&title)));
        s.push_str(&format!("Track {:02} Artist = {}\n", i + 1, sanitize(&performer)));
    }
    s
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

/// The external program [`read_cdtext`] shells out to on this platform.
/// Named so a failure can say which tool is missing instead of leaving the
/// user to guess.
#[cfg(target_os = "linux")]
pub const CDTEXT_TOOL: &str = "cdrskin";
#[cfg(target_os = "macos")]
pub const CDTEXT_TOOL: &str = "drutil";

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CdTextMiss {
    /// The read worked and the disc genuinely carries no CD-TEXT.
    Absent,
    /// The reader program is not installed. Carries its name.
    ToolMissing(&'static str),
    /// The program exists but could not be run (permissions, drive busy).
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

/// Read CD-TEXT off the loaded disc on macOS via `drutil -drive <id> cdtext`.
/// `drive_id` is the drutil enumeration index (`OpticalDrive::id`), the same
/// value the mac burn/rip paths pass. READS THE DISC — the caller MUST hold
/// the exclusive-read guard (drive-contention rule).
#[cfg(target_os = "macos")]
pub fn read_cdtext(drive_id: &str) -> Result<CdText, CdTextMiss> {
    let out = std::process::Command::new(CDTEXT_TOOL)
        .args(["-drive", drive_id, "cdtext"])
        .output()
        .map_err(|e| classify_spawn_error(e, CDTEXT_TOOL))?;
    let cd = parse_drutil_cdtext(&String::from_utf8_lossy(&out.stdout));
    if cd.is_empty() {
        Err(CdTextMiss::Absent)
    } else {
        Ok(cd)
    }
}

/// Split a failed `Command::output()` into "the tool isn't there" and
/// everything else. `NotFound` is the case worth naming: it is not a property
/// of the disc, and it will not fix itself on the next disc.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn classify_spawn_error(e: std::io::Error, tool: &'static str) -> CdTextMiss {
    if e.kind() == std::io::ErrorKind::NotFound {
        CdTextMiss::ToolMissing(tool)
    } else {
        CdTextMiss::ToolFailed(e.to_string())
    }
}

/// Parse `drutil cdtext` output into a [`CdText`].
///
/// `drutil` does not print a human-readable dump: it builds one
/// `{Properties, Tracks}` dictionary per CD-TEXT block, wraps them in an
/// array, and serializes the lot as an XML property list
/// (`NSPropertyListXMLFormat_v1_0`), which it writes to stdout verbatim.
/// That shape was read out of `/usr/bin/drutil` itself — the disassembled
/// `cdtext` command ends in `dataWithPropertyList:format:100…` followed by
/// `printf("%.*s")` — and the keys are DiscRecording's own
/// (`kDRCDTextTitleKey` == `"DRCDTextTitleKey"`, and so on), so a dump looks
/// like:
///
/// ```text
/// <array>
///   <dict>
///     <key>Properties</key>
///     <dict><key>DRCDTextLanguageKey</key><string>en</string>…</dict>
///     <key>Tracks</key>
///     <array>
///       <dict><key>DRCDTextTitleKey</key><string>Greatest Hits</string>…</dict>
///       <dict><key>DRCDTextTitleKey</key><string>First Song</string>…</dict>
///     </array>
///   </dict>
/// </array>
/// ```
///
/// Index 0 of `Tracks` describes the DISC and index N describes track N —
/// the indexing `DRCDTextBlockGetTrackDictionaries` documents, not an
/// off-by-one. Blocks after the first are the same disc in other languages,
/// so every field is first-wins: block 0 (English on essentially every
/// commercial disc) names the disc and a later block can only fill a gap it
/// left. Per-track performers are read but discarded, matching the Linux
/// v07t readback path — the overlay is titles plus a disc-level artist.
///
/// Tolerant by construction: anything unrecognized is skipped and a dump
/// that yields nothing comes back empty, which the caller treats as "no
/// CD-TEXT". A `<string>` whose value contains a literal newline would be
/// missed by this line-at-a-time scan, the same constraint
/// `detect::parse_toc_plist` already carries; CD-TEXT fields are
/// single-line by format.
///
/// Only called from the macOS `read_cdtext` arm above; on other platforms
/// its sole non-test caller is compiled out, so it would otherwise flag as
/// dead code in the binary target (same pattern as `leading_number` in
/// `disc/toc.rs`).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_drutil_cdtext(text: &str) -> CdText {
    let mut out = CdText::default();
    let mut last_key = String::new();
    // Only dictionaries inside a block's `Tracks` array carry names; the
    // sibling `Properties` dict holds the language/encoding and must not be
    // mistaken for disc-level text.
    let mut in_tracks = false;
    // Position of the dict currently being read within `Tracks`: 0 = disc,
    // N = track N. `next_index` is where the *next* `<dict>` lands.
    let mut next_index: u32 = 0;
    let mut cur_index: Option<u32> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if let Some(k) = line
            .strip_prefix("<key>")
            .and_then(|r| r.strip_suffix("</key>"))
        {
            last_key = k.to_string();
        } else if line == "<array>" {
            // The only array a block opens under a key is `Tracks`; the
            // outer array of blocks arrives with no key in front of it.
            if last_key == "Tracks" {
                in_tracks = true;
                next_index = 0;
            }
            last_key.clear();
        } else if line == "</array>" {
            in_tracks = false;
        } else if line == "<dict>" {
            if in_tracks {
                cur_index = Some(next_index);
                next_index += 1;
            }
        } else if line == "</dict>" {
            cur_index = None;
        } else if let Some(v) = line
            .strip_prefix("<string>")
            .and_then(|r| r.strip_suffix("</string>"))
        {
            let Some(index) = cur_index else { continue };
            let value = xml_unescape(v);
            if value.is_empty() {
                continue;
            }
            match (index, last_key.as_str()) {
                (0, "DRCDTextTitleKey") => out.album.get_or_insert(value),
                (0, "DRCDTextPerformerKey") => out.artist.get_or_insert(value),
                (n, "DRCDTextTitleKey") => {
                    if !out.track_titles.iter().any(|(t, _)| *t == n) {
                        out.track_titles.push((n, value));
                    }
                    continue;
                }
                _ => continue,
            };
        }
    }
    out.track_titles.sort_by_key(|(n, _)| *n);
    out
}

/// Resolve XML entity references in a plist `<string>` body. Written as one
/// left-to-right pass rather than chained `replace`s so an escaped escape
/// (`&amp;lt;`) resolves to the literal `&lt;` instead of being unescaped
/// twice into `<`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        // Entities are short; a bare `&` in text (invalid XML, but be
        // tolerant) has no `;` nearby and falls through as a literal.
        let end = tail[..tail.len().min(12)].find(';');
        let Some(end) = end else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let body = &tail[1..end];
        let resolved = match body {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => body
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.parse::<u32>().ok(),
                })
                .and_then(char::from_u32),
        };
        match resolved {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            // Unknown entity: keep it verbatim rather than dropping text.
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
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

        let missing = CdTextMiss::ToolMissing(CDTEXT_TOOL).user_message().unwrap();
        assert!(missing.contains(CDTEXT_TOOL), "message must name the tool: {missing}");
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

    /// Header + wrapper every `drutil cdtext` dump carries, so the fixtures
    /// below only have to spell out the part that varies.
    fn drutil_dump(blocks: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<array>\n{blocks}</array>\n</plist>\n"
        )
    }

    /// The `Properties` dict drutil emits ahead of every block's `Tracks`
    /// array: language and encoding, never any names. Present in the
    /// fixtures because the parser has to skip past it correctly.
    fn drutil_properties(lang: &str) -> String {
        format!(
            "\t\t<key>Properties</key>
\t\t<dict>
\t\t\t<key>DRCDTextCFStringEncodingKey</key>
\t\t\t<integer>1536</integer>
\t\t\t<key>DRCDTextCharacterCodeKey</key>
\t\t\t<integer>1</integer>
\t\t\t<key>DRCDTextLanguageKey</key>
\t\t\t<string>{lang}</string>
\t\t\t<key>DRCDTextNSStringEncodingKey</key>
\t\t\t<integer>1</integer>
\t\t</dict>
"
        )
    }

    fn drutil_block(lang: &str, track_dicts: &str) -> String {
        format!(
            "\t<dict>\n{}\t\t<key>Tracks</key>\n\t\t<array>\n{track_dicts}\t\t</array>\n\t</dict>\n",
            drutil_properties(lang)
        )
    }

    #[test]
    fn drutil_cdtext_parses_album_artist_and_titles() {
        // Real `drutil cdtext` output shape, generated on macOS through the
        // same DiscRecording + NSPropertyListSerialization calls disassembled
        // out of /usr/bin/drutil. Tracks[0] is the DISC, Tracks[N] is track N
        // — NOT a track list starting at 1.
        let dump = drutil_dump(&drutil_block(
            "en",
            "\t\t\t<dict>
\t\t\t\t<key>DRCDTextPerformerKey</key>
\t\t\t\t<string>The Band</string>
\t\t\t\t<key>DRCDTextTitleKey</key>
\t\t\t\t<string>Greatest Hits</string>
\t\t\t</dict>
\t\t\t<dict>
\t\t\t\t<key>DRCDTextTitleKey</key>
\t\t\t\t<string>First Song</string>
\t\t\t</dict>
\t\t\t<dict>
\t\t\t\t<key>DRCDTextPerformerKey</key>
\t\t\t\t<string>Guest Artist</string>
\t\t\t\t<key>DRCDTextTitleKey</key>
\t\t\t\t<string>Second Song</string>
\t\t\t</dict>
",
        ));
        let cd = parse_drutil_cdtext(&dump);
        assert_eq!(cd.album.as_deref(), Some("Greatest Hits"));
        // "Guest Artist" is track 2's performer and must not displace the
        // disc's own; the language string in `Properties` must not either.
        assert_eq!(cd.artist.as_deref(), Some("The Band"));
        assert_eq!(cd.track_titles.len(), 2);
        assert_eq!(cd.track_titles[0], (1, "First Song".into()));
        assert_eq!(cd.track_titles[1], (2, "Second Song".into()));

        // Round-trips into the same gnudb-style overlay entry as the v07t path.
        let x = cd.to_xmcd("deadbeef");
        assert_eq!(x.album, "Greatest Hits");
        assert_eq!(x.track_titles[1], "Second Song");

        // A disc with no CD-TEXT: drutil writes its "No CD-Text information
        // available" line to stderr and leaves stdout empty, so the parser
        // sees "" and the caller reads that as a miss.
        assert!(parse_drutil_cdtext("").is_empty());
        // A drive that reported a block but no readable text still parses to
        // a miss rather than an entry full of empty strings.
        assert!(parse_drutil_cdtext(&drutil_dump(&drutil_block("en", ""))).is_empty());
    }

    #[test]
    fn drutil_cdtext_does_not_leak_per_track_performer_into_disc_artist() {
        // Per-track performers must never be promoted to the disc artist:
        // only Tracks[0] — the disc dictionary — may set it. Here no block
        // names a disc performer at all, so `artist` stays None even though
        // every track has one.
        let dump = drutil_dump(&drutil_block(
            "en",
            "\t\t\t<dict>
\t\t\t\t<key>DRCDTextTitleKey</key>
\t\t\t\t<string>Greatest Hits</string>
\t\t\t</dict>
\t\t\t<dict>
\t\t\t\t<key>DRCDTextPerformerKey</key>
\t\t\t\t<string>Solo Artist A</string>
\t\t\t\t<key>DRCDTextTitleKey</key>
\t\t\t\t<string>First Song</string>
\t\t\t</dict>
\t\t\t<dict>
\t\t\t\t<key>DRCDTextPerformerKey</key>
\t\t\t\t<string>Solo Artist B</string>
\t\t\t\t<key>DRCDTextTitleKey</key>
\t\t\t\t<string>Second Song</string>
\t\t\t</dict>
",
        ));
        let cd = parse_drutil_cdtext(&dump);
        assert_eq!(cd.artist, None);
        assert_eq!(cd.album.as_deref(), Some("Greatest Hits"));
        assert_eq!(cd.track_titles.len(), 2);
        assert_eq!(cd.track_titles[0], (1, "First Song".into()));
        assert_eq!(cd.track_titles[1], (2, "Second Song".into()));
    }

    #[test]
    fn drutil_cdtext_multi_language_blocks_are_first_wins() {
        // A disc carrying several language blocks emits one dict per block.
        // Fields are taken first-wins, so block 0 (English in practice) names
        // the disc and a later block only supplies what block 0 left out —
        // here the disc performer and the third track.
        let dump = drutil_dump(&format!(
            "{}{}",
            drutil_block(
                "en",
                "\t\t\t<dict>
\t\t\t\t<key>DRCDTextTitleKey</key>
\t\t\t\t<string>Greatest Hits</string>
\t\t\t</dict>
\t\t\t<dict>
\t\t\t\t<key>DRCDTextTitleKey</key>
\t\t\t\t<string>First Song</string>
\t\t\t</dict>
",
            ),
            drutil_block(
                "fr",
                "\t\t\t<dict>
\t\t\t\t<key>DRCDTextPerformerKey</key>
\t\t\t\t<string>Le Groupe</string>
\t\t\t\t<key>DRCDTextTitleKey</key>
\t\t\t\t<string>Grands Succes</string>
\t\t\t</dict>
\t\t\t<dict>
\t\t\t\t<key>DRCDTextTitleKey</key>
\t\t\t\t<string>Premiere</string>
\t\t\t</dict>
\t\t\t<dict>
\t\t\t\t<key>DRCDTextTitleKey</key>
\t\t\t\t<string>Deuxieme</string>
\t\t\t</dict>
",
            )
        ));
        let cd = parse_drutil_cdtext(&dump);
        assert_eq!(cd.album.as_deref(), Some("Greatest Hits"));
        assert_eq!(cd.artist.as_deref(), Some("Le Groupe"));
        assert_eq!(
            cd.track_titles,
            vec![
                (1, "First Song".to_string()),
                (2, "Deuxieme".to_string()),
            ]
        );
    }

    #[test]
    fn drutil_cdtext_resolves_xml_entities() {
        // Names go through XML escaping on the way out of
        // NSPropertyListSerialization, so "&" and "<" arrive as entities and
        // have to come back as themselves. Double quotes are not escaped in
        // element content and must survive untouched.
        let dump = drutil_dump(&drutil_block(
            "en",
            "\t\t\t<dict>
\t\t\t\t<key>DRCDTextPerformerKey</key>
\t\t\t\t<string>Simon &amp; Garfunkel</string>
\t\t\t\t<key>DRCDTextTitleKey</key>
\t\t\t\t<string>Bridge &lt;over&gt; \"Water\"</string>
\t\t\t</dict>
\t\t\t<dict>
\t\t\t\t<key>DRCDTextTitleKey</key>
\t\t\t\t<string>Song #1 &amp; More</string>
\t\t\t</dict>
",
        ));
        let cd = parse_drutil_cdtext(&dump);
        assert_eq!(cd.artist.as_deref(), Some("Simon & Garfunkel"));
        assert_eq!(cd.album.as_deref(), Some("Bridge <over> \"Water\""));
        assert_eq!(cd.track_titles[0], (1, "Song #1 & More".into()));

        // An escaped escape resolves once, not twice: a title that really
        // contains the text "&lt;" must not decay into "<".
        assert_eq!(xml_unescape("a &amp;lt; b"), "a &lt; b");
        // Numeric references, and a bare "&" left alone rather than eating
        // the rest of the line.
        assert_eq!(xml_unescape("&#65;&#x42;C &amp; D"), "ABC & D");
        assert_eq!(xml_unescape("Rock & Roll"), "Rock & Roll");
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

    /// macOS counterpart, and the one command that answers "does the parser
    /// match this drive?" — it prints the raw `drutil cdtext` dump next to
    /// what the parser made of it, so a mismatch is visible rather than just
    /// showing up as a disc with no names. `SPARKAMP_TEST_DRIVE` is the
    /// drutil drive index (`drutil list`), default `1`.
    /// Run: `cargo test --lib live_drutil_cdtext_read -- --ignored --nocapture`.
    #[test]
    #[ignore = "requires a real disc with CD-TEXT in the drive; human-run"]
    #[cfg(target_os = "macos")]
    fn live_drutil_cdtext_read() {
        let drive = std::env::var("SPARKAMP_TEST_DRIVE").unwrap_or_else(|_| "1".into());
        let out = std::process::Command::new("drutil")
            .args(["-drive", &drive, "cdtext"])
            .output()
            .expect("drutil should be present on macOS");
        let stdout = String::from_utf8_lossy(&out.stdout);
        println!("--- raw `drutil -drive {drive} cdtext` stdout ---\n{stdout}");
        println!("--- stderr ---\n{}", String::from_utf8_lossy(&out.stderr));
        let cd = parse_drutil_cdtext(&stdout);
        println!("--- parsed ---\n{cd:#?}");
        assert!(
            !cd.is_empty(),
            "parsed nothing out of drive {drive}; paste the raw dump above into \
             docs/mac-pass-checklist.md (Phase 9) so the parser can be corrected"
        );
    }
}
