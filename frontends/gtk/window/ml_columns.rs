/// Defines all columns that can appear in both the Media Library window
/// and the ID3 tag editor.  `id3_editable` fields are shown as text entries
/// in the ID3 editor; `read_only` fields are shown as non-editable labels.
struct MlColumnDef {
    id: &'static str,
    header: &'static str,
    expand: bool,
    #[allow(dead_code)]
    id3_editable: bool,
    #[allow(dead_code)]
    default_ml_visible: bool,
    #[allow(dead_code)]
    default_id3_visible: bool,
}

const ALL_COLUMNS: &[MlColumnDef] = &[
    // ── Read-only file data ────────────────────────────────────────────────
    MlColumnDef {
        id: "num",
        header: "#",
        expand: false,
        id3_editable: false,
        default_ml_visible: true,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "filename",
        header: "Filename",
        expand: true,
        id3_editable: false,
        default_ml_visible: true,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "path",
        header: "Path",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "filetype",
        header: "Type",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "bitrate",
        header: "Bitrate",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "channels",
        header: "Ch",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "sample_rate",
        header: "Sample Rate",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "file_size",
        header: "Size",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "added_at",
        header: "Date Added",
        expand: false,
        id3_editable: false,
        default_ml_visible: true,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "file_mtime",
        header: "File Modified",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "bitrate_mode",
        header: "Mode",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "duration",
        header: "Duration",
        expand: false,
        id3_editable: false,
        default_ml_visible: true,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "play_count",
        header: "# Play",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "last_played",
        header: "Last Played",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "last_scanned",
        header: "Last Scanned",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    MlColumnDef {
        id: "artwork_path",
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
    MlColumnDef {
        id: "rg_gain",
        header: "ReplayGain",
        expand: false,
        id3_editable: false,
        default_ml_visible: false,
        default_id3_visible: false,
    },
    // ── Editable ID3 fields ────────────────────────────────────────────────
    MlColumnDef {
        id: "title",
        header: "Title",
        expand: false,
        id3_editable: true,
        default_ml_visible: true,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "artist",
        header: "Artist",
        expand: false,
        id3_editable: true,
        default_ml_visible: true,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "album",
        header: "Album",
        expand: false,
        id3_editable: true,
        default_ml_visible: true,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "album_artist",
        header: "Album Artist",
        expand: false,
        id3_editable: true,
        default_ml_visible: true,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "year",
        header: "Year",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "genre",
        header: "Genre",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "track_num",
        header: "Track #",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "track_total",
        header: "Track Total",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "disc_num",
        header: "Disc",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "disc_total",
        header: "Disc Total",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "bpm",
        header: "BPM",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "comment",
        header: "Comment",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "composer",
        header: "Composer",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "original_artist",
        header: "Original Artist",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "copyright",
        header: "Copyright",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "url",
        header: "URL",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "encoded_by",
        header: "Encoded By",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
    MlColumnDef {
        id: "lyric",
        header: "Lyric",
        expand: false,
        id3_editable: true,
        default_ml_visible: false,
        default_id3_visible: true,
    },
];

/// Re-apply the shared media-library column config (visibility, widths, order)
/// to a ColumnView's named columns. `fixed_leading` is how many pinned columns
/// precede the named ones (the files view has 0, the editor 2 = status +
/// position, the device view 1 = playlist-order). Used so the files view, the
/// playlist editor, and the device view all reflect the same column settings.
fn apply_ml_columns_to(
    col_view: &ColumnView,
    named: &[(String, ColumnViewColumn)],
    state: &Rc<RefCell<AppState>>,
    fixed_leading: u32,
) {
    let (visible_ids, widths, order): (
        Vec<String>,
        std::collections::HashMap<String, i32>,
        Vec<String>,
    ) = {
        let s = state.borrow();
        (
            s.config.media_library.visible_columns.clone(),
            s.config.media_library.ml_file_col_widths.clone(),
            s.config.media_library.ml_file_col_order.clone(),
        )
    };
    for (id, col) in named {
        col.set_visible(visible_ids.contains(id));
        if let Some(&w) = widths.get(id) {
            if w > 0 {
                col.set_fixed_width(w);
            }
        }
    }
    if !order.is_empty() {
        for (_, col) in named {
            col_view.remove_column(col);
        }
        let mut pos = fixed_leading;
        for col_id in &order {
            if let Some((_, col)) = named.iter().find(|(id, _)| id == col_id) {
                col_view.insert_column(pos, col);
                pos += 1;
            }
        }
        for (id, col) in named {
            if !order.contains(id) {
                col_view.insert_column(pos, col);
                pos += 1;
            }
        }
    }
}

/// Human file size: whole KB under 1 MB, one-decimal MB above.
fn format_file_size(bytes: i64) -> String {
    if bytes < 1_000_000 {
        format!("{} KB", bytes / 1_000)
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
}

/// Text shown for a `LibTrack` in a given media-library column. Shared by the
/// device track view so it mirrors the files view's columns.
fn ml_cell_text(t: &crate::media_library::LibTrack, id: &str) -> String {
    match id {
        "num" | "track_num" => t.track_num.map(|n| n.to_string()).unwrap_or_default(),
        "title" => t.title.clone().unwrap_or_else(|| t.filename.clone()),
        "artist" => t.artist.clone().unwrap_or_default(),
        "album" => t.album.clone().unwrap_or_default(),
        "album_artist" => t.album_artist.clone().unwrap_or_default(),
        "duration" => t
            .length_secs
            .map(|s| {
                let ss = s as u64;
                format!("{}:{:02}", ss / 60, ss % 60)
            })
            .unwrap_or_else(|| "-:--".to_string()),
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

fn ml_sort_key(t: &crate::media_library::LibTrack, col: &str) -> String {
    match col {
        "num" => t.sort_keys.num.clone(),
        "title" => t.sort_keys.title.clone(),
        "artist" => t.sort_keys.artist.clone(),
        "album" => t.sort_keys.album.clone(),
        "duration" => t.sort_keys.duration.clone(),
        "filename" => t.sort_keys.filename.clone(),
        "year" => t.sort_keys.year.clone(),
        "genre" => t.sort_keys.genre.clone(),
        "bitrate" => t.sort_keys.bitrate.clone(),
        "channels" => format!("{:02}", t.channels.unwrap_or(0)),
        "sample_rate" => format!("{:010}", t.sample_rate.unwrap_or(0)),
        "file_size" => format!("{:010}", t.file_size.unwrap_or(0)),
        "added_at" => t.added_at.clone().unwrap_or_default(),
        "file_mtime" => t.file_mtime.clone().unwrap_or_default(),
        "bitrate_mode" => t.bitrate_mode.as_deref().unwrap_or("").to_lowercase(),
        "path" => t.path.to_lowercase(),
        "play_count" => format!("{:010}", t.play_count),
        "last_played" => t.last_played.clone().unwrap_or_default(),
        "last_scanned" => t.last_scanned.clone().unwrap_or_default(),
        "comment" => t.sort_keys.comment.clone(),
        "album_artist" => t.sort_keys.album_artist.clone(),
        "disc_num" => format!("{:010}", t.disc_num.unwrap_or(0)),
        "disc_total" => format!("{:010}", t.disc_total.unwrap_or(0)),
        "composer" => t.sort_keys.composer.clone(),
        "original_artist" => t.original_artist.as_deref().unwrap_or("").to_lowercase(),
        "copyright" => t.copyright.as_deref().unwrap_or("").to_lowercase(),
        "url" => t.url.as_deref().unwrap_or("").to_lowercase(),
        "encoded_by" => t.encoded_by.as_deref().unwrap_or("").to_lowercase(),
        "bpm" => t.bpm.as_deref().unwrap_or("").to_lowercase(),
        "lyric" => t.lyric.as_deref().unwrap_or("").to_lowercase(),
        "artwork_path" => t.artwork_path.as_deref().unwrap_or("").to_lowercase(),
        // Unanalyzed tracks ("0" prefix) sort before every real gain ("1"
        // prefix) regardless of direction — same convention as the other
        // "no data yet" columns above, which key off `unwrap_or_default()`
        // landing before real text. A plain `{:012.4}` zero-pad on the raw
        // (possibly negative) dB value would NOT sort correctly here: Rust's
        // sign-aware zero-padding keeps the `-` in front and pads after it,
        // so e.g. -12.5 ("-000012.5000") sorts AFTER -6.2 ("-000006.2000")
        // lexically — backwards. Shifting by +1000 first keeps every
        // realistic gain positive, where fixed-width zero-padding sorts
        // correctly.
        "rg_gain" => t
            .rg_track_gain
            .map(|g| format!("1{:012.4}", g + 1000.0))
            .unwrap_or_else(|| "0".to_string()),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Visualizer draw helpers (module-level so both build() and open_waveform_fullscreen can use them)
// ---------------------------------------------------------------------------

