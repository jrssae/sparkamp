//! The three-cell source indicator: where a song's copies are, and whether
//! they agree.
//!
//! Cell one is the local copy, cell two the server copies, cell three the
//! sync state. The glyphs are East Asian "ambiguous width" characters, which
//! some terminals (mostly in CJK locales) draw two cells wide, so there is an
//! ASCII set too.

use super::merge::SongStatus;

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

/// The three cells for `ind`.
pub fn cells(ind: &Indicator, ascii: bool) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ind(has_local: bool, has_server: bool, status: SongStatus) -> Indicator {
        Indicator { has_local, has_server, status, possible_match: false, unreachable: false }
    }

    #[test]
    fn each_state_has_its_cells() {
        use SongStatus::*;
        assert_eq!(cells(&ind(true, false, InSync), false), "▪  ");
        assert_eq!(cells(&ind(false, true, InSync), false), " ☁ ");
        assert_eq!(cells(&ind(true, true, InSync), false), "▪☁ ");
        assert_eq!(cells(&ind(true, true, LocalChanged), false), "▪☁↑");
        assert_eq!(cells(&ind(true, true, ServerChanged), false), "▪☁↓");
        assert_eq!(cells(&ind(true, true, Conflict), false), "▪☁!");
        assert_eq!(cells(&ind(true, true, FirstLinkDiffers), false), "▪☁?");
    }

    #[test]
    fn a_possible_match_and_an_unreachable_copy_show_too() {
        let mut i = ind(false, true, SongStatus::InSync);
        i.possible_match = true;
        assert_eq!(cells(&i, false), " ☁≈");
        let mut i = ind(false, true, SongStatus::InSync);
        i.unreachable = true;
        assert_eq!(cells(&i, false), " × ");
    }

    #[test]
    fn the_ascii_set_is_one_cell_per_glyph() {
        use SongStatus::*;
        assert_eq!(cells(&ind(true, false, InSync), true), "L  ");
        assert_eq!(cells(&ind(true, true, LocalChanged), true), "LC^");
        assert_eq!(cells(&ind(true, true, ServerChanged), true), "LCv");
        assert_eq!(cells(&ind(true, true, Conflict), true), "LC!");
        assert_eq!(cells(&ind(true, true, FirstLinkDiffers), true), "LC?");
        let mut i = ind(false, true, InSync);
        i.possible_match = true;
        assert_eq!(cells(&i, true), " C~");
        i.possible_match = false;
        i.unreachable = true;
        assert_eq!(cells(&i, true), " x ");
    }
}
