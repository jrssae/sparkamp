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
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph},
    Frame,
};
use std::time::Duration;

use super::{App, Mode};
use crate::{
    config::VisualizerMode,
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

// ---------------------------------------------------------------------------
// Top-level draw — assembles all sections into the terminal frame
// ---------------------------------------------------------------------------

mod id3;
mod media_library;
mod overlays;
mod settings_eq;

use id3::draw_id3_editor_overlay;
use media_library::draw_media_library;
use overlays::{
    draw_add_file_overlay, draw_help_overlay, draw_jump_overlay,
    draw_move_track_overlay, draw_remove_track_overlay,
};
use settings_eq::{draw_eq_overlay, draw_settings_overlay};

// One import prelude shared by the sibling files so each split file
// does not repeat the ratatui boilerplate.  pub(super) re-exports only;
// nothing leaks outside `ui`.
mod imports {
    pub(super) use ratatui::{
        layout::{Constraint, Direction, Layout, Rect},
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
        Frame,
    };
    pub(super) use crate::config::PlaylistAddBehavior;
    pub(super) use super::super::{
        id3_genre_matches, render_progress_line, App, BurnSetupState,
        DiscTagEditState, EqState, Id3EditorState, MediaLibraryState,
        MediaLibraryTab, MetaField, Mode, RipSetupState, SettingsState,
    };
    pub(super) use crate::config::VisualizerMode;
    pub(super) use super::{
        centered_popup, hint, sep, tail_chars, C_ACCENT, C_DIM, C_ERR, C_PLAYING,
        C_TEXT, C_WARN,
    };
}

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
            // Read-only: the overlay shows the queue of the drive currently
            // in view, not a picker — same shown-drive rule as `b`.
            let empty_burn_list = crate::disc::burnlist::BurnList::default();
            let burn_list = state
                .drives
                .get(state.selected_drive)
                .and_then(|d| app.burn_queues.get(&d.id))
                .unwrap_or(&empty_burn_list);
            draw_media_library(
                frame,
                state,
                app.status_message.as_deref(),
                app.rip_progress.as_ref(),
                app.burn_phase.as_ref(),
                burn_list,
                app.anim_tick,
                area,
            )
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
pub(super) fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
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
pub(super) fn draw_header_viz(frame: &mut Frame, app: &App, area: Rect) {
    let mode_label = match app.config.visualizer.mode {
        VisualizerMode::Bars     => "▲",
        VisualizerMode::Waveform => "~",
        VisualizerMode::Granite  => "▲", // TUI falls back to bars rendering
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
        // Granite has no terminal renderer; fall back to bars in the TUI so
        // a user who toggled to Granite from the GUI still sees something.
        VisualizerMode::Bars | VisualizerMode::Granite => {
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
pub(super) fn draw_header_track_info(frame: &mut Frame, app: &App, area: Rect) {
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
pub(super) fn draw_progress(frame: &mut Frame, app: &App, area: Rect) {
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
pub(super) fn draw_transport_hints(frame: &mut Frame, app: &App, area: Rect) {
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
pub(super) fn draw_playlist(frame: &mut Frame, app: &App, area: Rect) {
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
pub(super) fn draw_playlist_hints(frame: &mut Frame, _app: &App, area: Rect) {
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
pub(super) fn render_bars(
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
pub(super) fn render_waveform(
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

/// Render the ID3 tag editor overlay.
/// Return the last `n` characters of `s` (by Unicode scalar, not bytes).
/// When `s` is shorter than or equal to `n` characters, returns `s` unchanged.
/// Used for tail-scrolling text input fields so the cursor is always visible.
pub(super) fn tail_chars(s: &str, n: usize) -> &str {
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

/// Compute a centred popup rectangle within `area`.
///
/// The popup is at most `max_w` columns wide and exactly `h` rows tall.  It
/// is centred both horizontally and vertically.  A minimum width of 30 is
/// enforced so the popup is never too narrow to read.
pub(super) fn centered_popup(area: Rect, max_w: u16, h: u16) -> Rect {
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
pub(super) fn hint<'a>(key: &'a str, label: &'a str) -> Span<'a> {
    Span::styled(format!("[{}]{}", key, label), Style::default().fg(C_ACCENT))
}

/// Build a dim separator span (` · `).
pub(super) fn sep() -> Span<'static> {
    Span::styled(" · ", Style::default().fg(C_DIM))
}

// ---------------------------------------------------------------------------
// Media library full-screen view
// ---------------------------------------------------------------------------
