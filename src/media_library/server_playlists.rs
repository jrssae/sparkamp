//! Server playlists: kept only in the database, refreshed with each catalog
//! update, listed beside the playlist files.
//!
//! A server playlist's id in the lists is the negative of its row id, as a
//! server song's is, so it can never meet a playlist file's id. Its entries
//! name songs by server path, never by song ID: Navidrome derives song IDs
//! from tags, so a retag would orphan them. Each entry resolves the way a
//! `#SPARKAMP-SONG` line does, to the local copy when the song has one.
//!
//! Nothing here writes to a server. Until playlist changes are sent, a server
//! playlist is read-only in Sparkamp and every change is refused.

use anyhow::{bail, Result};
use rusqlite::params;
use std::collections::HashMap;

use super::servers::path_key;
use super::{LibPlaylist, LibTrack, MediaLibrary};
use crate::servers::api::{ServerPlaylist, ServerSong};
use crate::servers::uri::{playlist_uri, song_uri};

/// Where a listed playlist lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistSource {
    /// A playlist file on this computer.
    Local,
    /// A playlist on the server with this id.
    Server(String),
}

/// One playlist as the playlist lists show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedPlaylist {
    /// A playlist file's row id, or the negative row id of a server playlist.
    pub id: i64,
    pub name: String,
    pub source: PlaylistSource,
}

impl MediaLibrary {
    pub(super) fn init_server_playlist_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS server_playlists (
                id          INTEGER PRIMARY KEY,
                server_id   TEXT NOT NULL,
                playlist_id TEXT NOT NULL,
                name        TEXT NOT NULL DEFAULT '',
                comment     TEXT NOT NULL DEFAULT '',
                owner       TEXT NOT NULL DEFAULT '',
                readonly    INTEGER NOT NULL DEFAULT 0,
                song_count  INTEGER NOT NULL DEFAULT 0,
                -- The server's `changed` stamp the entries below were read
                -- at; NULL until they have been, so the next update asks.
                changed     TEXT,
                UNIQUE (server_id, playlist_id)
            );
            CREATE TABLE IF NOT EXISTS server_playlist_entries (
                playlist INTEGER NOT NULL,
                position INTEGER NOT NULL,
                path_key TEXT NOT NULL,
                -- Shown when the catalog no longer has the song.
                title    TEXT NOT NULL DEFAULT '',
                artist   TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (playlist, position)
            );
            CREATE TRIGGER IF NOT EXISTS trg_server_playlists_gone AFTER DELETE ON server_playlists BEGIN
                DELETE FROM server_playlist_entries WHERE playlist = old.id;
            END;
            ",
        )?;
        Ok(())
    }

    /// Every playlist, files and server playlists together, by name
    /// ignoring case. [`Self::all_playlists`] stays files only: devices and
    /// saving work on files.
    pub fn listed_playlists(&self) -> Result<Vec<ListedPlaylist>> {
        let mut out: Vec<ListedPlaylist> = self
            .all_playlists()?
            .into_iter()
            .map(|p| ListedPlaylist { id: p.id, name: p.name, source: PlaylistSource::Local })
            .collect();
        let mut stmt = self.conn.prepare("SELECT id, name, server_id FROM server_playlists")?;
        let servers = stmt.query_map([], |r| {
            Ok(ListedPlaylist {
                id: -r.get::<_, i64>(0)?,
                name: r.get(1)?,
                source: PlaylistSource::Server(r.get(2)?),
            })
        })?;
        for p in servers {
            out.push(p?);
        }
        out.sort_by_key(|p| p.name.to_lowercase());
        Ok(out)
    }

    /// The `changed` stamp each of `server_id`'s playlists was last read
    /// at, by playlist id. `None` for one whose songs were never read.
    pub fn server_playlist_stamps(&self, server_id: &str) -> Result<HashMap<String, Option<String>>> {
        let mut stmt =
            self.conn.prepare("SELECT playlist_id, changed FROM server_playlists WHERE server_id = ?1")?;
        let rows = stmt.query_map(params![server_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Make `heads` the whole of `server_id`'s playlists. A playlist with
    /// an entry in `songs` (by playlist id) takes those songs and its
    /// `changed` stamp; any other keeps the songs it had. Playlists missing
    /// from `heads` are dropped. Returns whether the lists would show
    /// anything different: a playlist added, gone or renamed, or songs read.
    pub fn store_server_playlists(
        &self,
        server_id: &str,
        heads: &[ServerPlaylist],
        songs: &HashMap<String, Vec<ServerSong>>,
    ) -> Result<bool> {
        let before = self.server_playlist_names(server_id)?;
        let tx = self.conn.unchecked_transaction()?;
        let keep: Vec<&str> = heads.iter().map(|h| h.id.as_str()).collect();
        let stale: Vec<String> = self
            .server_playlist_stamps(server_id)?
            .into_keys()
            .filter(|id| !keep.contains(&id.as_str()))
            .collect();
        for id in stale {
            tx.execute(
                "DELETE FROM server_playlists WHERE server_id = ?1 AND playlist_id = ?2",
                params![server_id, id],
            )?;
        }
        for h in heads {
            tx.execute(
                "INSERT INTO server_playlists (server_id, playlist_id, name, comment, owner, readonly, song_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (server_id, playlist_id) DO UPDATE SET
                    name = excluded.name, comment = excluded.comment, owner = excluded.owner,
                    readonly = excluded.readonly, song_count = excluded.song_count",
                params![server_id, h.id, h.name, h.comment, h.owner, h.readonly, h.song_count as i64],
            )?;
            let Some(entries) = songs.get(&h.id) else { continue };
            let row: i64 = tx.query_row(
                "SELECT id FROM server_playlists WHERE server_id = ?1 AND playlist_id = ?2",
                params![server_id, h.id],
                |r| r.get(0),
            )?;
            tx.execute("DELETE FROM server_playlist_entries WHERE playlist = ?1", params![row])?;
            for (position, s) in entries.iter().enumerate() {
                tx.execute(
                    "INSERT INTO server_playlist_entries (playlist, position, path_key, title, artist)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![row, position as i64, path_key(s), s.title, s.artist],
                )?;
            }
            tx.execute("UPDATE server_playlists SET changed = ?1 WHERE id = ?2", params![h.changed, row])?;
        }
        tx.commit()?;
        Ok(!songs.is_empty() || self.server_playlist_names(server_id)? != before)
    }

    /// `(playlist id, name)` of each of `server_id`'s playlists, in id order.
    fn server_playlist_names(&self, server_id: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT playlist_id, name FROM server_playlists WHERE server_id = ?1 ORDER BY playlist_id",
        )?;
        let rows = stmt.query_map(params![server_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Server playlist `-id` as a playlist, its path a URI naming it.
    pub(super) fn server_playlist_by_id(&self, id: i64) -> Result<LibPlaylist> {
        Ok(self.conn.query_row(
            "SELECT server_id, playlist_id, name FROM server_playlists WHERE id = ?1",
            params![-id],
            |r| {
                let server: String = r.get(0)?;
                let playlist: String = r.get(1)?;
                Ok(LibPlaylist {
                    id,
                    path: playlist_uri(&server, &playlist),
                    name: r.get(2)?,
                    tracks: Vec::new(),
                })
            },
        )?)
    }

    /// Server playlist `-id`'s songs in order: each one's local copy when
    /// it has one, else the server copy, else (the catalog lacks it) a
    /// missing entry with its title and artist.
    pub(super) fn server_playlist_tracks(&self, id: i64) -> Result<Vec<LibTrack>> {
        let server: String =
            self.conn.query_row("SELECT server_id FROM server_playlists WHERE id = ?1", params![-id], |r| r.get(0))?;
        let mut stmt = self.conn.prepare(
            "SELECT path_key, title, artist FROM server_playlist_entries WHERE playlist = ?1 ORDER BY position",
        )?;
        let entries: Vec<(String, String, String)> = stmt
            .query_map(params![-id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut tracks = Vec::with_capacity(entries.len());
        for (key, title, artist) in entries {
            let uri = song_uri(&server, &key);
            match self.resolve_song_uri(&uri)? {
                Some(t) => tracks.push(t),
                None => tracks.push(LibTrack {
                    id: 0,
                    filename: key.rsplit('/').next().unwrap_or(&key).to_string(),
                    path: uri,
                    title: (!title.is_empty()).then_some(title),
                    artist: (!artist.is_empty()).then_some(artist),
                    ..LibTrack::default()
                }),
            }
        }
        Ok(tracks)
    }

    pub(super) fn forget_server_playlists(&self, server_id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM server_playlists WHERE server_id = ?1", params![server_id])?;
        Ok(())
    }
}

/// Refuse to change server playlist `id`: changes are not sent to servers
/// yet, and a change kept only here would be lost at the next update.
pub(super) fn refuse_server_playlist(id: i64) -> Result<()> {
    if id < 0 {
        bail!("server playlists cannot be changed in Sparkamp yet");
    }
    Ok(())
}
