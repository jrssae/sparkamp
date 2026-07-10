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

    // -----------------------------------------------------------------------
    // Discs tab: gnudb identification + tag override
    // -----------------------------------------------------------------------

    /// The selected drive's TOC and freedb id, when an audio disc is loaded.
    fn selected_disc_identity(&self) -> Option<(crate::disc::DiscToc, String)> {
        let Mode::MediaLibrary(s) = &self.mode else {
            return None;
        };
        let toc = s.drives.get(s.selected_drive)?.toc.clone()?;
        let id = crate::disc::discid::freedb_discid(&toc);
        Some((toc, id))
    }

    /// Kick off a background gnudb query for the selected drive's disc.
    /// Results arrive through `disc_lookup` in the tick loop, so the UI never
    /// blocks on the network (10 s timeout inside the client).
    pub(super) fn spawn_disc_lookup(&mut self) {
        if self.disc_lookup.is_some() {
            self.set_status("gnudb lookup already running…");
            return;
        }
        let Some((toc, discid)) = self.selected_disc_identity() else {
            self.set_status("No audio disc to identify");
            return;
        };
        let email = self.config.disc.gnudb_email.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.disc_lookup = Some(rx);
        self.set_status("Asking gnudb…");
        std::thread::spawn(move || {
            use crate::disc::{gnudb, xmcd};
            let msg = match gnudb::query(&toc, &email) {
                Err(e) => super::DiscLookupMsg::Failed(e.to_string()),
                Ok(matches) if matches.is_empty() => super::DiscLookupMsg::Failed(
                    "No gnudb match — press e to fill tags in manually".to_string(),
                ),
                Ok(matches) if matches.len() == 1 && matches[0].exact => {
                    match gnudb::read(&matches[0].category, &matches[0].discid, &email) {
                        Ok(text) => match xmcd::parse(&text) {
                            Some(entry) => super::DiscLookupMsg::Entry(discid, entry),
                            None => super::DiscLookupMsg::Failed(
                                "gnudb entry was unreadable".to_string(),
                            ),
                        },
                        Err(e) => super::DiscLookupMsg::Failed(e.to_string()),
                    }
                }
                Ok(matches) => super::DiscLookupMsg::Matches(matches),
            };
            // Receiver dropped = user closed the library; nothing to do.
            let _ = tx.send(msg);
        });
    }

    /// Fetch one picked match in the background (same channel as the query).
    fn spawn_disc_read(&mut self, category: String, matched_id: String) {
        let Some((_, discid)) = self.selected_disc_identity() else {
            return;
        };
        let email = self.config.disc.gnudb_email.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.disc_lookup = Some(rx);
        self.set_status("Fetching entry…");
        std::thread::spawn(move || {
            use crate::disc::{gnudb, xmcd};
            let msg = match gnudb::read(&category, &matched_id, &email) {
                Ok(text) => match xmcd::parse(&text) {
                    Some(entry) => super::DiscLookupMsg::Entry(discid, entry),
                    None => {
                        super::DiscLookupMsg::Failed("gnudb entry was unreadable".to_string())
                    }
                },
                Err(e) => super::DiscLookupMsg::Failed(e.to_string()),
            };
            let _ = tx.send(msg);
        });
    }

    /// Apply a background lookup result (called from the tick loop).
    pub(super) fn handle_disc_lookup(&mut self, msg: super::DiscLookupMsg) {
        match msg {
            super::DiscLookupMsg::Failed(text) => {
                self.disc_lookup = None;
                self.set_status(text);
            }
            super::DiscLookupMsg::Matches(list) => {
                self.disc_lookup = None;
                let showing_discs = matches!(
                    &self.mode,
                    Mode::MediaLibrary(s) if s.tab == MediaLibraryTab::Discs
                );
                if showing_discs {
                    if let Mode::MediaLibrary(s) = &mut self.mode {
                        s.gnudb_matches = Some((list, 0));
                    }
                } else {
                    // The user left the Discs tab (or the library) while the
                    // lookup ran — never drop the result. Park it and say so;
                    // the picker reopens on the next Discs-tab visit.
                    let n = list.len();
                    self.pending_disc_matches = Some(list);
                    self.set_status(format!(
                        "gnudb: {n} candidate{} found — open the Discs tab to choose",
                        if n == 1 { "" } else { "s" }
                    ));
                }
            }
            super::DiscLookupMsg::Entry(discid, entry) => {
                self.disc_lookup = None;
                let label = format!("{} — {}", entry.artist, entry.album);
                // Keep the untouched match as the submission baseline.
                self.disc_official.insert(discid.clone(), entry.clone());
                self.disc_tags.insert(discid.clone(), entry);
                self.persist_disc_tags(&discid);
                self.apply_disc_tags_to_entries();
                self.propagate_disc_tags_to_playlist();
                self.set_status(label);
            }
            super::DiscLookupMsg::Submitted(msg) => {
                self.disc_lookup = None;
                self.set_status(format!("gnudb: {msg}"));
            }
        }
    }

    /// Open the submission category picker, preselecting the best-effort
    /// genre→category suggestion. Requires an edited/matched tag set, and —
    /// per the gnudb howto — the user's own email, captured here the first
    /// time (the config ships blank on purpose).
    fn open_submit_category_picker(&mut self) {
        let Some((_, discid)) = self.selected_disc_identity() else {
            self.set_status("No audio disc loaded");
            return;
        };
        let Some(entry) = self.disc_tags.get(&discid) else {
            self.set_status("No tags yet — press m to identify or e to edit first");
            return;
        };
        if crate::disc::gnudb::is_unset_email(&self.config.disc.gnudb_email)
            || !crate::disc::gnudb::is_valid_email(&self.config.disc.gnudb_email)
        {
            if let Mode::MediaLibrary(s) = &mut self.mode {
                s.submit_email = Some(String::new());
            }
            return;
        }
        let suggested = crate::disc::gnudb::suggest_category(&entry.genre);
        let idx = crate::disc::gnudb::CATEGORIES
            .iter()
            .position(|c| *c == suggested)
            .unwrap_or(0);
        if let Mode::MediaLibrary(s) = &mut self.mode {
            s.submit_category = Some(idx);
        }
    }

    /// Keys in the first-submission email prompt: type/Backspace edit,
    /// Enter saves (rough shape check) and continues to the category picker,
    /// Esc cancels the submission.
    fn handle_submit_email_key(&mut self, code: KeyCode) {
        let mut saved: Option<String> = None;
        if let Mode::MediaLibrary(s) = &mut self.mode {
            let Some(buf) = &mut s.submit_email else { return };
            match code {
                KeyCode::Esc => s.submit_email = None,
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Enter => {
                    let e = buf.trim().to_string();
                    // Shared shape rule: x@y.z (see gnudb::is_valid_email).
                    if crate::disc::gnudb::is_valid_email(&e) {
                        saved = Some(e);
                        s.submit_email = None;
                    }
                }
                KeyCode::Char(ch) => buf.push(ch),
                _ => {}
            }
        }
        if let Some(email) = saved {
            self.config.disc.gnudb_email = email;
            // Straight on to the category picker now that we're submittable.
            self.open_submit_category_picker();
        }
    }

    /// Keys in the category picker: ↑/↓ select, Enter submit, Esc cancel.
    fn handle_submit_category_key(&mut self, code: KeyCode) {
        let mut submit_with: Option<&'static str> = None;
        if let Mode::MediaLibrary(s) = &mut self.mode {
            let Some(selected) = &mut s.submit_category else {
                return;
            };
            match code {
                KeyCode::Esc => s.submit_category = None,
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    if *selected + 1 < crate::disc::gnudb::CATEGORIES.len() {
                        *selected += 1;
                    }
                }
                KeyCode::Enter => {
                    submit_with = Some(crate::disc::gnudb::CATEGORIES[*selected]);
                    s.submit_category = None;
                }
                _ => {}
            }
        }
        if let Some(category) = submit_with {
            self.spawn_disc_submit(category);
        }
    }

    /// Validate and POST the disc's tags to gnudb on a background thread.
    /// The revision comes from the official match (old + 1) or 0 for a disc
    /// gnudb doesn't know yet.
    fn spawn_disc_submit(&mut self, category: &'static str) {
        if self.disc_lookup.is_some() {
            self.set_status("gnudb request already running…");
            return;
        }
        let Some((toc, discid)) = self.selected_disc_identity() else {
            return;
        };
        let Some(mut entry) = self.disc_tags.get(&discid).cloned() else {
            return;
        };
        entry.revision = self
            .disc_official
            .get(&discid)
            .map(|o| o.revision + 1)
            .unwrap_or(0);
        // Fast local validation for immediate feedback (the server would
        // reject these anyway, after a round-trip).
        if let Err(reason) = crate::disc::xmcd::validate_for_submit(&entry, &toc) {
            self.set_status(reason);
            return;
        }
        let email = self.config.disc.gnudb_email.clone();
        let test_mode = self.config.disc.gnudb_submit_mode_test;
        let (tx, rx) = std::sync::mpsc::channel();
        self.disc_lookup = Some(rx);
        self.set_status(if test_mode {
            "Submitting to gnudb (test mode)…"
        } else {
            "Submitting to gnudb…"
        });
        std::thread::spawn(move || {
            use crate::disc::{discid as discid_mod, gnudb, xmcd};
            let body = xmcd::build(&entry, &toc, entry.revision);
            let id = discid_mod::freedb_discid(&toc);
            let msg = match gnudb::submit(&body, category, &id, &email, test_mode) {
                Ok(server_msg) => super::DiscLookupMsg::Submitted(if test_mode {
                    format!("{server_msg} (test mode — not published)")
                } else {
                    server_msg
                }),
                Err(e) => super::DiscLookupMsg::Failed(e.to_string()),
            };
            let _ = tx.send(msg);
        });
    }

    /// Overlay the stored tag set's titles onto the visible disc entries
    /// ("Track N" stays wherever a title is missing).
    pub(super) fn apply_disc_tags_to_entries(&mut self) {
        let Some((_, discid)) = self.selected_disc_identity() else {
            return;
        };
        let Some(entry) = self.disc_tags.get(&discid) else {
            return;
        };
        let titles = entry.track_titles.clone();
        if let Mode::MediaLibrary(s) = &mut self.mode {
            for e in &mut s.disc_entries {
                let i = e.number as usize - 1;
                if let Some(t) = titles.get(i) {
                    if !t.is_empty() {
                        e.title = t.clone();
                    }
                }
            }
        }
    }

    /// Keys while the gnudb match overlay is open: ↑/↓ select, Enter fetch,
    /// Esc dismiss.
    fn handle_gnudb_matches_key(&mut self, code: KeyCode) {
        let mut chosen: Option<(String, String)> = None;
        if let Mode::MediaLibrary(s) = &mut self.mode {
            let Some((list, selected)) = &mut s.gnudb_matches else {
                return;
            };
            match code {
                KeyCode::Esc => s.gnudb_matches = None,
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    if *selected + 1 < list.len() {
                        *selected += 1;
                    }
                }
                KeyCode::Enter => {
                    if let Some(m) = list.get(*selected) {
                        chosen = Some((m.category.clone(), m.discid.clone()));
                    }
                    s.gnudb_matches = None;
                }
                _ => {}
            }
        }
        if let Some((category, discid)) = chosen {
            self.spawn_disc_read(category, discid);
        }
    }

    /// Open the tag editor prefilled from the stored tag set (or the visible
    /// "Track N" titles when the disc has no tags yet).
    fn open_disc_tag_editor(&mut self) {
        let Some((_, discid)) = self.selected_disc_identity() else {
            self.set_status("No audio disc loaded");
            return;
        };
        let stored = self.disc_tags.get(&discid).cloned();
        if let Mode::MediaLibrary(s) = &mut self.mode {
            let count = s.disc_entries.len();
            let mut titles: Vec<String> = (0..count)
                .map(|i| {
                    stored
                        .as_ref()
                        .and_then(|e| e.track_titles.get(i).cloned())
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| {
                            s.disc_entries
                                .get(i)
                                .map(|e| e.title.clone())
                                .unwrap_or_default()
                        })
                })
                .collect();
            if titles.is_empty() {
                titles = vec![String::new()];
            }
            s.tag_edit = Some(super::DiscTagEditState {
                discid,
                artist: stored.as_ref().map(|e| e.artist.clone()).unwrap_or_default(),
                album: stored.as_ref().map(|e| e.album.clone()).unwrap_or_default(),
                year: stored.as_ref().map(|e| e.year.clone()).unwrap_or_default(),
                genre: stored.as_ref().map(|e| e.genre.clone()).unwrap_or_default(),
                titles,
                selected: 0,
                editing: false,
            });
        }
    }

    /// Keys while the tag editor overlay is open. Not editing: ↑/↓ move,
    /// Enter edits the row, Esc saves + closes. Editing: chars/Backspace
    /// change the value, Enter/Esc stop editing.
    fn handle_disc_tag_edit_key(&mut self, code: KeyCode) {
        let mut save: Option<super::DiscTagEditState> = None;
        if let Mode::MediaLibrary(s) = &mut self.mode {
            let Some(ed) = &mut s.tag_edit else { return };
            let rows = 4 + ed.titles.len();
            if ed.editing {
                let value: &mut String = match ed.selected {
                    0 => &mut ed.artist,
                    1 => &mut ed.album,
                    2 => &mut ed.year,
                    3 => &mut ed.genre,
                    i => &mut ed.titles[i - 4],
                };
                match code {
                    KeyCode::Enter | KeyCode::Esc => ed.editing = false,
                    KeyCode::Backspace => {
                        value.pop();
                    }
                    KeyCode::Char(ch) => value.push(ch),
                    _ => {}
                }
            } else {
                match code {
                    KeyCode::Esc => {
                        // Save-and-close.
                        save = s.tag_edit.take();
                    }
                    KeyCode::Up | KeyCode::Char('k') => ed.selected = ed.selected.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        if ed.selected + 1 < rows {
                            ed.selected += 1;
                        }
                    }
                    KeyCode::Enter => ed.editing = true,
                    _ => {}
                }
            }
        }
        if let Some(ed) = save {
            // Keep the matched entry's revision (an update must submit old+1;
            // the submit path derives that from the official copy).
            let revision = self
                .disc_tags
                .get(&ed.discid)
                .map(|e| e.revision)
                .unwrap_or(0);
            let entry = crate::disc::xmcd::XmcdEntry {
                discid: ed.discid.clone(),
                artist: ed.artist,
                album: ed.album,
                year: ed.year,
                genre: ed.genre,
                track_titles: ed.titles,
                extd: String::new(),
                extt: Vec::new(),
                revision,
            };
            let discid = ed.discid;
            self.disc_tags.insert(discid.clone(), entry);
            self.persist_disc_tags(&discid);
            self.apply_disc_tags_to_entries();
            self.propagate_disc_tags_to_playlist();
            self.set_status("Disc tags saved");
        }
    }

    /// Write one disc's current tags (+ official baseline) to the on-disk
    /// cache so they survive restarts.
    fn persist_disc_tags(&self, discid: &str) {
        let Some(user) = self.disc_tags.get(discid) else {
            return;
        };
        let mut store = crate::disc::tagstore::DiscTagStore::load();
        store.set(
            discid,
            user.clone(),
            self.disc_official.get(discid).cloned(),
        );
    }

    /// Push the current tag set into every already-added active-playlist row
    /// for the selected disc — tag edits must show up there immediately, not
    /// only on future adds. Disc entries and playlist rows share exact path
    /// strings, so matching is by path.
    fn propagate_disc_tags_to_playlist(&mut self) {
        let Some((_, discid)) = self.selected_disc_identity() else {
            return;
        };
        let (disc_artist, disc_album) = self
            .disc_tags
            .get(&discid)
            .map(|t| (t.artist.clone(), t.album.clone()))
            .unwrap_or_default();
        // (path, title, artist) per entry, with the sampler "Artist / Title"
        // split — same rules as add_disc_entries.
        let updates: Vec<(String, String, String)> =
            if let Mode::MediaLibrary(s) = &self.mode {
                s.disc_entries
                    .iter()
                    .map(|e| {
                        let meta = crate::disc::track_meta(&e.title, &disc_artist);
                        (e.path.clone(), meta.title, meta.artist)
                    })
                    .collect()
            } else {
                return;
            };
        for track in &mut self.playlist.tracks {
            let track_path = track.path.display().to_string();
            if let Some((_, title, artist)) =
                updates.iter().find(|(p, _, _)| *p == track_path)
            {
                track.title = title.clone();
                track.artist = artist.clone();
                track.album = disc_album.clone();
            }
        }
    }

    // -----------------------------------------------------------------------
    // Discs tab: rip to MP3
    // -----------------------------------------------------------------------

    /// Open the rip-setup overlay: all tracks preselected, destination from
    /// config → first watched folder → ~/Music, quality from config.
    fn open_rip_setup(&mut self) {
        if self.rip_progress.is_some() {
            self.set_status("A rip is already running (c cancels it)");
            return;
        }
        let dest = crate::disc::rip::default_dest(
            self.config.disc.rip_dest_dir.as_deref(),
            self.watched_folders().first().map(String::as_str),
        );
        let quality = self.config.disc.rip_mp3_quality;
        let dest_watched = crate::disc::rip::dest_is_watched(&dest, &self.watched_folders());
        if let Mode::MediaLibrary(s) = &mut self.mode {
            if s.disc_entries.is_empty() {
                self.set_status("No audio disc loaded");
                return;
            }
            s.rip = Some(super::RipSetupState {
                selected: vec![true; s.disc_entries.len()],
                cursor: 0,
                dest,
                editing_dest: false,
                dest_watched,
                quality,
            });
        }
    }

    /// The watched library folder paths (empty when no library is open).
    fn watched_folders(&self) -> Vec<String> {
        self.media_lib
            .as_ref()
            .and_then(|l| l.list_folders().ok())
            .map(|folders| folders.into_iter().map(|(_, p)| p).collect())
            .unwrap_or_default()
    }

    /// Keys in the rip overlay. Browsing: ↑/↓ move, Space toggle, a all/none,
    /// q cycle quality, d edit destination, Enter start, Esc cancel.
    /// Editing the destination: chars/Backspace, Enter/Esc done.
    fn handle_rip_setup_key(&mut self, code: KeyCode) {
        let mut start = false;
        let mut close = false;
        // Set while editing the destination: the unwatched-folder warning is
        // re-checked after the mutable borrow on the overlay state ends.
        let mut recheck_dest: Option<String> = None;

        if let Mode::MediaLibrary(s) = &mut self.mode {
            let Some(rip) = &mut s.rip else { return };
            if rip.editing_dest {
                match code {
                    KeyCode::Enter | KeyCode::Esc => rip.editing_dest = false,
                    KeyCode::Backspace => {
                        rip.dest.pop();
                    }
                    KeyCode::Char(ch) => rip.dest.push(ch),
                    _ => {}
                }
                recheck_dest = Some(rip.dest.clone());
            } else {
                match code {
                    KeyCode::Esc => close = true,
                    KeyCode::Up | KeyCode::Char('k') => {
                        rip.cursor = rip.cursor.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if rip.cursor + 1 < rip.selected.len() {
                            rip.cursor += 1;
                        }
                    }
                    KeyCode::Char(' ') => {
                        if let Some(v) = rip.selected.get_mut(rip.cursor) {
                            *v = !*v;
                        }
                    }
                    KeyCode::Char('a') | KeyCode::Char('A') => {
                        let all = rip.selected.iter().all(|v| *v);
                        rip.selected.iter_mut().for_each(|v| *v = !all);
                    }
                    KeyCode::Char('q') | KeyCode::Char('Q') => {
                        rip.quality = (rip.quality + 1) % 3;
                    }
                    KeyCode::Char('d') | KeyCode::Char('D') => rip.editing_dest = true,
                    KeyCode::Enter => start = true,
                    _ => {}
                }
            }
        }

        if close {
            if let Mode::MediaLibrary(s) = &mut self.mode {
                s.rip = None;
            }
            return;
        }
        if let Some(dest) = recheck_dest {
            let watched = crate::disc::rip::dest_is_watched(&dest, &self.watched_folders());
            if let Mode::MediaLibrary(s) = &mut self.mode {
                if let Some(rip) = &mut s.rip {
                    rip.dest_watched = watched;
                }
            }
            return;
        }
        if start {
            self.spawn_rip();
        }
    }

    /// Kick the selected tracks off on a rip thread; progress arrives via
    /// `disc_rip` in the tick loop and cancel stops after the current track.
    fn spawn_rip(&mut self) {
        let Some((_, discid)) = self.selected_disc_identity() else {
            return;
        };
        // An active cdda:// playback shares the drive head with the rip's
        // cdiocddasrc — the device allows one reader, so both would thrash.
        // Refuse instead of wedging (same contention rule as the disc poll).
        let playing_disc = *self.player.state() != crate::engine::PlayerState::Stopped
            && self
                .playlist
                .current()
                .map(|t| t.path.to_string_lossy().starts_with("cdda://"))
                .unwrap_or(false);
        if playing_disc {
            self.set_status("Stop disc playback before ripping (one reader per drive)");
            return;
        }
        let tags = self.disc_tags.get(&discid).cloned().unwrap_or_default();

        let (entries, dest, quality) = {
            let Mode::MediaLibrary(s) = &mut self.mode else {
                return;
            };
            let Some(rip) = s.rip.take() else { return };
            let entries: Vec<crate::disc::DiscTrackEntry> = s
                .disc_entries
                .iter()
                .zip(&rip.selected)
                .filter(|(_, sel)| **sel)
                .map(|(e, _)| e.clone())
                .collect();
            if entries.is_empty() || rip.dest.trim().is_empty() {
                self.set_status("Nothing selected to rip");
                return;
            }
            (entries, rip.dest.trim().to_string(), rip.quality)
        };

        // Remember the choices for next time (persisted on quit).
        self.config.disc.rip_dest_dir = Some(std::path::PathBuf::from(&dest));
        self.config.disc.rip_mp3_quality = quality;

        let total_on_disc = {
            let Mode::MediaLibrary(s) = &self.mode else {
                return;
            };
            s.disc_entries.len() as u8
        };

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.rip_cancel = Some(cancel.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        self.disc_rip = Some(rx);
        self.rip_progress = Some((0, entries.len(), entries[0].title.clone(), 0.0));

        std::thread::spawn(move || {
            use crate::disc::rip;
            let outcome = rip::run_job(
                &entries,
                std::path::Path::new(&dest),
                rip::Mp3Quality::from_config(quality),
                &tags,
                total_on_disc,
                &cancel,
                |i, n, title, frac| {
                    let _ = tx.send(super::RipMsg::Progress(i, n, title.to_string(), frac));
                },
            );
            let _ = tx.send(super::RipMsg::Done(outcome));
        });
    }

    /// Apply a rip progress/result message (called from the tick loop).
    pub(super) fn handle_rip_msg(&mut self, msg: super::RipMsg) {
        match msg {
            super::RipMsg::Progress(i, n, title, frac) => {
                self.rip_progress = Some((i, n, title, frac));
            }
            super::RipMsg::Done(outcome) => {
                self.disc_rip = None;
                self.rip_cancel = None;
                self.rip_progress = None;
                // Import the fresh files so the library sees them now. The
                // import only registers files under watched folders — the
                // shared status line reports honestly when some (or all)
                // stayed outside the library.
                let mut imported = 0;
                if !outcome.ripped.is_empty() {
                    if let Some(lib) = &self.media_lib {
                        imported = lib.add_files_to_library(&outcome.ripped).unwrap_or(0);
                    }
                }
                self.set_status(outcome.status_message(imported));
            }
        }
    }

    // -----------------------------------------------------------------------
    // Discs tab: burn (blind-implemented; hardware pass = Opus)
    // -----------------------------------------------------------------------

    /// Files tab `b`: queue the highlighted library track on the Burn list.
    fn add_selected_ml_track_to_burn_list(&mut self) {
        let track = if let Mode::MediaLibrary(s) = &self.mode {
            s.tracks.get(s.selected_track).cloned()
        } else {
            None
        };
        let Some(t) = track else { return };
        let display = match &t.artist {
            Some(a) if !a.is_empty() => format!(
                "{} - {}",
                a,
                t.title.clone().unwrap_or_else(|| t.filename.clone())
            ),
            _ => t.title.clone().unwrap_or_else(|| t.filename.clone()),
        };
        let bytes = std::fs::metadata(&t.path).map(|m| m.len()).unwrap_or(0);
        let added = self.burn_list.add(crate::disc::burnlist::BurnItem {
            path: std::path::PathBuf::from(&t.path),
            display,
            duration_secs: t.length_secs.map(|s| s as u32),
            bytes,
        });
        self.set_status(if added {
            format!("Queued for burning ({} on the list)", self.burn_list.len())
        } else {
            "Already on the burn list".to_string()
        });
    }

    /// Discs tab `b`: open the burn overlay for the selected drive's media.
    fn open_burn_setup(&mut self) {
        if self.burn_phase.is_some() {
            self.set_status("A burn is already running (c cancels it)");
            return;
        }
        if self.burn_list.is_empty() {
            self.set_status("Burn list is empty — queue tracks with b in the Files tab");
            return;
        }
        let drive = if let Mode::MediaLibrary(s) = &self.mode {
            s.drives.get(s.selected_drive).cloned()
        } else {
            None
        };
        let Some(drive) = drive else {
            self.set_status("No drive selected");
            return;
        };
        if crate::disc::burn::erase_decision(&drive) == crate::disc::burn::EraseDecision::Refuse {
            self.set_status(
                "This disc can't be written — insert a blank or rewritable disc",
            );
            return;
        }
        if let Mode::MediaLibrary(s) = &mut self.mode {
            s.burn = Some(super::BurnSetupState {
                cursor: 0,
                audio: true,
                confirm_erase: false,
            });
        }
    }

    /// Keys in the burn overlay: ↑/↓ move, x remove, t toggle audio/data,
    /// Enter start (or confirm the erase prompt with y), Esc close.
    fn handle_burn_setup_key(&mut self, code: KeyCode) {
        let mut start: Option<bool> = None; // Some(audio_mode) when confirmed
        if let Mode::MediaLibrary(s) = &mut self.mode {
            let Some(burn) = &mut s.burn else { return };
            if burn.confirm_erase {
                match code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        start = Some(burn.audio);
                        s.burn = None;
                    }
                    _ => {
                        // Anything else backs out of the destructive step.
                        burn.confirm_erase = false;
                    }
                }
            } else {
                match code {
                    KeyCode::Esc => s.burn = None,
                    KeyCode::Up | KeyCode::Char('k') => {
                        burn.cursor = burn.cursor.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if burn.cursor + 1 < self.burn_list.len() {
                            burn.cursor += 1;
                        }
                    }
                    KeyCode::Char('x') | KeyCode::Char('X') => {
                        self.burn_list.remove(burn.cursor);
                        if burn.cursor >= self.burn_list.len() && burn.cursor > 0 {
                            burn.cursor -= 1;
                        }
                        if self.burn_list.is_empty() {
                            s.burn = None;
                        }
                    }
                    // [ / ] — move the highlighted row up/down (burn order =
                    // track order on the disc).
                    KeyCode::Char('[') => {
                        self.burn_list.move_up(burn.cursor);
                        burn.cursor = burn.cursor.saturating_sub(1);
                    }
                    KeyCode::Char(']') => {
                        if burn.cursor + 1 < self.burn_list.len() {
                            self.burn_list.move_down(burn.cursor);
                            burn.cursor += 1;
                        }
                    }
                    KeyCode::Char('t') | KeyCode::Char('T') => burn.audio = !burn.audio,
                    KeyCode::Enter => {
                        // Erase-needed media asks for the explicit yes first.
                        let needs_confirm = s
                            .drives
                            .get(s.selected_drive)
                            .map(|d| {
                                crate::disc::burn::erase_decision(d)
                                    == crate::disc::burn::EraseDecision::EraseAfterConfirm
                            })
                            .unwrap_or(false);
                        if needs_confirm {
                            burn.confirm_erase = true;
                        } else {
                            start = Some(burn.audio);
                            s.burn = None;
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(audio) = start {
            self.spawn_burn(audio);
        }
    }

    /// Run the whole burn on a worker thread: optional erase, per-track WAV
    /// preparation (audio mode), then the burn tool. Progress arrives via
    /// `disc_burn` in the tick loop.
    fn spawn_burn(&mut self, audio: bool) {
        let drive = if let Mode::MediaLibrary(s) = &self.mode {
            s.drives.get(s.selected_drive).cloned()
        } else {
            None
        };
        let Some(drive) = drive else { return };

        // Capacity guard before anything touches the disc.
        if audio {
            let cap = crate::disc::burn::audio_capacity_secs(&drive);
            if self.burn_list.over_audio_capacity(cap) {
                self.set_status(format!(
                    "Over audio capacity ({} s of {} s) — remove tracks first",
                    self.burn_list.total_secs(),
                    cap
                ));
                return;
            }
        } else if drive.media.free_bytes > 0
            && self.burn_list.over_data_capacity(drive.media.free_bytes)
        {
            self.set_status("Over the disc's free space — remove files first");
            return;
        }

        let erase_first = crate::disc::burn::erase_decision(&drive)
            == crate::disc::burn::EraseDecision::EraseAfterConfirm;
        let verify = self.config.disc.burn_verify;
        // The MP3-CD companion playlist follows the app-wide format setting.
        let use_m3u = matches!(
            self.config.media_library.playlist_format,
            crate::config::PlaylistFormat::M3u
        );
        let items = self.burn_list.items.clone();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.burn_prep_cancel = Some(cancel.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        self.disc_burn = Some(rx);
        self.burn_phase = Some("Starting…".to_string());

        std::thread::spawn(move || {
            use crate::disc::burn;
            let staged = std::env::temp_dir().join(format!(
                "sparkamp-burn-{}",
                std::process::id()
            ));
            let _ = std::fs::create_dir_all(&staged);

            let result = (|| -> Result<String, String> {
                if erase_first {
                    let _ = tx.send(super::BurnMsg::Phase("Erasing…".to_string()));
                    burn::erase(&drive)?;
                }
                if audio {
                    let mut wavs = Vec::with_capacity(items.len());
                    for (i, item) in items.iter().enumerate() {
                        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            return Err("cancelled".to_string());
                        }
                        let _ = tx.send(super::BurnMsg::Phase(format!(
                            "Preparing {}/{} · {}",
                            i + 1,
                            items.len(),
                            item.display
                        )));
                        let out = staged.join(burn::staged_wav_name(i));
                        burn::prepare_wav(&item.path, &out)?;
                        wavs.push(out);
                    }
                    let _ = tx.send(super::BurnMsg::Phase(
                        "Burning… (this takes a while)".to_string(),
                    ));
                    burn::burn_audio(&drive, &staged, &wavs, verify)?;
                    Ok(format!("Audio CD burned ({} tracks)", items.len()))
                } else {
                    let files: Vec<std::path::PathBuf> =
                        items.iter().map(|i| i.path.clone()).collect();
                    let _ = tx.send(super::BurnMsg::Phase(
                        "Burning… (this takes a while)".to_string(),
                    ));
                    let staged_files = burn::stage_data_files(&files, &staged)?;
                    burn::write_data_playlist(&staged, &staged_files, use_m3u)?;
                    burn::burn_data(&drive, &staged, verify)?;
                    Ok(format!(
                        "Data disc burned ({} files + playlist)",
                        items.len()
                    ))
                }
            })();

            let _ = std::fs::remove_dir_all(&staged);
            let _ = tx.send(super::BurnMsg::Done(result));
        });
    }

    /// Apply a burn progress/result message (called from the tick loop).
    pub(super) fn handle_burn_msg(&mut self, msg: super::BurnMsg) {
        match msg {
            super::BurnMsg::Phase(text) => self.burn_phase = Some(text),
            super::BurnMsg::Done(result) => {
                self.disc_burn = None;
                self.burn_prep_cancel = None;
                self.burn_phase = None;
                match result {
                    Ok(summary) => {
                        self.burn_list.clear();
                        self.set_status(summary);
                    }
                    Err(e) => self.set_status(format!("Burn failed: {e}")),
                }
            }
        }
    }

    /// Re-detect optical drives (only on Discs-tab entry or an explicit
    /// `r`), clamp the drive selection, and reload the track entries of the
    /// selected drive. While a cdda:// track plays the scan is skipped —
    /// even status ioctls fault flaky drives mid-stream (the shared
    /// exclusive-read flag would blank the fresh-start list otherwise).
    pub(super) fn refresh_ml_drives(&mut self) {
        let playing_disc = *self.player.state() != crate::engine::PlayerState::Stopped
            && self
                .playlist
                .current()
                .map(|t| t.path.to_string_lossy().starts_with("cdda://"))
                .unwrap_or(false);
        if playing_disc {
            self.set_status("Drive busy (disc playing) — showing the last scan");
            return;
        }
        let drives = crate::disc::detect::list_drives_shared();
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

    /// Rebuild `disc_entries` for the currently selected drive, then overlay
    /// any stored tag-set titles for that disc.
    pub(super) fn reload_ml_disc_entries(&mut self) {
        if let Mode::MediaLibrary(s) = &mut self.mode {
            s.disc_entries = s
                .drives
                .get(s.selected_drive)
                .map(crate::disc::toc::track_entries)
                .unwrap_or_default();
            s.selected_disc_track = 0;
        }
        self.apply_disc_tags_to_entries();
    }

    /// Append disc-track entries to the current playlist with their tags:
    /// title from the entry (already overlaid with the disc's tag set),
    /// artist/album from the disc-level tags so the playlist shows
    /// "Artist - Title" like every other entry; the xmcd sampler convention
    /// ("Artist / Title" inside the track title) yields a per-track artist.
    /// No tag scan or duration probe: durations are exact from the TOC, and
    /// Linux `cdda://` pseudo-paths aren't probeable files anyway. Honors the
    /// same add-behavior config as the Files tab.
    pub(super) fn add_disc_entries(&mut self, entries: &[crate::disc::DiscTrackEntry]) {
        if entries.is_empty() {
            return;
        }
        let (disc_artist, disc_album) = self
            .selected_disc_identity()
            .and_then(|(_, id)| self.disc_tags.get(&id))
            .map(|t| (t.artist.clone(), t.album.clone()))
            .unwrap_or_default();
        let was_empty = self.playlist.is_empty();
        if self.config.behavior.playlist_add_behavior == crate::config::PlaylistAddBehavior::Replace
        {
            self.playlist.tracks.clear();
            self.playlist.current_index = 0;
            self.shuffle_state.reset();
        }
        for e in entries {
            // Sampler discs put the per-track artist in the title.
            let meta = crate::disc::track_meta(&e.title, &disc_artist);
            self.playlist.add(crate::model::Track {
                path: std::path::PathBuf::from(&e.path),
                title: meta.title,
                artist: meta.artist,
                album_artist: String::new(),
                album: disc_album.clone(),
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
