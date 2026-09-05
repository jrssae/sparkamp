//! On-disk per-disc tag store: `~/.config/sparkamp/disc_tags.toml`.
//!
//! Disc tags (a gnudb match, the user's edits, or both) must survive an app
//! restart — the disc itself is read-only, so this local cache is the only
//! place they can live, exactly like every CD player's local CDDB cache.
//! Keyed by freedb disc ID. Each record keeps the user's current tags and,
//! when the disc was matched, the untouched official entry — the baseline
//! for "worth submitting?" and for the revision an update must increment.
//!
//! Plain TOML in the config dir (matches the app's storage convention);
//! load-modify-save whole-file, same simple model as `config.toml`.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::xmcd::XmcdEntry;

/// One disc's stored tags.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscTagRecord {
    /// The user's current tags (what the UIs display and rip/submission use).
    pub user: XmcdEntry,
    /// The untouched gnudb match, when the disc was identified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official: Option<XmcdEntry>,
}

/// The whole store: freedb disc ID → record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscTagStore {
    #[serde(default)]
    pub discs: HashMap<String, DiscTagRecord>,
}

impl DiscTagStore {
    /// `~/.config/sparkamp/disc_tags.toml` (same base dir as `config.toml`).
    fn path() -> PathBuf {
        crate::config::Config::config_path()
            .parent()
            .map(|d| d.join("disc_tags.toml"))
            .unwrap_or_else(|| PathBuf::from("disc_tags.toml"))
    }

    /// Load the store; missing or unreadable file is an empty store (the
    /// cache is best-effort — never block disc features on it).
    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|text| Self::from_toml(&text))
            .unwrap_or_default()
    }

    /// Persist the store; errors are ignored (best-effort cache).
    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Some(text) = self.to_toml() {
            let _ = std::fs::write(path, text);
        }
    }

    /// One record (user + official) for a disc.
    // The FFI (lib target) reads through this; the TUI (bin target) iterates
    // `discs` wholesale at startup instead, so it's dead there only.
    #[allow(dead_code)]
    pub fn get(&self, discid: &str) -> Option<&DiscTagRecord> {
        self.discs.get(discid)
    }

    /// Insert/replace a disc's record and persist immediately.
    pub fn set(&mut self, discid: &str, user: XmcdEntry, official: Option<XmcdEntry>) {
        self.discs
            .insert(discid.to_string(), DiscTagRecord { user, official });
        self.save();
    }

    /// Forget a disc's record entirely, so it falls back to whatever the disc
    /// itself says. Returns whether there was anything to forget.
    ///
    /// This is the undo for a wrong match. gnudb returns inexact matches by
    /// design, and accepting one is a single click, so a disc could be
    /// mislabelled with no way back: `set` was the only writer, the record
    /// outranked CD-TEXT everywhere, and it persisted across restarts.
    ///
    /// It removes the user's edits along with the official entry, which is the
    /// point rather than a side effect: "this match is wrong" means the tags
    /// derived from it are wrong too. What the disc carries in its own CD-TEXT
    /// is untouched, because that was never stored here.
    pub fn clear(&mut self, discid: &str) -> bool {
        let removed = self.discs.remove(discid).is_some();
        if removed {
            self.save();
        }
        removed
    }

    fn from_toml(text: &str) -> Option<Self> {
        toml::from_str(text).ok()
    }

    fn to_toml(&self) -> Option<String> {
        toml::to_string_pretty(self).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearing_a_disc_forgets_both_halves_of_its_record() {
        let mut store = DiscTagStore::default();
        let user = XmcdEntry { artist: "Wrong Band".into(), ..Default::default() };
        let official = XmcdEntry { artist: "Wrong Band".into(), ..Default::default() };
        store
            .discs
            .insert("abc123".into(), DiscTagRecord { user, official: Some(official) });

        assert!(store.clear("abc123"), "a stored disc reports as removed");
        assert!(store.get("abc123").is_none(), "nothing may outrank CD-TEXT afterwards");
        assert!(!store.clear("abc123"), "clearing again removes nothing");
        assert!(!store.clear("never-stored"), "an unknown disc is not an error");
    }

    fn entry(artist: &str, titles: &[&str]) -> XmcdEntry {
        XmcdEntry {
            discid: "6f067d08".into(),
            artist: artist.into(),
            album: "Album".into(),
            year: "2001".into(),
            genre: "Rock".into(),
            track_titles: titles.iter().map(|t| t.to_string()).collect(),
            extd: String::new(),
            extt: Vec::new(),
            revision: 2,
        }
    }

    #[test]
    fn toml_round_trip_with_and_without_official() {
        let mut store = DiscTagStore::default();
        store.discs.insert(
            "6f067d08".into(),
            DiscTagRecord {
                user: entry("Edited Artist", &["One", "Two"]),
                official: Some(entry("Official Artist", &["A", "B"])),
            },
        );
        store.discs.insert(
            "0c025603".into(),
            DiscTagRecord {
                user: entry("Manual Only", &["X"]),
                official: None,
            },
        );

        let text = store.to_toml().expect("serialize");
        let back = DiscTagStore::from_toml(&text).expect("parse");
        assert_eq!(back, store);
        assert_eq!(
            back.get("6f067d08").unwrap().official.as_ref().unwrap().artist,
            "Official Artist"
        );
        assert!(back.get("0c025603").unwrap().official.is_none());
    }

    #[test]
    fn garbage_text_is_empty_store() {
        assert!(DiscTagStore::from_toml("not toml [").is_none());
        // load() maps that to default — verified via the None above.
    }
}
