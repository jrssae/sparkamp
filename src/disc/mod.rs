//! Optical-disc (CD/DVD) support — shared core.
//!
//! ## Module map (start here)
//!
//! | Module      | Owns                                                        | Platform code? |
//! |-------------|-------------------------------------------------------------|----------------|
//! | [`detect`]  | Drive/media/TOC discovery → [`OpticalDrive`]                | glue only: macOS `drutil`+`plutil`, Linux sysfs+`cd-info`; every parser is a plain `&str` fn tested on all OSes |
//! | [`toc`]     | Duration math + playlist entries (AIFF paths / `cdda://`)  | tiny cfg split in `track_entries` |
//! | [`discid`]  | freedb disc ID + `cddb query` args (pure)                  | none |
//! | [`gnudb`]   | CDDB query/read/submit over HTTP (`minreq`)                | none |
//! | [`mount`]   | Read-only data-disc mount (udisks2) + audio-file listing    | Linux-only (zbus/udisks2) |
//! | [`xmcd`]    | Entry parse/build + submission validation                  | none |
//! | [`tagstore`]| Per-disc tag cache on disk (`disc_tags.toml`)              | none |
//! | [`rip`]     | Track → tagged MP3 (GStreamer pipeline per track)          | source arm differs (AIFF vs `cdda`) |
//! | [`burnlist`]| The Burn queue model + capacity math (pure)                | none |
//! | [`burn`]    | WAV prepare, burn/erase command builders + runner          | command-level split: `drutil` (mac) vs `cdrskin`/`xorriso` (Linux) |
//!
//! The FFI for all of it lives in `src/ffi/disc.rs` (JSON in/out, ctx-free —
//! callable from any thread; long ops are blocking by design and the
//! frontends loop on worker threads). Frontends: `frontends/tui/media_library.rs`
//! (direct calls) and `frontends/SparkampMac/Sources/Disc*.swift` (FFI).
//!
//! Useful test commands:
//! - `cargo test --lib disc` — every parser/builder/model test.
//! - `cargo test --lib live_list_drives -- --ignored --nocapture` — real drive.
//! - `cargo test --lib live_gnudb -- --ignored --nocapture` — real gnudb.
//! - `cargo test --lib live_rip -- --ignored --nocapture` — real rip.
//! - `cargo test --lib live_prepare_wav -- --ignored --nocapture` — Red Book WAV.
//!
//! Burning was blind-implemented (no blank media) — the hardware test matrix
//! lives in `docs/superpowers/plans/2026-06-23-optical-disc-support.md`,
//! Phases 5–7.
//!
//! Platform boundaries: Linux reads drives via `/sys` + `cd-info`, macOS via
//! `drutil` and the auto-mounted audio-CD volume's `.TOC.plist`. Both produce
//! the same [`OpticalDrive`] shape, so the GTK/TUI frontends (direct calls)
//! and SparkampMac (JSON-over-FFI) render discs identically.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod burn;
pub mod burnlist;
pub mod cdtext;
pub mod detect;
pub mod discid;
pub mod gnudb;
// Read-only data-disc mount + audio-file listing over udisks2 (Linux-only —
// the `zbus` dependency and the GTK caller (Task 9) are both Linux-gated).
#[cfg(target_os = "linux")]
pub mod mount;
pub mod rip;
pub mod tagstore;
pub mod toc;
pub mod xmcd;

/// One track's position on the disc. `start_frame` is the **CDDB-absolute**
/// frame (75 frames = 1 s), i.e. LBA **+ 150** (the 2-second lead-in pregap).
/// The detectors are responsible for this: macOS `.TOC.plist` "Start Block"
/// values are already absolute (track 1 reads 150), while libcdio/GStreamer
/// report the post-pregap LSN and the Linux detector adds 150. Get this wrong
/// and every freedb disc-ID is wrong and gnudb never matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TocTrack {
    pub number: u8,
    pub start_frame: u32,
    pub is_audio: bool,
}

/// Full table of contents for the loaded disc. `leadout_frame` is CDDB-absolute
/// like the track offsets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscToc {
    pub tracks: Vec<TocTrack>,
    pub leadout_frame: u32,
}

/// Writable-media kind, for the burn phases. `Unknown` covers pressed discs
/// and anything the probe couldn't classify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    CdR,
    CdRw,
    DvdR,
    DvdRw,
    DvdRam,
    Unknown,
}

/// What kind of media is in the drive and what we can do with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub present: bool,
    pub is_audio_cd: bool,
    pub is_blank: bool,
    pub rewritable: bool,
    pub kind: MediaKind,
    pub free_bytes: u64,
    pub capacity_bytes: u64,
}

impl MediaInfo {
    /// Empty tray.
    pub fn none() -> Self {
        MediaInfo {
            present: false,
            is_audio_cd: false,
            is_blank: false,
            rewritable: false,
            kind: MediaKind::Unknown,
            free_bytes: 0,
            capacity_bytes: 0,
        }
    }
}

/// One physical optical drive. Every drive present is listed in the sidebar,
/// exactly like each external device — never collapsed to a single "the drive".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpticalDrive {
    /// Stable per-drive id used for sidebar identity + subprocess targeting:
    /// Linux device node (e.g. `/dev/sr0`); macOS `drutil` drive index.
    pub id: String,
    /// Human label from the drive (vendor + model, e.g. "MATSHITA DVD-RAM UJ8C2").
    pub label: String,
    pub media: MediaInfo,
    /// TOC when an audio disc is loaded; `None` when blank, data-only or empty.
    pub toc: Option<DiscToc>,
    /// Where the disc's files are reachable, when the OS mounts it:
    /// macOS audio CDs mount as a volume of AIFF files (e.g.
    /// `/Volumes/Audio CD`); Linux audio CDs don't mount (playback goes
    /// through `cdda://` URIs against the device node instead).
    pub mount_path: Option<PathBuf>,
}

impl OpticalDrive {
    /// One-line loaded-media state for sidebar rows, e.g. "Audio CD (8 tracks)",
    /// "Blank CD-R", "Data disc", "No disc".
    pub fn media_summary(&self) -> String {
        if !self.media.present {
            return "No disc".to_string();
        }
        if self.media.is_audio_cd {
            let n = self.toc.as_ref().map(|t| t.tracks.len()).unwrap_or(0);
            return format!("Audio CD ({n} track{})", if n == 1 { "" } else { "s" });
        }
        if self.media.is_blank {
            let kind = match self.media.kind {
                MediaKind::CdR => "CD-R",
                MediaKind::CdRw => "CD-RW",
                MediaKind::DvdR => "DVD-R",
                MediaKind::DvdRw => "DVD-RW",
                MediaKind::DvdRam => "DVD-RAM",
                MediaKind::Unknown => "disc",
            };
            return format!("Blank {kind}");
        }
        "Data disc".to_string()
    }
}

/// A ready-to-add playlist entry for one disc track: the platform-appropriate
/// path/URI plus display metadata known from the TOC (titles come later from
/// gnudb; until then "Track N").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscTrackEntry {
    pub number: u8,
    /// What goes in `Track.path`: macOS the mounted AIFF path; Linux a
    /// `cdda://N?device=/dev/srX` pseudo-URI the engine understands.
    pub path: String,
    pub title: String,
    pub duration_secs: u32,
}

/// Split a `cdda://N?device=/dev/srX` pseudo-URI (built by
/// [`toc::track_entries`]) into its track part and device node. `None` when
/// the string isn't a cdda URI; the device is `None` when the URI carries no
/// `?device=` suffix. The engine's loader and the rip source builder both
/// parse through here, so the URI format has one producer and one consumer
/// shape.
pub fn parse_cdda_uri(uri: &str) -> Option<(&str, Option<&str>)> {
    let rest = uri.strip_prefix("cdda://")?;
    Some(match rest.split_once("?device=") {
        Some((track, device)) => (track, Some(device)),
        None => (rest, None),
    })
}

/// Display/tag metadata for one disc track after applying the xmcd sampler
/// convention: a track title of the form "Artist / Title" carries a per-track
/// artist, and the disc-level artist is demoted to album artist. Plain titles
/// keep the disc artist and an empty album artist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackMeta {
    pub artist: String,
    pub title: String,
    pub album_artist: String,
}

/// One shared rule for the sampler split — playlist adds, tag-edit
/// propagation, and rip tagging must all agree on it.
pub fn track_meta(raw_title: &str, disc_artist: &str) -> TrackMeta {
    match raw_title.split_once(" / ") {
        Some((artist, title)) => TrackMeta {
            artist: artist.to_string(),
            title: title.to_string(),
            album_artist: disc_artist.to_string(),
        },
        None => TrackMeta {
            artist: disc_artist.to_string(),
            title: raw_title.to_string(),
            album_artist: String::new(),
        },
    }
}

#[cfg(test)]
mod shared_tests {
    use super::*;

    #[test]
    fn parse_cdda_uri_variants() {
        assert_eq!(
            parse_cdda_uri("cdda://3?device=/dev/sr0"),
            Some(("3", Some("/dev/sr0")))
        );
        assert_eq!(parse_cdda_uri("cdda://12"), Some(("12", None)));
        assert_eq!(parse_cdda_uri("/Volumes/Audio CD/1 Track.aiff"), None);
        assert_eq!(parse_cdda_uri("file:///x.mp3"), None);
    }

    #[test]
    fn track_meta_sampler_split() {
        let plain = track_meta("Song", "Band");
        assert_eq!(plain.artist, "Band");
        assert_eq!(plain.title, "Song");
        assert!(plain.album_artist.is_empty());

        let split = track_meta("Guest / Tune", "Various");
        assert_eq!(split.artist, "Guest");
        assert_eq!(split.title, "Tune");
        assert_eq!(split.album_artist, "Various");
    }
}
