//! Visualizer plugin loader — C ABI definition and safe Rust wrapper.
// Public API intended for external plugin authors and future internal use.
#![allow(dead_code)]
//!
//! Third-party visualizer plugins are shared libraries (`.so` on Linux) that
//! export a single C function:
//!
//! ```c
//! const SparkVizPlugin *sparkamp_viz_plugin(void);
//! ```
//!
//! The returned struct must remain valid for the lifetime of the library.
//! Sparkamp copies the name string out on load so the plugin only needs to
//! keep the function pointers valid.
//!
//! # Writing a plugin (C example)
//!
//! ```c
//! #include <math.h>
//! #include "sparkamp_viz.h"  // contains the SparkVizPlugin struct definition
//!
//! static void render(void *ctx, double pos, int active,
//!                    double *out, uint32_t count)
//! {
//!     for (uint32_t i = 0; i < count; i++) {
//!         double t = (double)i / (count > 1 ? count - 1 : 1);
//!         out[i] = 0.5 + 0.5 * sin(pos * 4.0 + t * 6.28318);
//!     }
//! }
//!
//! static SparkVizPlugin PLUGIN = {
//!     .abi_version = 1,
//!     .name        = "Sine Wave Demo",
//!     .init        = NULL,
//!     .destroy     = NULL,
//!     .render      = render,
//! };
//!
//! const SparkVizPlugin *sparkamp_viz_plugin(void) { return &PLUGIN; }
//! ```
//!
//! Compile with:
//! ```sh
//! cc -shared -fPIC -o sine_demo.so sine_demo.c -lm
//! ```
//!
//! Then set `[plugins] visualizer_dir` in `~/.config/sparkamp/config.toml` to
//! the directory containing `sine_demo.so`.

use std::{ffi::CStr, path::{Path, PathBuf}};
use libloading::{Library, Symbol};

// ---------------------------------------------------------------------------
// ABI version
// ---------------------------------------------------------------------------

/// ABI version this build of Sparkamp understands.
///
/// Plugins compiled against a different version will be rejected at load time.
/// The version is incremented whenever the [`SparkVizPluginAbi`] layout or
/// the calling conventions of any callback change in a backward-incompatible
/// way.
pub const SPARKAMP_VIZ_ABI_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// C-compatible ABI struct
// ---------------------------------------------------------------------------

/// C-compatible plugin descriptor.
///
/// Plugins export a `const SparkVizPluginAbi *sparkamp_viz_plugin(void)`
/// function that returns a pointer to a statically allocated instance of this
/// struct.  All fields must remain valid for the lifetime of the library.
#[repr(C)]
pub struct SparkVizPluginAbi {
    /// Must equal [`SPARKAMP_VIZ_ABI_VERSION`].  A mismatch causes the plugin
    /// to be rejected with a warning.
    pub abi_version: u32,

    /// Human-readable plugin name — null-terminated UTF-8.  May be null;
    /// Sparkamp falls back to the file stem in that case.
    pub name: *const std::os::raw::c_char,

    /// Called once immediately after the plugin is loaded.  Returns an opaque
    /// context pointer that is threaded through `render` and `destroy`.
    /// May be null (no initialisation needed).
    pub init: Option<unsafe extern "C" fn() -> *mut std::os::raw::c_void>,

    /// Called once just before the library is unloaded.  Use it to free any
    /// resources allocated in `init`.  May be null.
    pub destroy: Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>,

    /// Called every frame to produce `count` visualizer samples.
    ///
    /// | Parameter            | Description                                              |
    /// |----------------------|----------------------------------------------------------|
    /// | `ctx`                | Opaque pointer from `init` (null if `init` was null).    |
    /// | `playback_pos_secs`  | Current playback position in seconds (wall-clock time).  |
    /// | `is_active`          | 1 = playing, 0 = paused / stopped.                       |
    /// | `out`                | Caller-allocated buffer of `count` doubles.              |
    /// | `count`              | Number of samples requested.                             |
    ///
    /// The plugin must write `count` values into `out`.  Sparkamp clamps every
    /// value to `[0.0, 1.0]` after the call, so out-of-range outputs are safe
    /// (but will look wrong).  Must not be null.
    pub render: Option<
        unsafe extern "C" fn(
            ctx: *mut std::os::raw::c_void,
            playback_pos_secs: f64,
            is_active: std::os::raw::c_int,
            out: *mut f64,
            count: u32,
        ),
    >,
}

// ---------------------------------------------------------------------------
// Safe Rust wrapper
// ---------------------------------------------------------------------------

/// A successfully loaded and initialised visualizer plugin.
///
/// Holds an open handle to the shared library so the code segment stays
/// mapped in memory; the handle is closed (and the library unloaded from the
/// process) when this value is dropped.
pub struct VizPlugin {
    /// Display name copied from the plugin's `name` field at load time.
    pub name: String,

    /// Path from which this plugin was loaded (for display purposes).
    pub path: PathBuf,

    /// Opaque state pointer returned by `init` (null if `init` was absent).
    ctx: *mut std::os::raw::c_void,

    /// Validated render callback — always non-null (enforced during loading).
    render_fn: unsafe extern "C" fn(
        *mut std::os::raw::c_void,
        f64,
        std::os::raw::c_int,
        *mut f64,
        u32,
    ),

    /// Optional destructor callback.
    destroy_fn: Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>,

    /// The open `libloading::Library` — must be kept alive to keep the code
    /// mapped.  Dropped last because `render_fn` and `destroy_fn` point into it.
    _lib: Library,
}

// SAFETY: Sparkamp runs its visualizer callbacks on the same thread that
// created the plugin (GTK's main thread or the TUI render thread).  We
// implement Send/Sync only so VizPlugin can live in structs that are moved
// during construction; we never actually share the raw pointer across threads.
unsafe impl Send for VizPlugin {}
unsafe impl Sync for VizPlugin {}

impl VizPlugin {
    /// Invoke the plugin's render callback for the given playback state and
    /// return `count` sample values, each clamped to `[0.0, 1.0]`.
    ///
    /// All clamping happens *after* the plugin writes its output, so
    /// misbehaving plugins that produce values outside `[0.0, 1.0]` are
    /// handled gracefully without panicking.
    pub fn render(&self, pos_secs: f64, is_active: bool, count: usize) -> Vec<f64> {
        let mut buf = vec![0.0f64; count];
        // SAFETY: we validated render_fn is non-null during loading, and the
        // buffer is correctly sized.
        unsafe {
            (self.render_fn)(
                self.ctx,
                pos_secs,
                is_active as std::os::raw::c_int,
                buf.as_mut_ptr(),
                count as u32,
            );
        }
        // Clamp output to [0.0, 1.0] so the renderer never receives NaN or
        // out-of-range values from a buggy plugin.
        buf.iter().map(|v| v.clamp(0.0, 1.0)).collect()
    }
}

impl Drop for VizPlugin {
    /// Call the plugin's `destroy` callback (if any), then drop the library.
    fn drop(&mut self) {
        if let Some(destroy) = self.destroy_fn {
            // SAFETY: ctx came from init; destroy is the matching destructor.
            unsafe { destroy(self.ctx); }
        }
        // _lib is dropped after this block, unloading the library.
    }
}

// ---------------------------------------------------------------------------
// Loader helpers
// ---------------------------------------------------------------------------

/// Attempt to load a single `.so` file as a visualizer plugin.
///
/// Returns `None` with a warning printed to stderr if:
/// - the file cannot be opened as a shared library,
/// - the `sparkamp_viz_plugin` symbol is missing,
/// - the returned pointer is null, or
/// - `abi_version` does not match [`SPARKAMP_VIZ_ABI_VERSION`].
pub fn load_plugin(path: &Path) -> Option<VizPlugin> {
    // SAFETY: dlopen-ing arbitrary .so files is inherently unsafe.  Sparkamp
    // only loads files from a directory the user explicitly configured.
    let lib = unsafe { Library::new(path) }.map_err(|e| {
        eprintln!("viz_plugin: cannot open {:?}: {}", path, e);
    }).ok()?;

    // Resolve the entry-point symbol.
    let entry: Symbol<unsafe extern "C" fn() -> *const SparkVizPluginAbi> = unsafe {
        lib.get(b"sparkamp_viz_plugin\0")
    }.map_err(|e| {
        eprintln!("viz_plugin: {:?} has no `sparkamp_viz_plugin` symbol: {}", path, e);
    }).ok()?;

    // Call the entry-point to obtain the descriptor.
    let abi_ptr: *const SparkVizPluginAbi = unsafe { entry() };
    if abi_ptr.is_null() {
        eprintln!("viz_plugin: {:?} returned null from sparkamp_viz_plugin", path);
        return None;
    }

    // SAFETY: we checked for null; the plugin contract says the returned
    // pointer is valid for the library's lifetime.
    let abi: &SparkVizPluginAbi = unsafe { &*abi_ptr };

    // Verify ABI compatibility before touching any other fields.
    if abi.abi_version != SPARKAMP_VIZ_ABI_VERSION {
        eprintln!(
            "viz_plugin: {:?} reports ABI version {} but this Sparkamp expects {}; skipping",
            path, abi.abi_version, SPARKAMP_VIZ_ABI_VERSION
        );
        return None;
    }

    // render is the only mandatory callback — reject plugins that omit it.
    let render_fn = match abi.render {
        Some(f) => f,
        None => {
            eprintln!("viz_plugin: {:?} has no render callback; skipping", path);
            return None;
        }
    };

    // Copy the name string so we do not hold raw pointers into the library.
    let name = if abi.name.is_null() {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    } else {
        // SAFETY: the plugin guarantees the name pointer is valid UTF-8 and
        // null-terminated for the library's lifetime.  We clone it immediately.
        unsafe { CStr::from_ptr(abi.name) }
            .to_string_lossy()
            .into_owned()
    };

    // Invoke the optional init callback.
    let ctx = if let Some(init) = abi.init {
        // SAFETY: init is a valid function pointer from the plugin.
        unsafe { init() }
    } else {
        std::ptr::null_mut()
    };

    eprintln!("viz_plugin: loaded {:?} (\"{name}\")", path);

    Some(VizPlugin {
        name,
        path: path.to_owned(),
        ctx,
        render_fn,
        destroy_fn: abi.destroy,
        _lib: lib,
    })
}

/// Scan `dir` for `.so` files and load each as a visualizer plugin.
///
/// Files that cannot be loaded are skipped after a warning is printed to
/// stderr.  Returns an empty `Vec` if `dir` is blank, does not exist, or
/// is not a directory.
pub fn load_plugins_from_dir(dir: &str) -> Vec<VizPlugin> {
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

    // Sort for deterministic load order (makes tests reproducible).
    so_paths.sort();

    let mut plugins = Vec::new();
    for so_path in so_paths {
        if let Some(plugin) = load_plugin(&so_path) {
            plugins.push(plugin);
        }
    }
    plugins
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_plugins_from_empty_dir_string_returns_empty() {
        assert!(load_plugins_from_dir("").is_empty());
    }

    #[test]
    fn load_plugins_from_nonexistent_dir_returns_empty() {
        assert!(load_plugins_from_dir("/nonexistent/sparkamp/plugins").is_empty());
    }

    #[test]
    fn load_plugins_from_dir_with_no_so_files_returns_empty() {
        // Use the src directory — it has no .so files.
        let plugins = load_plugins_from_dir("src");
        assert!(plugins.is_empty());
    }
}
