//! Media-library overlay rendering: files table and playlists list.

#[rustfmt::skip]
use super::imports::*;

/// Render the full-screen media library browser.
///
/// ## Layout (Winamp-style)
///
/// ```text
/// ┌──────────────────────────────────────────────────────────────┐
/// │ Sparkamp — Media Library                                      │  ← title border
/// │ ▶ Files    │ Search: ________________                         │
/// │   Playlists│ Artist │ Title  │ Album │ Len                    │  ← column headers
/// │            │ row 1 …                                          │
/// │            │ …                                                │
/// │            │ Esc:close  Tab:tab  /:search  Enter:add  s:sort  │  ← hint/toast
/// └──────────────────────────────────────────────────────────────┘
/// ```
///
/// The left sidebar shows the navigation sections; the right pane shows the
/// content for the active section.  Occupies the full terminal area.
pub(super) fn draw_media_library(
    frame: &mut Frame,
    state: &MediaLibraryState,
    toast: Option<&str>,
    rip_progress: Option<&(usize, usize, String)>,
    burn_phase: Option<&str>,
    burn_list: &crate::disc::burnlist::BurnList,
    area: Rect,
) {
    // Erase the player/playlist underneath so there are no legibility issues.
    frame.render_widget(Clear, area);

    // Outer border.
    let block = Block::default()
        .title(Span::styled(
            " Sparkamp — Media Library ",
            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(C_ACCENT));
    frame.render_widget(block, area);

    // Work inside the border.
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    if inner.height < 4 {
        return;
    }

    // Split horizontally: narrow sidebar on the left, content on the right.
    const SIDEBAR_W: u16 = 13;
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(SIDEBAR_W), Constraint::Min(1)])
        .split(inner);

    // ── Left sidebar: vertical tab list ──────────────────────────────────
    let sidebar_items: Vec<ListItem> = [
        ("Files", MediaLibraryTab::Files),
        ("Playlists", MediaLibraryTab::Playlists),
        ("Discs", MediaLibraryTab::Discs),
    ]
    .iter()
    .map(|(label, tab)| {
        if *tab == state.tab {
            ListItem::new(Span::styled(
                format!("▶ {label}"),
                Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
            ))
        } else {
            ListItem::new(Span::styled(
                format!("  {label}"),
                Style::default().fg(C_DIM),
            ))
        }
    })
    .collect();

    let sidebar = List::new(sidebar_items).block(
        Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(C_DIM)),
    );
    frame.render_widget(sidebar, cols[0]);

    // ── Right pane ────────────────────────────────────────────────────────
    // Split: search bar (1 row), content (rest − 1), hint/toast bar (1 row).
    let right = cols[1];
    let pane = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // search bar
            Constraint::Min(1),    // content
            Constraint::Length(1), // hint / toast
        ])
        .split(right);

    // Top bar: add-path prompt takes priority over search bar.
    let (top_str, top_style) = if let Some(ref buf) = state.add_input {
        (format!("Add path: {buf}|"), Style::default().fg(C_ACCENT))
    } else if state.search_active {
        (
            format!("Search: {}|", state.search_query),
            Style::default().fg(C_WARN),
        )
    } else if !state.search_query.is_empty() {
        (
            format!("Search: {}", state.search_query),
            Style::default().fg(C_WARN),
        )
    } else {
        (" /: search".to_string(), Style::default().fg(C_DIM))
    };
    frame.render_widget(Paragraph::new(Span::styled(top_str, top_style)), pane[0]);

    // Content.
    match state.tab {
        MediaLibraryTab::Files => draw_ml_files(frame, state, pane[1]),
        MediaLibraryTab::Playlists => draw_ml_playlists(frame, state, pane[1]),
        MediaLibraryTab::Discs => draw_ml_discs(frame, state, pane[1]),
    }

    // Hint / toast bar — a running burn/rip's progress wins, then status
    // messages, then the per-tab key hints.
    let hint_line = if let Some(phase) = burn_phase {
        Line::from(Span::styled(
            format!("{phase} — c: cancel"),
            Style::default().fg(C_PLAYING),
        ))
    } else if let Some((i, n, title)) = rip_progress {
        Line::from(Span::styled(
            format!("Ripping {}/{} · {} — c: cancel", i + 1, n, title),
            Style::default().fg(C_PLAYING),
        ))
    } else if let Some(msg) = toast {
        Line::from(Span::styled(msg, Style::default().fg(C_PLAYING)))
    } else if state.add_input.is_some() {
        Line::from(vec![
            hint("Enter", "add path"),
            sep(),
            hint("Esc", "cancel"),
        ])
    } else if state.search_active {
        Line::from(hint("Esc", "exit search"))
    } else if state.tab == MediaLibraryTab::Discs {
        Line::from(vec![
            hint("Esc", "close"),
            sep(),
            hint("Tab", "tab"),
            sep(),
            hint("Enter", "add track"),
            sep(),
            hint("a", "add disc"),
            sep(),
            hint("m", "identify"),
            sep(),
            hint("e", "tags"),
            sep(),
            hint("u", "submit"),
            sep(),
            hint("g", "rip"),
            sep(),
            hint("b", "burn"),
            sep(),
            hint("←→", "drive"),
            sep(),
            hint("r", "rescan"),
        ])
    } else {
        Line::from(vec![
            hint("Esc", "close"),
            sep(),
            hint("Tab", "tab"),
            sep(),
            hint("/", "search"),
            sep(),
            hint("Enter", "add"),
            sep(),
            hint("←→", "scroll cols"),
            sep(),
            hint("s", "sort"),
            sep(),
            hint("a", "add to ML"),
            sep(),
            hint("i", "help"),
            sep(),
            Span::styled("Alt+z/x/c/v/b/j", Style::default().fg(C_DIM)),
        ])
    };
    frame.render_widget(Paragraph::new(hint_line), pane[2]);

    // Disc overlays paint last, centered atop everything else.
    if let Some((matches, selected)) = &state.gnudb_matches {
        draw_gnudb_matches(frame, matches, *selected, inner);
    }
    if let Some(ed) = &state.tag_edit {
        draw_disc_tag_editor(frame, ed, inner);
    }
    if let Some(selected) = state.submit_category {
        draw_submit_category(frame, selected, inner);
    }
    if let Some(buf) = &state.submit_email {
        draw_submit_email(frame, buf, inner);
    }
    if let Some(rip) = &state.rip {
        draw_rip_setup(frame, rip, state, inner);
    }
    if let Some(burn) = &state.burn {
        draw_burn_setup(frame, burn, burn_list, inner);
    }
}

/// Burn overlay: the queued list with capacity totals, audio/data mode, and
/// the erase confirmation prompt when the media needs wiping first.
fn draw_burn_setup(
    frame: &mut Frame,
    burn: &BurnSetupState,
    list: &crate::disc::burnlist::BurnList,
    area: Rect,
) {
    let w = area.width.saturating_sub(6).min(72).max(44);
    let rows = list.len() as u16 + 5;
    let h = rows.min(area.height.saturating_sub(2)).max(8);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let title = if burn.confirm_erase {
        " ERASE DISC? contents are destroyed — y: erase & burn · other: back "
    } else {
        " Burn — t: audio/data · x: remove · [ ]: reorder · Enter: start · Esc "
    };
    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default().fg(if burn.confirm_erase { C_WARN } else { C_ACCENT }),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if burn.confirm_erase { C_WARN } else { C_ACCENT }));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let secs = list.total_secs();
    let mb = list.total_bytes() / 1_000_000;
    let mode = if burn.audio {
        format!(
            "Mode: AUDIO CD — {}:{:02} queued{}",
            secs / 60,
            secs % 60,
            if list.has_unknown_durations() {
                " (some durations unknown)"
            } else {
                ""
            }
        )
    } else {
        format!("Mode: DATA DISC — {mb} MB queued")
    };
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(mode, Style::default().fg(C_TEXT))),
        Line::from(""),
    ];
    for (i, item) in list.items.iter().enumerate() {
        let style = if i == burn.cursor {
            Style::default().fg(C_ACCENT).bg(Color::Rgb(30, 30, 50))
        } else {
            Style::default().fg(C_TEXT)
        };
        lines.push(Line::from(Span::styled(
            format!("{:>2}. {}", i + 1, ml_truncate(&item.display, 60)),
            style,
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Rip-setup overlay: track checkboxes, destination, quality preset.
fn draw_rip_setup(frame: &mut Frame, rip: &RipSetupState, state: &MediaLibraryState, area: Rect) {
    let w = area.width.saturating_sub(6).min(72).max(44);
    let rows = rip.selected.len() as u16 + 6;
    let h = rows.min(area.height.saturating_sub(2)).max(9);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let title = if rip.editing_dest {
        " Rip — type destination · Enter: done "
    } else {
        " Rip — Space: track · a: all · q: quality · d: dest · Enter: start · Esc "
    };
    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(C_ACCENT)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(C_ACCENT));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let quality_label = match rip.quality {
        0 => "VBR V0 (~245 kbps)",
        2 => "320 kbps CBR",
        _ => "VBR V2 (~190 kbps)",
    };
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            format!(
                "Into: {}{}",
                rip.dest,
                if rip.editing_dest { "|" } else { "" }
            ),
            Style::default().fg(if rip.editing_dest { C_WARN } else { C_TEXT }),
        )),
        Line::from(Span::styled(
            format!("Quality: {quality_label}   (Artist/Album/NN - Title.mp3, added to library)"),
            Style::default().fg(C_DIM),
        )),
        if rip.dest_watched {
            Line::from("")
        } else {
            Line::from(Span::styled(
                "⚠ Not a watched folder — files rip here but won't appear in the library.",
                Style::default().fg(C_WARN),
            ))
        },
    ];
    for (i, sel) in rip.selected.iter().enumerate() {
        let entry_title = state
            .disc_entries
            .get(i)
            .map(|e| e.title.as_str())
            .unwrap_or("?");
        let marker = if *sel { "[x]" } else { "[ ]" };
        let style = if i == rip.cursor && !rip.editing_dest {
            Style::default().fg(C_ACCENT).bg(Color::Rgb(30, 30, 50))
        } else {
            Style::default().fg(C_TEXT)
        };
        lines.push(Line::from(Span::styled(
            format!("{marker} {:>2}. {}", i + 1, ml_truncate(entry_title, 56)),
            style,
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// First-submission email prompt (gnudb requires the submitter's own
/// address; the config ships blank on purpose).
fn draw_submit_email(frame: &mut Frame, buf: &str, area: Rect) {
    let w = 56u16.min(area.width.saturating_sub(4));
    let h = 5u16.min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .title(Span::styled(
            " Your email for gnudb — Enter: save · Esc: cancel ",
            Style::default().fg(C_ACCENT),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(C_ACCENT));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let lines = vec![
        Line::from(Span::styled(
            "Sent only with submissions (never a default).",
            Style::default().fg(C_DIM),
        )),
        Line::from(Span::styled(
            format!("Email: {buf}|"),
            Style::default().fg(C_TEXT),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Centered overlay picking the CDDB submission category (fixed set).
fn draw_submit_category(frame: &mut Frame, selected: usize, area: Rect) {
    let cats = crate::disc::gnudb::CATEGORIES;
    let w = 44u16.min(area.width.saturating_sub(4));
    let h = (cats.len() as u16 + 2).min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .title(Span::styled(
            " Submit — category · Enter: send · Esc: cancel ",
            Style::default().fg(C_ACCENT),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(C_ACCENT));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let items: Vec<ListItem> = cats
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let style = if i == selected {
                Style::default().fg(C_ACCENT).bg(Color::Rgb(30, 30, 50))
            } else {
                Style::default().fg(C_TEXT)
            };
            ListItem::new(Span::styled(format!("  {c}"), style))
        })
        .collect();
    let mut list_state = ListState::default();
    list_state.select(Some(selected));
    frame.render_stateful_widget(List::new(items), inner, &mut list_state);
}

/// Centered overlay listing gnudb matches: ↑/↓ select, Enter fetch, Esc close.
fn draw_gnudb_matches(
    frame: &mut Frame,
    matches: &[crate::disc::gnudb::DiscMatch],
    selected: usize,
    area: Rect,
) {
    let w = area.width.saturating_sub(8).min(70).max(30);
    let h = (matches.len() as u16 + 4).min(area.height.saturating_sub(2)).max(6);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .title(Span::styled(
            " gnudb matches — Enter: use · Esc: cancel ",
            Style::default().fg(C_ACCENT),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(C_ACCENT));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let items: Vec<ListItem> = matches
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let tag = if m.exact { "exact " } else { "close " };
            let style = if i == selected {
                Style::default().fg(C_ACCENT).bg(Color::Rgb(30, 30, 50))
            } else {
                Style::default().fg(C_TEXT)
            };
            ListItem::new(Span::styled(
                format!("{tag}[{}] {}", m.category, m.title),
                style,
            ))
        })
        .collect();
    let mut list_state = ListState::default();
    list_state.select(Some(selected));
    frame.render_stateful_widget(List::new(items), inner, &mut list_state);
}

/// Centered overlay editing the disc's tag set. Rows 0–3 = disc fields,
/// 4+ = per-track titles; `editing` shows a cursor bar on the value.
fn draw_disc_tag_editor(frame: &mut Frame, ed: &DiscTagEditState, area: Rect) {
    let w = area.width.saturating_sub(6).min(76).max(40);
    let rows = 4 + ed.titles.len() as u16;
    let h = (rows + 4).min(area.height.saturating_sub(2)).max(8);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let title = if ed.editing {
        " Disc tags — Enter/Esc: done editing "
    } else {
        " Disc tags — Enter: edit · ↑↓: move · Esc: save + close "
    };
    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(C_ACCENT)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(C_ACCENT));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let field = |label: &str, value: &str, row: usize| -> ListItem<'static> {
        let sel = row == ed.selected;
        let cursor = if sel && ed.editing { "|" } else { "" };
        let style = if sel {
            Style::default().fg(C_ACCENT).bg(Color::Rgb(30, 30, 50))
        } else {
            Style::default().fg(C_TEXT)
        };
        ListItem::new(Span::styled(
            format!("{label:<9} {value}{cursor}"),
            style,
        ))
    };

    let mut items: Vec<ListItem> = vec![
        field("Artist", &ed.artist, 0),
        field("Album", &ed.album, 1),
        field("Year", &ed.year, 2),
        field("Genre", &ed.genre, 3),
    ];
    for (i, t) in ed.titles.iter().enumerate() {
        items.push(field(&format!("Track {:>2}", i + 1), t, i + 4));
    }
    let mut list_state = ListState::default();
    list_state.select(Some(ed.selected));
    frame.render_stateful_widget(List::new(items), inner, &mut list_state);
}

/// Width (chars) for each column ID in the Files tab.
pub(super) fn ml_col_width(id: &str) -> usize {
    match id {
        "num" => 4,
        "title" => 28,
        "artist" => 22,
        "album" => 20,
        "duration" => 6,
        "filename" => 24,
        "year" => 5,
        "genre" => 12,
        "bitrate" => 7,
        _ => 12,
    }
}

/// Human-readable header label for a column ID.
pub(super) fn ml_col_label(id: &str) -> &'static str {
    match id {
        "num" => "#",
        "title" => "Title",
        "artist" => "Artist",
        "album" => "Album",
        "duration" => "Len",
        "filename" => "Filename",
        "year" => "Year",
        "genre" => "Genre",
        "bitrate" => "Bitrate",
        _ => "?",
    }
}

/// Extract the display value for a given column from a `LibTrack`.
pub(super) fn ml_col_value<'a>(id: &str, t: &'a crate::media_library::LibTrack) -> std::borrow::Cow<'a, str> {
    match id {
        "num" => t
            .track_num
            .map(|n| n.to_string())
            .unwrap_or_default()
            .into(),
        "title" => t.title.as_deref().unwrap_or(&t.filename).into(),
        "artist" => t.artist.as_deref().unwrap_or("-").into(),
        "album" => t.album.as_deref().unwrap_or("-").into(),
        "duration" => t
            .length_secs
            .map(|s| {
                let u = s as u64;
                format!("{:>2}:{:02}", u / 60, u % 60)
            })
            .unwrap_or_else(|| "-:--".to_string())
            .into(),
        "filename" => t.filename.as_str().into(),
        "year" => t.year.map(|y| y.to_string()).unwrap_or_default().into(),
        "genre" => t.genre.as_deref().unwrap_or("").into(),
        "bitrate" => t
            .bitrate
            .map(|b| format!("{b}k"))
            .unwrap_or_default()
            .into(),
        _ => "".into(),
    }
}

/// Render the Files tab: column headers and a scrollable track list.
///
/// The columns shown, their order, and the starting scroll offset come from
/// `state.visible_columns` and `state.col_offset`.  The sorted column is
/// marked with ▲ / ▼ in the header.
pub(super) fn draw_ml_files(frame: &mut Frame, state: &MediaLibraryState, area: Rect) {
    if area.height < 2 {
        return;
    }

    let header_area = Rect { height: 1, ..area };
    let list_area = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };

    // The visible columns starting from the scroll offset.
    let cols: Vec<&str> = state
        .visible_columns
        .iter()
        .skip(state.col_offset)
        .map(String::as_str)
        .collect();

    if cols.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "No columns selected. Add columns via Settings → Media Library.",
                Style::default().fg(C_DIM),
            )),
            area,
        );
        return;
    }

    // Build the header line.
    let mut header_spans: Vec<Span> = Vec::new();
    for (ci, &col) in cols.iter().enumerate() {
        let w = ml_col_width(col);
        let label = ml_col_label(col);
        let sort_indicator = if col == state.sort_col.as_str() {
            if state.sort_desc {
                " ▼"
            } else {
                " ▲"
            }
        } else {
            ""
        };
        let text = format!("{:<w$}", format!("{label}{sort_indicator}"), w = w);
        let style = if col == state.sort_col.as_str() {
            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(C_DIM)
        };
        header_spans.push(Span::styled(text, style));
        if ci + 1 < cols.len() {
            header_spans.push(Span::styled("  ", Style::default().fg(C_DIM)));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(header_spans)), header_area);

    if state.tracks.is_empty() {
        let msg = if state.search_query.is_empty() {
            "No tracks in the media library.  Open the GTK4 UI and add a folder with the ML button."
        } else {
            "No tracks match the search query."
        };
        frame.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(C_DIM))),
            list_area,
        );
        return;
    }

    let items: Vec<ListItem> = state
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut row = String::new();
            for (ci, &col) in cols.iter().enumerate() {
                let w = ml_col_width(col);
                let val = ml_col_value(col, t);
                // Right-align numeric/duration columns; left-align text columns.
                match col {
                    "num" | "duration" | "year" | "bitrate" => {
                        row.push_str(&format!("{:>w$}", ml_truncate(&val, w), w = w));
                    }
                    _ => {
                        row.push_str(&format!("{:<w$}", ml_truncate(&val, w), w = w));
                    }
                }
                if ci + 1 < cols.len() {
                    row.push_str("  ");
                }
            }
            let style = if i == state.selected_track {
                Style::default().fg(C_ACCENT).bg(Color::Rgb(30, 30, 50))
            } else {
                Style::default().fg(C_TEXT)
            };
            ListItem::new(Span::styled(row, style))
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected_track));
    let list =
        List::new(items).highlight_style(Style::default().fg(C_ACCENT).bg(Color::Rgb(30, 30, 50)));
    frame.render_stateful_widget(list, list_area, &mut list_state);
}

/// Render the Playlists tab: left pane = playlist list, right pane = track
/// preview for the selected playlist (populated after pressing Enter).
pub(super) fn draw_ml_playlists(frame: &mut Frame, state: &MediaLibraryState, area: Rect) {
    if area.width < 20 {
        return;
    }

    // Split: left ~30 % for playlist names, rest for preview.
    let left_w = (area.width / 3).max(20).min(40);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(left_w), Constraint::Min(1)])
        .split(area);

    // Left: playlist names.
    if state.playlists.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "No playlists found.",
                Style::default().fg(C_DIM),
            )),
            cols[0],
        );
    } else {
        let pl_items: Vec<ListItem> = state
            .playlists
            .iter()
            .enumerate()
            .map(|(i, pl)| {
                let style = if i == state.selected_playlist {
                    Style::default().fg(C_ACCENT).bg(Color::Rgb(30, 30, 50))
                } else {
                    Style::default().fg(C_TEXT)
                };
                ListItem::new(Span::styled(pl.name.clone(), style))
            })
            .collect();

        let mut list_state = ListState::default();
        list_state.select(Some(state.selected_playlist));
        let list = List::new(pl_items)
            .block(
                Block::default()
                    .borders(Borders::RIGHT)
                    .border_style(Style::default().fg(C_DIM)),
            )
            .highlight_style(Style::default().fg(C_ACCENT).bg(Color::Rgb(30, 30, 50)));
        frame.render_stateful_widget(list, cols[0], &mut list_state);
    }

    // Right: track preview.
    let right = Rect {
        x: cols[1].x + 1,
        width: cols[1].width.saturating_sub(1),
        ..cols[1]
    };
    match &state.playlist_preview {
        None => {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "Press Enter to load playlist tracks.",
                    Style::default().fg(C_DIM),
                )),
                right,
            );
        }
        Some(tracks) => {
            let items: Vec<ListItem> = tracks
                .iter()
                .map(|t| {
                    let title = t.title.as_deref().unwrap_or(&t.filename);
                    let artist = t.artist.as_deref().unwrap_or("-");
                    ListItem::new(Span::styled(
                        format!("{} — {}", artist, title),
                        Style::default().fg(C_TEXT),
                    ))
                })
                .collect();
            frame.render_widget(List::new(items), right);
        }
    }
}

/// Render the Discs tab: drive rows on top (one per physical drive, like the
/// external-device list), the selected drive's audio-disc track list below.
pub(super) fn draw_ml_discs(frame: &mut Frame, state: &MediaLibraryState, area: Rect) {
    if area.height < 2 {
        return;
    }

    if state.drives.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "No optical drives found.  Connect a drive and press r to rescan.",
                Style::default().fg(C_DIM),
            )),
            area,
        );
        return;
    }

    // Drive rows: capped so the track list keeps most of the space.
    let drives_h = (state.drives.len() as u16).min(4);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(drives_h),
            Constraint::Length(1), // separator/header line
            Constraint::Min(1),    // track list
        ])
        .split(area);

    let drive_items: Vec<ListItem> = state
        .drives
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let marker = if i == state.selected_drive { "▶ " } else { "  " };
            let style = if i == state.selected_drive {
                Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(C_TEXT)
            };
            ListItem::new(Span::styled(
                format!("{marker}{} — {}", d.label, d.media_summary()),
                style,
            ))
        })
        .collect();
    frame.render_widget(List::new(drive_items), rows[0]);

    // Track list of the selected drive.
    let drive = state.drives.get(state.selected_drive);
    if state.disc_entries.is_empty() {
        let msg = match drive {
            Some(d) if d.media.present && !d.media.is_audio_cd => {
                "Not an audio CD.  Data-disc files appear when the volume mounts; burning arrives in a later phase."
            }
            Some(_) => "No disc loaded.  Insert an audio CD and press r.",
            None => "",
        };
        frame.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(C_DIM))),
            rows[2],
        );
        return;
    }

    frame.render_widget(
        Paragraph::new(Span::styled(
            format!("{:>4}  {:<40} {:>6}", "#", "Title", "Len"),
            Style::default().fg(C_DIM),
        )),
        rows[1],
    );

    let items: Vec<ListItem> = state
        .disc_entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let style = if i == state.selected_disc_track {
                Style::default().fg(C_ACCENT).bg(Color::Rgb(30, 30, 50))
            } else {
                Style::default().fg(C_TEXT)
            };
            ListItem::new(Span::styled(
                format!(
                    "{:>4}  {:<40} {:>3}:{:02}",
                    e.number,
                    ml_truncate(&e.title, 40),
                    e.duration_secs / 60,
                    e.duration_secs % 60
                ),
                style,
            ))
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected_disc_track));
    let list =
        List::new(items).highlight_style(Style::default().fg(C_ACCENT).bg(Color::Rgb(30, 30, 50)));
    frame.render_stateful_widget(list, rows[2], &mut list_state);
}

/// Truncate a string to at most `max_chars` characters, appending `…` when cut.
pub(super) fn ml_truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars()
            .take(max_chars.saturating_sub(1))
            .collect::<String>()
            + "…"
    }
}
