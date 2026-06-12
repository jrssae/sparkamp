//! ID3 editor overlay rendering.

#[rustfmt::skip]
use super::imports::*;

///
/// When `state.show_extra` is false, shows the standard two-column form for
/// the 12 default tag fields.  When true, shows the Customize sub-panel
/// listing all other ID3v2 frames present in the file.
pub(super) fn draw_id3_editor_overlay(frame: &mut Frame, state: &Id3EditorState, area: Rect) {
    // Use most of the screen for the editor — leave a 2-row gutter top/bottom.
    let popup_h = area.height.saturating_sub(4);
    let popup = centered_popup(area, 100, popup_h);
    frame.render_widget(Clear, popup);

    if state.show_extra {
        draw_id3_extra_panel(frame, state, popup);
    } else {
        draw_id3_main_panel(frame, state, popup);
    }
}

/// Render the 12-field two-column editor form.
pub(super) fn draw_id3_main_panel(frame: &mut Frame, state: &Id3EditorState, area: Rect) {
    // Filename shown in the title bar for quick reference.
    let fname = state
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("(unknown)");

    let outer = Block::default()
        .title(Span::styled(
            format!(" ID3 Tag Editor — {} ", fname),
            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(C_ACCENT));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    // Split inner vertically: fields area + bottom hint line.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // two-column field form
            Constraint::Length(1), // status message (if any)
            Constraint::Length(1), // bottom hint bar
        ])
        .split(inner);

    // The fields area is split into two equal columns.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);

    let pairs = state.fields.field_pairs(); // 12 (label, value) pairs
    let mid = pairs.len() / 2; // 6 in each column

    // Render each column.
    draw_id3_field_column(frame, state, &pairs[..mid], 0, cols[0]);
    draw_id3_field_column(frame, state, &pairs[mid..], mid as usize, cols[1]);

    // Genre typeahead popup (only when genre field is focused and has matches).
    if state.focused == 4 {
        let matches = id3_genre_matches(&state.fields.genre);
        if !matches.is_empty() {
            // Show the dropdown below the genre row in the left column.
            // Genre is row index 4 within the left column (0-based), so the
            // dropdown starts at inner.y + 4 (+ 1 for 0-offset).
            let drop_y = cols[0].y + 4 + 1; // approximate position
            let drop_h = (matches.len() as u16 + 2).min(area.height.saturating_sub(drop_y));
            if drop_y < area.y + area.height && drop_h > 2 {
                let drop = Rect {
                    x: cols[0].x,
                    y: drop_y,
                    width: cols[0].width.min(30),
                    height: drop_h,
                };
                frame.render_widget(Clear, drop);
                let items: Vec<Line> = matches
                    .iter()
                    .enumerate()
                    .map(|(i, g)| {
                        if i == state.genre_sel {
                            Line::from(Span::styled(
                                format!(" ▶ {}", g),
                                Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
                            ))
                        } else {
                            Line::from(format!("   {}", g))
                        }
                    })
                    .collect();
                let dropdown = Paragraph::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(C_ACCENT)),
                    )
                    .style(Style::default().fg(C_TEXT));
                frame.render_widget(dropdown, drop);
            }
        }
    }

    // Status / error message.
    if let Some(ref msg) = state.status {
        frame.render_widget(
            Paragraph::new(Span::styled(msg.as_str(), Style::default().fg(C_ERR))),
            rows[1],
        );
    }

    // Bottom hint bar.
    let hints = Line::from(vec![
        hint("Tab", " next field"),
        sep(),
        hint("↑↓", " nav / genre"),
        sep(),
        hint("c", " Customize"),
        sep(),
        hint("^S", " save"),
        sep(),
        hint("Alt+z/x/c/v/b", " transport"),
        sep(),
        Span::styled("[Esc] cancel", Style::default().fg(C_WARN)),
    ]);
    frame.render_widget(Paragraph::new(hints), rows[2]);
}

/// Render one column of the ID3 field form.
///
/// `pairs` is a slice of `(label, value)` from `field_pairs()`.
/// `offset` is the index of `pairs[0]` within the full 12-field list, used
/// to determine which field is highlighted.
pub(super) fn draw_id3_field_column(
    frame: &mut Frame,
    state: &Id3EditorState,
    pairs: &[(&'static str, String)],
    offset: usize,
    area: Rect,
) {
    // The label occupies 15 chars ("         Title: ") leaving the rest for
    // the value.  When the value is longer than that we tail-scroll it so
    // the cursor (end of string) is always visible.
    let value_cols = (area.width as usize).saturating_sub(15);

    // Build one text line per field.
    let lines: Vec<Line> = pairs
        .iter()
        .enumerate()
        .map(|(i, (label, value))| {
            let field_idx = offset + i;
            let focused = field_idx == state.focused;

            // Label: right-aligned in a 13-char column; value follows.
            let label_text = format!("{:>13}: ", label);

            // For focused fields: scroll the view so the cursor is always
            // visible, then render the cursor marker (▌) at its position.
            // For non-focused fields: show the tail so the full value is
            // visible from the end (matching how most text UIs tail-scroll).
            let value_text = if focused {
                let avail_text = value_cols.saturating_sub(1); // 1 col for ▌
                let chars: Vec<char> = value.chars().collect();
                let len = chars.len();
                let cur = state.cursor.min(len);
                // Scroll the window so the cursor sits at the right edge when
                // the text is longer than the visible area.
                let scroll = if cur <= avail_text {
                    0
                } else {
                    cur - avail_text
                };
                let vis_end = (scroll + avail_text).min(len);
                let before: String = chars[scroll..cur].iter().collect();
                let after: String = chars[cur..vis_end].iter().collect();
                format!("{}▌{}", before, after)
            } else {
                tail_chars(value, value_cols).to_owned()
            };

            let label_span = Span::styled(
                label_text,
                Style::default().fg(if focused { C_ACCENT } else { C_DIM }),
            );
            let value_span = Span::styled(
                value_text,
                if focused {
                    Style::default().fg(C_TEXT).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(C_TEXT)
                },
            );
            Line::from(vec![label_span, value_span])
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(C_TEXT)),
        area,
    );
}

/// Render the Customize (extra frames) sub-panel.
pub(super) fn draw_id3_extra_panel(frame: &mut Frame, state: &Id3EditorState, area: Rect) {
    let outer = Block::default()
        .title(Span::styled(
            " ID3 Extra Frames — Customize ",
            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(C_ACCENT));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    if state.extra_frames.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "  (no extra frames found in this file)",
                Style::default().fg(C_DIM),
            )),
            rows[0],
        );
    } else if state.extra_editing {
        // Show the editing buffer for the focused frame with cursor support.
        let frame_ref = state.extra_frames.get(state.extra_focused);
        let id = frame_ref.map(|f| f.id.as_str()).unwrap_or("???");
        let prefix = format!("  Editing frame {} — new value: ", id);
        let prefix_cols = prefix.chars().count();
        let avail_text = (rows[0].width as usize)
            .saturating_sub(prefix_cols)
            .saturating_sub(1);
        let chars: Vec<char> = state.extra_input.chars().collect();
        let len = chars.len();
        let cur = state.extra_cursor.min(len);
        let scroll = if cur <= avail_text {
            0
        } else {
            cur - avail_text
        };
        let vis_end = (scroll + avail_text).min(len);
        let before: String = chars[scroll..cur].iter().collect();
        let after: String = chars[cur..vis_end].iter().collect();
        let input_with_cursor = format!("{}▌{}", before, after);
        let label_line = Line::from(vec![
            Span::styled(prefix, Style::default().fg(C_DIM)),
            Span::styled(
                input_with_cursor,
                Style::default().fg(C_TEXT).add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(Paragraph::new(vec![label_line]), rows[0]);
    } else {
        // List all extra frames; highlight the focused one.
        let items: Vec<Line> = state
            .extra_frames
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let focused = i == state.extra_focused;
                let row = format!("  {:4}  {}", f.id, f.value);
                if focused {
                    Line::from(Span::styled(
                        row,
                        Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
                    ))
                } else {
                    Line::from(row)
                }
            })
            .collect();
        frame.render_widget(
            Paragraph::new(items).style(Style::default().fg(C_TEXT)),
            rows[0],
        );
    }

    // Bottom hint bar.
    let hints = if state.extra_editing {
        Line::from(vec![
            hint("Enter", " save frame"),
            sep(),
            Span::styled("[Esc] discard", Style::default().fg(C_WARN)),
        ])
    } else {
        Line::from(vec![
            hint("↑↓", " navigate"),
            sep(),
            hint("Enter", " edit value"),
            sep(),
            hint("^S", " save all"),
            sep(),
            Span::styled("[Esc] back to fields", Style::default().fg(C_WARN)),
        ])
    };
    frame.render_widget(Paragraph::new(hints), rows[1]);
}

// ---------------------------------------------------------------------------
// Layout helpers
// ---------------------------------------------------------------------------
