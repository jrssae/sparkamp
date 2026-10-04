//! The catalog cache for Navidrome / OpenSubsonic servers.
//!
//! `tracks` keeps meaning "a file on this computer". Server songs live in
//! `server_tracks`, one row per song per server, keyed by server id and the
//! path the server reports. The server's song ID is an ordinary column,
//! updated in place: Navidrome derives it from tags, so it changes whenever a
//! file is retagged, and a row that changed identity with it would lose its
//! links and history.
//!
//! Design: docs/superpowers/specs/2026-09-30-server-support-design.md.

use anyhow::Result;
use rusqlite::params;

use super::{LibTrack, MediaLibrary, SortKeys};
use crate::servers::api::{ReplayGain, ServerSong};
use crate::servers::matcher::{LinkReason, LocalCandidate, PossibleMatch, ServerCandidate};
use crate::servers::merge::{
    self, CopyId, CopyState, Field, FieldOutcome, Fields, SongMerge, SongStatus, Value,
};
use std::collections::{HashMap, HashSet};
use crate::servers::normalize;

/// Songs added and updated by one page of a pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PageOutcome {
    pub added: usize,
    pub updated: usize,
}

/// What finishing a complete pull did.
#[derive(Debug, Clone, PartialEq)]
pub enum PullOutcome {
    /// Songs the server no longer has were removed from the cache.
    Removed(Vec<ServerTrackRow>),
    /// Too many songs would have disappeared at once, so nothing was removed.
    /// A missing NAS mount on the server looks exactly like a mass deletion,
    /// and only a person can tell them apart: see
    /// [`MediaLibrary::confirm_server_removals`].
    Held { would_remove: usize, cached: usize },
}

/// One cached server song.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerTrackRow {
    /// Stable row id; links and baselines refer to this, never to the song ID.
    pub id: i64,
    pub server_id: String,
    pub song: ServerSong,
    /// The cached cover thumbnail, once fetched.
    pub artwork_path: Option<String>,
}

/// One copy of a song: a local file (`tracks.id`) or a server copy
/// (`server_tracks.id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Member {
    Local(i64),
    Server(i64),
}

/// One linked copy with how it was linked and its last agreed values.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberInfo {
    pub member: Member,
    pub how: LinkReason,
    /// `None` for a copy linked since the copies last agreed.
    pub baseline: Option<Fields>,
}

/// Every copy of one song: the local copy first, then server copies.
#[derive(Debug, Clone, PartialEq)]
pub struct SongCopies {
    pub group_id: i64,
    pub members: Vec<MemberInfo>,
}

/// Which songs the Media Library file list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceFilter {
    All,
    /// Songs with a local copy, whether or not a server has one too. That is
    /// also exactly what plays offline.
    Local,
    /// Songs with a copy on this server.
    Server(String),
    /// Local-only songs, and songs whose local copy is ahead of a server:
    /// what an export would carry.
    LocalChanges,
    /// Conflicts, first-link differences, server changes, possible matches.
    NeedsAttention,
}

/// One row of the merged Media Library list.
#[derive(Debug, Clone)]
pub struct LibraryRow {
    /// The local file's row, or for a server-only song its server copy as a
    /// row (negative id, song URI path).
    pub track: LibTrack,
    pub has_local: bool,
    /// Servers holding a copy, sorted.
    pub servers: Vec<String>,
    /// `InSync` for a song with a single copy.
    pub status: SongStatus,
    /// The matcher found more than one plausible partner for this song.
    pub possible_match: bool,
}

impl LibraryRow {
    /// The row's source indicator: where its song is and whether the copies
    /// agree. Whether a server can be reached right now is not the row's to
    /// know, so `unreachable` is false; a frontend that knows sets it.
    pub fn indicator(&self) -> crate::servers::indicator::Indicator {
        crate::servers::indicator::Indicator {
            has_local: self.has_local,
            has_server: !self.servers.is_empty(),
            status: self.status,
            possible_match: self.possible_match,
            unreachable: false,
        }
    }
}

/// A removal this large is held for the user whatever the percentage.
pub const MASS_REMOVAL_ABSOLUTE: usize = 500;
/// A removal of more than this share of the cache is held...
pub const MASS_REMOVAL_PERCENT: usize = 10;
/// ...unless it is this small. Without a floor, deleting 2 songs from a
/// 10-song test library would ask for confirmation.
pub const MASS_REMOVAL_FLOOR: usize = 50;

/// The columns of `server_tracks` that describe the song, in the order
/// [`row_to_song`] reads them.
const SONG_COLUMNS: &str = "id, server_id, path, song_id, title, artist, album, album_artist,
    genre, comment, track_num, disc_num, year, bpm, length_secs, file_size, suffix, bitrate,
    cover_art, rating, play_count, played, musicbrainz_id, isrc,
    rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak, album_id, artwork_path";

impl MediaLibrary {
    /// Add the columns a library made by an earlier build lacks: its tables
    /// exist, so `CREATE TABLE IF NOT EXISTS` left them as they were. Runs
    /// before the indexes, some of which use these columns.
    fn upgrade_server_columns(&self) -> Result<()> {
        let added: [(&str, &[(&str, &str)]); 2] = [
            ("server_state", &[("last_success_secs", "INTEGER")]),
            (
                "server_tracks",
                &[
                    ("album_id", "TEXT"),
                    ("artwork_path", "TEXT"),
                    ("shown", "INTEGER NOT NULL DEFAULT 1"),
                    ("seen_pull", "INTEGER NOT NULL DEFAULT 0"),
                    ("added_pull", "INTEGER NOT NULL DEFAULT 0"),
                ],
            ),
        ];
        for (table, columns) in added {
            let existing: std::collections::HashSet<String> = self
                .conn
                .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?
                .query_map([], |r| r.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            for (column, definition) in columns {
                if !existing.contains(*column) {
                    self.conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"), [])?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn init_server_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS server_state (
                server_id       TEXT PRIMARY KEY,
                pull_seq        INTEGER NOT NULL DEFAULT 0,
                last_scan       TEXT,
                last_success_at TEXT,
                last_success_secs INTEGER,
                server_version  TEXT,
                extensions      TEXT
            );

            -- One row per song per server. `path_key` is the reported path,
            -- or `id:<song id>` for a server that reports none; it is the
            -- durable identity. `song_id` changes with tags on Navidrome.
            CREATE TABLE IF NOT EXISTS server_tracks (
                id             INTEGER PRIMARY KEY,
                server_id      TEXT NOT NULL,
                path_key       TEXT NOT NULL,
                path           TEXT,
                song_id        TEXT NOT NULL,
                title          TEXT NOT NULL DEFAULT '',
                artist         TEXT NOT NULL DEFAULT '',
                album          TEXT NOT NULL DEFAULT '',
                album_artist   TEXT NOT NULL DEFAULT '',
                genre          TEXT NOT NULL DEFAULT '',
                comment        TEXT NOT NULL DEFAULT '',
                track_num      INTEGER,
                disc_num       INTEGER,
                year           INTEGER,
                bpm            INTEGER,
                length_secs    INTEGER,
                file_size      INTEGER,
                suffix         TEXT,
                bitrate        INTEGER,
                cover_art      TEXT,
                album_id       TEXT,
                artwork_path   TEXT,
                rating         INTEGER NOT NULL DEFAULT 0,
                play_count     INTEGER NOT NULL DEFAULT 0,
                played         TEXT,
                musicbrainz_id TEXT,
                isrc           TEXT NOT NULL DEFAULT '',
                rg_track_gain  REAL,
                rg_track_peak  REAL,
                rg_album_gain  REAL,
                rg_album_peak  REAL,
                -- 1 when this copy is listed as its own row: not linked to a
                -- local file. Partial indexes on it keep the merged list and
                -- the album gallery as fast as a single table (measured).
                shown          INTEGER NOT NULL DEFAULT 1,
                seen_pull      INTEGER NOT NULL DEFAULT 0,
                added_pull     INTEGER NOT NULL DEFAULT 0,
                UNIQUE (server_id, path_key)
            );

            ",
        )?;
        self.upgrade_server_columns()?;
        self.conn.execute_batch(
            "
            CREATE INDEX IF NOT EXISTS idx_server_tracks_song
                ON server_tracks(server_id, song_id);
            CREATE INDEX IF NOT EXISTS idx_server_tracks_shown
                ON server_tracks(shown);
            -- Must match album_rows()'s GROUP BY exactly, like the tracks
            -- index it mirrors.
            CREATE INDEX IF NOT EXISTS idx_server_tracks_album_shown
                ON server_tracks(
                    LOWER(TRIM(COALESCE(album,''))),
                    LOWER(TRIM(COALESCE(album_artist,''))),
                    LOWER(TRIM(COALESCE(artist,'')))
                ) WHERE shown = 1;

            -- Linked copies. A group is one song; a group of one is no group
            -- and is dissolved by the trigger below.
            CREATE TABLE IF NOT EXISTS song_groups (id INTEGER PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS song_members (
                id              INTEGER PRIMARY KEY,
                group_id        INTEGER NOT NULL,
                local_track_id  INTEGER UNIQUE,
                server_track_id INTEGER UNIQUE,
                how             TEXT NOT NULL,
                baseline        TEXT,
                CHECK ((local_track_id IS NULL) <> (server_track_id IS NULL))
            );
            CREATE INDEX IF NOT EXISTS idx_song_members_group ON song_members(group_id);

            -- Pairs the user unlinked, by durable keys so a retag on the
            -- server (new song ID) does not forget them.
            CREATE TABLE IF NOT EXISTS never_link (
                local_path      TEXT NOT NULL,
                server_id       TEXT NOT NULL,
                server_path_key TEXT NOT NULL,
                PRIMARY KEY (local_path, server_id, server_path_key)
            );

            -- Server songs with more than one plausible local file, shown as possible matches.
            CREATE TABLE IF NOT EXISTS server_possible_matches (
                server_track_id INTEGER NOT NULL,
                local_track_id  INTEGER NOT NULL,
                PRIMARY KEY (server_track_id, local_track_id)
            );

            -- Changes waiting for a server: scrobbles with the time the play
            -- happened, and the latest rating per song. Keyed by row id so
            -- they follow a song through a song ID change.
            CREATE TABLE IF NOT EXISTS server_pending_scrobbles (
                id              INTEGER PRIMARY KEY,
                server_track_id INTEGER NOT NULL,
                at_ms           INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS server_pending_ratings (
                server_track_id INTEGER PRIMARY KEY,
                rating          INTEGER NOT NULL
            );

            -- Local values before the last apply of server changes, so it can
            -- be undone. One batch per apply; only the newest is kept useful.
            CREATE TABLE IF NOT EXISTS server_apply_undo (
                id       INTEGER PRIMARY KEY,
                batch    INTEGER NOT NULL,
                track_id INTEGER NOT NULL,
                path     TEXT NOT NULL,
                fields   TEXT NOT NULL
            );

            -- Many code paths delete from `tracks` (rescan prune, dedupe,
            -- the delete action, path normalization). Triggers keep links
            -- consistent whichever one it was.
            CREATE TRIGGER IF NOT EXISTS trg_tracks_unlink AFTER DELETE ON tracks BEGIN
                DELETE FROM song_members WHERE local_track_id = old.id;
                DELETE FROM server_possible_matches WHERE local_track_id = old.id;
            END;
            CREATE TRIGGER IF NOT EXISTS trg_server_tracks_unlink AFTER DELETE ON server_tracks BEGIN
                DELETE FROM song_members WHERE server_track_id = old.id;
                DELETE FROM server_possible_matches WHERE server_track_id = old.id;
                DELETE FROM server_pending_scrobbles WHERE server_track_id = old.id;
                DELETE FROM server_pending_ratings WHERE server_track_id = old.id;
            END;
            CREATE TRIGGER IF NOT EXISTS trg_song_members_left AFTER DELETE ON song_members BEGIN
                DELETE FROM song_members WHERE group_id = old.group_id
                    AND (SELECT COUNT(*) FROM song_members WHERE group_id = old.group_id) = 1;
                UPDATE server_tracks SET shown = 1 WHERE shown = 0
                    AND id NOT IN (SELECT server_track_id FROM song_members
                                   WHERE server_track_id IS NOT NULL);
                UPDATE server_tracks SET shown = 1
                    WHERE id = (SELECT MIN(server_track_id) FROM song_members
                                WHERE group_id = old.group_id)
                      AND NOT EXISTS (SELECT 1 FROM song_members
                                      WHERE group_id = old.group_id
                                        AND local_track_id IS NOT NULL);
            END;
            ",
        )?;
        self.init_server_playlist_schema()
    }

    /// Link two copies as the same song, merging their songs if both were
    /// already linked to others. Refused if the song would end up with two
    /// local copies or two copies from one server.
    pub fn link_copies(&self, a: Member, b: Member, how: LinkReason) -> Result<()> {
        let ga = self.group_of(a)?;
        let gb = self.group_of(b)?;
        if ga.is_some() && ga == gb {
            return Ok(());
        }
        let mut all: Vec<Member> = Vec::new();
        for (m, g) in [(a, ga), (b, gb)] {
            match g {
                Some(g) => all.extend(self.group_members(g)?),
                None => all.push(m),
            }
        }
        let locals = all.iter().filter(|m| matches!(m, Member::Local(_))).count();
        if locals > 1 {
            anyhow::bail!("a song cannot have two local copies");
        }
        let mut servers: Vec<String> = Vec::new();
        for m in &all {
            if let Member::Server(id) = m {
                servers.push(self.conn.query_row(
                    "SELECT server_id FROM server_tracks WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )?);
            }
        }
        let distinct: std::collections::HashSet<&String> = servers.iter().collect();
        if distinct.len() != servers.len() {
            anyhow::bail!("a song cannot have two copies from one server");
        }

        let tx = self.conn.unchecked_transaction()?;
        let group = match (ga, gb) {
            (Some(g), Some(other)) => {
                tx.execute(
                    "UPDATE song_members SET group_id = ?1 WHERE group_id = ?2",
                    params![g, other],
                )?;
                g
            }
            (Some(g), None) | (None, Some(g)) => g,
            (None, None) => {
                tx.execute("INSERT INTO song_groups DEFAULT VALUES", [])?;
                tx.last_insert_rowid()
            }
        };
        for (m, g) in [(a, ga), (b, gb)] {
            if g.is_none() {
                let (local, server) = member_columns(m);
                tx.execute(
                    "INSERT INTO song_members (group_id, local_track_id, server_track_id, how)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![group, local, server, how.as_str()],
                )?;
            }
        }
        // Only the song's own row stays listed: the local file if there is
        // one, else its first server copy.
        tx.execute(
            "UPDATE server_tracks SET shown = 0 WHERE id IN
                (SELECT server_track_id FROM song_members WHERE group_id = ?1)",
            params![group],
        )?;
        tx.execute(
            "UPDATE server_tracks SET shown = 1
             WHERE id = (SELECT MIN(server_track_id) FROM song_members WHERE group_id = ?1)
               AND NOT EXISTS (SELECT 1 FROM song_members
                               WHERE group_id = ?1 AND local_track_id IS NOT NULL)",
            params![group],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Take one copy out of its song. Every local/server pair this breaks is
    /// remembered so the matcher never proposes it again.
    pub fn unlink_copy(&self, member: Member) -> Result<()> {
        let Some(group) = self.group_of(member)? else { return Ok(()) };
        let others: Vec<Member> =
            self.group_members(group)?.into_iter().filter(|m| *m != member).collect();
        let tx = self.conn.unchecked_transaction()?;
        for other in others {
            match (member, other) {
                (Member::Local(local), Member::Server(server)) | (Member::Server(server), Member::Local(local)) => {
                    tx.execute(
                        "INSERT OR IGNORE INTO never_link (local_path, server_id, server_path_key)
                         SELECT t.path, s.server_id, s.path_key FROM tracks t, server_tracks s
                         WHERE t.id = ?1 AND s.id = ?2",
                        params![local, server],
                    )?;
                }
                // Two servers' copies: each side records the other by its
                // song URI, where a local pair has the file path, so the
                // matcher of either server leaves them apart.
                (Member::Server(a), Member::Server(b)) => {
                    let (Some(a), Some(b)) = (self.server_row(a)?, self.server_row(b)?) else { continue };
                    for (this, that) in [(&a, &b), (&b, &a)] {
                        tx.execute(
                            "INSERT OR IGNORE INTO never_link (local_path, server_id, server_path_key)
                             VALUES (?1, ?2, ?3)",
                            params![
                                crate::servers::uri::song_uri(&that.server_id, &path_key(&that.song)),
                                this.server_id,
                                path_key(&this.song)
                            ],
                        )?;
                    }
                }
                (Member::Local(_), Member::Local(_)) => {}
            }
        }
        let (local, server) = member_columns(member);
        tx.execute(
            "DELETE FROM song_members WHERE local_track_id IS ?1 AND server_track_id IS ?2",
            params![local, server],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The unlinked pairs for `server_id`, as `(tracks.id, server_tracks.id)`
    /// for the matcher.
    pub fn never_link_pairs(&self, server_id: &str) -> Result<Vec<(i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, s.id FROM never_link n
             JOIN tracks t ON t.path = n.local_path
             JOIN server_tracks s ON s.server_id = n.server_id AND s.path_key = n.server_path_key
             WHERE n.server_id = ?1 ORDER BY t.id, s.id",
        )?;
        let rows = stmt.query_map(params![server_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Every copy of the song `member` belongs to, or `None` if it is not
    /// linked to anything.
    pub fn song_copies(&self, member: Member) -> Result<Option<SongCopies>> {
        let Some(group_id) = self.group_of(member)? else { return Ok(None) };
        let mut stmt = self.conn.prepare(
            "SELECT local_track_id, server_track_id, how, baseline FROM song_members
             WHERE group_id = ?1 ORDER BY local_track_id IS NULL, server_track_id",
        )?;
        let members = stmt
            .query_map(params![group_id], |r| {
                let local: Option<i64> = r.get(0)?;
                let server: Option<i64> = r.get(1)?;
                let how: String = r.get(2)?;
                let baseline: Option<String> = r.get(3)?;
                Ok(MemberInfo {
                    member: match (local, server) {
                        (Some(l), _) => Member::Local(l),
                        (None, s) => Member::Server(s.unwrap_or_default()),
                    },
                    how: LinkReason::from_stored(&how),
                    baseline: baseline.and_then(|b| serde_json::from_str(&b).ok()),
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(Some(SongCopies { group_id, members }))
    }

    /// Record `member`'s last agreed values, or clear them (`None`).
    pub fn set_baseline(&self, member: Member, baseline: Option<&Fields>) -> Result<()> {
        let json = baseline.map(serde_json::to_string).transpose()?;
        let (local, server) = member_columns(member);
        self.conn.execute(
            "UPDATE song_members SET baseline = ?1
             WHERE local_track_id IS ?2 AND server_track_id IS ?3",
            params![json, local, server],
        )?;
        Ok(())
    }

    /// How the copies of `member`'s song compare, or `None` if it is not
    /// linked to anything.
    pub fn song_merge(&self, member: Member) -> Result<Option<SongMerge>> {
        Ok(self.copy_states(member)?.map(|(_, states)| merge::merge(&states)))
    }

    /// Accept every current difference: each copy's current values become
    /// its last agreed values, so the song reads as in sync until a copy
    /// changes again.
    pub fn accept_differences(&self, member: Member) -> Result<()> {
        let Some((members, states)) = self.copy_states(member)? else { return Ok(()) };
        for (m, st) in members.iter().zip(&states) {
            self.set_baseline(*m, Some(&st.current))?;
        }
        Ok(())
    }

    /// Record the user's decision for a song: `targets` names the value that
    /// wins for each field they decided. Fields left out keep the automatic
    /// outcome. Every conflict and every first-link choice must be decided.
    ///
    /// Copies that do not hold a winning value stay behind on it until they
    /// catch up: each is given its own current value as history, while the
    /// copies that hold the winner are given a history that differs from it.
    /// This writes history only; writing the local file is the caller's job.
    pub fn resolve_song(&self, member: Member, targets: &[(Field, Value)]) -> Result<()> {
        let Some((members, states)) = self.copy_states(member)? else { return Ok(()) };
        let merged = merge::merge(&states);
        let mut baselines: Vec<Fields> = states
            .iter()
            .map(|st| st.baseline.clone().unwrap_or_else(|| st.current.clone()))
            .collect();
        for (field, outcome) in &merged.fields {
            let chosen = targets.iter().find(|(f, _)| f == field).map(|(_, v)| v.clone());
            let winner = match (chosen, outcome) {
                (Some(v), _) => Some(v),
                (None, FieldOutcome::Wins { value, .. }) => Some(value.clone()),
                (None, FieldOutcome::Agreed) => None,
                (None, FieldOutcome::Conflict { .. } | FieldOutcome::Choose { .. }) => {
                    anyhow::bail!("{field:?} needs a decision");
                }
            };
            let Some(v) = winner else { continue };
            let behind: Vec<Value> = states
                .iter()
                .map(|st| st.current.get(*field))
                .filter(|cur| *cur != v)
                .collect();
            for (st, base) in states.iter().zip(baselines.iter_mut()) {
                let cur = st.current.get(*field);
                let history = if cur == v { behind.first().cloned().unwrap_or(v.clone()) } else { cur };
                base.set(*field, &history);
            }
        }
        for (m, base) in members.iter().zip(&baselines) {
            self.set_baseline(*m, Some(base))?;
        }
        Ok(())
    }

    /// Advance history where the copies agree: a field every copy holds the
    /// same value for becomes agreed history. A newly linked copy only gets
    /// history once every field agrees, so its open differences are not
    /// silently accepted.
    pub fn settle_song(&self, member: Member) -> Result<()> {
        let Some((members, states)) = self.copy_states(member)? else { return Ok(()) };
        let all_equal = |f: Field| states.iter().all(|st| st.current.get(f) == states[0].current.get(f));
        let whole = Field::ALL.iter().all(|f| all_equal(*f));
        for (m, st) in members.iter().zip(&states) {
            let base = match (&st.baseline, whole) {
                (_, true) => st.current.clone(),
                (None, false) => continue,
                (Some(b), false) => {
                    let mut b = b.clone();
                    for f in Field::ALL {
                        if all_equal(f) {
                            b.set(f, &st.current.get(f));
                        }
                    }
                    b
                }
            };
            self.set_baseline(*m, Some(&base))?;
        }
        Ok(())
    }

    /// Record that the server now holds `rating` for this copy, after a
    /// successful `setRating`, so the song does not read as changed until
    /// the next pull confirms it.
    pub fn set_cached_server_rating(&self, server_track_id: i64, rating: u8) -> Result<()> {
        self.conn.execute(
            "UPDATE server_tracks SET rating = ?1 WHERE id = ?2",
            params![rating.min(5) as i64, server_track_id],
        )?;
        Ok(())
    }

    /// A local file's rating (0 to 5) as the library holds it.
    pub fn local_rating(&self, track_id: i64) -> Result<u8> {
        let r: i64 = self.conn.query_row(
            "SELECT COALESCE(rating, 0) FROM tracks WHERE id = ?1",
            params![track_id],
            |r| r.get(0),
        )?;
        Ok(r.clamp(0, 5) as u8)
    }

    /// Start a new undo batch for an apply, discarding older ones: there is
    /// one level of undo.
    pub fn start_apply_batch(&self) -> Result<i64> {
        let next: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(batch), 0) + 1 FROM server_apply_undo",
            [],
            |r| r.get(0),
        )?;
        self.conn.execute("DELETE FROM server_apply_undo WHERE batch < ?1", params![next])?;
        Ok(next)
    }

    /// Remember a file's values before an apply changes them.
    pub fn record_apply_undo(&self, batch: i64, track_id: i64, path: &str, before: &Fields) -> Result<()> {
        self.conn.execute(
            "INSERT INTO server_apply_undo (batch, track_id, path, fields) VALUES (?1, ?2, ?3, ?4)",
            params![batch, track_id, path, serde_json::to_string(before)?],
        )?;
        Ok(())
    }

    /// The newest apply's saved values, as `(track id, path, values)`, and
    /// forget them.
    pub fn take_last_apply_batch(&self) -> Result<Vec<(i64, String, Fields)>> {
        let batch: Option<i64> =
            self.conn.query_row("SELECT MAX(batch) FROM server_apply_undo", [], |r| r.get(0))?;
        let Some(batch) = batch else { return Ok(Vec::new()) };
        let mut stmt = self.conn.prepare(
            "SELECT track_id, path, fields FROM server_apply_undo WHERE batch = ?1 ORDER BY id",
        )?;
        let rows: Vec<(i64, String, Fields)> = stmt
            .query_map(params![batch], |r| {
                let json: String = r.get(2)?;
                Ok((r.get(0)?, r.get(1)?, serde_json::from_str(&json).unwrap_or_default()))
            })?
            .filter_map(|r| r.ok())
            .collect();
        self.conn.execute("DELETE FROM server_apply_undo WHERE batch = ?1", params![batch])?;
        Ok(rows)
    }

    /// Set a local file's rating (0 to 5) in the library.
    pub fn set_local_rating(&self, track_id: i64, rating: u8) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET rating = ?1 WHERE id = ?2",
            params![rating.min(5) as i64, track_id],
        )?;
        Ok(())
    }

    /// The members of `member`'s song and each one's state for the merge.
    fn copy_states(&self, member: Member) -> Result<Option<(Vec<Member>, Vec<CopyState>)>> {
        let Some(copies) = self.song_copies(member)? else { return Ok(None) };
        let mut members = Vec::new();
        let mut states = Vec::new();
        for info in copies.members {
            let (copy, current) = match info.member {
                Member::Local(id) => {
                    let Some(track) = self.tracks_by_ids(&[id])?.remove(&id) else { continue };
                    let rating: i64 = self.conn.query_row(
                        "SELECT COALESCE(rating, 0) FROM tracks WHERE id = ?1",
                        params![id],
                        |r| r.get(0),
                    )?;
                    (CopyId::Local, normalize::local_fields(&track, rating.clamp(0, 5) as u8))
                }
                Member::Server(id) => {
                    let row = self.conn.query_row(
                        &format!("SELECT {SONG_COLUMNS} FROM server_tracks WHERE id = ?1"),
                        params![id],
                        row_to_song,
                    )?;
                    (CopyId::Server(row.server_id), normalize::server_fields(&row.song))
                }
            };
            members.push(info.member);
            states.push(CopyState { copy, current, baseline: info.baseline });
        }
        Ok(Some((members, states)))
    }

    /// One cached server song by row id.
    pub fn server_row(&self, id: i64) -> Result<Option<ServerTrackRow>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {SONG_COLUMNS} FROM server_tracks WHERE id = ?1"),
                params![id],
                row_to_song,
            )
            .ok())
    }

    /// One cached server song by server and durable path key, the two halves
    /// of a song URI.
    pub fn server_row_by_key(&self, server_id: &str, key: &str) -> Result<Option<ServerTrackRow>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {SONG_COLUMNS} FROM server_tracks WHERE server_id = ?1 AND path_key = ?2"
                ),
                params![server_id, key],
                row_to_song,
            )
            .ok())
    }

    /// The local file linked to server copy `server_track_id`, if any.
    pub fn local_copy_of(&self, server_track_id: i64) -> Result<Option<i64>> {
        Ok(self
            .song_copies(Member::Server(server_track_id))?
            .and_then(|c| {
                c.members.iter().find_map(|m| match m.member {
                    Member::Local(id) => Some(id),
                    Member::Server(_) => None,
                })
            }))
    }

    /// What a song URI stands for now: the local copy when the song has one,
    /// else the cached server song, else `None` (the server no longer has it).
    pub fn resolve_song_uri(&self, uri: &str) -> Result<Option<LibTrack>> {
        let Some((server_id, key)) = crate::servers::uri::parse_song_uri(uri) else {
            return Ok(None);
        };
        let Some(row) = self.server_row_by_key(&server_id, &key)? else { return Ok(None) };
        if let Some(local) = self.local_copy_of(row.id)? {
            if let Some(t) = self.tracks_by_ids(&[local])?.remove(&local) {
                return Ok(Some(t));
            }
        }
        Ok(Some(server_track_as_lib_track(&row)))
    }

    /// Record one play of `path` (a file or a song URI) at `at_ms`
    /// (milliseconds since the Unix epoch). A local file counts in the
    /// library as before; every server holding a copy of the song gets a
    /// queued scrobble carrying the play time.
    pub fn record_play_at(&self, path: &str, at_ms: i64) -> Result<()> {
        let member = if let Some((server_id, key)) = crate::servers::uri::parse_song_uri(path) {
            match self.server_row_by_key(&server_id, &key)? {
                Some(row) => Member::Server(row.id),
                None => return Ok(()),
            }
        } else {
            self.record_local_play(path)?;
            let canonical = Self::canonical_track_path(path);
            match self
                .conn
                .query_row("SELECT id FROM tracks WHERE path = ?1", params![canonical], |r| r.get(0))
            {
                Ok(id) => Member::Local(id),
                Err(_) => return Ok(()),
            }
        };
        let Some(copies) = self.song_copies(member)? else {
            if let Member::Server(id) = member {
                self.queue_scrobble(id, at_ms)?;
            }
            return Ok(());
        };
        for m in copies.members {
            match m.member {
                Member::Server(id) => self.queue_scrobble(id, at_ms)?,
                Member::Local(id) if member != Member::Local(id) => {
                    if let Some(t) = self.tracks_by_ids(&[id])?.remove(&id) {
                        self.record_local_play(&t.path)?;
                    }
                }
                Member::Local(_) => {}
            }
        }
        Ok(())
    }

    fn queue_scrobble(&self, server_track_id: i64, at_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO server_pending_scrobbles (server_track_id, at_ms) VALUES (?1, ?2)",
            params![server_track_id, at_ms],
        )?;
        Ok(())
    }

    /// Queued plays for `server_id` as `(current song ID, play time ms)`,
    /// oldest first. The song ID is looked up now, not when the play was
    /// queued, because it may have changed since.
    pub fn pending_scrobbles(&self, server_id: &str) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.song_id, p.at_ms FROM server_pending_scrobbles p
             JOIN server_tracks s ON s.id = p.server_track_id
             WHERE s.server_id = ?1 ORDER BY p.at_ms, p.id",
        )?;
        let rows = stmt.query_map(params![server_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Up to `limit` of `server_id`'s queued plays, oldest first, as
    /// `(queue id, current song ID, play time ms)`.
    pub fn pending_scrobble_batch(
        &self,
        server_id: &str,
        limit: usize,
    ) -> Result<Vec<(i64, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, s.song_id, p.at_ms FROM server_pending_scrobbles p
             JOIN server_tracks s ON s.id = p.server_track_id
             WHERE s.server_id = ?1 ORDER BY p.at_ms, p.id LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![server_id, limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Forget the queued plays with these queue ids.
    pub fn clear_scrobbles(&self, ids: &[i64]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for id in ids {
            tx.execute("DELETE FROM server_pending_scrobbles WHERE id = ?1", params![id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Whether sending `rating` for server copy `server_track_id` would
    /// overwrite a rating changed on the server since the copies last agreed.
    pub fn rating_changed_on_server(&self, server_track_id: i64, rating: u8) -> Result<bool> {
        let Some(row) = self.server_row(server_track_id)? else { return Ok(false) };
        let baseline = self
            .song_copies(Member::Server(server_track_id))?
            .and_then(|c| c.members.into_iter().find(|m| m.member == Member::Server(server_track_id)))
            .and_then(|m| m.baseline);
        Ok(match baseline {
            Some(b) => row.song.user_rating != b.rating && row.song.user_rating != rating,
            None => false,
        })
    }

    /// Forget `server_id`'s queued plays once the server has them.
    pub fn clear_pending_scrobbles(&self, server_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM server_pending_scrobbles WHERE server_track_id IN
                (SELECT id FROM server_tracks WHERE server_id = ?1)",
            params![server_id],
        )?;
        Ok(())
    }

    /// Queue a rating for server copy `server_track_id`, replacing any
    /// rating already waiting for it.
    pub fn queue_rating(&self, server_track_id: i64, rating: u8) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO server_pending_ratings (server_track_id, rating) VALUES (?1, ?2)",
            params![server_track_id, rating.min(5) as i64],
        )?;
        Ok(())
    }

    /// Queued ratings for `server_id` as `(row id, current song ID, rating)`.
    pub fn pending_ratings(&self, server_id: &str) -> Result<Vec<(i64, String, u8)>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.song_id, p.rating FROM server_pending_ratings p
             JOIN server_tracks s ON s.id = p.server_track_id
             WHERE s.server_id = ?1 ORDER BY s.id",
        )?;
        let rows = stmt.query_map(params![server_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)?.clamp(0, 5) as u8))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Forget a queued rating once the server has it.
    pub fn clear_pending_rating(&self, server_track_id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM server_pending_ratings WHERE server_track_id = ?1",
            params![server_track_id],
        )?;
        Ok(())
    }

    /// The merged Media Library list: local files plus server-only songs,
    /// each once, filtered by source and optionally by search words, sorted
    /// by `col` like [`Self::all_tracks_sorted`].
    pub fn library_rows(
        &self,
        filter: &SourceFilter,
        query: Option<&str>,
        col: &str,
        desc: bool,
    ) -> Result<Vec<LibraryRow>> {
        let query = query.map(str::trim).filter(|q| !q.is_empty());
        let locals = match query {
            Some(q) => self.search_tracks_sorted(q, col, desc)?,
            None => self.all_tracks_sorted(col, desc)?,
        };
        let any_server: bool =
            self.conn.query_row("SELECT EXISTS (SELECT 1 FROM server_tracks)", [], |r| r.get(0))?;
        let info = if any_server { self.song_info()? } else { SongInfo::default() };

        let mut rows: Vec<LibraryRow> = locals
            .into_iter()
            .map(|track| {
                let (servers, status) =
                    info.local.get(&track.id).cloned().unwrap_or((Vec::new(), SongStatus::InSync));
                // The ≈ mark belongs to the server song that could not be
                // matched; its candidates are listed in its Copies panel.
                LibraryRow { track, has_local: true, servers, status, possible_match: false }
            })
            .collect();
        let local_count = rows.len();
        if any_server {
            let words: Vec<String> = query
                .map(|q| q.split_whitespace().map(str::to_lowercase).collect())
                .unwrap_or_default();
            for row in self.query_server_rows(
                &format!("SELECT {SONG_COLUMNS} FROM server_tracks WHERE shown = 1"),
                [],
            )? {
                let track = server_track_as_lib_track(&row);
                if !words.iter().all(|w| matches_word(&track, w)) {
                    continue;
                }
                let (servers, status) = info
                    .server
                    .get(&row.id)
                    .cloned()
                    .unwrap_or((vec![row.server_id.clone()], SongStatus::InSync));
                let possible_match = info.possible_servers.contains(&row.id);
                rows.push(LibraryRow { track, has_local: false, servers, status, possible_match });
            }
        }
        rows.retain(|r| match filter {
            SourceFilter::All => true,
            SourceFilter::Local => r.has_local,
            SourceFilter::Server(id) => r.servers.iter().any(|s| s == id),
            SourceFilter::LocalChanges => {
                (r.has_local && r.servers.is_empty()) || r.status == SongStatus::LocalChanged
            }
            SourceFilter::NeedsAttention => {
                r.possible_match
                    || matches!(
                        r.status,
                        SongStatus::Conflict | SongStatus::FirstLinkDiffers | SongStatus::ServerChanged
                    )
            }
        });
        // Local rows arrive sorted from SQL; only a list that also holds
        // server rows needs sorting here.
        if rows.len() > local_count || rows.iter().any(|r| !r.has_local) {
            rows.sort_by(|a, b| compare_rows(&a.track, &b.track, col, desc));
        }
        Ok(rows)
    }

    /// Record agreement for every linked song (see [`Self::settle_song`]),
    /// in one pass. Run after each update, so a later change on one side
    /// reads as "changed there" instead of an unresolved difference.
    /// Returns how many copies' agreed values changed.
    pub fn settle_all_songs(&self) -> Result<usize> {
        let groups = self.all_group_states()?;
        let tx = self.conn.unchecked_transaction()?;
        let mut written = 0;
        for (_, copies) in groups {
            let all_equal = |f: Field| copies.iter().all(|c| c.3.get(f) == copies[0].3.get(f));
            let whole = Field::ALL.iter().all(|f| all_equal(*f));
            for (member, baseline, _, current) in &copies {
                let new = match (baseline, whole) {
                    (_, true) => current.clone(),
                    (None, false) => continue,
                    (Some(b), false) => {
                        let mut b = b.clone();
                        for f in Field::ALL {
                            if all_equal(f) {
                                b.set(f, &current.get(f));
                            }
                        }
                        b
                    }
                };
                if baseline.as_ref() != Some(&new) {
                    let (local, server) = member_columns(*member);
                    tx.execute(
                        "UPDATE song_members SET baseline = ?1
                         WHERE local_track_id IS ?2 AND server_track_id IS ?3",
                        params![serde_json::to_string(&new)?, local, server],
                    )?;
                    written += 1;
                }
            }
        }
        tx.commit()?;
        Ok(written)
    }

    /// Every linked song's servers and status, keyed by local id and by
    /// server row id, computed in one pass.
    fn song_info(&self) -> Result<SongInfo> {
        let groups = self.all_group_states()?;
        let mut info = SongInfo::default();
        for (_, mut copies) in groups {
            copies.sort_by_key(|(m, ..)| match m {
                Member::Local(id) => (0, *id),
                Member::Server(id) => (1, *id),
            });
            let states: Vec<CopyState> = copies
                .iter()
                .map(|(_, baseline, copy, current)| CopyState {
                    copy: copy.clone(),
                    current: current.clone(),
                    baseline: baseline.clone(),
                })
                .collect();
            let status = merge::merge(&states).status();
            let mut servers: Vec<String> = copies
                .iter()
                .filter_map(|(_, _, c, _)| match c {
                    CopyId::Server(s) => Some(s.clone()),
                    CopyId::Local => None,
                })
                .collect();
            servers.sort();
            servers.dedup();
            for (m, ..) in &copies {
                match m {
                    Member::Local(id) => info.local.insert(*id, (servers.clone(), status)),
                    Member::Server(id) => info.server.insert(*id, (servers.clone(), status)),
                };
            }
        }
        let mut stmt =
            self.conn.prepare("SELECT DISTINCT server_track_id FROM server_possible_matches")?;
        for server in stmt.query_map([], |r| r.get::<_, i64>(0))? {
            info.possible_servers.insert(server?);
        }
        Ok(info)
    }

    /// Forget everything cached about `server_id`: its songs, their links
    /// and queued changes (through the triggers), its state. Local files are
    /// never touched.
    pub fn forget_server(&self, server_id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM server_tracks WHERE server_id = ?1", params![server_id])?;
        tx.execute("DELETE FROM server_state WHERE server_id = ?1", params![server_id])?;
        tx.execute("DELETE FROM never_link WHERE server_id = ?1", params![server_id])?;
        // Other servers' records of pairs with this server's copies.
        tx.execute(
            "DELETE FROM never_link WHERE substr(local_path, 1, length(?1)) = ?1",
            params![crate::servers::uri::song_uri(server_id, "")],
        )?;
        tx.commit()?;
        self.forget_server_playlists(server_id)
    }

    /// Every linked song's copies with their last agreed and current values,
    /// by group, read in bulk.
    fn all_group_states(&self) -> Result<HashMap<i64, Vec<(Member, Option<Fields>, CopyId, Fields)>>> {
        let mut groups: HashMap<i64, Vec<(Member, Option<Fields>, CopyId, Fields)>> = HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT m.group_id, m.baseline, t.id, t.title, t.artist, t.album, t.album_artist,
                    t.genre, t.comment, t.track_num, t.disc_num, t.year, t.bpm, COALESCE(t.rating, 0)
             FROM song_members m JOIN tracks t ON t.id = m.local_track_id",
        )?;
        let local_rows = stmt.query_map([], |r| {
            let track = LibTrack {
                id: r.get(2)?,
                title: r.get(3)?,
                artist: r.get(4)?,
                album: r.get(5)?,
                album_artist: r.get(6)?,
                genre: r.get(7)?,
                comment: r.get(8)?,
                track_num: r.get(9)?,
                disc_num: r.get(10)?,
                year: r.get(11)?,
                bpm: r.get(12)?,
                ..LibTrack::default()
            };
            let rating: i64 = r.get(13)?;
            let baseline: Option<String> = r.get(1)?;
            Ok((r.get::<_, i64>(0)?, baseline, track, rating))
        })?;
        for row in local_rows.filter_map(|r| r.ok()) {
            let (group, baseline, track, rating) = row;
            let fields = normalize::local_fields(&track, rating.clamp(0, 5) as u8);
            groups.entry(group).or_default().push((
                Member::Local(track.id),
                baseline.and_then(|b| serde_json::from_str(&b).ok()),
                CopyId::Local,
                fields,
            ));
        }
        let mut stmt = self
            .conn
            .prepare("SELECT group_id, baseline, server_track_id FROM song_members WHERE server_track_id IS NOT NULL")?;
        let server_members: Vec<(i64, Option<String>, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .filter_map(|r| r.ok())
            .collect();
        let server_rows: HashMap<i64, ServerTrackRow> = self
            .query_server_rows(
                &format!(
                    "SELECT {SONG_COLUMNS} FROM server_tracks
                     WHERE id IN (SELECT server_track_id FROM song_members)"
                ),
                [],
            )?
            .into_iter()
            .map(|r| (r.id, r))
            .collect();
        for (group, baseline, id) in server_members {
            let Some(row) = server_rows.get(&id) else { continue };
            groups.entry(group).or_default().push((
                Member::Server(id),
                baseline.and_then(|b| serde_json::from_str(&b).ok()),
                CopyId::Server(row.server_id.clone()),
                normalize::server_fields(&row.song),
            ));
        }

        Ok(groups)
    }

    /// The `lastScan` of the last pull that was applied, if any.
    pub fn server_last_scan(&self, server_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT last_scan FROM server_state WHERE server_id = ?1",
                params![server_id],
                |r| r.get(0),
            )
            .ok()
            .flatten())
    }

    /// Record that a pull was applied: the server's `lastScan` at that time,
    /// and when.
    pub fn record_server_pull_complete(&self, server_id: &str, last_scan: Option<&str>) -> Result<()> {
        self.record_server_update_success(server_id, Some(last_scan))
    }

    /// Record a successful update of `server_id` now. `last_scan` is `Some`
    /// when a pull was applied (its `lastScan`, possibly `None`), `None` when
    /// the update found nothing to pull and the stored `lastScan` stands.
    pub fn record_server_update_success(
        &self,
        server_id: &str,
        last_scan: Option<Option<&str>>,
    ) -> Result<()> {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let now = crate::timeutil::format_current_timestamp();
        match last_scan {
            Some(scan) => self.conn.execute(
                "INSERT INTO server_state (server_id, last_scan, last_success_at, last_success_secs)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(server_id) DO UPDATE SET
                    last_scan = ?2, last_success_at = ?3, last_success_secs = ?4",
                params![server_id, scan, now, secs],
            )?,
            None => self.conn.execute(
                "INSERT INTO server_state (server_id, last_success_at, last_success_secs)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(server_id) DO UPDATE SET last_success_at = ?2, last_success_secs = ?3",
                params![server_id, now, secs],
            )?,
        };
        Ok(())
    }

    /// When `server_id` was last updated successfully, if ever.
    pub fn server_last_success(&self, server_id: &str) -> Result<Option<std::time::SystemTime>> {
        let secs: Option<i64> = self
            .conn
            .query_row(
                "SELECT last_success_secs FROM server_state WHERE server_id = ?1",
                params![server_id],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        Ok(secs.map(|s| std::time::UNIX_EPOCH + std::time::Duration::from_secs(s.max(0) as u64)))
    }

    /// What the matcher works on for `server_id`: local files that have no
    /// copy on that server yet, and that server's songs linked to nothing.
    pub fn match_candidates(
        &self,
        server_id: &str,
    ) -> Result<(Vec<LocalCandidate>, Vec<ServerCandidate>)> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.path, f.path, t.title, t.artist, t.album, t.length_secs
             FROM tracks t LEFT JOIN folders f ON f.id = t.folder_id
             WHERE t.deleted_at IS NULL AND NOT EXISTS (
                SELECT 1 FROM song_members lm
                JOIN song_members sm ON sm.group_id = lm.group_id
                JOIN server_tracks st ON st.id = sm.server_track_id
                WHERE lm.local_track_id = t.id AND st.server_id = ?1)",
        )?;
        let locals = stmt
            .query_map(params![server_id], |r| {
                let path: String = r.get(1)?;
                let folder: Option<String> = r.get(2)?;
                let rel_path = relative_to(&path, folder.as_deref());
                Ok(LocalCandidate {
                    id: r.get(0)?,
                    rel_path,
                    title: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    artist: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    album: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    duration_secs: r.get(6)?,
                    ..LocalCandidate::default()
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        let servers = self
            .query_server_rows(
                &format!(
                    "SELECT {SONG_COLUMNS} FROM server_tracks s
                     WHERE server_id = ?1
                       AND NOT EXISTS (SELECT 1 FROM song_members m WHERE m.server_track_id = s.id)"
                ),
                params![server_id],
            )?
            .into_iter()
            .map(|row| ServerCandidate {
                key: row.id,
                path: row.song.path.clone().unwrap_or_default(),
                title: row.song.title.clone(),
                artist: row.song.artist.clone(),
                album: row.song.album.clone(),
                duration_secs: row.song.duration_secs.map(|d| d as f64),
                musicbrainz_id: row.song.musicbrainz_id.clone(),
                isrc: row.song.isrc.clone(),
            })
            .collect();
        Ok((locals, servers))
    }

    /// What can match `server_id`'s unlinked songs on other servers: the
    /// songs listed as their own rows elsewhere (no local copy) whose song
    /// has no copy on `server_id` yet, standing in as the matcher's "local"
    /// side with their row ids. Each carries only its file name, never its
    /// path: two servers' library roots differ, so paths say nothing about
    /// sameness between them, while the tags, IDs and file name still do.
    pub fn server_match_candidates(
        &self,
        server_id: &str,
    ) -> Result<(Vec<LocalCandidate>, Vec<ServerCandidate>)> {
        let others = self
            .query_server_rows(
                &format!(
                    "SELECT {SONG_COLUMNS} FROM server_tracks s
                     WHERE s.server_id != ?1 AND s.shown = 1
                       AND NOT EXISTS (
                         SELECT 1 FROM song_members m
                         JOIN song_members o ON o.group_id = m.group_id
                         JOIN server_tracks x ON x.id = o.server_track_id
                         WHERE m.server_track_id = s.id AND x.server_id = ?1)"
                ),
                params![server_id],
            )?
            .into_iter()
            .map(|row| {
                let key = path_key(&row.song);
                LocalCandidate {
                    id: row.id,
                    rel_path: key.rsplit('/').next().unwrap_or(&key).to_string(),
                    title: row.song.title,
                    artist: row.song.artist,
                    album: row.song.album,
                    duration_secs: row.song.duration_secs.map(|d| d as f64),
                    musicbrainz_id: row.song.musicbrainz_id,
                    isrc: row.song.isrc,
                }
            })
            .collect();
        let (_, mine) = self.match_candidates(server_id)?;
        Ok((others, mine))
    }

    /// The pairs of `server_id`'s copies and other servers' copies the user
    /// unlinked, as `(other row, row on server_id)` for the matcher.
    pub fn never_link_server_pairs(&self, server_id: &str) -> Result<Vec<(i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT n.local_path, s.id FROM never_link n
             JOIN server_tracks s ON s.server_id = n.server_id AND s.path_key = n.server_path_key
             WHERE n.server_id = ?1",
        )?;
        let rows: Vec<(String, i64)> =
            stmt.query_map(params![server_id], |r| Ok((r.get(0)?, r.get(1)?)))?.filter_map(|r| r.ok()).collect();
        let mut pairs = Vec::new();
        for (uri, mine) in rows {
            let Some((other_server, key)) = crate::servers::uri::parse_song_uri(&uri) else { continue };
            if let Some(other) = self.server_row_by_key(&other_server, &key)? {
                pairs.push((other.id, mine));
            }
        }
        pairs.sort();
        Ok(pairs)
    }

    /// Every local file's path relative to its watched folder, by track id.
    /// A file outside every watched folder maps to its file name.
    pub fn local_rel_paths(&self) -> Result<HashMap<i64, String>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.path, f.path FROM tracks t LEFT JOIN folders f ON f.id = t.folder_id",
        )?;
        let rows = stmt.query_map([], |r| {
            let path: String = r.get(1)?;
            let folder: Option<String> = r.get(2)?;
            Ok((r.get::<_, i64>(0)?, relative_to(&path, folder.as_deref())))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Replace `server_id`'s possible matches with `possible`.
    pub fn record_possible_matches(&self, server_id: &str, possible: &[PossibleMatch]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM server_possible_matches WHERE server_track_id IN
                (SELECT id FROM server_tracks WHERE server_id = ?1)",
            params![server_id],
        )?;
        for p in possible {
            for local in &p.candidates {
                tx.execute(
                    "INSERT OR IGNORE INTO server_possible_matches (server_track_id, local_track_id)
                     VALUES (?1, ?2)",
                    params![p.server, local],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Library rows by id, where a negative id is a server song (see
    /// [`server_track_as_lib_track`]). Ids with no row are absent.
    pub fn library_tracks_by_ids(&self, ids: &[i64]) -> Result<HashMap<i64, LibTrack>> {
        let local: Vec<i64> = ids.iter().copied().filter(|id| *id > 0).collect();
        let mut found = self.tracks_by_ids(&local)?;
        for id in ids.iter().copied().filter(|id| *id < 0) {
            if let Some(row) = self.server_row(-id)? {
                found.insert(id, server_track_as_lib_track(&row));
            }
        }
        Ok(found)
    }

    /// Server-only songs (and the one copy listed for a song only on
    /// servers) as library rows.
    pub fn shown_server_lib_tracks(&self) -> Result<Vec<LibTrack>> {
        Ok(self
            .query_server_rows(&format!("SELECT {SONG_COLUMNS} FROM server_tracks WHERE shown = 1"), [])?
            .iter()
            .map(server_track_as_lib_track)
            .collect())
    }

    /// The cached cover file for server copy `id`, if fetched.
    pub fn server_artwork_path(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT artwork_path FROM server_tracks WHERE id = ?1", params![id], |r| r.get(0))
            .ok()
            .flatten())
    }

    /// Covers `server_id` still needs, one per album (or per song for a
    /// song with no album id), as `(group key, cover id)`.
    pub fn covers_needed(&self, server_id: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            // Only songs listed as their own rows: an album whose every
            // song has a local copy shows local art and needs nothing here.
            "SELECT COALESCE(album_id, 'song:' || id) AS k, MIN(cover_art) FROM server_tracks
             WHERE server_id = ?1 AND cover_art IS NOT NULL AND artwork_path IS NULL
               AND shown = 1
             GROUP BY k ORDER BY k",
        )?;
        let rows = stmt.query_map(params![server_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Record the cover file for a group from [`Self::covers_needed`].
    pub fn set_cover_path(&self, server_id: &str, key: &str, path: &str) -> Result<()> {
        match key.strip_prefix("song:").and_then(|id| id.parse::<i64>().ok()) {
            Some(id) => self.conn.execute(
                "UPDATE server_tracks SET artwork_path = ?1 WHERE id = ?2",
                params![path, id],
            )?,
            None => self.conn.execute(
                "UPDATE server_tracks SET artwork_path = ?1 WHERE server_id = ?2 AND album_id = ?3",
                params![path, server_id, key],
            )?,
        };
        Ok(())
    }

    /// Server copies listed as their own rows, by id. What the merged list
    /// and album queries read through the partial indexes.
    pub fn shown_server_track_ids(&self) -> Result<Vec<i64>> {
        let mut stmt =
            self.conn.prepare("SELECT id FROM server_tracks WHERE shown = 1 ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn group_of(&self, member: Member) -> Result<Option<i64>> {
        let (local, server) = member_columns(member);
        Ok(self
            .conn
            .query_row(
                "SELECT group_id FROM song_members
                 WHERE local_track_id IS ?1 AND server_track_id IS ?2",
                params![local, server],
                |r| r.get(0),
            )
            .ok())
    }

    fn group_members(&self, group: i64) -> Result<Vec<Member>> {
        let mut stmt = self.conn.prepare(
            "SELECT local_track_id, server_track_id FROM song_members WHERE group_id = ?1",
        )?;
        let rows = stmt.query_map(params![group], |r| {
            let local: Option<i64> = r.get(0)?;
            let server: Option<i64> = r.get(1)?;
            Ok(match local {
                Some(l) => Member::Local(l),
                None => Member::Server(server.unwrap_or_default()),
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Start a catalog pull for `server_id`. Returns the pull's id, which
    /// every page and the finish call carry.
    pub fn begin_server_pull(&self, server_id: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO server_state (server_id, pull_seq) VALUES (?1, 1)
             ON CONFLICT(server_id) DO UPDATE SET pull_seq = pull_seq + 1",
            params![server_id],
        )?;
        Ok(self.conn.query_row(
            "SELECT pull_seq FROM server_state WHERE server_id = ?1",
            params![server_id],
            |r| r.get(0),
        )?)
    }

    /// Record one page of a pull: add new songs, update changed ones in
    /// place. Adding is safe before the pull completes; nothing is removed
    /// here. Writes go in transactions of 100, like every other bulk insert.
    pub fn apply_server_songs(
        &self,
        server_id: &str,
        pull_id: i64,
        songs: &[ServerSong],
    ) -> Result<PageOutcome> {
        let mut outcome = PageOutcome::default();
        for chunk in songs.chunks(100) {
            let tx = self.conn.unchecked_transaction()?;
            for incoming in chunk {
                let mut song = incoming.clone();
                if song.replay_gain.as_ref().is_some_and(|g| *g == ReplayGain::default()) {
                    song.replay_gain = None;
                }
                let key = path_key(&song);
                let stored: Option<ServerTrackRow> = tx
                    .query_row(
                        &format!(
                            "SELECT {SONG_COLUMNS} FROM server_tracks
                             WHERE server_id = ?1 AND path_key = ?2"
                        ),
                        params![server_id, key],
                        row_to_song,
                    )
                    .ok();
                match stored {
                    None => {
                        tx.execute(
                            "INSERT INTO server_tracks (server_id, path_key, song_id, seen_pull, added_pull)
                             VALUES (?1, ?2, '', ?3, ?3)",
                            params![server_id, key, pull_id],
                        )?;
                        write_song(&tx, tx.last_insert_rowid(), &song)?;
                        outcome.added += 1;
                    }
                    Some(row) => {
                        if row.song != song {
                            write_song(&tx, row.id, &song)?;
                            outcome.updated += 1;
                            // New art on the server: fetch it again.
                            if row.song.cover_art != song.cover_art {
                                tx.execute(
                                    "UPDATE server_tracks SET artwork_path = NULL WHERE id = ?1",
                                    params![row.id],
                                )?;
                            }
                        }
                        tx.execute(
                            "UPDATE server_tracks SET seen_pull = ?1 WHERE id = ?2",
                            params![pull_id, row.id],
                        )?;
                    }
                }
            }
            tx.commit()?;
        }
        Ok(outcome)
    }

    /// Finish a pull that saw every page. Songs it did not see are removed,
    /// unless the removal is large enough to be held.
    pub fn finish_server_pull(&self, server_id: &str, pull_id: i64) -> Result<PullOutcome> {
        self.finish_server_pull_after_moves(server_id, pull_id, 0)
    }

    /// [`Self::finish_server_pull`] when `moved` of the unseen songs were
    /// found again at new paths in this pull. Moves are not disappearances,
    /// so they do not count toward holding the removal; their old rows are
    /// still dropped.
    pub fn finish_server_pull_after_moves(
        &self,
        server_id: &str,
        pull_id: i64,
        moved: usize,
    ) -> Result<PullOutcome> {
        let cached: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM server_tracks WHERE server_id = ?1",
            params![server_id],
            |r| r.get::<_, i64>(0),
        )? as usize;
        let would_remove: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM server_tracks WHERE server_id = ?1 AND seen_pull <> ?2",
            params![server_id, pull_id],
            |r| r.get::<_, i64>(0),
        )? as usize;
        let vanished = would_remove.saturating_sub(moved);
        let too_many = vanished > MASS_REMOVAL_ABSOLUTE
            || (vanished >= MASS_REMOVAL_FLOOR && vanished * 100 > cached * MASS_REMOVAL_PERCENT);
        if too_many {
            return Ok(PullOutcome::Held { would_remove, cached });
        }
        Ok(PullOutcome::Removed(self.confirm_server_removals(server_id, pull_id)?))
    }

    /// Apply a held removal after the user confirmed it. Returns the rows
    /// removed.
    pub fn confirm_server_removals(
        &self,
        server_id: &str,
        pull_id: i64,
    ) -> Result<Vec<ServerTrackRow>> {
        let gone = self.query_server_rows(
            &format!(
                "SELECT {SONG_COLUMNS} FROM server_tracks
                 WHERE server_id = ?1 AND seen_pull <> ?2 ORDER BY path_key"
            ),
            params![server_id, pull_id],
        )?;
        self.conn.execute(
            "DELETE FROM server_tracks WHERE server_id = ?1 AND seen_pull <> ?2",
            params![server_id, pull_id],
        )?;
        Ok(gone)
    }

    /// Songs of `server_id` that pull `pull_id` did not see: about to be
    /// removed, unless they only moved.
    pub fn unseen_rows(&self, server_id: &str, pull_id: i64) -> Result<Vec<ServerTrackRow>> {
        self.query_server_rows(
            &format!(
                "SELECT {SONG_COLUMNS} FROM server_tracks
                 WHERE server_id = ?1 AND seen_pull <> ?2"
            ),
            params![server_id, pull_id],
        )
    }

    /// Whether server copy `id` is linked to anything.
    pub fn is_linked(&self, id: i64) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM song_members WHERE server_track_id = ?1)",
            params![id],
            |r| r.get(0),
        )?)
    }

    /// Unlinked songs of `server_id` that pull `pull_id` added.
    pub fn unlinked_rows_added_in(&self, server_id: &str, pull_id: i64) -> Result<Vec<ServerTrackRow>> {
        self.query_server_rows(
            &format!(
                "SELECT {SONG_COLUMNS} FROM server_tracks s
                 WHERE server_id = ?1 AND added_pull = ?2
                   AND NOT EXISTS (SELECT 1 FROM song_members m WHERE m.server_track_id = s.id)"
            ),
            params![server_id, pull_id],
        )
    }

    /// Hand server copy `from`'s place in its song, with its history, to
    /// server copy `to`: the same file at a new path.
    pub fn move_member(&self, from: i64, to: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE song_members SET server_track_id = ?2 WHERE server_track_id = ?1",
            params![from, to],
        )?;
        tx.execute(
            "UPDATE server_tracks SET shown = (SELECT shown FROM server_tracks WHERE id = ?1)
             WHERE id = ?2",
            params![from, to],
        )?;
        tx.execute("UPDATE server_tracks SET shown = 1 WHERE id = ?1", params![from])?;
        tx.commit()?;
        Ok(())
    }

    /// Every cached song of `server_id`, ordered by path.
    pub fn server_songs(&self, server_id: &str) -> Result<Vec<ServerTrackRow>> {
        self.query_server_rows(
            &format!(
                "SELECT {SONG_COLUMNS} FROM server_tracks WHERE server_id = ?1 ORDER BY path_key"
            ),
            params![server_id],
        )
    }

    fn query_server_rows<P: rusqlite::Params>(
        &self,
        sql: &str,
        params: P,
    ) -> Result<Vec<ServerTrackRow>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, row_to_song)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }
}

/// A server-only song as a library row, so lists, playlists and the play
/// queue can carry it like any other track.
///
/// Its `id` is the negated `server_tracks.id` (local rows are positive, 0 is
/// "not in the library"), and its `path` is the song URI, never a stream URL.
pub fn server_track_as_lib_track(row: &ServerTrackRow) -> LibTrack {
    let s = &row.song;
    let key = path_key(s);
    let text = |v: &str| if v.is_empty() { None } else { Some(v.to_string()) };
    let mut t = LibTrack {
        id: -row.id,
        path: crate::servers::uri::song_uri(&row.server_id, &key),
        filename: key.rsplit('/').next().unwrap_or(&key).to_string(),
        title: text(&s.title),
        artist: text(&s.artist),
        album: text(&s.album),
        album_artist: text(&s.album_artist),
        genre: text(&s.genre),
        comment: text(&s.comment),
        track_num: s.track,
        disc_num: s.disc,
        year: s.year,
        bpm: s.bpm.map(|b| b.to_string()),
        length_secs: s.duration_secs.map(|d| d as f64),
        bitrate: s.bit_rate,
        filetype: s.suffix.clone(),
        file_size: s.size,
        play_count: s.play_count as i64,
        last_played: s.played.clone(),
        rg_track_gain: s.replay_gain.as_ref().and_then(|g| g.track_gain),
        rg_track_peak: s.replay_gain.as_ref().and_then(|g| g.track_peak),
        rg_album_gain: s.replay_gain.as_ref().and_then(|g| g.album_gain),
        rg_album_peak: s.replay_gain.as_ref().and_then(|g| g.album_peak),
        artwork_path: row.artwork_path.clone(),
        ..LibTrack::default()
    };
    t.sort_keys = SortKeys::from_track(&t);
    t
}

/// `path` relative to `folder`, or its file name when it is not inside it.
fn relative_to(path: &str, folder: Option<&str>) -> String {
    folder
        .and_then(|f| path.strip_prefix(f))
        .map(|rest| rest.trim_start_matches('/').to_string())
        .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path).to_string())
}

/// Linked songs' servers and status, and possible-match membership.
#[derive(Default)]
struct SongInfo {
    local: HashMap<i64, (Vec<String>, SongStatus)>,
    server: HashMap<i64, (Vec<String>, SongStatus)>,
    possible_servers: HashSet<i64>,
}

/// Whether `word` (lowercase) appears in any searched field, mirroring
/// [`MediaLibrary::search_tracks_sorted`].
fn matches_word(t: &LibTrack, word: &str) -> bool {
    let has = |v: &Option<String>| v.as_deref().is_some_and(|s| s.to_lowercase().contains(word));
    has(&t.artist)
        || has(&t.title)
        || has(&t.album)
        || has(&t.album_artist)
        || has(&t.genre)
        || has(&t.filetype)
        || t.filename.to_lowercase().contains(word)
        || t.year.unwrap_or(0).to_string().contains(word)
}

/// One sort key component.
#[derive(PartialEq, PartialOrd)]
enum SortVal {
    Text(String),
    Num(f64),
    Opt(Option<i64>),
}

/// The ORDER BY of `sort_order_clause`, in Rust: the first key in the
/// requested direction, the tie-breakers ascending. Lowercasing is ASCII
/// only, like SQLite's `LOWER`.
fn compare_rows(a: &LibTrack, b: &LibTrack, col: &str, desc: bool) -> std::cmp::Ordering {
    fn keys(t: &LibTrack, col: &str) -> Vec<SortVal> {
        let s = |v: &Option<String>| SortVal::Text(v.as_deref().unwrap_or("").to_ascii_lowercase());
        let n = |v: Option<i64>| SortVal::Num(v.unwrap_or(0) as f64);
        match col {
            "title" => vec![s(&t.title), s(&t.artist)],
            "album" => vec![s(&t.album), s(&t.artist), SortVal::Opt(t.track_num)],
            "album_artist" => vec![s(&t.album_artist), s(&t.album), SortVal::Opt(t.track_num)],
            "duration" => vec![SortVal::Num(t.length_secs.unwrap_or(0.0)), s(&t.artist)],
            "filename" => vec![SortVal::Text(t.filename.to_ascii_lowercase())],
            "year" => vec![n(t.year), s(&t.artist)],
            "genre" => vec![s(&t.genre), s(&t.artist)],
            "bitrate" => vec![n(t.bitrate), s(&t.artist)],
            "disc_num" => vec![n(t.disc_num), n(t.track_num), s(&t.artist)],
            _ => vec![s(&t.artist), s(&t.album), SortVal::Opt(t.track_num)],
        }
    }
    let (ka, kb) = (keys(a, col), keys(b, col));
    for (i, (x, y)) in ka.iter().zip(&kb).enumerate() {
        let ord = x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal);
        let ord = if i == 0 && desc { ord.reverse() } else { ord };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

/// `(local_track_id, server_track_id)` for a member; exactly one is set.
fn member_columns(m: Member) -> (Option<i64>, Option<i64>) {
    match m {
        Member::Local(id) => (Some(id), None),
        Member::Server(id) => (None, Some(id)),
    }
}

/// The durable key of a server song: its reported path, or its song ID on a
/// server that reports no path at all. The path half of its song URI.
pub fn path_key(song: &ServerSong) -> String {
    match &song.path {
        Some(p) if !p.is_empty() => p.clone(),
        _ => format!("id:{}", song.id),
    }
}

fn write_song(conn: &rusqlite::Connection, id: i64, s: &ServerSong) -> rusqlite::Result<usize> {
    let rg = s.replay_gain.clone().unwrap_or_default();
    conn.execute(
        "UPDATE server_tracks SET path = ?2, song_id = ?3, title = ?4, artist = ?5, album = ?6,
            album_artist = ?7, genre = ?8, comment = ?9, track_num = ?10, disc_num = ?11,
            year = ?12, bpm = ?13, length_secs = ?14, file_size = ?15, suffix = ?16,
            bitrate = ?17, cover_art = ?18, rating = ?19, play_count = ?20, played = ?21,
            musicbrainz_id = ?22, isrc = ?23, rg_track_gain = ?24, rg_track_peak = ?25,
            rg_album_gain = ?26, rg_album_peak = ?27, album_id = ?28
         WHERE id = ?1",
        params![
            id,
            s.path,
            s.id,
            s.title,
            s.artist,
            s.album,
            s.album_artist,
            s.genre,
            s.comment,
            s.track,
            s.disc,
            s.year,
            s.bpm,
            s.duration_secs,
            s.size,
            s.suffix,
            s.bit_rate,
            s.cover_art,
            s.user_rating as i64,
            s.play_count as i64,
            s.played,
            s.musicbrainz_id,
            s.isrc.join(";"),
            rg.track_gain,
            rg.track_peak,
            rg.album_gain,
            rg.album_peak,
            s.album_id,
        ],
    )
}

fn row_to_song(r: &rusqlite::Row<'_>) -> rusqlite::Result<ServerTrackRow> {
    let isrc: String = r.get(23)?;
    let rg = ReplayGain {
        track_gain: r.get(24)?,
        track_peak: r.get(25)?,
        album_gain: r.get(26)?,
        album_peak: r.get(27)?,
    };
    Ok(ServerTrackRow {
        id: r.get(0)?,
        server_id: r.get(1)?,
        song: ServerSong {
            path: r.get(2)?,
            id: r.get(3)?,
            title: r.get(4)?,
            artist: r.get(5)?,
            album: r.get(6)?,
            album_artist: r.get(7)?,
            genre: r.get(8)?,
            comment: r.get(9)?,
            track: r.get(10)?,
            disc: r.get(11)?,
            year: r.get(12)?,
            bpm: r.get(13)?,
            duration_secs: r.get(14)?,
            size: r.get(15)?,
            suffix: r.get(16)?,
            bit_rate: r.get(17)?,
            cover_art: r.get(18)?,
            user_rating: r.get::<_, i64>(19)? as u8,
            play_count: r.get::<_, i64>(20)? as u64,
            played: r.get(21)?,
            musicbrainz_id: r.get(22)?,
            isrc: if isrc.is_empty() {
                Vec::new()
            } else {
                isrc.split(';').map(str::to_string).collect()
            },
            replay_gain: if rg == ReplayGain::default() { None } else { Some(rg) },
            album_id: r.get(28)?,
        },
        artwork_path: r.get(29)?,
    })
}
