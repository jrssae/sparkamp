//! Discs tab: gnudb identification + submission.

use crossterm::event::KeyCode;
use super::super::*;

impl App {
    /// The selected drive's TOC and freedb id, when an audio disc is loaded.
    pub(super) fn selected_disc_identity(&self) -> Option<(crate::disc::DiscToc, String)> {
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
    pub(crate) fn spawn_disc_lookup(&mut self) {
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
                Err(e) => super::super::DiscLookupMsg::Failed(e.to_string()),
                Ok(matches) if matches.is_empty() => super::super::DiscLookupMsg::Failed(
                    "No gnudb match — press e to fill tags in manually".to_string(),
                ),
                Ok(matches) if matches.len() == 1 && matches[0].exact => {
                    match gnudb::read(&matches[0].category, &matches[0].discid, &email) {
                        Ok(text) => match xmcd::parse(&text) {
                            Some(entry) => super::super::DiscLookupMsg::Entry(discid, entry),
                            None => super::super::DiscLookupMsg::Failed(
                                "gnudb entry was unreadable".to_string(),
                            ),
                        },
                        Err(e) => super::super::DiscLookupMsg::Failed(e.to_string()),
                    }
                }
                Ok(matches) => super::super::DiscLookupMsg::Matches(matches),
            };
            // Receiver dropped = user closed the library; nothing to do.
            let _ = tx.send(msg);
        });
    }

    /// Fetch one picked match in the background (same channel as the query).
    pub(super) fn spawn_disc_read(&mut self, category: String, matched_id: String) {
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
                    Some(entry) => super::super::DiscLookupMsg::Entry(discid, entry),
                    None => {
                        super::super::DiscLookupMsg::Failed("gnudb entry was unreadable".to_string())
                    }
                },
                Err(e) => super::super::DiscLookupMsg::Failed(e.to_string()),
            };
            let _ = tx.send(msg);
        });
    }

    /// Apply a background lookup result (called from the tick loop).
    pub(crate) fn handle_disc_lookup(&mut self, msg: super::super::DiscLookupMsg) {
        match msg {
            super::super::DiscLookupMsg::Failed(text) => {
                self.disc_lookup = None;
                self.set_status(text);
            }
            super::super::DiscLookupMsg::Matches(list) => {
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
            super::super::DiscLookupMsg::Entry(discid, entry) => {
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
            super::super::DiscLookupMsg::Submitted(msg) => {
                self.disc_lookup = None;
                self.set_status(format!("gnudb: {msg}"));
            }
        }
    }

    /// Open the submission category picker, preselecting the best-effort
    /// genre→category suggestion. Requires an edited/matched tag set, and —
    /// per the gnudb howto — the user's own email, captured here the first
    /// time (the config ships blank on purpose).
    pub(super) fn open_submit_category_picker(&mut self) {
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
    pub(super) fn handle_submit_email_key(&mut self, code: KeyCode) {
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
    pub(super) fn handle_submit_category_key(&mut self, code: KeyCode) {
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
    pub(super) fn spawn_disc_submit(&mut self, category: &'static str) {
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
                Ok(server_msg) => super::super::DiscLookupMsg::Submitted(if test_mode {
                    format!("{server_msg} (test mode — not published)")
                } else {
                    server_msg
                }),
                Err(e) => super::super::DiscLookupMsg::Failed(e.to_string()),
            };
            let _ = tx.send(msg);
        });
    }

    /// Keys while the gnudb match overlay is open: ↑/↓ select, Enter fetch,
    /// Esc dismiss.
    pub(super) fn handle_gnudb_matches_key(&mut self, code: KeyCode) {
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
}
