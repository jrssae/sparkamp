//! Tag sync between a library file and its copy on a device.
//!
//! The engine compares a snapshot ("baseline") taken at the last sync against
//! the current tags on each side and decides which way to propagate. Only the
//! [`decide`] logic and [`tag_hash`] are pure; reading and writing tags is I/O
//! via the `id3` crate.
//!
//! Syncable fields: the common text tags plus a 0–5 star rating and a play
//! count, both carried in the ID3 POPM (Popularimeter) frame.

// The GTK Sync action wires these in; unreferenced in the macOS bin until then.
#![allow(dead_code)]

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The syncable tag fields of one file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagState {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub genre: String,
    pub comment: String,
    pub track_num: Option<i64>,
    pub year: Option<i64>,
    /// 0–5 stars.
    pub rating: u8,
    pub play_count: u64,
}

/// Map a raw POPM rating byte (0–255) to 0–5 stars (Windows Media Player
/// convention).
pub fn popm_to_stars(raw: u8) -> u8 {
    match raw {
        0 => 0,
        1..=31 => 1,
        32..=95 => 2,
        96..=159 => 3,
        160..=223 => 4,
        _ => 5,
    }
}

/// Map 0–5 stars back to a representative POPM rating byte.
pub fn stars_to_popm(stars: u8) -> u8 {
    match stars {
        0 => 0,
        1 => 1,
        2 => 64,
        3 => 128,
        4 => 196,
        _ => 255,
    }
}

/// A stable, deterministic hash of the syncable fields (FNV-1a, hex). Used as
/// the per-pair baseline so a later sync can tell which side changed.
pub fn tag_hash(s: &TagState) -> String {
    let canonical = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{:?}\u{1f}{:?}\u{1f}{}\u{1f}{}",
        s.title,
        s.artist,
        s.album,
        s.album_artist,
        s.genre,
        s.comment,
        s.track_num,
        s.year,
        s.rating,
        s.play_count,
    );
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in canonical.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The current state of one side of a pair, for [`decide`].
#[derive(Debug, Clone)]
pub struct SideState {
    pub hash: String,
    /// File modification time (seconds), used only for the both-changed tiebreak.
    pub mtime: i64,
}

/// What a sync should do for one pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncAction {
    /// No change on either side.
    None,
    /// Device tags are newer — copy them into the library file.
    DeviceToLibrary,
    /// Library tags are newer — copy them onto the device file.
    LibraryToDevice,
    /// The library file is gone (offer to unpair).
    MissingLibrary,
    /// The device file is gone.
    MissingDevice,
    /// Both files are gone.
    MissingBoth,
    /// Both sides' tags changed since the baseline — needs the user to choose.
    Conflict,
}

/// One tag field that differs between the computer and device copies of a song,
/// for the conflict dialog. Only differing fields are produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDiff {
    pub label: String,
    pub computer: String,
    pub device: String,
}

fn opt_num(v: Option<i64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_default()
}

fn stars(n: u8) -> String {
    if n == 0 {
        "—".to_string()
    } else {
        "★".repeat(n.min(5) as usize)
    }
}

/// Compute the differing syncable fields between the computer (`lib`) and the
/// device (`dev`) tag states. Identical fields are omitted; an empty result
/// means there is no real conflict.
pub fn tag_field_diffs(lib: &TagState, dev: &TagState) -> Vec<FieldDiff> {
    let mut out = Vec::new();
    let mut push = |label: &str, a: String, b: String| {
        if a != b {
            out.push(FieldDiff {
                label: label.to_string(),
                computer: a,
                device: b,
            });
        }
    };
    push("Title", lib.title.clone(), dev.title.clone());
    push("Artist", lib.artist.clone(), dev.artist.clone());
    push("Album", lib.album.clone(), dev.album.clone());
    push("Album artist", lib.album_artist.clone(), dev.album_artist.clone());
    push("Genre", lib.genre.clone(), dev.genre.clone());
    push("Comment", lib.comment.clone(), dev.comment.clone());
    push("Track", opt_num(lib.track_num), opt_num(dev.track_num));
    push("Year", opt_num(lib.year), opt_num(dev.year));
    push("Rating", stars(lib.rating), stars(dev.rating));
    push("Play count", lib.play_count.to_string(), dev.play_count.to_string());
    out
}

/// Decide the sync direction for one pair from the baseline and the current
/// per-side state.
///
/// Only-device-changed → device wins; only-library-changed → library wins;
/// both changed → [`SyncAction::Conflict`] (the user chooses); neither →
/// nothing.
pub fn decide(baseline: &str, lib: Option<&SideState>, dev: Option<&SideState>) -> SyncAction {
    match (lib, dev) {
        (None, None) => SyncAction::MissingBoth,
        (None, Some(_)) => SyncAction::MissingLibrary,
        (Some(_), None) => SyncAction::MissingDevice,
        (Some(l), Some(d)) => {
            let lib_changed = l.hash != baseline;
            let dev_changed = d.hash != baseline;
            match (lib_changed, dev_changed) {
                (false, false) => SyncAction::None,
                (false, true) => SyncAction::DeviceToLibrary,
                (true, false) => SyncAction::LibraryToDevice,
                (true, true) => SyncAction::Conflict,
            }
        }
    }
}

/// A stable hash of an ordered playlist entry list (basenames), used as the
/// per-playlist sync baseline so a later sync can tell which side changed.
/// Order- and duplicate-sensitive (a reorder or a repeated entry changes it).
pub fn entries_hash(entries: &[String]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for e in entries {
        for b in e.bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0x1f; // unit separator between entries
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Which way to sync one playlist between the library and a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaylistSyncDir {
    /// In sync — nothing to do.
    None,
    /// Library is the changed side — push it to the device.
    Push,
    /// Device is the changed side — pull it into the library.
    Pull,
    /// Both sides changed since the baseline — ask the user.
    Conflict,
}

/// Decide which way a single playlist should sync, from the per-playlist
/// baseline hash and the current library/device entry hashes.
///
/// - No baseline (never synced via the baseline): if the device copy is absent
///   there is nothing to compare (caller skips); if present and equal it is
///   already in sync; if present and different the library wins (legacy push,
///   establishing the baseline).
/// - With a baseline: only-library-changed → push; only-device-changed → pull;
///   the device copy missing → push (recreate); both changed → conflict.
pub fn decide_playlist(
    baseline: Option<&str>,
    device_present: bool,
    lib_hash: &str,
    dev_hash: &str,
) -> PlaylistSyncDir {
    match baseline {
        None => {
            if !device_present {
                PlaylistSyncDir::None
            } else if lib_hash == dev_hash {
                PlaylistSyncDir::None
            } else {
                PlaylistSyncDir::Push
            }
        }
        Some(base) => {
            if !device_present {
                return PlaylistSyncDir::Push;
            }
            let lib_changed = lib_hash != base;
            let dev_changed = dev_hash != base;
            match (lib_changed, dev_changed) {
                (false, false) => PlaylistSyncDir::None,
                (true, false) => PlaylistSyncDir::Push,
                (false, true) => PlaylistSyncDir::Pull,
                (true, true) => PlaylistSyncDir::Conflict,
            }
        }
    }
}

/// Read the syncable tags of a file. Returns [`TagState::default`] when the
/// file has no readable ID3 tag.
pub fn read_tag_state(path: &Path) -> TagState {
    use id3::TagLike;
    let Ok(tag) = id3::Tag::read_from_path(path) else {
        return TagState::default();
    };
    let popm = tag.get("POPM").and_then(|f| f.content().popularimeter());
    TagState {
        title: tag.title().unwrap_or_default().to_string(),
        artist: tag.artist().unwrap_or_default().to_string(),
        album: tag.album().unwrap_or_default().to_string(),
        album_artist: tag.album_artist().unwrap_or_default().to_string(),
        genre: tag.genre().unwrap_or_default().to_string(),
        comment: tag.comments().next().map(|c| c.text.clone()).unwrap_or_default(),
        track_num: tag.track().map(|n| n as i64),
        year: tag.year().map(|y| y as i64),
        rating: popm.map(|p| popm_to_stars(p.rating)).unwrap_or(0),
        play_count: popm.map(|p| p.counter).unwrap_or(0),
    }
}

/// Write the fields of `from` onto the existing tag of `to` (preserving other
/// frames), then save. Used to propagate the winning side's tags.
pub fn apply_tags(from: &TagState, to: &Path) -> id3::Result<()> {
    use id3::TagLike;
    let mut tag = id3::Tag::read_from_path(to).unwrap_or_default();
    tag.set_title(from.title.clone());
    tag.set_artist(from.artist.clone());
    tag.set_album(from.album.clone());
    tag.set_album_artist(from.album_artist.clone());
    tag.set_genre(from.genre.clone());
    if let Some(n) = from.track_num {
        if n > 0 {
            tag.set_track(n as u32);
        }
    }
    if let Some(y) = from.year {
        tag.set_year(y as i32);
    }
    // Comment: replace any existing.
    tag.remove("COMM");
    if !from.comment.is_empty() {
        tag.add_frame(id3::frame::Comment {
            lang: "eng".to_string(),
            description: String::new(),
            text: from.comment.clone(),
        });
    }
    // Rating + play count via POPM.
    tag.remove("POPM");
    tag.add_frame(id3::frame::Popularimeter {
        user: "Sparkamp".to_string(),
        rating: stars_to_popm(from.rating),
        counter: from.play_count,
    });
    tag.write_to_path(to, id3::Version::Id3v24)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> TagState {
        TagState {
            title: "T".into(),
            artist: "A".into(),
            rating: 3,
            play_count: 7,
            ..Default::default()
        }
    }

    #[test]
    fn tag_hash_is_stable_and_sensitive() {
        let a = base();
        assert_eq!(tag_hash(&a), tag_hash(&a.clone()));
        let mut b = base();
        b.play_count = 8;
        assert_ne!(tag_hash(&a), tag_hash(&b));
        let mut c = base();
        c.rating = 4;
        assert_ne!(tag_hash(&a), tag_hash(&c));
    }

    #[test]
    fn popm_star_mapping_roundtrips_endpoints() {
        assert_eq!(popm_to_stars(0), 0);
        assert_eq!(popm_to_stars(255), 5);
        assert_eq!(popm_to_stars(128), 3);
        for stars in 0..=5u8 {
            assert_eq!(popm_to_stars(stars_to_popm(stars)), stars);
        }
    }

    #[test]
    fn entries_hash_is_order_and_dup_sensitive() {
        let a = vec!["x.mp3".to_string(), "y.mp3".to_string()];
        let b = vec!["y.mp3".to_string(), "x.mp3".to_string()];
        let c = vec!["x.mp3".to_string(), "y.mp3".to_string(), "x.mp3".to_string()];
        assert_eq!(entries_hash(&a), entries_hash(&a.clone()));
        assert_ne!(entries_hash(&a), entries_hash(&b)); // reorder changes it
        assert_ne!(entries_hash(&a), entries_hash(&c)); // duplicate changes it
        assert_ne!(entries_hash(&a), entries_hash(&[])); // empty differs
    }

    #[test]
    fn decide_playlist_covers_branches() {
        // No baseline.
        assert_eq!(decide_playlist(None, false, "L", "D"), PlaylistSyncDir::None);
        assert_eq!(decide_playlist(None, true, "X", "X"), PlaylistSyncDir::None);
        assert_eq!(decide_playlist(None, true, "L", "D"), PlaylistSyncDir::Push);
        // With baseline "B".
        assert_eq!(decide_playlist(Some("B"), false, "L", "D"), PlaylistSyncDir::Push);
        assert_eq!(decide_playlist(Some("B"), true, "B", "B"), PlaylistSyncDir::None);
        assert_eq!(decide_playlist(Some("B"), true, "L", "B"), PlaylistSyncDir::Push);
        assert_eq!(decide_playlist(Some("B"), true, "B", "D"), PlaylistSyncDir::Pull);
        assert_eq!(decide_playlist(Some("B"), true, "L", "D"), PlaylistSyncDir::Conflict);
    }

    #[test]
    fn tag_field_diffs_only_lists_differences() {
        let a = TagState {
            title: "T".into(),
            artist: "A".into(),
            rating: 3,
            play_count: 7,
            comment: "same".into(),
            ..Default::default()
        };
        let mut b = a.clone();
        // No differences.
        assert!(tag_field_diffs(&a, &b).is_empty());
        // Change comment + rating.
        b.comment = "different".into();
        b.rating = 5;
        let diffs = tag_field_diffs(&a, &b);
        let labels: Vec<&str> = diffs.iter().map(|d| d.label.as_str()).collect();
        assert_eq!(labels, vec!["Comment", "Rating"]);
        assert_eq!(diffs[0].computer, "same");
        assert_eq!(diffs[0].device, "different");
        assert_eq!(diffs[1].computer, "★★★");
        assert_eq!(diffs[1].device, "★★★★★");
    }

    #[test]
    fn decide_covers_every_branch() {
        let baseline = "BASE";
        let same = SideState { hash: "BASE".into(), mtime: 100 };
        let changed_old = SideState { hash: "X".into(), mtime: 50 };
        let changed_new = SideState { hash: "Y".into(), mtime: 200 };

        // Nothing changed.
        assert_eq!(decide(baseline, Some(&same), Some(&same)), SyncAction::None);
        // Only device changed.
        assert_eq!(
            decide(baseline, Some(&same), Some(&changed_new)),
            SyncAction::DeviceToLibrary
        );
        // Only library changed.
        assert_eq!(
            decide(baseline, Some(&changed_new), Some(&same)),
            SyncAction::LibraryToDevice
        );
        // Both changed → conflict (user decides), regardless of mtime.
        assert_eq!(
            decide(baseline, Some(&changed_old), Some(&changed_new)),
            SyncAction::Conflict
        );
        assert_eq!(
            decide(baseline, Some(&changed_new), Some(&changed_old)),
            SyncAction::Conflict
        );
        // Missing sides.
        assert_eq!(decide(baseline, None, Some(&same)), SyncAction::MissingLibrary);
        assert_eq!(decide(baseline, Some(&same), None), SyncAction::MissingDevice);
        assert_eq!(decide(baseline, None, None), SyncAction::MissingBoth);
    }
}
