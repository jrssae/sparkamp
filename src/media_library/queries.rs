//! Track queries — listing, search, sorted views — and track removal
//! (hard delete, soft delete, purge) plus artwork cache maintenance.

use anyhow::Result;
use rusqlite::params;
use std::collections::HashMap;

use crate::play_stats::effective_album_artist;
use crate::tags::read_track_tags;

use super::{LibTrack, MediaLibrary};

/// How to order the album gallery's groups. See [`MediaLibrary::albums`].
// No frontend consumer yet (phase-11 gallery view lands later); allow
// dead-code until then, mirroring the allow on the surrounding impl block.
#[allow(dead_code)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AlbumSort {
    Artist,
    Album,
    Year,
}

/// One album (or the single "no album" bucket) as folded from the tracks
/// table. Produced by [`MediaLibrary::albums`]; feeds the phase-11 album
/// gallery view.
#[derive(Clone, Debug, PartialEq)]
pub struct AlbumGroup {
    pub album: String,
    pub album_artist: String,
    pub year: Option<i64>,
    pub track_count: i64,
    pub artwork_path: Option<String>,
    pub is_no_album: bool,
}

/// Lean per-track projection used only to fold rows into [`AlbumGroup`]s.
/// Deliberately narrower than [`LibTrack`] — the gallery grid needs none of
/// the other 30-odd columns just to group and count.
struct AlbumRow {
    artist: String,
    album: String,
    album_artist: String,
    year: Option<i64>,
    artwork_path: Option<String>,
}

// Bin build on macOS gates out GTK, leaving these FFI/GTK-reachable
// methods unused there; mirrors the allow on the original impl block.
#[allow(dead_code)]
impl MediaLibrary {

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
                    copyright, url, encoded_by, lyric, artwork_path, last_scanned,
                    sample_rate, file_size, file_mtime, added_at, bitrate_mode,
                    rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak
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
                    copyright, url, encoded_by, lyric, artwork_path, last_scanned,
                    sample_rate, file_size, file_mtime, added_at, bitrate_mode,
                    rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak
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
                    copyright, url, encoded_by, lyric, artwork_path, last_scanned,
                    sample_rate, file_size, file_mtime, added_at, bitrate_mode,
                    rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak
             FROM tracks
             WHERE {word_clauses}
             ORDER BY {order}",
        );
        let mut stmt = self.conn.prepare(&sql)?;
        Self::collect_tracks(&mut stmt, rusqlite::params_from_iter(words.iter()))
    }

    /// Build the SQL ORDER BY clause for a given column ID and direction.
    pub(super) fn sort_order_clause(col: &str, desc: bool) -> String {
        let dir = if desc { "DESC" } else { "ASC" };
        match col {
            "title" => format!("LOWER(COALESCE(title,'')) {dir}, LOWER(COALESCE(artist,'')) ASC"),
            "artist" => format!(
                "LOWER(COALESCE(artist,'')) {dir}, LOWER(COALESCE(album,'')) ASC, track_num ASC"
            ),
            "album" => format!(
                "LOWER(COALESCE(album,'')) {dir}, LOWER(COALESCE(artist,'')) ASC, track_num ASC"
            ),
            "album_artist" => format!(
                "LOWER(COALESCE(album_artist,'')) {dir}, LOWER(COALESCE(album,'')) ASC, track_num ASC"
            ),
            "composer" => format!(
                "LOWER(COALESCE(composer,'')) {dir}, LOWER(COALESCE(artist,'')) ASC"
            ),
            "comment" => format!(
                "LOWER(COALESCE(comment,'')) {dir}, LOWER(COALESCE(artist,'')) ASC"
            ),
            "bpm" => format!(
                "LOWER(COALESCE(bpm,'')) {dir}, LOWER(COALESCE(artist,'')) ASC"
            ),
            "disc_num" => format!(
                "COALESCE(disc_num, 0) {dir}, COALESCE(track_num, 0) ASC, LOWER(COALESCE(artist,'')) ASC"
            ),
            "duration" => format!("COALESCE(length_secs, 0) {dir}, LOWER(COALESCE(artist,'')) ASC"),
            "filename" => format!("LOWER(COALESCE(filename,'')) {dir}"),
            "year" => format!("COALESCE(year, 0) {dir}, LOWER(COALESCE(artist,'')) ASC"),
            "genre" => format!("LOWER(COALESCE(genre,'')) {dir}, LOWER(COALESCE(artist,'')) ASC"),
            "bitrate" => format!("COALESCE(bitrate, 0) {dir}, LOWER(COALESCE(artist,'')) ASC"),
            "sample_rate" => format!(
                "COALESCE(sample_rate, 0) {dir}, LOWER(COALESCE(artist,'')) ASC"
            ),
            "file_size" => format!(
                "COALESCE(file_size, 0) {dir}, LOWER(COALESCE(artist,'')) ASC"
            ),
            "added_at" => format!("LOWER(COALESCE(added_at,'')) {dir}, LOWER(COALESCE(artist,'')) ASC"),
            "file_mtime" => format!(
                "LOWER(COALESCE(file_mtime,'')) {dir}, LOWER(COALESCE(artist,'')) ASC"
            ),
            "bitrate_mode" => format!(
                "LOWER(COALESCE(bitrate_mode,'')) {dir}, LOWER(COALESCE(artist,'')) ASC"
            ),
            "num" => format!("COALESCE(track_num, 0) {dir}, LOWER(COALESCE(artist,'')) ASC"),
            "play_count" => format!("COALESCE(play_count, 0) {dir}, LOWER(COALESCE(artist,'')) ASC"),
            // last_played sorts NULLs (never played) to the end regardless of direction
            // so users browsing recent activity see real timestamps first.
            "last_played" => format!(
                "CASE WHEN last_played IS NULL OR last_played = '' THEN 1 ELSE 0 END ASC, \
                 last_played {dir}, LOWER(COALESCE(artist,'')) ASC"
            ),
            // Unanalyzed tracks (rg_track_gain IS NULL) sort to the end
            // regardless of direction, same convention as last_played above —
            // SQLite's native NULL ordering doesn't need a COALESCE fallback
            // (unlike bitrate/file_size, 0 dB is a real, meaningful gain, so
            // blending "not yet analyzed" into it would be misleading).
            "rg_gain" => format!(
                "CASE WHEN rg_track_gain IS NULL THEN 1 ELSE 0 END ASC, \
                 rg_track_gain {dir}, LOWER(COALESCE(artist,'')) ASC"
            ),
            // Default: artist → album → track number
            _ => format!(
                "LOWER(COALESCE(artist,'')) {dir}, LOWER(COALESCE(album,'')) ASC, track_num ASC"
            ),
        }
    }

    /// Return the first track in the library whose `filename` column matches
    /// (case-sensitive).  Used as a fallback when the full path has changed.
    pub(super) fn track_by_filename_first(&self, filename: &str) -> Result<LibTrack> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, artist, title, album, track_num, genre, year, bpm,
                    length_secs, bitrate, channels, filetype, filename, play_count, last_played,
                    comment, album_artist, disc_num, disc_total, composer, original_artist,
                    copyright, url, encoded_by, lyric, artwork_path, last_scanned,
                    sample_rate, file_size, file_mtime, added_at, bitrate_mode,
                    rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak
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

    /// Look up a single track by its path.  Returns an error if not found.
    pub fn track_by_path(&self, path: &str) -> Result<LibTrack> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, artist, title, album, track_num, genre, year, bpm,
                    length_secs, bitrate, channels, filetype, filename, play_count, last_played,
                    comment, album_artist, disc_num, disc_total, composer, original_artist,
                    copyright, url, encoded_by, lyric, artwork_path, last_scanned,
                    sample_rate, file_size, file_mtime, added_at, bitrate_mode,
                    rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak
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
        // Only delete cached extractions. artwork_path can now point at the
        // user's own folder image (F2 fallback) — deleting that would be
        // destroying their file, not our cache.
        if let Ok(track) = self.track_by_path(path) {
            if let Some(ref old_art) = track.artwork_path {
                let cache_root = dirs::cache_dir()
                    .unwrap_or_else(std::env::temp_dir)
                    .join("sparkamp");
                if std::path::Path::new(old_art).starts_with(&cache_root) {
                    let _ = std::fs::remove_file(old_art);
                }
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

    // -----------------------------------------------------------------------
    // Album gallery (phase 11): grouping + per-album track fetch
    // -----------------------------------------------------------------------

    /// Lean projection over every track, ordered deterministically so that
    /// [`Self::albums`] can fold rows into groups (and pick a stable
    /// "first" row per group for representative artwork) with a single pass.
    fn album_rows(&self) -> Result<Vec<AlbumRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(artist,''), COALESCE(album,''), COALESCE(album_artist,''),
                    year, artwork_path
             FROM tracks
             ORDER BY LOWER(COALESCE(album_artist,'')), LOWER(COALESCE(artist,'')),
                      LOWER(COALESCE(album,'')), COALESCE(disc_num,0), COALESCE(track_num,0)",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(AlbumRow {
                artist: r.get(0)?,
                album: r.get(1)?,
                album_artist: r.get(2)?,
                year: r.get(3)?,
                artwork_path: r.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// Fold every track into album groups for the gallery grid.
    ///
    /// Grouping key is `(album.trim().lowercase(), effective_album_artist(...).lowercase())`
    /// — the album-artist toggle is resolved through
    /// [`crate::play_stats::effective_album_artist`], the single source of
    /// truth shared with the Media Library's own display/grouping, so SQL
    /// never re-derives that logic. Blank/whitespace albums collapse into
    /// one `is_no_album` bucket, always sorted last regardless of `sort`.
    pub fn albums(&self, sort: AlbumSort, artist_as_album: bool) -> Result<Vec<AlbumGroup>> {
        struct Acc {
            album: String,
            album_artist: String,
            year: Option<i64>,
            track_count: i64,
            artwork_path: Option<String>,
            is_no_album: bool,
        }

        let no_album_key = (String::new(), String::new());
        let mut groups: HashMap<(String, String), Acc> = HashMap::new();

        for row in self.album_rows()? {
            let is_no_album = row.album.trim().is_empty();
            let eff_artist = effective_album_artist(&row.artist, &row.album_artist, artist_as_album);
            let key = if is_no_album {
                no_album_key.clone()
            } else {
                (row.album.trim().to_lowercase(), eff_artist.to_lowercase())
            };

            let acc = groups.entry(key).or_insert_with(|| Acc {
                album: if is_no_album {
                    String::new()
                } else {
                    row.album.trim().to_string()
                },
                album_artist: if is_no_album {
                    String::new()
                } else {
                    eff_artist.clone()
                },
                year: None,
                track_count: 0,
                artwork_path: None,
                is_no_album,
            });

            acc.track_count += 1;
            if let Some(y) = row.year {
                acc.year = Some(acc.year.map_or(y, |existing| existing.min(y)));
            }
            if acc.artwork_path.is_none() {
                acc.artwork_path = row.artwork_path.clone();
            }
        }

        let mut no_album_bucket = None;
        let mut result: Vec<AlbumGroup> = Vec::with_capacity(groups.len());
        for (key, acc) in groups {
            let group = AlbumGroup {
                album: acc.album,
                album_artist: acc.album_artist,
                year: acc.year,
                track_count: acc.track_count,
                artwork_path: acc.artwork_path,
                is_no_album: acc.is_no_album,
            };
            if key == no_album_key {
                no_album_bucket = Some(group);
            } else {
                result.push(group);
            }
        }

        match sort {
            AlbumSort::Artist => result.sort_by(|a, b| {
                (a.album_artist.to_lowercase(), a.album.to_lowercase())
                    .cmp(&(b.album_artist.to_lowercase(), b.album.to_lowercase()))
            }),
            AlbumSort::Album => result.sort_by(|a, b| {
                (a.album.to_lowercase(), a.album_artist.to_lowercase())
                    .cmp(&(b.album.to_lowercase(), b.album_artist.to_lowercase()))
            }),
            AlbumSort::Year => result.sort_by(|a, b| {
                let ay = a.year.unwrap_or(i64::MAX);
                let by = b.year.unwrap_or(i64::MAX);
                (ay, a.album_artist.to_lowercase(), a.album.to_lowercase()).cmp(&(
                    by,
                    b.album_artist.to_lowercase(),
                    b.album.to_lowercase(),
                ))
            }),
        }

        if let Some(bucket) = no_album_bucket {
            result.push(bucket);
        }

        Ok(result)
    }

    /// Fetch every track belonging to one album, for the gallery's
    /// album-detail view.
    ///
    /// Matches `(album, album_artist)` against the requested pair using the
    /// same `effective_album_artist` resolution as [`Self::albums`] — kept in
    /// Rust rather than SQL for the same single-source-of-truth reason.
    /// Ordered by `(disc_num, track_num, filename)`, with NULL `disc_num`
    /// treated as 0 and NULL `track_num` sorted after any known track number.
    pub fn album_tracks(
        &self,
        album: &str,
        album_artist: &str,
        artist_as_album: bool,
    ) -> Result<Vec<LibTrack>> {
        let sql = "SELECT id, path, artist, title, album, track_num, genre, year, bpm,
                    length_secs, bitrate, channels, filetype, filename, play_count, last_played,
                    comment, album_artist, disc_num, disc_total, composer, original_artist,
                    copyright, url, encoded_by, lyric, artwork_path, last_scanned,
                    sample_rate, file_size, file_mtime, added_at, bitrate_mode,
                    rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak
             FROM tracks";
        let mut stmt = self.conn.prepare(sql)?;
        let mut tracks = Self::collect_tracks(&mut stmt, [])?;

        let want_album = album.trim().to_lowercase();
        let want_artist = album_artist.trim().to_lowercase();
        tracks.retain(|t| {
            let t_album = t.album.as_deref().unwrap_or("").trim().to_lowercase();
            let eff = effective_album_artist(
                t.artist.as_deref().unwrap_or(""),
                t.album_artist.as_deref().unwrap_or(""),
                artist_as_album,
            )
            .to_lowercase();
            t_album == want_album && eff == want_artist
        });

        tracks.sort_by(|a, b| {
            let ad = a.disc_num.unwrap_or(0);
            let bd = b.disc_num.unwrap_or(0);
            ad.cmp(&bd)
                .then_with(|| match (a.track_num, b.track_num) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                })
                .then_with(|| a.filename.to_lowercase().cmp(&b.filename.to_lowercase()))
        });

        Ok(tracks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_lib() -> (MediaLibrary, tempfile::NamedTempFile) {
        let db_file = tempfile::NamedTempFile::with_suffix(".db").unwrap();
        let lib = MediaLibrary::open_at(db_file.path()).unwrap();
        (lib, db_file)
    }

    /// Insert a track row directly via SQL, bypassing the scan/probe path —
    /// tests need exact control over album/album_artist/year/disc/track
    /// fields that a fake audio file wouldn't yield through `upsert_track`.
    #[allow(clippy::too_many_arguments)]
    fn insert_track(
        lib: &MediaLibrary,
        path: &str,
        filename: &str,
        artist: &str,
        album: &str,
        album_artist: &str,
        track_num: Option<i64>,
        disc_num: Option<i64>,
        year: Option<i64>,
        artwork_path: Option<&str>,
    ) {
        lib.conn
            .execute(
                "INSERT INTO tracks
                    (path, filename, artist, album, album_artist, track_num, disc_num, year, artwork_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    path,
                    filename,
                    artist,
                    album,
                    album_artist,
                    track_num,
                    disc_num,
                    year,
                    artwork_path
                ],
            )
            .unwrap();
    }

    // (a) two artists sharing an album name → two groups.
    #[test]
    fn two_artists_sharing_album_name_produce_two_groups() {
        let (lib, _db) = temp_lib();
        insert_track(
            &lib, "/m/a1.mp3", "a1.mp3", "Artist A", "Best Hits", "Artist A", Some(1), None,
            None, None,
        );
        insert_track(
            &lib, "/m/a2.mp3", "a2.mp3", "Artist B", "Best Hits", "Artist B", Some(1), None,
            None, None,
        );

        let groups = lib.albums(AlbumSort::Album, false).unwrap();
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| g.album == "Best Hits"));
        let mut artists: Vec<&str> = groups.iter().map(|g| g.album_artist.as_str()).collect();
        artists.sort();
        assert_eq!(artists, vec!["Artist A", "Artist B"]);
    }

    // (b) multi-disc album folds to one group with track_count summed.
    #[test]
    fn multi_disc_album_folds_into_one_group_with_summed_track_count() {
        let (lib, _db) = temp_lib();
        insert_track(
            &lib, "/m/d1t1.mp3", "d1t1.mp3", "Band", "Big Album", "Band", Some(1), Some(1),
            None, None,
        );
        insert_track(
            &lib, "/m/d1t2.mp3", "d1t2.mp3", "Band", "Big Album", "Band", Some(2), Some(1),
            None, None,
        );
        insert_track(
            &lib, "/m/d2t1.mp3", "d2t1.mp3", "Band", "Big Album", "Band", Some(1), Some(2),
            None, None,
        );

        let groups = lib.albums(AlbumSort::Album, false).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].track_count, 3);
    }

    // (c) artist_as_album=true merges tracks whose album_artist is blank but
    // artist matches; artist_as_album=false splits them into two groups.
    #[test]
    fn artist_as_album_toggle_merges_or_splits_blank_album_artist_tracks() {
        let (lib, _db) = temp_lib();
        insert_track(
            &lib,
            "/m/c1.mp3",
            "c1.mp3",
            "Various Artists",
            "Compilation",
            "Various Artists",
            Some(1),
            None,
            None,
            None,
        );
        insert_track(
            &lib, "/m/c2.mp3", "c2.mp3", "Various Artists", "Compilation", "", Some(2), None,
            None, None,
        );

        let merged = lib.albums(AlbumSort::Album, true).unwrap();
        assert_eq!(merged.len(), 1, "artist_as_album=true should merge into one group");
        assert_eq!(merged[0].track_count, 2);

        let split = lib.albums(AlbumSort::Album, false).unwrap();
        assert_eq!(split.len(), 2, "artist_as_album=false should split into two groups");
    }

    // (d) blank-album bucket present, is_no_album, sorted last.
    #[test]
    fn blank_album_forms_single_no_album_bucket_sorted_last() {
        let (lib, _db) = temp_lib();
        insert_track(
            &lib, "/m/n1.mp3", "n1.mp3", "Artist", "", "Artist", None, None, None, None,
        );
        insert_track(
            &lib, "/m/n2.mp3", "n2.mp3", "Other", "   ", "Other", None, None, None, None,
        );
        insert_track(
            &lib, "/m/z1.mp3", "z1.mp3", "Artist", "Zebra Album", "Artist", Some(1), None,
            None, None,
        );
        insert_track(
            &lib, "/m/a1.mp3", "a1.mp3", "Artist", "Apple Album", "Artist", Some(1), None,
            None, None,
        );

        let groups = lib.albums(AlbumSort::Album, false).unwrap();
        assert_eq!(groups.len(), 3);
        let last = groups.last().unwrap();
        assert!(last.is_no_album);
        assert_eq!(last.album, "");
        assert_eq!(last.track_count, 2, "both blank-album tracks fold into one bucket");
        assert!(!groups[0].is_no_album && !groups[1].is_no_album);
    }

    // (e) representative artwork = first non-NULL in deterministic group order.
    #[test]
    fn representative_artwork_is_first_non_null_in_deterministic_order() {
        let (lib, _db) = temp_lib();
        insert_track(
            &lib, "/m/e1.mp3", "e1.mp3", "Artist", "Art Album", "Artist", Some(1), None, None,
            None,
        );
        insert_track(
            &lib,
            "/m/e2.mp3",
            "e2.mp3",
            "Artist",
            "Art Album",
            "Artist",
            Some(2),
            None,
            None,
            Some("/art/first.jpg"),
        );
        insert_track(
            &lib,
            "/m/e3.mp3",
            "e3.mp3",
            "Artist",
            "Art Album",
            "Artist",
            Some(3),
            None,
            None,
            Some("/art/second.jpg"),
        );

        let g1 = lib.albums(AlbumSort::Album, false).unwrap();
        let g2 = lib.albums(AlbumSort::Album, false).unwrap();
        assert_eq!(g1[0].artwork_path.as_deref(), Some("/art/first.jpg"));
        assert_eq!(g1, g2, "repeated calls must be deterministic");
    }

    // (f) year = min non-NULL year in the group.
    #[test]
    fn year_is_minimum_non_null_year_in_group() {
        let (lib, _db) = temp_lib();
        insert_track(
            &lib, "/m/f1.mp3", "f1.mp3", "Artist", "Timeless", "Artist", Some(1), None,
            Some(2005), None,
        );
        insert_track(
            &lib, "/m/f2.mp3", "f2.mp3", "Artist", "Timeless", "Artist", Some(2), None,
            Some(2001), None,
        );
        insert_track(
            &lib, "/m/f3.mp3", "f3.mp3", "Artist", "Timeless", "Artist", Some(3), None, None,
            None,
        );

        let groups = lib.albums(AlbumSort::Album, false).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].year, Some(2001));
    }

    // (g) each AlbumSort orders as specified.
    #[test]
    fn album_sort_artist_orders_by_album_artist_then_album() {
        let (lib, _db) = temp_lib();
        insert_track(
            &lib, "/m/g1.mp3", "g1.mp3", "Zed", "Zeta", "Zed", Some(1), None, None, None,
        );
        insert_track(
            &lib, "/m/g2.mp3", "g2.mp3", "Amy", "Beta", "Amy", Some(1), None, None, None,
        );
        insert_track(
            &lib, "/m/g3.mp3", "g3.mp3", "Amy", "Alpha", "Amy", Some(1), None, None, None,
        );

        let groups = lib.albums(AlbumSort::Artist, false).unwrap();
        let pairs: Vec<(String, String)> = groups
            .iter()
            .map(|g| (g.album_artist.clone(), g.album.clone()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("Amy".to_string(), "Alpha".to_string()),
                ("Amy".to_string(), "Beta".to_string()),
                ("Zed".to_string(), "Zeta".to_string()),
            ]
        );
    }

    #[test]
    fn album_sort_album_orders_by_album_then_album_artist() {
        let (lib, _db) = temp_lib();
        insert_track(
            &lib, "/m/h1.mp3", "h1.mp3", "Zed", "Zeta", "Zed", Some(1), None, None, None,
        );
        insert_track(
            &lib, "/m/h2.mp3", "h2.mp3", "Amy", "Alpha", "Amy", Some(1), None, None, None,
        );

        let groups = lib.albums(AlbumSort::Album, false).unwrap();
        let albums: Vec<&str> = groups.iter().map(|g| g.album.as_str()).collect();
        assert_eq!(albums, vec!["Alpha", "Zeta"]);
    }

    #[test]
    fn album_sort_year_orders_ascending_with_unknown_last() {
        let (lib, _db) = temp_lib();
        insert_track(
            &lib, "/m/i1.mp3", "i1.mp3", "Artist", "NoYear", "Artist", Some(1), None, None,
            None,
        );
        insert_track(
            &lib, "/m/i2.mp3", "i2.mp3", "Artist", "Old", "Artist", Some(1), None, Some(1990),
            None,
        );
        insert_track(
            &lib, "/m/i3.mp3", "i3.mp3", "Artist", "New", "Artist", Some(1), None, Some(2020),
            None,
        );

        let groups = lib.albums(AlbumSort::Year, false).unwrap();
        let albums: Vec<&str> = groups.iter().map(|g| g.album.as_str()).collect();
        assert_eq!(albums, vec!["Old", "New", "NoYear"]);
    }

    // (h) album_tracks ordering: disc 2 track 1 after disc 1 track 9, NULL
    // track_num last by filename.
    #[test]
    fn album_tracks_orders_by_disc_then_track_num_then_filename_with_nulls_last() {
        let (lib, _db) = temp_lib();
        insert_track(
            &lib, "/m/j_d1t9.mp3", "d1t9.mp3", "Band", "Opus", "Band", Some(9), Some(1), None,
            None,
        );
        insert_track(
            &lib, "/m/j_d2t1.mp3", "d2t1.mp3", "Band", "Opus", "Band", Some(1), Some(2), None,
            None,
        );
        insert_track(
            &lib, "/m/j_dnb.mp3", "zz_b.mp3", "Band", "Opus", "Band", None, Some(1), None, None,
        );
        insert_track(
            &lib, "/m/j_dna.mp3", "aa_a.mp3", "Band", "Opus", "Band", None, Some(1), None, None,
        );

        let tracks = lib.album_tracks("Opus", "Band", false).unwrap();
        let names: Vec<&str> = tracks.iter().map(|t| t.filename.as_str()).collect();
        assert_eq!(names, vec!["d1t9.mp3", "aa_a.mp3", "zz_b.mp3", "d2t1.mp3"]);
    }

    // album_tracks: the no-album bucket ("") and blank album_artist both
    // resolve correctly through the same trim/lowercase matching rule.
    #[test]
    fn album_tracks_matches_no_album_bucket_with_empty_query() {
        let (lib, _db) = temp_lib();
        insert_track(
            &lib, "/m/k1.mp3", "k1.mp3", "Artist", "", "Artist", Some(1), None, None, None,
        );
        insert_track(
            &lib, "/m/k2.mp3", "k2.mp3", "Artist", "Real Album", "Artist", Some(1), None, None,
            None,
        );

        let tracks = lib.album_tracks("", "Artist", false).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].filename, "k1.mp3");
    }
}
