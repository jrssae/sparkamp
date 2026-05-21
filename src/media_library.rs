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

use crate::model::AUDIO_EXTENSIONS;

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
    pub comment: Option<String>,
    pub album_artist: Option<String>,
    pub disc_num: Option<i64>,
    pub disc_total: Option<i64>,
    pub composer: Option<String>,
    pub original_artist: Option<String>,
    pub copyright: Option<String>,
    pub url: Option<String>,
    pub encoded_by: Option<String>,
    pub lyric: Option<String>,
    pub artwork_path: Option<String>,
    /// ISO-8601 datetime string of the last metadata scan, or `None` if never scanned.
    pub last_scanned: Option<String>,
    /// Pre-computed lowercase strings and zero-padded numbers for sort comparisons.
    /// All strings are lowercase; all numeric fields are zero-padded so string
    /// comparison gives correct numeric ordering.
    pub sort_keys: SortKeys,
}

/// Single-line display string for a [`LibTrack`] — em-dash separator,
/// matching the macOS `mlTrackDisplay` and the active-playlist row.
///
/// - `"Artist — Title"` when artist is non-empty.
/// - `"AlbumArtist — Title"` when artist is empty but album_artist is set.
/// - Plain `filename` when both are blank.
/// - Title falls back to filename when blank.
#[allow(dead_code)] // GTK-only; out of bin reach on macOS where GTK is gated.
pub fn lib_track_display(t: &LibTrack) -> String {
    let title = t.title.as_deref().unwrap_or(&t.filename);
    if let Some(a) = t.artist.as_deref().filter(|s| !s.is_empty()) {
        format!("{a} — {title}")
    } else if let Some(aa) = t.album_artist.as_deref().filter(|s| !s.is_empty()) {
        format!("{aa} — {title}")
    } else {
        t.filename.clone()
    }
}

/// Pre-computed sort keys for a [`LibTrack`].
/// All strings are lowercase; all numeric fields are zero-padded so string
/// comparison gives correct numeric ordering.
///
/// Fields are read by the GTK frontend's column-sort logic; macOS uses
/// SwiftUI's KeyPathComparator on the live `LibTrack` fields and does not
/// touch these.  Allow dead-code so the bin build stays warning-free on
/// platforms where GTK is gated out.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct SortKeys {
    pub num: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: String,
    pub filename: String,
    pub year: String,
    pub genre: String,
    pub bitrate: String,
    pub album_artist: String,
    pub composer: String,
    pub comment: String,
}

impl SortKeys {
    fn from_track(track: &LibTrack) -> Self {
        SortKeys {
            num: format!("{:010}", track.track_num.unwrap_or(0)),
            title: track
                .title
                .as_deref()
                .unwrap_or(&track.filename)
                .to_lowercase(),
            artist: track.artist.as_deref().unwrap_or("").to_lowercase(),
            album: track.album.as_deref().unwrap_or("").to_lowercase(),
            duration: format!("{:015.3}", track.length_secs.unwrap_or(0.0)),
            filename: track.filename.to_lowercase(),
            year: format!("{:010}", track.year.unwrap_or(0)),
            genre: track.genre.as_deref().unwrap_or("").to_lowercase(),
            bitrate: format!("{:010}", track.bitrate.unwrap_or(0)),
            album_artist: track.album_artist.as_deref().unwrap_or("").to_lowercase(),
            composer: track.composer.as_deref().unwrap_or("").to_lowercase(),
            comment: track.comment.as_deref().unwrap_or("").to_lowercase(),
        }
    }
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
// ReadOnlyTrackFields — formatted display values for the ID3 editor
// ---------------------------------------------------------------------------

/// Read-only file and library metadata for the ID3 editor.
///
/// All values are formatted display strings (e.g., bitrate as "128k",
/// channels as "stereo", duration as "3:45").  Use [`read_only_track_fields`]
/// to populate this struct from a path and optional media library track.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct ReadOnlyTrackFields {
    pub filename: String,
    pub path: String,
    pub filetype: String,
    pub bitrate: String,
    pub channels: String,
    pub duration: String,
    pub play_count: String,
    pub last_played: String,
    pub num: String,
    pub artwork_path: String,
}

/// Compose read-only field values for the ID3 editor, formatted for display.
///
/// `track` may be `None` if the file is not indexed in the media library;
/// in that case all library-derived fields fall back to empty strings.
///
/// Used by the GTK ID3 editor; macOS reads these fields directly off the
/// `MLTrack` struct in Swift.
#[allow(dead_code)]
pub fn read_only_track_fields(
    path: &std::path::Path,
    track: Option<&LibTrack>,
) -> ReadOnlyTrackFields {
    let filename = track.map(|t| t.filename.clone()).unwrap_or_else(|| {
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string()
    });
    let path_str = path.to_string_lossy().into_owned();
    let filetype = track.and_then(|t| t.filetype.clone()).unwrap_or_default();
    let bitrate = track
        .and_then(|t| t.bitrate)
        .map(|b| format!("{b}k"))
        .unwrap_or_default();
    let channels = track
        .and_then(|t| t.channels)
        .map(|c| match c {
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            n => format!("{}ch", n),
        })
        .unwrap_or_default();
    let duration = track
        .and_then(|t| t.length_secs)
        .map(|s| {
            let ss = s as u64;
            format!("{}:{:02}", ss / 60, ss % 60)
        })
        .unwrap_or_else(|| "-:--".to_string());
    let play_count = track.map(|t| t.play_count.to_string()).unwrap_or_default();
    let last_played = track
        .and_then(|t| t.last_played.clone())
        .unwrap_or_default();
    let num = track
        .and_then(|t| t.track_num)
        .map(|n| n.to_string())
        .unwrap_or_default();
    let artwork_path = track
        .and_then(|t| t.artwork_path.clone())
        .unwrap_or_default();

    ReadOnlyTrackFields {
        filename,
        path: path_str,
        filetype,
        bitrate,
        channels,
        duration,
        play_count,
        last_played,
        num,
        artwork_path,
    }
}

/// Check if a file is read-only by attempting to open it for writing.
///
/// Returns `true` if the file cannot be written to (permission denied or read-only filesystem).
/// Returns `false` if the file can be opened for writing, or if an error occurs.
/// This method works reliably for all filesystem types including network shares
/// (SMB/CIFS/NFS) and system-level read-only mounts.
pub fn is_read_only(path: &std::path::Path) -> bool {
    match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(_) => false,
        Err(e) => matches!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
        ),
    }
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
        // Normalize any portal-path duplicates left by earlier versions.
        let _ = lib.dedup_folders();
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
    /// already exist.  Adding new columns to an existing DB is handled by
    /// checking column existence first.
    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS folders (
                id   INTEGER PRIMARY KEY,
                path TEXT NOT NULL UNIQUE
            );

            CREATE TABLE IF NOT EXISTS tracks (
                id              INTEGER PRIMARY KEY,
                path            TEXT NOT NULL UNIQUE,
                folder_id       INTEGER REFERENCES folders(id),
                artist          TEXT,
                title           TEXT,
                album           TEXT,
                track_num       INTEGER,
                genre           TEXT,
                year            INTEGER,
                bpm             TEXT,
                length_secs     REAL,
                bitrate         INTEGER,
                channels        INTEGER,
                filetype        TEXT,
                filename        TEXT,
                play_count      INTEGER NOT NULL DEFAULT 0,
                last_played     TEXT,
                comment         TEXT,
                album_artist    TEXT,
                disc_num        INTEGER,
                disc_total      INTEGER,
                composer        TEXT,
                original_artist TEXT,
                copyright       TEXT,
                url             TEXT,
                encoded_by      TEXT,
                lyric           TEXT,
                artwork_path    TEXT,
                last_scanned   TEXT
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

        let new_cols = [
            ("comment", "TEXT"),
            ("album_artist", "TEXT"),
            ("disc_num", "INTEGER"),
            ("disc_total", "INTEGER"),
            ("composer", "TEXT"),
            ("original_artist", "TEXT"),
            ("copyright", "TEXT"),
            ("url", "TEXT"),
            ("encoded_by", "TEXT"),
            ("lyric", "TEXT"),
            ("artwork_path", "TEXT"),
            ("last_scanned", "TEXT"),
            ("deleted_at", "TEXT"),
        ];
        let existing: std::collections::HashSet<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT name FROM pragma_table_info('tracks')")?;
            stmt.query_map([], |row| row.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect()
        };
        for (col, typ) in new_cols {
            if !existing.contains(col) {
                self.conn.execute(
                    &format!("ALTER TABLE tracks ADD COLUMN {} {}", col, typ),
                    [],
                )?;
            }
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Folder management
    // -----------------------------------------------------------------------

    /// Canonicalize a folder path so `add_folder` and `folder_exists`
    /// agree on the comparison key under symlink indirection (macOS
    /// `/var → /private/var`, Flatpak document-portal FUSE mounts).
    /// Falls back to the raw input when the path does not exist on disk.
    fn canonicalize_folder_path(path: &str) -> String {
        Path::new(path)
            .canonicalize()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_owned())
    }

    /// Check if a folder path is already in the watch list.
    /// Returns `Ok(Some(id))` if found, `Ok(None)` if not found.
    ///
    /// The input is canonicalized before lookup so callers can pass any
    /// equivalent path (with or without symlink resolution) and still get
    /// a hit on a previously-added folder.
    fn folder_exists(&self, path: &str) -> Result<Option<i64>> {
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
    fn dedup_folders(&self) -> Result<()> {
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

    /// Scan a single folder for audio files and `.m3u` playlists.
    ///
    /// Walk the directory tree recursively, collecting:
    /// - Audio files (by extension) → upsert into `tracks`.
    /// - `.m3u` files → upsert into `playlists`.
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
                    "SELECT id, path FROM tracks WHERE folder_id = ?1 AND (artist IS NULL OR length_secs IS NULL)"
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

    /// Return only tracks that have already had their metadata scanned
    /// (`last_scanned IS NOT NULL`), sorted by artist then title.
    ///
    /// Used by the deduplication feature, which cannot make useful comparisons
    /// on entries whose ID3 tags have not been read yet.
    pub fn scanned_tracks(&self) -> Result<Vec<LibTrack>> {
        let sql =
            "SELECT id, path, artist, title, album, track_num, genre, year, bpm,
                    length_secs, bitrate, channels, filetype, filename, play_count, last_played,
                    comment, album_artist, disc_num, disc_total, composer, original_artist,
                    copyright, url, encoded_by, lyric, artwork_path, last_scanned
             FROM tracks
             WHERE last_scanned IS NOT NULL
             ORDER BY LOWER(COALESCE(artist,'')), LOWER(COALESCE(title,''))";
        let mut stmt = self.conn.prepare(sql)?;
        Self::collect_tracks(&mut stmt, [])
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
                    length_secs, bitrate, channels, filetype, filename, play_count, last_played,
                    comment, album_artist, disc_num, disc_total, composer, original_artist,
                    copyright, url, encoded_by, lyric, artwork_path, last_scanned
             FROM tracks
             ORDER BY {order}",
        );
        let mut stmt = self.conn.prepare(&sql)?;
        Self::collect_tracks(&mut stmt, [])
    }

    /// Case-insensitive, word-based substring search.
    ///
    /// Every whitespace-delimited word in `query` must appear in at least one
    /// of: artist, title, album, album_artist, filename, genre, filetype, or
    /// year.  Cross-field queries therefore work — "ed sheeran don't" finds a
    /// track titled "Don't" by "Ed Sheeran" even though no single field
    /// contains the whole query.  An empty query returns an empty result.
    ///
    /// Returns all matching tracks in the default sort order.
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
        // Split into words; empty query returns nothing (consistent with the
        // playlist jump window which also returns nothing for an empty query).
        let words: Vec<String> = query
            .split_whitespace()
            .map(|w| format!("%{}%", w.to_lowercase()))
            .collect();
        if words.is_empty() {
            return Ok(Vec::new());
        }

        let order = Self::sort_order_clause(col, desc);

        // Build one WHERE group per word — each group matches any field, all
        // groups must match (AND).  This produces SQL like:
        //   WHERE (artist LIKE ?1 OR title LIKE ?1 OR ...)
        //     AND (artist LIKE ?2 OR title LIKE ?2 OR ...)
        let word_clauses: String = (1..=words.len())
            .map(|i| {
                format!(
                    "(LOWER(COALESCE(artist,''))        LIKE ?{i}
                      OR LOWER(COALESCE(title,''))       LIKE ?{i}
                      OR LOWER(COALESCE(album,''))        LIKE ?{i}
                      OR LOWER(COALESCE(album_artist,'')) LIKE ?{i}
                      OR LOWER(COALESCE(filename,''))     LIKE ?{i}
                      OR LOWER(COALESCE(genre,''))        LIKE ?{i}
                      OR LOWER(COALESCE(filetype,''))     LIKE ?{i}
                      OR CAST(COALESCE(year,0) AS TEXT)   LIKE ?{i})"
                )
            })
            .collect::<Vec<_>>()
            .join(" AND ");

        let sql = format!(
            "SELECT id, path, artist, title, album, track_num, genre, year, bpm,
                    length_secs, bitrate, channels, filetype, filename, play_count, last_played,
                    comment, album_artist, disc_num, disc_total, composer, original_artist,
                    copyright, url, encoded_by, lyric, artwork_path, last_scanned
             FROM tracks
             WHERE {word_clauses}
             ORDER BY {order}",
        );
        let mut stmt = self.conn.prepare(&sql)?;
        Self::collect_tracks(&mut stmt, rusqlite::params_from_iter(words.iter()))
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
            "play_count" => format!("COALESCE(play_count, 0) {dir}, LOWER(COALESCE(artist,'')) ASC"),
            // last_played sorts NULLs (never played) to the end regardless of direction
            // so users browsing recent activity see real timestamps first.
            "last_played" => format!(
                "CASE WHEN last_played IS NULL OR last_played = '' THEN 1 ELSE 0 END ASC, \
                 last_played {dir}, LOWER(COALESCE(artist,'')) ASC"
            ),
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

    /// Parse an `.m3u` file and return all entries.
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

            // 1. Exact path match in DB.
            if let Ok(t) = self.track_by_path(&path_str) {
                tracks.push(t);
                extinf_title = None;
                extinf_artist = None;
                extinf_secs = None;
                continue;
            }

            // 2. Filename-only fallback (handles tracks moved within the library).
            if let Ok(t) = self.track_by_filename_first(&filename) {
                tracks.push(t);
                extinf_title = None;
                extinf_artist = None;
                extinf_secs = None;
                continue;
            }

            // 3. Not in library — emit a synthetic missing-file stub so the UI
            //    can display it in red rather than hiding it entirely.
            let title  = extinf_title.take();
            let artist = extinf_artist.take();
            let secs   = extinf_secs.take();
            let sort   = SortKeys {
                title:    title.as_deref().unwrap_or(&filename).to_lowercase(),
                artist:   artist.as_deref().unwrap_or("").to_lowercase(),
                filename: filename.to_lowercase(),
                ..SortKeys::default()
            };
            tracks.push(LibTrack {
                id:              0,          // sentinel: not in the DB
                path:            raw_line.to_string(),
                filename,
                title,
                artist,
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
            });
        }
        Ok(tracks)
    }

    /// Return the first track in the library whose `filename` column matches
    /// (case-sensitive).  Used as a fallback when the full path has changed.
    fn track_by_filename_first(&self, filename: &str) -> Result<LibTrack> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, artist, title, album, track_num, genre, year, bpm,
                    length_secs, bitrate, channels, filetype, filename, play_count, last_played,
                    comment, album_artist, disc_num, disc_total, composer, original_artist,
                    copyright, url, encoded_by, lyric, artwork_path, last_scanned
             FROM tracks WHERE filename = ?1 LIMIT 1",
        )?;
        let mut rows = Self::collect_tracks(&mut stmt, params![filename])?;
        rows.pop()
            .ok_or_else(|| anyhow::anyhow!("track not found by filename: {}", filename))
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

    /// Mark tracks as deleted by setting `deleted_at` timestamp.
    /// Processes IDs in chunks of 999 to stay within SQLite's parameter limit.
    /// Used for soft delete before background purge.
    #[allow(dead_code)]
    pub fn soft_delete_tracks(&self, track_ids: &[i64]) -> Result<()> {
        if track_ids.is_empty() {
            return Ok(());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_default();
        for chunk in track_ids.chunks(999) {
            let placeholders: Vec<String> = chunk.iter().map(|_| "?".to_string()).collect();
            let sql = format!(
                "UPDATE tracks SET deleted_at = ?1 WHERE id IN ({})",
                placeholders.join(",")
            );
            let mut params: Vec<&dyn rusqlite::ToSql> = vec![&now];
            params.extend(chunk.iter().map(|i| i as &dyn rusqlite::ToSql));
            self.conn.execute(&sql, params.as_slice())?;
        }
        Ok(())
    }

    /// Mark tracks as deleted by their paths, using batched queries to avoid
    /// SQLite parameter limits. Returns the total number of rows updated.
    #[allow(dead_code)]
    pub fn soft_delete_tracks_by_paths(&self, paths: &[String]) -> Result<usize> {
        if paths.is_empty() {
            return Ok(0);
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_default();
        let mut total = 0usize;
        for chunk in paths.chunks(1000) {
            let placeholders: Vec<String> = chunk.iter().map(|_| "?".to_string()).collect();
            let sql = format!(
                "UPDATE tracks SET deleted_at = ?1 WHERE path IN ({})",
                placeholders.join(",")
            );
            let mut params: Vec<&dyn rusqlite::ToSql> = vec![&now];
            params.extend(chunk.iter().map(|s| s as &dyn rusqlite::ToSql));
            total += self.conn.execute(&sql, params.as_slice())?;
        }
        Ok(total)
    }

    /// Get count of soft-deleted tracks.
    #[allow(dead_code)]
    pub fn get_deleted_track_count(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tracks WHERE deleted_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Purge all soft-deleted tracks from the database.
    /// Called by background cleanup and on application startup.
    #[allow(dead_code)]
    pub fn purge_deleted_tracks(&self) -> Result<usize> {
        let count = self
            .conn
            .execute("DELETE FROM tracks WHERE deleted_at IS NOT NULL", [])?;
        Ok(count)
    }

    /// Cleanup orphaned soft-deleted records on startup.
    /// Logs the count of purged records.
    #[allow(dead_code)]
    pub fn cleanup_on_startup(&self) -> Result<usize> {
        let count = self.get_deleted_track_count()?;
        if count > 0 {
            self.purge_deleted_tracks()?;
        }
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

    /// Create a new empty playlist with `name`.
    ///
    /// Writes an `#EXTM3U` header to
    /// `~/.config/sparkamp/playlists/<name>.m3u` (sanitising the name for
    /// the filesystem) and registers the file in the library database.
    /// Returns the new playlist row id.
    pub fn create_playlist(&self, name: &str) -> Result<i64> {
        let dir = Self::playlists_dir();
        let safe = name
            .chars()
            .map(|c| if r#"/\:*?"<>|"#.contains(c) { '_' } else { c })
            .collect::<String>();
        let safe = if safe.is_empty() { "Untitled".to_string() } else { safe };

        // Avoid clobbering an existing file.
        let mut path = dir.join(format!("{safe}.m3u"));
        let mut counter = 1u32;
        while path.exists() {
            path = dir.join(format!("{safe}_{counter}.m3u"));
            counter += 1;
        }
        std::fs::write(&path, b"#EXTM3U\n")
            .with_context(|| format!("create playlist file {}", path.display()))?;
        self.add_playlist_file(&path.to_string_lossy())
    }

    /// Rename playlist `id`.  Updates both the database record and the `.m3u`
    /// file on disk.
    pub fn rename_playlist(&self, id: i64, new_name: &str) -> Result<()> {
        let pl = self.playlist_by_id(id)?;
        let old_path = Path::new(&pl.path);
        let safe = new_name
            .chars()
            .map(|c| if r#"/\:*?"<>|"#.contains(c) { '_' } else { c })
            .collect::<String>();
        let safe = if safe.is_empty() { "Untitled".to_string() } else { safe };
        let new_filename = format!("{safe}.m3u");
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

    /// Overwrite the playlist `.m3u` file with the tracks specified by
    /// `track_ids` (in order).  IDs not found in the library are skipped.
    pub fn save_playlist_tracks(&self, id: i64, track_ids: &[i64]) -> Result<()> {
        let pl = self.playlist_by_id(id)?;
        let mut lines = vec!["#EXTM3U".to_string()];
        for &tid in track_ids {
            if let Ok(path) = self.conn.query_row(
                "SELECT path FROM tracks WHERE id = ?1",
                params![tid],
                |row| row.get::<_, String>(0),
            ) {
                lines.push(path);
            }
        }
        std::fs::write(&pl.path, lines.join("\n") + "\n")
            .with_context(|| format!("write playlist {}", pl.path))?;
        Ok(())
    }

    /// Create a new playlist named `new_name` and write `track_paths` to it.
    ///
    /// Append `track_paths` to an existing playlist's `.m3u` file on disk.
    ///
    /// Used by the "Add to Playlist" right-click menu so the user can grow
    /// a saved playlist with raw paths (including stubs from the active
    /// playlist).  Duplicates are not filtered — callers that care should
    /// pre-filter.  The DB row is unchanged because playlist contents live
    /// in the `.m3u` file, not the database.
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
        // so each new path is on its own line.
        let mut body = existing;
        if !body.ends_with('\n') { body.push('\n'); }
        for p in track_paths {
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
    pub fn save_playlist_tracks_as(&self, new_name: &str, track_paths: &[String]) -> Result<i64> {
        let id = self.create_playlist(new_name)?;
        let pl = self.playlist_by_id(id)?;
        let mut lines = vec!["#EXTM3U".to_string()];
        for path in track_paths {
            lines.push(path.clone());
        }
        std::fs::write(&pl.path, lines.join("\n") + "\n")
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
                 bpm, length_secs, bitrate, channels, filetype, filename,
                 play_count, last_played,
                 comment, album_artist, disc_num, disc_total, composer, original_artist,
                 copyright, url, encoded_by, lyric, artwork_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    0, NULL,
                    ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)
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
                artwork_path    = excluded.artwork_path",
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
            ],
        )?;
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

    fn get_folder_id_for_path(&self, path: &str) -> Result<i64> {
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
        if let Some(scanned_secs) = Self::parse_iso_timestamp(last_scanned) {
            return mtime_secs > scanned_secs + 2;
        }

        true // If we can't parse the timestamp, rescan
    }

    /// Parse an ISO 8601 timestamp (format: YYYY-MM-DDTHH:MM:SSZ) to Unix seconds.
    fn parse_iso_timestamp(ts: &str) -> Option<u64> {
        // Expected format: "2024-01-15T10:30:00Z"
        let ts = ts.strip_suffix('Z')?;
        let parts: Vec<&str> = ts.split(|c| c == '-' || c == 'T' || c == ':').collect();
        if parts.len() < 6 {
            return None;
        }

        let year: u64 = parts[0].parse().ok()?;
        let month: u64 = parts[1].parse().ok()?;
        let day: u64 = parts[2].parse().ok()?;
        let hour: u64 = parts[3].parse().ok()?;
        let min: u64 = parts[4].parse().ok()?;
        let sec: u64 = parts[5].parse().ok()?;

        // Validate ranges
        if month < 1 || month > 12 {
            return None;
        }
        if day < 1 || day > 31 {
            return None;
        }
        if hour > 23 || min > 59 || sec > 59 {
            return None;
        }
        if day > Self::days_in_month(year, month) {
            return None;
        }

        // Simple conversion to Unix timestamp (ignoring leap seconds and timezone)
        let days_since_epoch = Self::days_since_1970(year, month, day);
        let secs = days_since_epoch as u64 * 86400 + hour * 3600 + min * 60 + sec;
        Some(secs)
    }

    /// Calculate days since 1970-01-01 (simplified, not accounting for Julian calendar)
    fn days_since_1970(year: u64, month: u64, day: u64) -> u64 {
        let mut days = (year - 1970) * 365;
        days += (year - 1969) / 4 - (year - 1901) / 100 + (year - 1601) / 400; // leap days
        for m in 1..month {
            days += Self::days_in_month(year, m);
        }
        days + day - 1
    }

    /// Get days in a month
    fn days_in_month(year: u64, month: u64) -> u64 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0) {
                    29
                } else {
                    28
                }
            }
            _ => 30,
        }
    }

    /// Update the `last_scanned` timestamp for a track.
    fn update_last_scanned(&self, path: &str) -> Result<()> {
        let now = Self::format_current_timestamp();
        self.conn.execute(
            "UPDATE tracks SET last_scanned = ?1 WHERE path = ?2",
            params![now, path],
        )?;
        Ok(())
    }

    /// Get current timestamp in ISO 8601 format.
    fn format_current_timestamp() -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let secs = now.as_secs();
        let days = secs / 86400;
        let rem = secs % 86400;
        let hour = rem / 3600;
        let min = (rem % 3600) / 60;
        let sec = rem % 60;

        // Find year, month, day from days since 1970
        let (year, month, day) = Self::year_month_day_from_days(days);
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            year, month, day, hour, min, sec
        )
    }

    /// Convert days since 1970 to (year, month, day).
    fn year_month_day_from_days(days: u64) -> (u64, u64, u64) {
        let mut year = 1970;
        let mut remaining_days = days;

        loop {
            let days_in_year = if Self::is_leap_year(year) { 366 } else { 365 };
            if remaining_days < days_in_year {
                break;
            }
            remaining_days -= days_in_year;
            year += 1;
        }

        let mut month = 1;
        loop {
            let days_in_month = Self::days_in_month(year, month);
            if remaining_days < days_in_month {
                return (year, month, remaining_days + 1);
            }
            remaining_days -= days_in_month;
            month += 1;
        }
    }

    fn is_leap_year(year: u64) -> bool {
        (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
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

    /// Look up a single track by its path.  Returns an error if not found.
    pub fn track_by_path(&self, path: &str) -> Result<LibTrack> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, artist, title, album, track_num, genre, year, bpm,
                    length_secs, bitrate, channels, filetype, filename, play_count, last_played,
                    comment, album_artist, disc_num, disc_total, composer, original_artist,
                    copyright, url, encoded_by, lyric, artwork_path, last_scanned
             FROM tracks WHERE path = ?1",
        )?;
        let mut rows = Self::collect_tracks(&mut stmt, params![path])?;
        rows.pop()
            .ok_or_else(|| anyhow::anyhow!("track not found: {}", path))
    }

    /// Clear the cached artwork path for a track so it gets re-extracted on next read.
    pub fn clear_artwork(&self, track_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET artwork_path = NULL WHERE id = ?1",
            params![track_id],
        )?;
        Ok(())
    }

    /// Clear and re-extract artwork from a track file.
    /// Updates the DB with the new cached artwork path.
    pub fn refresh_artwork(&self, track_id: i64, path: &str) -> Result<()> {
        // Delete old cached artwork file
        if let Ok(track) = self.track_by_path(path) {
            if let Some(ref old_art) = track.artwork_path {
                let _ = std::fs::remove_file(old_art);
            }
        }

        // Re-extract artwork from the file
        let tags = read_track_tags(std::path::Path::new(path));

        // Update DB with new artwork path
        self.conn.execute(
            "UPDATE tracks SET artwork_path = ?1 WHERE id = ?2",
            params![tags.artwork_path, track_id],
        )?;

        Ok(())
    }

    /// Map rows from a prepared statement into [`LibTrack`] values.
    ///
    /// `P` matches rusqlite's `Params` trait so this helper works with both
    /// `[]` (no params) and `params![...]`.
    fn collect_tracks<P: rusqlite::Params>(
        stmt: &mut rusqlite::Statement<'_>,
        params: P,
    ) -> Result<Vec<LibTrack>> {
        let mut tracks = Vec::new();
        let mut rows = stmt.query(params)?;
        while let Some(row) = rows.next()? {
            let path: String = row.get(1)?;
            let filename: Option<String> = row.get(13)?;
            let fname = filename.unwrap_or_else(|| {
                Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string()
            });
            let mut track = LibTrack {
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
                comment: row.get::<_, Option<String>>(16)?.map(|s| sanitize(&s)),
                album_artist: row.get::<_, Option<String>>(17)?.map(|s| sanitize(&s)),
                disc_num: row.get(18)?,
                disc_total: row.get(19)?,
                composer: row.get::<_, Option<String>>(20)?.map(|s| sanitize(&s)),
                original_artist: row.get::<_, Option<String>>(21)?.map(|s| sanitize(&s)),
                copyright: row.get::<_, Option<String>>(22)?.map(|s| sanitize(&s)),
                url: row.get::<_, Option<String>>(23)?.map(|s| sanitize(&s)),
                encoded_by: row.get::<_, Option<String>>(24)?.map(|s| sanitize(&s)),
                lyric: row.get::<_, Option<String>>(25)?.map(|s| sanitize(&s)),
                artwork_path: row.get::<_, Option<String>>(26)?.map(|s| sanitize(&s)),
                last_scanned: row.get::<_, Option<String>>(27)?,
                sort_keys: SortKeys::default(),
            };
            track.sort_keys = SortKeys::from_track(&track);
            tracks.push(track);
        }
        Ok(tracks)
    }
}

// ---------------------------------------------------------------------------
// Tag reading helpers
// ---------------------------------------------------------------------------

/// Raw tag data extracted from an audio file.
#[derive(Default)]
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
    comment: Option<String>,
    album_artist: Option<String>,
    disc_num: Option<i64>,
    disc_total: Option<i64>,
    composer: Option<String>,
    original_artist: Option<String>,
    copyright: Option<String>,
    url: Option<String>,
    encoded_by: Option<String>,
    lyric: Option<String>,
    artwork_path: Option<String>,
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
        let get_text = |frame_id: &str| -> Option<String> {
            tag.get(frame_id)
                .and_then(|f| f.content().text())
                .map(|s| sanitize(&s))
        };
        let get_first_comment =
            || -> Option<String> { tag.comments().next().map(|c| sanitize(&c.text)) };
        let disc = tag.disc();
        let (disc_num, disc_total) = if let Some(d) = disc {
            (Some(d as i64), tag.total_discs().map(|t| t as i64))
        } else {
            (None, None)
        };
        let lyric_text = tag.lyrics().next().map(|l| sanitize(&l.text));

        // Look for APIC (album art) and save it to the cache dir.
        let artwork_path = tag.pictures().next().map(|pic| {
            let cache_dir = dirs::cache_dir()
                .unwrap_or_else(|| std::env::temp_dir())
                .join("sparkamp");
            let _ = std::fs::create_dir_all(&cache_dir);
            // Use a hash of the path as the filename to avoid collisions.
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            path.hash(&mut h);
            let hash = h.finish();
            let ext = match pic.mime_type.as_str() {
                "image/png" => "png",
                "image/jpeg" | "image/jpg" => "jpg",
                _ => "bin",
            };
            let art_path = cache_dir.join(format!("{:016x}.{}", hash, ext));
            if !art_path.exists() {
                let _ = std::fs::write(&art_path, &pic.data);
            }
            art_path.to_string_lossy().into_owned()
        });

        TrackTags {
            title: tag.title().map(|s| sanitize(&s)),
            artist: tag.artist().map(|s| sanitize(&s)),
            album: tag.album().map(|s| sanitize(&s)),
            track_num: tag.track().map(|n| n as i64),
            genre: tag.genre().map(|s| sanitize(&s)),
            year: tag.year().map(|y| y as i64),
            bpm: get_text("TBPM"),
            bitrate: None,
            channels: None,
            comment: get_first_comment(),
            album_artist: tag.album_artist().map(|s| sanitize(&s)),
            disc_num,
            disc_total,
            composer: get_text("TCOM"),
            original_artist: get_text("TOPE"),
            copyright: get_text("TCOP"),
            url: get_text("WXXX"),
            encoded_by: get_text("TENC"),
            lyric: lyric_text,
            artwork_path,
        }
    } else {
        // Strategy 2: Symphonia generic (Vorbis Comments, FLAC, Opus, etc.).
        if let Some(meta) = read_symphonia_tags(path) {
            return meta;
        }
        // Fallback: no tags at all.
        TrackTags::default()
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
        comment: None,
        album_artist: None,
        disc_num: None,
        disc_total: None,
        composer: None,
        original_artist: None,
        copyright: None,
        url: None,
        encoded_by: None,
        lyric: None,
        artwork_path: None,
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

        lib.rescan_folder_metadata(
            folder_id,
            &cancel,
            |done, total| {
                assert!(done <= total);
                *progress_count_clone.lock().unwrap() += 1;
            },
            None,
        )
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
        let result = lib.rescan_folder_metadata(folder_id, &cancel, |_, _| {}, None);
        assert!(result.is_ok());
    }

    #[test]
    fn rescan_folder_metadata_sets_last_scanned() {
        gstreamer::init().ok();

        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 3);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap();

        // Verify tracks have no last_scanned yet
        let tracks_before = lib.all_tracks().unwrap();
        assert!(tracks_before.iter().all(|t| t.last_scanned.is_none()));

        // Run metadata scan
        let cancel = std::sync::atomic::AtomicBool::new(false);
        lib.rescan_folder_metadata(folder_id, &cancel, |_, _| {}, None)
            .unwrap();

        // Verify tracks now have last_scanned set
        let tracks_after = lib.all_tracks().unwrap();
        assert!(tracks_after.iter().all(|t| t.last_scanned.is_some()));
    }

    #[test]
    fn rescan_track_updates_metadata() {
        gstreamer::init().ok();

        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 2);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap();

        // Get first track path
        let tracks = lib.all_tracks().unwrap();
        assert!(!tracks.is_empty());
        let track_path = &tracks[0].path;

        // Verify no last_scanned initially
        assert!(tracks[0].last_scanned.is_none());

        // Rescan the track
        lib.rescan_track(track_path).unwrap();

        // Verify last_scanned is now set
        let tracks_after = lib.all_tracks().unwrap();
        let rescanned = tracks_after.iter().find(|t| t.path == *track_path).unwrap();
        assert!(rescanned.last_scanned.is_some());
    }

    // ── Smart scan helpers ─────────────────────────────────────────────────

    #[test]
    fn parse_iso_timestamp_valid() {
        // 2024-01-15T10:30:00Z
        let secs = MediaLibrary::parse_iso_timestamp("2024-01-15T10:30:00Z");
        assert!(secs.is_some());
        // Just verify it's a reasonable timestamp (after 2020)
        assert!(secs.unwrap() > 1700000000);
    }

    #[test]
    fn parse_iso_timestamp_invalid() {
        assert!(MediaLibrary::parse_iso_timestamp("not-a-date").is_none());
        assert!(MediaLibrary::parse_iso_timestamp("2024-13-45T10:30:00Z").is_none()); // Invalid date
        assert!(MediaLibrary::parse_iso_timestamp("").is_none());
    }

    #[test]
    fn needs_metadata_scan_never_scanned() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.mp3");
        fs::write(&file_path, b"fake").unwrap();
        let path = file_path.to_str().unwrap();

        // Never scanned - should need scan
        assert!(MediaLibrary::needs_metadata_scan(path, None));
    }

    #[test]
    fn needs_metadata_scan_file_missing() {
        // File doesn't exist - should need scan
        assert!(MediaLibrary::needs_metadata_scan(
            "/nonexistent/file.mp3",
            Some("2024-01-15T10:30:00Z")
        ));
    }

    #[test]
    fn needs_metadata_scan_file_changed_after_scan() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.mp3");
        fs::write(&file_path, b"fake").unwrap();

        // Wait a moment so mtime is definitely after old timestamp
        std::thread::sleep(std::time::Duration::from_millis(10));

        let path = file_path.to_str().unwrap();
        let old_timestamp = "2020-01-01T00:00:00Z";

        // File was modified after scan - should need scan
        assert!(MediaLibrary::needs_metadata_scan(path, Some(old_timestamp)));
    }

    #[test]
    fn needs_metadata_scan_file_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.mp3");
        fs::write(&file_path, b"fake").unwrap();

        let path = file_path.to_str().unwrap();

        // Get current mtime as a string (this is what we'd store after scanning)
        let current_ts = MediaLibrary::format_current_timestamp();

        // File hasn't changed since scan - should NOT need scan
        assert!(!MediaLibrary::needs_metadata_scan(path, Some(&current_ts)));
    }

    #[test]
    fn format_current_timestamp_format() {
        let ts = MediaLibrary::format_current_timestamp();
        // Should end with Z
        assert!(ts.ends_with('Z'));
        // Should be parseable
        assert!(MediaLibrary::parse_iso_timestamp(&ts).is_some());
    }

    // ── scan_folder ─────────────────────────────────────────────────────────

    #[test]
    fn scan_folder_scans_never_scanned() {
        gstreamer::init().ok();
        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 3);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap(); // Add tracks

        // Verify tracks have no last_scanned yet
        let tracks_before = lib.all_tracks().unwrap();
        assert!(tracks_before.iter().all(|t| t.last_scanned.is_none()));

        // Scan folder
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut progress_calls = Vec::new();
        let (scanned, skipped, _) = lib
            .scan_folder(folder_id, &cancel, |curr, total| {
                progress_calls.push((curr, total));
            })
            .unwrap();

        assert_eq!(scanned, 3);
        assert_eq!(skipped, 0);
        assert!(!progress_calls.is_empty());

        // Verify tracks now have last_scanned set
        let tracks_after = lib.all_tracks().unwrap();
        assert!(tracks_after.iter().all(|t| t.last_scanned.is_some()));
    }

    #[test]
    fn scan_folder_skips_unchanged_files() {
        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 2);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap();

        // Scan once
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (scanned1, _, _) = lib.scan_folder(folder_id, &cancel, |_, _| {}).unwrap();
        assert_eq!(scanned1, 2);

        // Scan again - should skip all (nothing changed)
        let cancel2 = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (scanned2, skipped2, _) = lib.scan_folder(folder_id, &cancel2, |_, _| {}).unwrap();
        assert_eq!(scanned2, 0);
        assert_eq!(skipped2, 2);
    }

    #[test]
    fn scan_folder_rescans_changed_files() {
        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 2);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap();

        // Scan once
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        lib.scan_folder(folder_id, &cancel, |_, _| {}).unwrap();

        // Wait and modify one file (3 seconds to ensure mtime differs after 2-second buffer)
        std::thread::sleep(std::time::Duration::from_secs(3));
        let files: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        fs::write(files[0].as_ref().unwrap().path(), b"modified data").unwrap();

        // Scan again - should rescan the modified file
        let cancel2 = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (scanned, skipped, _) = lib.scan_folder(folder_id, &cancel2, |_, _| {}).unwrap();
        assert_eq!(scanned, 1); // Only the modified file
        assert_eq!(skipped, 1); // The unchanged file
    }

    #[test]
    fn scan_folder_respects_cancel() {
        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 5);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap();

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);

        let result = lib.scan_folder(folder_id, &cancel, |_, _| {});
        assert!(result.is_ok()); // Should not error on cancel
    }

    // ── scan_all_folders ───────────────────────────────────────────────────

    #[test]
    fn scan_all_folders_processes_all_folders() {
        gstreamer::init().ok();
        let (lib, _db) = temp_lib();

        let dir1 = temp_dir_with_files("mp3", 2);
        let dir2 = temp_dir_with_files("flac", 3);

        let folder_id1 = lib.add_folder(dir1.path().to_str().unwrap()).unwrap().id();
        let folder_id2 = lib.add_folder(dir2.path().to_str().unwrap()).unwrap().id();

        lib.rescan_folder_fast(folder_id1, dir1.path().to_str().unwrap())
            .unwrap();
        lib.rescan_folder_fast(folder_id2, dir2.path().to_str().unwrap())
            .unwrap();

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (scanned, skipped, _) = lib.scan_all_folders(&cancel, |_, _| {}).unwrap();

        assert_eq!(scanned, 5); // 2 + 3
        assert_eq!(skipped, 0);
    }

    #[test]
    fn scan_all_folders_cumulative_progress() {
        gstreamer::init().ok();
        let (lib, _db) = temp_lib();

        let dir1 = temp_dir_with_files("mp3", 2);
        let dir2 = temp_dir_with_files("flac", 3);

        let folder_id1 = lib.add_folder(dir1.path().to_str().unwrap()).unwrap().id();
        let folder_id2 = lib.add_folder(dir2.path().to_str().unwrap()).unwrap().id();

        lib.rescan_folder_fast(folder_id1, dir1.path().to_str().unwrap())
            .unwrap();
        lib.rescan_folder_fast(folder_id2, dir2.path().to_str().unwrap())
            .unwrap();

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut last_total = 0usize;
        let result = lib
            .scan_all_folders(&cancel, |current, total| {
                // Total should be consistent (all files to scan)
                assert_eq!(total, 5);
                // Current should increase monotonically
                assert!(current >= last_total);
                last_total = current;
            })
            .unwrap();

        assert_eq!(result.0, 5); // All scanned
    }

    #[test]
    fn scan_all_folders_empty_library() {
        let (lib, _db) = temp_lib();

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (scanned, skipped, _) = lib.scan_all_folders(&cancel, |_, _| {}).unwrap();

        assert_eq!(scanned, 0);
        assert_eq!(skipped, 0);
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

    // ── soft_delete and purge ──────────────────────────────────────────

    #[test]
    fn soft_delete_marks_tracks_with_timestamp() {
        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 3);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap();

        let tracks = lib.all_tracks().unwrap();
        let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();

        // Soft delete 2 tracks
        lib.soft_delete_tracks(&ids[0..2]).unwrap();

        // Check count
        assert_eq!(lib.get_deleted_track_count().unwrap(), 2);

        // Tracks still exist but are marked as deleted
        assert_eq!(lib.all_tracks().unwrap().len(), 3);
    }

    #[test]
    fn purge_deleted_removes_marked_tracks() {
        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 3);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap();

        let tracks = lib.all_tracks().unwrap();
        let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();

        // Soft delete all tracks
        lib.soft_delete_tracks(&ids).unwrap();

        // Purge them
        let purged = lib.purge_deleted_tracks().unwrap();
        assert_eq!(purged, 3);

        // Tracks are now gone
        assert_eq!(lib.all_tracks().unwrap().len(), 0);
        assert_eq!(lib.get_deleted_track_count().unwrap(), 0);
    }

    #[test]
    fn purge_keeps_active_tracks() {
        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 3);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap();

        let tracks = lib.all_tracks().unwrap();
        let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();

        // Soft delete only first track
        lib.soft_delete_tracks(&ids[0..1]).unwrap();

        // Purge
        lib.purge_deleted_tracks().unwrap();

        // Only the non-deleted tracks remain
        assert_eq!(lib.all_tracks().unwrap().len(), 2);
    }

    #[test]
    fn cleanup_on_startup_purges_deleted() {
        let (lib, _db) = temp_lib();
        let dir = temp_dir_with_files("mp3", 3);
        let path = dir.path().to_str().unwrap();

        let folder_id = lib.add_folder(path).unwrap().id();
        lib.rescan_folder_fast(folder_id, path).unwrap();

        let tracks = lib.all_tracks().unwrap();
        let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();

        // Soft delete
        lib.soft_delete_tracks(&ids).unwrap();

        // Cleanup on startup (simulated)
        lib.cleanup_on_startup().unwrap();

        // All deleted
        assert_eq!(lib.all_tracks().unwrap().len(), 0);
    }

    #[test]
    fn soft_delete_empty_ids_is_noop() {
        let (lib, _db) = temp_lib();
        let result = lib.soft_delete_tracks(&[]);
        assert!(result.is_ok());
        assert_eq!(lib.get_deleted_track_count().unwrap(), 0);
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

    // ── SortKeys pre-computation ───────────────────────────────────────────

    #[test]
    fn sort_keys_are_precomputed_from_libtrack() {
        let track = LibTrack {
            id: 1,
            path: "/music/Test Song.mp3".into(),
            artist: Some("The ARTIST".into()),
            title: Some("My TITLE".into()),
            album: Some("The ALBUM".into()),
            track_num: Some(7),
            genre: Some("Rock".into()),
            year: Some(2024),
            bpm: None,
            length_secs: Some(180.5),
            bitrate: Some(320),
            channels: None,
            filetype: Some("mp3".into()),
            filename: "Test Song.mp3".into(),
            play_count: 0,
            last_played: None,
            comment: Some("Great track!".into()),
            album_artist: Some("Various Artists".into()),
            disc_num: None,
            disc_total: None,
            composer: None,
            original_artist: None,
            copyright: None,
            url: None,
            encoded_by: None,
            lyric: None,
            artwork_path: None,
            last_scanned: None,
            sort_keys: SortKeys::default(),
        };
        let keys = SortKeys::from_track(&track);

        assert_eq!(keys.num, "0000000007");
        assert_eq!(keys.title, "my title");
        assert_eq!(keys.artist, "the artist");
        assert_eq!(keys.album, "the album");
        assert_eq!(keys.duration, "00000000180.500");
        assert_eq!(keys.filename, "test song.mp3");
        assert_eq!(keys.year, "0000002024");
        assert_eq!(keys.genre, "rock");
        assert_eq!(keys.bitrate, "0000000320");
        assert_eq!(keys.album_artist, "various artists");
        assert_eq!(keys.composer, "");
        assert_eq!(keys.comment, "great track!");
    }

    #[test]
    fn sort_keys_fallback_to_filename_for_title() {
        let track = LibTrack {
            id: 1,
            path: "/music/No Title.mp3".into(),
            artist: None,
            title: None,
            album: None,
            track_num: None,
            genre: None,
            year: None,
            bpm: None,
            length_secs: None,
            bitrate: None,
            channels: None,
            filetype: None,
            filename: "No Title.mp3".into(),
            play_count: 0,
            last_played: None,
            comment: None,
            album_artist: None,
            disc_num: None,
            disc_total: None,
            composer: None,
            original_artist: None,
            copyright: None,
            url: None,
            encoded_by: None,
            lyric: None,
            artwork_path: None,
            last_scanned: None,
            sort_keys: SortKeys::default(),
        };
        let keys = SortKeys::from_track(&track);

        assert_eq!(keys.title, "no title.mp3");
    }

    // ── record_play ────────────────────────────────────────────────────────

    #[test]
    fn record_play_increments_play_count() {
        let (lib, _db) = temp_lib();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("song.mp3");
        let path = file_path.to_str().unwrap();
        fs::write(&file_path, b"fake").unwrap();

        let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();
        lib.rescan_folder_fast(folder_id, dir.path().to_str().unwrap())
            .unwrap();

        // play_count starts at 0.
        let track = lib.track_by_path(path).unwrap();
        assert_eq!(track.play_count, 0);

        lib.record_play(path).unwrap();

        let track = lib.track_by_path(path).unwrap();
        assert_eq!(track.play_count, 1);
        assert!(track.last_played.is_some());
    }

    #[test]
    fn record_play_accumulates_multiple_calls() {
        let (lib, _db) = temp_lib();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("song.mp3");
        let path = file_path.to_str().unwrap();
        fs::write(&file_path, b"fake").unwrap();

        let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();
        lib.rescan_folder_fast(folder_id, dir.path().to_str().unwrap())
            .unwrap();

        for i in 1..=5 {
            lib.record_play(path).unwrap();
            let track = lib.track_by_path(path).unwrap();
            assert_eq!(track.play_count, i);
        }
    }

    #[test]
    fn record_play_updates_last_played_timestamp() {
        let (lib, _db) = temp_lib();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("song.mp3");
        let path = file_path.to_str().unwrap();
        fs::write(&file_path, b"fake").unwrap();

        let folder_id = lib.add_folder(dir.path().to_str().unwrap()).unwrap().id();
        lib.rescan_folder_fast(folder_id, dir.path().to_str().unwrap())
            .unwrap();

        lib.record_play(path).unwrap();
        let first = lib.track_by_path(path).unwrap().last_played.clone();
        assert!(first.is_some(), "first play should set last_played");

        // Wait 1.1 seconds so the second play gets a different timestamp
        // (timestamps are stored as seconds, not milliseconds).
        std::thread::sleep(std::time::Duration::from_millis(1100));
        lib.record_play(path).unwrap();
        let second = lib.track_by_path(path).unwrap().last_played;

        assert!(second.is_some(), "second play should update last_played");
        assert_ne!(first, second, "second play should have a newer timestamp");
    }

    #[test]
    fn record_play_noop_for_unknown_path() {
        let (lib, _db) = temp_lib();
        // No track added — record_play should succeed without error.
        let result = lib.record_play("/nonexistent/path.mp3");
        assert!(result.is_ok());
    }

    // -----------------------------------------------------------------------
    // read_only_track_fields
    // -----------------------------------------------------------------------

    #[test]
    fn read_only_track_fields_all_values_formatted() {
        let track = LibTrack {
            id: 1,
            path: "/music/song.mp3".into(),
            artist: Some("The Artist".into()),
            title: Some("My Song".into()),
            album: Some("The Album".into()),
            track_num: Some(5),
            genre: Some("Rock".into()),
            year: Some(2020),
            bpm: Some("120".into()),
            length_secs: Some(185.0),
            bitrate: Some(320),
            channels: Some(2),
            filetype: Some("MP3".into()),
            filename: "song.mp3".into(),
            play_count: 42,
            last_played: Some("2024-01-15T10:30:00Z".into()),
            comment: None,
            album_artist: None,
            disc_num: None,
            disc_total: None,
            composer: None,
            original_artist: None,
            copyright: None,
            url: None,
            encoded_by: None,
            lyric: None,
            artwork_path: Some("/music/cover.jpg".into()),
            last_scanned: None,
            sort_keys: SortKeys::default(),
        };
        let path = std::path::Path::new("/music/song.mp3");
        let ro = read_only_track_fields(path, Some(&track));

        assert_eq!(ro.filename, "song.mp3");
        assert_eq!(ro.path, "/music/song.mp3");
        assert_eq!(ro.filetype, "MP3");
        assert_eq!(ro.bitrate, "320k");
        assert_eq!(ro.channels, "stereo");
        assert_eq!(ro.duration, "3:05");
        assert_eq!(ro.play_count, "42");
        assert_eq!(ro.last_played, "2024-01-15T10:30:00Z");
        assert_eq!(ro.num, "5");
        assert_eq!(ro.artwork_path, "/music/cover.jpg");
    }

    #[test]
    fn read_only_track_fields_fallback_when_no_track() {
        let path = std::path::Path::new("/unknown/file.mp3");
        let ro = read_only_track_fields(path, None);

        assert_eq!(ro.filename, "file.mp3");
        assert_eq!(ro.path, "/unknown/file.mp3");
        assert_eq!(ro.filetype, "");
        assert_eq!(ro.bitrate, "");
        assert_eq!(ro.channels, "");
        assert_eq!(ro.duration, "-:--");
        assert_eq!(ro.play_count, "");
        assert_eq!(ro.last_played, "");
        assert_eq!(ro.num, "");
        assert_eq!(ro.artwork_path, "");
    }

    #[test]
    fn read_only_track_fields_channels_mono() {
        let track = LibTrack {
            id: 0,
            path: String::new(),
            artist: None,
            title: None,
            album: None,
            track_num: None,
            genre: None,
            year: None,
            bpm: None,
            length_secs: None,
            bitrate: None,
            channels: Some(1),
            filetype: None,
            filename: String::new(),
            play_count: 0,
            last_played: None,
            comment: None,
            album_artist: None,
            disc_num: None,
            disc_total: None,
            composer: None,
            original_artist: None,
            copyright: None,
            url: None,
            encoded_by: None,
            lyric: None,
            artwork_path: None,
            last_scanned: None,
            sort_keys: SortKeys::default(),
        };
        let path = std::path::Path::new("/test.mp3");
        let ro = read_only_track_fields(path, Some(&track));
        assert_eq!(ro.channels, "mono");
    }

    #[test]
    fn read_only_track_fields_channels_multi() {
        let track = LibTrack {
            id: 0,
            path: String::new(),
            artist: None,
            title: None,
            album: None,
            track_num: None,
            genre: None,
            year: None,
            bpm: None,
            length_secs: None,
            bitrate: None,
            channels: Some(6),
            filetype: None,
            filename: String::new(),
            play_count: 0,
            last_played: None,
            comment: None,
            album_artist: None,
            disc_num: None,
            disc_total: None,
            composer: None,
            original_artist: None,
            copyright: None,
            url: None,
            encoded_by: None,
            lyric: None,
            artwork_path: None,
            last_scanned: None,
            sort_keys: SortKeys::default(),
        };
        let path = std::path::Path::new("/test.mp3");
        let ro = read_only_track_fields(path, Some(&track));
        assert_eq!(ro.channels, "6ch");
    }

}
