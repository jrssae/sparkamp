//! Optical-disc (CD/DVD) support — shared core.
//!
//! Owns everything platform-neutral about disc handling: the TOC data model,
//! duration math, and (later phases) the freedb disc-ID, gnudb client, rip
//! pipeline, and burn orchestration. Platform code is confined to
//! [`detect`]: Linux reads drives via `/sys` + `cd-info`, macOS via `drutil`
//! and the auto-mounted audio-CD volume's `.TOC.plist`. Both produce the same
//! [`OpticalDrive`] shape, so the GTK/TUI frontends (direct calls) and
//! SparkampMac (JSON-over-FFI) render discs identically.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod detect;
pub mod discid;
pub mod gnudb;
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
