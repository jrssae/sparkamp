//! Persistence for external-device records and the per-file sync pairs that
//! record which library files were explicitly copied to which device.
//!
//! A pair exists ONLY for a file the user transferred through Sparkamp (either
//! direction). Coincidental same-named files never get a pair, so the sync
//! engine never touches them.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::MediaLibrary;

/// A remembered external device. `id` is the volume UUID when available, else
/// a generated marker-file id assigned by the detection layer.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceRecord {
    pub id: String,
    pub label: String,
    /// ISO-8601 UTC of the last time the device was seen connected.
    pub last_seen: Option<String>,
    /// Serialized per-device smart-sync rules; `None` until any are set.
    pub smart_rules: Option<String>,
}

/// One file copied via Sparkamp between the library and a device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncPair {
    pub device_id: String,
    /// Path on the device, relative to its mount root.
    pub device_relpath: String,
    pub library_path: String,
    /// Hash of the normalized tags at the last successful sync (written by the
    /// sync engine in a later phase; stored verbatim here).
    pub baseline_tag_hash: String,
    pub baseline_rating: i64,
    pub baseline_playcount: i64,
    pub last_sync_at: Option<String>,
}

/// The state of one library playlist as last synced to a device, used to tell
/// which side (computer or device) changed a playlist's contents/name.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistBaseline {
    pub device_id: String,
    pub library_playlist_id: i64,
    /// The playlist filename on the device at last sync (e.g. "Sync Test.m3u8").
    pub device_filename: String,
    /// Hash of the agreed ordered entry list (basenames) at last sync.
    pub entries_hash: String,
    pub last_sync_at: Option<String>,
}

// The macOS bin gates out the GTK/FFI callers of these methods; mirror the
// dead-code allow used on the other media_library impl blocks.
#[allow(dead_code)]
impl MediaLibrary {
    /// Insert or refresh the sync baseline for one library playlist on a device.
    pub fn upsert_playlist_baseline(&self, b: &PlaylistBaseline) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO device_playlist_baselines
                    (device_id, library_playlist_id, device_filename, entries_hash, last_sync_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(device_id, library_playlist_id) DO UPDATE SET
                     device_filename = excluded.device_filename,
                     entries_hash    = excluded.entries_hash,
                     last_sync_at    = excluded.last_sync_at",
                params![
                    b.device_id,
                    b.library_playlist_id,
                    b.device_filename,
                    b.entries_hash,
                    b.last_sync_at
                ],
            )
            .context("upsert_playlist_baseline")?;
        Ok(())
    }

    /// All playlist baselines recorded for a device.
    pub fn playlist_baselines_for_device(&self, device_id: &str) -> Result<Vec<PlaylistBaseline>> {
        let mut stmt = self.conn.prepare(
            "SELECT device_id, library_playlist_id, device_filename, entries_hash, last_sync_at
             FROM device_playlist_baselines WHERE device_id = ?1",
        )?;
        let rows = stmt.query_map(params![device_id], |row| {
            Ok(PlaylistBaseline {
                device_id: row.get(0)?,
                library_playlist_id: row.get(1)?,
                device_filename: row.get(2)?,
                entries_hash: row.get(3)?,
                last_sync_at: row.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("playlist_baselines_for_device")
    }

    /// Remove a playlist baseline (e.g. when the playlist no longer exists).
    pub fn delete_playlist_baseline(&self, device_id: &str, library_playlist_id: i64) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM device_playlist_baselines
                 WHERE device_id = ?1 AND library_playlist_id = ?2",
                params![device_id, library_playlist_id],
            )
            .context("delete_playlist_baseline")?;
        Ok(())
    }
    /// Insert a device, or update its label/last_seen/rules if `id` exists.
    pub fn upsert_device(&self, dev: &DeviceRecord) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO devices (id, label, last_seen, smart_rules)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET
                     label       = excluded.label,
                     last_seen   = excluded.last_seen,
                     smart_rules = excluded.smart_rules",
                params![dev.id, dev.label, dev.last_seen, dev.smart_rules],
            )
            .context("upsert_device")?;
        Ok(())
    }

    /// Fetch a device record by id, or `None` when it has never been seen.
    pub fn get_device(&self, id: &str) -> Result<Option<DeviceRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, label, last_seen, smart_rules FROM devices WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(DeviceRecord {
                id: row.get(0)?,
                label: row.get(1)?,
                last_seen: row.get(2)?,
                smart_rules: row.get(3)?,
            })),
            None => Ok(None),
        }
    }

    /// Create a sync pair, or replace it when `(device_id, device_relpath)`
    /// already exists (e.g. re-copying or refreshing the baseline after sync).
    pub fn upsert_sync_pair(&self, pair: &SyncPair) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO device_sync_pairs
                    (device_id, device_relpath, library_path,
                     baseline_tag_hash, baseline_rating, baseline_playcount, last_sync_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(device_id, device_relpath) DO UPDATE SET
                     library_path       = excluded.library_path,
                     baseline_tag_hash  = excluded.baseline_tag_hash,
                     baseline_rating    = excluded.baseline_rating,
                     baseline_playcount = excluded.baseline_playcount,
                     last_sync_at       = excluded.last_sync_at",
                params![
                    pair.device_id,
                    pair.device_relpath,
                    pair.library_path,
                    pair.baseline_tag_hash,
                    pair.baseline_rating,
                    pair.baseline_playcount,
                    pair.last_sync_at
                ],
            )
            .context("upsert_sync_pair")?;
        Ok(())
    }

    /// All pairs for a device, ordered by on-device path.
    pub fn sync_pairs_for_device(&self, device_id: &str) -> Result<Vec<SyncPair>> {
        let mut stmt = self.conn.prepare(
            "SELECT device_id, device_relpath, library_path, baseline_tag_hash,
                    baseline_rating, baseline_playcount, last_sync_at
             FROM device_sync_pairs WHERE device_id = ?1
             ORDER BY device_relpath",
        )?;
        let rows = stmt.query_map(params![device_id], Self::row_to_pair)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("sync_pairs_for_device")
    }

    /// All pairs whose library side is `library_path` (used when a library file
    /// is removed, to offer deleting the device copies).
    pub fn sync_pairs_for_library_path(&self, library_path: &str) -> Result<Vec<SyncPair>> {
        let mut stmt = self.conn.prepare(
            "SELECT device_id, device_relpath, library_path, baseline_tag_hash,
                    baseline_rating, baseline_playcount, last_sync_at
             FROM device_sync_pairs WHERE library_path = ?1 ORDER BY device_id, device_relpath",
        )?;
        let rows = stmt.query_map(params![library_path], Self::row_to_pair)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("sync_pairs_for_library_path")
    }

    /// Distinct library paths recorded for a device relpath, across *all*
    /// device ids. Lets sync recover the exact source a device file was copied
    /// from even when the device's id changed (e.g. an MTP re-detect), instead
    /// of guessing a same-named library file.
    pub fn library_paths_for_device_relpath(&self, device_relpath: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT library_path FROM device_sync_pairs WHERE device_relpath = ?1",
        )?;
        let rows = stmt.query_map(params![device_relpath], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("library_paths_for_device_relpath")
    }

    /// Remove a single pair. Does nothing if it does not exist.
    pub fn delete_sync_pair(&self, device_id: &str, device_relpath: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM device_sync_pairs
                 WHERE device_id = ?1 AND device_relpath = ?2",
                params![device_id, device_relpath],
            )
            .context("delete_sync_pair")?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Volume grants
    //
    // Under the App Sandbox a mount path grants nothing on its own. Reading
    // anything under /Volumes needs a security-scoped bookmark, and a
    // bookmark is only obtainable while the user's own pick is live. Measured
    // against a bundle signed with the shipping entitlements: every USB
    // volume and every optical data disc returns EPERM on `read_dir`, with
    // `files.removable-media.read-write` requested and granted. That
    // entitlement does not do what its name suggests.
    //
    // Keyed by volume rather than by device, because a data disc needs
    // exactly the same grant as a USB stick.
    // -----------------------------------------------------------------------

    /// Remember the user's grant to read `mount`.
    ///
    /// Must be called while the grant is live, which means immediately after
    /// the user picked the volume. Later is too late: the bookmark would be
    /// made without the access it is supposed to capture.
    ///
    /// Returns false outside a sandbox, where there is no bookmark to take
    /// and nothing to remember. That is not a failure; the volume is already
    /// readable.
    pub fn grant_volume(&self, volume_id: &str, label: &str, mount: &Path) -> Result<bool> {
        let Some(bookmark) = crate::sandbox::bookmark(mount) else {
            return Ok(false);
        };
        self.conn
            .execute(
                "INSERT INTO volume_grants (volume_id, label, bookmark)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(volume_id) DO UPDATE SET label = ?2, bookmark = ?3",
                params![volume_id, label, bookmark],
            )
            .context("grant_volume")?;
        Ok(true)
    }

    /// Whether a grant is already stored for this volume.
    pub fn has_volume_grant(&self, volume_id: &str) -> Result<bool> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM volume_grants WHERE volume_id = ?1",
                params![volume_id],
                |row| row.get(0),
            )
            .context("has_volume_grant")?;
        Ok(n > 0)
    }

    /// Re-hold every stored grant for the life of the process.
    ///
    /// The mirror of `restore_folder_access`, and called at the same point in
    /// startup. Returns the labels of grants that would not resolve, which is
    /// the ordinary answer for a volume that is simply not plugged in: the
    /// row is kept, because it will resolve again when it comes back.
    pub fn restore_volume_access(&self) -> Result<Vec<String>> {
        let rows: Vec<(String, String, Vec<u8>)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT volume_id, label, bookmark FROM volume_grants")?;
            let mapped = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            mapped
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("restore_volume_access query")?
        };

        let mut unavailable = Vec::new();
        for (volume_id, label, bookmark) in rows {
            match crate::sandbox::hold(&bookmark) {
                Some((path, stale)) => {
                    // A stale bookmark still resolved, so the volume is here
                    // and readable. Re-take it from where it landed, which is
                    // what stops it going stale again on the next launch.
                    if stale {
                        let _ = self.grant_volume(&volume_id, &label, &path);
                    }
                }
                None => unavailable.push(if label.is_empty() {
                    volume_id.clone()
                } else {
                    label.clone()
                }),
            }
        }
        Ok(unavailable)
    }

    /// Drop a stored grant, for a volume the user no longer wants remembered.
    pub fn forget_volume_grant(&self, volume_id: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM volume_grants WHERE volume_id = ?1",
                params![volume_id],
            )
            .context("forget_volume_grant")?;
        Ok(())
    }

    fn row_to_pair(row: &rusqlite::Row<'_>) -> rusqlite::Result<SyncPair> {
        Ok(SyncPair {
            device_id: row.get(0)?,
            device_relpath: row.get(1)?,
            library_path: row.get(2)?,
            baseline_tag_hash: row.get(3)?,
            baseline_rating: row.get(4)?,
            baseline_playcount: row.get(5)?,
            last_sync_at: row.get(6)?,
        })
    }
}
