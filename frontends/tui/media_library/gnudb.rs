//! Discs tab: gnudb identification + submission.

use crossterm::event::KeyCode;
use super::super::*;

impl App {
    /// The selected drive's TOC and freedb id, when an audio disc is loaded.
    pub(super) fn selected_disc_identity(&self) -> Option<(sparkamp::disc::DiscToc, String)> {
        let Mode::MediaLibrary(s) = &self.mode else {
            return None;
        };
        let toc = s.drives.get(s.selected_drive)?.toc.clone()?;
        let id = sparkamp::disc::discid::freedb_discid(&toc);
        Some((toc, id))
    }

    /// The selected drive's id (device node on Linux, `drutil` index on
    /// macOS) — the value every disc subprocess (gnudb aside) targets.
    pub(super) fn selected_disc_drive_id(&self) -> Option<String> {
        let Mode::MediaLibrary(s) = &self.mode else {
            return None;
        };
        s.drives.get(s.selected_drive).map(|d| d.id.clone())
    }

    /// Whether the selected drive currently holds a recognized audio CD.
    /// A readable TOC alone isn't enough — data discs also report a TOC,
    /// just with `is_audio: false` tracks — so CD-TEXT reads (which spin
    /// the drive) must gate on this, not on `selected_disc_identity`.
    pub(super) fn selected_disc_is_audio_cd(&self) -> bool {
        let Mode::MediaLibrary(s) = &self.mode else {
            return false;
        };
        s.drives
            .get(s.selected_drive)
            .map(|d| d.media.is_audio_cd)
            .unwrap_or(false)
    }

    /// Read CD-TEXT off the currently selected unknown audio disc on a
    /// background thread (it spins the drive). One attempt per disc-id;
    /// result arrives through `disc_cdtext_read` in the tick loop. No-op
    /// when the selected drive isn't an audio CD (data discs have a TOC
    /// too, so `selected_disc_identity` alone can't tell), a read is
    /// already in flight, the disc was already tried, or gnudb already has
    /// an entry (CD-TEXT is a total-miss fallback only — Winamp precedence,
    /// see `apply_disc_tags_to_entries`).
    pub(crate) fn spawn_disc_cdtext_read(&mut self) {
        let Some((_, discid)) = self.selected_disc_identity() else {
            return;
        };
        if !self.selected_disc_is_audio_cd() {
            return;
        }
        if self.disc_tags.contains_key(&discid)
            || self.disc_cdtext_tried.contains(&discid)
            || self.disc_cdtext_read.is_some()
        {
            return;
        }
        let Some(drive_id) = self.selected_disc_drive_id() else {
            return;
        };
        self.disc_cdtext_tried.insert(discid.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        self.disc_cdtext_read = Some(rx);
        std::thread::spawn(move || {
            sparkamp::disc::detect::begin_exclusive_read();
            let cd = sparkamp::disc::cdtext::read_cdtext(&drive_id);
            sparkamp::disc::detect::end_exclusive_read();
            if let Ok(cd) = cd {
                // Receiver dropped = user closed the library; ignore send error.
                let _ = tx.send((discid.clone(), cd.to_xmcd(&discid)));
            }
        });
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
            use sparkamp::disc::{gnudb, xmcd};
            let msg = match gnudb::query(&toc, &email) {
                Err(e) => super::super::DiscLookupMsg::Failed(e.to_string()),
                Ok(matches) if matches.is_empty() => super::super::DiscLookupMsg::Failed(
                    "No gnudb match. Press e to fill tags in manually".to_string(),
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
            use sparkamp::disc::{gnudb, xmcd};
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
                        "gnudb: {n} candidate{} found. Open the Discs tab to choose",
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
            self.set_status("No tags yet. Press m to identify or e to edit first");
            return;
        };
        if sparkamp::disc::gnudb::is_unset_email(&self.config.disc.gnudb_email)
            || !sparkamp::disc::gnudb::is_valid_email(&self.config.disc.gnudb_email)
        {
            if let Mode::MediaLibrary(s) = &mut self.mode {
                s.submit_email = Some(String::new());
            }
            return;
        }
        let suggested = sparkamp::disc::gnudb::suggest_category(&entry.genre);
        let idx = sparkamp::disc::gnudb::CATEGORIES
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
                    if sparkamp::disc::gnudb::is_valid_email(&e) {
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
                    if *selected + 1 < sparkamp::disc::gnudb::CATEGORIES.len() {
                        *selected += 1;
                    }
                }
                KeyCode::Enter => {
                    submit_with = Some(sparkamp::disc::gnudb::CATEGORIES[*selected]);
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
        if let Err(reason) = sparkamp::disc::xmcd::validate_for_submit(&entry, &toc) {
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
            use sparkamp::disc::{discid as discid_mod, gnudb, xmcd};
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
    /// `n` forget the current match, Esc dismiss.
    pub(super) fn handle_gnudb_matches_key(&mut self, code: KeyCode) {
        let mut chosen: Option<(String, String)> = None;
        let mut forget = false;
        if let Mode::MediaLibrary(s) = &mut self.mode {
            let Some((list, selected)) = &mut s.gnudb_matches else {
                return;
            };
            match code {
                KeyCode::Esc => s.gnudb_matches = None,
                // The way out of a wrong match. gnudb answers with inexact
                // results by design and accepting one is a single keypress, so
                // without this a disc stayed mislabelled: the stored record
                // outranks CD-TEXT and survives restarts.
                KeyCode::Char('n') => {
                    forget = true;
                    s.gnudb_matches = None;
                }
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
        if forget {
            self.clear_disc_match();
        }
        if let Some((category, discid)) = chosen {
            self.spawn_disc_read(category, discid);
        }
    }

    /// Forget the loaded disc's stored tags and fall back to its own CD-TEXT.
    ///
    /// Drops the user's edits along with the official baseline, which is the
    /// point rather than a side effect: tags derived from a wrong match are
    /// wrong too. `disc_cdtext` is the disc's own data and is left alone, so
    /// the views fall straight back to it.
    pub(super) fn clear_disc_match(&mut self) {
        let Some((_, discid)) = self.selected_disc_identity() else {
            self.set_status("No audio disc loaded");
            return;
        };
        let had = self.disc_tags.remove(&discid).is_some();
        self.disc_official.remove(&discid);
        let mut store = sparkamp::disc::tagstore::DiscTagStore::load();
        store.clear(&discid);
        // Let the CD-TEXT read run again. It skips any disc that already has
        // tags and fires once per disc per launch, so a disc matched on gnudb
        // never had its CD-TEXT read at all: clearing without this leaves
        // nothing to fall back to and the view keeps the wrong album.
        self.disc_cdtext_tried.remove(&discid);
        self.spawn_disc_cdtext_read();
        self.apply_disc_tags_to_entries();
        self.propagate_disc_tags_to_playlist();
        if !had {
            self.set_status("No stored match for this disc");
        } else if self.disc_cdtext.contains_key(&discid) {
            self.set_status("Match removed. Using the disc's CD-TEXT.");
        } else {
            self.set_status("Match removed.");
        }
    }
}
