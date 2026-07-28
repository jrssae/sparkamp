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

/// Read CD-TEXT off the loaded disc via `cdrskin cdtext_to_v07t=-`. `None`
/// when the disc carries no CD-TEXT or cdrskin fails. READS THE DISC — the
/// caller MUST hold the exclusive-read guard (drive-contention rule).
#[cfg(target_os = "linux")]
pub fn read_cdtext(drive_id: &str) -> Option<CdText> {
    let out = std::process::Command::new("cdrskin")
        .args([&format!("dev={drive_id}"), "cdtext_to_v07t=-"])
        .output()
        .ok()?;
    let cd = parse_v07t_readback(&String::from_utf8_lossy(&out.stdout));
    (!cd.is_empty()).then_some(cd)
}

/// Read CD-TEXT off the loaded disc on macOS via `drutil -drive <id> cdtext`.
/// `drive_id` is the drutil enumeration index (`OpticalDrive::id`), the same
/// value the mac burn/rip paths pass. `None` when the disc has no CD-TEXT or
/// drutil fails. READS THE DISC — the caller MUST hold the exclusive-read
/// guard (drive-contention rule).
#[cfg(target_os = "macos")]
pub fn read_cdtext(drive_id: &str) -> Option<CdText> {
    let out = std::process::Command::new("drutil")
        .args(["-drive", drive_id, "cdtext"])
        .output()
        .ok()?;
    let cd = parse_drutil_cdtext(&String::from_utf8_lossy(&out.stdout));
    (!cd.is_empty()).then_some(cd)
}

/// Parse `drutil cdtext` output into a [`CdText`]. Tolerant, quote-aware:
/// the FIRST `TITLE`/`PERFORMER` pair is disc-level; each later `TITLE`
/// (under a `Track N:` heading) becomes that track's title in order.
///
/// NOTE: `drutil cdtext`'s exact format is undocumented; this handles the
/// plausible `KEY "value"` token form. A macOS worker must confirm the real
/// output shape and adjust this parser + its test fixture (see plan).
///
/// Only called from the macOS `read_cdtext` arm below; on other platforms
/// its sole non-test caller is compiled out, so it would otherwise flag as
/// dead code in the binary target (same pattern as `leading_number` in
/// `disc/toc.rs`).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_drutil_cdtext(text: &str) -> CdText {
    let mut out = CdText::default();
    let mut cur_track: Option<u32> = None;
    // Scope flag: true once any "Track N:" heading has been seen. Unlike
    // `cur_track` (which is consumed by the next TITLE so it can track the
    // *number* for the following track title), this never resets — once a
    // track block starts, disc-level PERFORMER/TITLE promotion is over for
    // good, structurally matching `parse_v07t_readback`.
    let mut in_track = false;
    let mut next_seq: u32 = 0; // fallback track counter when no explicit "Track N"
    for line in text.lines() {
        let t = line.trim();
        // "Track 3:" heading sets the track the following TITLE belongs to.
        if let Some(rest) = t.strip_prefix("Track ") {
            if let Some(nums) = rest.split([':', ' ']).next() {
                if let Ok(n) = nums.trim().parse::<u32>() {
                    cur_track = Some(n);
                    in_track = true;
                    continue;
                }
            }
        }
        let Some(val) = quoted_value(t) else { continue };
        if val.is_empty() {
            continue;
        }
        if let Some(key) = t.split_whitespace().next() {
            match key {
                "TITLE" | "Title" => {
                    if out.album.is_none() && !in_track {
                        out.album = Some(val);
                    } else {
                        let n = cur_track.take().unwrap_or_else(|| {
                            next_seq += 1;
                            next_seq
                        });
                        if n > next_seq {
                            next_seq = n;
                        }
                        out.track_titles.push((n, val));
                    }
                }
                "PERFORMER" | "Performer" => {
                    if out.artist.is_none() && !in_track {
                        out.artist = Some(val);
                    }
                    // per-track performers are ignored (title-only overlay,
                    // same as the v07t readback path).
                }
                _ => {}
            }
        }
    }
    out
}

/// Extract the first double-quoted substring from a line, if any.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn quoted_value(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
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

    #[test]
    fn drutil_cdtext_parses_album_artist_and_titles() {
        // PROVISIONAL fixture — verify/replace against a real `drutil cdtext`
        // dump on macOS (see the BLIND-FORMAT WARNING in the plan). The parser
        // treats the first TITLE/PERFORMER pair as disc-level and each
        // subsequent TITLE as track 1, 2, … in order.
        let dump = "\
CD-Text, Block 0 (English):
  TITLE \"Greatest Hits\"
  PERFORMER \"The Band\"
  Track 1:
    TITLE \"First Song\"
  Track 2:
    TITLE \"Second Song\"
";
        let cd = parse_drutil_cdtext(dump);
        assert_eq!(cd.album.as_deref(), Some("Greatest Hits"));
        assert_eq!(cd.artist.as_deref(), Some("The Band"));
        assert_eq!(cd.track_titles.len(), 2);
        assert_eq!(cd.track_titles[0], (1, "First Song".into()));
        assert_eq!(cd.track_titles[1], (2, "Second Song".into()));

        // Round-trips into the same gnudb-style overlay entry as the v07t path.
        let x = cd.to_xmcd("deadbeef");
        assert_eq!(x.album, "Greatest Hits");
        assert_eq!(x.track_titles[1], "Second Song");

        // No CD-TEXT → empty (caller treats as a miss).
        assert!(parse_drutil_cdtext("No CD-Text on this disc.\n").is_empty());
    }

    #[test]
    fn drutil_cdtext_does_not_leak_per_track_performer_into_disc_artist() {
        // Per-track PERFORMER lines must never be promoted to the disc-level
        // artist, regardless of whether TITLE or PERFORMER comes first within
        // the track block. Only the disc-level PERFORMER (before any "Track
        // N:" heading) may set `out.artist`.
        let dump = "\
CD-Text, Block 0 (English):
  TITLE \"Greatest Hits\"
  PERFORMER \"The Band\"
  Track 1:
    TITLE \"First Song\"
    PERFORMER \"Solo Artist A\"
  Track 2:
    PERFORMER \"Solo Artist B\"
    TITLE \"Second Song\"
";
        let cd = parse_drutil_cdtext(dump);
        assert_eq!(cd.artist.as_deref(), Some("The Band"));
        assert_eq!(cd.album.as_deref(), Some("Greatest Hits"));
        assert_eq!(cd.track_titles.len(), 2);
        assert_eq!(cd.track_titles[0], (1, "First Song".into()));
        assert_eq!(cd.track_titles[1], (2, "Second Song".into()));

        // No disc-level PERFORMER at all: track performers must not leak in
        // as a fallback disc artist — `cd.artist` stays `None`.
        let dump_no_disc_performer = "\
CD-Text, Block 0 (English):
  TITLE \"Greatest Hits\"
  Track 1:
    TITLE \"First Song\"
    PERFORMER \"Solo Artist A\"
  Track 2:
    PERFORMER \"Solo Artist B\"
    TITLE \"Second Song\"
";
        let cd2 = parse_drutil_cdtext(dump_no_disc_performer);
        assert_eq!(cd2.artist, None);
        assert_eq!(cd2.album.as_deref(), Some("Greatest Hits"));
        assert_eq!(cd2.track_titles.len(), 2);
        assert_eq!(cd2.track_titles[0], (1, "First Song".into()));
        assert_eq!(cd2.track_titles[1], (2, "Second Song".into()));
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
        assert!(cd.is_some(), "no CD-TEXT read from {dev}");
    }
}
