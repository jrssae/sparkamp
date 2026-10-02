//! The three-cell source indicator: where a song's copies are, and whether
//! they agree.
//!
//! In the symbol and ASCII sets, cell one is the local copy, cell two the
//! server copies, cell three the sync state. The symbols are East Asian
//! "ambiguous width" characters, which some terminals (mostly in CJK
//! locales) draw two cells wide, hence the ASCII set.
//!
//! The emoji set is one emoji per state, as the macOS app draws one icon.
//! Each is East Asian "wide" with emoji presentation by default, so every
//! terminal draws it exactly two cells wide; a trailing space makes three.
//! Emoji that need a variation selector to look like emoji (☁️, ⬆️, ⚠️) are
//! avoided: terminals disagree on their width and the columns would shift.
//! A terminal without an emoji font (the Linux text console) shows boxes,
//! which is what the other two sets are for.

use super::merge::SongStatus;
use serde::{Deserialize, Serialize};

/// Which glyphs draw the indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkStyle {
    /// One emoji per state: 💻 🌐 ✅ 🔼 🔽 ❗ ❓ 🔗 🚫.
    #[default]
    Emoji,
    /// Three cells: `▪ ☁ ×` and `↑ ↓ ! ? ≈`. The macOS app reads this set.
    Symbols,
    /// Three cells: `L C x` and `^ v ! ? ~`.
    Ascii,
}

/// What the indicator shows for one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Indicator {
    pub has_local: bool,
    pub has_server: bool,
    pub status: SongStatus,
    pub possible_match: bool,
    /// No copy of this server-only song can be reached right now.
    pub unreachable: bool,
}

/// The indicator for `ind`, three terminal cells wide in every style.
pub fn cells(ind: &Indicator, style: MarkStyle) -> String {
    let ascii = match style {
        MarkStyle::Emoji => return emoji(ind),
        MarkStyle::Symbols => false,
        MarkStyle::Ascii => true,
    };
    let (local, cloud, gone) = if ascii { ('L', 'C', 'x') } else { ('▪', '☁', '×') };
    let first = if ind.has_local { local } else { ' ' };
    let second = match (ind.has_server, ind.unreachable && !ind.has_local) {
        (_, true) => gone,
        (true, false) => cloud,
        (false, false) => ' ',
    };
    let third = if ind.possible_match {
        if ascii { '~' } else { '≈' }
    } else {
        match (ind.status, ascii) {
            (SongStatus::InSync, _) => ' ',
            (SongStatus::LocalChanged, false) => '↑',
            (SongStatus::LocalChanged, true) => '^',
            (SongStatus::ServerChanged, false) => '↓',
            (SongStatus::ServerChanged, true) => 'v',
            (SongStatus::Conflict, _) => '!',
            (SongStatus::FirstLinkDiffers, _) => '?',
        }
    };
    [first, second, third].iter().collect()
}

/// The icon a graphical frontend draws for `ind`, by the name of its file
/// (`source-<name>`), most important state first: the same choice as the
/// emoji and as the macOS app's symbols. `None` when there is nothing to
/// show.
pub fn icon_name(ind: &Indicator) -> Option<&'static str> {
    if ind.unreachable && !ind.has_local {
        return Some("unreachable");
    }
    if ind.possible_match {
        return Some("possible-match");
    }
    Some(match (ind.status, ind.has_local, ind.has_server) {
        (SongStatus::Conflict, ..) => "conflict",
        (SongStatus::FirstLinkDiffers, ..) => "choose",
        (SongStatus::LocalChanged, ..) => "local-newer",
        (SongStatus::ServerChanged, ..) => "server-newer",
        (SongStatus::InSync, true, true) => "synced",
        (SongStatus::InSync, false, true) => "server",
        (SongStatus::InSync, true, false) => "local",
        (SongStatus::InSync, false, false) => return None,
    })
}

/// `ind` in words, for a tooltip.
pub fn describe(ind: &Indicator) -> String {
    if ind.unreachable && !ind.has_local {
        return "On a server that cannot be reached".into();
    }
    if ind.possible_match {
        return "Only on a server; may be the same song as a file here".into();
    }
    match (ind.status, ind.has_local, ind.has_server) {
        (SongStatus::Conflict, ..) => "Changed in both places differently",
        (SongStatus::FirstLinkDiffers, ..) => "The copies differ; choose which is right",
        (SongStatus::LocalChanged, ..) => "Changed here; the server is behind",
        (SongStatus::ServerChanged, ..) => "Changed on the server",
        (SongStatus::InSync, true, true) => "On this computer and on a server, in sync",
        (SongStatus::InSync, false, true) => "Only on a server",
        (SongStatus::InSync, true, false) => "Only on this computer",
        (SongStatus::InSync, false, false) => "",
    }
    .into()
}

/// Where a group of songs (an album, a playlist) is: the badge on its tile.
/// Sync state stays with the songs; a group only says where its songs are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spread {
    Local,
    Server,
    /// Some or all songs here and some or all on a server.
    Both,
}

impl Spread {
    /// From how many of a group's `total` songs have a local copy and how
    /// many a server copy. A linked song counts on both sides. `None` for
    /// an empty group.
    pub fn of(local: i64, server: i64, total: i64) -> Option<Spread> {
        match (local > 0, server > 0) {
            _ if total <= 0 => None,
            (true, false) => Some(Spread::Local),
            (false, true) => Some(Spread::Server),
            (true, true) => Some(Spread::Both),
            (false, false) => None,
        }
    }

    /// The icon file a graphical frontend draws: `source-<name>`.
    pub fn icon_name(self) -> &'static str {
        match self {
            Spread::Local => "local",
            Spread::Server => "server",
            Spread::Both => "both",
        }
    }

    /// The terminal mark. Emoji are two cells each, so the computer and the
    /// server keep their own places: five cells, where songs take three.
    pub fn cells(self, style: MarkStyle) -> &'static str {
        match (style, self) {
            (MarkStyle::Emoji, Spread::Local) => "💻   ",
            (MarkStyle::Emoji, Spread::Server) => "  🌐 ",
            (MarkStyle::Emoji, Spread::Both) => "💻🌐 ",
            (MarkStyle::Symbols, Spread::Local) => "▪  ",
            (MarkStyle::Symbols, Spread::Server) => " ☁ ",
            (MarkStyle::Symbols, Spread::Both) => "▪☁ ",
            (MarkStyle::Ascii, Spread::Local) => "L  ",
            (MarkStyle::Ascii, Spread::Server) => " C ",
            (MarkStyle::Ascii, Spread::Both) => "LC ",
        }
    }
}

/// A group's spread in words, for a tooltip.
pub fn spread_note(local: i64, server: i64, total: i64) -> String {
    match Spread::of(local, server, total) {
        Some(Spread::Local) => "On this computer".into(),
        Some(Spread::Server) => "On a server".into(),
        Some(Spread::Both) => {
            format!("{local} of {total} on this computer, {server} of {total} on a server")
        }
        None => String::new(),
    }
}

/// One emoji for the row's state, the one that matters most first, plus a
/// space; three spaces when there is nothing to show.
fn emoji(ind: &Indicator) -> String {
    let e = if ind.unreachable && !ind.has_local {
        "🚫"
    } else if ind.possible_match {
        "🔗"
    } else {
        match (ind.status, ind.has_local, ind.has_server) {
            (SongStatus::Conflict, ..) => "❗",
            (SongStatus::FirstLinkDiffers, ..) => "❓",
            (SongStatus::LocalChanged, ..) => "🔼",
            (SongStatus::ServerChanged, ..) => "🔽",
            (SongStatus::InSync, true, true) => "✅",
            (SongStatus::InSync, false, true) => "🌐",
            (SongStatus::InSync, true, false) => "💻",
            (SongStatus::InSync, false, false) => return "   ".into(),
        }
    };
    format!("{e} ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ind(has_local: bool, has_server: bool, status: SongStatus) -> Indicator {
        Indicator { has_local, has_server, status, possible_match: false, unreachable: false }
    }

    #[test]
    fn each_state_has_its_cells() {
        use SongStatus::*;
        assert_eq!(cells(&ind(true, false, InSync), MarkStyle::Symbols), "▪  ");
        assert_eq!(cells(&ind(false, true, InSync), MarkStyle::Symbols), " ☁ ");
        assert_eq!(cells(&ind(true, true, InSync), MarkStyle::Symbols), "▪☁ ");
        assert_eq!(cells(&ind(true, true, LocalChanged), MarkStyle::Symbols), "▪☁↑");
        assert_eq!(cells(&ind(true, true, ServerChanged), MarkStyle::Symbols), "▪☁↓");
        assert_eq!(cells(&ind(true, true, Conflict), MarkStyle::Symbols), "▪☁!");
        assert_eq!(cells(&ind(true, true, FirstLinkDiffers), MarkStyle::Symbols), "▪☁?");
    }

    #[test]
    fn a_possible_match_and_an_unreachable_copy_show_too() {
        let mut i = ind(false, true, SongStatus::InSync);
        i.possible_match = true;
        assert_eq!(cells(&i, MarkStyle::Symbols), " ☁≈");
        let mut i = ind(false, true, SongStatus::InSync);
        i.unreachable = true;
        assert_eq!(cells(&i, MarkStyle::Symbols), " × ");
    }

    #[test]
    fn the_emoji_set_is_one_wide_emoji_per_state_padded_to_three_cells() {
        use SongStatus::*;
        let e = MarkStyle::Emoji;
        assert_eq!(cells(&ind(true, false, InSync), e), "💻 ");
        assert_eq!(cells(&ind(false, true, InSync), e), "🌐 ");
        assert_eq!(cells(&ind(true, true, InSync), e), "✅ ");
        assert_eq!(cells(&ind(true, true, LocalChanged), e), "🔼 ");
        assert_eq!(cells(&ind(true, true, ServerChanged), e), "🔽 ");
        assert_eq!(cells(&ind(true, true, Conflict), e), "❗ ");
        assert_eq!(cells(&ind(true, true, FirstLinkDiffers), e), "❓ ");
        let mut i = ind(false, true, InSync);
        i.possible_match = true;
        assert_eq!(cells(&i, e), "🔗 ");
        i.possible_match = false;
        i.unreachable = true;
        assert_eq!(cells(&i, e), "🚫 ");
    }

    #[test]
    fn each_state_names_one_icon_and_says_what_it_means() {
        use SongStatus::*;
        let cases = [
            (ind(true, false, InSync), Some("local"), "Only on this computer"),
            (ind(false, true, InSync), Some("server"), "Only on a server"),
            (ind(true, true, InSync), Some("synced"), "On this computer and on a server, in sync"),
            (ind(true, true, LocalChanged), Some("local-newer"), "Changed here; the server is behind"),
            (ind(true, true, ServerChanged), Some("server-newer"), "Changed on the server"),
            (ind(true, true, Conflict), Some("conflict"), "Changed in both places differently"),
            (ind(true, true, FirstLinkDiffers), Some("choose"), "The copies differ; choose which is right"),
            (ind(false, false, InSync), None, ""),
        ];
        for (i, icon, words) in cases {
            assert_eq!(icon_name(&i), icon, "{i:?}");
            assert_eq!(describe(&i), words, "{i:?}");
        }
        let mut i = ind(false, true, InSync);
        i.possible_match = true;
        assert_eq!(icon_name(&i), Some("possible-match"));
        assert_eq!(describe(&i), "Only on a server; may be the same song as a file here");
        i.possible_match = false;
        i.unreachable = true;
        assert_eq!(icon_name(&i), Some("unreachable"));
        assert_eq!(describe(&i), "On a server that cannot be reached");
    }

    #[test]
    fn a_group_of_songs_is_here_on_a_server_or_both() {
        assert_eq!(Spread::of(12, 0, 12), Some(Spread::Local));
        assert_eq!(Spread::of(0, 12, 12), Some(Spread::Server));
        assert_eq!(Spread::of(12, 12, 12), Some(Spread::Both), "every song linked");
        assert_eq!(Spread::of(5, 7, 12), Some(Spread::Both), "some here, the rest on a server");
        assert_eq!(Spread::of(0, 0, 0), None);

        assert_eq!(Spread::Local.icon_name(), "local");
        assert_eq!(Spread::Server.icon_name(), "server");
        assert_eq!(Spread::Both.icon_name(), "both");

        assert_eq!(spread_note(12, 0, 12), "On this computer");
        assert_eq!(spread_note(0, 12, 12), "On a server");
        assert_eq!(spread_note(5, 9, 12), "5 of 12 on this computer, 9 of 12 on a server");
    }

    #[test]
    fn a_groups_cells_put_the_computer_and_the_server_in_their_own_places() {
        assert_eq!(Spread::Local.cells(MarkStyle::Emoji), "💻   ");
        assert_eq!(Spread::Server.cells(MarkStyle::Emoji), "  🌐 ");
        assert_eq!(Spread::Both.cells(MarkStyle::Emoji), "💻🌐 ");
        assert_eq!(Spread::Both.cells(MarkStyle::Symbols), "▪☁ ");
        assert_eq!(Spread::Server.cells(MarkStyle::Ascii), " C ");
    }

    #[test]
    fn the_ascii_set_is_one_cell_per_glyph() {
        use SongStatus::*;
        assert_eq!(cells(&ind(true, false, InSync), MarkStyle::Ascii), "L  ");
        assert_eq!(cells(&ind(true, true, LocalChanged), MarkStyle::Ascii), "LC^");
        assert_eq!(cells(&ind(true, true, ServerChanged), MarkStyle::Ascii), "LCv");
        assert_eq!(cells(&ind(true, true, Conflict), MarkStyle::Ascii), "LC!");
        assert_eq!(cells(&ind(true, true, FirstLinkDiffers), MarkStyle::Ascii), "LC?");
        let mut i = ind(false, true, InSync);
        i.possible_match = true;
        assert_eq!(cells(&i, MarkStyle::Ascii), " C~");
        i.possible_match = false;
        i.unreachable = true;
        assert_eq!(cells(&i, MarkStyle::Ascii), " x ");
    }
}
