//! Deciding which server songs are the same song as a local file.
//!
//! Tiers are tried strongest first, and a tier only links when it finds
//! exactly one candidate. Two or more candidates at a tier make a possible
//! match for the user instead, and weaker tiers are not consulted: if the
//! tags fit two files, a filename happening to fit one of them proves little.

use crate::dedupe::normalize;
use std::collections::{HashMap, HashSet};

/// How a link was made. Shown in the Copies panel so a wrong link is easy
/// to spot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkReason {
    /// Same path below the library root.
    Path,
    /// Same MusicBrainz track ID.
    MusicBrainz,
    /// Same ISRC, duration within 2 seconds.
    Isrc,
    /// Same normalized artist, title and album, duration within 2 seconds.
    Tags,
    /// Same file name, duration within 1 second. The only way to match a
    /// file that has no tags on one side.
    Filename,
    /// The user linked them.
    Manual,
}

impl LinkReason {
    /// The stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            LinkReason::Path => "path",
            LinkReason::MusicBrainz => "musicbrainz",
            LinkReason::Isrc => "isrc",
            LinkReason::Tags => "tags",
            LinkReason::Filename => "filename",
            LinkReason::Manual => "manual",
        }
    }

    /// The reason for a stored name. Unknown names read as manual: a link
    /// whose origin is lost is one the user can still see and undo.
    pub fn from_stored(name: &str) -> Self {
        match name {
            "path" => LinkReason::Path,
            "musicbrainz" => LinkReason::MusicBrainz,
            "isrc" => LinkReason::Isrc,
            "tags" => LinkReason::Tags,
            "filename" => LinkReason::Filename,
            _ => LinkReason::Manual,
        }
    }
}

/// A local file that could be matched.
#[derive(Debug, Clone, Default)]
pub struct LocalCandidate {
    pub id: i64,
    /// Path relative to its watched folder, `/`-separated.
    pub rel_path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_secs: Option<f64>,
    pub musicbrainz_id: Option<String>,
    pub isrc: Vec<String>,
}

/// A server song that could be matched.
#[derive(Debug, Clone, Default)]
pub struct ServerCandidate {
    /// The caller's key for this server copy.
    pub key: i64,
    /// The path the server reports.
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_secs: Option<f64>,
    pub musicbrainz_id: Option<String>,
    pub isrc: Vec<String>,
}

/// A link the matcher is confident about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub server: i64,
    pub local: i64,
    pub how: LinkReason,
}

/// A server song with more than one plausible local file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PossibleMatch {
    pub server: i64,
    pub candidates: Vec<i64>,
}

/// What matching produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MatchResult {
    pub matches: Vec<Match>,
    pub possible: Vec<PossibleMatch>,
}

/// Match `servers` against `locals`. `never_link` holds `(local id, server
/// key)` pairs the user unlinked; they are never proposed again.
pub fn match_songs(
    locals: &[LocalCandidate],
    servers: &[ServerCandidate],
    never_link: &[(i64, i64)],
) -> MatchResult {
    let index = Index::build(locals);
    let blocked: HashSet<(i64, i64)> = never_link.iter().copied().collect();

    // First pass: each server song's candidates at its strongest tier.
    let mut proposals: Vec<(i64, Vec<usize>, LinkReason)> = Vec::new();
    for s in servers {
        let allowed = |i: &usize| !blocked.contains(&(locals[*i].id, s.key));
        let near = |i: &usize, secs: f64| within(locals[*i].duration_secs, s.duration_secs, secs);
        let tiers: [(LinkReason, Vec<usize>); 5] = [
            (LinkReason::Path, index.by_path(&s.path)),
            (
                LinkReason::MusicBrainz,
                s.musicbrainz_id.as_ref().map(|id| index.get(&index.mbid, id)).unwrap_or_default(),
            ),
            (
                LinkReason::Isrc,
                s.isrc
                    .iter()
                    .flat_map(|code| index.get(&index.isrc, code))
                    .filter(|i| near(i, 2.0))
                    .collect(),
            ),
            (
                LinkReason::Tags,
                tag_key(&s.artist, &s.title, &s.album)
                    .map(|k| index.get(&index.tags, &k))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|i| near(i, 2.0))
                    .collect(),
            ),
            (
                LinkReason::Filename,
                index
                    .get(&index.filename, &file_name(&s.path))
                    .into_iter()
                    .filter(|i| near(i, 1.0))
                    .collect(),
            ),
        ];
        for (how, found) in tiers {
            let mut found: Vec<usize> = found.into_iter().filter(allowed).collect();
            found.sort_unstable();
            found.dedup();
            if !found.is_empty() {
                proposals.push((s.key, found, how));
                break;
            }
        }
    }

    // Second pass: a local file wanted by two server songs links to neither.
    let mut claims: HashMap<usize, usize> = HashMap::new();
    for (_, found, _) in &proposals {
        if found.len() == 1 {
            *claims.entry(found[0]).or_default() += 1;
        }
    }
    let mut result = MatchResult::default();
    for (server, found, how) in proposals {
        if found.len() == 1 && claims[&found[0]] == 1 {
            result.matches.push(Match { server, local: locals[found[0]].id, how });
        } else {
            let mut candidates: Vec<i64> = found.iter().map(|i| locals[*i].id).collect();
            candidates.sort_unstable();
            result.possible.push(PossibleMatch { server, candidates });
        }
    }
    result
}

/// Lookups from each matching key to the local files carrying it.
struct Index {
    path_suffix: HashMap<String, Vec<usize>>,
    mbid: HashMap<String, Vec<usize>>,
    isrc: HashMap<String, Vec<usize>>,
    tags: HashMap<String, Vec<usize>>,
    filename: HashMap<String, Vec<usize>>,
}

impl Index {
    fn build(locals: &[LocalCandidate]) -> Self {
        let mut ix = Index {
            path_suffix: HashMap::new(),
            mbid: HashMap::new(),
            isrc: HashMap::new(),
            tags: HashMap::new(),
            filename: HashMap::new(),
        };
        for (i, l) in locals.iter().enumerate() {
            // Only the full relative path is indexed; a server path is then
            // tried from its longest suffix down.
            let rel = l.rel_path.to_lowercase();
            if rel.matches('/').count() >= 1 {
                ix.path_suffix.entry(rel).or_default().push(i);
            }
            if let Some(id) = &l.musicbrainz_id {
                ix.mbid.entry(id.clone()).or_default().push(i);
            }
            for code in &l.isrc {
                ix.isrc.entry(code.clone()).or_default().push(i);
            }
            if let Some(k) = tag_key(&l.artist, &l.title, &l.album) {
                ix.tags.entry(k).or_default().push(i);
            }
            ix.filename.entry(file_name(&l.rel_path)).or_default().push(i);
        }
        ix
    }

    fn get(&self, map: &HashMap<String, Vec<usize>>, key: &str) -> Vec<usize> {
        map.get(key).cloned().unwrap_or_default()
    }

    /// Local files whose relative path is the longest `/`-bounded suffix of
    /// `server_path` with at least two components.
    fn by_path(&self, server_path: &str) -> Vec<usize> {
        let lower = server_path.to_lowercase();
        let parts: Vec<&str> = lower.split('/').filter(|p| !p.is_empty()).collect();
        for start in 0..parts.len().saturating_sub(1) {
            if let Some(found) = self.path_suffix.get(&parts[start..].join("/")) {
                return found.clone();
            }
        }
        Vec::new()
    }
}

/// Normalized artist, title and album; `None` when there is no title to go on.
fn tag_key(artist: &str, title: &str, album: &str) -> Option<String> {
    let title = normalize(title);
    if title.is_empty() {
        return None;
    }
    Some(format!("{}\u{1}{}\u{1}{}", normalize(artist), title, normalize(album)))
}

fn file_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_lowercase()
}

fn within(a: Option<f64>, b: Option<f64>, secs: f64) -> bool {
    matches!((a, b), (Some(a), Some(b)) if (a - b).abs() <= secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(id: i64, rel: &str, artist: &str, title: &str, album: &str, secs: f64) -> LocalCandidate {
        LocalCandidate {
            id,
            rel_path: rel.into(),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            duration_secs: Some(secs),
            ..LocalCandidate::default()
        }
    }

    fn server(key: i64, path: &str, artist: &str, title: &str, album: &str, secs: f64) -> ServerCandidate {
        ServerCandidate {
            key,
            path: path.into(),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            duration_secs: Some(secs),
            ..ServerCandidate::default()
        }
    }

    fn one(result: MatchResult) -> Match {
        assert!(result.possible.is_empty(), "{result:?}");
        assert_eq!(result.matches.len(), 1, "{result:?}");
        result.matches[0].clone()
    }

    #[test]
    fn same_path_below_the_library_root_links_by_path() {
        let m = one(match_songs(
            &[local(1, "Miles Davis/Kind of Blue/01 So What.mp3", "", "", "", 562.0)],
            &[server(10, "/music/Miles Davis/Kind of Blue/01 So What.mp3", "Miles Davis", "So What", "Kind of Blue", 562.0)],
            &[],
        ));
        assert_eq!(m, Match { server: 10, local: 1, how: LinkReason::Path });
    }

    #[test]
    fn a_different_folder_layout_links_by_tags_within_two_seconds() {
        let m = one(match_songs(
            &[local(1, "Miles Davis - Kind of Blue/So What.mp3", "Miles Davis", "So What", "Kind of Blue", 563.4)],
            &[server(10, "/music/Miles Davis/Kind of Blue/01 So What.flac", "Miles Davis", "So What!", "Kind Of Blue", 562.0)],
            &[],
        ));
        assert_eq!(m.how, LinkReason::Tags);
    }

    #[test]
    fn tags_that_fit_but_a_duration_three_seconds_off_do_not_link() {
        let r = match_songs(
            &[local(1, "a/So What.mp3", "Miles Davis", "So What", "Kind of Blue", 565.5)],
            &[server(10, "/music/b/01.mp3", "Miles Davis", "So What", "Kind of Blue", 562.0)],
            &[],
        );
        assert_eq!(r, MatchResult::default());
    }

    /// The design's easy case: the server copy is untagged, the local copy is
    /// tagged, only the file name and length connect them.
    #[test]
    fn an_untagged_server_file_links_by_filename_and_duration() {
        let m = one(match_songs(
            &[local(1, "Rips/test.mp3", "Artist", "Test Song", "Album", 201.4)],
            &[server(10, "/music/incoming/test.mp3", "", "", "", 201.0)],
            &[],
        ));
        assert_eq!(m, Match { server: 10, local: 1, how: LinkReason::Filename });
    }

    #[test]
    fn musicbrainz_id_links_even_when_tags_differ() {
        let mut l = local(1, "x/1.mp3", "Artist", "Old Title", "", 100.0);
        l.musicbrainz_id = Some("0b4e6f0f".into());
        let mut s = server(10, "/music/y/2.mp3", "Artist", "New Title", "", 180.0);
        s.musicbrainz_id = Some("0b4e6f0f".into());
        assert_eq!(one(match_songs(&[l], &[s], &[])).how, LinkReason::MusicBrainz);
    }

    #[test]
    fn isrc_links_within_two_seconds() {
        let mut l = local(1, "x/1.mp3", "", "", "", 200.0);
        l.isrc = vec!["USSM15900113".into()];
        let mut s = server(10, "/music/y/2.mp3", "", "", "", 201.0);
        s.isrc = vec!["GBAYE0601498".into(), "USSM15900113".into()];
        assert_eq!(one(match_songs(&[l], &[s], &[])).how, LinkReason::Isrc);
    }

    #[test]
    fn two_local_files_that_both_fit_make_a_possible_match_not_a_link() {
        let r = match_songs(
            &[
                local(1, "Hits/So What.mp3", "Miles Davis", "So What", "Kind of Blue", 562.0),
                local(2, "Kind of Blue/So What.mp3", "Miles Davis", "So What", "Kind of Blue", 562.5),
            ],
            &[server(10, "/music/Miles/01.mp3", "Miles Davis", "So What", "Kind of Blue", 562.0)],
            &[],
        );
        assert!(r.matches.is_empty());
        assert_eq!(r.possible, vec![PossibleMatch { server: 10, candidates: vec![1, 2] }]);
    }

    #[test]
    fn two_server_copies_of_one_local_file_both_become_possible_matches() {
        let r = match_songs(
            &[local(1, "a/So What.mp3", "Miles Davis", "So What", "Kind of Blue", 562.0)],
            &[
                server(10, "/music/x/01.mp3", "Miles Davis", "So What", "Kind of Blue", 562.0),
                server(11, "/music/dupes/01.mp3", "Miles Davis", "So What", "Kind of Blue", 562.0),
            ],
            &[],
        );
        assert!(r.matches.is_empty());
        assert_eq!(
            r.possible,
            vec![
                PossibleMatch { server: 10, candidates: vec![1] },
                PossibleMatch { server: 11, candidates: vec![1] },
            ]
        );
    }

    #[test]
    fn a_pair_the_user_unlinked_is_never_proposed_again() {
        let r = match_songs(
            &[local(1, "a/So What.mp3", "Miles Davis", "So What", "Kind of Blue", 562.0)],
            &[server(10, "/music/x/01.mp3", "Miles Davis", "So What", "Kind of Blue", 562.0)],
            &[(1, 10)],
        );
        assert_eq!(r, MatchResult::default());
    }

    /// A strong tier with two candidates stops the search: a weaker tier
    /// that happens to single one out is not trusted over that ambiguity.
    #[test]
    fn ambiguity_at_a_strong_tier_is_not_resolved_by_a_weaker_one() {
        let r = match_songs(
            &[
                local(1, "a/01.mp3", "Miles Davis", "So What", "Kind of Blue", 562.0),
                local(2, "b/other.mp3", "Miles Davis", "So What", "Kind of Blue", 562.0),
            ],
            &[server(10, "/music/z/01.mp3", "Miles Davis", "So What", "Kind of Blue", 562.0)],
            &[],
        );
        assert!(r.matches.is_empty());
        assert_eq!(r.possible.len(), 1);
    }

    #[test]
    fn a_file_at_the_top_of_a_folder_does_not_link_by_path_alone() {
        // "01.mp3" is too common a name to be a path match.
        let r = match_songs(
            &[local(1, "01.mp3", "A", "One", "X", 100.0)],
            &[server(10, "/music/B/Y/01.mp3", "B", "Two", "Y", 300.0)],
            &[],
        );
        assert_eq!(r, MatchResult::default());
    }
}
