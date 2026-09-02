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

mod devices;
mod playlists;
mod queries;
mod scan;

// Re-export for callers; no consumer in the bin build yet, so allow the unused-import warning.
#[allow(unused_imports)]
pub use devices::{DeviceRecord, PlaylistBaseline, SyncPair};
// PlaySnapshot: pre-play stats captured before record_play's 20s timer fires.
#[allow(unused_imports)]
pub use playlists::PlaySnapshot;
// AlbumGroup/AlbumSort: phase-11 album gallery grouping types; no frontend
// consumer yet (that lands in a later phase), so allow the unused-import
// warning until then.
#[allow(unused_imports)]
pub use queries::{AlbumGroup, AlbumSort, NO_ALBUM_LABEL};
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
    /// Sample rate in Hz, read from the codec header by `technical_probe`.
    pub sample_rate: Option<i64>,
    /// File size in bytes, captured at scan time.
    pub file_size: Option<i64>,
    /// ISO-8601 datetime string of the file's on-disk modification time.
    pub file_mtime: Option<String>,
    /// ISO-8601 datetime string of the row's first INSERT. Never updated on
    /// later upserts, so it reflects when the file entered the library.
    pub added_at: Option<String>,
    /// "VBR" / "CBR" for MP3 files, `None` when undetermined or non-MP3.
    pub bitrate_mode: Option<String>,
    /// ReplayGain values from analysis, in dB (gains) / linear peak (0..~1).
    /// `None` until the track has been analyzed.
    pub rg_track_gain: Option<f64>,
    pub rg_track_peak: Option<f64>,
    pub rg_album_gain: Option<f64>,
    pub rg_album_peak: Option<f64>,
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
    pub(crate) fn from_track(track: &LibTrack) -> Self {
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
    pub sample_rate: String,
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
    read_only_fields_inner(path, track, true)
}

/// [`read_only_track_fields`] with the filesystem left alone.
///
/// Same fields, but a file the library has no row for simply comes back with
/// the library-derived ones empty instead of being read off disk. For a
/// display surface that is the right trade, and for one that refreshes on
/// every track change it is the only safe option.
///
/// This exists because the probing version reached the now-playing panel.
/// There it ran on the UI thread on every track change, and for a macOS audio
/// CD — where a track is a real ~40 MB AIFF on the drive rather than Linux's
/// unopenable `cdda://` URI — it read the whole file twice, starving playback
/// of the very disc it was reading and wedging the app. The ID3 editor keeps
/// the probing version: it is user-initiated, happens once per dialog, and the
/// user is already waiting on it.
#[allow(dead_code)]
pub fn read_only_track_fields_no_probe(
    path: &std::path::Path,
    track: Option<&LibTrack>,
) -> ReadOnlyTrackFields {
    read_only_fields_inner(path, track, false)
}

/// `probe` decides whether a file with no library row may be read off disk.
/// Every filesystem access in here is gated on it — there is no such thing as
/// a partially-quiet call.
fn read_only_fields_inner(
    path: &std::path::Path,
    track: Option<&LibTrack>,
    probe: bool,
) -> ReadOnlyTrackFields {
    // Files outside the library (played from the active playlist, Testing
    // dirs, …) have no LibTrack row, but the tech line should still work:
    // probe the file directly. One probe per editor-open — cheap enough.
    let may_probe = probe && track.is_none();
    let probed = if may_probe {
        crate::technical_probe::probe_technical(path)
    } else {
        crate::technical_probe::TechProbe::default()
    };
    let probed_len = if may_probe {
        crate::duration_probe::probe_duration(path)
            .or_else(|| crate::duration_probe::discover_duration(path))
            .map(|d| d.as_secs_f64())
    } else {
        None
    };
    let probed_size = if may_probe {
        std::fs::metadata(path).ok().map(|m| m.len())
    } else {
        None
    };

    let filename = track.map(|t| t.filename.clone()).unwrap_or_else(|| {
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string()
    });
    let path_str = path.to_string_lossy().into_owned();
    let filetype = track
        .and_then(|t| t.filetype.clone())
        .or_else(|| {
            path.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
        })
        .unwrap_or_default();
    let bitrate = track
        .and_then(|t| t.bitrate)
        .or_else(|| {
            probed_size
                .zip(probed_len)
                .and_then(|(sz, len)| crate::technical_probe::avg_bitrate_kbps(sz, len))
        })
        .map(|b| format!("{b}k"))
        .unwrap_or_default();
    let sample_rate = track
        .and_then(|t| t.sample_rate)
        .or(probed.sample_rate)
        .map(|s| format!("{:.1} kHz", s as f64 / 1000.0))
        .unwrap_or_default();
    let channels = track
        .and_then(|t| t.channels)
        .or(probed.channels)
        .map(|c| match c {
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            n => format!("{}ch", n),
        })
        .unwrap_or_default();
    let duration = crate::model::fmt_secs(track.and_then(|t| t.length_secs).or(probed_len));
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
        .or_else(|| {
            // Only probe embedded/folder art for files OUTSIDE the library
            // (track.is_none()). Probing for indexed library rows too made
            // the ID3 editor (which calls this fn directly to pre-fill its
            // artwork entry) silently embed a loose folder image into the
            // file's APIC tag on save whenever the DB's art column was empty
            // — an unrequested mutation. The now-playing display gets its
            // own folder/embedded fallback in `now_playing::build_now_playing_info`
            // instead, which is display-only and never feeds a save path.
            if may_probe {
                crate::tags::read_track_tags(path).artwork_path
            } else {
                None
            }
        })
        .unwrap_or_default();

    ReadOnlyTrackFields {
        filename,
        path: path_str,
        filetype,
        bitrate,
        sample_rate,
        channels,
        duration,
        play_count,
        last_played,
        num,
        artwork_path,
    }
}

/// One-line technical summary for the ID3 window: uppercase filetype,
/// bitrate, sample rate, channel layout, duration — skipping empty parts.
/// Deliberately NOT shown on the main player window (spec deviation from
/// Winamp): the ID3 window is Sparkamp's home for technical detail.
#[allow(dead_code)]
pub fn tech_summary(ro: &ReadOnlyTrackFields) -> String {
    let ft = ro.filetype.to_uppercase();
    [ft.as_str(), &ro.bitrate, &ro.sample_rate, &ro.channels, &ro.duration]
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Check if a file is read-only by attempting to open it for writing.
///
/// Returns `true` if the file cannot be written to (permission denied or read-only filesystem).
/// Returns `false` if the file can be opened for writing, or if an error occurs.
/// This method works reliably for all filesystem types including network shares
/// (SMB/CIFS/NFS) and system-level read-only mounts.
pub fn is_read_only(path: &std::path::Path) -> bool {
    // Ask the kernel whether we *could* write, rather than opening the file to
    // find out.
    //
    // This used to be `OpenOptions::new().write(true).open(path)`, which gives
    // the same answer and then emits an `IN_CLOSE_WRITE` inotify event when the
    // handle drops — Linux reports that for any descriptor opened for writing,
    // whether or not a byte was written. The folder watcher saw those events,
    // called them modifications, rewrote the rows and rebuilt the Files view;
    // rebuilding rebound the rows, whose status column probed more files, which
    // emitted more events. A closed loop that reset the user's scroll position
    // and selection every 15-20 seconds with nothing touching the disk
    // (2026-08-11).
    //
    // `access(W_OK)` answers from the mount flags and the permission bits
    // without a descriptor, so it catches a read-only mount as well as a
    // read-only file, and is cheaper besides.
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `c` is a valid NUL-terminated path.
        unsafe { libc::access(c.as_ptr(), libc::W_OK) != 0 }
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path)
            .map(|m| m.permissions().readonly())
            .unwrap_or(false)
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

        // Enable WAL mode for better concurrent read performance, and a busy
        // timeout so a second connection (e.g. a background scan thread) waits
        // for the write lock instead of failing with SQLITE_BUSY.
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;

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
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
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

            CREATE TABLE IF NOT EXISTS device_playlist_baselines (
                device_id           TEXT NOT NULL,
                library_playlist_id INTEGER NOT NULL,
                device_filename     TEXT NOT NULL,
                entries_hash        TEXT NOT NULL DEFAULT '',
                last_sync_at        TEXT,
                PRIMARY KEY (device_id, library_playlist_id)
            );

            CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(artist);
            CREATE INDEX IF NOT EXISTS idx_tracks_title  ON tracks(title);
            CREATE INDEX IF NOT EXISTS idx_tracks_album  ON tracks(album);
            CREATE INDEX IF NOT EXISTS idx_tracks_folder ON tracks(folder_id);
            -- `track_by_same_file`'s fallback narrows by filename before
            -- comparing (device, inode). Without this it is a full scan of
            -- every row, and it runs on the miss path — which is every track
            -- the library does not hold. Measured on a 36,329-track library:
            -- the now-playing rebuild took 312-2677 ms per track change,
            -- entirely here, and on macOS that starved the audio pipeline of
            -- the CD it was playing.
            --
            -- Linux never showed it. There a disc track is a `cdda://`
            -- pseudo-URI, so `file_identity` cannot stat it and the fallback
            -- returns before reaching this query at all.
            CREATE INDEX IF NOT EXISTS idx_tracks_filename ON tracks(filename);
            -- Covers MediaLibrary::album_rows()'s GROUP BY. A plain
            -- (album, album_artist, artist) index does NOT get used here —
            -- confirmed with EXPLAIN QUERY PLAN, still 'USE TEMP B-TREE FOR
            -- GROUP BY' — because the query groups on
            -- LOWER(TRIM(COALESCE(...))) of each column, not the raw column.
            -- The index expressions must match that exactly for SQLite to
            -- scan in already-grouped order instead of sorting; if
            -- album_rows()'s GROUP BY expression ever changes, this index
            -- must change with it.
            CREATE INDEX IF NOT EXISTS idx_tracks_album_group
                ON tracks(
                    LOWER(TRIM(COALESCE(album,''))),
                    LOWER(TRIM(COALESCE(album_artist,''))),
                    LOWER(TRIM(COALESCE(artist,'')))
                );
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
            ("sample_rate", "INTEGER"),
            ("file_size", "INTEGER"),
            ("file_mtime", "TEXT"),
            ("added_at", "TEXT"),
            ("bitrate_mode", "TEXT"),
            ("rg_track_gain", "REAL"),
            ("rg_track_peak", "REAL"),
            ("rg_album_gain", "REAL"),
            ("rg_album_peak", "REAL"),
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

        // Same additive-migration pattern for `folders`: DBs created before
        // per-folder recurse existed need the column backfilled, defaulting
        // to 1 (recurse) so existing watched folders keep scanning exactly
        // as they did before this column existed.
        let folder_cols: std::collections::HashSet<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT name FROM pragma_table_info('folders')")?;
            stmt.query_map([], |row| row.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect()
        };
        if !folder_cols.contains("recurse") {
            self.conn.execute(
                "ALTER TABLE folders ADD COLUMN recurse INTEGER NOT NULL DEFAULT 1",
                [],
            )?;
        }
        // The security-scoped bookmark that keeps this folder readable across
        // launches under the App Sandbox, where the path alone grants nothing.
        // NULL for every row written before the column existed and for every
        // row written outside a sandbox, both of which are ordinary: see
        // `crate::sandbox`.
        if !folder_cols.contains("bookmark") {
            self.conn
                .execute("ALTER TABLE folders ADD COLUMN bookmark BLOB", [])?;
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
                sample_rate: row.get(28)?,
                file_size: row.get(29)?,
                file_mtime: row.get::<_, Option<String>>(30)?,
                added_at: row.get::<_, Option<String>>(31)?,
                bitrate_mode: row.get::<_, Option<String>>(32)?.map(|s| sanitize(&s)),
                rg_track_gain: row.get(33)?,
                rg_track_peak: row.get(34)?,
                rg_album_gain: row.get(35)?,
                rg_album_peak: row.get(36)?,
                sort_keys: SortKeys::default(),
            };
            track.sort_keys = SortKeys::from_track(&track);
            tracks.push(track);
        }
        Ok(tracks)
    }

    /// Store ReplayGain analysis results for a track (gains in dB, peaks
    /// linear 0..~1). Written by the analysis job; the scan/upsert path leaves
    /// these NULL. `#[allow(dead_code)]` until P4-T4 wires the analyzer.
    #[allow(dead_code)]
    pub fn set_replaygain(
        &self,
        id: i64,
        track_gain: f64,
        track_peak: f64,
        album_gain: f64,
        album_peak: f64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET rg_track_gain = ?1, rg_track_peak = ?2, \
             rg_album_gain = ?3, rg_album_peak = ?4 WHERE id = ?5",
            rusqlite::params![track_gain, track_peak, album_gain, album_peak, id],
        )?;
        Ok(())
    }

    /// Set (or clear, with `None`) just the track gain for the row at `path`,
    /// leaving peaks and album gain alone. Used by the ID3 editor's manual
    /// ReplayGain edit, which changes one measured number rather than
    /// replacing a whole analysis result. Returns the number of rows updated
    /// (0 when the file isn't in the library).
    pub fn set_track_gain_by_path(&self, path: &str, gain_db: Option<f64>) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE tracks SET rg_track_gain = ?1 WHERE path = ?2",
            rusqlite::params![gain_db, path],
        )?)
    }

    /// An O(1) proxy for "has anything in the database changed since I last
    /// looked," for callers that cache a query result (the album gallery's
    /// whole-library fold) and want to know when to distrust the cache.
    ///
    /// Deliberately two constant-time reads, not a query: `SELECT COUNT(*),
    /// MAX(last_scanned)` measured at 6-7ms warm against a 43.6ms album fold
    /// on the reference library — paying 14% of the cost of the thing being
    /// cached, on every cache check, would defeat the point of caching it.
    ///
    /// - `sqlite3_total_changes()` — rows changed by THIS connection since it
    ///   was opened. rusqlite 0.31 does not expose this as a safe method:
    ///   `Connection::changes()` wraps `sqlite3_changes()` instead, which
    ///   reports only the most recently completed statement, not a running
    ///   total, so it can't stand in here (two writes between checks would
    ///   look like one, or like none, depending on timing). Reached instead
    ///   through `Connection::handle()`, the raw-handle escape hatch
    ///   rusqlite itself documents for exactly this kind of gap, paired with
    ///   `ffi::sqlite3_total_changes64()` (confirmed present in the bundled
    ///   SQLite 3.45.0 the `bundled` feature compiles — added upstream in
    ///   3.37.0). This is the only `unsafe` in `media_library/`; precedent
    ///   for `unsafe` elsewhere in the codebase is `src/ffi/*.rs` (the macOS
    ///   C ABI bridge) and `src/engine.rs`, not previously this module.
    /// - `PRAGMA data_version` — bumps when ANOTHER connection commits, and
    ///   — this is SQLite's own documented behaviour for this pragma, see
    ///   the comment above `sqlite3_changes()` in sqlite3.h — *omits*
    ///   changes made by this connection, which is exactly why it can't
    ///   stand in alone either. Every write path in this codebase (scan, tag
    ///   edit, track removal, device sync) opens its own `Connection` via
    ///   `open_at` on a background thread, so this half is what actually
    ///   catches those.
    ///
    /// A change on this connection moves the first number; a change on any
    /// other connection moves the second. A cache keyed on just one half
    /// would miss whichever kind of write the other half exists to catch.
    ///
    /// Known limitation, accepted rather than fixed (fix round 2 review):
    /// this is a whole-database token, not an albums-table-specific one. Any
    /// write on this connection moves the first number, including
    /// `record_play`'s `UPDATE ... play_count` (`tick.rs`, fired every few
    /// seconds during ordinary listening on the same shared connection the
    /// gallery reads through) — so the gallery's cached fold gets dropped by
    /// nothing more than the user leaving a track playing. Narrowing that
    /// would mean going back to per-call-site invalidation (i.e. some list
    /// of "these writes matter, these don't"), which is exactly the
    /// discipline that silently broke for track removal in fix round 1. The
    /// cost of leaving it broad is bounded: worst case, opening the gallery
    /// after a listening session pays one extra re-fold — the same cost a
    /// cache-less gallery would always pay, never worse.
    pub fn change_token(&self) -> (i64, i64) {
        // SAFETY: `handle()` returns `self.conn`'s raw `sqlite3*`, valid for
        // as long as `self.conn` is alive (it is, for the duration of this
        // call). `sqlite3_total_changes64` only reads an in-memory counter
        // already maintained by the connection — it performs no I/O, takes
        // no lock beyond what SQLite already holds internally, and cannot
        // invalidate the handle.
        let total_changes: i64 =
            unsafe { rusqlite::ffi::sqlite3_total_changes64(self.conn.handle()) };
        let data_version: i64 = self
            .conn
            .pragma_query_value(None, "data_version", |row| row.get(0))
            // Fix round 2 review nit: on an already-open, already-initialized
            // connection this PRAGMA is effectively infallible, but a
            // constant fallback (`.unwrap_or(0)`) would fail in the wrong
            // direction if it ever weren't — every subsequent call would
            // read the same `0`, so a cache comparing tokens would see "no
            // change" forever and go silently stale for good, the exact
            // failure mode this whole mechanism exists to prevent. A fresh
            // value on every failed call instead makes the token compare
            // unequal to whatever was last cached, forcing a re-fold on
            // every `rebuild()` until the PRAGMA works again — a real cost,
            // but only while something is already broken, and never a
            // silent lie.
            .unwrap_or_else(|_| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(i64::MIN)
            });
        (total_changes, data_version)
    }
}

