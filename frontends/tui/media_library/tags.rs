//! Discs tab: per-disc tag editing, persistence, and playlist propagation.

use crossterm::event::KeyCode;
use super::super::*;

impl App {
    /// Overlay the stored tag set's titles onto the visible disc entries
    /// ("Track N" stays wherever a title is missing).
    pub(crate) fn apply_disc_tags_to_entries(&mut self) {
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

    /// Open the tag editor prefilled from the stored tag set (or the visible
    /// "Track N" titles when the disc has no tags yet).
    pub(super) fn open_disc_tag_editor(&mut self) {
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
            s.tag_edit = Some(super::super::DiscTagEditState {
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
    pub(super) fn handle_disc_tag_edit_key(&mut self, code: KeyCode) {
        let mut save: Option<super::super::DiscTagEditState> = None;
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
    pub(super) fn persist_disc_tags(&self, discid: &str) {
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
    pub(super) fn propagate_disc_tags_to_playlist(&mut self) {
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
}
