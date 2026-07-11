//! Discs tab: rip to MP3.

use crossterm::event::KeyCode;
use super::super::*;

impl App {
    // -----------------------------------------------------------------------
    // Discs tab: rip to MP3
    // -----------------------------------------------------------------------

    /// Open the rip-setup overlay: all tracks preselected, destination from
    /// config → first watched folder → ~/Music, quality from config.
    pub(super) fn open_rip_setup(&mut self) {
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
            s.rip = Some(super::super::RipSetupState {
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
    pub(super) fn watched_folders(&self) -> Vec<String> {
        self.media_lib
            .as_ref()
            .and_then(|l| l.list_folders().ok())
            .map(|folders| folders.into_iter().map(|(_, p)| p).collect())
            .unwrap_or_default()
    }

    /// Keys in the rip overlay. Browsing: ↑/↓ move, Space toggle, a all/none,
    /// q cycle quality, d edit destination, Enter start, Esc cancel.
    /// Editing the destination: chars/Backspace, Enter/Esc done.
    pub(super) fn handle_rip_setup_key(&mut self, code: KeyCode) {
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
    pub(super) fn spawn_rip(&mut self) {
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
                    let _ = tx.send(super::super::RipMsg::Progress(i, n, title.to_string(), frac));
                },
            );
            let _ = tx.send(super::super::RipMsg::Done(outcome));
        });
    }

    /// Apply a rip progress/result message (called from the tick loop).
    pub(crate) fn handle_rip_msg(&mut self, msg: super::super::RipMsg) {
        match msg {
            super::super::RipMsg::Progress(i, n, title, frac) => {
                self.rip_progress = Some((i, n, title, frac));
            }
            super::super::RipMsg::Done(outcome) => {
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
}
