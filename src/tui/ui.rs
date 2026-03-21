//! TUI rendering — converts [`App`] state into ratatui widget trees.
//!
//! ## Layout (top → bottom)
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │ [mini viz box (22 cols)] │ SparkAmp — ▶ Title — Artist [n/N] │  ← header (4 rows)
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
//! mirroring the classic Winamp 2.x layout where the spectrum / oscilloscope
//! sits to the left of the scrolling song title.
//!
//! When the user hides the playlist (`p` key) the playlist section collapses
//! to zero height, giving the remaining content more breathing room in narrow
//! terminals.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph,
    },
    Frame,
};
use std::time::Duration;

use super::{App, Mode};
use crate::{config::VisualizerMode, engine::PlayerState, model::fmt_duration};

// ---------------------------------------------------------------------------
// Colour palette — centralised so re-skinning only needs edits here
// ---------------------------------------------------------------------------

/// Accent colour: used for borders, labels and the oscilloscope waveform.
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
    match &app.mode {
        Mode::Jump { .. }        => draw_jump_overlay(frame, app, area),
        Mode::AddFile { .. }     => draw_add_file_overlay(frame, app, area),
        Mode::MoveTrack { .. }   => draw_move_track_overlay(frame, app, area),
        Mode::RemoveTrack { .. } => draw_remove_track_overlay(frame, app, area),
        Mode::Help               => draw_help_overlay(frame, area),
        Mode::Normal             => {}
    }
}

// ---------------------------------------------------------------------------
// Header — mini visualizer on the left, track info on the right
// ---------------------------------------------------------------------------

/// Draw the combined header row.
///
/// The row is split horizontally:
/// - **Left 22 columns**: a small bordered box containing the visualizer
///   (bars or oscilloscope), labelled with the current mode name.
/// - **Right (remainder)**: now-playing information — state icon, title,
///   artist and track index — inside a bordered box titled "SparkAmp".
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
/// Uses the same [`render_bars`] / [`render_oscilloscope`] functions as the
/// full-size standalone visualizer so the rendering logic stays in one place.
fn draw_header_viz(frame: &mut Frame, app: &App, area: Rect) {
    let mode_label = match app.config.visualizer.mode {
        VisualizerMode::Bars        => "▲",
        VisualizerMode::Oscilloscope => "~",
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
            Rect { y: mid_y, height: 1, ..inner },
        );
        return;
    }

    // Request enough data points to fill the mini box width.
    let col_count = (inner.width as usize).max(10);
    let data = app.visualizer_data(col_count);
    let n_rows = inner.height as usize;

    let lines: Vec<Line> = match app.config.visualizer.mode {
        VisualizerMode::Bars        => render_bars(&data, n_rows),
        VisualizerMode::Oscilloscope => render_oscilloscope(&data, n_rows),
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
        PlayerState::Paused  => ("⏸", C_WARN),
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
        format!(" [{}/{}]", app.playlist.current_index + 1, app.playlist.len())
    } else {
        String::new()
    };

    // Two-line content: scrolling title on top, state + index below.
    let marquee_line = Line::from(
        Span::styled(marquee, Style::default().fg(C_TEXT).add_modifier(Modifier::BOLD))
    );
    let state_line = Line::from(vec![
        Span::styled(format!("{} ", state_icon), Style::default().fg(state_color)),
        Span::styled(index_label, Style::default().fg(C_DIM)),
    ]);

    // [q] quit lives in the upper-right corner of the player box; [p] toggle
    // in the lower-right so it is always discoverable from the player view.
    let pl_hint = if app.playlist_visible { " [p] hide " } else { " [p] show " };
    let block = Block::default()
        .title_top(
            Line::from(Span::styled(
                " SparkAmp ",
                Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
            ))
            .centered(),
        )
        .title_top(
            Line::from(Span::styled(" [q] quit ", Style::default().fg(C_DIM)))
                .right_aligned(),
        )
        .title_bottom(
            Line::from(Span::styled(pl_hint, Style::default().fg(C_DIM)))
                .right_aligned(),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(C_ACCENT));

    frame.render_widget(
        Paragraph::new(vec![marquee_line, state_line]).block(block).alignment(Alignment::Center),
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

    let label = format!("{}  /  {}", fmt_duration(Some(pos)), fmt_duration(Some(dur)));

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

    frame.render_widget(
        Paragraph::new(content).alignment(Alignment::Center),
        area,
    );
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
    let name_w  = inner_w.saturating_sub(DUR_COL + 1);

    let items: Vec<ListItem> = app
        .playlist
        .tracks
        .iter()
        .enumerate()
        .map(|(i, track)| {
            let is_current = i == app.playlist.current_index;
            let is_broken  = track.broken;
            let dur_str    = fmt_duration(track.duration);

            // Prefix: ▶ for current, ⚠ for broken (⚠▶ when both).
            let prefix = match (is_current, is_broken) {
                (true,  true)  => "⚠▶",
                (true,  false) => "▶ ",
                (false, true)  => "⚠ ",
                (false, false) => "  ",
            };

            // Build the left portion truncated to name_w.
            let index_part  = format!("{}{}. ", prefix, i + 1);
            let avail_title = name_w.saturating_sub(index_part.chars().count());
            let display     = track.display_name();
            let shown_title = if display.chars().count() > avail_title {
                display.chars().take(avail_title.saturating_sub(1)).collect::<String>() + "…"
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
                Span::styled(format!("{:>width$}", dur_str, width = DUR_COL), Style::default().fg(C_DIM)),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    if !app.playlist.is_empty() {
        state.select(Some(app.playlist_cursor));
    }

    // Title shows track count; [p] is in the SparkAmp player box (lower right).
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

    frame.render_widget(
        Paragraph::new(content).alignment(Alignment::Center),
        area,
    );
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
/// least 10 (the minimum for a meaningful frequency split).  Colour grades
/// from green at the bottom through cyan to blue at the top so the display
/// pops against the dark background.
fn render_bars(data: &[f64], n_rows: usize) -> Vec<Line<'static>> {
    let n_rows = n_rows.max(1);
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
                        // Colour gradient: bottom rows green, middle cyan, top blue.
                        let intensity = row as f64 / n_rows as f64;
                        let color = if intensity < 0.33 {
                            Color::Blue
                        } else if intensity < 0.67 {
                            Color::Cyan
                        } else {
                            Color::Green
                        };
                        Span::styled("█", Style::default().fg(color))
                    } else {
                        Span::raw(" ")
                    }
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}

/// Render the oscilloscope waveform visualizer.
///
/// The waveform is rendered as a continuous line:
/// - A dim `─` baseline is drawn at the vertical mid-point as a reference.
/// - The waveform sample for each column is plotted as a `●` at its target row.
/// - When adjacent samples are more than one row apart, `│` connectors fill
///   the gap between them so the trace looks continuous rather than a scatter
///   of isolated dots.
///
/// This matches the look of a classic triggered oscilloscope display.
fn render_oscilloscope(data: &[f64], n_rows: usize) -> Vec<Line<'static>> {
    let n_rows = n_rows.max(1);

    // Pre-compute the target row for every column so we can look ahead when
    // drawing connectors.
    let targets: Vec<usize> = data
        .iter()
        .map(|&v| {
            // v ∈ [0, 1]; 0 = bottom, 1 = top.
            // Target row: 0 = top, n_rows-1 = bottom → invert v.
            ((1.0 - v) * (n_rows - 1) as f64).round() as usize
        })
        .collect();

    let center_row = n_rows / 2;

    (0..n_rows)
        .map(|row| {
            let spans: Vec<Span> = (0..targets.len())
                .map(|col| {
                    let target = targets[col];

                    // Determine whether this row should show a connector that
                    // bridges the gap between this column's sample and the
                    // next column's sample.
                    let connects_to_next = col + 1 < targets.len() && {
                        let next = targets[col + 1];
                        let (lo, hi) = if target < next {
                            (target, next)
                        } else {
                            (next, target)
                        };
                        // The connector occupies rows strictly between the two
                        // sample positions (not the sample row itself).
                        row > lo && row < hi
                    };

                    if target == row {
                        // Waveform sample position — show the dot.
                        Span::styled("●", Style::default().fg(C_ACCENT))
                    } else if connects_to_next {
                        // Vertical bridge between two non-adjacent samples.
                        Span::styled(
                            "│",
                            Style::default().fg(Color::Rgb(0, 100, 130)),
                        )
                    } else if row == center_row {
                        // Reference baseline — always visible as orientation aid.
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
    let Mode::Jump { query, results, selected } = &app.mode else { return };

    let h = area.height.saturating_sub(4).min(22).max(8);
    let popup = Rect { height: h, ..centered_popup(area, 70, h) };

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
    let Mode::AddFile { input } = &app.mode else { return };

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
    let Mode::MoveTrack { input, from } = &app.mode else { return };

    let popup = centered_popup(area, 50, 5);
    frame.render_widget(Clear, popup);

    let (title, prompt) = match from {
        None    => (
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
    let Mode::RemoveTrack { input } = &app.mode else { return };

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

/// Render a full keyboard-shortcut reference overlay.  Any key dismisses it.
fn draw_help_overlay(frame: &mut Frame, area: Rect) {
    let lines: Vec<Line> = vec![
        Line::from(Span::styled("── Playback ─────────────────────────────────────────", Style::default().fg(C_DIM))),
        Line::from(vec![Span::styled("  z", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw("   Previous / restart")]),
        Line::from(vec![Span::styled("  x", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw("   Play")]),
        Line::from(vec![Span::styled("  c", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw("   Pause / resume")]),
        Line::from(vec![Span::styled("  v", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw("   Stop")]),
        Line::from(vec![Span::styled("  b", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw("   Next track")]),
        Line::from(vec![Span::styled("  ← →", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw(" Seek −5 s / +5 s")]),
        Line::from(""),
        Line::from(Span::styled("── Volume ────────────────────────────────────────────", Style::default().fg(C_DIM))),
        Line::from(vec![Span::styled("  -", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw("   Volume down 5 %")]),
        Line::from(vec![Span::styled("  =", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw("   Volume up 5 %")]),
        Line::from(""),
        Line::from(Span::styled("── Playlist ──────────────────────────────────────────", Style::default().fg(C_DIM))),
        Line::from(vec![Span::styled("  n", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw("   Add file(s) / folder(s)  (comma-separated list ok)")]),
        Line::from(vec![Span::styled("  ,", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw("   Move track (enter from → to positions)")]),
        Line::from(vec![Span::styled("  .", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw("   Remove track by number")]),
        Line::from(vec![Span::styled("  /", Style::default().fg(C_ERR  ).add_modifier(Modifier::BOLD)), Span::raw("   Clear all tracks")]),
        Line::from(vec![Span::styled("  j", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw("   Jump / search")]),
        Line::from(vec![Span::styled("  ↑ k", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw(" Browse up")]),
        Line::from(vec![Span::styled("  ↓ l", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw(" Browse down")]),
        Line::from(vec![Span::styled("  Enter", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw(" Play selected")]),
        Line::from(vec![Span::styled("  Del", Style::default().fg(C_ERR  ).add_modifier(Modifier::BOLD)), Span::raw("   Remove highlighted track")]),
        Line::from(vec![Span::styled("  p", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw("   Toggle playlist panel")]),
        Line::from(""),
        Line::from(Span::styled("── View / Other ──────────────────────────────────────", Style::default().fg(C_DIM))),
        Line::from(vec![Span::styled("  a", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw("   Cycle visualizer mode")]),
        Line::from(vec![Span::styled("  i", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)), Span::raw("   Show this help")]),
        Line::from(vec![Span::styled("  q / Esc", Style::default().fg(C_WARN  ).add_modifier(Modifier::BOLD)), Span::raw(" Quit")]),
        Line::from(""),
        Line::from(Span::styled("  (any key closes this overlay)", Style::default().fg(C_DIM))),
    ];

    let h = (lines.len() as u16 + 4).min(area.height.saturating_sub(4));
    let popup = centered_popup(area, 60, h);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(Span::styled(" Keyboard Shortcuts ", Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(C_ACCENT)),
            )
            .style(Style::default().fg(C_TEXT)),
        popup,
    );
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
    Rect { x, y, width: w, height: h }
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

/// Build a key-hint span: `[key]label` in accent colour.
fn hint<'a>(key: &'a str, label: &'a str) -> Span<'a> {
    Span::styled(
        format!("[{}]{}", key, label),
        Style::default().fg(C_ACCENT),
    )
}

/// Build a dim separator span (` · `).
fn sep() -> Span<'static> {
    Span::styled(" · ", Style::default().fg(C_DIM))
}

