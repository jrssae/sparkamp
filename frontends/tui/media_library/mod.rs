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

    /// The media library's state, while it is the open view.
    fn ml_state(&mut self) -> Option<&mut MediaLibraryState> {
        match &mut self.mode {
            Mode::MediaLibrary(s) => Some(s),
            _ => None,
        }
    }

    /// Read-only [`ml_state`](Self::ml_state).
    fn ml_state_ref(&self) -> Option<&MediaLibraryState> {
        match &self.mode {
            Mode::MediaLibrary(s) => Some(s),
            _ => None,
        }
    }

    /// Handle key events while the full-screen media library view is open.
    ///
    /// In order of precedence:
    ///   Alt+z/x/c/v/b/j — transport and Jump, passed through to the player
    ///   a disc overlay  — rip, burn, gnudb matches, tag edit, submit: while
    ///                     one is open it takes every key
    ///   Esc in an album — back to the album list
    ///   a text input    — the add-path prompt, then the search box
    ///   any tab         — Esc closes, Tab cycles Files → Playlists → Discs →
    ///                     Albums, / or Ctrl+F searches, s cycles the sort,
    ///                     i opens Help
    ///   the tab's own   — see `ml_files_key` and its siblings
    pub(super) fn handle_media_library(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        if modifiers.contains(KeyModifiers::ALT) && self.ml_transport_key(code) {
            return;
        }
        let Some(s) = self.ml_state_ref() else {
            return;
        };
        let (search_active, add_active, tab, album_drilled) = (
            s.search_active,
            s.add_input.is_some(),
            s.tab.clone(),
            s.album_drill.is_some(),
        );
        if self.ml_overlay_key(code) {
            return;
        }

        // Esc inside an album pops back to the album list rather than closing
        // the media library — unless a text input is open, which Esc closes
        // first, as on every other tab.
        if tab == MediaLibraryTab::Albums
            && album_drilled
            && !add_active
            && !search_active
            && code == KeyCode::Esc
        {
            if let Some(s) = self.ml_state() {
                s.album_drill = None;
                s.album_tracks.clear();
                s.selected_album_track = 0;
            }
            return;
        }
        if add_active {
            self.ml_add_input_key(code);
            return;
        }
        if search_active {
            self.ml_search_input_key(code);
            return;
        }

        match code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Tab => self.ml_next_tab(),
            KeyCode::Char('/') => self.ml_open_search(),
            KeyCode::Char('f') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.ml_open_search()
            }
            KeyCode::Char('s') => self.ml_cycle_sort(),
            // i — the Help overlay, scrolled to the Media Library section.
            KeyCode::Char('i') | KeyCode::Char('I') => self.mode = Mode::Help { scroll: 34 },
            _ => match tab {
                MediaLibraryTab::Files => self.ml_files_key(code),
                MediaLibraryTab::Playlists => self.ml_playlists_key(code),
                MediaLibraryTab::Discs => self.ml_discs_key(code),
                MediaLibraryTab::Albums => self.ml_albums_key(code),
            },
        }
    }

    /// Alt+z/x/c/v/b drive the player and Alt+j opens Jump, as they do from
    /// the main view. Returns whether `code` was one of them.
    fn ml_transport_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('z') => self.play_prev(),
            KeyCode::Char('x') => {
                if *self.player.state() == sparkamp::engine::PlayerState::Stopped {
                    self.play_current();
                } else {
                    let _ = self.player.play();
                }
            }
            KeyCode::Char('c') => {
                let _ = self.player.toggle_pause();
            }
            KeyCode::Char('v') => {
                let _ = self.player.stop();
            }
            KeyCode::Char('b') => self.play_next(),
            KeyCode::Char('j') => {
                let results = (0..self.playlist.len()).collect();
                self.mode = Mode::Jump {
                    query: String::new(),
                    results,
                    selected: 0,
                    from_media_library: true,
                };
            }
            _ => return false,
        }
        true
    }

    /// The Discs tab's overlays take every key while one is open. Returns
    /// whether one was.
    fn ml_overlay_key(&mut self, code: KeyCode) -> bool {
        let Some(s) = self.ml_state_ref() else {
            return false;
        };
        if s.rip.is_some() {
            self.handle_rip_setup_key(code);
        } else if s.burn.is_some() {
            self.handle_burn_setup_key(code);
        } else if s.gnudb_matches.is_some() {
            self.handle_gnudb_matches_key(code);
        } else if s.tag_edit.is_some() {
            self.handle_disc_tag_edit_key(code);
        } else if s.submit_email.is_some() {
            self.handle_submit_email_key(code);
        } else if s.submit_category.is_some() {
            self.handle_submit_category_key(code);
        } else {
            return false;
        }
        true
    }

    /// The "add a path to the library" prompt (`a` on most tabs).
    fn ml_add_input_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                if let Some(s) = self.ml_state() {
                    s.add_input = None;
                }
            }
            KeyCode::Enter => {
                let input = self
                    .ml_state()
                    .and_then(|s| s.add_input.take())
                    .unwrap_or_default();
                self.commit_ml_add_path(input);
            }
            KeyCode::Backspace => {
                if let Some(buf) = self.ml_state().and_then(|s| s.add_input.as_mut()) {
                    buf.pop();
                }
            }
            KeyCode::Char(ch) => {
                if let Some(buf) = self.ml_state().and_then(|s| s.add_input.as_mut()) {
                    buf.push(ch);
                }
            }
            _ => {}
        }
    }

    /// The search box (`/` or Ctrl+F).
    fn ml_search_input_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                // Deactivate search, keep query so the user can see results.
                if let Some(s) = self.ml_state() {
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
                if let Some(s) = self.ml_state() {
                    s.search_query.pop();
                }
                self.note_ml_search_changed();
            }
            KeyCode::Char(ch) => {
                if let Some(s) = self.ml_state() {
                    s.search_query.push(ch);
                }
                self.note_ml_search_changed();
            }
            _ => {}
        }
    }

    fn ml_open_search(&mut self) {
        if let Some(s) = self.ml_state() {
            s.search_active = true;
        }
    }

    /// Tab: cycle Files → Playlists → Discs → Albums.
    fn ml_next_tab(&mut self) {
        let Some(s) = self.ml_state() else {
            return;
        };
        s.tab = match s.tab {
            MediaLibraryTab::Files => MediaLibraryTab::Playlists,
            MediaLibraryTab::Playlists => MediaLibraryTab::Discs,
            MediaLibraryTab::Discs => MediaLibraryTab::Albums,
            MediaLibraryTab::Albums => MediaLibraryTab::Files,
        };
        s.selected_track = 0;
        s.selected_playlist = 0;
        s.playlist_preview = None;
        let now_discs = s.tab == MediaLibraryTab::Discs;
        let need_detect = now_discs && s.drives.is_empty();
        let now_albums = s.tab == MediaLibraryTab::Albums;
        if now_albums {
            // Always start at the album list, never mid-drill, on
            // (re-)entry to the tab.
            s.selected_album = 0;
            s.album_drill = None;
            s.album_tracks.clear();
            s.selected_album_track = 0;
        }
        // First visit: detect drives (subprocess-backed, so only on entry /
        // explicit refresh, never per-frame).
        if need_detect {
            self.refresh_ml_drives();
        }
        // A lookup that finished while this tab wasn't showing parked its
        // matches — reopen the picker now.
        if now_discs {
            if let Some(list) = self.pending_disc_matches.take() {
                if let Some(s) = self.ml_state() {
                    s.gnudb_matches = Some((list, 0));
                }
            }
        }
        // Load the album list on entry — a single DB query, not re-run per
        // keystroke while browsing it.
        if now_albums {
            self.refresh_ml_albums();
        }
    }

    /// s — cycle the sort column; pressing s again on the same column
    /// reverses the direction.
    fn ml_cycle_sort(&mut self) {
        let Some(s) = self.ml_state_ref() else {
            return;
        };
        let (sort_col, sort_desc, cols) =
            (s.sort_col.clone(), s.sort_desc, s.visible_columns.clone());
        // Find the next column in the visible list after the current sort col.
        let (new_col, new_desc) = match cols.iter().position(|c| *c == sort_col) {
            None => (cols.first().cloned().unwrap_or(sort_col), false),
            // Move to the next column, ascending.
            Some(i) if i + 1 < cols.len() => (cols[i + 1].clone(), false),
            // Wrap: same column again — toggle direction.
            Some(_) => (cols[0].clone(), !sort_desc),
        };
        if let Some(s) = self.ml_state() {
            s.sort_col = new_col;
            s.sort_desc = new_desc;
        }
        self.refresh_ml_sort();
    }

    /// ← / → scroll the track columns sideways (every tab but Discs).
    fn ml_scroll_columns(&mut self, right: bool) {
        if let Some(s) = self.ml_state() {
            if right {
                if s.col_offset < s.visible_columns.len().saturating_sub(1) {
                    s.col_offset += 1;
                }
            } else {
                s.col_offset = s.col_offset.saturating_sub(1);
            }
        }
    }

    /// a — prompt for a path to add to the library (every tab but Discs).
    fn ml_prompt_add_path(&mut self) {
        if let Some(s) = self.ml_state() {
            s.add_input = Some(String::new());
        }
    }

    /// l — View/Search Lyrics for `track`.
    fn ml_open_lyrics_for(&mut self, track: Option<sparkamp::media_library::LibTrack>) {
        if let Some(t) = track {
            self.open_lyrics(
                std::path::PathBuf::from(&t.path),
                t.artist.unwrap_or_default(),
                t.title.unwrap_or_default(),
                t.album_artist.unwrap_or_default(),
            );
        }
    }

    /// Keys on the Files tab: the library's tracks.
    fn ml_files_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(s) = self.ml_state() {
                    s.selected_track = s.selected_track.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(s) = self.ml_state() {
                    if s.selected_track + 1 < s.tracks.len() {
                        s.selected_track += 1;
                    }
                }
            }
            // Enter — add the selected track to the current playlist.
            KeyCode::Enter => {
                let path = self
                    .ml_state_ref()
                    .and_then(|s| s.tracks.get(s.selected_track).map(|t| t.path.clone()));
                if let Some(path) = path {
                    self.add_ml_track_path_to_playlist(path);
                }
            }
            KeyCode::Left => self.ml_scroll_columns(false),
            KeyCode::Right => self.ml_scroll_columns(true),
            KeyCode::Char('l') => {
                let track = self
                    .ml_state_ref()
                    .and_then(|s| s.tracks.get(s.selected_track).cloned());
                self.ml_open_lyrics_for(track);
            }
            KeyCode::Char('a') | KeyCode::Char('A') => self.ml_prompt_add_path(),
            // b — queue the highlighted track on the Burn list.
            KeyCode::Char('b') => self.add_selected_ml_track_to_burn_list(),
            _ => {}
        }
    }

    /// Keys on the Playlists tab: the library's saved playlists.
    fn ml_playlists_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(s) = self.ml_state() {
                    s.selected_playlist = s.selected_playlist.saturating_sub(1);
                    s.playlist_preview = None; // refreshed on Enter
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(s) = self.ml_state() {
                    if s.selected_playlist + 1 < s.playlists.len() {
                        s.selected_playlist += 1;
                    }
                    s.playlist_preview = None;
                }
            }
            // Enter — load the preview tracks for the selected playlist.
            KeyCode::Enter => {
                let playlist = self
                    .ml_state_ref()
                    .and_then(|s| s.playlists.get(s.selected_playlist).cloned());
                if let Some(pl) = playlist {
                    let preview = self
                        .media_lib
                        .as_ref()
                        .and_then(|lib| lib.load_playlist_tracks(&pl).ok())
                        .unwrap_or_default();
                    if let Some(s) = self.ml_state() {
                        s.playlist_preview = Some(preview);
                    }
                }
            }
            KeyCode::Left => self.ml_scroll_columns(false),
            KeyCode::Right => self.ml_scroll_columns(true),
            KeyCode::Char('a') | KeyCode::Char('A') => self.ml_prompt_add_path(),
            _ => {}
        }
    }

    /// Keys on the Discs tab: optical drives and the loaded disc's tracks.
    fn ml_discs_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(s) = self.ml_state() {
                    s.selected_disc_track = s.selected_disc_track.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(s) = self.ml_state() {
                    if s.selected_disc_track + 1 < s.disc_entries.len() {
                        s.selected_disc_track += 1;
                    }
                }
            }
            // Enter — add the selected disc track to the current playlist.
            KeyCode::Enter => {
                let entry = self
                    .ml_state_ref()
                    .and_then(|s| s.disc_entries.get(s.selected_disc_track).cloned());
                if let Some(e) = entry {
                    self.add_disc_entries(&[e]);
                }
            }
            // ← / → — switch between drives (one row per physical drive).
            KeyCode::Left => {
                let switched = self.ml_state().is_some_and(|s| {
                    let prev = s.selected_drive;
                    s.selected_drive = s.selected_drive.saturating_sub(1);
                    s.selected_drive != prev
                });
                if switched {
                    self.reload_ml_disc_entries();
                }
            }
            KeyCode::Right => {
                let switched = self.ml_state().is_some_and(|s| {
                    let next = s.selected_drive + 1 < s.drives.len();
                    if next {
                        s.selected_drive += 1;
                    }
                    next
                });
                if switched {
                    self.reload_ml_disc_entries();
                }
            }
            // a — add the whole disc to the current playlist.
            KeyCode::Char('a') | KeyCode::Char('A') => {
                let entries = self
                    .ml_state_ref()
                    .map(|s| s.disc_entries.clone())
                    .unwrap_or_default();
                if !entries.is_empty() {
                    self.add_disc_entries(&entries);
                }
            }
            // r — re-detect drives and reload the track list (disc swapped,
            // drive plugged/unplugged).
            KeyCode::Char('r') | KeyCode::Char('R') => self.refresh_ml_drives(),
            // m — identify the loaded disc on gnudb (background).
            KeyCode::Char('m') | KeyCode::Char('M') => self.spawn_disc_lookup(),
            // e — the per-disc tag editor (with or without a gnudb match).
            KeyCode::Char('e') | KeyCode::Char('E') => self.open_disc_tag_editor(),
            // u — submit the disc's tags to gnudb (category picker first;
            // honors the test-mode config until verified end-to-end).
            KeyCode::Char('u') | KeyCode::Char('U') => self.open_submit_category_picker(),
            // g — rip setup ("grab").
            KeyCode::Char('g') | KeyCode::Char('G') => self.open_rip_setup(),
            // c — cancel a running rip after the current track, or a burn.
            KeyCode::Char('c') | KeyCode::Char('C')
                if self.rip_progress.is_some() || self.burn_phase.is_some() =>
            {
                if let Some(flag) = &self.rip_cancel {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    self.set_status("Stopping after the current track…");
                }
                if self.burn_phase.is_some() {
                    if let Some(flag) = &self.burn_prep_cancel {
                        flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    sparkamp::disc::burn::request_cancel();
                    self.set_status("Cancelling burn…");
                }
            }
            // b — the burn overlay.
            KeyCode::Char('b') | KeyCode::Char('B') => self.open_burn_setup(),
            _ => {}
        }
    }

    /// Keys on the Albums tab: the album list, or one album's tracks once
    /// drilled into with Enter.
    fn ml_albums_key(&mut self, code: KeyCode) {
        let drilled = self.ml_state_ref().is_some_and(|s| s.album_drill.is_some());
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(s) = self.ml_state() {
                    if drilled {
                        s.selected_album_track = s.selected_album_track.saturating_sub(1);
                    } else {
                        s.selected_album = s.selected_album.saturating_sub(1);
                    }
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(s) = self.ml_state() {
                    if drilled {
                        if s.selected_album_track + 1 < s.album_tracks.len() {
                            s.selected_album_track += 1;
                        }
                    } else if s.selected_album + 1 < s.albums.len() {
                        s.selected_album += 1;
                    }
                }
            }
            // Enter in an album adds the highlighted track to the current
            // playlist, as on the Files tab.
            KeyCode::Enter if drilled => {
                let path = self.ml_state_ref().and_then(|s| {
                    s.album_tracks
                        .get(s.selected_album_track)
                        .map(|t| t.path.clone())
                });
                if let Some(path) = path {
                    self.add_ml_track_path_to_playlist(path);
                }
            }
            // Enter on the album list drills into the selected album.
            KeyCode::Enter => {
                let group = self.ml_state_ref().and_then(|s| {
                    s.albums
                        .get(s.selected_album)
                        .map(|g| (g.album.clone(), g.album_artist.clone()))
                });
                if let Some((album, album_artist)) = group {
                    let artist_as_album = self.config.media_library.artist_as_album_artist;
                    let tracks = self
                        .media_lib
                        .as_ref()
                        .and_then(|lib| {
                            lib.album_tracks(&album, &album_artist, artist_as_album)
                                .ok()
                        })
                        .unwrap_or_default();
                    if let Some(s) = self.ml_state() {
                        s.album_tracks = tracks;
                        s.album_drill = Some((album, album_artist));
                        s.selected_album_track = 0;
                    }
                }
            }
            KeyCode::Left => self.ml_scroll_columns(false),
            KeyCode::Right => self.ml_scroll_columns(true),
            KeyCode::Char('l') if drilled => {
                let track = self
                    .ml_state_ref()
                    .and_then(|s| s.album_tracks.get(s.selected_album_track).cloned());
                self.ml_open_lyrics_for(track);
            }
            KeyCode::Char('a') | KeyCode::Char('A') => self.ml_prompt_add_path(),
            _ => {}
        }
    }

    /// Add a folder or file path to the media library (called from 'a' key in ML).
    /// If the folder is already watched, triggers a rescan instead.
    pub(super) fn commit_ml_add_path(&mut self, input: String) {
        use sparkamp::media_library::AddFolderResult;
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
        let rows = sparkamp::playlist_ingest::resolve(self.media_lib.as_ref(), &[path]);
        if rows.is_empty() {
            self.set_status("Cannot add track");
            return;
        }
        let was_empty = self.playlist.is_empty();
        if self.config.behavior.playlist_add_behavior
            == sparkamp::config::PlaylistAddBehavior::Replace
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
        let view = sparkamp::lyrics::lyrics_view(&path, &artist, &title, &album_artist);
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
        let albums: Vec<sparkamp::media_library::AlbumGroup> = self
            .media_lib
            .as_ref()
            .and_then(|lib| {
                lib.albums(sparkamp::media_library::AlbumSort::Artist, artist_as_album)
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
