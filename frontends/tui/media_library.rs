//! Media-library overlay: tab/key handling, search and sort refresh.

use crossterm::event::{KeyCode, KeyModifiers};

use super::{App, MediaLibraryState, MediaLibraryTab, Mode};

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
                let entered_discs = if let Mode::MediaLibrary(s) = &mut self.mode {
                    s.tab = match s.tab {
                        MediaLibraryTab::Files => MediaLibraryTab::Playlists,
                        MediaLibraryTab::Playlists => MediaLibraryTab::Discs,
                        MediaLibraryTab::Discs => MediaLibraryTab::Files,
                    };
                    s.selected_track = 0;
                    s.selected_playlist = 0;
                    s.playlist_preview = None;
                    s.tab == MediaLibraryTab::Discs && s.drives.is_empty()
                } else {
                    false
                };
                // First visit: detect drives (subprocess-backed, so only on
                // entry / explicit refresh, never per-frame).
                if entered_discs {
                    self.refresh_ml_drives();
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

            // i — open the Help overlay scrolled to the Media Library section.
            KeyCode::Char('i') | KeyCode::Char('I') => {
                self.mode = Mode::Help { scroll: 34 };
            }

            _ => {}
        }
    }

    /// Re-detect optical drives (subprocess-backed — only on Discs-tab entry
    /// or an explicit `r`), clamp the drive selection, and reload the track
    /// entries of the selected drive.
    pub(super) fn refresh_ml_drives(&mut self) {
        let drives = crate::disc::detect::list_drives();
        if let Mode::MediaLibrary(s) = &mut self.mode {
            s.selected_drive = s.selected_drive.min(drives.len().saturating_sub(1));
            s.drives = drives;
        }
        self.reload_ml_disc_entries();
        let n = if let Mode::MediaLibrary(s) = &self.mode {
            s.drives.len()
        } else {
            0
        };
        self.set_status(format!(
            "{n} optical drive{} found",
            if n == 1 { "" } else { "s" }
        ));
    }

    /// Rebuild `disc_entries` for the currently selected drive.
    pub(super) fn reload_ml_disc_entries(&mut self) {
        if let Mode::MediaLibrary(s) = &mut self.mode {
            s.disc_entries = s
                .drives
                .get(s.selected_drive)
                .map(crate::disc::toc::track_entries)
                .unwrap_or_default();
            s.selected_disc_track = 0;
        }
    }

    /// Append disc-track entries to the current playlist with their TOC
    /// titles and durations. No tag scan or duration probe: the titles come
    /// from the TOC ("Track N" until gnudb — Phase 2), the durations are
    /// exact, and Linux `cdda://` pseudo-paths aren't probeable files anyway.
    /// Honors the same add-behavior config as the Files tab.
    pub(super) fn add_disc_entries(&mut self, entries: &[crate::disc::DiscTrackEntry]) {
        if entries.is_empty() {
            return;
        }
        let was_empty = self.playlist.is_empty();
        if self.config.behavior.playlist_add_behavior == crate::config::PlaylistAddBehavior::Replace
        {
            self.playlist.tracks.clear();
            self.playlist.current_index = 0;
            self.shuffle_state.reset();
        }
        for e in entries {
            self.playlist.add(crate::model::Track {
                path: std::path::PathBuf::from(&e.path),
                title: e.title.clone(),
                artist: String::new(),
                album_artist: String::new(),
                album: String::new(),
                duration: Some(std::time::Duration::from_secs(e.duration_secs as u64)),
                broken: false,
                read_only: true, // disc media is never writable in place
            });
        }
        if self.config.behavior.autoplay_on_add && was_empty {
            self.play_current();
        }
        self.set_status(format!(
            "Added {} disc track{} to playlist",
            entries.len(),
            if entries.len() == 1 { "" } else { "s" }
        ));
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

    // -----------------------------------------------------------------------
    // ID3 editor key handler
    // -----------------------------------------------------------------------
}
