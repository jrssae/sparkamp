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
//! | [`mount`]   | Read-only data-disc mount (udisks2) + audio-file listing    | `ensure_mounted` is Linux-only (zbus/udisks2); the walk/list is platform-neutral — macOS calls it directly against the OS's auto-mount path (`OpticalDrive::mount_path`, Task 11) |
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
//! Burning was blind-implemented (no blank media) and first validated against
//! real hardware on 2026-08-31, on a CD-RW in a Slimtype DVD A DS8A5SH. All
//! four live burn tests passed, each one's disc state feeding the next: an
//! erase-first data rewrite, an erase to blank, an audio burn (2 tracks with
//! CD-TEXT, 77 s) and a data burn (3 files + playlist.m3u8, 63 s). Detection
//! re-typed the disc correctly at every step — data, blank, audio, data.
//!
//! They are `#[ignore]`d because they DESTROY the loaded disc, and they need
//! rewritable media actually in a drive; `live_rw_drive` skips otherwise.
//! Run them one at a time (`--test-threads=1`): the live disc tests share the
//! drive and leave it busy for each other, which is a contention failure, not
//! a defect. The hardware test matrix lives in
//! `docs/superpowers/plans/2026-06-23-optical-disc-support.md`, Phases 5–7.
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
// Read-only data-disc mount + audio-file listing. `ensure_mounted` (udisks2
// via `zbus`) is Linux-only and cfg-gated inside the module; the walk/list
// half is platform-neutral so the mac FFI (`sparkamp_disc_mount_list`, Task
// 11) can call it directly against the OS's own auto-mount path without ever
// touching zbus.
pub mod mount;
pub mod rip;
pub mod source;
pub mod tagstore;
pub mod burn_gate;
pub mod toc;
/// udisks2 optical typing — the fallback when `cdrskin -minfo` finds the
/// drive busy because the desktop mounted the disc. Linux only.
#[cfg(target_os = "linux")]
pub mod udisks;
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
    /// True when the media-typing probe could not run, so `is_blank`,
    /// `rewritable`, `kind` and the capacities are defaults rather than
    /// readings.
    ///
    /// On Linux the typing comes from `cdrskin -minfo`, which needs to open
    /// the device, and the OS auto-mounts every data disc — so a burned data
    /// CD-RW types as "not blank, not rewritable", which
    /// [`burn::erase_decision`] can only read as write-once-with-content and
    /// refuse. The disc is fine; we just couldn't look at it. Frontends use
    /// this to say so instead of leaving a burn button mysteriously dead
    /// (2026-08-10).
    #[serde(default)]
    pub typing_unknown: bool,
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
            typing_unknown: false,
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
    /// Whether the drive itself can write, independent of any disc in it.
    ///
    /// A DVD-ROM answers `false` and no disc will ever change that, so its
    /// burn panel is hidden outright rather than shown with permanently dead
    /// buttons. Whether the *loaded medium* can be written is a separate and
    /// much more changeable question — [`burn::erase_decision`]'s.
    ///
    /// Linux reads udisks2's `Drive.MediaCompatibility`. macOS has no
    /// equivalent probe yet and defaults to `true`, which preserves the Mac
    /// app's existing behaviour until `drutil` parsing lands.
    ///
    /// `#[serde(default)]` is load-bearing, not tidiness: this struct crosses
    /// the C FFI, and the Swift app hands drive JSON back to entry points like
    /// `sparkamp_disc_read_cdtext` that deserialize it. A required field makes
    /// every payload written before it existed fail to parse, and those entry
    /// points answer null — indistinguishable, to the caller, from "this disc
    /// has no CD-TEXT". Defaulting true keeps such a drive's burn UI rather
    /// than silently taking it away.
    #[serde(default = "default_supports_writing")]
    pub supports_writing: bool,
    /// Where the disc's files are reachable, when the OS mounts it:
    /// macOS audio CDs mount as a volume of AIFF files (e.g.
    /// `/Volumes/Audio CD`); Linux audio CDs don't mount (playback goes
    /// through `cdda://` URIs against the device node instead).
    pub mount_path: Option<PathBuf>,
}

/// The plain-language state of a non-audio disc, for the status line under the
/// drive's name.
///
/// Deliberately not an error: these are the ordinary things a drive can be
/// doing. They used to share the page's warning banner, which painted "Blank
/// disc — ready to burn." in the same alarm colour as a failure.
///
/// `None` for an audio CD, whose track list says everything this would.
pub fn disc_status_note(media: &MediaInfo) -> Option<&'static str> {
    if media.is_audio_cd {
        return None;
    }
    Some(if !media.present {
        "No disc in the drive. Insert an audio CD to play its tracks."
    } else if media.is_blank {
        "Blank disc — ready to burn."
    } else {
        "Data disc — browse, play, and add its files to your library below."
    })
}

/// Absent from older payloads means "unknown", and an unknown drive keeps its
/// burn UI — the panel's own buttons still refuse media that cannot take a burn.
fn default_supports_writing() -> bool {
    true
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

/// The URIs a whole-disc drag carries: every track's playlist address
/// (`DiscTrackEntry::path`), in TOC order — the container rule (dragging the
/// drive drags every track on it) applied to whatever the drive's overview
/// card currently has cached.
pub fn disc_drag_uris(entries: &[DiscTrackEntry]) -> Vec<String> {
    entries.iter().map(|e| e.path.clone()).collect()
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

    fn entry(number: u8, path: &str) -> DiscTrackEntry {
        DiscTrackEntry {
            number,
            path: path.to_string(),
            title: format!("Track {number}"),
            duration_secs: 180,
        }
    }

    #[test]
    fn disc_drag_uris_carries_every_track_in_toc_order() {
        let entries = vec![
            entry(1, "cdda://1?device=/dev/sr0"),
            entry(2, "cdda://2?device=/dev/sr0"),
            entry(3, "cdda://3?device=/dev/sr0"),
        ];
        assert_eq!(
            disc_drag_uris(&entries),
            vec![
                "cdda://1?device=/dev/sr0".to_string(),
                "cdda://2?device=/dev/sr0".to_string(),
                "cdda://3?device=/dev/sr0".to_string(),
            ]
        );
    }

    #[test]
    fn disc_drag_uris_of_an_empty_disc_is_empty() {
        assert!(disc_drag_uris(&[]).is_empty());
    }
}

#[cfg(test)]
mod status_note_tests {
    use super::*;

    fn media(present: bool, blank: bool, audio: bool) -> MediaInfo {
        MediaInfo {
            present,
            is_audio_cd: audio,
            is_blank: blank,
            ..MediaInfo::none()
        }
    }

    #[test]
    fn an_empty_tray_says_so() {
        assert_eq!(
            disc_status_note(&media(false, false, false)),
            Some("No disc in the drive. Insert an audio CD to play its tracks.")
        );
    }

    #[test]
    fn a_blank_disc_is_reported_as_ready_not_as_a_problem() {
        let note = disc_status_note(&media(true, true, false)).expect("a note");
        assert!(note.contains("ready to burn"), "got: {note}");
    }

    #[test]
    fn a_data_disc_describes_what_can_be_done_with_it() {
        let note = disc_status_note(&media(true, false, false)).expect("a note");
        assert!(note.starts_with("Data disc"), "got: {note}");
    }

    #[test]
    fn an_audio_cd_has_no_note_because_the_track_list_speaks_for_it() {
        assert_eq!(disc_status_note(&media(true, false, true)), None);
    }
}

#[cfg(test)]
mod optical_drive_wire_tests {
    use super::*;

    #[test]
    fn drive_json_written_before_supports_writing_existed_still_parses() {
        // The field crosses the C FFI: `sparkamp_disc_read_cdtext` and friends
        // deserialize an OpticalDrive the Swift app hands back. Adding it as a
        // required field broke every such payload silently — `json_in` returns
        // None and the FFI answers null, which those entry points cannot
        // distinguish from "no CD-TEXT on this disc".
        let old = r#"{
            "id": "/dev/sr0",
            "label": "MATSHITA DVD-RAM UJ8C2",
            "media": {
                "present": true,
                "is_audio_cd": true,
                "is_blank": false,
                "rewritable": false,
                "kind": "Unknown",
                "free_bytes": 0,
                "capacity_bytes": 0
            },
            "toc": null,
            "mount_path": null
        }"#;
        let d: OpticalDrive =
            serde_json::from_str(old).expect("pre-existing drive JSON must still parse");
        assert_eq!(d.id, "/dev/sr0");
        // Defaults to true, matching what macOS reports until drutil parsing
        // lands: a drive we know nothing about keeps its burn UI rather than
        // losing it to a missing field.
        assert!(d.supports_writing);
    }

    #[test]
    fn a_drive_that_cannot_write_survives_the_round_trip() {
        let d = OpticalDrive {
            id: "/dev/sr1".into(),
            label: "READER".into(),
            media: MediaInfo::none(),
            toc: None,
            supports_writing: false,
            mount_path: None,
        };
        let json = serde_json::to_string(&d).expect("serialise");
        let back: OpticalDrive = serde_json::from_str(&json).expect("round-trip");
        assert!(!back.supports_writing, "an explicit false must not be lost");
    }
}
