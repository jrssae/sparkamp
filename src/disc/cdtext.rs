//! CD-TEXT for audio burns, written as a Sony v07t definition sheet that
//! cdrskin consumes via `input_sheet_v07t=<path>`. Field names verified
//! against `man cdrskin` (dev-box) — the "purpose specifier" table under
//! `input_sheet_v07t=`: session-level `Album Title` / `Artist Name`,
//! per-track `Track NN Title` / `Track NN Artist`. Titles come from the
//! queue's display lines ("Artist - Title", or the whole string when
//! untagged), matching the display logic everywhere else in the app.

use crate::disc::burnlist::BurnItem;

#[derive(Debug, Clone, PartialEq)]
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
}
