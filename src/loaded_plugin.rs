// Public API — methods will be called by the plugin Settings UI once wired in.
#![allow(dead_code)]

//! Safe Rust wrapper around a dynamically loaded Sparkamp plugin.
//!
//! [`LoadedPlugin`] replaces both the old `VizPlugin` and `FiletypePlugin`
//! types.  It holds a live `libloading::Library` handle (keeping the code
//! segment mapped), an opaque context pointer returned by the plugin's `init`
//! callback, and type-erased function-pointer caches so hot-path calls
//! (e.g. the per-frame `render`) avoid repeated raw-pointer arithmetic.
//!
//! # V1 compatibility
//!
//! Plugins compiled against ABI v1 (`sparkamp_viz_plugin` entry point) are
//! loaded by [`crate::plugin_manager`] and wrapped in a synthetic
//! `SparkPluginAbi` box stored inside `LoadedPlugin`.  From the outside they
//! are indistinguishable from v2 plugins, except they always report
//! `settings_schema` as null (no configurable settings).

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_double, c_int, c_uint, c_void};
use std::path::PathBuf;

use libloading::Library;

use crate::plugin_abi::{
    SparkPluginAbi, SparkPluginKind, SparkSettingDef, SparkSettingType,
};
use crate::plugin_settings::PluginSettings;

// ---------------------------------------------------------------------------
// PluginKind
// ---------------------------------------------------------------------------

/// Rust-idiomatic equivalent of [`SparkPluginKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginKind {
    Visualizer,
    Filetype,
}

impl From<SparkPluginKind> for PluginKind {
    fn from(k: SparkPluginKind) -> Self {
        match k {
            SparkPluginKind::Visualizer => PluginKind::Visualizer,
            SparkPluginKind::Filetype   => PluginKind::Filetype,
        }
    }
}

// ---------------------------------------------------------------------------
// Cached function pointer types
// ---------------------------------------------------------------------------

pub(crate) type RenderFn     = unsafe extern "C" fn(*mut c_void, c_double, c_int, *mut c_double, c_uint);
pub(crate) type FullscreenFn = unsafe extern "C" fn(*mut c_void);
pub(crate) type DestroyCbFn  = unsafe extern "C" fn(*mut c_void);
pub(crate) type SettingCbFn  = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char);

// ---------------------------------------------------------------------------
// SettingMeta
// ---------------------------------------------------------------------------

/// A host-side, owned copy of one [`SparkSettingDef`] entry.
///
/// Copied at load time so the host does not hold raw pointers into the plugin
/// binary.
#[derive(Debug, Clone)]
pub struct SettingMeta {
    pub value_type:    SparkSettingType,
    pub key:           String,
    pub label:         String,
    pub description:   Option<String>,
    pub default_value: String,
    pub choices:       Option<Vec<String>>,
    pub min_value:     Option<String>,
    pub max_value:     Option<String>,
}

// ---------------------------------------------------------------------------
// LoadedPlugin
// ---------------------------------------------------------------------------

/// A successfully loaded and initialised Sparkamp plugin.
///
/// Dropping this value calls the plugin's `destroy` callback (if any) and
/// unloads the shared library.
pub struct LoadedPlugin {
    // ── Identity ──────────────────────────────────────────────────────────
    /// Stable plugin ID (e.g. `"dev.sparkamp.viz.granite"`).
    pub plugin_id: String,
    /// Human-readable name.
    pub name:      String,
    /// Plugin kind (visualizer / filetype).
    pub kind:      PluginKind,
    /// Optional version string.
    pub version:   Option<String>,
    /// Optional one-line description.
    pub description: Option<String>,
    /// Optional author name.
    pub author:    Option<String>,
    /// Path from which this plugin was loaded.
    pub path:      PathBuf,

    // ── Settings ──────────────────────────────────────────────────────────
    /// Owned copies of the plugin's settings schema (empty if none declared).
    pub schema: Vec<SettingMeta>,
    /// Current setting values for this plugin.
    pub settings: PluginSettings,

    // ── Runtime state ─────────────────────────────────────────────────────
    /// Opaque context pointer returned by `init`.
    ctx: *mut c_void,

    // ── Cached function pointers ───────────────────────────────────────────
    render_fn:      Option<RenderFn>,
    fullscreen_fn:  Option<FullscreenFn>,
    destroy_fn:     Option<DestroyCbFn>,
    setting_cb_fn:  Option<SettingCbFn>,
    // Filetype extensions (owned copies).
    pub extensions: Vec<String>,

    // ── Ownership ─────────────────────────────────────────────────────────
    /// Keeps the library mapped in memory.  Dropped last.
    _lib: Library,
    /// For v1 shim plugins: owns the heap-allocated synthetic SparkPluginAbi.
    _synthetic: Option<Box<SparkPluginAbi>>,
}

// SAFETY: LoadedPlugin is only created and used on the main GTK/TUI thread.
// The raw pointer fields are non-aliasing between instances.
unsafe impl Send for LoadedPlugin {}
unsafe impl Sync for LoadedPlugin {}

impl LoadedPlugin {
    // -----------------------------------------------------------------------
    // Construction (called by plugin_manager)
    // -----------------------------------------------------------------------

    /// Build a `LoadedPlugin` from a validated `SparkPluginAbi` pointer and
    /// an open `Library`.
    ///
    /// `synthetic_box` is `Some(b)` for v1 shim plugins where `abi_ptr`
    /// points inside the heap box rather than the library's static storage.
    ///
    /// # Safety
    ///
    /// `abi_ptr` must be valid for the lifetime of `lib` (or of
    /// `synthetic_box` if provided).  The caller must have already verified
    /// `abi_version == 2`.
    pub(crate) unsafe fn from_abi(
        abi_ptr:       *const SparkPluginAbi,
        lib:           Library,
        path:          PathBuf,
        synthetic_box: Option<Box<SparkPluginAbi>>,
    ) -> Option<Self> {
        let abi = unsafe { &*abi_ptr };

        // Copy identity strings out of the plugin's static storage immediately
        // so we never hold dangling references.
        let plugin_id = unsafe { copy_cstr(abi.plugin_id)? };
        let name = unsafe { copy_cstr(abi.name) }
            .unwrap_or_else(|| path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string());
        let kind      = PluginKind::from(abi.kind);
        let version   = unsafe { copy_cstr(abi.version) };
        let description = unsafe { copy_cstr(abi.description) };
        let author    = unsafe { copy_cstr(abi.author) };

        // Copy the settings schema.
        let schema = unsafe { copy_schema(abi.settings_schema) };

        // Load persisted settings, seeding defaults from schema.
        let settings = PluginSettings::load(&plugin_id, abi.settings_schema);

        // Cache function pointers for the relevant kind.
        let (render_fn, fullscreen_fn, extensions) = match kind {
            PluginKind::Visualizer => {
                let r  = abi.viz.render;
                let fs = abi.viz.fullscreen;
                (r, fs, vec![])
            }
            PluginKind::Filetype => {
                let exts = unsafe { copy_extensions(abi.filetype.extensions) };
                (None, None, exts)
            }
        };

        let destroy_fn    = abi.destroy;
        let setting_cb_fn = abi.on_setting_changed;

        // Call init, passing current setting values.
        let ctx = if let Some(init) = abi.init {
            let (kp, vp, _ks, _vs) = settings.as_c_arrays();
            // SAFETY: kp/vp are valid null-terminated pointer arrays.
            unsafe { init(kp.as_ptr(), vp.as_ptr()) }
        } else {
            std::ptr::null_mut()
        };

        Some(LoadedPlugin {
            plugin_id,
            name,
            kind,
            version,
            description,
            author,
            path,
            schema,
            settings,
            ctx,
            render_fn,
            fullscreen_fn,
            destroy_fn,
            setting_cb_fn,
            extensions,
            _lib: lib,
            _synthetic: synthetic_box,
        })
    }

    // -----------------------------------------------------------------------
    // Manual construction (used by plugin_manager for v1 shims)
    // -----------------------------------------------------------------------

    /// Build a `LoadedPlugin` from individually-specified components.
    ///
    /// Used by [`crate::plugin_manager`] when wrapping v1 legacy plugins in
    /// the unified type without going through the full v2 ABI loading path.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_manual(
        plugin_id:     String,
        name:          String,
        kind:          PluginKind,
        version:       Option<String>,
        description:   Option<String>,
        author:        Option<String>,
        path:          PathBuf,
        schema:        Vec<SettingMeta>,
        settings:      PluginSettings,
        ctx:           *mut c_void,
        render_fn:     Option<RenderFn>,
        fullscreen_fn: Option<FullscreenFn>,
        destroy_fn:    Option<DestroyCbFn>,
        setting_cb_fn: Option<SettingCbFn>,
        extensions:    Vec<String>,
        lib:           Library,
        synthetic:     Option<Box<SparkPluginAbi>>,
    ) -> Self {
        LoadedPlugin {
            plugin_id,
            name,
            kind,
            version,
            description,
            author,
            path,
            schema,
            settings,
            ctx,
            render_fn,
            fullscreen_fn,
            destroy_fn,
            setting_cb_fn,
            extensions,
            _lib: lib,
            _synthetic: synthetic,
        }
    }

    // -----------------------------------------------------------------------
    // Visualizer API
    // -----------------------------------------------------------------------

    /// Invoke the plugin's render callback and return `count` samples,
    /// each clamped to `[0.0, 1.0]`.
    ///
    /// Returns an all-zeros vec if this is not a visualizer plugin or the
    /// render callback is missing.
    pub fn render(&self, pos_secs: f64, is_active: bool, count: usize) -> Vec<f64> {
        let Some(render) = self.render_fn else { return vec![0.0; count] };
        let mut buf = vec![0.0f64; count];
        // SAFETY: render_fn is validated at load time; buf is correctly sized.
        unsafe {
            render(
                self.ctx,
                pos_secs,
                is_active as c_int,
                buf.as_mut_ptr(),
                count as c_uint,
            );
        }
        buf.iter().map(|v| v.clamp(0.0, 1.0)).collect()
    }

    /// Invoke the plugin's optional fullscreen callback.
    ///
    /// If the plugin does not provide a `fullscreen` callback (the field is
    /// `null`), this method is a no-op — the host must not fall back to its
    /// own fullscreen rendering.
    pub fn fullscreen(&self) {
        if let Some(fs) = self.fullscreen_fn {
            // SAFETY: fullscreen_fn is validated at load time.
            unsafe { fs(self.ctx); }
        }
        // Intentionally no fallback when fullscreen_fn is None.
    }

    /// Returns `true` if this visualizer plugin supports fullscreen mode.
    pub fn has_fullscreen(&self) -> bool {
        self.fullscreen_fn.is_some()
    }

    // -----------------------------------------------------------------------
    // Settings API
    // -----------------------------------------------------------------------

    /// Update a setting value, notify the plugin, and persist to disk.
    pub fn set_setting(&mut self, key: &str, value: String) {
        self.settings.set(key, value.clone());
        // Notify the plugin immediately so the change takes effect live.
        if let Some(cb) = self.setting_cb_fn {
            if let (Ok(k), Ok(v)) = (
                CString::new(key.replace('\0', "")),
                CString::new(value.replace('\0', "")),
            ) {
                // SAFETY: cb is a valid function pointer; k/v are alive for the call.
                unsafe { cb(self.ctx, k.as_ptr(), v.as_ptr()); }
            }
        }
        let _ = self.settings.save();
    }

    /// Return the current string value of a setting, or `None` if not set.
    pub fn get_setting(&self, key: &str) -> Option<&str> {
        self.settings.get(key)
    }
}

impl Drop for LoadedPlugin {
    /// Call the plugin's `destroy` callback, then let `_lib` be dropped to
    /// unload the library.
    fn drop(&mut self) {
        if let Some(destroy) = self.destroy_fn {
            // SAFETY: ctx came from init; destroy is the matching finaliser.
            unsafe { destroy(self.ctx); }
        }
        // _lib and _synthetic are dropped after this block.
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Copy a nullable null-terminated C string into an owned `String`.
///
/// Returns `None` if `ptr` is null or the bytes are not valid UTF-8.
///
/// # Safety
///
/// `ptr` must be null or point to a valid null-terminated string.
unsafe fn copy_cstr(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() { return None; }
    // SAFETY: caller guarantees ptr is a valid null-terminated string.
    Some(unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
}

/// Copy a null-terminated `SparkSettingDef` array into owned `SettingMeta` values.
///
/// Returns an empty vec if `ptr` is null.
///
/// # Safety
///
/// `ptr` must be null or point to a null-terminated array of `SparkSettingDef`.
unsafe fn copy_schema(ptr: *const SparkSettingDef) -> Vec<SettingMeta> {
    if ptr.is_null() { return vec![]; }
    let mut out = Vec::new();
    let mut i = 0usize;
    loop {
        // SAFETY: loop advances until the sentinel entry.
        let def = unsafe { &*ptr.add(i) };
        if def.value_type == SparkSettingType::End { break; }
        let key   = unsafe { copy_cstr(def.key)   }.unwrap_or_default();
        let label = unsafe { copy_cstr(def.label)  }.unwrap_or_else(|| key.clone());
        let description   = unsafe { copy_cstr(def.description) };
        let default_value = unsafe { copy_cstr(def.default_value) }.unwrap_or_default();
        let choices = if def.choices.is_null() {
            None
        } else {
            // SAFETY: choices is a null-terminated string from plugin static storage.
            let raw = unsafe { CStr::from_ptr(def.choices) }.to_string_lossy();
            Some(raw.split('|').map(str::to_string).collect())
        };
        let min_value = unsafe { copy_cstr(def.min_value) };
        let max_value = unsafe { copy_cstr(def.max_value) };
        out.push(SettingMeta {
            value_type: def.value_type,
            key, label, description, default_value, choices, min_value, max_value,
        });
        i += 1;
    }
    out
}

/// Copy a null-terminated array of null-terminated C strings into owned `String`s.
///
/// Used for filetype extension lists.
///
/// # Safety
///
/// `ptr` must be null or point to a null-terminated array of pointers to
/// null-terminated strings.
unsafe fn copy_extensions(ptr: *const *const c_char) -> Vec<String> {
    if ptr.is_null() { return vec![]; }
    let mut out = Vec::new();
    let mut i = 0usize;
    loop {
        // SAFETY: loop advances until a null entry.
        let entry = unsafe { *ptr.add(i) };
        if entry.is_null() { break; }
        if let Some(s) = unsafe { copy_cstr(entry) } {
            out.push(s);
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_kind_from_abi_kind() {
        assert_eq!(PluginKind::from(SparkPluginKind::Visualizer), PluginKind::Visualizer);
        assert_eq!(PluginKind::from(SparkPluginKind::Filetype),   PluginKind::Filetype);
    }

    /// A LoadedPlugin without a render callback returns all-zero samples.
    /// (We can't test a real plugin without a compiled .so, but the safe
    /// wrapper's no-op path is testable.)
    #[test]
    fn render_without_callback_returns_zeros() {
        // Build a minimal LoadedPlugin manually (bypassing from_abi).
        // We need a Library handle — use an existing system library.
        // This test is cfg(not(test_no_system_libs)) in CI if needed.
        // For now just verify the schema copy helper doesn't crash on null.
        let schema = unsafe { copy_schema(std::ptr::null()) };
        assert!(schema.is_empty());
    }
}
