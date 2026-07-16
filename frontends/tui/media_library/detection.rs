//! Discs tab: optical-drive detection and disc-track entries.

use super::super::*;

impl App {
    /// Re-detect optical drives (only on Discs-tab entry or an explicit
    /// `r`), clamp the drive selection, and reload the track entries of the
    /// selected drive. While a cdda:// track plays the scan is skipped —
    /// even status ioctls fault flaky drives mid-stream (the shared
    /// exclusive-read flag would blank the fresh-start list otherwise).
    pub(crate) fn refresh_ml_drives(&mut self) {
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
        // Detect the drive we were viewing vanishing mid-session (unplugged or
        // ejected): flag it rather than silently resetting to another drive.
        let prev_selected_id = if let Mode::MediaLibrary(s) = &self.mode {
            s.drives.get(s.selected_drive).map(|d| d.id.clone())
        } else {
            None
        };
        let disconnected = prev_selected_id
            .as_ref()
            .map(|id| !drives.iter().any(|d| &d.id == id))
            .unwrap_or(false);

        // Auto-refresh (Task 10, mirrors the GTK poll from Phase-2 Task 3):
        // only rebuild disc_entries when the shown drive's media fingerprint
        // actually changed since the last poll — an unchanged disc keeps its
        // entries and highlighted track exactly as the user left them, so a
        // no-op `r` (or, in future, a periodic poll) never resets the
        // selection. No prior fingerprint (first visit this session) always
        // rebuilds.
        let entries_stale = match &prev_selected_id {
            Some(id) => drives
                .iter()
                .find(|d| &d.id == id)
                .map(|d| {
                    let new_fp = crate::disc::detect::media_fingerprint(d);
                    Some(new_fp) != self.disc_fingerprints.get(id).copied()
                })
                .unwrap_or(true),
            None => true,
        };
        // Snapshot fingerprints for every attached drive before `drives`
        // moves into `s.drives` below — next poll compares against these.
        self.disc_fingerprints = drives
            .iter()
            .map(|d| (d.id.clone(), crate::disc::detect::media_fingerprint(d)))
            .collect();

        if let Mode::MediaLibrary(s) = &mut self.mode {
            s.selected_drive = s.selected_drive.min(drives.len().saturating_sub(1));
            s.drives = drives;
        }
        if disconnected {
            // Invalidate the gone drive's session and prompt a reload — don't
            // silently show a different drive's tracks under the banner.
            if let Mode::MediaLibrary(s) = &mut self.mode {
                s.disc_entries.clear();
                s.selected_disc_track = 0;
            }
            self.set_status("Drive disconnected — reconnect and reload (r)");
            return;
        }
        // Rebuild disc_entries only when the shown drive's disc changed.
        if entries_stale {
            self.reload_ml_disc_entries();
        }
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
    pub(crate) fn reload_ml_disc_entries(&mut self) {
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
    pub(crate) fn add_disc_entries(&mut self, entries: &[crate::disc::DiscTrackEntry]) {
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
}
