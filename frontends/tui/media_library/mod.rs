//! Media-library overlay: tab/key dispatch plus the Files-tab operations
//! (add path, search, sort). The Discs-tab behavior lives in the submodules:
//! [`detection`] (drives + entries), [`gnudb`] (identify/submit), [`tags`]
//! (edit/persist/propagate), [`rip`], and [`burn`].

use crossterm::event::{KeyCode, KeyModifiers};

use super::{App, MediaLibraryState, MediaLibraryTab, Mode};

mod burn;
mod detection;
mod gnudb;
mod rip;
mod tags;

impl App {

    /// Open the media library view, loading the track list from the DB.
    ///
    /// If the media library DB is not open (e.g. failed to initialise at
    /// startup), a status message is shown instead and the mode is unchanged.
    pub(super) fn open_media_library(&mut self) {
        let visible_columns = self.config.media_library.visible_columns.clone();
        // Default sort: artist ascending (first column alphabetically).
        let sort_col = "artist".to_string();
        let sort_desc = false;
        let tracks = if let Some(ref lib) = self.media_lib {
            lib.all_tracks_sorted(&sort_col, sort_desc)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let playlists = if let Some(ref lib) = self.media_lib {
            lib.all_playlists().unwrap_or_default()
        } else {
            Vec::new()
        };
        self.mode = Mode::MediaLibrary(MediaLibraryState {
            tab: MediaLibraryTab::Files,
            search_query: String::new(),
            search_active: false,
            tracks,
            playlists,
            selected_track: 0,
            selected_playlist: 0,
            playlist_preview: None,
            visible_columns,
            col_offset: 0,
            sort_col,
            sort_desc,
            add_input: None,
            // Drives are detected lazily on first entry to the Discs tab —
            // detection shells out (drutil / cd-info) and must not slow down
            // opening the library.
            drives: Vec::new(),
            selected_drive: 0,
            disc_entries: Vec::new(),
            selected_disc_track: 0,
            gnudb_matches: None,
            tag_edit: None,
            submit_category: None,
            submit_email: None,
            rip: None,
            burn: None,
        });
    }

    /// Handle key events while the full-screen media library view is open.
    ///
    /// Key map:
    ///   Esc            — close the media library and return to Normal
    ///   Tab            — switch between Files and Playlists tabs
    ///   / or Ctrl+F    — activate the search input
    ///   Esc (search)   — deactivate search input (clear query)
    ///   ↑ / k          — move selection up
    ///   ↓ / j          — move selection down
    ///   Enter (Files)  — add selected track to the current playlist
    ///   Alt+z/x/c/v/b  — pass transport commands through while in this mode
    pub(super) fn handle_media_library(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // --- Alt + transport bindings pass through to the player ---
        if modifiers.contains(KeyModifiers::ALT) {
            match code {
                KeyCode::Char('z') => {
                    self.play_prev();
                    return;
                }
                KeyCode::Char('x') => {
                    if *self.player.state() == crate::engine::PlayerState::Stopped {
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
                KeyCode::Char('j') => {
                    let results = (0..self.playlist.len()).collect();
                    self.mode = Mode::Jump {
                        query: String::new(),
                        results,
                        selected: 0,
                        from_media_library: true,
                    };
                    return;
                }
                _ => {}
            }
        }

        // Snapshot relevant state before borrowing mutably.
        let (search_active, add_active, tab) = match &self.mode {
            Mode::MediaLibrary(s) => (s.search_active, s.add_input.is_some(), s.tab.clone()),
            _ => return,
        };

        // Disc overlays capture all keys while open.
        let (matches_open, tag_edit_open, submit_open, email_open, rip_open, burn_open) =
            match &self.mode {
                Mode::MediaLibrary(s) => (
                    s.gnudb_matches.is_some(),
                    s.tag_edit.is_some(),
                    s.submit_category.is_some(),
                    s.submit_email.is_some(),
                    s.rip.is_some(),
                    s.burn.is_some(),
                ),
                _ => (false, false, false, false, false, false),
            };
        if rip_open {
            self.handle_rip_setup_key(code);
            return;
        }
        if burn_open {
            self.handle_burn_setup_key(code);
            return;
        }
        if matches_open {
            self.handle_gnudb_matches_key(code);
            return;
        }
        if tag_edit_open {
            self.handle_disc_tag_edit_key(code);
            return;
        }
        if email_open {
            self.handle_submit_email_key(code);
            return;
        }
        if submit_open {
            self.handle_submit_category_key(code);
            return;
        }

        // --- Add-to-ML path input mode ---
        if add_active {
            match code {
                KeyCode::Esc => {
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        s.add_input = None;
                    }
                }
                KeyCode::Enter => {
                    let input = if let Mode::MediaLibrary(s) = &self.mode {
                        s.add_input.clone().unwrap_or_default()
                    } else {
                        String::new()
                    };
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        s.add_input = None;
                    }
                    self.commit_ml_add_path(input);
                }
                KeyCode::Backspace => {
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        if let Some(ref mut buf) = s.add_input {
                            buf.pop();
                        }
                    }
                }
                KeyCode::Char(ch) => {
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        if let Some(ref mut buf) = s.add_input {
                            buf.push(ch);
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        // --- Search-input mode ---
        if search_active {
            match code {
                KeyCode::Esc => {
                    // Deactivate search, keep query so the user can see results.
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        s.search_active = false;
                    }
                }
                KeyCode::Backspace => {
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        s.search_query.pop();
                    }
                    self.refresh_ml_search();
                }
                KeyCode::Char(ch) => {
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        s.search_query.push(ch);
                    }
                    self.refresh_ml_search();
                }
                _ => {}
            }
            return;
        }

        // --- Normal navigation ---
        match code {
            // Close media library.
            KeyCode::Esc => {
                self.mode = Mode::Normal;
            }

            // Tab: cycle Files → Playlists → Discs.
            KeyCode::Tab => {
                let (now_discs, need_detect) = if let Mode::MediaLibrary(s) = &mut self.mode {
                    s.tab = match s.tab {
                        MediaLibraryTab::Files => MediaLibraryTab::Playlists,
                        MediaLibraryTab::Playlists => MediaLibraryTab::Discs,
                        MediaLibraryTab::Discs => MediaLibraryTab::Files,
                    };
                    s.selected_track = 0;
                    s.selected_playlist = 0;
                    s.playlist_preview = None;
                    let discs = s.tab == MediaLibraryTab::Discs;
                    (discs, discs && s.drives.is_empty())
                } else {
                    (false, false)
                };
                // First visit: detect drives (subprocess-backed, so only on
                // entry / explicit refresh, never per-frame).
                if need_detect {
                    self.refresh_ml_drives();
                }
                // A lookup that finished while this tab wasn't showing parked
                // its matches — reopen the picker now.
                if now_discs {
                    if let Some(list) = self.pending_disc_matches.take() {
                        if let Mode::MediaLibrary(s) = &mut self.mode {
                            s.gnudb_matches = Some((list, 0));
                        }
                    }
                }
            }

            // '/' or Ctrl+F — activate search.
            KeyCode::Char('/') | KeyCode::Char('f')
                if code == KeyCode::Char('/') || modifiers.contains(KeyModifiers::CONTROL) =>
            {
                if let Mode::MediaLibrary(s) = &mut self.mode {
                    s.search_active = true;
                }
            }

            // Navigation: up.
            KeyCode::Up | KeyCode::Char('k') => {
                if let Mode::MediaLibrary(s) = &mut self.mode {
                    match s.tab {
                        MediaLibraryTab::Files => {
                            s.selected_track = s.selected_track.saturating_sub(1);
                        }
                        MediaLibraryTab::Playlists => {
                            let prev = s.selected_playlist.saturating_sub(1);
                            s.selected_playlist = prev;
                            s.playlist_preview = None; // refreshed on Enter
                        }
                        MediaLibraryTab::Discs => {
                            s.selected_disc_track = s.selected_disc_track.saturating_sub(1);
                        }
                    }
                }
            }

            // Navigation: down.
            KeyCode::Down | KeyCode::Char('j') => {
                if let Mode::MediaLibrary(s) = &mut self.mode {
                    match s.tab {
                        MediaLibraryTab::Files => {
                            if s.selected_track + 1 < s.tracks.len() {
                                s.selected_track += 1;
                            }
                        }
                        MediaLibraryTab::Playlists => {
                            if s.selected_playlist + 1 < s.playlists.len() {
                                s.selected_playlist += 1;
                            }
                            s.playlist_preview = None;
                        }
                        MediaLibraryTab::Discs => {
                            if s.selected_disc_track + 1 < s.disc_entries.len() {
                                s.selected_disc_track += 1;
                            }
                        }
                    }
                }
            }

            // Enter: act on the selected item.
            KeyCode::Enter => {
                match tab {
                    MediaLibraryTab::Files => {
                        // Add the selected track to the current playlist.
                        let path = if let Mode::MediaLibrary(s) = &self.mode {
                            s.tracks.get(s.selected_track).map(|t| t.path.clone())
                        } else {
                            None
                        };
                        if let Some(path_str) = path {
                            let p = std::path::Path::new(&path_str);
                            match crate::model::Track::from_path(p) {
                                Ok(track) => {
                                    let was_empty = self.playlist.is_empty();
                                    if self.config.behavior.playlist_add_behavior
                                        == crate::config::PlaylistAddBehavior::Replace
                                    {
                                        self.playlist.tracks.clear();
                                        self.playlist.current_index = 0;
                                        self.shuffle_state.reset();
                                    }
                                    let before = self.playlist.tracks.len();
                                    self.playlist.add(track);
                                    self.probe_new_tracks(before);
                                    if self.config.behavior.autoplay_on_add && was_empty {
                                        self.play_current();
                                    }
                                    self.set_status("Track added to playlist");
                                }
                                Err(e) => {
                                    self.set_status(format!("Cannot add track: {e}"));
                                }
                            }
                        }
                    }
                    MediaLibraryTab::Playlists => {
                        // Load the preview tracks for the selected playlist.
                        let playlist_info = if let Mode::MediaLibrary(s) = &self.mode {
                            s.playlists.get(s.selected_playlist).cloned()
                        } else {
                            None
                        };
                        if let Some(pl) = playlist_info {
                            let preview = self
                                .media_lib
                                .as_ref()
                                .and_then(|lib| lib.load_playlist_tracks(&pl).ok())
                                .unwrap_or_default();
                            if let Mode::MediaLibrary(s) = &mut self.mode {
                                s.playlist_preview = Some(preview);
                            }
                        }
                    }
                    MediaLibraryTab::Discs => {
                        // Add the selected disc track to the current playlist.
                        let entry = if let Mode::MediaLibrary(s) = &self.mode {
                            s.disc_entries.get(s.selected_disc_track).cloned()
                        } else {
                            None
                        };
                        if let Some(e) = entry {
                            self.add_disc_entries(&[e]);
                        }
                    }
                }
            }

            // ← / → — scroll the Files columns; in the Discs tab they switch
            // between drives instead (one row per physical drive).
            KeyCode::Left => {
                let switch = if let Mode::MediaLibrary(s) = &mut self.mode {
                    if s.tab == MediaLibraryTab::Discs {
                        let prev = s.selected_drive;
                        s.selected_drive = s.selected_drive.saturating_sub(1);
                        s.selected_drive != prev
                    } else {
                        s.col_offset = s.col_offset.saturating_sub(1);
                        false
                    }
                } else {
                    false
                };
                if switch {
                    self.reload_ml_disc_entries();
                }
            }
            KeyCode::Right => {
                let switch = if let Mode::MediaLibrary(s) = &mut self.mode {
                    if s.tab == MediaLibraryTab::Discs {
                        if s.selected_drive + 1 < s.drives.len() {
                            s.selected_drive += 1;
                            true
                        } else {
                            false
                        }
                    } else {
                        let max = s.visible_columns.len().saturating_sub(1);
                        if s.col_offset < max {
                            s.col_offset += 1;
                        }
                        false
                    }
                } else {
                    false
                };
                if switch {
                    self.reload_ml_disc_entries();
                }
            }

            // s — cycle the sort column; pressing s again on the same column
            // reverses the direction.
            KeyCode::Char('s') => {
                let (sort_col, sort_desc, cols) = if let Mode::MediaLibrary(s) = &self.mode {
                    (s.sort_col.clone(), s.sort_desc, s.visible_columns.clone())
                } else {
                    return;
                };
                // Find the next column in the visible list after the current sort col.
                let pos = cols.iter().position(|c| *c == sort_col);
                let (new_col, new_desc) = match pos {
                    None => (cols.first().cloned().unwrap_or(sort_col), false),
                    Some(i) => {
                        let next = i + 1;
                        if next < cols.len() {
                            // Move to the next column, ascending.
                            (cols[next].clone(), false)
                        } else {
                            // Wrap: same column again — toggle direction.
                            (cols[0].clone(), !sort_desc)
                        }
                    }
                };
                if let Mode::MediaLibrary(s) = &mut self.mode {
                    s.sort_col = new_col.clone();
                    s.sort_desc = new_desc;
                }
                self.refresh_ml_sort();
            }

            // a — Files/Playlists: prompt for a path to add to the library.
            //     Discs: add the whole disc to the current playlist.
            KeyCode::Char('a') | KeyCode::Char('A') => {
                if tab == MediaLibraryTab::Discs {
                    let entries = if let Mode::MediaLibrary(s) = &self.mode {
                        s.disc_entries.clone()
                    } else {
                        Vec::new()
                    };
                    if !entries.is_empty() {
                        self.add_disc_entries(&entries);
                    }
                } else if let Mode::MediaLibrary(s) = &mut self.mode {
                    s.add_input = Some(String::new());
                }
            }

            // r — Discs tab: re-detect drives and reload the track list
            // (disc swapped, drive plugged/unplugged).
            KeyCode::Char('r') | KeyCode::Char('R') if tab == MediaLibraryTab::Discs => {
                self.refresh_ml_drives();
            }

            // m — Discs tab: identify the loaded disc on gnudb (background).
            KeyCode::Char('m') | KeyCode::Char('M') if tab == MediaLibraryTab::Discs => {
                self.spawn_disc_lookup();
            }

            // e — Discs tab: open the per-disc tag editor (works with or
            // without a gnudb match).
            KeyCode::Char('e') | KeyCode::Char('E') if tab == MediaLibraryTab::Discs => {
                self.open_disc_tag_editor();
            }

            // u — Discs tab: submit the disc's tags to gnudb (category picker
            // first; honors the test-mode config until verified end-to-end).
            KeyCode::Char('u') | KeyCode::Char('U') if tab == MediaLibraryTab::Discs => {
                self.open_submit_category_picker();
            }

            // g — Discs tab: rip setup ("grab"); c cancels a running rip
            // after the current track (or a running burn).
            KeyCode::Char('g') | KeyCode::Char('G') if tab == MediaLibraryTab::Discs => {
                self.open_rip_setup();
            }
            KeyCode::Char('c') | KeyCode::Char('C')
                if tab == MediaLibraryTab::Discs
                    && (self.rip_progress.is_some() || self.burn_phase.is_some()) =>
            {
                if let Some(flag) = &self.rip_cancel {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    self.set_status("Stopping after the current track…");
                }
                if self.burn_phase.is_some() {
                    if let Some(flag) = &self.burn_prep_cancel {
                        flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    crate::disc::burn::request_cancel();
                    self.set_status("Cancelling burn…");
                }
            }

            // b — Files tab: queue the highlighted track on the Burn list.
            //     Discs tab: open the burn overlay.
            KeyCode::Char('b') if tab == MediaLibraryTab::Files => {
                self.add_selected_ml_track_to_burn_list();
            }
            KeyCode::Char('b') | KeyCode::Char('B') if tab == MediaLibraryTab::Discs => {
                self.open_burn_setup();
            }

            // i — open the Help overlay scrolled to the Media Library section.
            KeyCode::Char('i') | KeyCode::Char('I') => {
                self.mode = Mode::Help { scroll: 34 };
            }

            _ => {}
        }
    }

    /// Add a folder or file path to the media library (called from 'a' key in ML).
    /// If the folder is already watched, triggers a rescan instead.
    pub(super) fn commit_ml_add_path(&mut self, input: String) {
        use crate::media_library::AddFolderResult;
        let path_str = input.trim().to_string();
        if path_str.is_empty() {
            return;
        }
        let path = std::path::Path::new(&path_str);
        if !path.exists() {
            self.set_status(format!("Path not found: {path_str}"));
            self.open_media_library();
            return;
        }
        let result = if let Some(ref lib) = self.media_lib {
            match lib.add_folder(&path_str) {
                Ok(add_result) => {
                    let is_new = matches!(add_result, AddFolderResult::New(_));
                    let folder_id = add_result.id();
                    lib.rescan_folder(folder_id, &path_str).map(|r| (r, is_new))
                }
                Err(e) => Err(e),
            }
        } else {
            self.set_status("Media library not available");
            self.open_media_library();
            return;
        };
        match result {
            Ok(((added, _removed), is_new)) => {
                if is_new {
                    self.set_status(format!("Added {added} track(s) to media library"));
                } else {
                    self.set_status(format!("Rescanned — {added} track(s) in library"));
                }
            }
            Err(e) => {
                self.set_status(format!("Error adding to ML: {e}"));
            }
        }
        self.open_media_library();
    }

    /// Re-query the DB after a sort-column or sort-direction change.
    pub(super) fn refresh_ml_sort(&mut self) {
        let (query, sort_col, sort_desc) = if let Mode::MediaLibrary(s) = &self.mode {
            (s.search_query.clone(), s.sort_col.clone(), s.sort_desc)
        } else {
            return;
        };
        let tracks = if let Some(ref lib) = self.media_lib {
            if query.is_empty() {
                lib.all_tracks_sorted(&sort_col, sort_desc)
                    .unwrap_or_default()
            } else {
                lib.search_tracks_sorted(&query, &sort_col, sort_desc)
                    .unwrap_or_default()
            }
        } else {
            Vec::new()
        };
        if let Mode::MediaLibrary(s) = &mut self.mode {
            s.tracks = tracks;
            s.selected_track = 0;
        }
    }

    /// Refresh the media library track list after the search query changes.
    ///
    /// Respects the current sort column and direction.
    pub(super) fn refresh_ml_search(&mut self) {
        let (query, sort_col, sort_desc) = if let Mode::MediaLibrary(s) = &self.mode {
            (s.search_query.clone(), s.sort_col.clone(), s.sort_desc)
        } else {
            return;
        };

        let tracks = if let Some(ref lib) = self.media_lib {
            if query.is_empty() {
                lib.all_tracks_sorted(&sort_col, sort_desc)
                    .unwrap_or_default()
            } else {
                lib.search_tracks_sorted(&query, &sort_col, sort_desc)
                    .unwrap_or_default()
            }
        } else {
            Vec::new()
        };

        if let Mode::MediaLibrary(s) = &mut self.mode {
            s.tracks = tracks;
            s.selected_track = 0;
        }
    }
}
