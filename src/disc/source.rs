//! Which source produced the disc metadata currently shown in the disc view.
//! Precedence is whole-entry (the whole displayed track set comes from one
//! source), so this is a single disc-level classification, not per-track.

/// The origin of the disc's displayed album/artist/track names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscMetaSource {
    /// An official gnudb match (possibly user-tweaked, but gnudb-derived).
    Gnudb,
    /// A user-created/edited tag set with no official gnudb match behind it.
    Edited,
    /// CD-TEXT read off the disc (gnudb had no match).
    CdText,
    /// No metadata — the "Track N" fallback; no badge shown.
    None,
}

impl DiscMetaSource {
    /// Classify from what each per-disc cache holds. `has_official` = an
    /// untouched gnudb match is on file; `has_user_tags` = a displayed/edited
    /// tag set exists; `has_cdtext` = CD-TEXT was read. gnudb/user win over
    /// CD-TEXT (whole-entry precedence).
    pub fn resolve(has_official: bool, has_user_tags: bool, has_cdtext: bool) -> Self {
        if has_official {
            DiscMetaSource::Gnudb
        } else if has_user_tags {
            DiscMetaSource::Edited
        } else if has_cdtext {
            DiscMetaSource::CdText
        } else {
            DiscMetaSource::None
        }
    }

    /// Short pill text, or `None` when there is nothing to badge.
    pub fn badge(self) -> Option<&'static str> {
        match self {
            DiscMetaSource::Gnudb => Some("gnudb"),
            DiscMetaSource::Edited => Some("edited"),
            DiscMetaSource::CdText => Some("CD-TEXT"),
            DiscMetaSource::None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_and_badge_follow_whole_entry_precedence() {
        // official gnudb match → gnudb, regardless of cdtext.
        assert_eq!(DiscMetaSource::resolve(true, true, true), DiscMetaSource::Gnudb);
        assert_eq!(DiscMetaSource::resolve(true, false, false), DiscMetaSource::Gnudb);
        // user tag set, no official → edited (even if cdtext also present).
        assert_eq!(DiscMetaSource::resolve(false, true, true), DiscMetaSource::Edited);
        // only cdtext → CD-TEXT.
        assert_eq!(DiscMetaSource::resolve(false, false, true), DiscMetaSource::CdText);
        // nothing → None, no pill.
        assert_eq!(DiscMetaSource::resolve(false, false, false), DiscMetaSource::None);

        assert_eq!(DiscMetaSource::Gnudb.badge(), Some("gnudb"));
        assert_eq!(DiscMetaSource::Edited.badge(), Some("edited"));
        assert_eq!(DiscMetaSource::CdText.badge(), Some("CD-TEXT"));
        assert_eq!(DiscMetaSource::None.badge(), None);
    }
}
