//! Settings and equalizer overlay key handling.

use crossterm::event::{KeyCode, KeyModifiers};

use super::{settings_tab_len, App, Mode};

impl App {

    /// Handle a key press inside the settings overlay.
    ///
    /// Key map (normal navigation):
    ///   Left / Right (or h / l) — switch tabs
    ///   Up / Down (or k / j)   — move cursor within the active tab
    ///   Space / Enter          — toggle a bool, cycle an enum, or enter
    ///                            text-edit mode for a string field
    ///   Esc / e                — save config to disk and close the overlay
    ///
    /// Key map (text-edit mode for Filetypes paths):
    ///   Any printable char     — append to the edit buffer
    ///   Backspace              — delete the last character
    ///   Enter                  — confirm and write the value back to config
    ///   Esc                    — abandon the edit (revert to previous value)
    pub(super) fn handle_settings(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        use crate::config::{PlaylistAddBehavior, VisualizerMode};

        // Alt + transport keys pass through to the player without closing settings.
        if modifiers.contains(KeyModifiers::ALT) {
            match code {
                KeyCode::Char('z') => {
                    self.play_prev();
                    return;
                }
                KeyCode::Char('x') => {
                    self.play_current();
                    return;
                }
                KeyCode::Char('c') => {
                    let _ = self.player.toggle_pause();
                    return;
                }
                KeyCode::Char('v') => {
                    let _ = self.player.stop();
                    return;
                }
                KeyCode::Char('b') => {
                    self.play_next();
                    return;
                }
                KeyCode::Char('j') => {
                    let results = (0..self.playlist.len()).collect();
                    self.mode = Mode::Jump {
                        query: String::new(),
                        results,
                        selected: 0,
                        from_media_library: false,
                    };
                    return;
                }
                _ => {}
            }
        }

        // Snapshot the read-only fields we need before any mutable borrow.
        let (tab, cursor, in_edit) = match &self.mode {
            Mode::Settings(s) => (s.tab, s.cursor, s.edit_buf.is_some()),
            _ => return,
        };

        // ── Text-edit mode (Filetypes string fields) ──────────────────────
        if in_edit {
            match code {
                // Esc: abandon the edit, restore the previous value.
                KeyCode::Esc => {
                    if let Mode::Settings(s) = &mut self.mode {
                        s.edit_buf = None;
                    }
                }
                // Enter: commit the typed value back to config.
                KeyCode::Enter => {
                    let val = match &mut self.mode {
                        Mode::Settings(s) => s.edit_buf.take().unwrap_or_default(),
                        _ => return,
                    };
                    // Dispatch by (tab, cursor).
                    match (tab, cursor) {
                        (2, 0) => self.config.plugins.visualizer_dir = val,
                        (2, 1) => self.config.plugins.filetype_dir = val,
                        (3, 2) => {
                            // Parse interval minutes; silently keep old value on error.
                            if let Ok(mins) = val.trim().parse::<u64>() {
                                self.config.media_library.set_rescan_interval_mins(mins);
                            }
                        }
                        _ => {}
                    }
                }
                // Backspace: delete last character from the buffer.
                KeyCode::Backspace => {
                    if let Mode::Settings(s) = &mut self.mode {
                        if let Some(buf) = &mut s.edit_buf {
                            buf.pop();
                        }
                    }
                }
                // Any printable character: append to the buffer.
                KeyCode::Char(ch) => {
                    if let Mode::Settings(s) = &mut self.mode {
                        if let Some(buf) = &mut s.edit_buf {
                            buf.push(ch);
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        // ── Normal navigation ─────────────────────────────────────────────
        match code {
            // Esc / e: save config and close.
            KeyCode::Esc | KeyCode::Char('e') | KeyCode::Char('E') => {
                let _ = self.config.save();
                self.mode = Mode::Normal;
            }

            // Left / h: go to the previous tab.
            KeyCode::Left | KeyCode::Char('h') => {
                if let Mode::Settings(s) = &mut self.mode {
                    s.tab = s.tab.saturating_sub(1);
                    s.cursor = 0;
                }
            }
            // Right / l: go to the next tab (tabs 0–3).
            KeyCode::Right | KeyCode::Char('l') => {
                if let Mode::Settings(s) = &mut self.mode {
                    if s.tab < 3 {
                        s.tab += 1;
                    }
                    s.cursor = 0;
                }
            }

            // Up / k: move cursor up within the active tab.
            KeyCode::Up | KeyCode::Char('k') => {
                if let Mode::Settings(s) = &mut self.mode {
                    s.cursor = s.cursor.saturating_sub(1);
                }
            }
            // Down / j: move cursor down within the active tab.
            KeyCode::Down => {
                let tab_len = settings_tab_len(tab);
                if let Mode::Settings(s) = &mut self.mode {
                    if s.cursor + 1 < tab_len {
                        s.cursor += 1;
                    }
                }
            }
            // Also use 'j' for down navigation in settings (not jump).
            KeyCode::Char('j') if !modifiers.contains(KeyModifiers::ALT) => {
                let tab_len = settings_tab_len(tab);
                if let Mode::Settings(s) = &mut self.mode {
                    if s.cursor + 1 < tab_len {
                        s.cursor += 1;
                    }
                }
            }

            // Space / Enter: act on the focused setting.
            KeyCode::Enter | KeyCode::Char(' ') => {
                match tab {
                    // Behavior: row 0 = toggle autoplay; row 1 = cycle playlist add behavior.
                    0 => match cursor {
                        0 => {
                            self.config.behavior.autoplay_on_add =
                                !self.config.behavior.autoplay_on_add;
                        }
                        1 => {
                            self.config.behavior.playlist_add_behavior =
                                match self.config.behavior.playlist_add_behavior {
                                    PlaylistAddBehavior::Append => PlaylistAddBehavior::Replace,
                                    PlaylistAddBehavior::Replace => PlaylistAddBehavior::Append,
                                };
                        }
                        _ => {}
                    },
                    // Visualizer: cycle Bars ↔ Waveform in the TUI. Granite
                    // (GUI-only) is skipped — if it's the active mode, the
                    // toggle moves to Bars.
                    1 => {
                        self.config.visualizer.mode = match self.config.visualizer.mode {
                            VisualizerMode::Bars     => VisualizerMode::Waveform,
                            VisualizerMode::Waveform => VisualizerMode::Bars,
                            VisualizerMode::Granite  => VisualizerMode::Bars,
                        };
                    }
                    // Filetypes: enter text-edit mode for the focused path field.
                    2 => {
                        let current = match cursor {
                            0 => self.config.plugins.visualizer_dir.clone(),
                            1 => self.config.plugins.filetype_dir.clone(),
                            _ => String::new(),
                        };
                        if let Mode::Settings(s) = &mut self.mode {
                            s.edit_buf = Some(current);
                        }
                    }
                    // Media Library: toggle booleans or edit the interval field.
                    3 => {
                        match cursor {
                            0 => {
                                self.config.media_library.rescan_on_startup =
                                    !self.config.media_library.rescan_on_startup;
                            }
                            1 => {
                                self.config.media_library.periodic_rescan =
                                    !self.config.media_library.periodic_rescan;
                            }
                            2 => {
                                // Enter text-edit mode for the interval value.
                                let current =
                                    self.config.media_library.rescan_interval_mins.to_string();
                                if let Mode::Settings(s) = &mut self.mode {
                                    s.edit_buf = Some(current);
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Equalizer handler
    // -----------------------------------------------------------------------

    /// Handle key events while the equalizer overlay is open.
    ///
    /// Key map:
    ///   ←/→ (h/l)     — select previous / next band
    ///   ↑/↓ (+/-)     — raise / lower the selected band by 1 dB
    ///   PgUp/PgDn     — raise / lower by 3 dB (coarse)
    ///   [ / ]         — decrease / increase pre-amp by 5 %
    ///   p             — cycle to the next EQ preset
    ///   r             — reset all bands to flat (0 dB)
    ///   t             — toggle EQ enabled / disabled
    ///   Esc / u       — close the overlay (saves config)
    pub(super) fn handle_equalizer(&mut self, code: KeyCode) {
        let sel = match &self.mode {
            Mode::Equalizer(s) => s.selected_band,
            _ => return,
        };
        // sel == 10 means the pre-amp column is selected.
        let preamp_selected = sel == 10;

        // ── Helpers ───────────────────────────────────────────────────────────
        let adjust_band = |app: &mut App, delta: f64| {
            let b = match &app.mode {
                Mode::Equalizer(s) => s.selected_band,
                _ => return,
            };
            if b >= 10 {
                return;
            }
            let candidate = app.config.equalizer.bands.get(b).copied().unwrap_or(0.0) + delta;
            app.ctrl().set_eq_band(b, candidate);
        };

        let adjust_preamp = |app: &mut App, delta: f64| {
            let new = app.config.equalizer.preamp + delta;
            app.ctrl().set_preamp(new);
        };

        match code {
            // Close and save.
            KeyCode::Esc | KeyCode::Char('u') | KeyCode::Char('U') => {
                let _ = self.config.save();
                self.mode = Mode::Normal;
                return;
            }

            // Navigate: bands 0-9, then pre-amp at position 10.
            KeyCode::Left | KeyCode::Char('h') => {
                if let Mode::Equalizer(s) = &mut self.mode {
                    s.selected_band = s.selected_band.saturating_sub(1);
                }
                return;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if let Mode::Equalizer(s) = &mut self.mode {
                    if s.selected_band < 10 {
                        s.selected_band += 1;
                    }
                }
                return;
            }

            // Up/Down: adjust band gain or pre-amp depending on selection.
            KeyCode::Up | KeyCode::Char('+') => {
                if preamp_selected {
                    adjust_preamp(self, 0.05);
                } else {
                    adjust_band(self, 1.0);
                }
            }
            KeyCode::Down | KeyCode::Char('-') => {
                if preamp_selected {
                    adjust_preamp(self, -0.05);
                } else {
                    adjust_band(self, -1.0);
                }
            }

            // Coarse adjustment (3 dB / 15 %).
            KeyCode::PageUp => {
                if preamp_selected {
                    adjust_preamp(self, 0.15);
                } else {
                    adjust_band(self, 3.0);
                }
            }
            KeyCode::PageDown => {
                if preamp_selected {
                    adjust_preamp(self, -0.15);
                } else {
                    adjust_band(self, -3.0);
                }
            }

            // Cycle presets.
            KeyCode::Char('p') | KeyCode::Char('P') => {
                self.ctrl().cycle_eq_preset();
            }

            // Reset to flat.
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.ctrl().reset_eq_to_flat();
            }

            // Toggle enabled / disabled.
            KeyCode::Char('t') | KeyCode::Char('T') => {
                let new_enabled = !self.config.equalizer.enabled;
                self.ctrl().set_eq_enabled(new_enabled);
            }

            // Playback controls — execute without closing the overlay.
            KeyCode::Char('z') | KeyCode::Char('Z') => {
                self.play_prev();
            }
            KeyCode::Char('x') | KeyCode::Char('X') => {
                self.play_current();
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                let _ = self.player.toggle_pause();
            }
            KeyCode::Char('v') | KeyCode::Char('V') => {
                let _ = self.player.stop();
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                self.play_next();
            }

            // Jump — switch to jump mode (closes EQ overlay).
            KeyCode::Char('j') | KeyCode::Char('J') => {
                let _ = self.config.save();
                let results = (0..self.playlist.len()).collect();
                self.mode = Mode::Jump {
                    query: String::new(),
                    results,
                    selected: 0,
                    from_media_library: false,
                };
            }

            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Tick
    // -----------------------------------------------------------------------
}
