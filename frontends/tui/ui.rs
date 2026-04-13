//! TUI rendering — converts [`App`] state into ratatui widget trees.
//!
//! ## Layout (top → bottom)
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │ [mini viz box (22 cols)] │ Sparkamp — ▶ Title — Artist [n/N] │  ← header (4 rows)
//! ├──────────────────────────────────────────────────────────────┤
//! │ ████████░░░░░░  0:45 / 3:22                                  │  ← seek (2 rows)
//! ├──────────────────────────────────────────────────────────────┤
//! │ [z]prev · [x]play · [c]pause · [v]stop · [b]next            │  ← transport hints (1 row)
//! ├──────────────────────────────────────────────────────────────┤
//! │ Playlist (if visible)                                        │  ← playlist (min 3)
//! ├──────────────────────────────────────────────────────────────┤
//! │ [j]jump · [n]add · [m]move · [,]remove · [p]playlist …     │  ← playlist hints (1 row)
//! └──────────────────────────────────────────────────────────────┘
//! ```
//!
//! The visualizer lives inside the left column of the header row so that it
//! always occupies the same screen real estate as the now-playing information,
//! mirroring the classic Winamp 2.x layout where the spectrum / waveform
//! sits to the left of the scrolling song title.
//!
//! When the user hides the playlist (`p` key) the playlist section collapses
//! to zero height, giving the remaining content more breathing room in narrow
//! terminals.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph},
    Frame,
};
use std::time::Duration;

use super::{
    id3_genre_matches, App, EqState, Id3EditorState, MediaLibraryState, MediaLibraryTab, Mode,
    SettingsState,
};
use crate::{
    config::{AccentColorChoice, PlaylistAddBehavior, ThemeChoice, VisualizerMode},
    engine::PlayerState,
    model::fmt_duration,
    shuffle::RepeatMode,
};

// ---------------------------------------------------------------------------
// Colour palette — centralised so re-skinning only needs edits here
// ---------------------------------------------------------------------------

/// Accent colour: used for borders, labels and the waveform waveform.
const C_ACCENT: Color = Color::Cyan;
/// Playing state indicator colour.
const C_PLAYING: Color = Color::Green;
/// Dim / inactive colour: separators, borders, idle visualizer.
const C_DIM: Color = Color::DarkGray;
/// Primary text colour for track names and UI labels.
const C_TEXT: Color = Color::White;
/// Warning colour: paused state indicator, move-track overlay.
const C_WARN: Color = Color::Yellow;
/// Error colour: status messages and the remove-track overlay.
const C_ERR: Color = Color::Red;

/// Return a display string for the accent color setting.
fn accent_color_display(choice: &AccentColorChoice) -> String {
    match choice {
        AccentColorChoice::System => "[ System / Blue / Green / Purple / Red / Orange / Yellow / White / Grey / Custom ]  ●  System".to_string(),
        AccentColorChoice::Blue => "[ System / Blue / Green / Purple / Red / Orange / Yellow / White / Grey / Custom ]  ●  Blue".to_string(),
        AccentColorChoice::Green => "[ System / Blue / Green / Purple / Red / Orange / Yellow / White / Grey / Custom ]  ●  Green".to_string(),
        AccentColorChoice::Purple => "[ System / Blue / Green / Purple / Red / Orange / Yellow / White / Grey / Custom ]  ●  Purple".to_string(),
        AccentColorChoice::Red => "[ System / Blue / Green / Purple / Red / Orange / Yellow / White / Grey / Custom ]  ●  Red".to_string(),
        AccentColorChoice::Orange => "[ System / Blue / Green / Purple / Red / Orange / Yellow / White / Grey / Custom ]  ●  Orange".to_string(),
        AccentColorChoice::Yellow => "[ System / Blue / Green / Purple / Red / Orange / Yellow / White / Grey / Custom ]  ●  Yellow".to_string(),
        AccentColorChoice::White => "[ System / Blue / Green / Purple / Red / Orange / Yellow / White / Grey / Custom ]  ●  White".to_string(),
        AccentColorChoice::Grey => "[ System / Blue / Green / Purple / Red / Orange / Yellow / White / Grey / Custom ]  ●  Grey".to_string(),
        AccentColorChoice::Custom(hex) => format!("[ System / Blue / Green / Purple / Red / Orange / Yellow / White / Grey / Custom ]  ●  Custom ({hex})"),
    }
}

// ---------------------------------------------------------------------------
// Top-level draw — assembles all sections into the terminal frame
// ---------------------------------------------------------------------------

/// Render the entire TUI for the current frame.
///
/// The vertical layout is computed fresh on every draw so that it responds
/// correctly to terminal resize events.  Each section is delegated to a
/// dedicated helper function for readability.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Collapse both the playlist AND its hint bar when hidden so that no
    // orphaned control hints linger on screen.
    let (pl_height, hints_height) = if app.playlist_visible {
        (Constraint::Min(3), Constraint::Length(1))
    } else {
        (Constraint::Length(0), Constraint::Length(0))
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // header: mini viz (left) + track info (right)
            Constraint::Length(2), // seek / progress gauge
            Constraint::Length(1), // transport shortcut hints
            pl_height,             // playlist rows (collapsible)
            hints_height,          // playlist shortcut hints (hidden with playlist)
        ])
        .split(area);

    draw_header(frame, app, chunks[0]);
    draw_progress(frame, app, chunks[1]);
    draw_transport_hints(frame, app, chunks[2]);
    if app.playlist_visible {
        draw_playlist(frame, app, chunks[3]);
        draw_playlist_hints(frame, app, chunks[4]);
    }

    // Modal overlays — rendered on top of everything else.
    // MediaLibrary is a full-screen view, not an overlay.
    match &app.mode {
        Mode::Jump { .. } => draw_jump_overlay(frame, app, area),
        Mode::AddFile { .. } => draw_add_file_overlay(frame, app, area),
        Mode::MoveTrack { .. } => draw_move_track_overlay(frame, app, area),
        Mode::RemoveTrack { .. } => draw_remove_track_overlay(frame, app, area),
        Mode::Help { .. } => draw_help_overlay(frame, app, area),
        Mode::Id3Editor(state) => draw_id3_editor_overlay(frame, state, area),
        Mode::Settings(state) => draw_settings_overlay(frame, app, state, area),
        Mode::Equalizer(state) => draw_eq_overlay(frame, app, state, area),
        Mode::MediaLibrary(state) => {
            draw_media_library(frame, state, app.status_message.as_deref(), area)
        }
        Mode::Normal => {}
    }
}

// ---------------------------------------------------------------------------
// Header — mini visualizer on the left, track info on the right
// ---------------------------------------------------------------------------

/// Draw the combined header row.
///
/// The row is split horizontally:
/// - **Left 22 columns**: a small bordered box containing the visualizer
///   (bars or waveform), labelled with the current mode name.
/// - **Right (remainder)**: now-playing information — state icon, title,
///   artist and track index — inside a bordered box titled "Sparkamp".
fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    // Split the header horizontally: small fixed-width viz column on the left,
    // all remaining space for the track-info column on the right.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(22), // mini visualizer box
            Constraint::Min(0),     // track info (title, artist, state)
        ])
        .split(area);

    draw_header_viz(frame, app, cols[0]);
    draw_header_track_info(frame, app, cols[1]);
}

/// Render the mini visualizer inside the left column of the header.
///
/// Uses the same [`render_bars`] / [`render_waveform`] functions as the
/// full-size standalone visualizer so the rendering logic stays in one place.
fn draw_header_viz(frame: &mut Frame, app: &App, area: Rect) {
    let mode_label = match app.config.visualizer.mode {
        VisualizerMode::Bars => "▲",
        VisualizerMode::Waveform => "~",
    };

    let block = Block::default()
        .title(Span::styled(
            format!(" {} ", mode_label),
            Style::default().fg(C_DIM),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(C_DIM));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if !app.visualizer_active {
        // Idle state: draw a single flat dim line at the vertical midpoint.
        let flat: String = std::iter::repeat('─').take(inner.width as usize).collect();
        let mid_y = inner.y + inner.height.saturating_sub(1) / 2;
        frame.render_widget(
            Paragraph::new(Span::styled(flat, Style::default().fg(C_DIM))),
            Rect {
                y: mid_y,
                height: 1,
                ..inner
            },
        );
        return;
    }

    // Request enough data points to fill the mini box width.
    let col_count = (inner.width as usize).max(10);
    let data = app.visualizer_data(col_count);
    let n_rows = inner.height as usize;

    let lines: Vec<Line> = match app.config.visualizer.mode {
        VisualizerMode::Bars => {
            let mirror = app.config.visualizer.bars_mirror;
            let zones = app.config.visualizer.color_zones as usize;
            let zone_colors = &app.config.visualizer.zone_colors;
            render_bars(&data, n_rows, mirror, zones, zone_colors)
        }
        VisualizerMode::Waveform => {
            let zones = app.config.visualizer.waveform_color_zones as usize;
            let zone_colors = &app.config.visualizer.waveform_zone_colors;
            render_waveform(&data, n_rows, zones, zone_colors)
        }
    };

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Render the now-playing track info inside the right column of the header.
///
/// The first content line shows the Winamp-style marquee: "Title — Artist"
/// scrolled to `app.marquee_offset` so it slides left when the text is wider
/// than the available area.  The second line shows the playback state icon and
/// track index.  The outer block carries the app title and playlist hints.
fn draw_header_track_info(frame: &mut Frame, app: &App, area: Rect) {
    let (state_icon, state_color) = match app.player.state() {
        PlayerState::Playing => ("▶", C_PLAYING),
        PlayerState::Paused => ("⏸", C_WARN),
        PlayerState::Stopped => ("⏹", C_DIM),
    };

    // Reserve 2 chars for the block borders; the inner width drives the
    // marquee window so the text never runs into the border glyphs.
    let inner_w = area.width.saturating_sub(2) as usize;
    let marquee = if app.playlist.current().is_none() {
        "No tracks loaded".to_owned()
    } else {
        app.marquee_visible(inner_w)
    };

    let index_label = if !app.playlist.is_empty() {
        format!(
            " [{}/{}]",
            app.playlist.current_index + 1,
            app.playlist.len()
        )
    } else {
        String::new()
    };

    // Repeat indicator: only shown when not Off so it stays unobtrusive.
    let repeat_span = match app.config.playback.repeat_mode {
        RepeatMode::Off => Span::raw(""),
        RepeatMode::Song => Span::styled(" 🔂", Style::default().fg(C_ACCENT)),
        RepeatMode::Playlist => Span::styled(" 🔁", Style::default().fg(C_ACCENT)),
    };

    // Shuffle indicator: shown with accent colour when enabled.
    let shuffle_span = if app.shuffle_state.enabled {
        Span::styled(" 🔀", Style::default().fg(C_ACCENT))
    } else {
        Span::raw("")
    };

    // Two-line content: scrolling title on top, state + index + indicators below.
    let marquee_line = Line::from(Span::styled(
        marquee,
        Style::default().fg(C_TEXT).add_modifier(Modifier::BOLD),
    ));
    let state_line = Line::from(vec![
        Span::styled(format!("{} ", state_icon), Style::default().fg(state_color)),
        Span::styled(index_label, Style::default().fg(C_DIM)),
        repeat_span,
        shuffle_span,
    ]);

    // [q] quit lives in the upper-right corner of the player box; [p] toggle
    // in the lower-right so it is always discoverable from the player view.
    let pl_hint = if app.playlist_visible {
        " [p] hide "
    } else {
        " [p] show "
    };
    let block = Block::default()
        .title_top(
            Line::from(Span::styled(
                " ⚡🎧 SPARKAMP ",
                Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
            ))
            .centered(),
        )
        .title_top(
            Line::from(Span::styled(" [q] quit ", Style::default().fg(C_DIM))).right_aligned(),
        )
        .title_bottom(Line::from(Span::styled(pl_hint, Style::default().fg(C_DIM))).right_aligned())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(C_ACCENT));

    frame.render_widget(
        Paragraph::new(vec![marquee_line, state_line])
            .block(block)
            .alignment(Alignment::Center),
        area,
    );
}

// ---------------------------------------------------------------------------
// Progress bar
// ---------------------------------------------------------------------------

/// Render the seek / progress gauge showing elapsed and total time.
fn draw_progress(frame: &mut Frame, app: &App, area: Rect) {
    let pos = app.player.position().unwrap_or(Duration::ZERO);
    let dur = app.player.duration().unwrap_or(Duration::ZERO);

    let ratio = if dur.is_zero() {
        0.0_f64
    } else {
        (pos.as_secs_f64() / dur.as_secs_f64()).clamp(0.0, 1.0)
    };

    let label = format!(
        "{}  /  {}",
        fmt_duration(Some(pos)),
        fmt_duration(Some(dur))
    );

    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::NONE))
        .gauge_style(Style::default().fg(C_ACCENT).bg(C_DIM))
        .ratio(ratio)
        .label(label);

    frame.render_widget(gauge, area);
}

// ---------------------------------------------------------------------------
// Transport hints — Winamp-style key binding summary
// ---------------------------------------------------------------------------

/// Render the transport shortcut hints (or a status / error message).
///
/// When `app.status_message` is set (e.g. after adding a file or an error),
/// that message is shown instead of the hints so the user gets immediate
/// feedback.  The message is cleared on the next relevant action.
/// Render the transport shortcut hints (or a status / error message).
///
/// When `app.status_message` is set the message is shown instead of hints.
/// `[j]jump` and `[i]help` appear to the right of the playback buttons,
/// separated by extra whitespace so they read as a distinct group.
/// When the playlist panel is hidden, `[p]playlist` also appears here so
/// the user always has a way to bring it back.
fn draw_transport_hints(frame: &mut Frame, app: &App, area: Rect) {
    let content = if let Some(msg) = &app.status_message {
        Line::from(Span::styled(msg.as_str(), Style::default().fg(C_ERR)))
    } else {
        Line::from(vec![
            hint("z", "prev"),
            sep(),
            hint("x", "play"),
            sep(),
            hint("c", "pause"),
            sep(),
            hint("v", "stop"),
            sep(),
            hint("b", "next"),
            // Extra space visually separates the playback group from utility keys.
            Span::raw("   "),
            hint("j", "jump"),
            sep(),
            hint("i", "help"),
        ])
    };

    frame.render_widget(Paragraph::new(content).alignment(Alignment::Center), area);
}

// ---------------------------------------------------------------------------
// Playlist
// ---------------------------------------------------------------------------

/// Render the scrollable playlist.
///
/// The currently playing track is highlighted in green and prefixed with `▶`.
/// The cursor (keyboard-navigated selection) is highlighted with a dark blue
/// background so both current-playing and keyboard-position are visible
/// simultaneously.
fn draw_playlist(frame: &mut Frame, app: &App, area: Rect) {
    // Reserve space for the duration column: " M:SS" or " -:--" (5 chars + 1 gap).
    const DUR_COL: usize = 5;
    // Inner width = total - 2 border chars.  The track name gets the rest.
    let inner_w = area.width.saturating_sub(2) as usize;
    let name_w = inner_w.saturating_sub(DUR_COL + 1);

    let items: Vec<ListItem> = app
        .playlist
        .tracks
        .iter()
        .enumerate()
        .map(|(i, track)| {
            let is_current = i == app.playlist.current_index;
            let is_broken = track.broken;
            let dur_str = fmt_duration(track.duration);

            // Prefix: ▶ for current, ⚠ for broken (⚠▶ when both).
            let prefix = match (is_current, is_broken) {
                (true, true) => "⚠▶",
                (true, false) => "▶ ",
                (false, true) => "⚠ ",
                (false, false) => "  ",
            };

            // Build the left portion truncated to name_w.
            let index_part = format!("{}{}. ", prefix, i + 1);
            let avail_title = name_w.saturating_sub(index_part.chars().count());
            let display = track.display_name();
            let shown_title = if display.chars().count() > avail_title {
                display
                    .chars()
                    .take(avail_title.saturating_sub(1))
                    .collect::<String>()
                    + "…"
            } else {
                display
            };
            // Pad so the duration column always lines up.
            let left = format!("{}{:<width$}", index_part, shown_title, width = avail_title);

            let main_style = if is_broken {
                Style::default().fg(C_ERR).add_modifier(Modifier::DIM)
            } else if is_current {
                Style::default().fg(C_PLAYING).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(C_TEXT)
            };

            ListItem::new(Line::from(vec![
                Span::styled(left, main_style),
                Span::raw(" "),
                Span::styled(
                    format!("{:>width$}", dur_str, width = DUR_COL),
                    Style::default().fg(C_DIM),
                ),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    if !app.playlist.is_empty() {
        state.select(Some(app.playlist_cursor));
    }

    // Title shows track count; [p] is in the Sparkamp player box (lower right).
    let title = format!(" Playlist  ({} tracks) ", app.playlist.len());
    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(C_DIM)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(C_DIM));

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::Rgb(40, 40, 60)));

    frame.render_stateful_widget(list, area, &mut state);
}

// ---------------------------------------------------------------------------
// Playlist hints
// ---------------------------------------------------------------------------

/// Render the one-line row of playlist keyboard shortcut hints.
///
/// The `[p]` hint surfaces the playlist toggle so users can discover it
/// without reading the help text.
fn draw_playlist_hints(frame: &mut Frame, _app: &App, area: Rect) {
    let content = Line::from(vec![
        hint("n", "add"),
        sep(),
        hint(",", "move"),
        sep(),
        hint(".", "remove"),
        sep(),
        hint("/", "clear all"),
        sep(),
        hint("a", "viz"),
        sep(),
        hint("↑↓", "browse"),
        sep(),
        hint("Enter", "play"),
    ]);

    frame.render_widget(Paragraph::new(content).alignment(Alignment::Center), area);
}

// ---------------------------------------------------------------------------
// Visualizer renderers
// ---------------------------------------------------------------------------

/// Render the bar-spectrum visualizer.
///
/// Each bar represents a simulated discrete frequency bucket.  Lower-indexed
/// bars use a slower oscillation rate to mimic bass frequencies; higher-indexed
/// bars oscillate faster to mimic treble.  This gives the classic spectrum
/// analyser look even though no actual FFT is performed.
///
/// The number of bars equals `data.len()`, which the caller ensures is at
/// least 10 (the minimum for a meaningful frequency split).
/// Color zones: configurable via zone_colors parameter.
/// If mirror is true, bars extend both above and below the center line.
fn render_bars(
    data: &[f64],
    n_rows: usize,
    mirror: bool,
    color_zones: usize,
    zone_colors: &[String],
) -> Vec<Line<'static>> {
    let n_rows = n_rows.max(1);
    let num_zones = color_zones.max(1);

    // Parse hex color and find closest ANSI terminal color
    let hex_to_ansi = |hex: &str| -> Color {
        let hex = hex.trim_start_matches('#');
        if hex.len() >= 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0) as f32;
            let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0) as f32;
            let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0) as f32;

            // Map to closest ANSI color based on RGB values
            // Simple heuristic based on dominant channel and lightness
            let max_val = r.max(g).max(b);
            let min_val = r.min(g).min(b);
            let lightness = (max_val + min_val) / 2.0 / 255.0;

            if lightness < 0.2 {
                Color::DarkGray
            } else if lightness > 0.8 && r > 200.0 && g > 200.0 && b > 200.0 {
                Color::White
            } else if r > g && r > b {
                // Red/orange range
                if g > 100.0 {
                    Color::Yellow // orange-ish
                } else {
                    Color::Red
                }
            } else if g > r && g > b {
                // Green range
                if lightness > 0.5 {
                    Color::LightGreen
                } else {
                    Color::Green
                }
            } else if b > r && b > g {
                Color::Cyan
            } else {
                // Mixed colors - estimate hue
                if r >= g && r >= b {
                    Color::Yellow
                } else if g >= r && g >= b {
                    if lightness > 0.5 {
                        Color::LightGreen
                    } else {
                        Color::Green
                    }
                } else {
                    Color::Cyan
                }
            }
        } else {
            Color::Green // default
        }
    };

    // Get ANSI color for a zone
    let get_zone_color = |zone: usize| -> Color {
        let idx = zone.min(zone_colors.len().saturating_sub(1));
        hex_to_ansi(&zone_colors[idx])
    };

    if mirror {
        // Mirrored mode: center row is the reference, bars go up and down
        let center_row = n_rows / 2;
        (0..n_rows)
            .map(|row| {
                // Distance from center (0 = at center, positive = away)
                let dist_from_center = if row < center_row {
                    center_row - row
                } else {
                    row - center_row
                };
                // How far from center as a fraction of half the available space
                let half_space = center_row.max(1);
                let center_fraction = dist_from_center as f64 / half_space as f64;

                let spans: Vec<Span> = data
                    .iter()
                    .map(|&v| {
                        if v > center_fraction {
                            // This row is within the bar's amplitude
                            let zone = (((1.0 - v) * num_zones as f64) as usize).min(num_zones - 1);
                            Span::styled("█", Style::default().fg(get_zone_color(zone)))
                        } else {
                            Span::raw(" ")
                        }
                    })
                    .collect();
                Line::from(spans)
            })
            .collect()
    } else {
        // Singular mode: bars extend from bottom to top
        (0..n_rows)
            .map(|row| {
                // row 0 = top of the box, row n_rows-1 = bottom.
                // A bar with amplitude v fills upward from the bottom; this row
                // is "lit" when v exceeds the fraction that maps to this row.
                let row_fraction = 1.0 - (row as f64 + 1.0) / n_rows as f64;
                let spans: Vec<Span> = data
                    .iter()
                    .map(|&v| {
                        if v > row_fraction {
                            // Determine zone based on how high in the bar this row is
                            let bar_progress = (1.0 - v + row_fraction).max(0.0).min(1.0);
                            let zone = (((1.0 - bar_progress) * num_zones as f64) as usize)
                                .min(num_zones - 1);
                            Span::styled("█", Style::default().fg(get_zone_color(zone)))
                        } else {
                            Span::raw(" ")
                        }
                    })
                    .collect();
                Line::from(spans)
            })
            .collect()
    }
}

/// Render the real-audio waveform visualizer with zone-based colouring.
///
/// Data values are in [0, 1] where 0.5 = silence (centre line).
/// - A dim `─` baseline is drawn at the vertical mid-point as a reference.
/// - The waveform sample for each column is plotted as `●` coloured by zone.
/// - Vertical `│` connectors bridge gaps between adjacent samples; they are
///   coloured by whichever zone they fall in.
/// - Zone 0 (index 0 in zone_colors) is the bottom zone; zone N-1 is the top.
fn render_waveform(
    data: &[f64],
    n_rows: usize,
    num_zones: usize,
    zone_colors: &[String],
) -> Vec<Line<'static>> {
    let n_rows = n_rows.max(1);
    let num_zones = num_zones.max(1);

    // Map a hex colour string (#RRGGBB) to the nearest Ratatui terminal colour.
    let hex_to_color = |hex: &str| -> Color {
        let hex = hex.trim_start_matches('#');
        if hex.len() < 6 {
            return Color::Green;
        }
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(128);
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
        Color::Rgb(r, g, b)
    };

    // Given a terminal row (0 = top, n_rows-1 = bottom), return the zone index.
    // Zone 0 is at the bottom (highest row numbers), zone N-1 at the top.
    let zone_for_row = |row: usize| -> usize {
        // fraction from bottom: 0.0 at top, 1.0 at bottom
        let frac_from_bottom = (n_rows - 1 - row) as f64 / n_rows as f64;
        let z = (frac_from_bottom * num_zones as f64) as usize;
        z.min(num_zones - 1)
    };

    let zone_color = |row: usize| -> Color {
        let idx = zone_for_row(row).min(zone_colors.len().saturating_sub(1));
        hex_to_color(&zone_colors[idx])
    };

    // Pre-compute the target row for every column.
    let targets: Vec<usize> = data
        .iter()
        .map(|&v| {
            // v ∈ [0, 1]; 0.5 = centre. 1 = top, 0 = bottom.
            // Row 0 = top, n_rows-1 = bottom → invert.
            ((1.0 - v) * (n_rows - 1) as f64).round() as usize
        })
        .collect();

    let center_row = n_rows / 2;

    (0..n_rows)
        .map(|row| {
            let spans: Vec<Span> = (0..targets.len())
                .map(|col| {
                    let target = targets[col];

                    let connects_to_next = col + 1 < targets.len() && {
                        let next = targets[col + 1];
                        let (lo, hi) = if target < next {
                            (target, next)
                        } else {
                            (next, target)
                        };
                        row > lo && row < hi
                    };

                    if target == row {
                        Span::styled("●", Style::default().fg(zone_color(row)))
                    } else if connects_to_next {
                        Span::styled("│", Style::default().fg(zone_color(row)))
                    } else if row == center_row {
                        Span::styled("─", Style::default().fg(Color::Rgb(20, 60, 70)))
                    } else {
                        Span::raw(" ")
                    }
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Jump / search overlay
// ---------------------------------------------------------------------------

/// Render the jump-to-track search overlay.
///
/// Shows a text input for the search query at the top and a list of matching
/// results below.  The selected result is highlighted in yellow.  Navigation
/// is via `↑` / `↓`; `Enter` plays the highlighted track.
fn draw_jump_overlay(frame: &mut Frame, app: &App, area: Rect) {
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
fn draw_add_file_overlay(frame: &mut Frame, app: &App, area: Rect) {
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
fn draw_move_track_overlay(frame: &mut Frame, app: &App, area: Rect) {
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
fn draw_remove_track_overlay(frame: &mut Frame, app: &App, area: Rect) {
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
fn draw_help_overlay(frame: &mut Frame, app: &App, area: Rect) {
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
            Span::raw("    Switch Files / Playlists tab"),
        ]),
        Line::from(vec![
            key("  Enter"),
            Span::raw("   Add selected to playlist & play"),
        ]),
        Line::from(vec![key("  /  Ctrl+f"), Span::raw(" Activate search")]),
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

// ---------------------------------------------------------------------------
// Settings overlay
// ---------------------------------------------------------------------------

/// Names for the four settings tabs, shown in the tab bar.
const SETTINGS_TABS: [&str; 5] = [
    "Appearance",
    "Behavior",
    "Visualizer",
    "Filetypes",
    "Media Lib",
];

/// Render the full settings overlay with four tabs.
///
/// Layout (inside the popup border):
///   Row 0   — tab bar (highlighted tab shown in accent colour)
///   Row 1   — horizontal separator
///   Rows 2+ — setting rows for the active tab (label · value/toggle)
///   Last row — hint line: arrows=navigate, space=toggle, Esc=save & close
fn draw_settings_overlay(frame: &mut Frame, app: &App, state: &SettingsState, area: Rect) {
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
fn settings_rows_for_tab<'a>(
    app: &'a App,
    state: &'a SettingsState,
) -> Vec<(&'static str, String)> {
    match state.tab {
        // ── Appearance ────────────────────────────────────────────────────
        0 => vec![
            (
                "Theme",
                match app.config.appearance.theme {
                    ThemeChoice::Dark => "[ Dark  / Light ]  ●  Dark".to_string(),
                    ThemeChoice::Light => "[ Dark  / Light ]  ●  Light".to_string(),
                },
            ),
            (
                "Highlight color",
                accent_color_display(&app.config.appearance.accent_color),
            ),
            (
                "Custom skin name",
                if state.tab == 0 && state.cursor == 2 {
                    if let Some(buf) = &state.edit_buf {
                        format!("{buf}▌")
                    } else {
                        let v = &app.config.appearance.custom_skin;
                        if v.is_empty() {
                            "(none — uses Theme above)".to_string()
                        } else {
                            v.clone()
                        }
                    }
                } else {
                    let v = &app.config.appearance.custom_skin;
                    if v.is_empty() {
                        "(none — uses Theme above)".to_string()
                    } else {
                        v.clone()
                    }
                },
            ),
        ],

        // ── Behavior ─────────────────────────────────────────────────────
        1 => vec![
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
        2 => vec![(
            "Visualizer mode",
            match app.config.visualizer.mode {
                VisualizerMode::Bars => "[ Bars / Waveform ]  ●  Bars".to_string(),
                VisualizerMode::Waveform => {
                    "[ Bars / Waveform ]  ●  Waveform".to_string()
                }
            },
        )],

        // ── Filetypes ─────────────────────────────────────────────────────
        3 => {
            let viz_val = if state.cursor == 0 {
                if let Some(buf) = &state.edit_buf {
                    format!("{buf}▌") // show cursor block in edit mode
                } else {
                    let v = &app.config.plugins.visualizer_dir;
                    if v.is_empty() {
                        "(none)".to_string()
                    } else {
                        v.clone()
                    }
                }
            } else {
                let v = &app.config.plugins.visualizer_dir;
                if v.is_empty() {
                    "(none)".to_string()
                } else {
                    v.clone()
                }
            };
            let ft_val = if state.cursor == 1 {
                if let Some(buf) = &state.edit_buf {
                    format!("{buf}▌")
                } else {
                    let v = &app.config.plugins.filetype_dir;
                    if v.is_empty() {
                        "(none)".to_string()
                    } else {
                        v.clone()
                    }
                }
            } else {
                let v = &app.config.plugins.filetype_dir;
                if v.is_empty() {
                    "(none)".to_string()
                } else {
                    v.clone()
                }
            };
            vec![
                ("Visualizer plugin dir", viz_val),
                ("Filetype plugin dir", ft_val),
            ]
        }

        // ── Media Library ─────────────────────────────────────────────────
        4 => {
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
fn draw_eq_overlay(frame: &mut Frame, app: &App, state: &EqState, area: Rect) {
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

/// Render the ID3 tag editor overlay.
/// Return the last `n` characters of `s` (by Unicode scalar, not bytes).
/// When `s` is shorter than or equal to `n` characters, returns `s` unchanged.
/// Used for tail-scrolling text input fields so the cursor is always visible.
fn tail_chars(s: &str, n: usize) -> &str {
    if n == 0 {
        return "";
    }
    let char_count = s.chars().count();
    if char_count <= n {
        return s;
    }
    let skip = char_count - n;
    let byte_offset = s
        .char_indices()
        .nth(skip)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[byte_offset..]
}

///
/// When `state.show_extra` is false, shows the standard two-column form for
/// the 12 default tag fields.  When true, shows the Customize sub-panel
/// listing all other ID3v2 frames present in the file.
fn draw_id3_editor_overlay(frame: &mut Frame, state: &Id3EditorState, area: Rect) {
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
fn draw_id3_main_panel(frame: &mut Frame, state: &Id3EditorState, area: Rect) {
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
fn draw_id3_field_column(
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
fn draw_id3_extra_panel(frame: &mut Frame, state: &Id3EditorState, area: Rect) {
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

/// Compute a centred popup rectangle within `area`.
///
/// The popup is at most `max_w` columns wide and exactly `h` rows tall.  It
/// is centred both horizontally and vertically.  A minimum width of 30 is
/// enforced so the popup is never too narrow to read.
fn centered_popup(area: Rect, max_w: u16, h: u16) -> Rect {
    let w = area.width.saturating_sub(8).min(max_w).max(30);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

/// Build a key-hint span: `[key]label` in accent colour.
fn hint<'a>(key: &'a str, label: &'a str) -> Span<'a> {
    Span::styled(format!("[{}]{}", key, label), Style::default().fg(C_ACCENT))
}

/// Build a dim separator span (` · `).
fn sep() -> Span<'static> {
    Span::styled(" · ", Style::default().fg(C_DIM))
}

// ---------------------------------------------------------------------------
// Media library full-screen view
// ---------------------------------------------------------------------------

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
    }

    // Hint / toast bar — show a status message if one is pending, show a
    // minimal "Esc: exit search" hint while typing, otherwise display all hints.
    let hint_line = if let Some(msg) = toast {
        Line::from(Span::styled(msg, Style::default().fg(C_PLAYING)))
    } else if state.add_input.is_some() {
        Line::from(vec![
            hint("Enter", "add path"),
            sep(),
            hint("Esc", "cancel"),
        ])
    } else if state.search_active {
        Line::from(hint("Esc", "exit search"))
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
}

/// Width (chars) for each column ID in the Files tab.
fn ml_col_width(id: &str) -> usize {
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
fn ml_col_label(id: &str) -> &'static str {
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
fn ml_col_value<'a>(id: &str, t: &'a crate::media_library::LibTrack) -> std::borrow::Cow<'a, str> {
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
fn draw_ml_files(frame: &mut Frame, state: &MediaLibraryState, area: Rect) {
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
fn draw_ml_playlists(frame: &mut Frame, state: &MediaLibraryState, area: Rect) {
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

/// Truncate a string to at most `max_chars` characters, appending `…` when cut.
fn ml_truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars()
            .take(max_chars.saturating_sub(1))
            .collect::<String>()
            + "…"
    }
}
