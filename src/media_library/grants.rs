//! Access to files and folders the user handed Sparkamp outside the library.
//!
//! Under the App Sandbox the app can read what the user picks in a panel or
//! drops on a window, for this launch only. Library folders keep that access
//! across launches through `folders.bookmark`, and volumes through
//! `volume_grants`. This is the same thing for everything else: songs opened
//! or dropped into the active playlist, the rip folder, playlist files saved
//! through the Save panel, and folders granted through the Allow Access
//! prompt.
//!
//! Two lifetimes, because the two kinds of grant are not alike:
//!
//! - **While used** (`kept = 0`). A song or folder added to the active
//!   playlist. Nobody chose to give Sparkamp that folder for good; they
//!   dragged some songs in. The grant goes once nothing in the playlist is
//!   under it, which [`MediaLibrary::prune_path_grants`] checks at startup.
//! - **Kept** (`kept = 1`). Something the user granted on purpose: the rip
//!   folder, a saved playlist, a folder from the Allow Access prompt.
//!
//! Outside a sandbox `sandbox::bookmark` answers `None` and nothing is
//! stored, because a path is already all the access there is.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::params;

use super::MediaLibrary;

/// `?1` is the path asked about. A row covers it when the row's path is the
/// same, or a folder it lies under. `substr` rather than `LIKE`, because a
/// `%` or `_` in a folder name would turn `LIKE` into a pattern.
const COVERS: &str = "(?1 = path OR substr(?1, 1, length(path) + 1) = path || '/')";

impl MediaLibrary {
    /// Remember access to `path` across launches, if nothing already covers it.
    ///
    /// Must be called while the user's grant is live, which means during the
    /// same launch as the pick or drop that granted it. Returns whether a
    /// grant was stored. False is ordinary: outside a sandbox, or when a
    /// library folder or an earlier grant already covers the path.
    pub fn grant_path(&self, path: &Path, kept: bool) -> Result<bool> {
        if !crate::sandbox::is_app_sandboxed() || self.path_is_granted(path, kept)? {
            return Ok(false);
        }
        let Some(bookmark) = crate::sandbox::bookmark(path) else {
            return Ok(false);
        };
        self.store_path_grant(path, &bookmark, kept)?;
        Ok(true)
    }

    /// Whether a library folder or a stored grant already covers `path`.
    ///
    /// A request for a kept grant counts only kept grants as cover. A
    /// while-used grant for the same folder would be pruned once the playlist
    /// moved on, taking the rip folder's access with it.
    pub fn path_is_granted(&self, path: &Path, kept: bool) -> Result<bool> {
        let Some(path) = path.to_str() else {
            return Ok(false);
        };
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM folders WHERE {COVERS})
                 OR EXISTS(SELECT 1 FROM path_grants WHERE (?2 = 0 OR kept = 1) AND {COVERS})"
        );
        let covered: bool = self
            .conn
            .query_row(&sql, params![path, kept], |row| row.get(0))
            .context("path_is_granted")?;
        Ok(covered)
    }

    /// Store a bookmark for `path`. A kept grant never drops back to
    /// while-used, so the same folder dragged in later stays kept.
    pub(crate) fn store_path_grant(&self, path: &Path, bookmark: &[u8], kept: bool) -> Result<()> {
        let Some(path) = path.to_str() else {
            return Ok(());
        };
        self.conn
            .execute(
                "INSERT INTO path_grants (path, bookmark, kept) VALUES (?1, ?2, ?3)
                 ON CONFLICT(path) DO UPDATE SET bookmark = ?2, kept = MAX(kept, ?3)",
                params![path, bookmark, kept],
            )
            .context("store_path_grant")?;
        Ok(())
    }

    /// Hold every stored grant for the life of the process, and report the
    /// paths that would not resolve.
    ///
    /// The mirror of `restore_folder_access`, and called at the same point in
    /// startup. A grant that does not resolve keeps its row: a folder on an
    /// unplugged drive resolves again when the drive comes back, and the
    /// while-used ones are cleared by [`Self::prune_path_grants`] anyway.
    pub fn restore_path_access(&self) -> Result<Vec<String>> {
        let rows: Vec<(String, Vec<u8>)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT path, bookmark FROM path_grants ORDER BY path")?;
            let mapped = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            mapped
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("restore_path_access query")?
        };
        let mut unreachable = Vec::new();
        for (path, bookmark) in rows {
            match crate::sandbox::hold(&bookmark) {
                Some((resolved, stale)) => {
                    if stale {
                        self.refresh_path_grant(&path, &resolved)?;
                    }
                }
                None => unreachable.push(path),
            }
        }
        Ok(unreachable)
    }

    /// Re-make a stale bookmark from where it resolved, and move the row with
    /// it, so the next launch does not lean on a stale bookmark resolving
    /// twice.
    fn refresh_path_grant(&self, old: &str, resolved: &Path) -> Result<()> {
        let (Some(new), Some(bookmark)) = (resolved.to_str(), crate::sandbox::bookmark(resolved))
        else {
            return Ok(());
        };
        self.conn
            .execute(
                "UPDATE OR REPLACE path_grants SET path = ?1, bookmark = ?2 WHERE path = ?3",
                params![new, bookmark, old],
            )
            .context("refresh_path_grant")?;
        Ok(())
    }

    /// Drop the while-used grants nothing in `in_use` lies under. Kept grants
    /// are never touched. Returns how many went.
    ///
    /// Run at startup with the restored playlist's paths. A grant only
    /// matters while something needs it, and without this every song ever
    /// dragged in would leave a bookmark behind for good.
    pub fn prune_path_grants(&self, in_use: &[PathBuf]) -> Result<usize> {
        let unused: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT path FROM path_grants WHERE kept = 0")?;
            let mapped = stmt.query_map([], |row| row.get::<_, String>(0))?;
            mapped
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("prune_path_grants query")?
                .into_iter()
                .filter(|grant| !in_use.iter().any(|p| p.starts_with(grant)))
                .collect()
        };
        for path in &unused {
            self.conn
                .execute("DELETE FROM path_grants WHERE path = ?1", params![path])
                .context("prune_path_grants delete")?;
        }
        Ok(unused.len())
    }
}
