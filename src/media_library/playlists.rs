//! Playlist CRUD and M3U/M3U8 reading/writing, plus play-count
//! recording.

use anyhow::{Context, Result};
use rusqlite::params;
use std::path::{Path, PathBuf};

use crate::timeutil;

use super::{AddFolderResult, LibPlaylist, LibTrack, MediaLibrary, SortKeys};

// Bin build on macOS gates out GTK, leaving these FFI/GTK-reachable
// methods unused there; mirrors the allow on the original impl block.
#[allow(dead_code)]
impl MediaLibrary {

    /// Return all playlists (without populating their tracks).
    pub fn all_playlists(&self) -> Result<Vec<LibPlaylist>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path, name FROM playlists ORDER BY LOWER(name)")?;
        let rows = stmt.query_map([], |row| {
            Ok(LibPlaylist {
                id: row.get(0)?,
                path: row.get(1)?,
                name: row.get(2)?,
                tracks: Vec::new(),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("all_playlists query")
    }

    /// Parse a playlist file (`.m3u8` or legacy `.m3u`) and return all entries.
    ///
    /// Tracks found in the library are returned with full metadata.  Tracks
    /// whose paths are not present in the library (e.g. Windows-originated
    /// playlists, moved files) are returned as synthetic [`LibTrack`] stubs
    /// with `id = 0` so the UI can show them as missing rather than silently
    /// dropping them.
    pub fn load_playlist_tracks(&self, playlist: &LibPlaylist) -> Result<Vec<LibTrack>> {
        // Read as raw bytes then decode: try strict UTF-8 first, then fall back
        // to lossy replacement so Windows-1252 / Latin-1 encoded M3U files (e.g.
        // files created by Winamp or iTunes on Windows) are not silently empty.
        let bytes = std::fs::read(&playlist.path)
            .with_context(|| format!("read playlist {}", playlist.path))?;
        let content = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        };
        // Strip a leading UTF-8 BOM (\u{feff}) so the first line still
        // matches `starts_with('#')` checks below — without this the
        // `#EXTM3U` header on BOM-prefixed files (Windows tooling, FUSE
        // re-encoded mounts) falls through and shows up as a synthetic
        // missing-file track.
        let content = content.strip_prefix('\u{feff}').unwrap_or(&content).to_string();

        // Base directory for resolving relative paths in the M3U.
        let base = Path::new(&playlist.path)
            .parent()
            .unwrap_or_else(|| Path::new("/"));

        let mut tracks = Vec::new();
        // Title (and optionally artist) extracted from the preceding #EXTINF line.
        let mut extinf_title: Option<String> = None;
        let mut extinf_artist: Option<String> = None;
        let mut extinf_secs: Option<f64> = None;

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() { continue; }

            // Capture #EXTINF metadata for the next path line.
            if let Some(rest) = line.strip_prefix("#EXTINF:") {
                // Format: #EXTINF:<seconds>,<display-name>
                // display-name may be "Artist - Title" or just "Title".
                let (secs_str, display) = rest.split_once(',').unwrap_or((rest, ""));
                extinf_secs = secs_str.trim().parse::<f64>().ok();
                let display = display.trim();
                if let Some((a, t)) = display.split_once(" - ") {
                    extinf_artist = Some(a.trim().to_string());
                    extinf_title  = Some(t.trim().to_string());
                } else if !display.is_empty() {
                    extinf_title = Some(display.to_string());
                }
                continue;
            }
            // Skip other directives.
            if line.starts_with('#') { continue; }

            // Extract just the filename from the raw line, handling both
            // Unix ('/') and Windows ('\') separators.
            let raw_line = line;
            let filename: String = raw_line
                .replace('\\', "/")
                .split('/')
                .filter(|s| !s.is_empty())
                .last()
                .unwrap_or(raw_line)
                .to_string();

            // Resolve the path relative to the playlist file.
            // Replace Windows backslashes so Path can work with the string.
            let normalised = raw_line.replace('\\', "/");
            // `Path::is_absolute()` only returns true for Unix `/`-rooted paths.
            // Detect Windows drive-letter roots like `C:/…` explicitly so they
            // are not incorrectly joined with the playlist's parent directory.
            let is_win_abs = normalised.len() >= 3
                && normalised.as_bytes()[1] == b':'
                && (normalised.as_bytes()[2] == b'/' || normalised.as_bytes()[2] == b'\\');
            let path = if Path::new(&normalised).is_absolute() || is_win_abs {
                PathBuf::from(&normalised)
            } else {
                base.join(&normalised)
            };
            let path_str = match path.canonicalize() {
                Ok(p) => p.to_string_lossy().into_owned(),
                Err(_) => path.to_string_lossy().into_owned(),
            };

            // Snapshot any EXTINF metadata for this entry and clear it so it
            // never leaks onto a later line.
            let title  = extinf_title.take();
            let artist = extinf_artist.take();
            let secs   = extinf_secs.take();

            // Build a non-DB LibTrack ("stub") on `stub_path` from the EXTINF
            // metadata. `id == 0` only means "not catalogued" — whether the
            // UI shows it as missing is decided by the path's existence, so a
            // stub on an accessible path is a normal, playable track.
            let make_stub = |stub_path: String| -> LibTrack {
                let sort = SortKeys {
                    title:    title.as_deref().unwrap_or(&filename).to_lowercase(),
                    artist:   artist.as_deref().unwrap_or("").to_lowercase(),
                    filename: filename.to_lowercase(),
                    ..SortKeys::default()
                };
                LibTrack {
                    id:              0,          // sentinel: not in the DB
                    path:            stub_path,
                    filename:        filename.clone(),
                    title:           title.clone(),
                    artist:          artist.clone(),
                    length_secs:     secs,
                    album:           None,
                    track_num:       None,
                    genre:           None,
                    year:            None,
                    bpm:             None,
                    bitrate:         None,
                    channels:        None,
                    filetype:        None,
                    play_count:      0,
                    last_played:     None,
                    comment:         None,
                    album_artist:    None,
                    disc_num:        None,
                    disc_total:      None,
                    composer:        None,
                    original_artist: None,
                    copyright:       None,
                    url:             None,
                    encoded_by:      None,
                    lyric:           None,
                    artwork_path:    None,
                    last_scanned:    None,
                    sort_keys:       sort,
                }
            };

            // 1. Exact path match in the catalogue — richest, trusted data.
            if let Ok(t) = self.track_by_path(&path_str) {
                tracks.push(t);
                continue;
            }

            // 2. The line points at a file that exists on disk but isn't
            //    catalogued under this exact path (e.g. added to the active
            //    playlist without scanning, or the catalogue only knows a
            //    stale, inaccessible copy under a different path). Trust the
            //    file: keep its real, accessible path so it stays playable and
            //    never shows as missing. Borrow metadata from a same-filename
            //    catalogue row when one exists, but keep the verified path.
            if Path::new(&path_str).exists() {
                let mut t = self
                    .track_by_filename_first(&filename)
                    .unwrap_or_else(|_| make_stub(path_str.clone()));
                t.path = path_str.clone();
                t.filename = filename.clone();
                tracks.push(t);
                continue;
            }

            // 3. The literal path is gone — the file may have moved within the
            //    library. Fall back to a same-filename catalogue row, which
            //    can resolve to a new, accessible location.
            if let Ok(t) = self.track_by_filename_first(&filename) {
                tracks.push(t);
                continue;
            }

            // 4. Genuinely missing — synthetic stub on the raw path so the UI
            //    shows it in the unavailable color rather than hiding it.
            tracks.push(make_stub(raw_line.to_string()));
        }
        Ok(tracks)
    }

    /// Remove a playlist entry from the library by its row ID.
    ///
    /// The playlist file on disk is **not** deleted.
    #[allow(dead_code)]
    pub fn remove_playlist(&self, playlist_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM playlists WHERE id = ?1", params![playlist_id])?;
        Ok(())
    }

    /// Return the standard directory for user-created playlists.
    ///
    /// `~/.config/sparkamp/playlists/` on Linux/macOS.  Created if it does
    /// not exist yet.
    pub fn playlists_dir() -> PathBuf {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sparkamp")
            .join("playlists");
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// Format an `#EXTINF` line for a single entry.
    ///
    /// `duration` is rounded to whole seconds; `-1` is emitted when the
    /// duration is unknown (matches the Winamp / VLC convention).  The
    /// display name is `"Artist - Title"` when both are known, `"Title"`
    /// alone when only the title is known, else the filename fallback.
    pub(super) fn extinf_line(
        duration: Option<f64>,
        artist: Option<&str>,
        title: Option<&str>,
        fallback: &str,
    ) -> String {
        let secs = duration.map(|d| d.round() as i64).unwrap_or(-1);
        let artist = artist.filter(|s| !s.is_empty());
        let title = title.filter(|s| !s.is_empty());
        let display = match (artist, title) {
            (Some(a), Some(t)) => format!("{a} - {t}"),
            (None, Some(t)) => t.to_string(),
            _ => fallback.to_string(),
        };
        format!("#EXTINF:{secs},{display}")
    }

    /// Look up `(duration, artist, title)` for a track by its on-disk path,
    /// returning `(None, None, None)` when the path is not in the library.
    /// Used by `.m3u8` writers that only know paths (e.g. append-paths,
    /// save-as from raw paths) so the written file still gets `#EXTINF`
    /// metadata for tracks the library has already scanned.
    pub(super) fn metadata_by_path(
        &self,
        path: &str,
    ) -> (Option<f64>, Option<String>, Option<String>) {
        self.conn
            .query_row(
                "SELECT length_secs, artist, title FROM tracks WHERE path = ?1",
                params![path],
                |row| {
                    Ok((
                        row.get::<_, Option<f64>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .unwrap_or((None, None, None))
    }

    /// Build an `#EXTM3U` body from `(path, duration, artist, title)` tuples,
    /// emitting one `#EXTINF` line + the path for each entry.  Used by every
    /// path that writes a `.m3u8` so the format stays consistent.
    pub(super) fn build_m3u_body(
        entries: &[(String, Option<f64>, Option<String>, Option<String>)],
    ) -> String {
        let mut out = String::from("#EXTM3U\n");
        for (path, dur, artist, title) in entries {
            let fallback = Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(path);
            out.push_str(&Self::extinf_line(
                *dur,
                artist.as_deref(),
                title.as_deref(),
                fallback,
            ));
            out.push('\n');
            out.push_str(path);
            out.push('\n');
        }
        out
    }

    /// Create a new empty playlist with `name`.
    ///
    /// Writes an `#EXTM3U` header to
    /// `~/.config/sparkamp/playlists/<name>.m3u8` (sanitising the name for
    /// the filesystem) and registers the file in the library database.
    /// Returns the new playlist row id.
    ///
    /// New playlists are written as `.m3u8` (UTF-8 explicit) rather than
    /// `.m3u`; the loader still reads both extensions so existing files
    /// remain accessible.
    pub fn create_playlist(&self, name: &str) -> Result<i64> {
        let dir = Self::playlists_dir();
        let safe = name
            .chars()
            .map(|c| if r#"/\:*?"<>|"#.contains(c) { '_' } else { c })
            .collect::<String>();
        let safe = if safe.is_empty() { "Untitled".to_string() } else { safe };

        // Avoid clobbering an existing file.
        let mut path = dir.join(format!("{safe}.m3u8"));
        let mut counter = 1u32;
        while path.exists() {
            path = dir.join(format!("{safe}_{counter}.m3u8"));
            counter += 1;
        }
        std::fs::write(&path, b"#EXTM3U\n")
            .with_context(|| format!("create playlist file {}", path.display()))?;
        self.add_playlist_file(&path.to_string_lossy())
    }

    /// Rename playlist `id`.  Updates both the database record and the `.m3u`
    /// file on disk.
    ///
    /// Preserves the existing file extension (so a legacy `.m3u` stays
    /// `.m3u` and a new `.m3u8` stays `.m3u8`) — renaming should not
    /// silently change the on-disk format under the user.
    pub fn rename_playlist(&self, id: i64, new_name: &str) -> Result<()> {
        let pl = self.playlist_by_id(id)?;
        let old_path = Path::new(&pl.path);
        let safe = new_name
            .chars()
            .map(|c| if r#"/\:*?"<>|"#.contains(c) { '_' } else { c })
            .collect::<String>();
        let safe = if safe.is_empty() { "Untitled".to_string() } else { safe };
        let ext = old_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("m3u8");
        let new_filename = format!("{safe}.{ext}");
        let new_path = old_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(&new_filename);
        if old_path != new_path.as_path() {
            std::fs::rename(old_path, &new_path).with_context(|| {
                format!("rename {} → {}", old_path.display(), new_path.display())
            })?;
        }
        self.conn.execute(
            "UPDATE playlists SET name = ?1, path = ?2 WHERE id = ?3",
            params![new_name, new_path.to_string_lossy().as_ref(), id],
        )?;
        Ok(())
    }

    /// Overwrite the playlist `.m3u8` file with the tracks specified by
    /// `track_ids` (in order).  IDs not found in the library are skipped.
    ///
    /// Emits an `#EXTINF` line for every entry, pulling duration / artist /
    /// title from the library row, so the file can be reopened (here or by
    /// any other player) with the metadata intact.
    pub fn save_playlist_tracks(&self, id: i64, track_ids: &[i64]) -> Result<()> {
        let pl = self.playlist_by_id(id)?;
        let mut entries: Vec<(String, Option<f64>, Option<String>, Option<String>)> =
            Vec::with_capacity(track_ids.len());
        for &tid in track_ids {
            if let Ok((path, dur, artist, title)) = self.conn.query_row(
                "SELECT path, length_secs, artist, title FROM tracks WHERE id = ?1",
                params![tid],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<f64>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            ) {
                entries.push((path, dur, artist, title));
            }
        }
        let body = Self::build_m3u_body(&entries);
        std::fs::write(&pl.path, body)
            .with_context(|| format!("write playlist {}", pl.path))?;
        Ok(())
    }

    /// Create a new playlist named `new_name` and write `track_paths` to it.
    ///
    /// Write a new playlist file at exactly `target_path` and
    /// register it in the library.  Use this when the caller wants to
    /// choose the destination directory + filename (e.g. macOS NSSavePanel
    /// or any "Save As" flow that escapes the managed playlists dir).
    /// For the simple managed-dir case use [`save_playlist_tracks_as`]
    /// instead, which derives the path from the supplied name.
    ///
    /// Centralises the playlist body format so frontends don't reinvent it.
    /// Returns the new playlist row id.
    pub fn save_playlist_tracks_to_path(
        &self,
        target_path: &Path,
        track_paths: &[String],
    ) -> Result<i64> {
        let entries: Vec<(String, Option<f64>, Option<String>, Option<String>)> = track_paths
            .iter()
            .map(|p| {
                let (dur, artist, title) = self.metadata_by_path(p);
                (p.clone(), dur, artist, title)
            })
            .collect();
        std::fs::write(target_path, Self::build_m3u_body(&entries))
            .with_context(|| format!("write playlist {}", target_path.display()))?;
        self.add_playlist_file(&target_path.to_string_lossy())
    }

    /// Append `track_paths` to an existing playlist's `.m3u8` file on disk.
    ///
    /// Used by the "Add to Playlist" right-click menu so the user can grow
    /// a saved playlist with raw paths (including stubs from the active
    /// playlist).  Duplicates are not filtered — callers that care should
    /// pre-filter.  The DB row is unchanged because playlist contents live
    /// in the playlist file, not the database.
    ///
    /// Looks each path up in the library and emits an `#EXTINF` line ahead
    /// of it so duration / artist / title round-trip through the file.
    pub fn append_paths_to_playlist(
        &self,
        playlist_id: i64,
        track_paths: &[String],
    ) -> Result<()> {
        if track_paths.is_empty() { return Ok(()); }
        let pl = self.playlist_by_id(playlist_id)?;
        let existing = std::fs::read_to_string(&pl.path)
            .with_context(|| format!("read playlist {}", pl.path))?;
        // Preserve the existing trailing newline (or add one) before appending
        // so each new EXTINF/path pair starts on its own line.
        let mut body = existing;
        if !body.ends_with('\n') { body.push('\n'); }
        for p in track_paths {
            let (dur, artist, title) = self.metadata_by_path(p);
            let fallback = Path::new(p)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(p);
            body.push_str(&Self::extinf_line(
                dur,
                artist.as_deref(),
                title.as_deref(),
                fallback,
            ));
            body.push('\n');
            body.push_str(p);
            body.push('\n');
        }
        std::fs::write(&pl.path, body)
            .with_context(|| format!("write playlist {}", pl.path))?;
        Ok(())
    }

    /// Unlike [`save_playlist_tracks`] (which takes DB row IDs), this method
    /// accepts raw path strings so that missing-file stubs originating from
    /// Windows or moved-file playlists are preserved verbatim in the new file.
    /// Returns the new playlist's row id.
    ///
    /// Each path is looked up in the library; when a row exists its
    /// duration / artist / title are written as an `#EXTINF` line.  Stubs
    /// (paths not in the library) get a `-1` duration EXTINF using the
    /// filename as a display fallback.
    pub fn save_playlist_tracks_as(&self, new_name: &str, track_paths: &[String]) -> Result<i64> {
        let id = self.create_playlist(new_name)?;
        let pl = self.playlist_by_id(id)?;
        let entries: Vec<(String, Option<f64>, Option<String>, Option<String>)> = track_paths
            .iter()
            .map(|p| {
                let (dur, artist, title) = self.metadata_by_path(p);
                (p.clone(), dur, artist, title)
            })
            .collect();
        std::fs::write(&pl.path, Self::build_m3u_body(&entries))
            .with_context(|| format!("write playlist {}", pl.path))?;
        Ok(id)
    }

    /// Return `true` if the playlist file lives inside the Sparkamp-managed
    /// playlists directory (`~/.config/sparkamp/playlists/`).
    ///
    /// External playlists (scanned from watched folders) should not be
    /// overwritten via Save — the UI should offer Save As instead so the user
    /// gets a managed copy without clobbering the original.
    pub fn playlist_is_managed(&self, id: i64) -> bool {
        let Ok(pl) = self.playlist_by_id(id) else { return false };
        let pl_dir = Self::playlists_dir();
        Path::new(&pl.path)
            .parent()
            .map(|p| p == pl_dir.as_path())
            .unwrap_or(false)
    }

    /// Look up a playlist by its row ID.
    pub fn playlist_by_id(&self, id: i64) -> Result<LibPlaylist> {
        self.conn
            .query_row(
                "SELECT id, path, name FROM playlists WHERE id = ?1",
                params![id],
                |row| {
                    Ok(LibPlaylist {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        name: row.get(2)?,
                        tracks: Vec::new(),
                    })
                },
            )
            .context("playlist_by_id")
    }

    /// Add a playlist file (`.m3u8` / `.m3u`) to the library without scanning for audio tracks.
    ///
    /// Inserts the playlist into a synthetic folder (created if needed) whose
    /// path is the playlist file's parent directory.  Returns the new or
    /// existing row id.
    #[allow(dead_code)]
    pub fn add_playlist_file(&self, path: &str) -> Result<i64> {
        let p = Path::new(path);
        let parent = p.parent().unwrap_or(Path::new("/"));
        let folder_path = parent.to_string_lossy();
        let folder_id = match self.add_folder(&folder_path)? {
            AddFolderResult::New(id) | AddFolderResult::AlreadyExists(id) => id,
        };
        let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("Unnamed");
        self.conn.execute(
            "INSERT OR IGNORE INTO playlists (path, folder_id, name) VALUES (?1, ?2, ?3)",
            params![path, folder_id, name],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM playlists WHERE path = ?1",
            params![path],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    /// Increment the play count and update `last_played` for the track at `path`.
    #[allow(dead_code)]
    ///
    /// `last_played` is stored as an ISO-8601 UTC datetime string
    /// (`YYYY-MM-DDTHH:MM:SSZ`).  Does nothing if no track with that path
    /// exists in the database.
    pub fn record_play(&self, path: &str) -> Result<()> {
        // Build an ISO-8601 UTC timestamp from the current system time.
        let now = timeutil::format_current_timestamp();

        self.conn.execute(
            "UPDATE tracks SET play_count = play_count + 1, last_played = ?1
             WHERE path = ?2",
            params![now, path],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------
}
