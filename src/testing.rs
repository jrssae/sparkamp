//! Support for test builds, here so the TUI's and GTK's test binaries can
//! reach it as well as the core's own tests.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Point `HOME` and the XDG base folders at a throwaway folder for the rest
/// of this process, once. Tests call it before building anything that saves
/// settings, opens the library or writes a cache.
///
/// Without it a test run wrote straight into the user's real folders: the
/// config (replacing their settings, server list included), the library
/// database, the duration cache, the disc tag store and the cover cache.
/// The core's own paths call this in test builds (see `crate::home`); the
/// frontends' test binaries link the core without `cfg(test)`, so their
/// helpers call it themselves.
#[doc(hidden)]
pub fn isolate_home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("sparkamp-test-home-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a throwaway home for tests");
        // SAFETY: test-only, and done once before the paths are first read;
        // nothing else in a test process sets these variables.
        unsafe {
            std::env::set_var("HOME", &dir);
            std::env::set_var("XDG_CONFIG_HOME", dir.join(".config"));
            std::env::set_var("XDG_CACHE_HOME", dir.join(".cache"));
            std::env::set_var("XDG_DATA_HOME", dir.join(".local/share"));
        }
        dir
    })
}

#[cfg(test)]
mod tests {
    /// The guard that keeps test runs out of the user's settings: every path
    /// the core builds under the home folder lands in the throwaway one.
    #[test]
    fn core_paths_in_tests_never_reach_the_real_home() {
        let throwaway = super::isolate_home();
        for path in [
            crate::config::Config::config_path(),
            crate::media_library::MediaLibrary::db_path_pub(),
            crate::home::cache_dir().unwrap(),
        ] {
            assert!(path.starts_with(throwaway), "{path:?} is outside {throwaway:?}");
        }
    }
}
