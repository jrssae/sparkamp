//! Normal-mode key dispatch and the small text-input overlays
//! (jump, add-file, move/remove track).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use crossterm::event::{KeyCode, KeyModifiers};

use crate::duration_probe;
use crate::engine::PlayerState;
use crate::id3_editor::{read_extra_frames, read_tag_fields};
use crate::model::{Playlist, Track};

use super::{expand_tilde, App, EqState, Id3EditorState, Mode, ScanChannels, SettingsState, STATUS_TICKS};

impl App {

    /// Add one or more files / directories from `raw_input`.
    ///
    /// `raw_input` may contain a **comma-separated list** of paths; each item
    /// is trimmed and processed individually.  Directory paths are scanned
    /// recursively; file paths are added directly.
    ///
    /// Nothing is read off disk here. Paths are resolved against the media
    /// library in one batched query (`crate::playlist_ingest`), so a folder the
    /// library has scanned costs no file access at all; anything it has never
    /// seen appears at once under its filename and is finished in the
    /// background. Reading tags inline instead cost a measured 27.974 ms per
    /// file, which is roughly seventeen minutes for a 36k folder — with the TUI
    /// frozen for all of it.
    ///
    /// The status message is updated to reflect the total number of added
    /// tracks and any errors.
    #[allow(dead_code)]
    pub fn commit_add_file(&mut self, raw_input: &str) {
        let mut total_errors = 0usize;
        let mut wanted: Vec<std::path::PathBuf> = Vec::new();

        // Split on commas so the user can type "song.mp3, /music/rock" and
        // add both in one go — mirrors the GTK "Add Files" multi-select UX.
        for part in raw_input.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let expanded = expand_tilde(part);
            // Only what the user typed is checked for existence — a handful of
            // paths, so the stat is free, and it keeps the "n errors" report
            // honest. Files found inside a directory are known to be there
            // already, and a library row is never touched at all.
            if !expanded.exists() {
                total_errors += 1;
                continue;
            }
            wanted.push(expanded);
        }

        let rows = crate::playlist_ingest::resolve(self.media_lib.as_ref(), &wanted);
        let total_added = self.add_resolved(rows);

        // Any playlist mutation resets shuffle history (new playlist = fresh draw).
        if total_added > 0 {
            self.shuffle_state.reset();
        }

        // Human-readable status feedback.
        let status_msg = match (total_added, total_errors) {
            (0, _) => "No valid audio files found".to_string(),
            (1, 0) => {
                // Show the track name for single-file adds.
                let name = self
                    .playlist
                    .tracks
                    .last()
                    .map(|t| t.display_name())
                    .unwrap_or_default();
                format!("Added: {name}")
            }
            (n, 0) => format!("Added {n} files"),
            (n, e) => format!(
                "Added {n} file{} ({e} error{})",
                if n == 1 { "" } else { "s" },
                if e == 1 { "" } else { "s" }
            ),
        };
        self.set_status(status_msg);
    }

    /// Place resolved rows in the playlist and start whatever still has to be
    /// read for them.
    ///
    /// The two kinds of row need different work, and asking for both would mean
    /// reading every unknown file twice:
    ///
    /// * a row the library described is complete except when its record had no
    ///   length — those go to the duration probe, and the cache usually answers
    ///   without any probe at all;
    /// * a row the library had never seen needs its tags read, and goes to the
    ///   row worker, which measures its duration in the same pass so the title
    ///   and the time land together rather than as two separate flickers.
    ///
    /// Returns how many rows were added.
    pub(crate) fn add_resolved(&mut self, rows: Vec<crate::playlist_ingest::Row>) -> usize {
        if rows.is_empty() {
            return 0;
        }
        let start = self.playlist.tracks.len();
        // Kept alongside the added range so the two kinds can be told apart
        // after the tracks have been moved into the playlist. Nothing reorders
        // in between, so position is a safe key here.
        let flags: Vec<bool> = rows.iter().map(|r| r.needs_tags).collect();
        let mut checks = Vec::new();
        for row in rows {
            let needs_tags = row.needs_tags;
            let path = row.track.path.clone();
            self.playlist.add(row.track);
            if needs_tags {
                let id = self.playlist.tracks.last().map(|t| t.id).unwrap_or(0);
                checks.push(crate::file_status::RowCheck {
                    path,
                    needs_tags: true,
                    id,
                });
            }
        }
        let added = self.playlist.tracks.len() - start;
        if !checks.is_empty() {
            let _ = self.row_check_tx.send(checks);
        }
        // Cached durations are free, so apply them to everything just added.
        for track in &mut self.playlist.tracks[start..] {
            if track.duration.is_none() {
                if let Some(dur) = self.duration_cache.get(&track.path) {
                    track.duration = Some(dur);
                }
            }
        }
        // Probe only the library rows the cache could not answer for; the
        // others are already with the row worker.
        let uncached: Vec<PathBuf> = self.playlist.tracks[start..]
            .iter()
            .zip(flags.iter())
            .filter(|(t, needs_tags)| !**needs_tags && t.duration.is_none())
            .map(|(t, _)| t.path.clone())
            .collect();
        if !uncached.is_empty() {
            duration_probe::spawn_probes(uncached, self.probe_tx.clone(), self.broken_tx.clone());
        }
        added
    }

    /// Apply cached durations to tracks from `from` onward, then launch
    /// background probes for any that are still unknown.
    ///
    /// Called both at startup (for command-line tracks) and after each
    /// `commit_add_file` call so durations appear as quickly as possible.
    /// `pub(crate)` so unit tests can exercise the cache-hit path directly.
    pub(crate) fn probe_new_tracks(&mut self, from: usize) {
        // Fill from cache first — avoids redundant probe work.
        for track in &mut self.playlist.tracks[from..] {
            if track.duration.is_none() {
                if let Some(dur) = self.duration_cache.get(&track.path) {
                    track.duration = Some(dur);
                }
            }
        }
        // Probe whatever remains uncached.
        let uncached: Vec<PathBuf> = self.playlist.tracks[from..]
            .iter()
            .filter(|t| t.duration.is_none())
            .map(|t| t.path.clone())
            .collect();
        if !uncached.is_empty() {
            duration_probe::spawn_probes(uncached, self.probe_tx.clone(), self.broken_tx.clone());
        }
    }

    /// Move the track at 1-based `from` to 1-based `to`.
    pub fn commit_move_track(&mut self, from_1: usize, to_1: usize) {
        let len = self.playlist.len();
        if from_1 == 0 || from_1 > len || to_1 == 0 || to_1 > len {
            self.set_status(format!("Invalid position (playlist has {} tracks)", len));
            return;
        }
        self.playlist.move_track(from_1 - 1, to_1 - 1);
        self.playlist_cursor = self.playlist.current_index;
        self.status_message = None;
    }

    /// Remove the track at 1-based `pos`.
    pub fn commit_remove_track(&mut self, pos_1: usize) {
        let len = self.playlist.len();
        if pos_1 == 0 || pos_1 > len {
            self.set_status(format!("Invalid position (playlist has {} tracks)", len));
            return;
        }
        let idx = pos_1 - 1;
        let was_current = idx == self.playlist.current_index;
        self.playlist.remove(idx);
        // Shuffle history is no longer valid after a playlist mutation.
        self.shuffle_state.reset();
        self.playlist_cursor = self.playlist.current_index;
        if was_current && !self.playlist.is_empty() {
            self.play_current();
        } else if self.playlist.is_empty() {
            let _ = self.player.stop();
        }
        self.status_message = None;
    }

    // -----------------------------------------------------------------------
    // Input handling
    // -----------------------------------------------------------------------

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // Ctrl+Q = queue/dequeue the highlighted track — same enqueue hotkey
        // as the GTK frontend. Works in Normal (playlist cursor) and Jump
        // (highlighted result); ignored elsewhere.
        if modifiers.contains(KeyModifiers::CONTROL)
            && matches!(code, KeyCode::Char('q') | KeyCode::Char('Q'))
        {
            self.queue_toggle_highlighted();
            return;
        }
        // Ctrl+F — search, matching the Media Library's existing binding.
        // Skipped while the Media Library is open: it already owns Ctrl+F
        // for its own search field (media_library/mod.rs), and handling it
        // globally here would intercept the key before that handler ever
        // saw it, silently replacing ML search with playlist jump.
        if modifiers.contains(KeyModifiers::CONTROL)
            && matches!(code, KeyCode::Char('f') | KeyCode::Char('F'))
            && !matches!(self.mode, Mode::MediaLibrary(..))
        {
            let results = (0..self.playlist.len()).collect();
            self.mode = Mode::Jump {
                query: String::new(),
                results,
                selected: 0,
                from_media_library: false,
            };
            return;
        }
        match self.mode {
            Mode::Normal => self.handle_normal(code),
            Mode::Jump { .. } => self.handle_jump(code),
            Mode::Queue { .. } => self.handle_queue(code),
            Mode::PlaylistOps { .. } => self.handle_playlist_ops(code),
            Mode::AddFile { .. } => self.handle_add_file(code),
            Mode::MoveTrack { .. } => self.handle_move_track(code),
            Mode::RemoveTrack { .. } => self.handle_remove_track(code),
            Mode::Help { ref mut scroll } => {
                match code {
                    // Scroll the overlay.
                    KeyCode::Up => *scroll = scroll.saturating_sub(1),
                    KeyCode::Down => *scroll = scroll.saturating_add(1),

                    // Playback pass-throughs — stay in help mode.
                    KeyCode::Char('z') => self.play_prev(),
                    KeyCode::Char('x') => {
                        if *self.player.state() == PlayerState::Stopped {
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

                    // Jump — switches mode (closes help implicitly).
                    KeyCode::Char('j') | KeyCode::Char('J') => {
                        let results = (0..self.playlist.len()).collect();
                        self.mode = Mode::Jump {
                            query: String::new(),
                            results,
                            selected: 0,
                            from_media_library: false,
                        };
                    }

                    // Close the overlay.
                    KeyCode::Esc | KeyCode::Char('i') | KeyCode::Char('I') => {
                        self.mode = Mode::Normal;
                    }

                    _ => {}
                }
            }
            Mode::Id3Editor(..) => self.handle_id3_editor(code, modifiers),
            Mode::Settings(..) => self.handle_settings(code, modifiers),
            Mode::Equalizer(..) => self.handle_equalizer(code),
            Mode::MediaLibrary(..) => self.handle_media_library(code, modifiers),
            Mode::NowPlaying { ref mut scroll, .. } => {
                match code {
                    // Scroll the overlay.
                    KeyCode::Up => *scroll = scroll.saturating_sub(1),
                    KeyCode::Down => *scroll = scroll.saturating_add(1),

                    // Playback pass-throughs — stay in now-playing mode.
                    KeyCode::Char('z') => self.play_prev(),
                    KeyCode::Char('x') => {
                        if *self.player.state() == PlayerState::Stopped {
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

                    // Jump — switches mode (closes now-playing implicitly).
                    KeyCode::Char('j') | KeyCode::Char('J') => {
                        let results = (0..self.playlist.len()).collect();
                        self.mode = Mode::Jump {
                            query: String::new(),
                            results,
                            selected: 0,
                            from_media_library: false,
                        };
                    }

                    // Close the overlay.
                    KeyCode::Esc | KeyCode::Char('w') | KeyCode::Char('W') => {
                        self.mode = Mode::Normal;
                    }

                    _ => {}
                }
            }
            Mode::Lyrics { ref mut scroll, ref search_url, .. } => {
                match code {
                    KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
                    // d — DuckDuckGo lyrics search in the browser (F15 revision,
                    // point 6). The window/overlay stays open behind it.
                    KeyCode::Char('d') => {
                        let url = search_url.clone();
                        match std::process::Command::new("xdg-open").arg(&url).spawn() {
                            Ok(_) => self.status_message = Some("Opening lyrics search in browser…".into()),
                            Err(_) => self.status_message = Some(format!("Lyrics: {url}")),
                        }
                        self.status_ticks = STATUS_TICKS;
                    }
                    // Close and restore the mode the viewer was opened from
                    // (e.g. the Media Library with its selection intact).
                    KeyCode::Esc | KeyCode::Char('l') | KeyCode::Char('q') => {
                        if let Mode::Lyrics { return_mode, .. } =
                            std::mem::replace(&mut self.mode, Mode::Normal)
                        {
                            self.mode = *return_mode;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    pub(super) fn handle_normal(&mut self, code: KeyCode) {
        match code {
            // Esc quits; q opens the play-queue manager (Ctrl+Q, handled in
            // handle_key, enqueues the highlighted track).
            KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.mode = Mode::Queue { selected: 0 };
            }

            // o — playlist ops popup (sort / randomize / reverse).
            KeyCode::Char('o') => {
                self.mode = Mode::PlaylistOps { selected: 0 };
            }

            // Winamp bindings
            KeyCode::Char('z') => self.play_prev(),
            KeyCode::Char('x') => {
                // Stopped → fresh play (play_current clears the flag); Paused/
                // Playing → resume via play(), which must NOT clear it.
                if *self.player.state() == PlayerState::Stopped {
                    self.play_current();
                } else {
                    let _ = self.player.play();
                }
            }
            KeyCode::Char('c') => {
                // Pause / resume must keep stop-after-current armed.
                if let Err(e) = self.player.toggle_pause() {
                    self.set_status(format!("Error: {e}"));
                }
            }
            KeyCode::Char('v') => {
                // Manual stop cancels a pending stop-after-current.
                self.player.set_stop_after_current(false);
                if let Err(e) = self.player.stop() {
                    self.set_status(format!("Error: {e}"));
                }
            }
            KeyCode::Char('b') => self.play_next(),

            // Shift+V — stop with fadeout. The ramp is driven by the tick loop
            // and stops the player when it reaches silence.
            KeyCode::Char('V') => {
                self.player.set_stop_after_current(false);
                let fade = self.config.playback.fadeout_duration();
                self.player.begin_fadeout(fade);
                // begin_fadeout owns the "only while playing" rule; asking the
                // player whether it took avoids restating that rule here.
                if self.player.is_fading_out() {
                    self.set_status(format!("Fading out over {}s…", fade.as_secs()));
                }
            }

            // t — toggle stop-after-current (phase 6). Fires at the next EOS,
            // then clears; the combined ▶⏹ header glyph shows it is armed.
            KeyCode::Char('t') | KeyCode::Char('T') => {
                let now = !self.player.stop_after_current();
                self.player.set_stop_after_current(now);
                self.set_status(if now {
                    "Stopping after current track".to_string()
                } else {
                    "Stop-after-current cancelled".to_string()
                });
            }

            // Playlist editing
            // n — add file(s) or folder(s); supports comma-separated list.
            KeyCode::Char('n') => {
                self.mode = Mode::AddFile {
                    input: String::new(),
                    scan_cancel: None,
                    scan_added: 0,
                };
            }
            // , — move a track (type from-number, Enter, to-number, Enter).
            KeyCode::Char(',') => {
                self.mode = Mode::MoveTrack {
                    input: String::new(),
                    from: None,
                };
            }
            // . — remove a track by 1-based number.
            KeyCode::Char('.') => {
                self.mode = Mode::RemoveTrack {
                    input: String::new(),
                };
            }

            // Toggle playlist visibility.  'p' and 'P' both work so the user
            // doesn't need to worry about Shift.
            KeyCode::Char('p') | KeyCode::Char('P') => {
                self.playlist_visible = !self.playlist_visible;
            }

            // Jump/search
            KeyCode::Char('j') | KeyCode::Char('J') => {
                let results = (0..self.playlist.len()).collect();
                self.mode = Mode::Jump {
                    query: String::new(),
                    results,
                    selected: 0,
                    from_media_library: false,
                };
            }

            // Volume — held key repeats automatically via crossterm key-repeat.
            KeyCode::Char('-') => {
                let vol = self.ctrl().adjust_volume(-0.05);
                self.set_status(format!("Volume: {}%", (vol * 100.0).round() as u32));
            }
            KeyCode::Char('=') => {
                let vol = self.ctrl().adjust_volume(0.05);
                self.set_status(format!("Volume: {}%", (vol * 100.0).round() as u32));
            }

            // Seek ±5 s; crossterm key-repeat fires repeatedly while held,
            // giving continuous fast-forward / rewind behaviour.
            KeyCode::Left => self.seek_delta_secs(-5.0),
            KeyCode::Right => self.seek_delta_secs(5.0),

            // Visualizer mode cycle: Bars → Waveform → Bars
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.cycle_visualizer_mode();
            }

            // Playlist navigation
            KeyCode::Up | KeyCode::Char('k') => {
                self.playlist_cursor = self.playlist_cursor.saturating_sub(1);
            }
            // Down only: every other list in the TUI pairs k/j, and `l` is
            // the lyrics key now. The arrows navigate every list regardless.
            KeyCode::Down => {
                if self.playlist_cursor + 1 < self.playlist.len() {
                    self.playlist_cursor += 1;
                }
            }
            KeyCode::Enter => {
                self.playlist.jump_to(self.playlist_cursor);
                self.play_current();
            }

            // Delete key: remove the currently highlighted playlist item.
            KeyCode::Delete => {
                if !self.playlist.is_empty() {
                    self.commit_remove_track(self.playlist_cursor + 1);
                }
            }

            // / — open jump / search. Every terminal app binds '/' to
            // search, and the Media Library already does. This used to clear
            // the entire playlist, which was a data-loss trap on the key
            // users press to search; Remove All now lives in the `o` popup.
            KeyCode::Char('/') => {
                let results = (0..self.playlist.len()).collect();
                self.mode = Mode::Jump {
                    query: String::new(),
                    results,
                    selected: 0,
                    from_media_library: false,
                };
            }

            // i / I — show keyboard shortcut reference overlay.
            KeyCode::Char('i') | KeyCode::Char('I') => {
                self.mode = Mode::Help { scroll: 0 };
            }

            // F1 — help, alongside `i`. No F-keys were bound before this, so
            // there is no conflict; `i` still works if a terminal multiplexer
            // swallows F1.
            KeyCode::F(1) => {
                self.mode = Mode::Help { scroll: 0 };
            }

            // r — cycle repeat mode: Off → Song → Playlist → Off.
            // Current mode is shown in the header track-info area.
            KeyCode::Char('r') | KeyCode::Char('R') => {
                let new_mode = self.config.playback.repeat_mode.cycle();
                self.config.playback.repeat_mode = new_mode;
                self.set_status(new_mode.label());
            }

            // s — toggle shuffle on/off (hidden shortcut; only shown in help).
            // Resets the shuffle history so the new setting takes effect cleanly.
            KeyCode::Char('s') | KeyCode::Char('S') => {
                self.shuffle_state.toggle();
                // Mirror to config so the setting survives to the next session.
                self.config.playback.shuffle_enabled = self.shuffle_state.enabled;
                if self.shuffle_state.enabled {
                    self.set_status("Shuffle: On");
                } else {
                    self.set_status("Shuffle: Off");
                }
            }

            // d — open the ID3 tag editor for the currently highlighted track.
            // When the playlist is hidden, fall back to the currently playing
            // track (current_index) because the cursor may lag behind.
            KeyCode::Char('d') | KeyCode::Char('D') => {
                let d_idx = if self.playlist_visible {
                    self.playlist_cursor
                } else {
                    self.playlist.current_index
                };
                if let Some(track) = self.playlist.tracks.get(d_idx) {
                    let path = track.path.clone();
                    let fields = read_tag_fields(&path);
                    let extra_frames = read_extra_frames(&path);
                    let initial_cursor = fields.title.chars().count();
                    // Media library lookup is best-effort: files not yet
                    // indexed just show fewer parts (or nothing) in the
                    // technical summary line.
                    let lib_track = self.media_lib.as_ref().and_then(|ml| {
                        ml.track_by_path(&path.to_string_lossy()).ok()
                    });
                    let ro = crate::media_library::read_only_track_fields(
                        &path,
                        lib_track.as_ref(),
                    );
                    let tech_summary = crate::media_library::tech_summary(&ro);
                    // ReplayGain is not a tag field — read the stored value so
                    // the row shows it even when the file carries no
                    // REPLAYGAIN_* frames (the usual case).
                    let rg_seed = crate::replaygain::manual_gain_field_text(
                        self.media_lib.as_ref(),
                        &path.to_string_lossy(),
                    );
                    self.mode = Mode::Id3Editor(Id3EditorState {
                        rg_gain: rg_seed.clone(),
                        rg_seed,
                        path,
                        tech_summary,
                        fields,
                        focused: 0,
                        cursor: initial_cursor,
                        genre_sel: 0,
                        show_extra: false,
                        extra_frames,
                        extra_focused: 0,
                        extra_editing: false,
                        extra_input: String::new(),
                        extra_cursor: 0,
                        status: None,
                    });
                } else {
                    self.set_status("No track selected");
                }
            }

            // l — View/Search Lyrics for the active-playlist track under the
            // cursor (or the playing track when the list is collapsed). Same key
            // as the GTK and macOS frontends.
            KeyCode::Char('l') | KeyCode::Char('L') => {
                let y_idx = if self.playlist_visible {
                    self.playlist_cursor
                } else {
                    self.playlist.current_index
                };
                let track = self.playlist.tracks.get(y_idx).map(|t| {
                    (t.path.clone(), t.artist.clone(), t.title.clone(), t.album_artist.clone())
                });
                if let Some((path, artist, title, album_artist)) = track {
                    self.open_lyrics(path, artist, title, album_artist);
                } else {
                    self.set_status("No track selected");
                }
            }

            // e — open the settings overlay.
            KeyCode::Char('e') | KeyCode::Char('E') => {
                self.mode = Mode::Settings(SettingsState {
                    tab: 0,
                    cursor: 0,
                    edit_buf: None,
                });
            }

            // u — open the 10-band equalizer overlay.
            KeyCode::Char('u') | KeyCode::Char('U') => {
                self.mode = Mode::Equalizer(EqState { selected_band: 0 });
            }

            // m / M — open the media library full-screen view.
            KeyCode::Char('m') | KeyCode::Char('M') => {
                self.open_media_library();
            }

            // w / W — full-screen now-playing data (tags, technical, stats,
            // links). "Now playing" = the track actually playing
            // (current_index), not the playlist cursor, since the two can
            // diverge while browsing.
            KeyCode::Char('w') | KeyCode::Char('W') => {
                if let Some(track) = self.playlist.tracks.get(self.playlist.current_index) {
                    let path = track.path.clone();
                    let path_str = path.to_string_lossy();
                    let lib_track = self
                        .media_lib
                        .as_ref()
                        .and_then(|ml| ml.track_by_path(&path_str).ok());
                    let snapshot = self
                        .media_lib
                        .as_ref()
                        .map(|ml| ml.play_snapshot(&path_str))
                        .unwrap_or_default();
                    let info = crate::now_playing::build_now_playing_info(
                        &path,
                        lib_track.as_ref(),
                        snapshot,
                    );
                    self.mode = Mode::NowPlaying {
                        scroll: 0,
                        info: Box::new(info),
                    };
                } else {
                    self.set_status("Nothing playing");
                }
            }

            _ => {}
        }
    }

    pub(super) fn handle_jump(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                // Return to the media library if that's where Jump was opened from.
                let from_ml = matches!(
                    self.mode,
                    Mode::Jump {
                        from_media_library: true,
                        ..
                    }
                );
                if from_ml {
                    self.open_media_library();
                } else {
                    self.mode = Mode::Normal;
                }
            }

            KeyCode::Enter => {
                let to_play = if let Mode::Jump {
                    ref results,
                    selected,
                    ..
                } = self.mode
                {
                    results.get(selected).copied()
                } else {
                    None
                };
                let from_ml = matches!(
                    self.mode,
                    Mode::Jump {
                        from_media_library: true,
                        ..
                    }
                );
                if let Some(idx) = to_play {
                    self.playlist.jump_to(idx);
                    self.play_current();
                }
                if from_ml {
                    self.open_media_library();
                } else {
                    self.mode = Mode::Normal;
                }
            }

            KeyCode::Up => {
                if let Mode::Jump {
                    ref mut selected, ..
                } = self.mode
                {
                    *selected = selected.saturating_sub(1);
                }
            }

            KeyCode::Down => {
                if let Mode::Jump {
                    ref mut selected,
                    ref results,
                    ..
                } = self.mode
                {
                    *selected = (*selected + 1).min(results.len().saturating_sub(1));
                }
            }

            KeyCode::Char(c) => {
                let new_query = if let Mode::Jump { ref query, .. } = self.mode {
                    let mut q = query.clone();
                    q.push(c);
                    q
                } else {
                    return;
                };
                self.apply_jump_query(new_query);
            }

            KeyCode::Backspace => {
                let new_query = if let Mode::Jump { ref query, .. } = self.mode {
                    let mut q = query.clone();
                    q.pop();
                    q
                } else {
                    return;
                };
                self.apply_jump_query(new_query);
            }

            _ => {}
        }
    }

    /// Ctrl+Q: toggle the queue membership of the highlighted track — the
    /// playlist cursor in Normal mode, or the selected result in Jump mode.
    pub(super) fn queue_toggle_highlighted(&mut self) {
        let track_idx = match self.mode {
            Mode::Jump {
                ref results,
                selected,
                ..
            } => results.get(selected).copied(),
            Mode::Normal if !self.playlist.is_empty() => Some(self.playlist_cursor),
            _ => None,
        };
        if let Some(idx) = track_idx {
            self.playlist.ensure_ids();
            if let Some(id) = self.playlist.tracks.get(idx).map(|t| t.id) {
                self.queue.toggle(id);
                let n = self.queue.len();
                self.set_status(format!("Queue: {n} track{}", if n == 1 { "" } else { "s" }));
            }
        }
    }

    /// Set the Queue overlay's highlighted position (no-op outside Queue mode).
    fn set_queue_selected(&mut self, v: usize) {
        if let Mode::Queue { ref mut selected } = self.mode {
            *selected = v;
        }
    }

    /// Key handling for the play-queue manager overlay (`Mode::Queue`).
    ///   ↑/k ↓/j  navigate      [ ]  move selected up / down
    ///   Enter    play now       Del/x  remove       c clear   r randomize
    ///   Esc/q    close
    pub(super) fn handle_queue(&mut self, code: KeyCode) {
        let sel = if let Mode::Queue { selected } = self.mode {
            selected
        } else {
            return;
        };
        let qlen = self.queue.len();
        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.mode = Mode::Normal;
            }
            KeyCode::Up | KeyCode::Char('k') => self.set_queue_selected(sel.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => {
                self.set_queue_selected((sel + 1).min(qlen.saturating_sub(1)));
            }
            KeyCode::Char('[') => {
                self.queue.move_up(sel);
                self.set_queue_selected(sel.saturating_sub(1));
            }
            KeyCode::Char(']') => {
                self.queue.move_down(sel);
                self.set_queue_selected((sel + 1).min(self.queue.len().saturating_sub(1)));
            }
            KeyCode::Delete | KeyCode::Char('x') => {
                if let Some(id) = self.queue.ids().get(sel).copied() {
                    self.queue.dequeue(id);
                }
                self.set_queue_selected(sel.min(self.queue.len().saturating_sub(1)));
            }
            KeyCode::Char('c') => self.queue.clear(),
            KeyCode::Char('r') => self.queue.shuffle(),
            KeyCode::Enter => {
                if let Some(id) = self.queue.ids().get(sel).copied() {
                    self.queue.dequeue(id);
                    if let Some(idx) = self.playlist.tracks.iter().position(|t| t.id == id) {
                        self.playlist.jump_to(idx);
                        self.play_current();
                    }
                }
                self.mode = Mode::Normal;
            }
            _ => {}
        }
    }

    /// The active-playlist ops popup's menu entries, in display order.
    /// Kept as a single source of truth for both the key handler (which
    /// dispatches by index) and the overlay (which renders the labels).
    pub(super) const PLAYLIST_OPS_LABELS: [&'static str; 8] = [
        "Sort: Title",
        "Sort: Artist",
        "Sort: Album",
        "Sort: Filename",
        "Sort: Path",
        "Randomize",
        "Reverse",
        "Remove All",
    ];

    /// Set the PlaylistOps overlay's highlighted position (no-op outside
    /// PlaylistOps mode).
    fn set_playlist_ops_selected(&mut self, v: usize) {
        if let Mode::PlaylistOps { ref mut selected } = self.mode {
            *selected = v;
        }
    }

    /// Key handling for the active-playlist ops popup (`Mode::PlaylistOps`).
    ///   ↑/k ↓/j  navigate      Enter  run highlighted op & close
    ///   Esc/o    close
    pub(super) fn handle_playlist_ops(&mut self, code: KeyCode) {
        let sel = if let Mode::PlaylistOps { selected } = self.mode {
            selected
        } else {
            return;
        };
        let len = Self::PLAYLIST_OPS_LABELS.len();
        match code {
            KeyCode::Esc | KeyCode::Char('o') | KeyCode::Char('O') => {
                self.mode = Mode::Normal;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.set_playlist_ops_selected(sel.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.set_playlist_ops_selected((sel + 1).min(len.saturating_sub(1)));
            }
            KeyCode::Enter => {
                match sel {
                    0 => self.playlist_sort(crate::model::SortKey::Title),
                    1 => self.playlist_sort(crate::model::SortKey::Artist),
                    2 => self.playlist_sort(crate::model::SortKey::Album),
                    3 => self.playlist_sort(crate::model::SortKey::Filename),
                    4 => self.playlist_sort(crate::model::SortKey::Path),
                    5 => self.playlist_randomize(),
                    6 => self.playlist_reverse(),
                    7 => {
                        // Clearing the whole playlist belongs with the other
                        // whole-playlist operations, not on a bare keystroke.
                        // Mirrors GTK's List ▾ ▸ Remove All.
                        let _ = self.player.stop();
                        self.playlist.tracks.clear();
                        self.playlist.current_index = 0;
                        self.playlist_cursor = 0;
                        self.shuffle_state.reset();
                        self.set_status("Playlist cleared");
                    }
                    _ => {}
                }
                self.mode = Mode::Normal;
            }
            _ => {}
        }
    }

    pub(super) fn apply_jump_query(&mut self, query: String) {
        // Preserve the from_media_library flag so Esc/Enter still return to ML.
        let from_media_library = matches!(
            self.mode,
            Mode::Jump {
                from_media_library: true,
                ..
            }
        );
        let results = if query.is_empty() {
            (0..self.playlist.len()).collect()
        } else {
            self.playlist.search_indices(&query)
        };
        self.mode = Mode::Jump {
            query,
            results,
            selected: 0,
            from_media_library,
        };
    }

    pub(super) fn handle_add_file(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                // Cancel any active background scan.
                if let Mode::AddFile {
                    ref scan_cancel, ..
                } = self.mode
                {
                    if let Some(cancel) = scan_cancel {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                let input = if let Mode::AddFile { ref input, .. } = self.mode {
                    input.clone()
                } else {
                    return;
                };
                // Collect all audio files from the comma-separated input.  Dir
                // walks are fast (readdir only, no metadata), so we do this on
                // the main thread before handing the list to the background scan.
                let mut all_files: Vec<PathBuf> = Vec::new();
                for part in input.split(',') {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }
                    let path = expand_tilde(part);
                    if path.is_dir() {
                        let files = Playlist::collect_audio_files(&path);
                        all_files.extend(files);
                    } else {
                        all_files.push(path);
                    }
                }
                let scan_start = self.playlist.tracks.len();
                let cancel = Arc::new(AtomicBool::new(false));
                let (fast_tx, fast_rx) = mpsc::channel::<Track>();
                let (meta_tx, meta_rx) = mpsc::channel::<(usize, String, String, String, String)>();
                let (done_tx, done_rx) = mpsc::channel::<usize>();
                let (phase1_done_tx, phase1_done_rx) = mpsc::channel::<usize>();
                Playlist::scan_files_for_ui(
                    all_files,
                    cancel.clone(),
                    fast_tx,
                    meta_tx,
                    done_tx,
                    phase1_done_tx,
                );
                if let Mode::AddFile {
                    ref mut scan_cancel,
                    ..
                } = self.mode
                {
                    *scan_cancel = Some(cancel);
                }
                self.scan_channels = Some(ScanChannels {
                    fast_rx,
                    meta_rx,
                    done_rx,
                    phase1_done_rx,
                    scan_start,
                    fast_done: false,
                });
                self.status_message = Some("Scanning…".to_string());
                self.status_ticks = STATUS_TICKS;
                self.drain_add_file_scan();
            }
            KeyCode::Backspace => {
                if let Mode::AddFile { ref mut input, .. } = self.mode {
                    input.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Mode::AddFile { ref mut input, .. } = self.mode {
                    input.push(c);
                }
            }
            _ => {}
        }
    }

    pub(super) fn handle_move_track(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                // Clone state needed before the mutable borrow in commit_*.
                let (input, from) = if let Mode::MoveTrack { ref input, from } = self.mode {
                    (input.clone(), from)
                } else {
                    return;
                };

                match from {
                    None => {
                        // First Enter: parse "from" position.
                        match input.trim().parse::<usize>() {
                            Ok(n) if n > 0 => {
                                self.mode = Mode::MoveTrack {
                                    input: String::new(),
                                    from: Some(n),
                                };
                            }
                            _ => {
                                self.set_status("Enter a valid track number");
                                self.mode = Mode::Normal;
                            }
                        }
                    }
                    Some(from_pos) => {
                        // Second Enter: parse "to" position and commit.
                        self.mode = Mode::Normal;
                        match input.trim().parse::<usize>() {
                            Ok(to_pos) if to_pos > 0 => {
                                self.commit_move_track(from_pos, to_pos);
                            }
                            _ => {
                                self.set_status("Enter a valid track number");
                            }
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                if let Mode::MoveTrack { ref mut input, .. } = self.mode {
                    input.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Mode::MoveTrack { ref mut input, .. } = self.mode {
                    input.push(c);
                }
            }
            _ => {}
        }
    }

    pub(super) fn handle_remove_track(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                let input = if let Mode::RemoveTrack { ref input } = self.mode {
                    input.clone()
                } else {
                    return;
                };
                self.mode = Mode::Normal;
                match input.trim().parse::<usize>() {
                    Ok(pos) if pos > 0 => {
                        self.commit_remove_track(pos);
                    }
                    _ => {
                        self.set_status("Enter a valid track number");
                    }
                }
            }
            KeyCode::Backspace => {
                if let Mode::RemoveTrack { ref mut input } = self.mode {
                    input.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Mode::RemoveTrack { ref mut input } = self.mode {
                    input.push(c);
                }
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Media library key handler
    // -----------------------------------------------------------------------

    /// Called every 100 ms from the event loop.
    ///
    /// Responsibilities in order:
    /// 1. Drain async probe results and write durations into the playlist +
    ///    cache so they appear in the display immediately.
    /// 2. Write the GStreamer-queried duration back to the current track the
    ///    first time it becomes available (GStreamer only reports duration once
    ///    the pipeline is Playing; this catches it on the first tick).
    /// 3. Advance to the next track on end-of-stream.
    pub(super) fn drain_add_file_scan(&mut self) {
        let Some(channels) = self.scan_channels.take() else {
            return;
        };
        let ScanChannels {
            fast_rx,
            meta_rx,
            done_rx,
            phase1_done_rx,
            scan_start,
            mut fast_done,
        } = channels;

        // Phase 1: drain fast tracks (path + filename, no metadata yet).
        // Use a short blocking recv first so tiny scans (single file) complete
        // without waiting for the next tick.
        if !fast_done {
            let timeout = std::time::Duration::from_millis(50);
            if let Ok(track) = fast_rx.recv_timeout(timeout) {
                let count = if let Mode::AddFile { scan_added, .. } = &mut self.mode {
                    *scan_added += 1;
                    *scan_added
                } else {
                    1
                };
                self.playlist.add(track);
                self.status_message = Some(format!(
                    "Scanning… {count} track{}",
                    if count == 1 { "" } else { "s" }
                ));
                self.status_ticks = STATUS_TICKS;
            }
            while let Ok(track) = fast_rx.try_recv() {
                if let Mode::AddFile { scan_added, .. } = &mut self.mode {
                    *scan_added += 1;
                }
                self.playlist.add(track);
            }
        }

        // Phase 2: apply metadata patches from the background thread.
        // Each message is (index-within-scan, title, artist, album_artist, album).
        while let Ok((idx, title, artist, album_artist, album)) = meta_rx.try_recv() {
            fast_done = true; // metadata arriving means Phase 1 is done
            let pos = scan_start + idx;
            if let Some(track) = self.playlist.tracks.get_mut(pos) {
                track.title = title;
                track.artist = artist;
                track.album_artist = album_artist;
                track.album = album;
            }
        }

        // Check for completion signal.
        if let Ok(_total) = done_rx.try_recv() {
            let added = if let Mode::AddFile { scan_added, .. } = &self.mode {
                *scan_added
            } else {
                self.playlist.tracks.len().saturating_sub(scan_start)
            };
            // Probe durations for all newly added tracks.
            let before = scan_start;
            self.probe_new_tracks(before);
            self.mode = Mode::Normal;
            self.shuffle_state.reset();
            let msg = match added {
                0 => "No audio files found".to_string(),
                n => format!("Added {n} file{}", if n == 1 { "" } else { "s" }),
            };
            self.status_message = Some(msg);
            self.status_ticks = STATUS_TICKS;
            return; // scan complete, do not restore channels
        }

        // Scan still running — put channels back.
        self.scan_channels = Some(ScanChannels {
            fast_rx,
            meta_rx,
            done_rx,
            phase1_done_rx,
            scan_start,
            fast_done,
        });
    }
}
