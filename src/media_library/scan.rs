//! Folder management and the two-phase scan pipeline (fast path walk,
//! background tag read), plus single-track rescan helpers.

use anyhow::{Context, Result};
use rusqlite::params;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::model::AUDIO_EXTENSIONS;
use crate::tags::read_track_tags;
use crate::timeutil;

use super::{AddFolderResult, MediaLibrary};

// Bin build on macOS gates out GTK, leaving these FFI/GTK-reachable
// methods unused there; mirrors the allow on the original impl block.
#[allow(dead_code)]
impl MediaLibrary {

    /// Canonicalize a folder path so `add_folder` and `folder_exists`
    /// agree on the comparison key under symlink indirection (macOS
    /// `/var → /private/var`, Flatpak document-portal FUSE mounts).
    /// Resolves the existing part of a not-yet-created path via the shared
    /// [`crate::pathutil::canonicalize_lenient`], so a path that doesn't exist
    /// on disk still lands under the same resolved ancestors.
    pub(super) fn canonicalize_folder_path(path: &str) -> String {
        crate::pathutil::canonicalize_lenient(Path::new(path))
            .to_string_lossy()
            .into_owned()
    }

    /// Check if a folder path is already in the watch list.
    /// Returns `Ok(Some(id))` if found, `Ok(None)` if not found.
    ///
    /// The input is canonicalized before lookup so callers can pass any
    /// equivalent path (with or without symlink resolution) and still get
    /// a hit on a previously-added folder.
    pub(super) fn folder_exists(&self, path: &str) -> Result<Option<i64>> {
        let canonical = Self::canonicalize_folder_path(path);
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM folders WHERE path = ?1")?;
        let result = stmt.query_row(params![canonical.as_str()], |row| row.get(0));
        match result {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Add a folder path to the watch list.
    ///
    /// If the folder is already present, returns `AlreadyExists(id)` without
    /// modifying the database.  If it is new, inserts it and returns `New(id)`.
    ///
    /// Use this to distinguish "add a new folder" from "rescan an existing one"
    /// so callers can show appropriate feedback (e.g. "Added" vs "Rescanning…").
    ///
    /// The path is canonicalized before storing so that document-portal FUSE
    /// mounts (e.g. `/run/user/<uid>/doc/<hash>/Music` on Flatpak) and macOS
    /// `/var → /private/var` symlinks resolve to the same real path as a
    /// directly-added `~/Music`, preventing duplicates.
    pub fn add_folder(&self, path: &str) -> Result<AddFolderResult> {
        let canonical = Self::canonicalize_folder_path(path);
        let path = canonical.as_str();

        if let Some(id) = self.folder_exists(path)? {
            return Ok(AddFolderResult::AlreadyExists(id));
        }
        self.conn
            .execute("INSERT INTO folders (path) VALUES (?1)", params![path])?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM folders WHERE path = ?1",
            params![path],
            |row| row.get(0),
        )?;
        Ok(AddFolderResult::New(id))
    }

    /// Normalize any portal-path folder entries in the DB to their canonical
    /// real paths.  Called once at startup to repair duplicates created before
    /// `add_folder` gained canonicalization.
    ///
    /// If two folder entries resolve to the same canonical path (e.g. one is a
    /// `/run/user/.../doc/…` mirror of `~/Music`), the one with fewer tracks is
    /// removed and its tracks/playlists are re-homed to the surviving entry.
    pub(super) fn dedup_folders(&self) -> Result<()> {
        let folders = self.list_folders()?;

        // Build: canonical_path → list of (id, original_path)
        let mut by_canonical: std::collections::HashMap<String, Vec<(i64, String)>> =
            std::collections::HashMap::new();
        for (id, orig) in &folders {
            let canonical = Path::new(orig)
                .canonicalize()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| orig.clone());
            by_canonical
                .entry(canonical)
                .or_default()
                .push((*id, orig.clone()));
        }

        for (canonical, mut entries) in by_canonical {
            if entries.len() <= 1 {
                // Only one entry for this canonical path — just ensure it is
                // stored under the canonical string (update if it differed).
                if let Some((id, orig)) = entries.first() {
                    if orig != &canonical {
                        let _ = self.conn.execute(
                            "UPDATE folders SET path = ?1 WHERE id = ?2",
                            params![canonical, id],
                        );
                    }
                }
                continue;
            }

            // Multiple entries → keep the one whose path already is canonical
            // (or the first one if none is), merge the rest into it.
            entries.sort_by_key(|(_, p)| if p == &canonical { 0 } else { 1 });
            let (keep_id, keep_path) = entries[0].clone();

            // Ensure the surviving entry uses the canonical path.
            if keep_path != canonical {
                let _ = self.conn.execute(
                    "UPDATE folders SET path = ?1 WHERE id = ?2",
                    params![canonical, keep_id],
                );
            }

            // Re-home tracks and playlists from the duplicate entries.
            for (dup_id, _) in &entries[1..] {
                let _ = self.conn.execute(
                    "UPDATE tracks    SET folder_id = ?1 WHERE folder_id = ?2",
                    params![keep_id, dup_id],
                );
                let _ = self.conn.execute(
                    "UPDATE playlists SET folder_id = ?1 WHERE folder_id = ?2",
                    params![keep_id, dup_id],
                );
                let _ = self.conn.execute(
                    "DELETE FROM folders WHERE id = ?1",
                    params![dup_id],
                );
            }
        }

        Ok(())
    }

    /// Remove a folder and all its tracks and playlists from the library.
    ///
    /// Does nothing (no error) if `folder_id` does not exist.
    #[allow(dead_code)]
    pub fn remove_folder(&self, folder_id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM tracks    WHERE folder_id = ?1",
            params![folder_id],
        )?;
        self.conn.execute(
            "DELETE FROM playlists WHERE folder_id = ?1",
            params![folder_id],
        )?;
        self.conn
            .execute("DELETE FROM folders   WHERE id = ?1", params![folder_id])?;
        Ok(())
    }

    /// List all watched folders as `(id, path)` pairs, sorted by path.
    pub fn list_folders(&self) -> Result<Vec<(i64, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path FROM folders ORDER BY path")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("list_folders query")
    }

    /// Add a list of audio file paths to the library DB.  For each path,
    /// finds the deepest watched folder whose path is a prefix of the
    /// file's path and upserts the track under that folder.  Paths that
    /// live outside every watched folder are silently skipped — adding
    /// them would require registering a new watched folder, which the
    /// drop-onto-Files-table flow explicitly forbids (user-facing rule:
    /// "add to library DB only, no new watch folders").
    ///
    /// Returns the count of paths that were actually upserted.
    pub fn add_files_to_library(&self, paths: &[String]) -> Result<usize> {
        let folders = self.list_folders()?;
        let mut added = 0;
        for path in paths {
            // Deepest matching folder wins (handles nested watched folders).
            let mut best: Option<(i64, &str)> = None;
            for (fid, fpath) in &folders {
                if path.starts_with(fpath.as_str())
                    && (best.is_none() || fpath.len() > best.unwrap().1.len())
                {
                    best = Some((*fid, fpath.as_str()));
                }
            }
            let Some((folder_id, _)) = best else { continue };
            // upsert_track is fallible per-file (probe failure, IO, etc.);
            // log and continue so one bad file doesn't abort the batch.
            if let Err(e) = self.upsert_track(folder_id, path) {
                eprintln!("add_files_to_library: skip {path}: {e}");
                continue;
            }
            added += 1;
        }
        Ok(added)
    }

    /// Return all track IDs in a folder, for soft-delete UI updates.
    pub fn track_ids_for_folder(&self, folder_id: i64) -> Result<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM tracks WHERE folder_id = ?1")?;
        let rows = stmt.query_map(params![folder_id], |row| row.get::<_, i64>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("track_ids_for_folder query")
    }

    // -----------------------------------------------------------------------
    // Scanning
    // -----------------------------------------------------------------------

    /// Rescan all watched folders.
    ///
    /// Calls [`rescan_folder`] for each folder in the `folders` table.
    /// Returns the total `(added, removed)` counts across all folders.
    pub fn rescan_all(&self) -> Result<(usize, usize)> {
        // Snapshot folders first to avoid re-borrowing conn inside the loop.
        let folders = self.list_folders()?;
        let mut total_added = 0usize;
        let mut total_removed = 0usize;
        for (id, path) in folders {
            let (a, r) = self.rescan_folder(id, &path)?;
            total_added += a;
            total_removed += r;
        }
        Ok((total_added, total_removed))
    }

    /// Scan a single folder for audio files and `.m3u8` / `.m3u` playlists.
    ///
    /// Walk the directory tree recursively, collecting:
    /// - Audio files (by extension) → upsert into `tracks`.
    /// - `.m3u8` / `.m3u` files → upsert into `playlists`.
    ///
    /// Tracks that were previously in the DB but whose file no longer exists
    /// on disk are removed.  Returns `(added, removed)` counts.
    pub fn rescan_folder(&self, folder_id: i64, folder_path: &str) -> Result<(usize, usize)> {
        let mut audio_files: Vec<PathBuf> = Vec::new();
        let mut m3u_files: Vec<PathBuf> = Vec::new();
        Self::walk_dir(
            Path::new(folder_path),
            AUDIO_EXTENSIONS,
            &mut audio_files,
            &mut m3u_files,
        );

        // Use paths as-is for fast insert. Canonicalization adds a stat call per file,
        // which is the main bottleneck for large libraries. The path returned by
        // read_dir is already in canonical form for the access path.
        let audio_paths: Vec<String> = audio_files
            .iter()
            .filter_map(|p| p.to_str().map(String::from))
            .collect();

        let existing_paths: std::collections::HashSet<String> = if audio_paths.is_empty() {
            std::collections::HashSet::new()
        } else {
            let mut result = std::collections::HashSet::new();
            for chunk in audio_paths.chunks(1000) {
                let placeholders: Vec<String> = chunk.iter().map(|_| "?".to_string()).collect();
                let sql = format!(
                    "SELECT path FROM tracks WHERE path IN ({})",
                    placeholders.join(",")
                );
                let params: Vec<&dyn rusqlite::ToSql> =
                    chunk.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
                let mut stmt = self.conn.prepare(&sql)?;
                stmt.query_map(params.as_slice(), |r| r.get::<_, String>(0))?
                    .filter_map(|r| r.ok())
                    .for_each(|p| {
                        result.insert(p);
                    });
            }
            result
        };

        // Upsert each audio file, counting genuinely new insertions.
        let mut added = 0usize;
        for path in &audio_paths {
            let is_new = !existing_paths.contains(path);
            self.upsert_track(folder_id, path)?;
            if is_new {
                added += 1;
            }
        }

        // Upsert .m3u8 / .m3u playlists.  Use ON CONFLICT … DO UPDATE
        // (not INSERT OR REPLACE) so the row's id is preserved across
        // rescans — REPLACE deletes + re-inserts, churning the id and
        // invalidating any UI that captured the old value.
        for m3u in &m3u_files {
            if let Some(name) = m3u.file_stem().and_then(|s| s.to_str()) {
                let p = m3u.to_string_lossy();
                self.conn.execute(
                    "INSERT INTO playlists (path, folder_id, name)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(path) DO UPDATE SET
                         folder_id = excluded.folder_id,
                         name      = excluded.name",
                    params![p.as_ref(), folder_id, name],
                )?;
            }
        }

        // Remove tracks that belong to this folder but whose files no longer exist.
        let mut stmt = self
            .conn
            .prepare("SELECT id, path FROM tracks WHERE folder_id = ?1")?;
        let existing: Vec<(i64, String)> = stmt
            .query_map(params![folder_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        let mut removed = 0usize;
        for (id, path) in existing {
            if !std::path::Path::new(&path).exists() {
                self.conn
                    .execute("DELETE FROM tracks WHERE id = ?1", params![id])?;
                removed += 1;
            }
        }
        Ok((added, removed))
    }

    /// Fast path: insert file paths only (no metadata).
    /// This returns immediately after collecting paths and inserting them into DB.
    /// Call `rescan_folder_metadata` after this to update metadata asynchronously.
    pub fn rescan_folder_fast(&self, folder_id: i64, folder_path: &str) -> Result<(usize, usize)> {
        let mut audio_files: Vec<PathBuf> = Vec::new();
        let mut m3u_files: Vec<PathBuf> = Vec::new();
        Self::walk_dir(
            Path::new(folder_path),
            AUDIO_EXTENSIONS,
            &mut audio_files,
            &mut m3u_files,
        );

        // Use paths as-is for fast insert. Skipping canonicalize() removes a stat
        // call per file — the main bottleneck for large libraries.
        let audio_paths: Vec<String> = audio_files
            .iter()
            .filter_map(|p| p.to_str().map(String::from))
            .collect();

        let existing_paths: std::collections::HashSet<String> = if audio_paths.is_empty() {
            std::collections::HashSet::new()
        } else {
            let mut result = std::collections::HashSet::new();
            for chunk in audio_paths.chunks(1000) {
                let placeholders: Vec<String> = chunk.iter().map(|_| "?".to_string()).collect();
                let sql = format!(
                    "SELECT path FROM tracks WHERE path IN ({})",
                    placeholders.join(",")
                );
                let params: Vec<&dyn rusqlite::ToSql> =
                    chunk.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
                let mut stmt = self.conn.prepare(&sql)?;
                stmt.query_map(params.as_slice(), |r| r.get::<_, String>(0))?
                    .filter_map(|r| r.ok())
                    .for_each(|p| {
                        result.insert(p);
                    });
            }
            result
        };

        // Fast insert: just path and filename, no metadata.  Use a transaction for
        // much faster bulk inserts.
        let mut added = 0usize;
        self.conn.execute("BEGIN IMMEDIATE", [])?;
        for path in &audio_paths {
            if !existing_paths.contains(path) {
                let filename = Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                let filetype = Path::new(path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase());
                self.conn.execute(
                    "INSERT INTO tracks (path, folder_id, filename, filetype, play_count)
                     VALUES (?1, ?2, ?3, ?4, 0)
                     ON CONFLICT(path) DO NOTHING",
                    params![path, folder_id, filename, filetype],
                )?;
                added += 1;
            }
        }
        self.conn.execute("COMMIT", [])?;

        // Upsert .m3u8 / .m3u playlists.  Use ON CONFLICT … DO UPDATE
        // (not INSERT OR REPLACE) so the row's id is preserved across
        // rescans — REPLACE deletes + re-inserts, churning the id and
        // invalidating any UI that captured the old value.
        for m3u in &m3u_files {
            if let Some(name) = m3u.file_stem().and_then(|s| s.to_str()) {
                let p = m3u.to_string_lossy();
                self.conn.execute(
                    "INSERT INTO playlists (path, folder_id, name)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(path) DO UPDATE SET
                         folder_id = excluded.folder_id,
                         name      = excluded.name",
                    params![p.as_ref(), folder_id, name],
                )?;
            }
        }

        // Remove tracks that no longer exist.
        let mut stmt = self
            .conn
            .prepare("SELECT id, path FROM tracks WHERE folder_id = ?1")?;
        let existing: Vec<(i64, String)> = stmt
            .query_map(params![folder_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        let mut removed = 0usize;
        for (id, path) in existing {
            if !std::path::Path::new(&path).exists() {
                self.conn
                    .execute("DELETE FROM tracks WHERE id = ?1", params![id])?;
                removed += 1;
            }
        }
        Ok((added, removed))
    }

    /// Update metadata (ID3 tags, duration) for tracks in a folder.
    ///
    /// Reports progress via `progress(processed, total)` callback after each track.
    /// Checks `cancel.load(Ordering::Relaxed)` before each track; if true, returns early.
    ///
    /// When `paths` is `None`, queries tracks with missing metadata internally:
    ///   `WHERE folder_id = ?1 AND (artist IS NULL OR length_secs IS NULL)`
    ///
    /// When `paths` is `Some(vec)`, scans only the provided paths.
    ///
    /// This is the slow part - call after rescan_folder_fast in a background thread.
    pub fn rescan_folder_metadata<F>(
        &self,
        folder_id: i64,
        cancel: &AtomicBool,
        mut progress: F,
        paths: Option<Vec<String>>,
    ) -> Result<usize>
    where
        F: FnMut(usize, usize),
    {
        let tracks: Vec<String> = match paths {
            Some(p) => p,
            None => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, path FROM tracks WHERE folder_id = ?1 AND (artist IS NULL OR length_secs IS NULL OR sample_rate IS NULL)"
                )?;
                stmt.query_map(params![folder_id], |row| row.get::<_, String>(1))?
                    .filter_map(|r| r.ok())
                    .collect()
            }
        };

        let total = tracks.len();
        let mut updated = 0usize;
        // Wrap the per-track upserts in one transaction so SQLite syncs once
        // at commit instead of fsyncing per track. On a 30k-track library
        // this is the dominant scan cost; partial work is committed even on
        // user cancel so progress isn't lost.
        self.conn.execute("BEGIN IMMEDIATE", [])?;
        let mut cancelled = false;
        for path in tracks {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            if self.upsert_track(folder_id, &path).is_ok() {
                let _ = self.update_last_scanned(&path);
                updated += 1;
            }
            progress(updated, total);
        }
        let _ = self.conn.execute("COMMIT", []);
        if cancelled {
            return Ok(updated);
        }
        Ok(updated)
    }

    /// Recursively walk `dir`, partitioning entries into audio files
    /// (`audio_files`) and M3U playlists (`m3u_files`).
    ///
    /// Errors reading a directory are silently skipped so one permission
    /// problem does not abort the whole scan.
    pub(super) fn walk_dir(
        dir: &Path,
        audio_exts: &[&str],
        audio_files: &mut Vec<PathBuf>,
        m3u_files: &mut Vec<PathBuf>,
    ) {
        let read_dir = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return,
        };

        let mut entries: Vec<PathBuf> = read_dir.filter_map(|e| e.ok().map(|e| e.path())).collect();
        // Sort for deterministic ordering across runs.
        entries.sort_unstable_by(|a, b| a.file_name().cmp(&b.file_name()));

        for path in entries {
            if path.is_dir() {
                Self::walk_dir(&path, audio_exts, audio_files, m3u_files);
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let lower = ext.to_lowercase();
                if lower == "m3u" || lower == "m3u8" {
                    m3u_files.push(path);
                } else if audio_exts.contains(&lower.as_str()) {
                    audio_files.push(path);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    /// Insert or replace a single track's metadata in the DB.
    ///
    /// Reads ID3 tags (MP3) or Symphonia metadata (other formats), then
    /// probes the file duration via Symphonia.  Uses `INSERT OR REPLACE` so
    /// re-scanning an already-indexed file refreshes its metadata.
    pub(super) fn upsert_track(&self, folder_id: i64, path: &str) -> Result<()> {
        let p = Path::new(path);

        // Derive filename and filetype from the path.
        let filename = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let filetype = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());

        // Try ID3 first (MP3 and some other formats).  Fall back to Symphonia.
        let tags = read_track_tags(p);

        // Probe duration: Symphonia fast-path, then GStreamer Discoverer fallback
        // for CBR MP3 and formats Symphonia can't measure from headers alone.
        let length_secs = crate::duration_probe::probe_duration(p)
            .or_else(|| crate::duration_probe::discover_duration(p))
            .map(|d| d.as_secs_f64());

        // Technical columns: codec header (sample rate / channels), file
        // size and mtime from the filesystem, average bitrate derived from
        // size ÷ duration, and MP3 VBR/CBR mode sniffed from the Xing/Info
        // header. All degrade to NULL on error rather than failing the scan.
        let tech = crate::technical_probe::probe_technical(p);
        let fs_meta = std::fs::metadata(p).ok();
        let file_size = fs_meta.as_ref().map(|m| m.len() as i64);
        let file_mtime = fs_meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .map(crate::timeutil::format_system_time);
        let bitrate = file_size
            .zip(length_secs)
            .and_then(|(sz, len)| crate::technical_probe::avg_bitrate_kbps(sz as u64, len));
        let channels = tech.channels.or(tags.channels);
        let bitrate_mode = crate::technical_probe::mp3_bitrate_mode(p).map(str::to_string);
        // added_at is INSERT-only (see ON CONFLICT below): this value is only
        // ever used for a row's first insert, never to overwrite it.
        let now = crate::timeutil::format_current_timestamp();

        // Keep existing play_count and last_played if the row already exists.
        self.conn.execute(
            "INSERT INTO tracks
                (path, folder_id, artist, title, album, track_num, genre, year,
                 bpm, length_secs, bitrate, channels, filetype, filename,
                 play_count, last_played,
                 comment, album_artist, disc_num, disc_total, composer, original_artist,
                 copyright, url, encoded_by, lyric, artwork_path,
                 sample_rate, file_size, file_mtime, added_at, bitrate_mode)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    0, NULL,
                    ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25,
                    ?26, ?27, ?28, ?29, ?30)
             ON CONFLICT(path) DO UPDATE SET
                folder_id       = excluded.folder_id,
                artist          = excluded.artist,
                title           = excluded.title,
                album           = excluded.album,
                track_num       = excluded.track_num,
                genre           = excluded.genre,
                year            = excluded.year,
                bpm             = excluded.bpm,
                length_secs     = excluded.length_secs,
                bitrate         = excluded.bitrate,
                channels        = excluded.channels,
                filetype        = excluded.filetype,
                filename        = excluded.filename,
                comment         = excluded.comment,
                album_artist    = excluded.album_artist,
                disc_num        = excluded.disc_num,
                disc_total      = excluded.disc_total,
                composer        = excluded.composer,
                original_artist = excluded.original_artist,
                copyright       = excluded.copyright,
                url             = excluded.url,
                encoded_by      = excluded.encoded_by,
                lyric           = excluded.lyric,
                artwork_path    = excluded.artwork_path,
                sample_rate     = excluded.sample_rate,
                file_size       = excluded.file_size,
                file_mtime      = excluded.file_mtime,
                bitrate_mode    = excluded.bitrate_mode",
            params![
                path,
                folder_id,
                tags.artist,
                tags.title,
                tags.album,
                tags.track_num,
                tags.genre,
                tags.year,
                tags.bpm,
                length_secs,
                bitrate,
                channels,
                filetype,
                filename,
                tags.comment,
                tags.album_artist,
                tags.disc_num,
                tags.disc_total,
                tags.composer,
                tags.original_artist,
                tags.copyright,
                tags.url,
                tags.encoded_by,
                tags.lyric,
                tags.artwork_path,
                tech.sample_rate,
                file_size,
                file_mtime,
                now,
                bitrate_mode,
            ],
        )?;
        // This WAS a full scan (tags + duration read above), so stamp it.
        // Without the stamp, freshly imported rows (ripped CDs, drag-imports)
        // keep a NULL last_scanned and wear the "not yet scanned" clock icon
        // until some later folder rescan happens to touch them.
        self.update_last_scanned(path)?;
        Ok(())
    }

    /// Force re-read metadata for a specific track, ignoring last_scanned timestamp.
    ///
    /// Always re-scans the file's ID3 tags and duration, then updates last_scanned.
    pub fn rescan_track(&self, path: &str) -> Result<()> {
        let p = Path::new(path);
        if !p.exists() {
            return Ok(());
        }
        let folder_id = self.get_folder_id_for_path(path)?;
        self.upsert_track(folder_id, path)?;
        self.update_last_scanned(path)?;
        Ok(())
    }

    pub(super) fn get_folder_id_for_path(&self, path: &str) -> Result<i64> {
        let folder_id: i64 = self.conn.query_row(
            "SELECT folder_id FROM tracks WHERE path = ?1",
            params![path],
            |row| row.get(0),
        )?;
        Ok(folder_id)
    }

    /// Check if a file needs metadata scanning based on modification time vs last_scanned.
    ///
    /// Returns `true` if:
    /// - `last_scanned` is `None` (never scanned), or
    /// - The file's modification time is newer than `last_scanned`
    pub fn needs_metadata_scan(path: &str, last_scanned: Option<&str>) -> bool {
        let Some(last_scanned) = last_scanned else {
            return true; // Never scanned
        };

        let path = Path::new(path);
        let Ok(metadata) = std::fs::metadata(path) else {
            return true; // File doesn't exist or can't be read
        };

        let Ok(mtime) = metadata.modified() else {
            return true; // Can't get mtime
        };

        let mtime_secs = mtime
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Parse last_scanned (format: YYYY-MM-DDTHH:MM:SSZ)
        // We use second-level precision, so add a 2-second buffer to handle timing
        // edge cases where file mtime and scan timestamp are in the same second.
        if let Some(scanned_secs) = timeutil::parse_iso_timestamp(last_scanned) {
            return mtime_secs > scanned_secs + 2;
        }

        true // If we can't parse the timestamp, rescan
    }

    /// Update the `last_scanned` timestamp for a track.
    pub(super) fn update_last_scanned(&self, path: &str) -> Result<()> {
        let now = timeutil::format_current_timestamp();
        self.conn.execute(
            "UPDATE tracks SET last_scanned = ?1 WHERE path = ?2",
            params![now, path],
        )?;
        Ok(())
    }

    /// Scan a single folder, updating metadata for files that have changed.
    ///
    /// Uses smart skip logic: only rescans files where the file modification time
    /// is newer than the `last_scanned` timestamp. Reports progress via
    /// `progress(current, total)` callback on every iteration.
    ///
    /// Returns `(scanned, skipped, failed)` counts where:
    /// - `scanned`: files that were processed and metadata updated successfully
    /// - `skipped`: files that were checked but didn't need rescanning
    /// - `failed`: files that needed rescanning but the upsert failed
    pub fn scan_folder<F>(
        &self,
        folder_id: i64,
        cancel: &AtomicBool,
        mut progress: F,
    ) -> Result<(usize, usize, usize)>
    where
        F: FnMut(usize, usize),
    {
        // Get all tracks in the folder
        let mut stmt = self
            .conn
            .prepare("SELECT id, path, last_scanned FROM tracks WHERE folder_id = ?1")?;
        let tracks: Vec<(i64, String, Option<String>)> = stmt
            .query_map(params![folder_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let total = tracks.len();

        // Separate tracks into those needing scan and those to skip
        let paths_to_scan: Vec<(i64, String)> = tracks
            .into_iter()
            .filter(|(_, path, last_scanned)| {
                Self::needs_metadata_scan(path, last_scanned.as_deref())
            })
            .map(|(id, path, _)| (id, path))
            .collect();

        let to_scan_count = paths_to_scan.len();
        let mut scanned = 0usize;

        // Process files that need scanning
        for (_, path) in paths_to_scan {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if self.upsert_track(folder_id, &path).is_ok() {
                let _ = self.update_last_scanned(&path);
                scanned += 1;
            }
            progress(scanned, to_scan_count);
        }

        let skipped = total - scanned;

        Ok((scanned, skipped, to_scan_count - scanned))
    }

    /// Reset `last_scanned` to NULL for tracks that have no metadata at all
    /// (both `artist` and `length_secs` are NULL).
    ///
    /// Call this before a full rescan to recover tracks whose previous scan
    /// completed but wrote no metadata (e.g. due to an earlier bug).  After
    /// the reset, `scan_folder` will treat those tracks as never-scanned and
    /// re-read their tags.
    pub fn reset_unscanned_metadata(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET last_scanned = NULL WHERE artist IS NULL AND length_secs IS NULL",
            [],
        )?;
        Ok(())
    }

    /// Scan all watched folders, updating metadata for files that have changed.
    ///
    /// Uses smart skip logic per-folder. Reports progress via
    /// `progress(current, total)` callback on every iteration.
    ///
    /// Returns `(scanned, skipped, failed)` counts across all folders.
    pub fn scan_all_folders<F>(
        &self,
        cancel: &AtomicBool,
        mut progress: F,
    ) -> Result<(usize, usize, usize)>
    where
        F: FnMut(usize, usize),
    {
        let folders = self.list_folders()?;
        let mut total_scanned = 0usize;
        let mut total_skipped = 0usize;
        let mut total_failed = 0usize;

        // First pass: count total files that need scanning (unscanned)
        let mut total_to_scan = 0usize;
        for (folder_id, _) in &folders {
            let mut stmt = self
                .conn
                .prepare("SELECT id, path, last_scanned FROM tracks WHERE folder_id = ?1")?;
            let tracks: Vec<(i64, String, Option<String>)> = stmt
                .query_map(params![*folder_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect();

            total_to_scan += tracks
                .into_iter()
                .filter(|(_, path, last_scanned)| {
                    Self::needs_metadata_scan(path, last_scanned.as_deref())
                })
                .count();
        }

        for (folder_id, _) in folders {
            if cancel.load(Ordering::Relaxed) {
                break;
            }

            let (scanned, skipped, failed) = self.scan_folder(folder_id, cancel, |curr, _| {
                progress(total_scanned + curr, total_to_scan);
            })?;

            total_scanned += scanned;
            total_skipped += skipped;
            total_failed += failed;
        }

        Ok((total_scanned, total_skipped, total_failed))
    }
}
