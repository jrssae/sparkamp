//! The Burn list: a dedicated queue of library files to burn, separate from
//! the active playlist (Winamp-style). One list serves both modes — audio
//! (transcoded to Red Book WAV, capacity in seconds) and data (files as-is,
//! capacity in bytes).

use std::path::PathBuf;

/// One queued file plus the metadata the overlays display and the audio
/// capacity math needs.
#[derive(Debug, Clone, PartialEq)]
pub struct BurnItem {
    pub path: PathBuf,
    /// "Artist - Title" style display line (falls back to the file name).
    pub display: String,
    /// Playing time when known — audio capacity math treats unknown as 0 and
    /// the UIs flag the estimate as incomplete.
    pub duration_secs: Option<u32>,
    /// On-disk size in bytes (data-mode capacity math).
    pub bytes: u64,
}

/// The queue itself. Pure state + math; no IO.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BurnList {
    pub items: Vec<BurnItem>,
}

impl BurnList {
    /// Append, skipping paths already queued. Returns whether it was added.
    pub fn add(&mut self, item: BurnItem) -> bool {
        if self.items.iter().any(|i| i.path == item.path) {
            return false;
        }
        self.items.push(item);
        true
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.items.len() {
            self.items.remove(index);
        }
    }

    /// Swap with the previous/next row; out-of-range indices are no-ops.
    pub fn move_up(&mut self, index: usize) {
        if index > 0 && index < self.items.len() {
            self.items.swap(index, index - 1);
        }
    }

    pub fn move_down(&mut self, index: usize) {
        if index + 1 < self.items.len() {
            self.items.swap(index, index + 1);
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Total playing time (audio mode). Unknown durations count as 0.
    pub fn total_secs(&self) -> u32 {
        self.items
            .iter()
            .map(|i| i.duration_secs.unwrap_or(0))
            .sum()
    }

    /// True when any queued item has no known duration — the audio total is
    /// then a lower bound and the UIs say so.
    pub fn has_unknown_durations(&self) -> bool {
        self.items.iter().any(|i| i.duration_secs.is_none())
    }

    /// Total size (data mode).
    pub fn total_bytes(&self) -> u64 {
        self.items.iter().map(|i| i.bytes).sum()
    }

    /// Whether the audio total exceeds the media's capacity in seconds.
    pub fn over_audio_capacity(&self, capacity_secs: u32) -> bool {
        self.total_secs() > capacity_secs
    }

    /// Whether the data total exceeds the media's free bytes.
    pub fn over_data_capacity(&self, free_bytes: u64) -> bool {
        self.total_bytes() > free_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, secs: Option<u32>, bytes: u64) -> BurnItem {
        BurnItem {
            path: PathBuf::from(format!("/m/{name}")),
            display: name.to_string(),
            duration_secs: secs,
            bytes,
        }
    }

    #[test]
    fn add_dedups_and_totals_accumulate() {
        let mut bl = BurnList::default();
        assert!(bl.add(item("a.mp3", Some(200), 5_000_000)));
        assert!(bl.add(item("b.mp3", Some(300), 7_000_000)));
        assert!(!bl.add(item("a.mp3", Some(999), 1))); // dup path ignored
        assert_eq!(bl.len(), 2);
        assert_eq!(bl.total_secs(), 500);
        assert_eq!(bl.total_bytes(), 12_000_000);
        assert!(!bl.has_unknown_durations());
    }

    #[test]
    fn unknown_durations_flagged_and_counted_as_zero() {
        let mut bl = BurnList::default();
        bl.add(item("a.mp3", Some(100), 1));
        bl.add(item("b.mp3", None, 1));
        assert_eq!(bl.total_secs(), 100);
        assert!(bl.has_unknown_durations());
    }

    #[test]
    fn reorder_and_remove_respect_bounds() {
        let mut bl = BurnList::default();
        bl.add(item("a.mp3", Some(1), 1));
        bl.add(item("b.mp3", Some(2), 2));
        bl.add(item("c.mp3", Some(3), 3));

        bl.move_up(0); // no-op at top
        assert_eq!(bl.items[0].display, "a.mp3");
        bl.move_down(2); // no-op at bottom
        assert_eq!(bl.items[2].display, "c.mp3");

        bl.move_up(2);
        assert_eq!(bl.items[1].display, "c.mp3");
        bl.move_down(0);
        assert_eq!(bl.items[0].display, "c.mp3");

        bl.remove(10); // no-op out of range
        assert_eq!(bl.len(), 3);
        bl.remove(0);
        assert_eq!(bl.items[0].display, "a.mp3");
    }

    #[test]
    fn capacity_checks() {
        let mut bl = BurnList::default();
        bl.add(item("a.mp3", Some(4000), 600_000_000));
        // 80-minute CD ≈ 4800 s.
        assert!(!bl.over_audio_capacity(4800));
        bl.add(item("b.mp3", Some(900), 200_000_000));
        assert!(bl.over_audio_capacity(4800));
        assert!(bl.over_data_capacity(700_000_000));
        assert!(!bl.over_data_capacity(1_000_000_000));
    }
}
