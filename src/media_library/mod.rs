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
//! - **playlists** — `.m3u8` / `.m3u` files found under watched folders.

mod playlists;
mod queries;
mod scan;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

use crate::textutil::sanitize;

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

            CREATE TABLE IF NOT EXISTS devices (
                id          TEXT PRIMARY KEY,
                label       TEXT NOT NULL DEFAULT '',
                last_seen   TEXT,
                smart_rules TEXT
            );

            CREATE TABLE IF NOT EXISTS device_sync_pairs (
                device_id          TEXT NOT NULL,
                device_relpath     TEXT NOT NULL,
                library_path       TEXT NOT NULL,
                baseline_tag_hash  TEXT NOT NULL DEFAULT '',
                baseline_rating    INTEGER NOT NULL DEFAULT 0,
                baseline_playcount INTEGER NOT NULL DEFAULT 0,
                last_sync_at       TEXT,
                PRIMARY KEY (device_id, device_relpath)
            );

            CREATE INDEX IF NOT EXISTS idx_pairs_library
                ON device_sync_pairs(library_path);

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
            ("rating", "INTEGER"),
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

