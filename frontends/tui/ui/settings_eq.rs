//! Settings and equalizer overlay rendering.

#[rustfmt::skip]
use super::imports::*;

/// Names for the settings tabs, shown in the tab bar.
const SETTINGS_TABS: [&str; 4] = [
    "Behavior",
    "Visualizer",
    "Media Lib",
    "ReplayGain",
];

/// Render the full settings overlay with four tabs.
///
/// Layout (inside the popup border):
///   Row 0   — tab bar (highlighted tab shown in accent colour)
///   Row 1   — horizontal separator
///   Rows 2+ — setting rows for the active tab (label · value/toggle)
///   Last row — hint line: arrows=navigate, space=toggle, Esc=save & close
pub(super) fn draw_settings_overlay(frame: &mut Frame, app: &App, state: &SettingsState, area: Rect) {
    // The popup is 62 columns wide and tall enough for the longest tab.
    let popup = centered_popup(area, 62, 14);
    frame.render_widget(Clear, popup);

    // ── outer border ─────────────────────────────────────────────────────
    let block = Block::default()
        .title(Span::styled(
            " Settings ",
            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(C_ACCENT));
    frame.render_widget(block, popup);

    // Inner area (inside the border, 2 cells padding on left).
    let inner = Rect {
        x: popup.x + 2,
        y: popup.y + 1,
        width: popup.width.saturating_sub(4),
        height: popup.height.saturating_sub(2),
    };
    if inner.height == 0 {
        return;
    }

    // ── tab bar ───────────────────────────────────────────────────────────
    let tab_spans: Vec<Span> = SETTINGS_TABS
        .iter()
        .enumerate()
        .map(|(i, name)| {
            if i == state.tab {
                Span::styled(
                    format!(" {name} "),
                    Style::default()
                        .fg(C_ACCENT)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                )
            } else {
                Span::styled(format!(" {name} "), Style::default().fg(C_DIM))
            }
        })
        .collect();

    let tab_line = Line::from(tab_spans);
    frame.render_widget(
        Paragraph::new(vec![tab_line]).style(Style::default()),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );

    // Separator below the tab bar.
    let sep = "─".repeat(inner.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(sep, Style::default().fg(C_DIM))),
        Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: 1,
        },
    );

    // ── settings rows ─────────────────────────────────────────────────────
    let rows_area = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(3),
    };

    let rows = settings_rows_for_tab(app, state);
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, (label, value))| {
            let focused = i == state.cursor;
            let label_span = Span::styled(
                format!("{label:<24}"),
                Style::default().fg(if focused { C_ACCENT } else { C_TEXT }),
            );
            let value_span = Span::styled(
                value.clone(),
                Style::default()
                    .fg(if focused { C_ACCENT } else { C_TEXT })
                    .add_modifier(if focused {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            );
            // Prefix focused row with a pointer character.
            let prefix = if focused {
                Span::styled("▶ ", Style::default().fg(C_ACCENT))
            } else {
                Span::raw("  ")
            };
            ListItem::new(Line::from(vec![prefix, label_span, value_span]))
        })
        .collect();

    frame.render_widget(List::new(items), rows_area);

    // ── hint line ─────────────────────────────────────────────────────────
    let hint_y = popup.y + popup.height.saturating_sub(2);
    let hint = if state.edit_buf.is_some() {
        "  Type · Backspace=delete · Enter=confirm · Esc=cancel"
    } else {
        "  ←/→=tab  ↑/↓=item  Space/Enter=toggle  Esc=save & close"
    };
    frame.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(C_DIM))),
        Rect {
            x: popup.x + 1,
            y: hint_y,
            width: popup.width.saturating_sub(2),
            height: 1,
        },
    );
}

/// Build the (label, display-value) pairs for the currently active settings tab.
///
/// For the Filetypes tab, if `state.edit_buf` is Some, the active item shows
/// the in-progress edit buffer (with a cursor block appended).
pub(super) fn settings_rows_for_tab<'a>(
    app: &'a App,
    state: &'a SettingsState,
) -> Vec<(&'static str, String)> {
    match state.tab {
        // ── Behavior ─────────────────────────────────────────────────────
        0 => vec![
            (
                "Autoplay on add",
                if app.config.behavior.autoplay_on_add {
                    "[ On  / Off ]  ●  On".to_string()
                } else {
                    "[ On  / Off ]  ●  Off".to_string()
                },
            ),
            (
                "Media library → playlist",
                match app.config.behavior.playlist_add_behavior {
                    PlaylistAddBehavior::Append => "[ Append / Replace ]  ●  Append".to_string(),
                    PlaylistAddBehavior::Replace => "[ Append / Replace ]  ●  Replace".to_string(),
                },
            ),
        ],

        // ── Visualizer ────────────────────────────────────────────────────
        1 => vec![(
            "Visualizer mode",
            match app.config.visualizer.mode {
                VisualizerMode::Bars     => "[ Bars / Waveform ]  ●  Bars".to_string(),
                VisualizerMode::Waveform => "[ Bars / Waveform ]  ●  Waveform".to_string(),
                VisualizerMode::Granite  => "[ Bars / Waveform ]  ●  Granite (GUI only)".to_string(),
            },
        )],

        // ── Media Library ─────────────────────────────────────────────────
        2 => {
            let startup_val = if app.config.media_library.rescan_on_startup {
                "[ On  / Off ]  ●  On".to_string()
            } else {
                "[ On  / Off ]  ●  Off".to_string()
            };
            let periodic_val = if app.config.media_library.periodic_rescan {
                "[ On  / Off ]  ●  On".to_string()
            } else {
                "[ On  / Off ]  ●  Off".to_string()
            };
            // Show interval only when periodic rescan is on (always shown for editing).
            let interval_val = if state.cursor == 2 {
                if let Some(buf) = &state.edit_buf {
                    format!("{buf}▌ min")
                } else {
                    format!("{} min", app.config.media_library.rescan_interval_mins)
                }
            } else {
                format!("{} min", app.config.media_library.rescan_interval_mins)
            };
            vec![
                ("Rescan on startup", startup_val),
                ("Periodic rescan", periodic_val),
                ("Rescan interval", interval_val),
            ]
        }

        // ── ReplayGain ────────────────────────────────────────────────────
        3 => {
            let rg = &app.config.playback.replaygain;
            let on_off = |b: bool| {
                if b {
                    "[ On  / Off ]  ●  On".to_string()
                } else {
                    "[ On  / Off ]  ●  Off".to_string()
                }
            };
            let source_val = match rg.source {
                RgSource::Track => "[ Track / Album / Auto ]  ●  Track".to_string(),
                RgSource::Album => "[ Track / Album / Auto ]  ●  Album".to_string(),
                RgSource::Automatic => "[ Track / Album / Auto ]  ●  Automatic".to_string(),
            };
            // Fallback shows the live edit buffer while its row is being typed.
            let fallback_val = if state.cursor == 3 {
                if let Some(buf) = &state.edit_buf {
                    format!("{buf}▌ dB")
                } else {
                    format!("{:.1} dB", rg.fallback_db)
                }
            } else {
                format!("{:.1} dB", rg.fallback_db)
            };
            vec![
                ("Use ReplayGain", on_off(rg.enabled)),
                ("Gain source", source_val),
                ("Clip protection", on_off(rg.clip_protection)),
                ("Fallback gain", fallback_val),
                ("Analyze on scan", on_off(rg.auto_analyze)),
                ("Write tags (MP3)", on_off(rg.write_tags)),
            ]
        }

        _ => vec![],
    }
}

// ---------------------------------------------------------------------------
// ID3 tag editor overlay
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Equalizer overlay
// ---------------------------------------------------------------------------

/// Render the 10-band equalizer overlay.
///
/// Layout: a popup centred on the screen containing —
/// - Title bar: "10-Band Equalizer  [enabled/disabled]  preset name"
/// - Band columns: 10 narrow columns, each showing a vertical gain bar
///   (heights represent the ±12 dB range) plus the
///   and numeric gain value below.
/// - Hint bar at the bottom.
pub(super) fn draw_eq_overlay(frame: &mut Frame, app: &App, state: &EqState, area: Rect) {
    // 10 bands + 1 pre-amp column, each 5 wide = 55 + 3 left offset + 2 border = 60.
    // Allow up to 80 wide on large terminals.
    let popup_w = (area.width).min(80).max(62);
    let popup_h = 12u16;
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(popup_w)) / 2,
        y: area.y + (area.height.saturating_sub(popup_h)) / 2,
        width: popup_w,
        height: popup_h,
    };
    frame.render_widget(Clear, popup);

    let eq = &app.config.equalizer;
    let enabled_str = if eq.enabled { "ON " } else { "OFF" };
    let preset_str = if eq.preset.is_empty() {
        "Custom"
    } else {
        &eq.preset
    };
    let title = format!(" EQ [{enabled_str}]  {preset_str} ");

    let border_style = if eq.enabled {
        Style::default().fg(C_ACCENT)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(title.as_str())
        .borders(Borders::ALL)
        .border_style(border_style);
    frame.render_widget(block, popup);

    // Inner area (inside the border).
    let inner = Rect {
        x: popup.x + 1,
        y: popup.y + 1,
        width: popup.width.saturating_sub(2),
        height: popup.height.saturating_sub(2),
    };

    // Ensure we have 10 band values (pad with 0 if config is short).
    let mut gains = [0.0f64; 10];
    for (i, &v) in eq.bands.iter().take(10).enumerate() {
        gains[i] = v;
    }

    // Each band column is 5 chars wide; 6-char left offset aligns the single-
    // char bar with the centre of the 4-char value label rendered below it.
    const COL_W: u16 = 5;
    const LEFT_OFF: u16 = 6;
    let bar_h = inner.height.saturating_sub(2); // rows for the gain bar + value row
    let zero_row = (bar_h / 2).max(0); // row index for 0 dB / 100%

    // ── EQ band columns (0-9) ──────────────────────────────────────────────
    for i in 0..10 {
        let col_x = inner.x + LEFT_OFF + (i as u16) * COL_W;
        let selected = i == state.selected_band;
        let gain = gains[i];

        let col_style = if !eq.enabled {
            Style::default().fg(Color::DarkGray)
        } else if selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(C_ACCENT)
        };

        // Map gain from [-12, +12] to bar_h rows. Zero line is at midpoint.
        let filled = ((gain + 12.0) / 24.0 * bar_h as f64).round() as u16;
        let filled = filled.clamp(0, bar_h);

        for row in 0..bar_h {
            let display_row = bar_h - 1 - row;
            let ch = if display_row < filled {
                if display_row >= zero_row {
                    '█'
                } else {
                    '▒'
                }
            } else if display_row == zero_row {
                '─'
            } else {
                ' '
            };
            frame.render_widget(
                Paragraph::new(vec![Line::from(Span::styled(ch.to_string(), col_style))]),
                Rect {
                    x: col_x.saturating_sub(1),
                    y: inner.y + row,
                    width: 1,
                    height: 1,
                },
            );
        }

        // Gain value label directly below the bar, centred in the column.
        let gain_text = if gain == 0.0 {
            format!("{:>4}", "0")
        } else {
            format!("{:>+4.0}", gain)
        };
        frame.render_widget(
            Paragraph::new(vec![Line::from(Span::styled(
                gain_text,
                if selected {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                },
            ))]),
            Rect {
                x: col_x.saturating_sub(4),
                y: inner.y + bar_h,
                width: COL_W,
                height: 1,
            },
        );
    }

    // ── Pre-amp column (index 10) — right-aligned ─────────────────────────
    // The bar sits at the last inner column; the label and "Vol/Adj" text
    // sit to its left so the column floats clearly away from the EQ bands.
    let preamp_selected = state.selected_band == 10;
    let preamp_pct = (eq.preamp * 100.0).round() as i32;

    let preamp_style = if !eq.enabled {
        Style::default().fg(Color::DarkGray)
    } else if preamp_selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Cyan)
    };

    // Bar: 50% = bottom, 150% = top, 100% = midpoint.
    let preamp_filled = ((eq.preamp - 0.5) / 1.0 * bar_h as f64).round() as u16;
    let preamp_filled = preamp_filled.clamp(0, bar_h);

    // Bar sits 3 columns left of the right inner edge; value label stays at
    // the original flush-right position so only the slider moves.
    let preamp_bar_x = (inner.x + inner.width).saturating_sub(4);
    let preamp_value_anchor = (inner.x + inner.width).saturating_sub(1);

    for row in 0..bar_h {
        let display_row = bar_h - 1 - row;
        let ch = if display_row < preamp_filled {
            if display_row >= zero_row {
                '█'
            } else {
                '▒'
            }
        } else if display_row == zero_row {
            '─'
        } else {
            ' '
        };
        if preamp_bar_x < inner.x + inner.width {
            frame.render_widget(
                Paragraph::new(vec![Line::from(Span::styled(ch.to_string(), preamp_style))]),
                Rect {
                    x: preamp_bar_x,
                    y: inner.y + row,
                    width: 1,
                    height: 1,
                },
            );
        }
    }

    // "Vol" / "Adj" label centred on the zero line, 4 chars left of the bar.
    let label_x = preamp_value_anchor.saturating_sub(7);
    let label_style = Style::default().fg(if preamp_selected {
        Color::Yellow
    } else {
        Color::DarkGray
    });
    if zero_row > 0 && inner.y + zero_row.saturating_sub(1) >= inner.y {
        frame.render_widget(
            Paragraph::new(vec![Line::from(Span::styled("Vol", label_style))]),
            Rect {
                x: label_x,
                y: inner.y + zero_row.saturating_sub(1),
                width: 3,
                height: 1,
            },
        );
    }
    frame.render_widget(
        Paragraph::new(vec![Line::from(Span::styled("Adj", label_style))]),
        Rect {
            x: label_x,
            y: inner.y + zero_row,
            width: 3,
            height: 1,
        },
    );

    // Value label ("100%") right-aligned below the bar.
    let preamp_label = format!("{preamp_pct:>4}%");
    frame.render_widget(
        Paragraph::new(vec![Line::from(Span::styled(preamp_label, preamp_style))]),
        Rect {
            x: preamp_value_anchor.saturating_sub(6),
            y: inner.y + bar_h,
            width: 5,
            height: 1,
        },
    );

    // Hint line — `t` label reflects the action it will take (toggle to opposite).
    let t_action = if eq.enabled { "eq off" } else { "eq on " };
    let hint = format!(
        " ←/→ select  ↑/↓ ±1dB/5%  PgUp/Dn ±3dB/15%  p preset  r flat  t {t_action}  u close "
    );
    frame.render_widget(
        Paragraph::new(vec![Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        ))]),
        Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(1),
            width: inner.width,
            height: 1,
        },
    );
}
