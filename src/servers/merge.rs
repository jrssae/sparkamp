//! Comparing any number of copies of one song, field by field.
//!
//! Each copy remembers its own last agreed field values. For every field:
//! if no copy changed it, nothing happens; if the copies that changed it all
//! agree on the new value, that value wins and the other copies are behind;
//! if they disagree, that one field is a conflict for the user. A copy with
//! no history (just linked) is compared with the rest of the song instead.
//!
//! Play count is deliberately not a field here. It follows its own rule (the
//! highest count wins locally, and old differences are never scrobbled).

/// Which copy of a song.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CopyId {
    Local,
    /// A server copy, by server id.
    Server(String),
}

/// The fields compared between copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Title,
    Artist,
    Album,
    AlbumArtist,
    Genre,
    Comment,
    Track,
    Disc,
    Year,
    Bpm,
    Rating,
}

impl Field {
    pub const ALL: [Field; 11] = [
        Field::Title,
        Field::Artist,
        Field::Album,
        Field::AlbumArtist,
        Field::Genre,
        Field::Comment,
        Field::Track,
        Field::Disc,
        Field::Year,
        Field::Bpm,
        Field::Rating,
    ];
}

/// One field's value. Empty text, no number and rating 0 all count as empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Number(Option<i64>),
    Rating(u8),
}

impl Value {
    pub fn is_empty(&self) -> bool {
        match self {
            Value::Text(s) => s.is_empty(),
            Value::Number(n) => n.is_none(),
            Value::Rating(r) => *r == 0,
        }
    }
}

/// The compared fields of one copy, already normalized by the caller.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Fields {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub genre: String,
    pub comment: String,
    pub track: Option<i64>,
    pub disc: Option<i64>,
    pub year: Option<i64>,
    pub bpm: Option<i64>,
    /// 0 to 5; 0 is unrated.
    pub rating: u8,
}

impl Fields {
    pub fn get(&self, field: Field) -> Value {
        match field {
            Field::Title => Value::Text(self.title.clone()),
            Field::Artist => Value::Text(self.artist.clone()),
            Field::Album => Value::Text(self.album.clone()),
            Field::AlbumArtist => Value::Text(self.album_artist.clone()),
            Field::Genre => Value::Text(self.genre.clone()),
            Field::Comment => Value::Text(self.comment.clone()),
            Field::Track => Value::Number(self.track),
            Field::Disc => Value::Number(self.disc),
            Field::Year => Value::Number(self.year),
            Field::Bpm => Value::Number(self.bpm),
            Field::Rating => Value::Rating(self.rating),
        }
    }

    /// Set one field. A value of the wrong kind for the field is ignored.
    pub fn set(&mut self, field: Field, value: &Value) {
        match (field, value) {
            (Field::Title, Value::Text(v)) => self.title = v.clone(),
            (Field::Artist, Value::Text(v)) => self.artist = v.clone(),
            (Field::Album, Value::Text(v)) => self.album = v.clone(),
            (Field::AlbumArtist, Value::Text(v)) => self.album_artist = v.clone(),
            (Field::Genre, Value::Text(v)) => self.genre = v.clone(),
            (Field::Comment, Value::Text(v)) => self.comment = v.clone(),
            (Field::Track, Value::Number(v)) => self.track = *v,
            (Field::Disc, Value::Number(v)) => self.disc = *v,
            (Field::Year, Value::Number(v)) => self.year = *v,
            (Field::Bpm, Value::Number(v)) => self.bpm = *v,
            (Field::Rating, Value::Rating(v)) => self.rating = *v,
            _ => {}
        }
    }
}

/// One copy as the merge sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyState {
    pub copy: CopyId,
    pub current: Fields,
    /// The last agreed values, or `None` for a copy linked since.
    pub baseline: Option<Fields>,
}

/// What happens to one field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldOutcome {
    /// Nothing to do. Any remaining difference was accepted earlier.
    Agreed,
    /// `value` wins; the copies in `behind` should take it.
    Wins { value: Value, behind: Vec<CopyId> },
    /// Copies changed the field to different values since they last agreed.
    Conflict { values: Vec<(CopyId, Value)> },
    /// Newly linked copies hold different non-empty values and there is no
    /// history to say which is newer: the user picks ("?").
    Choose { values: Vec<(CopyId, Value)> },
}

/// A song's overall state, most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SongStatus {
    Conflict,
    /// Something to take into the local copy.
    ServerChanged,
    /// The local copy is ahead of at least one server.
    LocalChanged,
    /// Newly linked copies differ and need a choice.
    FirstLinkDiffers,
    InSync,
}

/// The outcome for every field of one song.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongMerge {
    pub fields: Vec<(Field, FieldOutcome)>,
}

impl SongMerge {
    pub fn outcome(&self, field: Field) -> &FieldOutcome {
        &self.fields.iter().find(|(f, _)| *f == field).expect("every field is present").1
    }

    /// The indicator state: the most urgent across all fields.
    pub fn status(&self) -> SongStatus {
        self.fields
            .iter()
            .map(|(_, outcome)| match outcome {
                FieldOutcome::Agreed => SongStatus::InSync,
                FieldOutcome::Conflict { .. } => SongStatus::Conflict,
                FieldOutcome::Choose { .. } => SongStatus::FirstLinkDiffers,
                FieldOutcome::Wins { behind, .. } if behind.contains(&CopyId::Local) => {
                    SongStatus::ServerChanged
                }
                FieldOutcome::Wins { .. } => SongStatus::LocalChanged,
            })
            .min()
            .unwrap_or(SongStatus::InSync)
    }
}

/// Merge the copies of one song.
pub fn merge(copies: &[CopyState]) -> SongMerge {
    SongMerge { fields: Field::ALL.iter().map(|f| (*f, merge_field(*f, copies))).collect() }
}

fn merge_field(field: Field, copies: &[CopyState]) -> FieldOutcome {
    let value = |c: &CopyState| c.current.get(field);
    let known: Vec<&CopyState> = copies.iter().filter(|c| c.baseline.is_some()).collect();
    let fresh: Vec<&CopyState> = copies.iter().filter(|c| c.baseline.is_none()).collect();

    // Among copies with history: who changed the field since they agreed?
    let changed: Vec<&CopyState> = known
        .iter()
        .copied()
        .filter(|c| c.baseline.as_ref().map(|b| b.get(field)) != Some(value(c)))
        .collect();
    let (known_outcome, song_value) = if changed.is_empty() {
        // Nobody changed it. The song's value is the local one when there is
        // a local copy, since accepted differences leave servers behind.
        let rep = known.iter().find(|c| c.copy == CopyId::Local).or(known.first());
        (FieldOutcome::Agreed, rep.map(|c| value(c)))
    } else {
        let v = value(changed[0]);
        if changed.iter().any(|c| value(c) != v) {
            return FieldOutcome::Conflict {
                values: changed.iter().map(|c| (c.copy.clone(), value(c))).collect(),
            };
        }
        let behind: Vec<CopyId> =
            known.iter().filter(|c| value(c) != v).map(|c| c.copy.clone()).collect();
        (wins_or_agreed(v.clone(), behind), Some(v))
    };
    if fresh.is_empty() {
        return known_outcome;
    }

    // Copies with no history: filled beats empty, two different filled
    // values need a choice.
    let mut candidates: Vec<Value> = song_value.iter().filter(|v| !v.is_empty()).cloned().collect();
    for c in &fresh {
        let v = value(c);
        if !v.is_empty() && !candidates.contains(&v) {
            candidates.push(v);
        }
    }
    match candidates.len() {
        0 => FieldOutcome::Agreed,
        1 => {
            let v = candidates.remove(0);
            // When the song already had this value, the copies with history
            // keep the outcome they had (an accepted difference stays quiet)
            // and only fresh copies can be added as behind. When a fresh copy
            // supplied the value, every copy without it is behind.
            let song_had_it = song_value.as_ref() == Some(&v);
            let mut behind: Vec<CopyId> = match (&known_outcome, song_had_it) {
                (FieldOutcome::Wins { behind, .. }, true) => behind.clone(),
                (_, true) => Vec::new(),
                (_, false) => known
                    .iter()
                    .filter(|c| value(c) != v)
                    .map(|c| c.copy.clone())
                    .collect(),
            };
            behind.extend(fresh.iter().filter(|c| value(c) != v).map(|c| c.copy.clone()));
            behind.sort_by_key(|id| copies.iter().position(|c| &c.copy == id));
            wins_or_agreed(v, behind)
        }
        _ => FieldOutcome::Choose {
            values: copies
                .iter()
                .filter(|c| c.baseline.is_some() || !value(c).is_empty())
                .map(|c| (c.copy.clone(), value(c)))
                .collect(),
        },
    }
}

fn wins_or_agreed(value: Value, behind: Vec<CopyId>) -> FieldOutcome {
    if behind.is_empty() {
        FieldOutcome::Agreed
    } else {
        FieldOutcome::Wins { value, behind }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oscar() -> CopyId {
        CopyId::Server("oscar".into())
    }
    fn server2() -> CopyId {
        CopyId::Server("server2".into())
    }

    fn temp(title: &str, genre: &str, rating: u8) -> Fields {
        Fields {
            title: title.into(),
            artist: "X".into(),
            genre: genre.into(),
            rating,
            ..Fields::default()
        }
    }

    fn copy(id: CopyId, current: Fields, baseline: Option<Fields>) -> CopyState {
        CopyState { copy: id, current, baseline }
    }

    fn text(s: &str) -> Value {
        Value::Text(s.into())
    }

    #[test]
    fn copies_that_still_match_their_agreed_state_are_in_sync() {
        let agreed = temp("Temp", "Rock", 3);
        let m = merge(&[
            copy(CopyId::Local, agreed.clone(), Some(agreed.clone())),
            copy(oscar(), agreed.clone(), Some(agreed.clone())),
        ]);
        assert!(m.fields.iter().all(|(_, o)| *o == FieldOutcome::Agreed));
        assert_eq!(m.status(), SongStatus::InSync);
    }

    /// The worked example in the design: three copies, three different
    /// fields changed in different places, and no conflict.
    #[test]
    fn changes_on_different_fields_merge_across_three_copies() {
        let agreed = temp("Temp", "Rock", 3);
        let m = merge(&[
            copy(CopyId::Local, temp("Temp (Live)", "Rock", 3), Some(agreed.clone())),
            copy(oscar(), temp("Temp", "Live Rock", 3), Some(agreed.clone())),
            copy(server2(), temp("Temp (Live)", "Rock", 4), Some(agreed.clone())),
        ]);
        assert_eq!(
            *m.outcome(Field::Title),
            FieldOutcome::Wins { value: text("Temp (Live)"), behind: vec![oscar()] }
        );
        assert_eq!(
            *m.outcome(Field::Genre),
            FieldOutcome::Wins { value: text("Live Rock"), behind: vec![CopyId::Local, server2()] }
        );
        assert_eq!(
            *m.outcome(Field::Rating),
            FieldOutcome::Wins { value: Value::Rating(4), behind: vec![CopyId::Local, oscar()] }
        );
        assert_eq!(*m.outcome(Field::Artist), FieldOutcome::Agreed);
        assert_eq!(m.status(), SongStatus::ServerChanged);
    }

    #[test]
    fn different_new_values_for_one_field_conflict_on_that_field_only() {
        let agreed = temp("Temp", "Rock", 3);
        let m = merge(&[
            copy(CopyId::Local, temp("Temp (Live)", "Rock", 3), Some(agreed.clone())),
            copy(oscar(), temp("Temp", "Live Rock", 3), Some(agreed.clone())),
            copy(server2(), temp("Temp (Acoustic)", "Rock", 3), Some(agreed.clone())),
        ]);
        assert_eq!(
            *m.outcome(Field::Title),
            FieldOutcome::Conflict {
                values: vec![
                    (CopyId::Local, text("Temp (Live)")),
                    (server2(), text("Temp (Acoustic)")),
                ]
            }
        );
        assert!(matches!(m.outcome(Field::Genre), FieldOutcome::Wins { .. }));
        assert_eq!(m.status(), SongStatus::Conflict);
    }

    #[test]
    fn a_local_edit_the_server_cannot_take_shows_local_changed() {
        let agreed = temp("Temp", "Rock", 3);
        let m = merge(&[
            copy(CopyId::Local, temp("Temp", "Rock", 3), Some(agreed.clone())),
            copy(oscar(), agreed.clone(), Some(agreed.clone())),
        ]);
        assert_eq!(m.status(), SongStatus::InSync);

        let m = merge(&[
            copy(CopyId::Local, temp("Temp", "Jazz", 3), Some(agreed.clone())),
            copy(oscar(), agreed.clone(), Some(agreed.clone())),
        ]);
        assert_eq!(
            *m.outcome(Field::Genre),
            FieldOutcome::Wins { value: text("Jazz"), behind: vec![oscar()] }
        );
        assert_eq!(m.status(), SongStatus::LocalChanged);
    }

    /// "Accept difference" stores each copy's own values as its agreed state,
    /// so a difference nobody touches again stays quiet.
    #[test]
    fn an_accepted_difference_stays_quiet() {
        let local = temp("Temp (Live)", "Rock", 3);
        let server = temp("Temp", "Rock", 3);
        let m = merge(&[
            copy(CopyId::Local, local.clone(), Some(local)),
            copy(oscar(), server.clone(), Some(server)),
        ]);
        assert_eq!(*m.outcome(Field::Title), FieldOutcome::Agreed);
        assert_eq!(m.status(), SongStatus::InSync);
    }

    /// The design's easy case: the server copy has no tags, the local copy
    /// does. With no history, filled beats empty.
    #[test]
    fn first_link_filled_fields_beat_empty_ones() {
        let local = Fields {
            title: "Test Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            rating: 4,
            ..Fields::default()
        };
        let server = Fields { title: "Test Song".into(), ..Fields::default() };
        let m = merge(&[copy(CopyId::Local, local, None), copy(oscar(), server, None)]);
        assert_eq!(*m.outcome(Field::Title), FieldOutcome::Agreed);
        assert_eq!(
            *m.outcome(Field::Artist),
            FieldOutcome::Wins { value: text("Artist"), behind: vec![oscar()] }
        );
        assert_eq!(
            *m.outcome(Field::Rating),
            FieldOutcome::Wins { value: Value::Rating(4), behind: vec![oscar()] }
        );
        assert_eq!(m.status(), SongStatus::LocalChanged);
    }

    #[test]
    fn first_link_with_two_different_filled_values_asks() {
        let m = merge(&[
            copy(CopyId::Local, Fields { title: "Test Song".into(), ..Fields::default() }, None),
            copy(oscar(), Fields { title: "test".into(), ..Fields::default() }, None),
        ]);
        assert_eq!(
            *m.outcome(Field::Title),
            FieldOutcome::Choose {
                values: vec![(CopyId::Local, text("Test Song")), (oscar(), text("test"))]
            }
        );
        assert_eq!(m.status(), SongStatus::FirstLinkDiffers);
    }

    #[test]
    fn a_server_joining_later_is_compared_with_the_song() {
        let agreed = temp("Temp", "Rock", 3);
        let joiner = Fields { title: "Temp".into(), artist: "X".into(), genre: "Blues".into(), ..Fields::default() };
        let m = merge(&[
            copy(CopyId::Local, agreed.clone(), Some(agreed.clone())),
            copy(oscar(), agreed.clone(), Some(agreed.clone())),
            copy(server2(), joiner, None),
        ]);
        assert_eq!(*m.outcome(Field::Title), FieldOutcome::Agreed);
        assert_eq!(
            *m.outcome(Field::Rating),
            FieldOutcome::Wins { value: Value::Rating(3), behind: vec![server2()] },
            "an empty field on the new copy is behind"
        );
        assert_eq!(
            *m.outcome(Field::Genre),
            FieldOutcome::Choose {
                values: vec![
                    (CopyId::Local, text("Rock")),
                    (oscar(), text("Rock")),
                    (server2(), text("Blues")),
                ]
            }
        );
    }

    #[test]
    fn a_server_joining_later_does_not_reopen_an_accepted_difference() {
        let local = temp("Temp (Live)", "Rock", 3);
        let oscar_accepted = temp("Temp", "Rock", 3);
        let m = merge(&[
            copy(CopyId::Local, local.clone(), Some(local.clone())),
            copy(oscar(), oscar_accepted.clone(), Some(oscar_accepted)),
            copy(server2(), local, None),
        ]);
        assert_eq!(*m.outcome(Field::Title), FieldOutcome::Agreed);
        assert_eq!(m.status(), SongStatus::InSync);
    }

    #[test]
    fn status_prefers_the_most_urgent_field() {
        let agreed = temp("Temp", "Rock", 3);
        // Local ahead on genre, server ahead on title: something to take in
        // beats something waiting to go out.
        let m = merge(&[
            copy(CopyId::Local, temp("Temp", "Jazz", 3), Some(agreed.clone())),
            copy(oscar(), temp("Temp 2", "Rock", 3), Some(agreed.clone())),
        ]);
        assert_eq!(m.status(), SongStatus::ServerChanged);
    }
}
