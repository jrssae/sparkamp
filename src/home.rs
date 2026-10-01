//! The user's folders, for every path the core builds under them.
//!
//! The same as the `dirs` functions, except that a test build first moves
//! `HOME` to a throwaway folder ([`crate::testing::isolate_home`]), so no
//! test of the core can write into the user's real settings, library or
//! caches. Use these, not `dirs`, anywhere in the core.

use std::path::PathBuf;

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
