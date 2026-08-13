//! The media-library column table, shared by every frontend.
//!
//! There used to be two of these: a 35-column table in the GTK frontend and a
//! 9-column reimplementation in the TUI, both keyed off the same persisted
//! setting (`config.media_library.visible_columns`). They therefore had to
//! agree, and nothing made them — the TUI drew a `?` header over an empty cell
//! for any of the 26 columns it had never heard of, and the two spelled the
//! duration header differently.
//!
//! One table now. A frontend that cannot render a column says so by leaving
//! [`ColumnDef::tui_width`] unset rather than by keeping its own list.
//!
//! What stays with each frontend is presentation, not content: [`value`]
//! returns the canonical text for a cell, and the TUI pads its duration column
//! and substitutes a dash for an empty artist or album because it draws into a
//! fixed-width terminal.

use crate::media_library::LibTrack;

/// One media-library column: what it is called, which views show it, and how
/// wide it is where width has to be declared up front.
pub struct ColumnDef {
    /// Stable identifier, as persisted in `visible_columns`.
    pub id: &'static str,
    /// Full header, used where there is room for it.
    pub header: &'static str,
    /// Shorter header for width-constrained views. `None` means the full one
    /// already fits.
    pub short_header: Option<&'static str>,
    /// Column width in characters for fixed-width views, and the marker for
    /// "this column can be rendered there at all" — `None` means a terminal
    /// frontend should leave it out rather than draw an empty cell.
    pub tui_width: Option<usize>,
    /// Whether the column should absorb spare horizontal space.
    pub expand: bool,
    /// Shown as an editable entry in the ID3 editor rather than a label.
    pub id3_editable: bool,
    /// In the default media-library column set.
    // Read by the column-picker's "Reset to defaults"; the compiler cannot see
    // that through the GTK closure that uses it.
    #[allow(dead_code)]
    pub default_ml_visible: bool,
    /// In the default ID3-editor column set.
    #[allow(dead_code)]
    pub default_id3_visible: bool,
}

impl ColumnDef {
    /// The header to draw where horizontal space is scarce.
    pub fn short(&self) -> &'static str {
        self.short_header.unwrap_or(self.header)
    }
}

/// Look a column up by its persisted id. `None` for an id no longer defined —
/// `visible_columns` is user-editable TOML and can name anything.
pub fn by_id(id: &str) -> Option<&'static ColumnDef> {
    ALL.iter().find(|c| c.id == id)
}

pub const ALL: &[ColumnDef] = &[
    // ── Read-only file data ────────────────────────────────────────────────
    ColumnDef {
        id: "num",
        tui_width: Some(4),
        short_header: None,
        header: "#",
        expand: false,
        id3_editable: false,
        default_ml_visible: true,
        default_id3_visible: false,
    },
    ColumnDef {
        id: "filename",
        tui_width: Some(24),
        short_header: None,
        header: "Filename",
        expand: true,
        id3_editable: false,
        default_ml_visible: true,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "path",
        tui_width: None,
        short_header: None,
        header: "Path",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "filetype",
        tui_width: None,
        short_header: None,
        header: "Type",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    ColumnDef {
        id: "bitrate",
        tui_width: Some(7),
        short_header: None,
        header: "Bitrate",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    ColumnDef {
        id: "channels",
        tui_width: None,
        short_header: None,
        header: "Ch",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    ColumnDef {
        id: "sample_rate",
        tui_width: None,
        short_header: None,
        header: "Sample Rate",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    ColumnDef {
        id: "file_size",
        tui_width: None,
        short_header: None,
        header: "Size",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    ColumnDef {
        id: "added_at",
        tui_width: None,
        short_header: None,
        header: "Date Added",
        expand: false,
        id3_editable: false,
        default_ml_visible: true,
        default_id3_visible: false,
    },
    ColumnDef {
        id: "file_mtime",
        tui_width: None,
        short_header: None,
        header: "File Modified",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    ColumnDef {
        id: "bitrate_mode",
        tui_width: None,
        short_header: None,
        header: "Mode",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    ColumnDef {
        id: "duration",
        tui_width: Some(6),
        short_header: Some("Len"),
        header: "Duration",
        expand: false,
        id3_editable: false,
        default_ml_visible: true,
        default_id3_visible: false,
    },
    ColumnDef {
        id: "play_count",
        tui_width: None,
        short_header: None,
        header: "# Play",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    ColumnDef {
        id: "last_played",
        tui_width: None,
        short_header: None,
        header: "Last Played",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    ColumnDef {
        id: "last_scanned",
        tui_width: None,
        short_header: None,
        header: "Last Scanned",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    ColumnDef {
        id: "artwork_path",
        tui_width: None,
        short_header: None,
        header: "Artwork",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    // Opt-in (phase 4): empty until a track is analyzed, via the bulk
    // "Analyze ReplayGain" button or the row context menu's "Calculate
    // ReplayGain". Off by default like the other read-only technical
    // columns above — most users never need to see it.
    ColumnDef {
        id: "rg_gain",
        tui_width: None,
        short_header: None,
        header: "ReplayGain",
        expand: false,
        // Editable in the ID3 editor (not a tag frame — it round-trips
        // through the library DB and the file's REPLAYGAIN_TRACK_GAIN tag
        // together, see replaygain::apply_manual_gain_edit).
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    // ── Editable ID3 fields ────────────────────────────────────────────────
    ColumnDef {
        id: "title",
        tui_width: Some(28),
        short_header: None,
        header: "Title",
        expand: false,
        id3_editable: true,
        default_ml_visible: true,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "artist",
        tui_width: Some(22),
        short_header: None,
        header: "Artist",
        expand: false,
        id3_editable: true,
        default_ml_visible: true,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "album",
        tui_width: Some(20),
        short_header: None,
        header: "Album",
        expand: false,
        id3_editable: true,
        default_ml_visible: true,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "album_artist",
        tui_width: None,
        short_header: None,
        header: "Album Artist",
        expand: false,
        id3_editable: true,
        default_ml_visible: true,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "year",
        tui_width: Some(5),
        short_header: None,
        header: "Year",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "genre",
        tui_width: Some(12),
        short_header: None,
        header: "Genre",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "track_num",
        tui_width: None,
        short_header: None,
        header: "Track #",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "track_total",
        tui_width: None,
        short_header: None,
        header: "Track Total",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "disc_num",
        tui_width: None,
        short_header: None,
        header: "Disc",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "disc_total",
        tui_width: None,
        short_header: None,
        header: "Disc Total",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "bpm",
        tui_width: None,
        short_header: None,
        header: "BPM",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "comment",
        tui_width: None,
        short_header: None,
        header: "Comment",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "composer",
        tui_width: None,
        short_header: None,
        header: "Composer",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "original_artist",
        tui_width: None,
        short_header: None,
        header: "Original Artist",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "copyright",
        tui_width: None,
        short_header: None,
        header: "Copyright",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "url",
        tui_width: None,
        short_header: None,
        header: "URL",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "encoded_by",
        tui_width: None,
        short_header: None,
        header: "Encoded By",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    ColumnDef {
        id: "lyric",
        tui_width: None,
        short_header: None,
        header: "Lyric",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
];

pub fn format_file_size(bytes: i64) -> String {
    if bytes < 1_000_000 {
        format!("{} KB", bytes / 1_000)
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
}

pub fn format_last_played(iso_timestamp: &str) -> String {
    if iso_timestamp.is_empty() {
        return String::new();
    }
    let parts: Vec<&str> = iso_timestamp
        .trim_end_matches('Z')
        .split(|c| c == 'T' || c == ':' || c == '-')
        .collect();
    if parts.len() < 5 {
        return iso_timestamp.to_string();
    }
    let year = parts[0];
    let month = parts[1];
    let day = parts[2];
    let hour: u32 = parts.get(3).and_then(|h| h.parse().ok()).unwrap_or(0);
    let minute = parts.get(4).unwrap_or(&"00");
    let (hour_12, am_pm) = if hour == 0 {
        (12, "AM")
    } else if hour < 12 {
        (hour, "AM")
    } else if hour == 12 {
        (12, "PM")
    } else {
        (hour - 12, "PM")
    };
    format!(
        "{}-{}-{} {:02}:{} {}",
        year, month, day, hour_12, minute, am_pm
    )
}

/// The canonical text for a `LibTrack` in a given column.
///
/// Content, not presentation: a frontend that needs padding, a placeholder for
/// an empty field, or a shorter form applies that itself. The TUI does both,
/// because it draws into a fixed-width terminal.
///
/// `artist_as_album_artist` is the F12.2 display fallback
/// (`config.media_library.artist_as_album_artist`), passed through to
/// `play_stats::effective_album_artist` for the "album_artist" column. A view
/// that does not render that column can pass `false`.
pub fn value(t: &LibTrack, id: &str, artist_as_album_artist: bool) -> String {
    match id {
        "num" | "track_num" => t.track_num.map(|n| n.to_string()).unwrap_or_default(),
        "title" => t.title.clone().unwrap_or_else(|| t.filename.clone()),
        "artist" => t.artist.clone().unwrap_or_default(),
        "album" => t.album.clone().unwrap_or_default(),
        "album_artist" => crate::play_stats::effective_album_artist(
            t.artist.as_deref().unwrap_or(""),
            t.album_artist.as_deref().unwrap_or(""),
            artist_as_album_artist,
        ),
        "duration" => crate::model::fmt_secs(t.length_secs),
        "filename" => t.filename.clone(),
        "path" => t.path.clone(),
        "year" => t.year.map(|y| y.to_string()).unwrap_or_default(),
        "genre" => t.genre.clone().unwrap_or_default(),
        "bitrate" => t.bitrate.map(|b| format!("{b}k")).unwrap_or_default(),
        "channels" => match t.channels.unwrap_or(0) {
            0 => String::new(),
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            n => format!("{n}ch"),
        },
        "sample_rate" => t
            .sample_rate
            .map(|s| format!("{:.1} kHz", s as f64 / 1000.0))
            .unwrap_or_default(),
        "file_size" => t.file_size.map(format_file_size).unwrap_or_default(),
        "added_at" => t
            .added_at
            .as_deref()
            .map(format_last_played)
            .unwrap_or_default(),
        "file_mtime" => t
            .file_mtime
            .as_deref()
            .map(format_last_played)
            .unwrap_or_default(),
        "bitrate_mode" => t.bitrate_mode.clone().unwrap_or_default(),
        "filetype" => t.filetype.clone().unwrap_or_default(),
        "play_count" => t.play_count.to_string(),
        "last_played" => t
            .last_played
            .as_deref()
            .map(format_last_played)
            .unwrap_or_default(),
        "last_scanned" => t.last_scanned.clone().unwrap_or_default(),
        "disc_num" => {
            let d = t.disc_num.unwrap_or(0);
            if d == 0 {
                String::new()
            } else if let Some(total) = t.disc_total.filter(|x| *x > 0) {
                format!("{d}/{total}")
            } else {
                d.to_string()
            }
        }
        "disc_total" => t.disc_total.map(|d| d.to_string()).unwrap_or_default(),
        "bpm" => t.bpm.clone().unwrap_or_default(),
        "comment" => t.comment.clone().unwrap_or_default(),
        "composer" => t.composer.clone().unwrap_or_default(),
        "original_artist" => t.original_artist.clone().unwrap_or_default(),
        "copyright" => t.copyright.clone().unwrap_or_default(),
        "url" => t.url.clone().unwrap_or_default(),
        "encoded_by" => t.encoded_by.clone().unwrap_or_default(),
        "lyric" => {
            let ly = t.lyric.as_deref().unwrap_or("");
            if ly.chars().count() > 30 {
                format!("{}…", ly.chars().take(30).collect::<String>())
            } else {
                ly.to_string()
            }
        }
        "artwork_path" => {
            if t.artwork_path.is_some() {
                "Yes".to_string()
            } else {
                String::new()
            }
        }
        // Empty until the track's been through ReplayGain analysis (never
        // falls back to album gain here — that would silently mislabel an
        // unanalyzed track as having a track gain). One decimal place for
        // on-screen brevity; the two-decimal Winamp-compatible format
        // (`format_gain_db`) is for the written tag, not this column.
        "rg_gain" => t.rg_track_gain.map(|g| format!("{g:.1} dB")).unwrap_or_default(),
        _ => String::new(),
    }
}
