//! The Burn list: a dedicated queue of library files to burn, separate from
//! the active playlist (Winamp-style). One list serves both modes — audio
//! (transcoded to Red Book WAV, capacity in seconds) and data (files as-is,
//! capacity in bytes).

use std::path::{Path, PathBuf};

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

/// Per-drive burn queues — each burner owns an independent list, so
/// "Send to Disc Drive → B" queues onto B only.
#[derive(Debug, Clone, Default)]
pub struct BurnQueues {
    queues: std::collections::HashMap<String, BurnList>,
}

impl BurnQueues {
    /// The queue for a drive, created empty on first use.
    pub fn queue(&mut self, drive_id: &str) -> &mut BurnList {
        self.queues.entry(drive_id.to_string()).or_default()
    }

    /// Read-only lookup without creating the queue (used by the burn
    /// overlay's read-only render path, which can't hold `&mut App`).
    pub fn get(&self, drive_id: &str) -> Option<&BurnList> {
        self.queues.get(drive_id)
    }

    /// Drop queues whose drive is no longer attached.
    pub fn remove_gone(&mut self, live: &[&str]) {
        self.queues.retain(|id, _| live.contains(&id.as_str()));
    }
}

/// Result of one batch add: what queued, what was already there, and what
/// could not be read (and therefore was NOT added — an unknown duration
/// would defeat the over-capacity gate).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AddOutcome {
    pub added: usize,
    pub duplicate: usize,
    pub failed: Vec<PathBuf>,
}

impl AddOutcome {
    /// One status line, shared wording across frontends.
    pub fn status_message(&self, drive_label: &str, total: usize) -> String {
        let mut s = format!(
            "Queued {} for burning on {drive_label} ({total} on the list)",
            self.added
        );
        if self.duplicate > 0 {
            s.push_str(&format!(" — {} already queued", self.duplicate));
        }
        s
    }

    /// Multi-line error body listing every skipped file; `None` when all
    /// files were readable.
    pub fn failed_message(&self) -> Option<String> {
        if self.failed.is_empty() {
            return None;
        }
        let mut s =
            String::from("These files could not be read and were not added:\n");
        for p in &self.failed {
            s.push_str(&format!("\n{}", p.display()));
        }
        Some(s)
    }
}

/// Queue a batch. `meta` supplies (display, known duration, bytes) from the
/// caller's library; when the duration is unknown, `probe` reads the file
/// (production: `duration_probe::probe_duration`). Probe failure ⇒ the file
/// is skipped and reported, never queued with an unknown length.
pub fn add_files(
    list: &mut BurnList,
    paths: &[PathBuf],
    meta: impl Fn(&Path) -> (String, Option<u32>, u64),
    probe: impl Fn(&Path) -> Option<u32>,
) -> AddOutcome {
    let mut out = AddOutcome::default();
    for path in paths {
        let (display, known_secs, bytes) = meta(path);
        let secs = match known_secs.or_else(|| probe(path)) {
            Some(s) => s,
            None => {
                out.failed.push(path.clone());
                continue;
            }
        };
        let added = list.add(BurnItem {
            path: path.clone(),
            display,
            duration_secs: Some(secs),
            bytes,
        });
        if added {
            out.added += 1;
        } else {
            out.duplicate += 1;
        }
    }
    out
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

    #[test]
    fn queues_are_isolated_per_drive() {
        let mut q = BurnQueues::default();
        q.queue("/dev/sr0").add(item("a.mp3", Some(1), 1));
        q.queue("/dev/sr1").add(item("b.mp3", Some(2), 2));
        assert_eq!(q.get("/dev/sr0").unwrap().len(), 1);
        assert_eq!(q.get("/dev/sr1").unwrap().len(), 1);
        assert_eq!(q.get("/dev/sr0").unwrap().items[0].display, "a.mp3");
        assert!(q.get("/dev/sr2").is_none());
    }

    #[test]
    fn remove_gone_prunes_unplugged_drives() {
        let mut q = BurnQueues::default();
        q.queue("/dev/sr0").add(item("a.mp3", Some(1), 1));
        q.queue("/dev/sr1").add(item("b.mp3", Some(2), 2));
        q.remove_gone(&["/dev/sr1"]);
        assert!(q.get("/dev/sr0").is_none());
        assert_eq!(q.get("/dev/sr1").unwrap().len(), 1);
    }

    #[test]
    fn add_files_probes_unknown_durations_and_skips_unreadable() {
        use std::path::Path;
        let mut bl = BurnList::default();
        let paths: Vec<PathBuf> =
            ["/m/known.mp3", "/m/probed.mp3", "/m/bad.mp3", "/m/known.mp3"]
                .iter().map(PathBuf::from).collect();
        let meta = |p: &Path| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let secs = (name == "known.mp3").then_some(120);
            (name, secs, 1_000u64)
        };
        let probe = |p: &Path| match p.file_name().unwrap().to_str().unwrap() {
            "probed.mp3" => Some(240),
            _ => None, // bad.mp3 is unreadable; known.mp3 never probed
        };
        let out = add_files(&mut bl, &paths, meta, probe);
        assert_eq!(out.added, 2);
        assert_eq!(out.duplicate, 1); // second known.mp3
        assert_eq!(out.failed, vec![PathBuf::from("/m/bad.mp3")]);
        assert_eq!(bl.len(), 2);
        assert_eq!(bl.total_secs(), 360); // 120 known + 240 probed
        assert!(!bl.has_unknown_durations()); // nothing unknown ever enters
    }

    #[test]
    fn add_outcome_messages() {
        let out = AddOutcome { added: 2, duplicate: 1, failed: vec![PathBuf::from("/m/x.mp3")] };
        let msg = out.status_message("Slimtype DS8A5SH", 5);
        assert!(msg.contains("Queued 2"), "{msg}");
        assert!(msg.contains("Slimtype DS8A5SH"), "{msg}");
        assert!(msg.contains("5 on the list"), "{msg}");
        assert!(msg.contains("1 already queued"), "{msg}");
        let fail = out.failed_message().unwrap();
        assert!(fail.contains("could not be read"), "{fail}");
        assert!(fail.contains("/m/x.mp3"), "{fail}");
        let clean = AddOutcome { added: 1, duplicate: 0, failed: vec![] };
        assert!(clean.failed_message().is_none());
        assert!(!clean.status_message("D", 1).contains("already queued"));
    }
}
