//! Track queries — listing, search, sorted views — and track removal
//! (hard delete, soft delete, purge) plus artwork cache maintenance.

use anyhow::Result;
use rusqlite::params;

use crate::tags::read_track_tags;

use super::{LibTrack, MediaLibrary};

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
}
