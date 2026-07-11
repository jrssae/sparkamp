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

#[cfg(test)]
mod tests {
    use super::*;

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
