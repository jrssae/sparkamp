//! Taking server changes into local files, and undoing that.
//!
//! This is the one place server sync writes to the user's files, and it only
//! runs when the user asks ("Apply server changes", one song or a batch).
//! Background updates never write to local files. Before writing, the
//! previous values are saved, so the last apply, however many files it
//! touched, can be put back.

use super::merge::{CopyId, Field, FieldOutcome, Fields, Value};
use super::normalize;
use crate::id3_editor::{TagFields, read_tag_fields, write_tag_fields};
use crate::media_library::MediaLibrary;
use crate::media_library::servers::Member;
use anyhow::Result;
use std::path::Path;

/// What applying server changes to one song did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApplyOutcome {
    /// Fields written into the local file (and library).
    pub taken: Vec<Field>,
    /// Fields the file would not take, with why. Nothing changed for these;
    /// today that is a rating on a file without a rating tag or a read-only
    /// file.
    pub refused: Vec<(Field, String)>,
}

/// Write every field where a server copy won and the local copy is behind
/// into the local file, and refresh its library row. A rating goes into the
/// file first and only then into the library; a file that will not take it
/// leaves the rating unchanged. Starts a new undo batch when anything is
/// written.
pub fn apply_server_changes(lib: &MediaLibrary, member: Member) -> Result<ApplyOutcome> {
    let mut batch = None;
    apply_one(lib, member, &mut batch)
}

/// [`apply_server_changes`] for many songs as one undoable batch ("Apply N
/// server changes"). Returns how many files were written.
pub fn apply_server_changes_to_all(lib: &MediaLibrary, members: &[Member]) -> Result<usize> {
    let mut batch = None;
    let mut written = 0;
    for m in members {
        if !apply_one(lib, *m, &mut batch)?.taken.is_empty() {
            written += 1;
        }
    }
    Ok(written)
}

fn apply_one(lib: &MediaLibrary, member: Member, batch: &mut Option<i64>) -> Result<ApplyOutcome> {
    let Some(merged) = lib.song_merge(member)? else { return Ok(ApplyOutcome::default()) };
    let wins: Vec<(Field, Value)> = merged
        .fields
        .iter()
        .filter_map(|(f, o)| match o {
            FieldOutcome::Wins { value, behind } if behind.contains(&CopyId::Local) => {
                Some((*f, value.clone()))
            }
            _ => None,
        })
        .collect();
    if wins.is_empty() {
        return Ok(ApplyOutcome::default());
    }
    let Some(local_id) = lib.song_copies(member)?.and_then(|c| {
        c.members.iter().find_map(|m| match m.member {
            Member::Local(id) => Some(id),
            Member::Server(_) => None,
        })
    }) else {
        return Ok(ApplyOutcome::default());
    };
    let Some(track) = lib.tracks_by_ids(&[local_id])?.remove(&local_id) else {
        return Ok(ApplyOutcome::default());
    };
    let rating = lib.local_rating(local_id)?;
    let batch_id = match *batch {
        Some(b) => b,
        None => {
            let b = lib.start_apply_batch()?;
            *batch = Some(b);
            b
        }
    };
    lib.record_apply_undo(batch_id, local_id, &track.path, &normalize::local_fields(&track, rating))?;

    let path = Path::new(&track.path);
    let mut outcome = ApplyOutcome::default();
    let mut tags = read_tag_fields(path);
    let mut new_rating = None;
    for (field, value) in &wins {
        match value {
            Value::Rating(r) => new_rating = Some(*r),
            v => {
                set_tag(&mut tags, *field, v);
                outcome.taken.push(*field);
            }
        }
    }
    if !outcome.taken.is_empty() {
        write_tag_fields(path, &tags)?;
        lib.rescan_track(&track.path)?;
    }
    // The file first, then the library; a file that refuses changes nothing.
    if let Some(r) = new_rating {
        match crate::rating::write_rating(path, r) {
            Ok(()) => {
                lib.set_local_rating(local_id, r)?;
                outcome.taken.push(Field::Rating);
            }
            Err(e) => outcome.refused.push((Field::Rating, e.to_string())),
        }
    }
    Ok(outcome)
}

/// Put back the files the last apply changed. Returns how many.
pub fn undo_last_apply(lib: &MediaLibrary) -> Result<usize> {
    let saved = lib.take_last_apply_batch()?;
    for (track_id, path, before) in &saved {
        let p = Path::new(path);
        let mut tags = read_tag_fields(p);
        restore_tags(&mut tags, before);
        write_tag_fields(p, &tags)?;
        lib.rescan_track(path)?;
        // Same order as any rating: the file, then the library.
        if crate::rating::write_rating(p, before.rating).is_ok() {
            lib.set_local_rating(*track_id, before.rating)?;
        }
    }
    Ok(saved.len())
}

fn set_tag(tags: &mut TagFields, field: Field, value: &Value) {
    let text = match value {
        Value::Text(s) => s.clone(),
        Value::Number(n) => n.map(|n| n.to_string()).unwrap_or_default(),
        Value::Rating(_) => return,
    };
    match field {
        Field::Title => tags.title = text,
        Field::Artist => tags.artist = text,
        Field::Album => tags.album = text,
        Field::AlbumArtist => tags.album_artist = text,
        Field::Genre => tags.genre = text,
        Field::Comment => tags.comment = text,
        Field::Track => tags.track_number = text,
        Field::Disc => tags.disc_number = text,
        Field::Year => tags.year = text,
        Field::Bpm => tags.bpm = text,
        Field::Rating => {}
    }
}

fn restore_tags(tags: &mut TagFields, before: &Fields) {
    for field in Field::ALL {
        let value = before.get(field);
        set_tag(tags, field, &value);
    }
}
