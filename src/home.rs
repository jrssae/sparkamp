//! The user's folders, for every path the core builds under them.
//!
//! The same as the `dirs` functions, except that a test build first moves
//! `HOME` to a throwaway folder ([`crate::testing::isolate_home`]), so no
//! test of the core can write into the user's real settings, library or
//! caches. Use these, not `dirs`, anywhere in the core.

use std::path::{Path, PathBuf};

#[inline]
fn guard() {
    #[cfg(test)]
    crate::testing::isolate_home();
}

pub(crate) fn config_dir() -> Option<PathBuf> {
    guard();
    dirs::config_dir()
}

pub(crate) fn cache_dir() -> Option<PathBuf> {
    guard();
    dirs::cache_dir()
}

pub(crate) fn data_dir() -> Option<PathBuf> {
    guard();
    dirs::data_dir()
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    guard();
    dirs::home_dir()
}

/// Create `dir` and its parents, then make `dir` its owner's alone (0700 on
/// Unix). For folders holding what a private server sent, which other
/// accounts on the machine have no business reading.
pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    restrict(dir, 0o700)
}

/// Make the file at `path` its owner's alone (0600 on Unix).
pub(crate) fn make_private_file(path: &Path) -> std::io::Result<()> {
    restrict(path, 0o600)
}

#[cfg(unix)]
fn restrict(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn restrict(_: &Path, _: u32) -> std::io::Result<()> {
    Ok(())
}
