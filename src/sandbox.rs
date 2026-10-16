//! Security-scoped bookmarks: keeping access to a user-picked folder across
//! launches.
//!
//! Outside a sandbox a path string is access. Inside one it is not: the App
//! Sandbox grants a folder to the *process that asked for it*, and that grant
//! dies with the process. What survives is a **bookmark** — an opaque blob the
//! app stores and resolves later to get the same access back.
//!
//! So a sandboxed Sparkamp that persists only `folders.path` starts up with a
//! library it can see the names of and read nothing from. The bookmark has to
//! be stored beside the path when the folder is added, and resolved before
//! anything scans or watches the tree.
//!
//! ## Why this is not a trait
//!
//! There is no Linux counterpart. A trait with a no-op implementation would
//! imply the concept exists on both platforms and merely does nothing on one,
//! which is not true — the honest shape is a macOS module and a documented
//! no-op module elsewhere, and callers that read as "grant access if this
//! platform has such a thing".
//!
//! ## What the no-op costs
//!
//! Nothing, and it is not a stub for later work. On Linux a path *is* access,
//! so [`bookmark`] answering `None` and [`Access::grant`] answering an inert
//! guard are the correct answers rather than placeholder ones.

#[cfg(target_os = "macos")]
mod imp {
    use std::path::Path;

    use objc2::rc::Retained;
    use objc2_foundation::{
        NSData, NSString, NSURL, NSURLBookmarkCreationOptions, NSURLBookmarkResolutionOptions,
    };

    /// Make a security-scoped bookmark for `path`, or `None` if the system
    /// will not issue one.
    ///
    /// `None` is not a failure worth reporting to the user. Creating a
    /// security-scoped bookmark needs the `files.bookmarks.app-scope`
    /// entitlement, so an unsandboxed build — the DMG, and every `cargo test`
    /// — answers `None` here and loses nothing by it, because outside the
    /// sandbox the path already grants what the bookmark would.
    pub fn bookmark(path: &Path) -> Option<Vec<u8>> {
        let path = path.to_str()?;
        // A Rust string may carry an interior NUL; a file URL may not, and
        // `+[NSURL fileURLWithPath:]` answers nil for one, which objc2 turns
        // into a panic rather than a `None`. Refusing here keeps a malformed
        // path a bookmark this platform declines to make, which is what it is.
        if path.contains('\0') {
            return None;
        }
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        // No resource keys, no base URL. The call reports failure through the
        // `Result` rather than a null.
        let data = url
            .bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
                NSURLBookmarkCreationOptions::WithSecurityScope,
                None,
                None,
            )
            .ok()?;
        Some(data.to_vec())
    }

    /// A live grant of access to one folder.
    ///
    /// Held for as long as anything may touch the tree, and released on drop.
    /// The pairing is the whole contract: `startAccessingSecurityScopedResource`
    /// is refcounted by the OS, and a start without its stop leaks a kernel
    /// resource for the life of the process.
    pub struct Access {
        url: Retained<NSURL>,
    }

    impl Access {
        /// Resolve `data` and start accessing what it names.
        ///
        /// `stale` is the system telling you the bookmark still resolves but
        /// the file moved or was replaced, and that it should be re-made from
        /// the resolved URL. It is reported rather than acted on here, because
        /// re-making it means writing to the library database and this module
        /// does not own that.
        pub fn grant(data: &[u8]) -> Option<(Access, bool)> {
            let data = NSData::with_bytes(data);
            let mut stale = objc2::runtime::Bool::NO;
            // SAFETY: live NSData, no base URL, and `stale` is a valid pointer
            // to a `bool` this frame owns for the length of the call.
            let url = unsafe {
                NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
                    &data,
                    NSURLBookmarkResolutionOptions::WithSecurityScope,
                    None,
                    &mut stale,
                )
            }
            .ok()?;
            // SAFETY: `url` is the live URL just resolved.
            if !unsafe { url.startAccessingSecurityScopedResource() } {
                return None;
            }
            Some((Access { url }, stale.as_bool()))
        }

        /// The path the bookmark resolved to, which is not necessarily the one
        /// it was made from — a folder the user moved resolves to where it is
        /// now, and that is the point of storing a bookmark rather than a path.
        pub fn path(&self) -> Option<std::path::PathBuf> {
            self.url
                .path()
                .map(|p| std::path::PathBuf::from(p.to_string()))
        }
    }

    impl Drop for Access {
        fn drop(&mut self) {
            // SAFETY: balances the `start` in `grant`, which is the only way
            // an `Access` is constructed.
            unsafe { self.url.stopAccessingSecurityScopedResource() };
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::path::{Path, PathBuf};

    /// No bookmark, because there is nothing here that a path does not already
    /// grant. Not a stub: `None` is the correct answer on this platform.
    pub fn bookmark(_path: &Path) -> Option<Vec<u8>> {
        None
    }

    /// An inert grant. Constructed only from bookmark bytes, which this
    /// platform never produces, so in practice it is never constructed at all.
    pub struct Access;

    impl Access {
        pub fn grant(_data: &[u8]) -> Option<(Access, bool)> {
            None
        }

        pub fn path(&self) -> Option<PathBuf> {
            None
        }
    }
}

pub use imp::{Access, bookmark};

use std::path::{Path, PathBuf};

/// Whether this process runs under the macOS App Sandbox, which sets
/// `APP_SANDBOX_CONTAINER_ID` for every sandboxed process.
///
/// Lets the grant paths skip work that can only come to nothing: outside the
/// sandbox no bookmark is issued and every file the user can see already
/// opens, so the DMG build pays nothing per added song.
pub fn is_app_sandboxed() -> bool {
    std::env::var_os("APP_SANDBOX_CONTAINER_ID").is_some()
}

/// The folder to ask the user to grant, when some of `paths` are files this
/// process is not allowed to open. `None` when every one opens, or is simply
/// missing.
///
/// Answered by opening each file, because only that says what the sandbox
/// will allow right now. A refusal is `EPERM`; a deleted file is `ENOENT`, and
/// asking the user for permission to a file that is gone would be no help.
pub fn folder_needing_access(paths: &[&Path]) -> Option<PathBuf> {
    if !is_app_sandboxed() {
        return None;
    }
    let denied: Vec<&Path> = paths.iter().copied().filter(|p| is_denied(p)).collect();
    folder_to_ask_for(&denied, music_dir().as_deref())
}

fn is_denied(path: &Path) -> bool {
    match std::fs::File::open(path) {
        Ok(_) => false,
        Err(e) => e.raw_os_error() == Some(libc::EPERM),
    }
}

/// The one folder that holds every denied file, widened to the user's Music
/// folder when it is inside it. Asking for `~/Music/Albums/Blue` would grant
/// one album, and the next playlist would ask again; Music grants them all.
fn folder_to_ask_for(denied: &[&Path], music: Option<&Path>) -> Option<PathBuf> {
    let mut parents = denied.iter().filter_map(|p| p.parent());
    let mut common = parents.next()?.to_path_buf();
    for parent in parents {
        while !parent.starts_with(&common) {
            if !common.pop() {
                return None;
            }
        }
    }
    match music {
        Some(music) if common.starts_with(music) => Some(music.to_path_buf()),
        _ => Some(common),
    }
}

/// The user's real Music folder.
///
/// Not `$HOME/Music`: under the App Sandbox `$HOME` is the app's container,
/// and its `Music` is a link the sandbox will not follow without the
/// entitlement Sparkamp no longer has. The password database still names the
/// real home.
fn music_dir() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: getpwuid returns null or a pointer to a static record, read
    // here before anything else on this thread could call it again.
    let home = unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr((*pw).pw_dir)
    };
    Some(Path::new(std::ffi::OsStr::from_bytes(home.to_bytes())).join("Music"))
}

/// Every folder access the process is holding, kept alive for its lifetime.
///
/// A grant lives as long as anything might read the tree, and in this app that
/// is "until quit": scans, the watch threads and playback all reach library
/// paths, at times no single call site brackets. Holding them centrally is
/// what makes the start/stop pairing something the type system can enforce —
/// `Access` releases on drop, and this vector is the only owner.
static GRANTS: std::sync::Mutex<Vec<Access>> = std::sync::Mutex::new(Vec::new());

/// Resolve a stored bookmark and hold the access it grants.
///
/// Returns the path it resolved to and whether the bookmark is stale. A stale
/// bookmark still works — the caller should re-make it from the returned path
/// so the next launch does not depend on the system's willingness to resolve
/// an out-of-date one twice.
pub fn hold(data: &[u8]) -> Option<(std::path::PathBuf, bool)> {
    let (access, stale) = Access::grant(data)?;
    let path = access.path();
    GRANTS.lock().ok()?.push(access);
    path.map(|p| (p, stale))
}

/// How many folder grants the process is holding. Diagnostics, and the one
/// thing a test can observe without a sandbox.
// Nothing in the app reads this; the tests below are its only callers.
pub fn held() -> usize {
    GRANTS.lock().map(|g| g.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path a file URL cannot express is a bookmark this platform declines
    /// to make, not a crash.
    ///
    /// `+[NSURL fileURLWithPath:]` answers nil for a string with an interior
    /// NUL, and objc2 turns that nil into a panic. The library's own
    /// `add_folder_path_with_nul_byte_is_handled` found this the moment
    /// bookmarks were wired in.
    #[test]
    fn a_path_with_a_nul_byte_makes_no_bookmark() {
        assert!(bookmark(std::path::Path::new("/tmp/we\0ird")).is_none());
    }

    #[test]
    fn the_folder_asked_for_holds_every_denied_file() {
        let a = Path::new("/Users/me/Downloads/Mix/01.flac");
        let b = Path::new("/Users/me/Downloads/Mix/Disc 2/01.flac");
        assert_eq!(
            folder_to_ask_for(&[a, b], None),
            Some(PathBuf::from("/Users/me/Downloads/Mix"))
        );
        assert_eq!(
            folder_to_ask_for(&[a], None),
            Some(PathBuf::from("/Users/me/Downloads/Mix"))
        );
        assert_eq!(folder_to_ask_for(&[], None), None);
    }

    #[test]
    fn songs_inside_music_ask_for_music_itself() {
        let music = Path::new("/Users/me/Music");
        let song = Path::new("/Users/me/Music/Albums/Blue/01.flac");
        assert_eq!(
            folder_to_ask_for(&[song], Some(music)),
            Some(music.to_path_buf())
        );
        // Outside Music, the folder stays where the songs are.
        let elsewhere = Path::new("/Users/me/Desktop/Mix/01.flac");
        assert_eq!(
            folder_to_ask_for(&[elsewhere], Some(music)),
            Some(PathBuf::from("/Users/me/Desktop/Mix"))
        );
    }

    #[test]
    fn files_that_open_or_are_missing_need_no_access() {
        let dir = std::env::temp_dir().join(format!("sparkamp-acc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let readable = dir.join("a.mp3");
        std::fs::write(&readable, b"x").unwrap();
        let missing = dir.join("gone.mp3");
        let found = folder_needing_access(&[&readable, &missing]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found, None);
    }

    #[test]
    fn the_real_music_folder_is_found() {
        let music = music_dir().expect("the password database names a home");
        assert!(music.ends_with("Music"));
        assert!(music.is_absolute());
    }

    /// Nothing to resolve is nothing held. Bookmark bytes are opaque and
    /// system-issued, so garbage must be refused rather than trusted.
    #[test]
    fn garbage_bookmark_data_grants_nothing() {
        let before = held();
        assert!(hold(b"not a bookmark").is_none());
        assert!(hold(&[]).is_none());
        assert_eq!(held(), before, "a refused bookmark must hold no grant");
    }

    /// Outside a sandbox a bookmark is either unavailable or resolvable, and
    /// both are correct — what must never happen is a bookmark that is issued
    /// and then does not resolve, because that is a folder the next launch
    /// cannot open.
    #[test]
    fn a_bookmark_this_platform_issues_can_be_resolved() {
        let dir = std::env::temp_dir().join(format!("sparkamp-bm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create dir");
        let made = bookmark(&dir);
        let result = made.as_ref().map(|b| {
            let before = held();
            let got = hold(b);
            (got, before)
        });
        let _ = std::fs::remove_dir_all(&dir);
        match result {
            None => println!("this platform issues no bookmarks; nothing to resolve"),
            Some((Some((resolved, _stale)), before)) => {
                assert_eq!(
                    std::fs::canonicalize(&resolved).ok(),
                    std::fs::canonicalize(&dir).ok(),
                    "a bookmark must resolve to the folder it was made from"
                );
                assert_eq!(held(), before + 1, "a resolved bookmark holds one grant");
            }
            Some((None, _)) => {
                panic!(
                    "a bookmark this platform issued must resolve, or the next launch loses the folder"
                )
            }
        }
    }
}
