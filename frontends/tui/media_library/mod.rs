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
        // Shared with the GTK frontend, which offers more columns than this one
        // renders. Drop the ones it cannot draw rather than showing a "?"
        // header over an empty cell; the config itself is left alone, so those
        // columns stay selected over there.
        let visible_columns = crate::tui::ui::media_library::known_columns(
            &self.config.media_library.visible_columns,
        );
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
            // Albums tab is loaded lazily on first entry, same as Discs.
            albums: Vec::new(),
            selected_album: 0,
            album_drill: None,
            album_tracks: Vec::new(),
            selected_album_track: 0,
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
        let (search_active, add_active, tab, album_drilled) = match &self.mode {
            Mode::MediaLibrary(s) => (
                s.search_active,
                s.add_input.is_some(),
                s.tab.clone(),
                s.album_drill.is_some(),
            ),
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

        // --- Albums tab drill-down: Esc pops back to the album list rather
        // than closing the media library. Must be intercepted here, before
        // the "Normal navigation" match's own `KeyCode::Esc` arm (which
        // closes the whole ML) — same local-precedence pattern as the
        // search-input and add-path blocks below. Must NOT fire while a
        // modal text input (add-path or search) is active — Esc should
        // close that input first, same precedence as every other tab. ---
        if tab == MediaLibraryTab::Albums
            && album_drilled
            && !add_active
            && !search_active
            && code == KeyCode::Esc
        {
            if let Mode::MediaLibrary(s) = &mut self.mode {
                s.album_drill = None;
                s.album_tracks.clear();
                s.selected_album_track = 0;
            }
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
                    // Leaving the input is the only way to reach the list, and
                    // Enter there acts on `s.tracks` by index. Run any pending
                    // search now rather than letting the user select a row from
                    // the previous query's results.
                    if self.ml_search_due.take().is_some() {
                        self.refresh_ml_search();
                    }
                }
                KeyCode::Backspace => {
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        s.search_query.pop();
                    }
                    self.note_ml_search_changed();
                }
                KeyCode::Char(ch) => {
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        s.search_query.push(ch);
                    }
                    self.note_ml_search_changed();
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

            // Tab: cycle Files → Playlists → Discs → Albums.
            KeyCode::Tab => {
                let (now_discs, need_detect, now_albums) =
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        s.tab = match s.tab {
                            MediaLibraryTab::Files => MediaLibraryTab::Playlists,
                            MediaLibraryTab::Playlists => MediaLibraryTab::Discs,
                            MediaLibraryTab::Discs => MediaLibraryTab::Albums,
                            MediaLibraryTab::Albums => MediaLibraryTab::Files,
                        };
                        s.selected_track = 0;
                        s.selected_playlist = 0;
                        s.playlist_preview = None;
                        let discs = s.tab == MediaLibraryTab::Discs;
                        let albums = s.tab == MediaLibraryTab::Albums;
                        if albums {
                            // Always start at the album list, never mid-drill,
                            // on (re-)entry to the tab.
                            s.selected_album = 0;
                            s.album_drill = None;
                            s.album_tracks.clear();
                            s.selected_album_track = 0;
                        }
                        (discs, discs && s.drives.is_empty(), albums)
                    } else {
                        (false, false, false)
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
                // Load the album list on entry — a single DB query, not
                // re-run per keystroke while browsing it.
                if now_albums {
                    self.refresh_ml_albums();
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

            // 'l' — View/Search Lyrics for the highlighted Files/Albums track.
            KeyCode::Char('l') => {
                let track = if let Mode::MediaLibrary(s) = &self.mode {
                    match s.tab {
                        MediaLibraryTab::Files => s.tracks.get(s.selected_track).map(|t| {
                            (
                                std::path::PathBuf::from(&t.path),
                                t.artist.clone().unwrap_or_default(),
                                t.title.clone().unwrap_or_default(),
                                t.album_artist.clone().unwrap_or_default(),
                            )
                        }),
                        MediaLibraryTab::Albums if album_drilled => {
                            s.album_tracks.get(s.selected_album_track).map(|t| {
                                (
                                    std::path::PathBuf::from(&t.path),
                                    t.artist.clone().unwrap_or_default(),
                                    t.title.clone().unwrap_or_default(),
                                    t.album_artist.clone().unwrap_or_default(),
                                )
                            })
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some((path, artist, title, album_artist)) = track {
                    self.open_lyrics(path, artist, title, album_artist);
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
                        MediaLibraryTab::Albums => {
                            if s.album_drill.is_some() {
                                s.selected_album_track =
                                    s.selected_album_track.saturating_sub(1);
                            } else {
                                s.selected_album = s.selected_album.saturating_sub(1);
                            }
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
                        MediaLibraryTab::Albums => {
                            if s.album_drill.is_some() {
                                if s.selected_album_track + 1 < s.album_tracks.len() {
                                    s.selected_album_track += 1;
                                }
                            } else if s.selected_album + 1 < s.albums.len() {
                                s.selected_album += 1;
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
                            self.add_ml_track_path_to_playlist(path_str);
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
                    MediaLibraryTab::Albums => {
                        let drilled = if let Mode::MediaLibrary(s) = &self.mode {
                            s.album_drill.is_some()
                        } else {
                            false
                        };
                        if drilled {
                            // Drilled into an album: add the highlighted
                            // track to the current playlist — same path as
                            // the Files tab's Enter.
                            let path = if let Mode::MediaLibrary(s) = &self.mode {
                                s.album_tracks
                                    .get(s.selected_album_track)
                                    .map(|t| t.path.clone())
                            } else {
                                None
                            };
                            if let Some(path_str) = path {
                                self.add_ml_track_path_to_playlist(path_str);
                            }
                        } else {
                            // Album list: drill into the selected group's
                            // track list.
                            let group = if let Mode::MediaLibrary(s) = &self.mode {
                                s.albums
                                    .get(s.selected_album)
                                    .map(|g| (g.album.clone(), g.album_artist.clone()))
                            } else {
                                None
                            };
                            if let Some((album, album_artist)) = group {
                                let artist_as_album =
                                    self.config.media_library.artist_as_album_artist;
                                let tracks = self
                                    .media_lib
                                    .as_ref()
                                    .and_then(|lib| {
                                        lib.album_tracks(&album, &album_artist, artist_as_album)
                                            .ok()
                                    })
                                    .unwrap_or_default();
                                if let Mode::MediaLibrary(s) = &mut self.mode {
                                    s.album_tracks = tracks;
                                    s.album_drill = Some((album, album_artist));
                                    s.selected_album_track = 0;
                                }
                            }
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
        let remove_missing = self.config.media_library.remove_missing_on_rescan;
        let result = if let Some(ref lib) = self.media_lib {
            match lib.add_folder(&path_str) {
                Ok(add_result) => {
                    let is_new = matches!(add_result, AddFolderResult::New(_));
                    let folder_id = add_result.id();
                    lib.rescan_folder(folder_id, &path_str, remove_missing)
                        .map(|r| (r, is_new))
                }
                Err(e) => Err(e),
            }
        } else {
            self.set_status("Media library not available");
            self.open_media_library();
            return;
        };
        let succeeded = result.is_ok();
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
        // A new (or newly re-added) folder changes the watch set — rebuild
        // so the live watcher picks it up immediately rather than waiting
        // for the next app restart. Only worth doing when the add actually
        // went through; an error here means nothing changed on disk.
        if succeeded {
            self.rebuild_watcher();
        }
        self.open_media_library();
    }

    /// Re-query the DB after a sort-column or sort-direction change.
    ///
    /// Identical work to [`Self::refresh_ml_search`]: both read the current
    /// query, sort column and direction, re-run the same query and replace the
    /// track list. Kept as a separate name because the call sites read better
    /// for it — the two bodies were byte-for-byte the same, which is a thing
    /// that only stays true by accident.
    pub(super) fn refresh_ml_sort(&mut self) {
        self.refresh_ml_search();
    }

    /// Record that the search query changed; `tick` runs the query once the
    /// deadline passes. Further typing pushes the deadline out rather than
    /// queueing a second search.
    ///
    /// 250 ms: long enough that a fast typist pays for one query rather than
    /// one per character, short enough that the list still feels attached to
    /// the input. See [`Self::refresh_ml_search`] for what is being deferred.
    pub(super) fn note_ml_search_changed(&mut self) {
        // Typing on the Albums tab leaves an open album. The query filters the
        // album list and means nothing inside a single album, so a drill-down
        // left standing would show one album's tracks under a box that is
        // filtering something else. GTK does the same on its Files search,
        // clearing `album_filter` as soon as a character lands.
        //
        // Popped here rather than behind the deadline below, for the reason
        // GTK gives for doing it outside its own debounce: it costs nothing
        // and should not wait on a timer.
        if let Mode::MediaLibrary(s) = &mut self.mode
            && s.tab == MediaLibraryTab::Albums
            && s.album_drill.is_some()
        {
            s.album_drill = None;
            s.album_tracks.clear();
            s.selected_album_track = 0;
        }
        self.ml_search_due =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(250));
    }

    /// Refresh the media library track list after the search query changes.
    ///
    /// Respects the current sort column and direction.
    ///
    /// Called from `tick` once typing settles, not straight from the key
    /// handler — see [`Self::note_ml_search_changed`].
    ///
    /// The Albums tab draws a list this query cannot produce — groups folded
    /// from the library, not rows returned by it — so it is handed off to
    /// [`Self::refresh_ml_albums`]. The hand-off lives here because this is
    /// the one choke point every route into a search runs through: the tick's
    /// deadline, a sort change, and a watch-folder event all arrive by it.
    pub(super) fn refresh_ml_search(&mut self) {
        if matches!(&self.mode, Mode::MediaLibrary(s) if s.tab == MediaLibraryTab::Albums) {
            self.refresh_ml_albums();
            return;
        }
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

    /// Add a media-library track (by path) to the current playlist. Shared
    /// by the Files tab's Enter and the Albums tab's drilled-down track
    /// list, so both honor the same replace/append and autoplay behavior.
    pub(super) fn add_ml_track_path_to_playlist(&mut self, path_str: String) {
        // The row came out of the library, so the library can describe it.
        // Reading its tags off disk again — which is what this used to do —
        // spends 27.974 ms for an answer already held.
        let path = std::path::PathBuf::from(&path_str);
        let rows = crate::playlist_ingest::resolve(self.media_lib.as_ref(), &[path]);
        if rows.is_empty() {
            self.set_status("Cannot add track");
            return;
        }
        let was_empty = self.playlist.is_empty();
        if self.config.behavior.playlist_add_behavior == crate::config::PlaylistAddBehavior::Replace
        {
            self.playlist.tracks.clear();
            self.playlist.current_index = 0;
            self.shuffle_state.reset();
        }
        self.add_resolved(rows);
        if self.config.behavior.autoplay_on_add && was_empty {
            self.play_current();
        }
        self.set_status("Track added to playlist");
    }

    /// View/Search Lyrics (F15) for one track. Saved USLT opens the read-only
    /// viewer overlay (preserving the current mode so Esc returns to the Media
    /// Library with its state intact); no lyrics best-effort launches a browser
    /// search, falling back to showing the URL in the status line when no
    /// `xdg-open` is available (terminals can't always open a browser).
    pub(super) fn open_lyrics(
        &mut self,
        path: std::path::PathBuf,
        artist: String,
        title: String,
        album_artist: String,
    ) {
        // The window/overlay ALWAYS opens now (F15 revision, point 2); a file
        // with no USLT shows "No lyrics available", and `d` searches the web.
        let view = crate::lyrics::lyrics_view(&path, &artist, &title, &album_artist);
        let lines: Vec<String> = match view.body {
            Some(text) => text.lines().map(|l| l.to_string()).collect(),
            None => vec!["No lyrics available".to_string()],
        };
        let prev = std::mem::replace(&mut self.mode, Mode::Normal);
        self.mode = Mode::Lyrics {
            title: view.title,
            lines,
            scroll: 0,
            search_url: view.search_url,
            return_mode: Box::new(prev),
        };
    }

    /// Load the Albums tab's grouped list, narrowed to the current search
    /// query.
    ///
    /// Called on Albums-tab entry (the Tab-cycle handler) and once typing
    /// settles — a lean, single query folded in Rust
    /// (`MediaLibrary::albums`), never re-run per keystroke, so a 36k-track
    /// library stays responsive. Default sort: Artist (no sort UI in the
    /// TUI — YAGNI until requested).
    ///
    /// The fold is what costs; filtering it afterwards is a walk over a few
    /// hundred structs, which is why the query is applied here rather than
    /// pushed into SQL. `AlbumGroup::matches` decides what stays, so the TUI
    /// and the GTK gallery agree on what a query means down to the no-album
    /// bucket.
    pub(super) fn refresh_ml_albums(&mut self) {
        let artist_as_album = self.config.media_library.artist_as_album_artist;
        let query = match &self.mode {
            Mode::MediaLibrary(s) => s.search_query.clone(),
            _ => String::new(),
        };
        let albums: Vec<crate::media_library::AlbumGroup> = self
            .media_lib
            .as_ref()
            .and_then(|lib| {
                lib.albums(crate::media_library::AlbumSort::Artist, artist_as_album)
                    .ok()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.matches(&query))
            .collect();
        if let Mode::MediaLibrary(s) = &mut self.mode {
            // Back to the top, as the Files list does on a new query: after a
            // filter the old index points at whichever album happens to have
            // taken that slot, which is not the one the user was looking at.
            s.selected_album = 0;
            s.albums = albums;
        }
    }
}
