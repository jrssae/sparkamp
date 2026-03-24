// Public API — install/uninstall and per-plugin accessors will be called by the
// plugin Settings UI once wired in.
#![allow(dead_code)]

//! Central plugin registry for Sparkamp.
//!
//! [`PluginManager`] owns all loaded plugins (both v2 and legacy v1) for the
//! lifetime of the application.  It is the single place where `.so` files are
//! opened with `libloading`, ABI versions are checked, and v1 shims are built.
//!
//! # Plugin discovery order
//!
//! 1. **Managed directory** — `~/.local/share/sparkamp/plugins/<plugin_id>/`
//!    subdirectories (created by [`PluginManager::install`]).
//! 2. **Legacy visualizer directory** — flat dir from `config.plugins.visualizer_dir`
//!    (v1 ABI, shimmed automatically).
//! 3. **Legacy filetype directory** — flat dir from `config.plugins.filetype_dir`
//!    (v1 ABI, shimmed automatically).
//!
//! # Loading sequence for a single `.so`
//!
//! 1. Try the v2 entry point `sparkamp_plugin()` → [`LoadedPlugin::from_abi`].
//! 2. Try the v1 viz entry point `sparkamp_viz_plugin()` → shim.
//! 3. Try the v1 filetype entry point `sparkamp_filetype_plugin()` → shim.
//! 4. If none match, print a warning and skip the file.
//!
//! # Install / uninstall
//!
//! [`PluginManager::install`] copies a `.so` into the managed directory tree
//! and loads it; [`PluginManager::uninstall`] drops the plugin and removes its
//! directory.

use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use libloading::{Library, Symbol};

use crate::loaded_plugin::{DestroyCbFn, LoadedPlugin, PluginKind, RenderFn};
use crate::plugin_abi::{SparkPluginAbi, SparkVizPluginAbi_v1, SPARKAMP_PLUGIN_ABI_VERSION};
use crate::plugin_settings::{self, PluginSettings};

// ---------------------------------------------------------------------------
// PluginManager
// ---------------------------------------------------------------------------

/// Owns all loaded Sparkamp plugins and tracks which visualizer is active.
pub struct PluginManager {
    /// All successfully loaded plugins, in load order.
    plugins: Vec<LoadedPlugin>,
    /// Which element of `plugins` (filtered to `Visualizer` kind) is active.
    ///
    /// `None` means "use the built-in visualizer".
    active_viz_idx: Option<usize>,
}

impl PluginManager {
    // -----------------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------------

    /// Create an empty `PluginManager` with no loaded plugins.
    pub fn new() -> Self {
        PluginManager {
            plugins: Vec::new(),
            active_viz_idx: None,
        }
    }

    /// Populate the manager by scanning the standard plugin directories.
    ///
    /// Scans the managed directory first, then the optional legacy directories
    /// from the config.  Duplicate `plugin_id`s are silently skipped.
    pub fn load_from_config(&mut self, config: &crate::config::Config) {
        self.scan_managed_dir();
        self.scan_dir_for_v1_viz(&config.plugins.visualizer_dir);
        self.scan_dir_for_v1_filetype(&config.plugins.filetype_dir);
    }

    // -----------------------------------------------------------------------
    // Directory scanning
    // -----------------------------------------------------------------------

    /// Scan `~/.local/share/sparkamp/plugins/` for managed v2 plugins.
    ///
    /// Each subdirectory is expected to contain exactly one `.so` file, which
    /// is the installed plugin library.
    pub fn scan_managed_dir(&mut self) {
        let managed = plugin_settings::managed_plugins_dir();
        if !managed.is_dir() {
            return;
        }

        // Collect all .so files found in immediate subdirectories.
        let mut so_paths: Vec<PathBuf> = std::fs::read_dir(&managed)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .flat_map(|plugin_dir| {
                std::fs::read_dir(&plugin_dir)
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().map_or(false, |ext| ext == "so"))
                    .collect::<Vec<_>>()
            })
            .collect();

        so_paths.sort(); // deterministic load order

        for path in so_paths {
            if let Some(plugin) = Self::try_load_path(&path) {
                if self.plugins.iter().any(|p| p.plugin_id == plugin.plugin_id) {
                    eprintln!(
                        "plugin_manager: skipping duplicate plugin '{}' at {:?}",
                        plugin.plugin_id, path
                    );
                    continue;
                }
                eprintln!("plugin_manager: loaded '{}' from {:?}", plugin.name, path);
                self.plugins.push(plugin);
            }
        }
    }

    /// Scan a legacy flat directory for v1 visualizer `.so` files.
    ///
    /// No-op if `dir` is empty or does not exist.
    pub fn scan_dir_for_v1_viz(&mut self, dir: &str) {
        self.scan_legacy_dir(dir, LegacyKind::Viz);
    }

    /// Scan a legacy flat directory for v1 filetype `.so` files.
    ///
    /// No-op if `dir` is empty or does not exist.
    pub fn scan_dir_for_v1_filetype(&mut self, dir: &str) {
        self.scan_legacy_dir(dir, LegacyKind::Filetype);
    }

    fn scan_legacy_dir(&mut self, dir: &str, kind: LegacyKind) {
        if dir.is_empty() {
            return;
        }
        let dir_path = Path::new(dir);
        if !dir_path.is_dir() {
            return;
        }

        let mut so_paths: Vec<PathBuf> = std::fs::read_dir(dir_path)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |ext| ext == "so"))
            .collect();

        so_paths.sort();

        for path in so_paths {
            // Open the library once; the winning constructor takes ownership.
            let lib = match unsafe { Library::new(&path) } {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("plugin_manager: cannot open {:?}: {}", path, e);
                    continue;
                }
            };
            let loaded = match kind {
                LegacyKind::Viz      => Self::try_load_v1_viz(&path, lib),
                LegacyKind::Filetype => Self::try_load_v1_filetype(&path, lib),
            };
            if let Some(plugin) = loaded {
                if self.plugins.iter().any(|p| p.plugin_id == plugin.plugin_id) {
                    continue;
                }
                eprintln!(
                    "plugin_manager: loaded legacy '{}' from {:?}",
                    plugin.name, path
                );
                self.plugins.push(plugin);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Install / uninstall
    // -----------------------------------------------------------------------

    /// Install a plugin from `src_path` into the managed directory.
    ///
    /// Steps:
    /// 1. Probe the file to discover the plugin's stable ID.
    /// 2. Create `~/.local/share/sparkamp/plugins/<plugin_id>/`.
    /// 3. Copy the `.so` there.
    /// 4. Load the plugin from its final location and register it.
    ///
    /// Returns the `plugin_id` on success.  Fails if the file is not a valid
    /// Sparkamp v2 plugin.
    pub fn install(&mut self, src_path: &Path) -> Result<String> {
        // Probe to get the plugin_id without permanently holding the library
        // handle (we re-open from the managed path after copying).
        let probe = Self::try_load_path(src_path)
            .ok_or_else(|| anyhow!("not a valid Sparkamp v2 plugin: {:?}", src_path))?;
        let plugin_id = probe.plugin_id.clone();
        // Drop probe — releases the library handle before we copy the file.
        drop(probe);

        // Create managed directory.
        let managed_dir = plugin_settings::plugin_dir(&plugin_id);
        std::fs::create_dir_all(&managed_dir)?;

        // Copy the .so into the managed directory.
        let filename = src_path
            .file_name()
            .ok_or_else(|| anyhow!("source path has no filename: {:?}", src_path))?;
        let dest_path = managed_dir.join(filename);
        std::fs::copy(src_path, &dest_path)?;

        // Load from the final managed path.
        let plugin = Self::try_load_path(&dest_path)
            .ok_or_else(|| anyhow!("failed to reload plugin from managed path {:?}", dest_path))?;

        // Replace any existing plugin with the same ID (upgrade path).
        self.plugins.retain(|p| p.plugin_id != plugin_id);
        self.plugins.push(plugin);

        self.fix_active_viz_idx();
        Ok(plugin_id)
    }

    /// Uninstall a plugin by its stable `plugin_id`.
    ///
    /// Drops the loaded plugin (calling its `destroy` callback and unloading
    /// the library), then removes the managed directory from disk.  Only
    /// managed plugins can be uninstalled this way; legacy plugins loaded from
    /// user-configured directories are not touched.
    pub fn uninstall(&mut self, plugin_id: &str) -> Result<()> {
        let idx = self.plugins.iter().position(|p| p.plugin_id == plugin_id)
            .ok_or_else(|| anyhow!("plugin '{}' is not loaded", plugin_id))?;

        let managed_dir = plugin_settings::plugin_dir(plugin_id);

        // Drop the plugin — this calls destroy() and unloads the .so.
        self.plugins.remove(idx);

        // Remove the managed directory (includes settings.toml and the .so).
        if managed_dir.is_dir() {
            std::fs::remove_dir_all(&managed_dir)?;
        }

        self.fix_active_viz_idx();
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Iterator over all loaded plugins.
    pub fn all_plugins(&self) -> &[LoadedPlugin] {
        &self.plugins
    }

    /// Iterator over loaded visualizer plugins only.
    pub fn viz_plugins(&self) -> impl Iterator<Item = &LoadedPlugin> {
        self.plugins.iter().filter(|p| p.kind == PluginKind::Visualizer)
    }

    /// Iterator over loaded filetype plugins only.
    pub fn filetype_plugins(&self) -> impl Iterator<Item = &LoadedPlugin> {
        self.plugins.iter().filter(|p| p.kind == PluginKind::Filetype)
    }

    /// Collect all extra file extensions registered by filetype plugins.
    ///
    /// Returns a deduplicated, sorted list (no leading dots, lower-case).
    pub fn extra_extensions(&self) -> Vec<String> {
        let mut exts: Vec<String> = self
            .filetype_plugins()
            .flat_map(|p| p.extensions.iter().cloned())
            .collect();
        exts.sort();
        exts.dedup();
        exts
    }

    /// Find a plugin by its stable `plugin_id`.
    pub fn get_plugin(&self, plugin_id: &str) -> Option<&LoadedPlugin> {
        self.plugins.iter().find(|p| p.plugin_id == plugin_id)
    }

    /// Find a plugin mutably by its stable `plugin_id`.
    pub fn get_plugin_mut(&mut self, plugin_id: &str) -> Option<&mut LoadedPlugin> {
        self.plugins.iter_mut().find(|p| p.plugin_id == plugin_id)
    }

    /// Index (into the list returned by [`viz_plugins`]) of the active visualizer.
    ///
    /// `None` means "use the host's built-in visualizer".
    pub fn active_viz_index(&self) -> Option<usize> {
        self.active_viz_idx
    }

    /// Set which visualizer plugin is active (by viz-list index, not global index).
    pub fn set_active_viz_index(&mut self, idx: Option<usize>) {
        let max = self.viz_plugins().count();
        self.active_viz_idx = idx.filter(|&i| i < max);
    }

    /// Return the currently active visualizer plugin, if any.
    pub fn active_viz_plugin(&self) -> Option<&LoadedPlugin> {
        let idx = self.active_viz_idx?;
        self.viz_plugins().nth(idx)
    }

    /// Return the currently active visualizer plugin mutably, if any.
    pub fn active_viz_plugin_mut(&mut self) -> Option<&mut LoadedPlugin> {
        let idx = self.active_viz_idx?;
        self.plugins
            .iter_mut()
            .filter(|p| p.kind == PluginKind::Visualizer)
            .nth(idx)
    }

    /// Advance the active visualizer to the next plugin.
    ///
    /// Cycling past the last plugin returns to `None` (built-in visualizer).
    pub fn cycle_viz(&mut self) {
        let count = self.viz_plugins().count();
        if count == 0 {
            self.active_viz_idx = None;
            return;
        }
        self.active_viz_idx = match self.active_viz_idx {
            None         => Some(0),
            Some(i) if i + 1 < count => Some(i + 1),
            _            => None,
        };
    }

    // -----------------------------------------------------------------------
    // Private loading helpers
    // -----------------------------------------------------------------------

    /// Try to load a `.so` as any supported plugin type (v2, v1 viz, v1 filetype).
    ///
    /// Opens the library once, detects which entry point is present by looking
    /// up symbol names, then delegates to the appropriate typed loader.
    fn try_load_path(path: &Path) -> Option<LoadedPlugin> {
        // SAFETY: loading arbitrary shared libraries is inherently unsafe; the
        // user explicitly configured or installed these paths.
        let lib = unsafe { Library::new(path) }.map_err(|e| {
            eprintln!("plugin_manager: cannot open {:?}: {}", path, e);
        }).ok()?;

        // Detect which entry point is present without consuming `lib`.
        // The Symbol borrows lib; we drop it before moving lib.
        let has_v2: bool = {
            let s: Result<Symbol<unsafe extern "C" fn()>, _> =
                unsafe { lib.get(b"sparkamp_plugin\0") };
            s.is_ok()
        };
        let has_v1_viz: bool = !has_v2 && {
            let s: Result<Symbol<unsafe extern "C" fn()>, _> =
                unsafe { lib.get(b"sparkamp_viz_plugin\0") };
            s.is_ok()
        };
        let has_v1_filetype: bool = !has_v2 && !has_v1_viz && {
            let s: Result<Symbol<unsafe extern "C" fn()>, _> =
                unsafe { lib.get(b"sparkamp_filetype_plugin\0") };
            s.is_ok()
        };

        if has_v2 {
            Self::try_load_v2(path, lib)
        } else if has_v1_viz {
            Self::try_load_v1_viz(path, lib)
        } else if has_v1_filetype {
            Self::try_load_v1_filetype(path, lib)
        } else {
            eprintln!(
                "plugin_manager: {:?} has no recognised Sparkamp entry point; skipping",
                path
            );
            None
        }
    }

    /// Try the ABI v2 entry point `sparkamp_plugin()`.
    fn try_load_v2(path: &Path, lib: Library) -> Option<LoadedPlugin> {
        let entry: Symbol<unsafe extern "C" fn() -> *const SparkPluginAbi> = unsafe {
            lib.get(b"sparkamp_plugin\0")
        }.ok()?;

        let abi_ptr = unsafe { entry() };
        if abi_ptr.is_null() {
            eprintln!("plugin_manager: {:?} returned null from sparkamp_plugin", path);
            return None;
        }

        let abi_version = unsafe { (*abi_ptr).abi_version };
        if abi_version != SPARKAMP_PLUGIN_ABI_VERSION {
            eprintln!(
                "plugin_manager: {:?} ABI version {} ≠ expected {}; skipping",
                path, abi_version, SPARKAMP_PLUGIN_ABI_VERSION
            );
            return None;
        }

        // SAFETY: abi_ptr is non-null and the version check passed.
        unsafe { LoadedPlugin::from_abi(abi_ptr, lib, path.to_owned(), None) }
    }

    /// Try the legacy v1 visualizer entry point `sparkamp_viz_plugin()`.
    fn try_load_v1_viz(path: &Path, lib: Library) -> Option<LoadedPlugin> {
        let entry: Symbol<unsafe extern "C" fn() -> *const SparkVizPluginAbi_v1> = unsafe {
            lib.get(b"sparkamp_viz_plugin\0")
        }.ok()?;

        let v1_ptr = unsafe { entry() };
        if v1_ptr.is_null() {
            eprintln!("plugin_manager: {:?} returned null from sparkamp_viz_plugin", path);
            return None;
        }

        let v1 = unsafe { &*v1_ptr };

        if v1.abi_version != 1 {
            eprintln!(
                "plugin_manager: {:?} reports v1 ABI version {} ≠ 1; skipping",
                path, v1.abi_version
            );
            return None;
        }

        let render_fn: Option<RenderFn> = v1.render;
        if render_fn.is_none() {
            eprintln!("plugin_manager: v1 viz {:?} has no render callback; skipping", path);
            return None;
        }

        // Derive a stable plugin_id from the name or file stem.
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
        let name = if v1.name.is_null() {
            stem.to_string()
        } else {
            unsafe { CStr::from_ptr(v1.name) }.to_string_lossy().into_owned()
        };
        let plugin_id = format!("legacy.viz.{}", slug(&name));

        // Call v1 init (no-arg signature, ignores settings).
        let ctx = if let Some(init) = v1.init {
            unsafe { init() }
        } else {
            std::ptr::null_mut()
        };

        let destroy_fn: Option<DestroyCbFn> = v1.destroy;
        let settings = PluginSettings::empty(&plugin_id);

        Some(LoadedPlugin::new_manual(
            plugin_id,
            name,
            PluginKind::Visualizer,
            None, None, None,
            path.to_owned(),
            vec![],
            settings,
            ctx,
            render_fn,
            None,          // no fullscreen in v1
            destroy_fn,
            None,          // no on_setting_changed in v1
            vec![],
            lib,
            None,
        ))
    }

    /// Try the legacy v1 filetype entry point `sparkamp_filetype_plugin()`.
    fn try_load_v1_filetype(path: &Path, lib: Library) -> Option<LoadedPlugin> {
        use crate::filetype_plugin::SparkFiletypePluginAbi;

        let entry: Symbol<unsafe extern "C" fn() -> *const SparkFiletypePluginAbi> = unsafe {
            lib.get(b"sparkamp_filetype_plugin\0")
        }.ok()?;

        let v1_ptr = unsafe { entry() };
        if v1_ptr.is_null() {
            eprintln!("plugin_manager: {:?} returned null from sparkamp_filetype_plugin", path);
            return None;
        }

        let v1 = unsafe { &*v1_ptr };

        if v1.abi_version != 1 {
            eprintln!(
                "plugin_manager: {:?} reports filetype ABI version {} ≠ 1; skipping",
                path, v1.abi_version
            );
            return None;
        }

        // Copy extensions from the null-terminated array.
        let extensions = unsafe { copy_null_terminated_strings(v1.extensions) };
        if extensions.is_empty() {
            eprintln!("plugin_manager: {:?} registers no extensions; skipping", path);
            return None;
        }

        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
        let name = if v1.name.is_null() {
            stem.to_string()
        } else {
            unsafe { CStr::from_ptr(v1.name) }.to_string_lossy().into_owned()
        };
        let plugin_id = format!("legacy.filetype.{}", slug(&name));

        let settings = PluginSettings::empty(&plugin_id);

        // v1 filetype ABI has no init/destroy callbacks.
        Some(LoadedPlugin::new_manual(
            plugin_id,
            name,
            PluginKind::Filetype,
            None, None, None,
            path.to_owned(),
            vec![],
            settings,
            std::ptr::null_mut(),
            None, None, None, None,
            extensions,
            lib,
            None,
        ))
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Clamp `active_viz_idx` after a plugin is added or removed.
    fn fix_active_viz_idx(&mut self) {
        let count = self.viz_plugins().count();
        if count == 0 {
            self.active_viz_idx = None;
        } else if let Some(idx) = self.active_viz_idx {
            if idx >= count {
                self.active_viz_idx = Some(count - 1);
            }
        }
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Module-level helpers
// ---------------------------------------------------------------------------

/// Discriminant used when scanning a legacy plugin directory.
#[derive(Clone, Copy)]
enum LegacyKind {
    Viz,
    Filetype,
}

/// Convert a name string to a compact lowercase ASCII identifier.
///
/// Non-alphanumeric characters become underscores; leading/trailing
/// underscores are stripped.
fn slug(s: &str) -> String {
    let raw: String = s.chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    raw.trim_matches('_').to_string()
}

/// Walk a null-terminated array of null-terminated C strings and collect them.
///
/// Returns lower-case owned strings; empty strings are skipped.
///
/// # Safety
///
/// `ptr` must be null or point to a null-terminated array of pointers to
/// null-terminated UTF-8 strings.
unsafe fn copy_null_terminated_strings(ptr: *const *const c_char) -> Vec<String> {
    if ptr.is_null() {
        return vec![];
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    loop {
        let entry = unsafe { *ptr.add(i) };
        if entry.is_null() {
            break;
        }
        let s = unsafe { CStr::from_ptr(entry) }.to_string_lossy().to_lowercase();
        if !s.is_empty() {
            out.push(s.to_string());
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
    fn new_manager_has_no_plugins() {
        let pm = PluginManager::new();
        assert!(pm.all_plugins().is_empty());
        assert!(pm.active_viz_index().is_none());
    }

    #[test]
    fn scan_managed_nonexistent_dir_does_not_panic() {
        let mut pm = PluginManager::new();
        // The managed dir almost certainly does not exist in CI; should be a no-op.
        pm.scan_managed_dir();
        // No assertion about plugin count — just must not panic.
    }

    #[test]
    fn scan_legacy_empty_dir_does_not_panic() {
        let mut pm = PluginManager::new();
        pm.scan_dir_for_v1_viz("");
        pm.scan_dir_for_v1_filetype("");
        assert!(pm.all_plugins().is_empty());
    }

    #[test]
    fn scan_legacy_nonexistent_dir_does_not_panic() {
        let mut pm = PluginManager::new();
        pm.scan_dir_for_v1_viz("/nonexistent/sparkamp/viz");
        pm.scan_dir_for_v1_filetype("/nonexistent/sparkamp/filetypes");
        assert!(pm.all_plugins().is_empty());
    }

    #[test]
    fn cycle_viz_with_no_plugins_stays_none() {
        let mut pm = PluginManager::new();
        pm.cycle_viz();
        assert_eq!(pm.active_viz_index(), None);
    }

    #[test]
    fn slug_converts_special_chars() {
        // Trailing punctuation is trimmed (slug strips leading/trailing underscores).
        assert_eq!(slug("Hello World!"), "hello_world");
        assert_eq!(slug("Granite v2.0"), "granite_v2_0");
        assert_eq!(slug("__leading"), "leading");
    }

    #[test]
    fn slug_empty_string() {
        assert_eq!(slug(""), "");
    }

    #[test]
    fn extra_extensions_empty_when_no_filetype_plugins() {
        let pm = PluginManager::new();
        assert!(pm.extra_extensions().is_empty());
    }
}
