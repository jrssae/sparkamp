// Public API — methods will be called by the plugin Settings UI once wired in.
#![allow(dead_code)]

//! Per-plugin settings persistence.
//!
//! Each installed plugin gets its own TOML file at:
//!
//! ```text
//! ~/.local/share/sparkamp/plugins/<plugin_id>/settings.toml
//! ```
//!
//! All values are stored as plain strings regardless of the declared setting
//! type — type enforcement only happens in the UI layer.  This makes the
//! format language-agnostic and trivially forward-compatible: unknown keys
//! are silently preserved, and new settings added by a plugin update are
//! seeded from the schema's `default_value` on the next load.
//!
//! `PluginSettings` is intentionally ignorant of the ABI types; it just
//! manages a `HashMap<String, String>` and a TOML file on disk.

use anyhow::Result;
use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::PathBuf;

use crate::plugin_abi::{SparkSettingDef, SparkSettingType};

// ---------------------------------------------------------------------------
// PluginSettings
// ---------------------------------------------------------------------------

/// In-memory view of one plugin's settings, with TOML persistence.
///
/// Create via [`PluginSettings::load`]; persist changes via [`PluginSettings::save`].
#[derive(Debug, Clone)]
pub struct PluginSettings {
    /// The plugin's stable reverse-DNS identifier (e.g. `"dev.sparkamp.viz.granite"`).
    plugin_id: String,
    /// Key → value map (all values are plain strings).
    values: HashMap<String, String>,
}

impl PluginSettings {
    // -----------------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------------

    /// Load (or create) the settings for `plugin_id`.
    ///
    /// If the settings file does not exist, returns an empty map.
    /// Any keys present in `schema` whose values are missing from the file
    /// are seeded with the schema's `default_value`.
    ///
    /// `schema` is the null-terminated `SparkSettingDef` array from the plugin
    /// ABI, or `null` if the plugin declares no settings.
    pub fn load(plugin_id: &str, schema: *const SparkSettingDef) -> Self {
        let path = Self::file_path(plugin_id);
        let mut values: HashMap<String, String> = HashMap::new();

        // Try to parse the existing file.
        if path.exists() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(table) = text.parse::<toml::Table>() {
                    if let Some(toml::Value::Table(settings)) = table.get("settings") {
                        for (k, v) in settings {
                            if let toml::Value::String(s) = v {
                                values.insert(k.clone(), s.clone());
                            }
                        }
                    }
                }
            }
        }

        // Seed any missing keys from the schema's default values.
        if !schema.is_null() {
            // SAFETY: schema is a null-terminated array owned by the plugin's
            // static storage; we only read from it here.
            let mut i = 0usize;
            loop {
                let def = unsafe { &*schema.add(i) };
                if def.value_type == SparkSettingType::End {
                    break;
                }
                if !def.key.is_null() {
                    // SAFETY: key is null-terminated UTF-8 from plugin static storage.
                    let key = unsafe { CStr::from_ptr(def.key) }
                        .to_string_lossy()
                        .into_owned();
                    if !values.contains_key(&key) {
                        let default = if def.default_value.is_null() {
                            String::new()
                        } else {
                            // SAFETY: default_value is null-terminated UTF-8.
                            unsafe { CStr::from_ptr(def.default_value) }
                                .to_string_lossy()
                                .into_owned()
                        };
                        values.insert(key, default);
                    }
                }
                i += 1;
            }
        }

        PluginSettings { plugin_id: plugin_id.to_string(), values }
    }

    /// Create an empty `PluginSettings` without reading from disk or a schema.
    ///
    /// Useful for v1 shim plugins that declare no settings.
    pub fn empty(plugin_id: &str) -> Self {
        PluginSettings { plugin_id: plugin_id.to_string(), values: HashMap::new() }
    }

    // -----------------------------------------------------------------------
    // Access
    // -----------------------------------------------------------------------

    /// Return the current string value for `key`, or `None` if not set.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// Update or insert a setting value in memory.
    ///
    /// Call [`save`](Self::save) to persist the change.
    pub fn set(&mut self, key: &str, value: String) {
        self.values.insert(key.to_string(), value);
    }

    /// Return an iterator over all `(key, value)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    // -----------------------------------------------------------------------
    // C-array helpers (used when calling plugin init)
    // -----------------------------------------------------------------------

    /// Build two parallel null-terminated `Vec<*const c_char>` arrays of keys
    /// and values, suitable for passing to the plugin's `init` callback.
    ///
    /// Returns `(key_ptrs, value_ptrs, _key_cstrings, _value_cstrings)` where
    /// the last two vecs own the backing `CString` allocations and must be kept
    /// alive for as long as the C-pointer arrays are in use.
    ///
    /// The trailing element of each pointer array is `null` (the C sentinel).
    pub fn as_c_arrays(
        &self,
    ) -> (
        Vec<*const c_char>,
        Vec<*const c_char>,
        Vec<std::ffi::CString>,
        Vec<std::ffi::CString>,
    ) {
        let mut key_cstrings   = Vec::with_capacity(self.values.len());
        let mut value_cstrings = Vec::with_capacity(self.values.len());

        for (k, v) in &self.values {
            // Replace interior NULs (should never happen, but be safe).
            key_cstrings.push(
                std::ffi::CString::new(k.replace('\0', "")).unwrap_or_default(),
            );
            value_cstrings.push(
                std::ffi::CString::new(v.replace('\0', "")).unwrap_or_default(),
            );
        }

        // Build null-terminated pointer arrays.
        let mut key_ptrs: Vec<*const c_char> =
            key_cstrings.iter().map(|s| s.as_ptr()).collect();
        key_ptrs.push(std::ptr::null());

        let mut value_ptrs: Vec<*const c_char> =
            value_cstrings.iter().map(|s| s.as_ptr()).collect();
        value_ptrs.push(std::ptr::null());

        (key_ptrs, value_ptrs, key_cstrings, value_cstrings)
    }

    // -----------------------------------------------------------------------
    // Persistence
    // -----------------------------------------------------------------------

    /// Write the current values to disk.
    ///
    /// Creates the parent directory if it does not exist.  A missing or
    /// corrupt file is silently overwritten.
    pub fn save(&self) -> Result<()> {
        let path = Self::file_path(&self.plugin_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut table = toml::Table::new();
        let mut settings = toml::Table::new();
        for (k, v) in &self.values {
            settings.insert(k.clone(), toml::Value::String(v.clone()));
        }
        table.insert("settings".to_string(), toml::Value::Table(settings));
        std::fs::write(&path, toml::to_string(&table)?)?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Path helpers
    // -----------------------------------------------------------------------

    /// Path to the settings file for a given plugin ID.
    pub fn file_path(plugin_id: &str) -> PathBuf {
        plugin_dir(plugin_id).join("settings.toml")
    }
}

// ---------------------------------------------------------------------------
// Directory helpers
// ---------------------------------------------------------------------------

/// Return the managed install directory for one plugin:
/// `~/.local/share/sparkamp/plugins/<plugin_id>/`.
pub fn plugin_dir(plugin_id: &str) -> PathBuf {
    managed_plugins_dir().join(plugin_id)
}

/// Return the root managed plugins directory:
/// `~/.local/share/sparkamp/plugins/`.
pub fn managed_plugins_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sparkamp")
        .join("plugins")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    /// An empty settings object has no values.
    #[test]
    fn empty_settings_has_no_values() {
        let s = PluginSettings::empty("test.plugin");
        assert!(s.get("any_key").is_none());
    }

    /// Setting a key makes it retrievable.
    #[test]
    fn set_and_get_roundtrip() {
        let mut s = PluginSettings::empty("test.plugin");
        s.set("speed", "1.5".to_string());
        assert_eq!(s.get("speed"), Some("1.5"));
    }

    /// as_c_arrays returns matching pointer arrays with null terminators.
    #[test]
    fn c_arrays_have_null_terminator() {
        let mut s = PluginSettings::empty("test.plugin");
        s.set("foo", "bar".to_string());
        let (keys, vals, _ks, _vs) = s.as_c_arrays();
        // Last entry in each array must be null.
        assert!(keys.last().copied() == Some(ptr::null()));
        assert!(vals.last().copied() == Some(ptr::null()));
    }

    /// load with a null schema still works (no crash, no defaults seeded).
    #[test]
    fn load_with_null_schema_does_not_crash() {
        // Use a plugin ID that will never have a real file on disk.
        let s = PluginSettings::load("__test_null_schema__", ptr::null());
        assert!(s.get("anything").is_none());
    }
}
