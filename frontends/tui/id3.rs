//! ID3 editor overlay key handling and save.

use crossterm::event::{KeyCode, KeyModifiers};

use sparkamp::engine::PlayerState;
use sparkamp::id3_editor::{write_extra_frame, write_tag_fields};
use sparkamp::model::Track;

use super::{id3_genre_matches, App, Mode};

impl App {

    /// Handle a key press when the ID3 editor overlay is open.
    ///
    /// The editor has two sub-modes:
    /// - **Main fields** (`show_extra == false`): the default 12-field form.
    /// - **Customize panel** (`show_extra == true`): a scrollable list of any
    ///   additional ID3v2 frames already present in the file, with in-place
    ///   editing.
    pub(super) fn handle_id3_editor(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // Alt+z/x/c/v/b/j trigger transport controls without closing the editor.
        // This check runs before the show_extra dispatch so it applies to both
        // the main panel and the Customize sub-panel.
        if modifiers.contains(KeyModifiers::ALT) {
            match code {
                KeyCode::Char('z') => {
                    self.play_prev();
                    return;
                }
                KeyCode::Char('x') => {
                    if *self.player.state() == PlayerState::Stopped {
                        self.play_current();
                    } else {
                        let _ = self.player.play();
                    }
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
                KeyCode::Char('j') | KeyCode::Char('J') => {
                    // Opens Jump, which changes mode and closes the editor.
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

        // --- Customize (extra frames) sub-panel ---
        if let Mode::Id3Editor(ref state) = self.mode {
            if state.show_extra {
                self.handle_id3_extra(code, modifiers);
                return;
            }
        }

        // --- Main fields panel ---
        match code {
            // Esc: close the editor without saving.
            KeyCode::Esc => {
                self.mode = Mode::Normal;
            }

            // Tab / Shift-Tab: advance/retreat through the offered rows.
            KeyCode::Tab => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.focused = (s.focused + 1) % s.rows.len().max(1);
                    s.cursor = s.focused_value_mut()
                        .chars()
                        .count();
                    s.genre_sel = 0;
                    s.status = None;
                }
            }
            KeyCode::BackTab => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.focused = if s.focused == 0 { s.rows.len().saturating_sub(1) } else { s.focused - 1 };
                    s.cursor = s.focused_value_mut()
                        .chars()
                        .count();
                    s.genre_sel = 0;
                    s.status = None;
                }
            }

            // Up / Down: navigate genre suggestions when on the genre row,
            // otherwise move between rows.
            KeyCode::Down => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    if s.focused_is_genre() {
                        let n = id3_genre_matches(&s.fields.genre).len();
                        if n > 0 {
                            s.genre_sel = (s.genre_sel + 1).min(n - 1);
                        }
                    } else {
                        s.focused = (s.focused + 1) % s.rows.len().max(1);
                        s.cursor = s.focused_value_mut()
                            .chars()
                            .count();
                        s.genre_sel = 0;
                    }
                }
            }
            KeyCode::Up => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    if s.focused_is_genre() {
                        s.genre_sel = s.genre_sel.saturating_sub(1);
                    } else {
                        s.focused = if s.focused == 0 { s.rows.len().saturating_sub(1) } else { s.focused - 1 };
                        s.cursor = s.focused_value_mut()
                            .chars()
                            .count();
                        s.genre_sel = 0;
                    }
                }
            }

            // Left / Right: move cursor within the focused field.
            KeyCode::Left => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.cursor = s.cursor.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    let len = s.focused_value_mut()
                        .chars()
                        .count();
                    s.cursor = (s.cursor + 1).min(len);
                }
            }

            // Home / End: jump to the start or end of the focused field.
            KeyCode::Home => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.cursor = 0;
                }
            }
            KeyCode::End => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.cursor = s.focused_value_mut()
                        .chars()
                        .count();
                }
            }

            // Enter: if genre field has suggestions, accept the highlighted one;
            // otherwise advance to the next field.
            KeyCode::Enter => {
                let accept = if let Mode::Id3Editor(ref s) = self.mode {
                    if s.focused_is_genre() {
                        let matches = id3_genre_matches(&s.fields.genre);
                        matches.get(s.genre_sel).map(|g| g.to_string())
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    if let Some(chosen) = accept {
                        s.fields.genre = chosen;
                        s.focused = (s.focused + 1) % s.rows.len().max(1);
                        s.cursor = s.focused_value_mut()
                            .chars()
                            .count();
                        s.genre_sel = 0;
                    } else {
                        s.focused = (s.focused + 1) % s.rows.len().max(1);
                        s.cursor = s.focused_value_mut()
                            .chars()
                            .count();
                        s.genre_sel = 0;
                    }
                }
            }

            // Ctrl+S: save tags and close the editor.
            KeyCode::Char('s') | KeyCode::Char('S')
                if modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.id3_save_and_close();
            }

            // c / C: open the Customize (extra frames) sub-panel.
            KeyCode::Char('c') | KeyCode::Char('C') => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.show_extra = true;
                    s.extra_focused = 0;
                    s.extra_editing = false;
                    s.extra_input.clear();
                }
            }

            // Backspace: delete the character immediately before the cursor.
            KeyCode::Backspace => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    let c = s.cursor;
                    if c > 0 {
                        let byte_idx = {
                            let field = s.focused_value_mut();
                            field.char_indices().nth(c - 1).map(|(i, _)| i)
                        };
                        if let Some(bi) = byte_idx {
                            s.focused_value_mut().remove(bi);
                            s.cursor -= 1;
                        }
                    }
                    s.genre_sel = 0;
                }
            }

            // Any printable character: insert at the cursor position.
            KeyCode::Char(ch) => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    let c = s.cursor;
                    let byte_idx = {
                        let field = s.focused_value_mut();
                        field
                            .char_indices()
                            .nth(c)
                            .map(|(i, _)| i)
                            .unwrap_or(field.len())
                    };
                    s.focused_value_mut().insert(byte_idx, ch);
                    s.cursor += 1;
                    s.genre_sel = 0;
                }
            }

            _ => {}
        }
    }

    /// Handle a key press inside the Customize (extra frames) sub-panel.
    ///
    /// When `extra_editing` is true, keystrokes go to the text buffer for the
    /// currently selected frame.  When false, Up/Down navigate frames and
    /// Enter starts editing the selected frame.
    pub(super) fn handle_id3_extra(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // Editing mode: keys modify the extra_input buffer.
        let editing = if let Mode::Id3Editor(ref s) = self.mode {
            s.extra_editing
        } else {
            return;
        };

        if editing {
            match code {
                // Esc: abandon the edit.
                KeyCode::Esc => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        s.extra_editing = false;
                        s.extra_input.clear();
                    }
                }
                // Enter / Ctrl+S: write the edited value back to the file.
                KeyCode::Enter | KeyCode::Char('s') | KeyCode::Char('S')
                    if code == KeyCode::Enter || modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    let (path, frame_id, value, idx) = if let Mode::Id3Editor(ref s) = self.mode {
                        let frame = s.extra_frames.get(s.extra_focused);
                        if let Some(f) = frame {
                            (
                                s.path.clone(),
                                f.id.clone(),
                                s.extra_input.clone(),
                                s.extra_focused,
                            )
                        } else {
                            return;
                        }
                    } else {
                        return;
                    };

                    match write_extra_frame(&path, &frame_id, &value) {
                        Ok(()) => {
                            if let Mode::Id3Editor(ref mut s) = self.mode {
                                // Update the in-memory list so the display refreshes.
                                if let Some(f) = s.extra_frames.get_mut(idx) {
                                    f.value = value;
                                }
                                s.extra_editing = false;
                                s.extra_input.clear();
                                s.status = Some("Frame saved".to_string());
                            }
                        }
                        Err(e) => {
                            if let Mode::Id3Editor(ref mut s) = self.mode {
                                s.status = Some(format!("Save error: {e}"));
                                s.extra_editing = false;
                            }
                        }
                    }
                }
                KeyCode::Left => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        s.extra_cursor = s.extra_cursor.saturating_sub(1);
                    }
                }
                KeyCode::Right => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        let len = s.extra_input.chars().count();
                        s.extra_cursor = (s.extra_cursor + 1).min(len);
                    }
                }
                KeyCode::Home => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        s.extra_cursor = 0;
                    }
                }
                KeyCode::End => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        s.extra_cursor = s.extra_input.chars().count();
                    }
                }
                KeyCode::Backspace => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        let c = s.extra_cursor;
                        if c > 0 {
                            if let Some((bi, _)) = s.extra_input.char_indices().nth(c - 1) {
                                s.extra_input.remove(bi);
                                s.extra_cursor -= 1;
                            }
                        }
                    }
                }
                KeyCode::Char(ch) => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        let c = s.extra_cursor;
                        let bi = s
                            .extra_input
                            .char_indices()
                            .nth(c)
                            .map(|(i, _)| i)
                            .unwrap_or(s.extra_input.len());
                        s.extra_input.insert(bi, ch);
                        s.extra_cursor += 1;
                    }
                }
                _ => {}
            }
            return;
        }

        // The add-tag picker, while it is open, owns every key.
        if matches!(&self.mode, Mode::Id3Editor(s) if s.adding) {
            match code {
                KeyCode::Esc => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        s.adding = false;
                    }
                }
                KeyCode::Up => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        s.add_focused = s.add_focused.saturating_sub(1);
                    }
                }
                KeyCode::Down => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        let max = s.add_choices.len().saturating_sub(1);
                        s.add_focused = (s.add_focused + 1).min(max);
                    }
                }
                // Enter: add the chosen frame with an empty value and start
                // typing it. Adding a tag and then having to find it in the
                // list to fill it in would be two steps for one intention.
                KeyCode::Enter => {
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        if let Some((id, label)) = s.add_choices.get(s.add_focused).cloned()
                        {
                            s.extra_frames.push(sparkamp::id3_editor::ExtraFrame {
                                id,
                                label,
                                value: String::new(),
                            });
                            s.extra_focused = s.extra_frames.len() - 1;
                            s.adding = false;
                            s.extra_editing = true;
                            s.extra_input = String::new();
                            s.extra_cursor = 0;
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        // Navigation mode.
        match code {
            // a: offer the frames this file can still hold. The list is built
            // now rather than at open, so it reflects anything added since.
            KeyCode::Char('a') | KeyCode::Char('A') => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.add_choices = sparkamp::id3_editor::addable_extra_frames(&s.path)
                        .into_iter()
                        .map(|(id, label)| (id.to_string(), label.to_string()))
                        .collect();
                    s.add_focused = 0;
                    s.adding = !s.add_choices.is_empty();
                    if !s.adding {
                        s.status = Some(
                            "This file already has every tag Sparkamp can add".to_string(),
                        );
                    }
                }
            }

            // Esc: close the Customize panel and return to the main fields.
            KeyCode::Esc => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.show_extra = false;
                }
            }

            KeyCode::Up => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.extra_focused = s.extra_focused.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    let max = s.extra_frames.len().saturating_sub(1);
                    s.extra_focused = (s.extra_focused + 1).min(max);
                }
            }

            // Enter: start editing the value of the focused extra frame.
            // Cursor starts at the end of the existing value.
            KeyCode::Enter => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    if !s.extra_frames.is_empty() {
                        let current_val = s.extra_frames[s.extra_focused].value.clone();
                        s.extra_cursor = current_val.chars().count();
                        s.extra_input = current_val;
                        s.extra_editing = true;
                    }
                }
            }

            // Ctrl+S from the Customize panel saves the main fields and closes.
            KeyCode::Char('s') | KeyCode::Char('S')
                if modifiers.contains(KeyModifiers::CONTROL) =>
            {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.show_extra = false; // return to main panel first
                }
                self.id3_save_and_close();
            }

            _ => {}
        }
    }

    /// Write the current `TagFields` back to disk, refresh the in-playlist
    /// track metadata, then close the editor.
    pub(super) fn id3_save_and_close(&mut self) {
        if let Mode::Id3Editor(ref s) = self.mode {
            if !s.taggable {
                self.mode = Mode::Normal;
                self.set_status("That file's format cannot store tags");
                return;
            }
        }
        let (path, fields, rg_gain, rg_seed) = if let Mode::Id3Editor(ref s) = self.mode {
            (
                s.path.clone(),
                s.fields.clone(),
                s.rg_gain.clone(),
                s.rg_seed.clone(),
            )
        } else {
            return;
        };

        // ReplayGain is not a TagFields frame — it goes through its own core
        // helper, which writes the file tag and the library row together.
        // Deliberately independent of the write-tags setting, which governs
        // only automatic analysis. Applied before write_tag_fields so a
        // rejected value stops the save with the editor still open.
        if rg_gain.trim() != rg_seed.trim() {
            match sparkamp::replaygain::apply_manual_gain_edit(
                self.media_lib.as_ref(),
                &path,
                &rg_gain,
            ) {
                Ok(_) => {}
                Err(e) => {
                    let msg = match e {
                        sparkamp::replaygain::ManualGainError::Unparseable => {
                            "ReplayGain must look like \"-6.20 dB\""
                        }
                        sparkamp::replaygain::ManualGainError::WriteFailed => {
                            "Could not write ReplayGain"
                        }
                    };
                    if let Mode::Id3Editor(ref mut s) = self.mode {
                        s.status = Some(msg.to_string());
                    }
                    return;
                }
            }
        }

        match write_tag_fields(&path, &fields) {
            Ok(()) => {
                // Refresh EVERY in-memory playlist row holding this file (it
                // can be listed more than once) with the full tag set, so the
                // playlist reflects the edit immediately.
                if let Ok(fresh) = Track::from_path(&path) {
                    for track in &mut self.playlist.tracks {
                        if track.path == path {
                            track.title = fresh.title.clone();
                            track.artist = fresh.artist.clone();
                            track.album_artist = fresh.album_artist.clone();
                            track.album = fresh.album.clone();
                        }
                    }
                }
                // And push the new tags into the media-library DB so its
                // views agree without waiting for the next folder rescan.
                if let Some(lib) = &self.media_lib {
                    let _ = lib.rescan_track(&path.display().to_string());
                }
                self.mode = Mode::Normal;
                self.set_status("Tags saved");
            }
            Err(e) => {
                if let Mode::Id3Editor(ref mut s) = self.mode {
                    s.status = Some(format!("Save error: {e}"));
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Settings overlay
    // -----------------------------------------------------------------------
}
