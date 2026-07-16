//! Discs tab: burn audio/data discs.

use crossterm::event::KeyCode;
use super::super::*;

impl App {
    // -----------------------------------------------------------------------
    // Discs tab: burn (blind-implemented; hardware pass = Opus)
    // -----------------------------------------------------------------------

    /// The burn queue of the drive the Discs tab currently shows. `b` in the
    /// Files tab and every burn-overlay action target this list — never a
    /// picker, since the disc view is already per-drive (one row per
    /// physical drive). Creates the drive's queue empty on first use.
    ///
    /// Only callable where `self.mode` isn't already borrowed; sites inside
    /// an active `&mut self.mode` match (e.g. `handle_burn_setup_key`) read
    /// the drive id off their own `s: &mut MediaLibraryState` instead and go
    /// through `self.burn_queues.queue(&id)` directly, since a method call
    /// here would conflict with that borrow.
    fn selected_burn_list(&mut self) -> &mut crate::disc::burnlist::BurnList {
        let id = self.selected_drive_id().unwrap_or_default();
        self.burn_queues.queue(&id)
    }

    /// Id of the drive shown in the Discs tab, when one is selected.
    fn selected_drive_id(&self) -> Option<String> {
        if let Mode::MediaLibrary(s) = &self.mode {
            s.drives.get(s.selected_drive).map(|d| d.id.clone())
        } else {
            None
        }
    }

    /// Human label of the drive shown in the Discs tab, for the status line.
    fn selected_drive_label(&self) -> Option<String> {
        if let Mode::MediaLibrary(s) = &self.mode {
            s.drives.get(s.selected_drive).map(|d| d.label.clone())
        } else {
            None
        }
    }

    /// Files tab `b`: queue the highlighted library track onto the shown
    /// drive's burn list. When the library doesn't already know the track's
    /// duration, this probes the file synchronously via
    /// `duration_probe::probe_duration` — a single-file probe is quick, but
    /// it does block the TUI's event loop for that one keypress; acceptable
    /// in a terminal flow. Unreadable files are reported and never queued.
    pub(super) fn add_selected_ml_track_to_burn_list(&mut self) {
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
        let known = t.length_secs.map(|s| s as u32);
        let path = std::path::PathBuf::from(&t.path);
        let label = self.selected_drive_label().unwrap_or_default();
        let list = self.selected_burn_list();
        let out = crate::disc::burnlist::add_files(
            list,
            &[path],
            |p| {
                let bytes = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                (display.clone(), known, bytes)
            },
            |p| {
                crate::duration_probe::probe_duration_full(p).map(|d| d.as_secs() as u32)
            },
        );
        let total = list.len();
        self.set_status(
            out.failed_message()
                .unwrap_or_else(|| out.status_message(&label, total)),
        );
    }

    /// Discs tab `b`: open the burn overlay for the selected drive's media.
    pub(super) fn open_burn_setup(&mut self) {
        if self.burn_phase.is_some() {
            self.set_status("A burn is already running (c cancels it)");
            return;
        }
        if self.selected_burn_list().is_empty() {
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
            s.burn = Some(super::super::BurnSetupState {
                cursor: 0,
                audio: true,
                confirm_erase: false,
            });
        }
    }

    /// Keys in the burn overlay: ↑/↓ move, x remove, t toggle audio/data,
    /// Enter start (or confirm the erase prompt with y), Esc close.
    pub(super) fn handle_burn_setup_key(&mut self, code: KeyCode) {
        let mut start: Option<bool> = None; // Some(audio_mode) when confirmed
        if let Mode::MediaLibrary(s) = &mut self.mode {
            // `s` already borrows self.mode mutably, so the list has to come
            // from the drive id read off `s` directly (disjoint field from
            // self.burn_queues) rather than through `self.selected_burn_list()`.
            let drive_id = s
                .drives
                .get(s.selected_drive)
                .map(|d| d.id.clone())
                .unwrap_or_default();
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
                        if burn.cursor + 1 < self.burn_queues.queue(&drive_id).len() {
                            burn.cursor += 1;
                        }
                    }
                    KeyCode::Char('x') | KeyCode::Char('X') => {
                        let list = self.burn_queues.queue(&drive_id);
                        list.remove(burn.cursor);
                        if burn.cursor >= list.len() && burn.cursor > 0 {
                            burn.cursor -= 1;
                        }
                        if list.is_empty() {
                            s.burn = None;
                        }
                    }
                    // [ / ] — move the highlighted row up/down (burn order =
                    // track order on the disc).
                    KeyCode::Char('[') => {
                        self.burn_queues.queue(&drive_id).move_up(burn.cursor);
                        burn.cursor = burn.cursor.saturating_sub(1);
                    }
                    KeyCode::Char(']') => {
                        if burn.cursor + 1 < self.burn_queues.queue(&drive_id).len() {
                            self.burn_queues.queue(&drive_id).move_down(burn.cursor);
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
    pub(super) fn spawn_burn(&mut self, audio: bool) {
        let drive = if let Mode::MediaLibrary(s) = &self.mode {
            s.drives.get(s.selected_drive).cloned()
        } else {
            None
        };
        let Some(drive) = drive else { return };

        // Capacity guard before anything touches the disc.
        if audio {
            let cap = crate::disc::burn::audio_capacity_secs(&drive);
            if self.burn_queues.queue(&drive.id).over_audio_capacity(cap) {
                let total_secs = self.burn_queues.queue(&drive.id).total_secs();
                self.set_status(format!(
                    "Over audio capacity ({total_secs} s of {cap} s) — remove tracks first",
                ));
                return;
            }
        } else {
            // Same fallback as GTK: some probes report free=0 on blank media
            // while the capacity is known — guard against the total then
            // rather than letting an oversized burn fail at the tool.
            let free = if drive.media.free_bytes > 0 {
                drive.media.free_bytes
            } else {
                drive.media.capacity_bytes
            };
            if free > 0 && self.burn_queues.queue(&drive.id).over_data_capacity(free) {
                self.set_status("Over the disc's free space — remove files first");
                return;
            }
        }

        let erase_first = crate::disc::burn::erase_decision(&drive)
            == crate::disc::burn::EraseDecision::EraseAfterConfirm;
        let verify = self.config.disc.burn_verify;
        // The MP3-CD companion playlist follows the app-wide format setting.
        let use_m3u = matches!(
            self.config.media_library.playlist_format,
            crate::config::PlaylistFormat::M3u
        );
        let items = self.burn_queues.queue(&drive.id).items.clone();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.burn_prep_cancel = Some(cancel.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        self.disc_burn = Some(rx);
        self.burn_phase = Some("Starting…".to_string());

        std::thread::spawn(move || {
            // The whole orchestration (staging, erase, prep, burn, cache
            // invalidation, cleanup) is the shared core job — this worker
            // only forwards its phase strings onto the tick channel.
            use crate::disc::burn;
            let mode = if audio {
                burn::BurnMode::Audio
            } else {
                burn::BurnMode::Data { use_m3u }
            };
            let result = burn::run_job(&drive, &items, mode, erase_first, verify, &cancel, |p| {
                let _ = tx.send(super::super::BurnMsg::Phase(p.to_string()));
            });
            let _ = tx.send(super::super::BurnMsg::Done(result));
        });
    }

    /// Apply a burn progress/result message (called from the tick loop).
    pub(crate) fn handle_burn_msg(&mut self, msg: super::super::BurnMsg) {
        match msg {
            super::super::BurnMsg::Phase(text) => self.burn_phase = Some(text),
            super::super::BurnMsg::Done(result) => {
                self.disc_burn = None;
                self.burn_prep_cancel = None;
                self.burn_phase = None;
                match result {
                    Ok(summary) => {
                        // Clears the *shown* drive's queue. Only one burn
                        // runs at a time, and nothing currently stops the
                        // user from switching the Discs-tab selection while
                        // it runs — a mid-burn switch would clear the wrong
                        // drive's list here. Pre-existing single-queue
                        // behavior; not tightened by this per-drive change.
                        self.selected_burn_list().clear();
                        self.set_status(summary);
                    }
                    Err(e) => self.set_status(format!("Burn failed: {e}")),
                }
            }
        }
    }
}
