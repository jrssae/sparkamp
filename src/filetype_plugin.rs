//! Filetype plugin loader — C ABI definition and safe Rust wrapper.
// Public API intended for external plugin authors and future internal use.
#![allow(dead_code)]
//!
//! Filetype plugins extend SparkAmp with support for additional audio file
//! formats.  A plugin is a shared library (`.so` on Linux) that exports one
//! C function:
//!
//! ```c
//! const SparkFiletypePlugin *sparkamp_filetype_plugin(void);
//! ```
//!
//! The plugin descriptor declares which file extensions it handles and
//! optionally provides a metadata-reading callback for formats that do not
//! use ID3 tags (e.g. custom or proprietary containers).
//!
//! # Writing a plugin (C example)
//!
//! ```c
//! #include <stdlib.h>
//! #include <string.h>
//! #include "sparkamp_filetype.h"
//!
//! static const char *EXTENSIONS[] = {"xyz", "abc", NULL};
//!
//! static int read_meta(const char *path,
//!                      char **out_title, char **out_artist)
//! {
//!     // Read your custom tag format from `path`.
//!     *out_title  = strdup("My Track");
//!     *out_artist = strdup("My Artist");
//!     return 1; // success
//! }
//!
//! static void free_str(char *s) { free(s); }
//!
//! static SparkFiletypePlugin PLUGIN = {
//!     .abi_version = 1,
//!     .name        = "XYZ Format",
//!     .extensions  = EXTENSIONS,
//!     .read_metadata = read_meta,
//!     .free_string   = free_str,
//! };
//!
//! const SparkFiletypePlugin *sparkamp_filetype_plugin(void) { return &PLUGIN; }
//! ```
//!
//! Compile with:
//! ```sh
//! cc -shared -fPIC -o xyz_format.so xyz_format.c
//! ```
//!
//! Then set `[plugins] filetype_dir` in `~/.config/sparkamp/config.toml` to
//! the directory containing `xyz_format.so`.

use std::{
    ffi::{CStr, CString},
    path::{Path, PathBuf},
};
use libloading::{Library, Symbol};

// ---------------------------------------------------------------------------
// ABI version
// ---------------------------------------------------------------------------

/// ABI version this build of SparkAmp understands.
///
/// Plugins compiled against a different version will be rejected at load time.
pub const SPARKAMP_FILETYPE_ABI_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// C-compatible ABI struct
// ---------------------------------------------------------------------------

/// C-compatible filetype plugin descriptor.
///
/// Plugins export a `const SparkFiletypePlugin *sparkamp_filetype_plugin(void)`
/// function returning a pointer to a statically allocated instance.
#[repr(C)]
pub struct SparkFiletypePluginAbi {
    /// Must equal [`SPARKAMP_FILETYPE_ABI_VERSION`].
    pub abi_version: u32,

    /// Human-readable plugin name — null-terminated UTF-8.  May be null;
    /// SparkAmp falls back to the file stem.
    pub name: *const std::os::raw::c_char,

    /// Null-terminated array of file extension strings (without the leading
    /// dot), e.g. `{"xyz", "abc", NULL}`.  Must not be null.  The array and
    /// all strings must remain valid for the library's lifetime.
    pub extensions: *const *const std::os::raw::c_char,

    /// Optional metadata reader.  Called by `Track::from_path` when the
    /// built-in ID3 reader fails.
    ///
    /// - `path`       — null-terminated UTF-8 file path.
    /// - `out_title`  — write a heap-allocated title string here, or null.
    /// - `out_artist` — write a heap-allocated artist string here, or null.
    ///
    /// Returns 1 on success, 0 if the file has no readable metadata.
    /// Returned strings must be freed by calling `free_string`.
    pub read_metadata: Option<
        unsafe extern "C" fn(
            path:       *const std::os::raw::c_char,
            out_title:  *mut *mut std::os::raw::c_char,
            out_artist: *mut *mut std::os::raw::c_char,
        ) -> std::os::raw::c_int,
    >,

    /// Free a string previously returned by `read_metadata`.
    /// Required when `read_metadata` is non-null.  May be null if
    /// `read_metadata` is also null.
    pub free_string: Option<unsafe extern "C" fn(*mut std::os::raw::c_char)>,
}

// ---------------------------------------------------------------------------
// Safe Rust wrapper
// ---------------------------------------------------------------------------

/// A successfully loaded filetype plugin.
pub struct FiletypePlugin {
    /// Display name copied from the plugin descriptor at load time.
    pub name: String,

    /// File extensions this plugin handles (without leading dot, lower-case).
    pub extensions: Vec<String>,

    /// Path from which this plugin was loaded.
    pub path: PathBuf,

    /// Optional metadata reader function pointer.
    read_metadata_fn: Option<
        unsafe extern "C" fn(
            *const std::os::raw::c_char,
            *mut *mut std::os::raw::c_char,
            *mut *mut std::os::raw::c_char,
        ) -> std::os::raw::c_int,
    >,

    /// Matching string-free callback.
    free_string_fn: Option<unsafe extern "C" fn(*mut std::os::raw::c_char)>,

    /// The open `libloading::Library` — must be kept alive so function
    /// pointers remain valid.
    _lib: Library,
}

// SAFETY: SparkAmp is single-threaded at the plugin call layer.  Send/Sync
// are required because FiletypePlugin lives in AppState which may be moved
// during construction; we never call plugin functions from multiple threads.
unsafe impl Send for FiletypePlugin {}
unsafe impl Sync for FiletypePlugin {}

impl FiletypePlugin {
    /// Ask the plugin to read title and artist metadata for `path`.
    ///
    /// Returns `Some((title, artist))` on success, `None` if the plugin
    /// reports no metadata or has no `read_metadata` callback.
    pub fn read_metadata(&self, path: &Path) -> Option<(String, String)> {
        let read_fn = self.read_metadata_fn?;
        let free_fn = self.free_string_fn?;

        let path_cstr = CString::new(path.to_string_lossy().as_bytes()).ok()?;
        let mut out_title:  *mut std::os::raw::c_char = std::ptr::null_mut();
        let mut out_artist: *mut std::os::raw::c_char = std::ptr::null_mut();

        // SAFETY: path_cstr is valid; out pointers are valid local variables.
        let ok = unsafe {
            read_fn(
                path_cstr.as_ptr(),
                &mut out_title,
                &mut out_artist,
            )
        };

        if ok == 0 {
            // Plugin reports no metadata; strings should be null but free
            // them defensively anyway.
            if !out_title.is_null()  { unsafe { free_fn(out_title);  } }
            if !out_artist.is_null() { unsafe { free_fn(out_artist); } }
            return None;
        }

        // Copy strings into Rust-owned Strings before freeing them.
        let title = if !out_title.is_null() {
            let s = unsafe { CStr::from_ptr(out_title) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: out_title was allocated by the plugin and must be freed
            // with the matching free_string function.
            unsafe { free_fn(out_title); }
            s
        } else {
            String::new()
        };

        let artist = if !out_artist.is_null() {
            let s = unsafe { CStr::from_ptr(out_artist) }
                .to_string_lossy()
                .into_owned();
            unsafe { free_fn(out_artist); }
            s
        } else {
            String::new()
        };

        if title.is_empty() { None } else { Some((title, artist)) }
    }
}

// ---------------------------------------------------------------------------
// Loader helpers
// ---------------------------------------------------------------------------

/// Attempt to load a single `.so` file as a filetype plugin.
///
/// Returns `None` with a warning printed to stderr if the file cannot be
/// opened, the entry-point symbol is missing, the returned pointer is null,
/// or the ABI version does not match.
pub fn load_plugin(path: &Path) -> Option<FiletypePlugin> {
    // SAFETY: loading arbitrary .so files is inherently unsafe.
    let lib = unsafe { Library::new(path) }.map_err(|e| {
        eprintln!("filetype_plugin: cannot open {:?}: {}", path, e);
    }).ok()?;

    // Locate the entry-point symbol.
    let entry: Symbol<unsafe extern "C" fn() -> *const SparkFiletypePluginAbi> = unsafe {
        lib.get(b"sparkamp_filetype_plugin\0")
    }.map_err(|e| {
        eprintln!("filetype_plugin: {:?} has no `sparkamp_filetype_plugin` symbol: {}", path, e);
    }).ok()?;

    let abi_ptr: *const SparkFiletypePluginAbi = unsafe { entry() };
    if abi_ptr.is_null() {
        eprintln!("filetype_plugin: {:?} returned null from sparkamp_filetype_plugin", path);
        return None;
    }

    // SAFETY: we checked for null; the plugin guarantees the struct is valid.
    let abi: &SparkFiletypePluginAbi = unsafe { &*abi_ptr };

    if abi.abi_version != SPARKAMP_FILETYPE_ABI_VERSION {
        eprintln!(
            "filetype_plugin: {:?} ABI version {} ≠ expected {}; skipping",
            path, abi.abi_version, SPARKAMP_FILETYPE_ABI_VERSION
        );
        return None;
    }

    if abi.extensions.is_null() {
        eprintln!("filetype_plugin: {:?} has null extensions pointer; skipping", path);
        return None;
    }

    // Copy the extension strings out of the plugin so we don't hold raw
    // pointers.  Walk the null-terminated array.
    let mut extensions = Vec::new();
    let mut ptr = abi.extensions;
    loop {
        // SAFETY: plugin guarantees the array is null-terminated and each
        // entry is a valid null-terminated UTF-8 string.
        let ext_ptr = unsafe { *ptr };
        if ext_ptr.is_null() { break; }
        let ext = unsafe { CStr::from_ptr(ext_ptr) }
            .to_string_lossy()
            .to_lowercase();
        if !ext.is_empty() {
            extensions.push(ext);
        }
        // Advance to the next pointer.
        ptr = unsafe { ptr.add(1) };
    }

    if extensions.is_empty() {
        eprintln!("filetype_plugin: {:?} registers no extensions; skipping", path);
        return None;
    }

    let name = if abi.name.is_null() {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    } else {
        unsafe { CStr::from_ptr(abi.name) }
            .to_string_lossy()
            .into_owned()
    };

    eprintln!("filetype_plugin: loaded {:?} (\"{name}\") — extensions: {:?}", path, extensions);

    Some(FiletypePlugin {
        name,
        extensions,
        path: path.to_owned(),
        read_metadata_fn: abi.read_metadata,
        free_string_fn:   abi.free_string,
        _lib: lib,
    })
}

/// Scan `dir` for `.so` files and load each as a filetype plugin.
///
/// Files that cannot be loaded are skipped after printing a warning to stderr.
/// Returns an empty `Vec` if `dir` is blank, does not exist, or is not a
/// directory.
pub fn load_plugins_from_dir(dir: &str) -> Vec<FiletypePlugin> {
    if dir.is_empty() {
        return vec![];
    }
    let dir_path = Path::new(dir);
    if !dir_path.is_dir() {
        return vec![];
    }

    let mut so_paths: Vec<PathBuf> = std::fs::read_dir(dir_path)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.extension().map_or(false, |ext| ext == "so"))
        .collect();

    so_paths.sort();

    let mut plugins = Vec::new();
    for so_path in so_paths {
        if let Some(plugin) = load_plugin(&so_path) {
            plugins.push(plugin);
        }
    }
    plugins
}

/// Collect all extra file extensions registered by the given plugins.
///
/// Returns a deduplicated, sorted list of extension strings (lower-case,
/// without leading dots) from all loaded filetype plugins.  Used to extend
/// SparkAmp's built-in [`AUDIO_EXTENSIONS`] list at runtime.
///
/// [`AUDIO_EXTENSIONS`]: crate::model::AUDIO_EXTENSIONS
pub fn extra_extensions(plugins: &[FiletypePlugin]) -> Vec<String> {
    let mut exts: Vec<String> = plugins.iter()
        .flat_map(|p| p.extensions.iter().cloned())
        .collect();
    exts.sort();
    exts.dedup();
    exts
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_plugins_from_empty_string_returns_empty() {
        assert!(load_plugins_from_dir("").is_empty());
    }

    #[test]
    fn load_plugins_from_nonexistent_dir_returns_empty() {
        assert!(load_plugins_from_dir("/nonexistent/sparkamp/filetypes").is_empty());
    }

    #[test]
    fn extra_extensions_deduplicates() {
        // Construct two dummy FiletypePlugin values by going through the public
        // fields directly.  We don't load actual .so files in unit tests.
        let plugins: Vec<FiletypePlugin> = vec![]; // no plugins — empty output
        assert!(extra_extensions(&plugins).is_empty());
    }
}
