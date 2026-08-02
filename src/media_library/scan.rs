//! Folder management and the two-phase scan pipeline (fast path walk,
//! background tag read), plus single-track rescan helpers.

use anyhow::{Context, Result};
use rusqlite::params;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::model::AUDIO_EXTENSIONS;
use crate::tags::{read_track_tags, TrackTags};
use crate::timeutil;

use super::{AddFolderResult, MediaLibrary};

/// Everything derived from tags/probes for one file, independent of
/// `folder_id` — factored out of `upsert_track` so the folder_id-bearing
/// INSERT path (`upsert_track`) and the folder_id-agnostic UPDATE path
/// (`update_track_metadata_only`, used for tracks outside every watched
/// folder) share one probing pass instead of duplicating the tag/duration/
/// technical-probe calls.
struct ProbedTrackMetadata {
    filename: String,
    filetype: Option<String>,
    tags: TrackTags,
    length_secs: Option<f64>,
    bitrate: Option<i64>,
    channels: Option<i64>,
    bitrate_mode: Option<String>,
    sample_rate: Option<i64>,
    file_size: Option<i64>,
    file_mtime: Option<String>,
    /// Current timestamp, stamped as `added_at` on first insert. On update
    /// it only heals a pre-existing NULL (see the COALESCE at each call
    /// site) — a real `added_at` value is never overwritten.
    now: String,
}

impl ProbedTrackMetadata {
    fn probe(p: &Path) -> Self {
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
        let computed_bitrate = file_size
            .zip(length_secs)
            .and_then(|(sz, len)| crate::technical_probe::avg_bitrate_kbps(sz as u64, len));
        let bitrate = MediaLibrary::resolve_bitrate(computed_bitrate, tags.bitrate);
        let channels = tech.channels.or(tags.channels);
        let bitrate_mode = crate::technical_probe::mp3_bitrate_mode(p).map(str::to_string);
        let now = crate::timeutil::format_current_timestamp();

        Self {
            filename,
            filetype,
            tags,
            length_secs,
            bitrate,
            channels,
            bitrate_mode,
            sample_rate: tech.sample_rate,
            file_size,
            file_mtime,
            now,
        }
    }
}

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

    /// Whether a watched folder should be scanned recursively into its
    /// subdirectories. Defaults to `true` (the column's SQL default) for
    /// every folder added before this setting existed.
    pub fn folder_recurse(&self, folder_id: i64) -> Result<bool> {
        let recurse: i64 = self.conn.query_row(
            "SELECT recurse FROM folders WHERE id = ?1",
            params![folder_id],
            |row| row.get(0),
        )?;
        Ok(recurse != 0)
    }

    /// Set whether a watched folder scans recursively. Takes effect on the
    /// next rescan (fast-path or full).
    pub fn set_folder_recurse(&self, folder_id: i64, recurse: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET recurse = ?1 WHERE id = ?2",
            params![recurse as i64, folder_id],
        )?;
        Ok(())
    }

    /// Find the deepest watched folder whose path is a prefix of `path`
    /// (nested watched folders resolve to the more specific one), given an
    /// already-fetched folder list. Pure lookup — no I/O — so callers that
    /// process many paths (e.g. `add_files_to_library`) can fetch
    /// `list_folders()` once and reuse it instead of requerying per path.
    fn best_matching_folder(path: &str, folders: &[(i64, String)]) -> Option<i64> {
        let mut best: Option<(i64, &str)> = None;
        for (fid, fpath) in folders {
            if path.starts_with(fpath.as_str())
                && (best.is_none() || fpath.len() > best.unwrap().1.len())
            {
                best = Some((*fid, fpath.as_str()));
            }
        }
        best.map(|(fid, _)| fid)
    }

    /// Resolve the folder that owns `path` (deepest watched-folder prefix
    /// match — see [`Self::best_matching_folder`]), or `None` if `path`
    /// lives outside every watched folder. Single-path convenience for
    /// `apply_watch_action`, which handles one filesystem event at a time
    /// so a fresh `list_folders()` query per call is not a hot loop the way
    /// `add_files_to_library`'s per-path resolution would be.
    ///
    /// `pub(crate)` (Phase 8 Task 10 fix wave) so the GTK frontend's
    /// auto-add-played call site can check "is this path already managed
    /// by a watched folder?" before calling `add_played_track` — the
    /// library stores un-canonicalized scan paths while the frontend's
    /// `Track::path` is canonicalized, so an inside-folder path can't be
    /// reliably matched against `add_played_track`'s exact-string dedup
    /// check; skipping the call entirely for inside-folder paths avoids
    /// the duplicate-row risk instead of trying to normalize around it.
    pub(crate) fn owning_folder_id(&self, path: &str) -> Result<Option<i64>> {
        let folders = self.list_folders()?;
        Ok(Self::best_matching_folder(path, &folders))
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
            let Some(folder_id) = Self::best_matching_folder(path, &folders) else {
                continue;
            };
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

    /// Route a filesystem watch event (from `FolderWatcher`) through the
    /// same DB write paths a manual scan uses, so a background fs change
    /// ends up identical to what a Rescan would have produced.
    ///
    /// `Upsert(path)`: resolves the owning folder the same way
    /// `add_files_to_library` does (deepest watched-folder prefix match, or
    /// `None` if `path` sits outside every watched folder — e.g. a file
    /// under some ancestor directory Sparkamp isn't watching). Fast-inserts
    /// the path row if it's not already present (mirrors
    /// `rescan_folder_fast`'s single-row insert), then fills in metadata:
    /// `upsert_track` for a resolved folder, or `update_track_metadata_only`
    /// for the no-folder case, since `upsert_track`'s ON CONFLICT clause
    /// always overwrites folder_id and there is no real id to give it.
    ///
    /// `Remove(path)`: semantics are a deliberate product decision (locked
    /// 2026-07-27), not an oversight. `remove_missing == true` hard-deletes
    /// the row. `remove_missing == false` is a no-op — the row is kept so
    /// temporarily-offline media (unmounted drives, network shares) keeps
    /// its metadata, matching Winamp. This schema has no "mark broken"
    /// state to fall back to; do not invent one here.
    pub fn apply_watch_action(
        &self,
        action: &crate::watch::WatchAction,
        remove_missing: bool,
    ) -> Result<()> {
        use crate::watch::WatchAction;

        match action {
            WatchAction::Upsert(path) => {
                let Some(path) = path.to_str() else {
                    eprintln!(
                        "apply_watch_action: skipping non-UTF8 path: {}",
                        path.to_string_lossy()
                    );
                    return Ok(());
                };
                self.upsert_path(path)
            }
            WatchAction::Remove(path) => {
                if !remove_missing {
                    return Ok(());
                }
                let Some(path) = path.to_str() else {
                    eprintln!(
                        "apply_watch_action: skipping non-UTF8 path: {}",
                        path.to_string_lossy()
                    );
                    return Ok(());
                };
                self.conn
                    .execute("DELETE FROM tracks WHERE path = ?1", params![path])?;
                Ok(())
            }
        }
    }

    /// Shared body for "get this UTF-8 path into the `tracks` table": resolve
    /// the owning watched folder (or `None`), fast-insert the row if it's
    /// not already present, then fill in metadata. Factored out of
    /// `apply_watch_action`'s `Upsert` arm so it has exactly one
    /// implementation, reused by [`Self::add_played_track`] (auto-add on
    /// first play) — a played file must land in the library the same way a
    /// fs watch event would, NULL-folder_id bucket included.
    fn upsert_path(&self, path: &str) -> Result<()> {
        let folder_id = self.owning_folder_id(path)?;

        let filename = Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let filetype = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());
        let now = timeutil::format_current_timestamp();
        self.conn.execute(
            "INSERT INTO tracks (path, folder_id, filename, filetype, play_count, added_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5)
             ON CONFLICT(path) DO NOTHING",
            params![path, folder_id, filename, filetype, now],
        )?;

        match folder_id {
            Some(fid) => self.upsert_track(fid, path)?,
            None => self.update_track_metadata_only(path)?,
        }
        Ok(())
    }

    /// Auto-add-played core method (Phase 8): make sure a file that just
    /// played is in the library. Unconditional — gating on the
    /// `auto_add_played` config setting is the caller's job (frontend
    /// playback call sites, wired in later tasks), not this method's.
    ///
    /// No-ops and returns `Ok(false)` if `path` already has a `tracks` row —
    /// playback of an already-known file must never re-upsert or touch
    /// anything (play-count bumping happens elsewhere, on its own path).
    /// Otherwise inserts the row via [`Self::upsert_path`] (same
    /// folder-resolution rules as `apply_watch_action`'s `Upsert` arm) and
    /// returns `Ok(true)`.
    pub fn add_played_track(&self, path: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tracks WHERE path = ?1",
            params![path],
            |row| row.get(0),
        )?;
        if count > 0 {
            return Ok(false);
        }
        self.upsert_path(path)?;
        Ok(true)
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
    /// `remove_missing` is threaded to every call — see [`rescan_folder`]
    /// for what it gates. Returns the total `(added, removed)` counts
    /// across all folders.
    pub fn rescan_all(&self, remove_missing: bool) -> Result<(usize, usize)> {
        // Snapshot folders first to avoid re-borrowing conn inside the loop.
        let folders = self.list_folders()?;
        let mut total_added = 0usize;
        let mut total_removed = 0usize;
        for (id, path) in folders {
            let (a, r) = self.rescan_folder(id, &path, remove_missing)?;
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
    /// `remove_missing` gates whether tracks previously in the DB but whose
    /// file no longer exists on disk are deleted. USER-DECIDED (2026-07-27):
    /// `false` (the new production default) KEEPS those rows — Winamp
    /// offline-media parity, letting a temporarily-unmounted drive or
    /// removable media come back without losing library metadata. `true`
    /// reproduces the prior unconditional-delete behavior. Returns
    /// `(added, removed)` counts; `removed` is always 0 when the flag is
    /// off.
    pub fn rescan_folder(
        &self,
        folder_id: i64,
        folder_path: &str,
        remove_missing: bool,
    ) -> Result<(usize, usize)> {
        let mut audio_files: Vec<PathBuf> = Vec::new();
        let mut m3u_files: Vec<PathBuf> = Vec::new();
        Self::walk_dir(
            Path::new(folder_path),
            AUDIO_EXTENSIONS,
            &mut audio_files,
            &mut m3u_files,
            self.folder_recurse(folder_id).unwrap_or(true),
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

        // Remove tracks that belong to this folder but whose files no longer
        // exist — gated on remove_missing (see doc comment above): off keeps
        // offline-media rows, so skip the query and DELETE loop entirely.
        let mut removed = 0usize;
        if remove_missing {
            let mut stmt = self
                .conn
                .prepare("SELECT id, path FROM tracks WHERE folder_id = ?1")?;
            let existing: Vec<(i64, String)> = stmt
                .query_map(params![folder_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .filter_map(|r| r.ok())
                .collect();
            for (id, path) in existing {
                if !std::path::Path::new(&path).exists() {
                    self.conn
                        .execute("DELETE FROM tracks WHERE id = ?1", params![id])?;
                    removed += 1;
                }
            }
        }
        Ok((added, removed))
    }

    /// Fast path: insert file paths only (no metadata).
    /// This returns immediately after collecting paths and inserting them into DB.
    /// Call `rescan_folder_metadata` after this to update metadata asynchronously.
    /// `remove_missing` gates the same deletion loop as [`rescan_folder`] —
    /// see its doc comment for the offline-media-parity rationale.
    pub fn rescan_folder_fast(
        &self,
        folder_id: i64,
        folder_path: &str,
        remove_missing: bool,
    ) -> Result<(usize, usize)> {
        let mut audio_files: Vec<PathBuf> = Vec::new();
        let mut m3u_files: Vec<PathBuf> = Vec::new();
        Self::walk_dir(
            Path::new(folder_path),
            AUDIO_EXTENSIONS,
            &mut audio_files,
            &mut m3u_files,
            self.folder_recurse(folder_id).unwrap_or(true),
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
        //
        // added_at is stamped here, not just in upsert_track: this is a file's
        // first sighting (the metadata pass that follows only fills in tags),
        // so "first sighting" IS the date added. One `now` for the whole batch
        // is fine — files discovered in the same fast-scan pass share it.
        let now = timeutil::format_current_timestamp();
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
                    "INSERT INTO tracks (path, folder_id, filename, filetype, play_count, added_at)
                     VALUES (?1, ?2, ?3, ?4, 0, ?5)
                     ON CONFLICT(path) DO NOTHING",
                    params![path, folder_id, filename, filetype, now],
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

        // Remove tracks that no longer exist — gated on remove_missing, same
        // rationale as rescan_folder's identical loop (offline-media parity).
        let mut removed = 0usize;
        if remove_missing {
            let mut stmt = self
                .conn
                .prepare("SELECT id, path FROM tracks WHERE folder_id = ?1")?;
            let existing: Vec<(i64, String)> = stmt
                .query_map(params![folder_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .filter_map(|r| r.ok())
                .collect();
            for (id, path) in existing {
                if !std::path::Path::new(&path).exists() {
                    self.conn
                        .execute("DELETE FROM tracks WHERE id = ?1", params![id])?;
                    removed += 1;
                }
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
    ///   `WHERE folder_id = ?1 AND (artist IS NULL OR length_secs IS NULL OR sample_rate IS NULL)`
    ///
    /// When `paths` is `Some(vec)`, scans only the provided paths.
    ///
    /// Neither GTK nor the mac frontend calls this — the real pipeline is
    /// `rescan_folder_fast` (path-only insert) followed by `scan_all_folders`
    /// → `scan_folder` → `needs_metadata_scan` (mtime smart-skip, with its
    /// own `sample_rate IS NULL` backfill net). This function is exercised
    /// only by its own tests; kept for that coverage and as a "scan just
    /// these known-stale paths" building block should a caller need it.
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

    /// Walk `dir`, partitioning entries into audio files (`audio_files`) and
    /// M3U playlists (`m3u_files`). Descends into subdirectories only when
    /// `recurse` is true — a folder configured non-recursive stops at its
    /// top level.
    ///
    /// Errors reading a directory are silently skipped so one permission
    /// problem does not abort the whole scan.
    pub(super) fn walk_dir(
        dir: &Path,
        audio_exts: &[&str],
        audio_files: &mut Vec<PathBuf>,
        m3u_files: &mut Vec<PathBuf>,
        recurse: bool,
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
                if recurse {
                    Self::walk_dir(&path, audio_exts, audio_files, m3u_files, recurse);
                }
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
        let m = ProbedTrackMetadata::probe(p);

        // Keep existing play_count and last_played if the row already exists.
        self.conn.execute(
            "INSERT INTO tracks
                (path, folder_id, artist, title, album, track_num, genre, year,
                 bpm, length_secs, bitrate, channels, filetype, filename,
                 play_count, last_played,
                 comment, album_artist, disc_num, disc_total, composer, original_artist,
                 copyright, url, encoded_by, lyric, artwork_path,
                 sample_rate, file_size, file_mtime, added_at, bitrate_mode,
                 rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    0, NULL,
                    ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25,
                    ?26, ?27, ?28, ?29, ?30,
                    ?31, ?32, ?33, ?34)
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
                bitrate_mode    = excluded.bitrate_mode,
                added_at        = COALESCE(added_at, excluded.added_at),
                -- ReplayGain is COALESCEd the other way round from the tag
                -- columns above: the file wins when it carries a value, but a
                -- file with no ReplayGain tags must NOT wipe a gain Sparkamp
                -- measured itself (analysis with write-tags off stores to the
                -- DB only, and a later rescan would otherwise erase it).
                rg_track_gain   = COALESCE(excluded.rg_track_gain, rg_track_gain),
                rg_track_peak   = COALESCE(excluded.rg_track_peak, rg_track_peak),
                rg_album_gain   = COALESCE(excluded.rg_album_gain, rg_album_gain),
                rg_album_peak   = COALESCE(excluded.rg_album_peak, rg_album_peak)",
            params![
                path,
                folder_id,
                m.tags.artist,
                m.tags.title,
                m.tags.album,
                m.tags.track_num,
                m.tags.genre,
                m.tags.year,
                m.tags.bpm,
                m.length_secs,
                m.bitrate,
                m.channels,
                m.filetype,
                m.filename,
                m.tags.comment,
                m.tags.album_artist,
                m.tags.disc_num,
                m.tags.disc_total,
                m.tags.composer,
                m.tags.original_artist,
                m.tags.copyright,
                m.tags.url,
                m.tags.encoded_by,
                m.tags.lyric,
                m.tags.artwork_path,
                m.sample_rate,
                m.file_size,
                m.file_mtime,
                m.now,
                m.bitrate_mode,
                m.tags.rg_track_gain,
                m.tags.rg_track_peak,
                m.tags.rg_album_gain,
                m.tags.rg_album_peak,
            ],
        )?;
        // This WAS a full scan (tags + duration read above), so stamp it.
        // Without the stamp, freshly imported rows (ripped CDs, drag-imports)
        // keep a NULL last_scanned and wear the "not yet scanned" clock icon
        // until some later folder rescan happens to touch them.
        self.update_last_scanned(path)?;
        Ok(())
    }

    /// Fill in metadata for a track outside every watched folder (a NULL
    /// `folder_id`), without touching `folder_id` at all — used by
    /// `apply_watch_action` after its NULL-folder fast-insert. Can't reuse
    /// `upsert_track` for this: its ON CONFLICT clause unconditionally sets
    /// `folder_id = excluded.folder_id`, and there is no real folder id to
    /// give it, so we'd either clobber the intended NULL or a legitimate
    /// pre-existing value. Assumes the row already exists (the caller's
    /// fast-insert guarantees this); a no-op if it doesn't.
    fn update_track_metadata_only(&self, path: &str) -> Result<()> {
        let p = Path::new(path);
        let m = ProbedTrackMetadata::probe(p);

        self.conn.execute(
            "UPDATE tracks SET
                artist = ?1, title = ?2, album = ?3, track_num = ?4, genre = ?5, year = ?6,
                bpm = ?7, length_secs = ?8, bitrate = ?9, channels = ?10, filetype = ?11,
                filename = ?12, comment = ?13, album_artist = ?14, disc_num = ?15,
                disc_total = ?16, composer = ?17, original_artist = ?18, copyright = ?19,
                url = ?20, encoded_by = ?21, lyric = ?22, artwork_path = ?23,
                sample_rate = ?24, file_size = ?25, file_mtime = ?26, bitrate_mode = ?27,
                added_at = COALESCE(added_at, ?28),
                -- Same asymmetry as upsert_track: keep a DB-only gain that
                -- Sparkamp measured when the file itself carries no tags.
                rg_track_gain = COALESCE(?30, rg_track_gain),
                rg_track_peak = COALESCE(?31, rg_track_peak),
                rg_album_gain = COALESCE(?32, rg_album_gain),
                rg_album_peak = COALESCE(?33, rg_album_peak)
             WHERE path = ?29",
            params![
                m.tags.artist,
                m.tags.title,
                m.tags.album,
                m.tags.track_num,
                m.tags.genre,
                m.tags.year,
                m.tags.bpm,
                m.length_secs,
                m.bitrate,
                m.channels,
                m.filetype,
                m.filename,
                m.tags.comment,
                m.tags.album_artist,
                m.tags.disc_num,
                m.tags.disc_total,
                m.tags.composer,
                m.tags.original_artist,
                m.tags.copyright,
                m.tags.url,
                m.tags.encoded_by,
                m.tags.lyric,
                m.tags.artwork_path,
                m.sample_rate,
                m.file_size,
                m.file_mtime,
                m.bitrate_mode,
                m.now,
                path,
                m.tags.rg_track_gain,
                m.tags.rg_track_peak,
                m.tags.rg_album_gain,
                m.tags.rg_album_peak,
            ],
        )?;
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
        // NULL-safe folder lookup (Phase 8 review Fix 1): auto-add-played
        // inserts outside-folder rows with folder_id = NULL (see
        // `upsert_path`), so a plain `tracks.folder_id` read here would
        // hit `InvalidColumnType` on such a row and every caller of
        // `rescan_track` silently swallows the Err, leaving stale
        // metadata forever. Mirror `upsert_path`'s branch instead.
        match self.owning_folder_id(path)? {
            Some(fid) => self.upsert_track(fid, path)?,
            None => self.update_track_metadata_only(path)?,
        }
        self.update_last_scanned(path)?;
        Ok(())
    }

    /// Prefer the size÷duration bitrate we computed ourselves; fall back to
    /// whatever the tag reader supplied when we couldn't derive one (e.g.
    /// duration probing failed). Mirrors `tech.channels.or(tags.channels)`.
    pub(super) fn resolve_bitrate(computed: Option<i64>, tag_bitrate: Option<i64>) -> Option<i64> {
        computed.or(tag_bitrate)
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
    /// Uses smart skip logic: rescans files where the file modification time
    /// is newer than the `last_scanned` timestamp, OR where `sample_rate` is
    /// still NULL — a one-time backfill net for rows written before the
    /// technical-columns phase (sample rate, file size/mtime, bitrate mode)
    /// shipped, so the existing library gets those columns filled in on the
    /// next Rescan instead of staying NULL forever. Reports progress via
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
        let mut stmt = self.conn.prepare(
            "SELECT id, path, last_scanned, sample_rate FROM tracks WHERE folder_id = ?1",
        )?;
        let tracks: Vec<(i64, String, Option<String>, Option<i64>)> = stmt
            .query_map(params![folder_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let total = tracks.len();

        // Separate tracks into those needing scan and those to skip
        let paths_to_scan: Vec<(i64, String)> = tracks
            .into_iter()
            .filter(|(_, path, last_scanned, sample_rate)| {
                sample_rate.is_none() || Self::needs_metadata_scan(path, last_scanned.as_deref())
            })
            .map(|(id, path, _, _)| (id, path))
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

        // First pass: count total files that need scanning (unscanned, or
        // still missing the technical-columns backfill — kept in sync with
        // scan_folder's own candidate filter below, or the progress bar's
        // total would undercount and the callback could report done > total).
        let mut total_to_scan = 0usize;
        for (folder_id, _) in &folders {
            let mut stmt = self.conn.prepare(
                "SELECT id, path, last_scanned, sample_rate FROM tracks WHERE folder_id = ?1",
            )?;
            let tracks: Vec<(i64, String, Option<String>, Option<i64>)> = stmt
                .query_map(params![*folder_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect();

            total_to_scan += tracks
                .into_iter()
                .filter(|(_, path, last_scanned, sample_rate)| {
                    sample_rate.is_none()
                        || Self::needs_metadata_scan(path, last_scanned.as_deref())
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

    /// Compact the database file with `VACUUM`, reclaiming space left by
    /// deleted rows (e.g. from remove_missing rescans or folder removal).
    /// Callers should run this after a full rescan when `compact_on_rescan`
    /// is enabled — that wiring is a later task; this method just provides
    /// the operation.
    pub fn compact(&self) -> Result<()> {
        self.conn.execute("VACUUM", [])?;
        Ok(())
    }
}
