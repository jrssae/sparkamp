//! Small path helpers shared across the core.

use std::path::{Path, PathBuf};

/// Canonicalize what exists of `path`: resolve its nearest existing ancestor
/// (following symlinks) and re-append the not-yet-created tail.
///
/// Plain [`Path::canonicalize`] fails outright on a path that doesn't exist
/// yet, so callers that fall back to the raw path get an *unresolved* string.
/// On macOS, where temp/home live under `/var` → `/private/var` (and Flatpak
/// document-portal mounts are similarly indirected on Linux), that raw path
/// then never `starts_with` a resolved watched folder — so a rip destination or
/// a not-yet-created watch folder is wrongly judged "outside the library". This
/// resolves the existing part so the comparison holds either way; a path with
/// no resolvable ancestor at all falls back to its literal form.
pub fn canonicalize_lenient(path: &Path) -> PathBuf {
    if let Ok(p) = path.canonicalize() {
        return p;
    }
    let mut ancestor = path;
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    while let Some(parent) = ancestor.parent() {
        if let Some(name) = ancestor.file_name() {
            tail.push(name);
        }
        if let Ok(resolved) = parent.canonicalize() {
            let mut out = resolved;
            out.extend(tail.iter().rev());
            return out;
        }
        ancestor = parent;
    }
    path.to_path_buf()
}

/// Make `name` safe to use as one file or folder name: characters that are
/// path separators or illegal on FAT (`/ \\ : * ? " < > |`) and control
/// characters become `_`, and surrounding spaces and dots are trimmed, so a
/// name can neither climb out of its directory (`..`) nor hide itself.
/// `fallback` is used when nothing usable is left.
///
/// The one place this is decided. Names here come from tags, gnudb, CD-TEXT
/// and what users type, any of which can carry a control character; a NUL
/// in a track title used to fail that track's rip.
pub fn safe_file_name(name: &str, fallback: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_file_name_replaces_separators_and_illegal_characters() {
        assert_eq!(safe_file_name("AC/DC", "x"), "AC_DC");
        assert_eq!(safe_file_name("a:b*c?", "x"), "a_b_c_");
        assert_eq!(safe_file_name("Road/Trip:2024", "x"), "Road_Trip_2024");
    }

    #[test]
    fn safe_file_name_replaces_control_characters() {
        assert_eq!(safe_file_name("bad\0name", "x"), "bad_name");
        assert_eq!(safe_file_name("tab\there", "x"), "tab_here");
    }

    #[test]
    fn safe_file_name_cannot_climb_out_and_falls_back_when_empty() {
        assert_eq!(safe_file_name("..", "Unknown"), "Unknown");
        assert_eq!(safe_file_name("  ..  ", "Unknown"), "Unknown");
        assert_eq!(safe_file_name("", "Playlist"), "Playlist");
        assert_eq!(safe_file_name(".hidden.", "x"), "hidden");
    }

    #[test]
    fn resolves_a_not_yet_created_subpath_via_its_existing_ancestor() {
        let base = std::env::temp_dir().join(format!("sparkamp-canon-{}", std::process::id()));
        let existing = base.join("Music");
        std::fs::create_dir_all(&existing).unwrap();

        // Existing path: fully resolved (symlinks followed), so it equals the
        // OS canonical form — matching the branch that already worked.
        assert_eq!(
            canonicalize_lenient(&existing),
            existing.canonicalize().unwrap()
        );

        // Not-yet-created child: resolved through the existing ancestor and the
        // literal tail re-appended, so it stays under the resolved ancestor.
        let child = existing.join("New Album");
        let resolved_child = canonicalize_lenient(&child);
        assert!(resolved_child.starts_with(existing.canonicalize().unwrap()));
        assert!(resolved_child.ends_with("New Album"));

        // A totally non-existent absolute path round-trips componentwise.
        let nowhere = Path::new("/no/such/place/at/all");
        assert_eq!(canonicalize_lenient(nowhere), nowhere.to_path_buf());

        let _ = std::fs::remove_dir_all(&base);
    }
}
