//! Small modal overlays: jump-to-track, add-file, move/remove track, help.

#[rustfmt::skip]
use super::imports::*;

/// Render the jump-to-track search overlay.
///
/// Shows a text input for the search query at the top and a list of matching
/// results below.  The selected result is highlighted in yellow.  Navigation
/// is via `↑` / `↓`; `Enter` plays the highlighted track.
pub(super) fn draw_jump_overlay(frame: &mut Frame, app: &App, area: Rect) {
    let Mode::Jump {
        query,
        results,
        selected,
        ..
    } = &app.mode
    else {
        return;
    };

    let h = area.height.saturating_sub(4).min(22).max(8);
    let popup = Rect {
        height: h,
        ..centered_popup(area, 70, h)
    };

    frame.render_widget(Clear, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(popup);

    // Search input with blinking-cursor simulation (trailing underscore).
    let input_text = format!("{}_", query);
    let input = Paragraph::new(input_text).block(
        Block::default()
            .title(Span::styled(
                " Jump to track  (Esc: cancel · Enter: play) ",
                Style::default().fg(C_WARN),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(C_WARN)),
    );
    frame.render_widget(input, chunks[0]);

    // Results list — shows all matching tracks.
    let result_items: Vec<ListItem> = results
        .iter()
        .enumerate()
        .map(|(i, &idx)| {
            let track = &app.playlist.tracks[idx];
            let text = format!("{}. {}", idx + 1, track.display_name());
            let style = if i == *selected {
                Style::default().fg(Color::Black).bg(C_WARN)
            } else {
                Style::default().fg(C_TEXT)
            };
            ListItem::new(text).style(style)
        })
        .collect();

    let mut list_state = ListState::default();
    if !results.is_empty() {
        list_state.select(Some(*selected));
    }

    let results_block = Block::default()
        .title(Span::styled(
            format!(" {} result(s) ", results.len()),
            Style::default().fg(C_DIM),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(C_DIM));

    let results_list = List::new(result_items)
        .block(results_block)
        .highlight_style(Style::default().fg(Color::Black).bg(C_WARN));

    frame.render_stateful_widget(results_list, chunks[1], &mut list_state);
}

// ---------------------------------------------------------------------------
// Add-file overlay
// ---------------------------------------------------------------------------

/// Render the path-entry overlay used for adding files or directories.
///
/// The user types a filesystem path.  If the path points to a directory, all
/// audio files beneath it are added recursively.  Single-file paths are added
/// directly.  This behaviour mirrors the GUI's "Add Folder" button.
pub(super) fn draw_add_file_overlay(frame: &mut Frame, app: &App, area: Rect) {
    let Mode::AddFile { input, .. } = &app.mode else {
        return;
    };

    let popup = centered_popup(area, 72, 5);
    frame.render_widget(Clear, popup);

    let text = format!("{}_", input);
    let widget = Paragraph::new(text).block(
        Block::default()
            .title(Span::styled(
                " Add path(s)  (file, folder, or comma-separated list · Esc: cancel · Enter: add) ",
                Style::default().fg(C_ACCENT),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(C_ACCENT)),
    );
    frame.render_widget(widget, popup);
}

// ---------------------------------------------------------------------------
// Move-track overlay
// ---------------------------------------------------------------------------

/// Render the two-step move-track overlay.
///
/// Step 1 asks for the source position; step 2 asks for the destination.
/// Both are 1-based track numbers matching what is displayed in the playlist.
pub(super) fn draw_move_track_overlay(frame: &mut Frame, app: &App, area: Rect) {
    let Mode::MoveTrack { input, from } = &app.mode else {
        return;
    };

    let popup = centered_popup(area, 50, 5);
    frame.render_widget(Clear, popup);

    let (title, prompt) = match from {
        None => (
            " Move track — step 1 of 2  (Esc: cancel) ",
            "From position: ".to_string(),
        ),
        Some(n) => (
            " Move track — step 2 of 2  (Esc: cancel) ",
            format!("Move {} → position: ", n),
        ),
    };

    let text = format!("{}{}_", prompt, input);
    let widget = Paragraph::new(text).block(
        Block::default()
            .title(Span::styled(title, Style::default().fg(C_WARN)))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(C_WARN)),
    );
    frame.render_widget(widget, popup);
}

// ---------------------------------------------------------------------------
// Remove-track overlay
// ---------------------------------------------------------------------------

/// Render the remove-track overlay.
///
/// Asks for a 1-based track number to remove from the playlist.  The actual
/// file on disk is **not** deleted; only the playlist entry is removed.
pub(super) fn draw_remove_track_overlay(frame: &mut Frame, app: &App, area: Rect) {
    let Mode::RemoveTrack { input } = &app.mode else {
        return;
    };

    let popup = centered_popup(area, 44, 5);
    frame.render_widget(Clear, popup);

    let text = format!("Remove track #: {}_", input);
    let widget = Paragraph::new(text).block(
        Block::default()
            .title(Span::styled(
                " Remove track  (Esc: cancel · Enter: remove) ",
                Style::default().fg(C_ERR),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(C_ERR)),
    );
    frame.render_widget(widget, popup);
}

// ---------------------------------------------------------------------------
// Help overlay
// ---------------------------------------------------------------------------

/// Render the full keyboard-shortcut reference overlay.
///
/// Receives `app` so it can display the current repeat/shuffle state inline,
/// making it easy for users to confirm what mode they are in.  Any key
/// dismisses the overlay.
pub(super) fn draw_help_overlay(frame: &mut Frame, app: &App, area: Rect) {
    let scroll = if let Mode::Help { scroll } = app.mode {
        scroll
    } else {
        0
    };

    // Build repeat/shuffle status strings for the live-state display.
    let repeat_status = app.config.playback.repeat_mode.label();
    let shuffle_status = if app.shuffle_state.enabled {
        "Shuffle: On"
    } else {
        "Shuffle: Off"
    };

    let key = |s: &'static str| {
        Span::styled(
            s,
            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        )
    };
    let dim = |s: &'static str| Span::styled(s, Style::default().fg(C_DIM));
    let sep = |s: &'static str| Line::from(dim(s));

    let lines: Vec<Line> = vec![
        sep("── Playback ─────────────────────────────────────────"),
        Line::from(vec![key("  z"), Span::raw("      Previous / restart")]),
        Line::from(vec![key("  x"), Span::raw("      Play")]),
        Line::from(vec![key("  c"), Span::raw("      Pause / resume")]),
        Line::from(vec![key("  v"), Span::raw("      Stop")]),
        Line::from(vec![key("  b"), Span::raw("      Next track")]),
        Line::from(vec![key("  ← →"), Span::raw("    Seek −5 s / +5 s")]),
        Line::from(vec![
            key("  r"),
            Span::raw("      Cycle repeat  "),
            Span::styled(
                format!("(now: {repeat_status})"),
                Style::default().fg(C_DIM),
            ),
        ]),
        Line::from(""),
        sep("── Volume ────────────────────────────────────────────"),
        Line::from(vec![key("  -"), Span::raw("      Volume down 5 %")]),
        Line::from(vec![key("  ="), Span::raw("      Volume up 5 %")]),
        Line::from(""),
        sep("── Playlist ──────────────────────────────────────────"),
        Line::from(vec![
            key("  n"),
            Span::raw("      Add file(s) / folder(s)  (comma-separated ok)"),
        ]),
        Line::from(vec![
            key("  ,"),
            Span::raw("      Move track (enter from → to positions)"),
        ]),
        Line::from(vec![key("  ."), Span::raw("      Remove track by number")]),
        Line::from(vec![
            Span::styled(
                "  /",
                Style::default().fg(C_ERR).add_modifier(Modifier::BOLD),
            ),
            Span::raw("      Clear all tracks"),
        ]),
        Line::from(vec![key("  j"), Span::raw("      Jump / search")]),
        Line::from(vec![key("  ↑  k"), Span::raw("    Browse up")]),
        Line::from(vec![key("  ↓  l"), Span::raw("    Browse down")]),
        Line::from(vec![key("  Enter"), Span::raw("   Play selected")]),
        Line::from(vec![
            Span::styled(
                "  Del",
                Style::default().fg(C_ERR).add_modifier(Modifier::BOLD),
            ),
            Span::raw("    Remove highlighted track"),
        ]),
        Line::from(vec![key("  p"), Span::raw("      Toggle playlist panel")]),
        Line::from(""),
        sep("── Equalizer (u to open) ─────────────────────────────"),
        Line::from(vec![
            key("  ← →"),
            Span::raw("    Select band (→ past band 9 = pre-amp)"),
        ]),
        Line::from(vec![key("  ↑ ↓"), Span::raw("    ±1 dB / ±5 % pre-amp")]),
        Line::from(vec![
            key("  PgUp PgDn"),
            Span::raw(" ±3 dB / ±15 % pre-amp"),
        ]),
        Line::from(vec![key("  p"), Span::raw("      Cycle preset")]),
        Line::from(vec![key("  r"), Span::raw("      Reset to flat")]),
        Line::from(vec![key("  t"), Span::raw("      Toggle EQ on/off")]),
        Line::from(vec![key("  u / Esc"), Span::raw("  Close equalizer")]),
        Line::from(""),
        sep("── Media Library ─────────────────────────────────────"),
        Line::from(vec![key("  m"), Span::raw("      Open media library")]),
        Line::from(vec![
            key("  ↑ ↓"),
            Span::raw("    Navigate tracks / playlists"),
        ]),
        Line::from(vec![
            key("  Tab"),
            Span::raw("    Switch Files / Playlists / Discs tab"),
        ]),
        Line::from(vec![
            key("  Enter"),
            Span::raw("   Add selected to playlist & play"),
        ]),
        Line::from(vec![key("  /  Ctrl+f"), Span::raw(" Activate search")]),
        Line::from(vec![
            key("  m e r"),
            Span::raw("  Discs tab: identify on gnudb / edit tags / rescan"),
        ]),
        Line::from(vec![key("  Esc"), Span::raw("    Close media library")]),
        Line::from(""),
        sep("── View / Other ──────────────────────────────────────"),
        Line::from(vec![key("  a"), Span::raw("      Cycle visualizer mode")]),
        Line::from(vec![
            key("  d"),
            Span::raw("      View/Edit ID3 tags for highlighted track"),
        ]),
        Line::from(vec![key("  i"), Span::raw("      Show this help")]),
        Line::from(vec![key("  q / Esc"), Span::raw("  Quit")]),
        Line::from(""),
        sep("── Hidden shortcuts ──────────────────────────────────"),
        Line::from(vec![
            key("  s"),
            Span::raw("      Toggle shuffle  "),
            Span::styled(
                format!("(now: {shuffle_status})"),
                Style::default().fg(C_DIM),
            ),
        ]),
        Line::from(vec![key("  e"), Span::raw("      Open settings")]),
        Line::from(vec![key("  u"), Span::raw("      Open equalizer")]),
        Line::from(""),
        Line::from(Span::styled(
            "  ↑/↓ scroll  ·  z/x/c/v/b/j work here  ·  Esc closes",
            Style::default().fg(C_DIM),
        )),
    ];

    // Popup fills most of the terminal height so all content is reachable.
    let popup_h = area.height.saturating_sub(4).max(6);
    let popup = centered_popup(area, 62, popup_h);

    // Visible content rows = popup height minus the two border rows.
    let visible = popup_h.saturating_sub(2) as usize;
    let total = lines.len();
    let max_scroll = total.saturating_sub(visible) as u16;
    let clamped_scroll = scroll.min(max_scroll);

    // Scroll hint appended to the title when the content overflows.
    let title = if total > visible {
        format!(
            " Keyboard Shortcuts  [{}/{}] ",
            clamped_scroll + 1,
            max_scroll + 1
        )
    } else {
        " Keyboard Shortcuts ".to_string()
    };

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(Span::styled(
                        title,
                        Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(C_ACCENT)),
            )
            .style(Style::default().fg(C_TEXT))
            .scroll((clamped_scroll, 0)),
        popup,
    );
}
