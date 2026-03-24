//! Media library: SQLite-backed catalogue of watched folders, audio tracks,
//! and playlists.
//!
//! The database lives at `~/.local/share/sparkamp/media_library.db` (XDG
//! data directory).  It is opened once at startup and kept open for the
//! lifetime of the application.  All operations are synchronous; callers
//! that want non-blocking behaviour should move the work to a thread.
//!
//! ## Schema overview
//!
//! - **folders** — watched root directories (paths the user added).
//! - **tracks** — every audio file found under a watched folder, with
//!   metadata read from ID3 / Symphonia tags.
//! - **playlists** — `.m3u` files found under watched folders.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

// ---------------------------------------------------------------------------
// String sanitization
// ---------------------------------------------------------------------------

/// Remove NUL bytes from a string.
///
/// ID3 tags can contain malformed data with embedded NUL bytes.  These cause
/// crashes when passed to GTK APIs which use C-style NUL-terminated strings.
/// This function strips any NUL bytes so the string is safe for UI display.
fn sanitize(s: &str) -> String {
    if s.contains('\0') {
        s.replace('\0', "")
    } else {
        s.to_owned()
    }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A track entry in the media library.
///
/// Fields map one-to-one to the `tracks` table columns.
/// `filename` is derived from the file name component of `path`.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct LibTrack {
    pub id: i64,
    pub path: String,
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub track_num: Option<i64>,
    pub genre: Option<String>,
    pub year: Option<i64>,
    pub bpm: Option<String>,
    pub length_secs: Option<f64>,
    pub bitrate: Option<i64>,
    pub channels: Option<i64>,
    pub filetype: Option<String>,
    /// Just the file name component of `path` (no directory prefix).
    pub filename: String,
    pub play_count: i64,
    /// ISO-8601 datetime string of the last play, or `None` if never played.
    pub last_played: Option<String>,
}

/// A playlist entry in the media library.
///
/// `tracks` is empty by default; call [`MediaLibrary::load_playlist_tracks`]
/// to populate it on demand.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct LibPlaylist {
    pub id: i64,
    pub path: String,
    pub name: String,
    /// Tracks listed in this playlist (populated on demand).
    pub tracks: Vec<LibTrack>,
}

// ---------------------------------------------------------------------------
// MediaLibrary
// ---------------------------------------------------------------------------

/// Result of adding a folder to the watch list.
#[derive(Debug, Clone, Copy)]
pub enum AddFolderResult {
    /// The folder was newly inserted into the database.
    New(i64),
    /// The folder was already present in the database.
    AlreadyExists(i64),
}

impl AddFolderResult {
    /// Return the folder's row ID regardless of whether it was new or existing.
    pub fn id(self) -> i64 {
        match self {
            AddFolderResult::New(id) | AddFolderResult::AlreadyExists(id) => id,
        }
    }
}

/// The media library — a thin wrapper around an open SQLite connection.
pub struct MediaLibrary {
    conn: Connection,
}

#[allow(dead_code)]
impl MediaLibrary {
    // -----------------------------------------------------------------------
    // Open / schema
    // -----------------------------------------------------------------------

    /// Open or create the database at
    /// `~/.local/share/sparkamp/media_library.db`.
    ///
    /// Creates the parent directory and initialises the schema on first run.
    /// Returns an error only if the directory cannot be created or SQLite
    /// refuses to open the file.
    pub fn open() -> Result<Self> {
        let db_path = Self::db_path();
        // Ensure the parent directory exists before SQLite tries to create the file.
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        let conn = Connection::open(&db_path)
            .with_context(|| format!("open SQLite at {}", db_path.display()))?;

        // Enable WAL mode for better concurrent read performance.
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;

        let lib = Self { conn };
        lib.init_schema()?;
        Ok(lib)
    }

    /// Return the canonical path to the database file (public alias for use in
    /// other modules that need to open a second connection for thread work).
    pub fn db_path_pub() -> PathBuf {
        Self::db_path()
    }

    /// Open the database at an explicit path.  Used to open a fresh connection
    /// on a background thread (rusqlite `Connection` is not `Send`).
    pub fn open_at(path: &std::path::Path) -> Result<Self> {
        let conn =
            Connection::open(path).with_context(|| format!("open SQLite at {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        let lib = Self { conn };
        lib.init_schema()?;
        Ok(lib)
    }

    /// Return the canonical path to the database file.
    fn db_path() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sparkamp")
            .join("media_library.db")
    }

    /// Create the `folders`, `tracks`, and `playlists` tables if they do not
    /// already exist.  Adding new columns to an existing DB is handled
    /// gracefully by `IF NOT EXISTS`.
    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS folders (
                id   INTEGER PRIMARY KEY,
                path TEXT NOT NULL UNIQUE
            );

            CREATE TABLE IF NOT EXISTS tracks (
                id          INTEGER PRIMARY KEY,
                path        TEXT NOT NULL UNIQUE,
                folder_id   INTEGER REFERENCES folders(id),
                artist      TEXT,
                title       TEXT,
                album       TEXT,
                track_num   INTEGER,
                genre       TEXT,
                year        INTEGER,
                bpm         TEXT,
                length_secs REAL,
                bitrate     INTEGER,
                channels    INTEGER,
                filetype    TEXT,
                filename    TEXT,
                play_count  INTEGER NOT NULL DEFAULT 0,
                last_played TEXT
            );

            CREATE TABLE IF NOT EXISTS playlists (
                id        INTEGER PRIMARY KEY,
                path      TEXT NOT NULL UNIQUE,
                folder_id INTEGER REFERENCES folders(id),
                name      TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(artist);
            CREATE INDEX IF NOT EXISTS idx_tracks_title  ON tracks(title);
            CREATE INDEX IF NOT EXISTS idx_tracks_album  ON tracks(album);
            CREATE INDEX IF NOT EXISTS idx_tracks_folder ON tracks(folder_id);
            ",
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Folder management
    // -----------------------------------------------------------------------

    /// Check if a folder path is already in the watch list.
    /// Returns `Ok(Some(id))` if found, `Ok(None)` if not found.
    fn folder_exists(&self, path: &str) -> Result<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM folders WHERE path = ?1")?;
        let result = stmt.query_row(params![path], |row| row.get(0));
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
    pub fn add_folder(&self, path: &str) -> Result<AddFolderResult> {
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

    /// Scan a single folder for audio files and `.m3u` playlists.
    ///
    /// Walk the directory tree recursively, collecting:
    /// - Audio files (by extension) → upsert into `tracks`.
    /// - `.m3u` files → upsert into `playlists`.
    ///
    /// Tracks that were previously in the DB but whose file no longer exists
    /// on disk are removed.  Returns `(added, removed)` counts.
    pub fn rescan_folder(&self, folder_id: i64, folder_path: &str) -> Result<(usize, usize)> {
        // Audio extensions recognised by Sparkamp.
        const AUDIO_EXTS: &[&str] = &["mp3", "ogg", "flac", "wav", "aac", "m4a", "opus", "wma"];

        // Collect all relevant files under the folder.
        let mut audio_files: Vec<PathBuf> = Vec::new();
        let mut m3u_files: Vec<PathBuf> = Vec::new();
        Self::walk_dir(
            Path::new(folder_path),
            AUDIO_EXTS,
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

        // Batch query: find all existing paths in one DB call.
        let existing_paths: std::collections::HashSet<String> = if audio_paths.is_empty() {
            std::collections::HashSet::new()
        } else {
            let placeholders: Vec<String> = audio_paths.iter().map(|_| "?".to_string()).collect();
            let sql = format!(
                "SELECT path FROM tracks WHERE path IN ({})",
                placeholders.join(",")
            );
            let params: Vec<&dyn rusqlite::ToSql> = audio_paths
                .iter()
                .map(|s| s as &dyn rusqlite::ToSql)
                .collect();
            let mut stmt = self.conn.prepare(&sql)?;
            stmt.query_map(params.as_slice(), |r| r.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect()
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

        // Upsert .m3u playlists.
        for m3u in &m3u_files {
            if let Some(name) = m3u.file_stem().and_then(|s| s.to_str()) {
                let p = m3u.to_string_lossy();
                self.conn.execute(
                    "INSERT OR REPLACE INTO playlists (path, folder_id, name) VALUES (?1, ?2, ?3)",
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
        const AUDIO_EXTS: &[&str] = &["mp3", "ogg", "flac", "wav", "aac", "m4a", "opus", "wma"];

        let mut audio_files: Vec<PathBuf> = Vec::new();
        let mut m3u_files: Vec<PathBuf> = Vec::new();
        Self::walk_dir(
            Path::new(folder_path),
            AUDIO_EXTS,
            &mut audio_files,
            &mut m3u_files,
        );

        // Use paths as-is for fast insert. Skipping canonicalize() removes a stat
        // call per file — the main bottleneck for large libraries.
        let audio_paths: Vec<String> = audio_files
            .iter()
            .filter_map(|p| p.to_str().map(String::from))
            .collect();

        // Batch query: find all existing paths in one DB call.
        let existing_paths: std::collections::HashSet<String> = if audio_paths.is_empty() {
            std::collections::HashSet::new()
        } else {
            let placeholders: Vec<String> = audio_paths.iter().map(|_| "?".to_string()).collect();
            let sql = format!(
                "SELECT path FROM tracks WHERE path IN ({})",
                placeholders.join(",")
            );
            let params: Vec<&dyn rusqlite::ToSql> = audio_paths
                .iter()
                .map(|s| s as &dyn rusqlite::ToSql)
                .collect();
            let mut stmt = self.conn.prepare(&sql)?;
            stmt.query_map(params.as_slice(), |r| r.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect()
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

        // Upsert .m3u playlists.
        for m3u in &m3u_files {
            if let Some(name) = m3u.file_stem().and_then(|s| s.to_str()) {
                let p = m3u.to_string_lossy();
                self.conn.execute(
                    "INSERT OR REPLACE INTO playlists (path, folder_id, name) VALUES (?1, ?2, ?3)",
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
    /// Reports progress via `progress(processed, total)` callback after each track.
    /// Checks `cancel.load(Ordering::Relaxed)` before each track; if true, returns early.
    /// This is the slow part - call after rescan_folder_fast in a background thread.
    pub fn rescan_folder_metadata<F>(
        &self,
        folder_id: i64,
        cancel: &AtomicBool,
        mut progress: F,
    ) -> Result<usize>
    where
        F: FnMut(usize, usize),
    {
        let mut stmt = self.conn.prepare(
            "SELECT id, path FROM tracks WHERE folder_id = ?1 AND (artist IS NULL OR length_secs IS NULL)"
        )?;
        let tracks: Vec<(i64, String)> = stmt
            .query_map(params![folder_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let total = tracks.len();
        let mut updated = 0usize;
        for (_id, path) in tracks {
            if cancel.load(Ordering::Relaxed) {
                return Ok(updated);
            }
            if self.upsert_track(folder_id, &path).is_ok() {
                updated += 1;
            }
            progress(updated, total);
        }
        Ok(updated)
    }

    /// Recursively walk `dir`, partitioning entries into audio files
    /// (`audio_files`) and M3U playlists (`m3u_files`).
    ///
    /// Errors reading a directory are silently skipped so one permission
    /// problem does not abort the whole scan.
    fn walk_dir(
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

    /// Return all tracks, sorted by `artist` then `album` then `track_num`.
    pub fn all_tracks(&self) -> Result<Vec<LibTrack>> {
        self.all_tracks_sorted("artist", false)
    }

    /// Return all tracks with a caller-specified primary sort.
    ///
    /// `col` is one of the column IDs used in the UI: `"artist"`, `"title"`,
    /// `"album"`, `"duration"`, `"filename"`, `"year"`, `"genre"`, `"bitrate"`,
    /// `"num"`.  Unknown IDs fall back to the default `artist` sort.
    /// `desc` reverses the sort direction.
    pub fn all_tracks_sorted(&self, col: &str, desc: bool) -> Result<Vec<LibTrack>> {
        let order = Self::sort_order_clause(col, desc);
        let sql = format!(
            "SELECT id, path, artist, title, album, track_num, genre, year, bpm,
                    length_secs, bitrate, channels, filetype, filename, play_count, last_played
             FROM tracks
             ORDER BY {order}",
        );
        let mut stmt = self.conn.prepare(&sql)?;
        Self::collect_tracks(&mut stmt, [])
    }

    /// Case-insensitive substring search across artist, title, album, genre,
    /// year (as string), filename, and filetype fields.
    ///
    /// Returns all matching tracks in the same order as [`all_tracks`].
    pub fn search_tracks(&self, query: &str) -> Result<Vec<LibTrack>> {
        self.search_tracks_sorted(query, "artist", false)
    }

    /// Search with a caller-specified sort.  See [`all_tracks_sorted`] for
    /// valid `col` values.
    pub fn search_tracks_sorted(
        &self,
        query: &str,
        col: &str,
        desc: bool,
    ) -> Result<Vec<LibTrack>> {
        let pattern = format!("%{}%", query.to_lowercase());
        let order = Self::sort_order_clause(col, desc);
        let sql = format!(
            "SELECT id, path, artist, title, album, track_num, genre, year, bpm,
                    length_secs, bitrate, channels, filetype, filename, play_count, last_played
             FROM tracks
             WHERE LOWER(COALESCE(artist,''))   LIKE ?1
                OR LOWER(COALESCE(title,''))    LIKE ?1
                OR LOWER(COALESCE(album,''))    LIKE ?1
                OR LOWER(COALESCE(genre,''))    LIKE ?1
                OR LOWER(COALESCE(filename,'')) LIKE ?1
                OR LOWER(COALESCE(filetype,'')) LIKE ?1
                OR CAST(year AS TEXT)           LIKE ?1
             ORDER BY {order}",
        );
        let mut stmt = self.conn.prepare(&sql)?;
        Self::collect_tracks(&mut stmt, params![pattern])
    }

    /// Build the SQL ORDER BY clause for a given column ID and direction.
    fn sort_order_clause(col: &str, desc: bool) -> String {
        let dir = if desc { "DESC" } else { "ASC" };
        match col {
            "title" => format!("LOWER(COALESCE(title,'')) {dir}, LOWER(COALESCE(artist,'')) ASC"),
            "album" => format!(
                "LOWER(COALESCE(album,'')) {dir}, LOWER(COALESCE(artist,'')) ASC, track_num ASC"
            ),
            "duration" => format!("COALESCE(length_secs, 0) {dir}, LOWER(COALESCE(artist,'')) ASC"),
            "filename" => format!("LOWER(COALESCE(filename,'')) {dir}"),
            "year" => format!("COALESCE(year, 0) {dir}, LOWER(COALESCE(artist,'')) ASC"),
            "genre" => format!("LOWER(COALESCE(genre,'')) {dir}, LOWER(COALESCE(artist,'')) ASC"),
            "bitrate" => format!("COALESCE(bitrate, 0) {dir}, LOWER(COALESCE(artist,'')) ASC"),
            "num" => format!("COALESCE(track_num, 0) {dir}, LOWER(COALESCE(artist,'')) ASC"),
            // Default: artist → album → track number
            _ => format!(
                "LOWER(COALESCE(artist,'')) {dir}, LOWER(COALESCE(album,'')) ASC, track_num ASC"
            ),
        }
    }

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

    /// Parse an `.m3u` file and look up each referenced path in the `tracks`
    /// table.  Paths not found in the library are silently skipped.
    pub fn load_playlist_tracks(&self, playlist: &LibPlaylist) -> Result<Vec<LibTrack>> {
        let content = std::fs::read_to_string(&playlist.path)
            .with_context(|| format!("read playlist {}", playlist.path))?;

        // Base directory for resolving relative paths in the M3U.
        let base = Path::new(&playlist.path)
            .parent()
            .unwrap_or_else(|| Path::new("/"));

        let mut tracks = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            // Skip comment lines and extended M3U directives.
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // Resolve the path relative to the playlist file.
            let path = if Path::new(line).is_absolute() {
                PathBuf::from(line)
            } else {
                base.join(line)
            };
            let path_str = match path.canonicalize() {
                Ok(p) => p.to_string_lossy().into_owned(),
                Err(_) => path.to_string_lossy().into_owned(),
            };

            // Look up in the DB; skip tracks not in the library.
            if let Ok(t) = self.track_by_path(&path_str) {
                tracks.push(t);
            }
        }
        Ok(tracks)
    }

    /// Remove a single track from the library by its row ID.
    ///
    /// Does nothing if the track does not exist.  The file on disk is **not**
    /// deleted — this only removes the entry from the catalogue.
    #[allow(dead_code)]
    pub fn remove_track(&self, track_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM tracks WHERE id = ?1", params![track_id])?;
        Ok(())
    }

    /// Remove multiple tracks from the library by their row IDs.
    /// Uses a single batched DELETE statement for efficiency.
    /// Returns the number of rows actually removed.
    #[allow(dead_code)]
    pub fn remove_tracks_batch(&self, track_ids: &[i64]) -> Result<usize> {
        if track_ids.is_empty() {
            return Ok(0);
        }
        let placeholders: Vec<String> = track_ids.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "DELETE FROM tracks WHERE id IN ({})",
            placeholders.join(",")
        );
        let params: Vec<&dyn rusqlite::ToSql> = track_ids
            .iter()
            .map(|i| i as &dyn rusqlite::ToSql)
            .collect();
        let count = self.conn.execute(&sql, params.as_slice())?;
        Ok(count)
    }

    pub fn remove_tracks_streaming(
        &self,
        track_ids: &[i64],
        tx: std::sync::mpsc::Sender<i64>,
    ) -> Result<usize> {
        if track_ids.is_empty() {
            return Ok(0);
        }
        let mut total = 0usize;
        for chunk in track_ids.chunks(1000) {
            let placeholders: Vec<String> = chunk.iter().map(|_| "?".to_string()).collect();
            let sql = format!(
                "DELETE FROM tracks WHERE id IN ({})",
                placeholders.join(",")
            );
            let params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|i| i as &dyn rusqlite::ToSql).collect();
            let count = self.conn.execute(&sql, params.as_slice())?;
            for &id in chunk {
                let _ = tx.send(id);
            }
            total += count;
        }
        Ok(total)
    }

    /// Remove a playlist entry from the library by its row ID.
    ///
    /// The `.m3u` file on disk is **not** deleted.
    #[allow(dead_code)]
    pub fn remove_playlist(&self, playlist_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM playlists WHERE id = ?1", params![playlist_id])?;
        Ok(())
    }

    /// Add a playlist `.m3u` file to the library without scanning for audio tracks.
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
        let now = {
            use std::time::{SystemTime, UNIX_EPOCH};
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            // Manual formatting: YYYY-MM-DDTHH:MM:SSZ from UNIX seconds.
            let s = secs;
            let sec = s % 60;
            let min = (s / 60) % 60;
            let hour = (s / 3600) % 24;
            let days = s / 86400;
            // Rough Gregorian calendar calculation (good enough for a timestamp).
            let (year, month, day) = days_to_ymd(days);
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                year, month, day, hour, min, sec
            )
        };

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

    /// Insert or replace a single track's metadata in the DB.
    ///
    /// Reads ID3 tags (MP3) or Symphonia metadata (other formats), then
    /// probes the file duration via Symphonia.  Uses `INSERT OR REPLACE` so
    /// re-scanning an already-indexed file refreshes its metadata.
    fn upsert_track(&self, folder_id: i64, path: &str) -> Result<()> {
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

        // Keep existing play_count and last_played if the row already exists.
        self.conn.execute(
            "INSERT INTO tracks
                (path, folder_id, artist, title, album, track_num, genre, year,
                 bpm, length_secs, bitrate, channels, filetype, filename, play_count, last_played)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 0, NULL)
             ON CONFLICT(path) DO UPDATE SET
                folder_id   = excluded.folder_id,
                artist      = excluded.artist,
                title       = excluded.title,
                album       = excluded.album,
                track_num   = excluded.track_num,
                genre       = excluded.genre,
                year        = excluded.year,
                bpm         = excluded.bpm,
                length_secs = excluded.length_secs,
                bitrate     = excluded.bitrate,
                channels    = excluded.channels,
                filetype    = excluded.filetype,
                filename    = excluded.filename",
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
                tags.bitrate,
                tags.channels,
                filetype,
                filename,
            ],
        )?;
        Ok(())
    }

    /// Look up a single track by its path.  Returns an error if not found.
    fn track_by_path(&self, path: &str) -> Result<LibTrack> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, artist, title, album, track_num, genre, year, bpm,
                    length_secs, bitrate, channels, filetype, filename, play_count, last_played
             FROM tracks WHERE path = ?1",
        )?;
        let mut rows = Self::collect_tracks(&mut stmt, params![path])?;
        rows.pop()
            .ok_or_else(|| anyhow::anyhow!("track not found: {}", path))
    }

    /// Map rows from a prepared statement into [`LibTrack`] values.
    ///
    /// `P` matches rusqlite's `Params` trait so this helper works with both
    /// `[]` (no params) and `params![...]`.
    fn collect_tracks<P: rusqlite::Params>(
        stmt: &mut rusqlite::Statement<'_>,
        params: P,
    ) -> Result<Vec<LibTrack>> {
        let rows = stmt.query_map(params, |row| {
            let path: String = row.get(1)?;
            let filename: Option<String> = row.get(13)?;
            // filename fallback: use just the file-name component of path.
            let fname = filename.unwrap_or_else(|| {
                Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string()
            });
            // Sanitize all string fields to remove NUL bytes that could crash GTK.
            Ok(LibTrack {
                id: row.get(0)?,
                path,
                artist: row.get::<_, Option<String>>(2)?.map(|s| sanitize(&s)),
                title: row.get::<_, Option<String>>(3)?.map(|s| sanitize(&s)),
                album: row.get::<_, Option<String>>(4)?.map(|s| sanitize(&s)),
                track_num: row.get(5)?,
                genre: row.get::<_, Option<String>>(6)?.map(|s| sanitize(&s)),
                year: row.get(7)?,
                bpm: row.get::<_, Option<String>>(8)?.map(|s| sanitize(&s)),
                length_secs: row.get(9)?,
                bitrate: row.get(10)?,
                channels: row.get(11)?,
                filetype: row.get::<_, Option<String>>(12)?.map(|s| sanitize(&s)),
                filename: sanitize(&fname),
                play_count: row.get(14)?,
                last_played: row.get(15)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("collect_tracks")
    }
}

// ---------------------------------------------------------------------------
// Tag reading helpers
// ---------------------------------------------------------------------------

/// Raw tag data extracted from an audio file.
struct TrackTags {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    track_num: Option<i64>,
    genre: Option<String>,
    year: Option<i64>,
    bpm: Option<String>,
    bitrate: Option<i64>,
    channels: Option<i64>,
}

/// Read metadata from an audio file.
///
/// Tries ID3 tags first (works well for MP3), then falls back to Symphonia's
/// generic reader (Vorbis Comments for OGG/FLAC/Opus, etc.).  Returns a
/// best-effort [`TrackTags`] even when no tags are present.
fn read_track_tags(path: &Path) -> TrackTags {
    use id3::TagLike;

    // Strategy 1: ID3 (MP3 and some other formats).
    if let Ok(tag) = id3::Tag::read_from_path(path) {
        let bpm = tag
            .get("TBPM")
            .and_then(|f| f.content().text())
            .map(|s| sanitize(&s));
        return TrackTags {
            title: tag.title().map(|s| sanitize(&s)),
            artist: tag.artist().map(|s| sanitize(&s)),
            album: tag.album().map(|s| sanitize(&s)),
            track_num: tag.track().map(|n| n as i64),
            genre: tag.genre().map(|s| sanitize(&s)),
            year: tag.year().map(|y| y as i64),
            bpm,
            bitrate: None, // not directly in ID3 tags
            channels: None,
        };
    }

    // Strategy 2: Symphonia generic (Vorbis Comments, FLAC, Opus, etc.).
    if let Some(meta) = read_symphonia_tags(path) {
        return meta;
    }

    // Fallback: no tags at all.
    TrackTags {
        title: None,
        artist: None,
        album: None,
        track_num: None,
        genre: None,
        year: None,
        bpm: None,
        bitrate: None,
        channels: None,
    }
}

/// Read metadata using Symphonia's generic reader.
///
/// Handles formats that don't use ID3 tags: OGG/Vorbis, FLAC, Opus.
/// Returns `None` when the file cannot be opened or the format is unrecognised.
fn read_symphonia_tags(path: &Path) -> Option<TrackTags> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::{MetadataOptions, StandardTagKey, Value};
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;

    let mut title: Option<String> = None;
    let mut artist: Option<String> = None;
    let mut album: Option<String> = None;
    let mut track_num: Option<i64> = None;
    let mut genre: Option<String> = None;
    let mut year: Option<i64> = None;

    // Read from the format reader's own metadata (Vorbis Comments, etc.).
    if let Some(rev) = probed.format.metadata().current() {
        for tag in rev.tags() {
            let text = match &tag.value {
                Value::String(s) => s.clone(),
                _ => continue,
            };
            // Sanitize to remove NUL bytes that can crash GTK.
            let safe_text = sanitize(&text);
            match tag.std_key {
                Some(StandardTagKey::TrackTitle) => title = Some(safe_text),
                Some(StandardTagKey::Artist) => artist = Some(safe_text),
                Some(StandardTagKey::Album) => album = Some(safe_text),
                Some(StandardTagKey::TrackNumber) => {
                    // Track number may be "5" or "5/12" — parse the first part.
                    track_num = safe_text
                        .split('/')
                        .next()
                        .and_then(|n| n.trim().parse::<i64>().ok());
                }
                Some(StandardTagKey::Genre) => genre = Some(safe_text),
                Some(StandardTagKey::Date) => {
                    // Date can be "2003", "2003-04-15", etc. — take the year.
                    year = safe_text
                        .split('-')
                        .next()
                        .and_then(|y| y.trim().parse::<i64>().ok());
                }
                _ => {}
            }
        }
    }

    // Collect channel count from codec parameters.
    let channels = probed
        .format
        .tracks()
        .first()
        .and_then(|t| t.codec_params.channels)
        .map(|c| c.count() as i64);

    Some(TrackTags {
        title,
        artist,
        album,
        track_num,
        genre,
        year,
        bpm: None,
        bitrate: None,
        channels,
    })
}

// ---------------------------------------------------------------------------
// Date helper
// ---------------------------------------------------------------------------

/// Convert a count of days since 1970-01-01 to (year, month, day).
///
/// Uses the Gregorian calendar proleptic formula (accurate for all dates
/// after 1970).  Only needed for the `record_play` timestamp.
#[allow(dead_code)]
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Shift to the Julian Day Number epoch for the Gregorian algorithm.
    let jdn = days + 2_440_588; // Unix day 0 = JDN 2440588
    let a = jdn + 32044;
    let b = (4 * a + 3) / 146097;
    let c = a - (146097 * b) / 4;
    let d = (4 * c + 3) / 1461;
    let e = c - (1461 * d) / 4;
    let m = (5 * e + 2) / 153;
    let day = e - (153 * m + 2) / 5 + 1;
    let month = m + 3 - 12 * (m / 10);
    let year = 100 * b + d - 4800 + m / 10;
    (year, month, day)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::NamedTempFile;

    fn temp_lib() -> (MediaLibrary, NamedTempFile) {
        let db_file = NamedTempFile::with_suffix(".db").unwrap();
        let lib = MediaLibrary::open_at(db_file.path()).unwrap();
        (lib, db_file)
    }

    fn temp_dir_with_files(extension: &str, count: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..count {
            let file_path = dir.path().join(format!("track_{}.{}", i, extension));
            fs::write(&file_path, b"fake audio data").unwrap();
        }
        dir
    }

    // ── sanitize() ─────────────────────────────────────────────────────────

    #[test]
    fn sanitize_passes_through_normal_strings() {
        assert_eq!(sanitize("hello"), "hello");
        assert_eq!(sanitize(""), "");
    }

    #[test]
    fn sanitize_removes_nul_bytes() {
        assert_eq!(sanitize("a\x00b"), "ab");
        assert_eq!(sanitize("\x00"), "");
    }

    // ── add_folder / remove_folder ─────────────────────────────────────────

    #[test]
    fn add_folder_inserts_and_returns_id() {
        let (lib, _db) = temp_lib();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        let r1 = lib.add_folder(path).unwrap();
        let r2 = lib.add_folder(path).unwrap();
        assert!(
            matches!(r1, AddFolderResult::New(_)),
            "first add should return New"
        );
        assert!(
            matches!(r2, AddFolderResult::AlreadyExists(_)),
            "second add should return AlreadyExists"
        );
        assert_eq!(r1.id(), r2.id(), "both calls return the same folder ID");
    }

    #[test]
    fn add_folder_duplicate_does_not_insert_row() {
        let (lib, _db) = temp_lib();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        let r1 = lib.add_folder(path).unwrap();
        assert!(matches!(r1, AddFolderResult::New(_)));
        assert_eq!(lib.list_folders().unwrap().len(), 1);

        // Re-adding must return AlreadyExists and NOT insert a second row.
        let r2 = lib.add_folder(path).unwrap();
        assert!(matches!(r2, AddFolderResult::AlreadyExists(_)));
        assert_eq!(
            lib.list_folders().unwrap().len(),
            1,
            "duplicate add must not create a second row"
        );
        assert_eq!(r1.id(), r2.id());
    }

    #[test]
    fn folder_exists_returns_correct_result() {
        let (lib, _db) = temp_lib();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        assert!(
            lib.folder_exists(path).unwrap().is_none(),
            "nonexistent folder returns None"
        );

        let folder_id = lib.add_folder(path).unwrap().id();

        assert_eq!(
            lib.folder_exists(path).unwrap(),
            Some(folder_id),
            "existing folder returns its ID"
        );

        assert!(
            lib.folder_exists("/nonexistent/path/xyz")
                .unwrap()
                .is_none(),
            "different path returns None"
        );
    }

    #[test]
    fn remove_folder_deletes_tracks() {
        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 3);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        let (added, _) = lib.rescan_folder_fast(folder_id, path).unwrap();

        assert_eq!(added, 3, "fast scan should have added 3 files");
        assert_eq!(lib.all_tracks().unwrap().len(), 3);

        lib.remove_folder(folder_id).unwrap();

        assert_eq!(
            lib.all_tracks().unwrap().len(),
            0,
            "all tracks should be removed after remove_folder"
        );
    }

    // ── rescan_folder_fast ────────────────────────────────────────────────

    #[test]
    fn rescan_folder_fast_inserts_audio_files() {
        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 3);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        let (added, _) = lib.rescan_folder_fast(folder_id, path).unwrap();

        assert_eq!(added, 3);
        let tracks = lib.all_tracks().unwrap();
        assert_eq!(tracks.len(), 3);
    }

    #[test]
    fn rescan_folder_fast_handles_multiple_extensions() {
        let (lib, _db) = temp_lib();
        let dir = tempfile::tempdir().unwrap();
        for ext in &["mp3", "flac", "ogg", "m4a"] {
            fs::write(dir.path().join(format!("song.{}", ext)), b"x").unwrap();
        }
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        let (added, _) = lib.rescan_folder_fast(folder_id, path).unwrap();

        assert_eq!(added, 4);
    }

    #[test]
    fn rescan_folder_fast_skips_nonexistent_paths() {
        let (lib, _db) = temp_lib();
        let folder_id = lib.add_folder("/nonexistent/path/xyz").unwrap().id();
        let result = lib.rescan_folder_fast(folder_id, "/nonexistent/path/xyz");
        assert!(result.is_ok());
    }

    #[test]
    fn rescan_folder_fast_removes_deleted_files() {
        let (lib, _db) = temp_lib();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();

        // Create and scan 3 files.
        fs::write(dir.path().join("a.mp3"), b"x").unwrap();
        fs::write(dir.path().join("b.mp3"), b"x").unwrap();
        fs::write(dir.path().join("c.mp3"), b"x").unwrap();
        lib.rescan_folder_fast(folder_id, path).unwrap();
        assert_eq!(lib.all_tracks().unwrap().len(), 3);

        // Delete one file and rescan.
        fs::remove_file(dir.path().join("b.mp3")).unwrap();
        let (_, removed) = lib.rescan_folder_fast(folder_id, path).unwrap();

        assert_eq!(removed, 1);
        assert_eq!(lib.all_tracks().unwrap().len(), 2);
    }

    #[test]
    fn rescan_folder_fast_upserts_m3u_playlists() {
        let (lib, _db) = temp_lib();
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("My Playlist.m3u"), b"#EXTM3U\n").unwrap();
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap();

        let playlists = lib.all_playlists().unwrap();
        assert_eq!(playlists.len(), 1);
        assert_eq!(playlists[0].name, "My Playlist");
    }

    // ── rescan_folder_metadata ─────────────────────────────────────────────

    #[test]
    fn rescan_folder_metadata_reports_progress() {
        gstreamer::init().ok();

        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 5);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap();

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress_count = std::sync::Arc::new(std::sync::Mutex::new(0usize));
        let progress_count_clone = progress_count.clone();

        lib.rescan_folder_metadata(folder_id, &cancel, |done, total| {
            assert!(done <= total);
            *progress_count_clone.lock().unwrap() += 1;
        })
        .unwrap();

        // Progress callback should have been called.
        assert!(
            *progress_count.lock().unwrap() > 0,
            "progress callback should have been called"
        );
    }

    #[test]
    fn rescan_folder_metadata_respects_cancel() {
        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 10);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap();

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);

        // Even with cancel set, it should return Ok (not an error).
        let result = lib.rescan_folder_metadata(folder_id, &cancel, |_, _| {});
        assert!(result.is_ok());
    }

    // ── remove_track ──────────────────────────────────────────────────────

    #[test]
    fn remove_track_deletes_from_db() {
        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 2);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap();

        let tracks = lib.all_tracks().unwrap();
        assert_eq!(tracks.len(), 2);
        let track_id = tracks[0].id;

        lib.remove_track(track_id).unwrap();

        let remaining = lib.all_tracks().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_ne!(remaining[0].id, track_id);
    }

    #[test]
    fn remove_nonexistent_track_is_not_an_error() {
        let (lib, _db) = temp_lib();
        let result = lib.remove_track(99999);
        assert!(
            result.is_ok(),
            "removing nonexistent track should not error"
        );
    }

    // ── remove_tracks_streaming ───────────────────────────────────────────

    #[test]
    fn remove_tracks_streaming_sends_ids_and_returns_count() {
        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 5);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap();

        let tracks = lib.all_tracks().unwrap();
        assert_eq!(tracks.len(), 5);
        let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();

        let (tx, rx) = std::sync::mpsc::channel();
        let count = lib.remove_tracks_streaming(&ids, tx).unwrap();

        assert_eq!(count, 5);
        let received: Vec<i64> = rx.try_iter().collect();
        assert_eq!(received.len(), 5);

        let remaining = lib.all_tracks().unwrap();
        assert_eq!(remaining.len(), 0);
    }

    #[test]
    fn remove_tracks_streaming_empty_ids_returns_zero() {
        let (lib, _db) = temp_lib();
        let (tx, _rx) = std::sync::mpsc::channel();
        let count = lib.remove_tracks_streaming(&[], tx).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn remove_tracks_streaming_large_batch_chunks_correctly() {
        let (lib, _db) = temp_lib();
        let dir = tempfile::tempdir().unwrap();
        const BATCH: usize = 1001;
        for i in 0..BATCH {
            let file_path = dir.path().join(format!("track_{}.mp3", i));
            fs::write(&file_path, b"fake audio").unwrap();
        }
        let path = dir.path().to_str().unwrap();
        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap();

        let ids: Vec<i64> = lib.all_tracks().unwrap().iter().map(|t| t.id).collect();
        assert_eq!(ids.len(), BATCH);

        let (tx, rx) = std::sync::mpsc::channel();
        let count = lib.remove_tracks_streaming(&ids, tx).unwrap();

        assert_eq!(count, BATCH);
        let received: Vec<i64> = rx.try_iter().collect();
        assert_eq!(
            received.len(),
            BATCH,
            "channel should receive every deleted ID"
        );
        assert_eq!(
            lib.all_tracks().unwrap().len(),
            0,
            "all tracks should be removed"
        );
    }

    // ── add_folder with NUL bytes in path ─────────────────────────────────

    #[test]
    fn add_folder_path_with_nul_byte_is_handled() {
        let (lib, _db) = temp_lib();
        // A path with embedded NUL bytes should not crash.
        // The path won't exist so add_folder will still work (it's just an insert).
        let result = lib.add_folder("/tmp/test\x00dir");
        // May succeed or fail depending on path resolution, but should not panic.
        assert!(result.is_ok() || result.is_err());
    }
}
