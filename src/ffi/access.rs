//! Sandbox access to files and folders outside the library, for the frontend.
//!
//! The playlist adds remember their own grants (see `playlist.rs`). These
//! calls are for the grants the user gives on purpose (the Allow Access
//! prompt, the rip folder) and for asking whether a prompt is needed at all.
//! Storage and lifetimes: `media_library/grants.rs`.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};

use super::SparkampCtx;

/// Keep access to `path` for good: a folder from the Allow Access prompt, or
/// the rip folder.
///
/// Call straight after the open panel returns, while its grant is live. A
/// bookmark taken later captures nothing. Returns whether a grant was stored,
/// which is false outside a sandbox and when a library folder or an earlier
/// kept grant already covers the path. Neither is a failure.
///
/// # Safety
///
/// `ctx` must be null or a live context, and `path` null or a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_grant_access(ctx: *mut SparkampCtx, path: *const c_char) -> bool {
    if ctx.is_null() || path.is_null() {
        return false;
    }
    // SAFETY: both checked non-null; the caller vouches for the rest.
    let (ctx, path) = unsafe { (&*ctx, CStr::from_ptr(path)) };
    let Ok(path) = path.to_str() else {
        return false;
    };
    let path = Path::new(path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(path));
    super::remember_access(ctx, &path, true)
}

/// The folder to ask the user to grant before `paths` can be played, or null
/// when every one of them opens (or is simply missing). Songs inside the
/// user's Music folder ask for Music itself, so one grant covers the next
/// playlist too.
///
/// Free the result with `sparkamp_free_string`.
///
/// # Safety
///
/// `paths` must be null or point to `count` pointers, each null or a
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_folder_needing_access(
    paths: *const *const c_char,
    count: c_int,
) -> *mut c_char {
    if paths.is_null() || count <= 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller vouches for `count` readable entries.
    let raw = unsafe { std::slice::from_raw_parts(paths, count as usize) };
    let owned: Vec<PathBuf> = raw
        .iter()
        .filter(|p| !p.is_null())
        // SAFETY: non-null, and NUL-terminated per the contract above.
        .filter_map(|&p| {
            unsafe { CStr::from_ptr(p) }
                .to_str()
                .ok()
                .map(PathBuf::from)
        })
        .collect();
    let refs: Vec<&Path> = owned.iter().map(PathBuf::as_path).collect();
    crate::sandbox::folder_needing_access(&refs)
        .and_then(|folder| CString::new(folder.to_string_lossy().into_owned()).ok())
        .map_or(std::ptr::null_mut(), CString::into_raw)
}

/// Whether the folder at `path` exists and can be read right now.
///
/// Asked by an actual read, because a remembered path says nothing about
/// whether the sandbox still allows it. The rip window uses this to tell a
/// rip folder that still works from one that needs choosing again.
///
/// # Safety
///
/// `path` must be null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_folder_is_readable(path: *const c_char) -> bool {
    if path.is_null() {
        return false;
    }
    // SAFETY: checked non-null; NUL-terminated per the contract above.
    let Ok(path) = unsafe { CStr::from_ptr(path) }.to_str() else {
        return false;
    };
    !path.is_empty() && std::fs::read_dir(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_folders_read_and_missing_ones_do_not() {
        let dir = std::env::temp_dir();
        let here = CString::new(dir.to_string_lossy().into_owned()).unwrap();
        let gone = CString::new("/nonexistent/sparkamp-rips").unwrap();
        let empty = CString::new("").unwrap();
        unsafe {
            assert!(sparkamp_folder_is_readable(here.as_ptr()));
            assert!(!sparkamp_folder_is_readable(gone.as_ptr()));
            assert!(!sparkamp_folder_is_readable(empty.as_ptr()));
            assert!(!sparkamp_folder_is_readable(std::ptr::null()));
        }
    }

    #[test]
    fn songs_that_open_need_no_folder() {
        let file =
            std::env::temp_dir().join(format!("sparkamp-ffi-acc-{}.mp3", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        let path = CString::new(file.to_string_lossy().into_owned()).unwrap();
        let list = [path.as_ptr()];
        let found = unsafe { sparkamp_folder_needing_access(list.as_ptr(), 1) };
        let _ = std::fs::remove_file(&file);
        assert!(found.is_null());
        assert!(unsafe { sparkamp_folder_needing_access(std::ptr::null(), 3) }.is_null());
    }
}
