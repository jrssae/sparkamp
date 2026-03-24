//! Sparkamp Plugin ABI — version 2.
//!
//! This module is the single source of truth for the C-compatible plugin
//! interface.  Every type here has a `#[repr(C)]` layout that exactly mirrors
//! the declarations in `sparkamp_plugin.h` so that plugins written in C,
//! C++, Zig, or any other language that honours C calling conventions can
//! implement the interface without depending on the Rust toolchain.
//!
//! # How to write a plugin
//!
//! 1. Copy (or `#include`) `sparkamp_plugin.h` from the SDK.
//! 2. Fill a static `SparkPluginAbi` struct.
//! 3. Export `const SparkPluginAbi *sparkamp_plugin(void)` that returns a
//!    pointer to your static descriptor.
//! 4. Build as a shared library (`-shared -fPIC` on Linux).
//! 5. Drop the resulting `.so` into the Sparkamp install dialog.
//!
//! # Versioning
//!
//! [`SPARKAMP_PLUGIN_ABI_VERSION`] is incremented whenever the layout of
//! [`SparkPluginAbi`] or the calling convention of any callback changes in a
//! backward-incompatible way.  Plugins compiled against an older version are
//! detected at load time and either shimmed (v1 → v2) or rejected with a
//! warning.
//!
//! # V1 backward compatibility
//!
//! Sparkamp still loads plugins compiled against ABI v1, which export the
//! old entry point `sparkamp_viz_plugin`.  Those plugins are wrapped in a
//! synthetic v2 descriptor by the plugin manager and behave identically to
//! first-class v2 visualizer plugins, except they have no settings schema.

#![allow(non_camel_case_types)]
// The ABI types are only "constructed" by external plugin code; the host reads
// field values at load time but does not create them from within this crate.
#![allow(dead_code)]

use std::os::raw::{c_char, c_double, c_int, c_uint, c_void};

// ---------------------------------------------------------------------------
// ABI version
// ---------------------------------------------------------------------------

/// The ABI version this build of Sparkamp understands for v2 plugins.
///
/// A plugin whose [`SparkPluginAbi::abi_version`] field does not equal this
/// constant is rejected at load time (unless it is a recognised v1 plugin,
/// which is shimmed automatically).
pub const SPARKAMP_PLUGIN_ABI_VERSION: u32 = 2;

// ---------------------------------------------------------------------------
// Setting value types
// ---------------------------------------------------------------------------

/// Discriminant for [`SparkSettingDef::value_type`].
///
/// The numerical values are stable across ABI versions; add new variants
/// only at the end so that existing plugins can safely ignore unknown types.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SparkSettingType {
    /// Sentinel — marks the end of the settings schema array.
    End    = 0,
    /// Boolean toggle.  Stored / passed as `"0"` (off) or `"1"` (on).
    Bool   = 1,
    /// Signed integer.  Stored / passed as a decimal string (e.g. `"32"`).
    Int    = 2,
    /// Floating-point number.  Stored / passed as a decimal string (e.g. `"0.75"`).
    Float  = 3,
    /// Free-form UTF-8 string.
    String = 4,
    /// One-of-N choice.  The valid options are listed in
    /// [`SparkSettingDef::choices`] as a pipe-separated string
    /// (e.g. `"Classic|Fire|Neon"`).  The stored value is the chosen option.
    Choice = 5,
}

// ---------------------------------------------------------------------------
// SparkSettingDef — one entry in a plugin's settings schema
// ---------------------------------------------------------------------------

/// Describes a single user-configurable setting exported by a plugin.
///
/// Plugins declare an array of `SparkSettingDef` values — terminated by an
/// entry with [`SparkSettingType::End`] — and assign it to
/// [`SparkPluginAbi::settings_schema`].  Sparkamp reads this array once at
/// load time and generates the matching UI widgets dynamically; the plugin
/// does not need to contain any GTK or TUI code.
///
/// All pointer fields that are listed as "may be null" are genuinely optional;
/// the host guards every dereference.
#[repr(C)]
pub struct SparkSettingDef {
    /// Type of this setting.  `End` marks the end of the array.
    pub value_type: SparkSettingType,

    /// Machine-readable key used as the TOML key and passed back to the
    /// plugin in `init()` and `on_setting_changed()`.
    /// Null-terminated ASCII, no whitespace.  Must not be null.
    pub key: *const c_char,

    /// Human-readable label shown in the settings UI (e.g. `"Bar count"`).
    /// May be null (host falls back to displaying `key`).
    pub label: *const c_char,

    /// Short description shown as a subtitle / tooltip.  May be null.
    pub description: *const c_char,

    /// Default value as a string.
    ///
    /// - `Bool`:   `"0"` or `"1"`.
    /// - `Int`:    decimal, e.g. `"32"`.
    /// - `Float`:  decimal, e.g. `"0.75"`.
    /// - `String`: any UTF-8 text.
    /// - `Choice`: must be one of the pipe-separated values in `choices`.
    ///
    /// May be null (treated as empty string / first choice).
    pub default_value: *const c_char,

    /// For [`SparkSettingType::Choice`]: pipe-separated option list,
    /// e.g. `"Classic|Fire|Neon"`.
    /// Null for all other types.
    pub choices: *const c_char,

    /// Minimum value (inclusive) for `Int` and `Float` types, as a string.
    /// Null means unbounded below.
    pub min_value: *const c_char,

    /// Maximum value (inclusive) for `Int` and `Float` types, as a string.
    /// Null means unbounded above.
    pub max_value: *const c_char,
}

// SAFETY: all pointers point into static storage inside the plugin binary.
unsafe impl Send for SparkSettingDef {}
unsafe impl Sync for SparkSettingDef {}

// ---------------------------------------------------------------------------
// Plugin kind
// ---------------------------------------------------------------------------

/// Discriminant for [`SparkPluginAbi::kind`].
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SparkPluginKind {
    /// Audio visualizer — provides a `render` callback.
    Visualizer = 1,
    /// Audio file decoder / metadata reader.
    Filetype   = 2,
}

// ---------------------------------------------------------------------------
// Type-specific callback blocks
// ---------------------------------------------------------------------------

/// Callbacks available to visualizer plugins.
///
/// Every function pointer is optional (`Option<fn...>`).  The host treats
/// a null pointer as "not implemented".
#[repr(C)]
pub struct SparkVizCallbacks {
    /// Called every frame to produce `count` output samples.
    ///
    /// | Parameter            | Description                                     |
    /// |----------------------|-------------------------------------------------|
    /// | `ctx`                | Opaque pointer returned by `init`.              |
    /// | `playback_pos_secs`  | Current track position in seconds.              |
    /// | `is_active`          | `1` = playing; `0` = paused or stopped.         |
    /// | `out`                | Caller-allocated buffer of `count` `f64` values.|
    /// | `count`              | Number of samples to write into `out`.          |
    ///
    /// The plugin writes `count` normalised values into `out`.  The host
    /// clamps each value to `[0.0, 1.0]` after the call.
    ///
    /// Must not be null for a visualizer plugin.
    pub render: Option<
        unsafe extern "C" fn(
            ctx:              *mut c_void,
            playback_pos_secs: c_double,
            is_active:         c_int,
            out:               *mut c_double,
            count:             c_uint,
        ),
    >,

    /// Optional fullscreen mode.
    ///
    /// Called when the user presses `f` or double-clicks the visualizer area.
    /// The plugin is expected to open its own fullscreen window (or use the
    /// terminal) and **block** until the user closes it (e.g. by pressing Esc).
    ///
    /// If this field is **null** the host does nothing — no fallback rendering
    /// is attempted.  Plugins that do not support a fullscreen mode should
    /// leave this as null.
    ///
    /// `ctx` is the same opaque pointer returned by `init`.
    pub fullscreen: Option<unsafe extern "C" fn(ctx: *mut c_void)>,
}

/// Callbacks available to filetype / decoder plugins.
#[repr(C)]
pub struct SparkFiletypeCallbacks {
    /// Null-terminated array of file extensions (without the leading dot)
    /// that this plugin can handle.  E.g. `["xyz", "abc", null]`.
    /// Must not be null.
    pub extensions: *const *const c_char,

    /// Optional metadata reader.  Writes into `out_title` and `out_artist`
    /// (both may be null on the caller side — the plugin skips them if null).
    /// Returns `1` on success, `0` on failure.
    /// Strings returned via the out-pointers must be freed with `free_string`.
    pub read_metadata: Option<
        unsafe extern "C" fn(
            path:       *const c_char,
            out_title:  *mut *mut c_char,
            out_artist: *mut *mut c_char,
        ) -> c_int,
    >,

    /// Matching free function for strings returned by `read_metadata`.
    /// Must not be null if `read_metadata` is non-null.
    pub free_string: Option<unsafe extern "C" fn(*mut c_char)>,
}

// ---------------------------------------------------------------------------
// SparkPluginAbi — the master plugin descriptor
// ---------------------------------------------------------------------------

/// The top-level plugin descriptor.  Every v2 plugin exports a `static` of
/// this type and returns a pointer to it from `sparkamp_plugin()`.
///
/// The struct must remain valid for the entire lifetime of the loaded library.
/// Sparkamp copies all string fields it needs at load time; the plugin does
/// not need to keep them alive beyond that.
#[repr(C)]
pub struct SparkPluginAbi {
    // ── Identity ─────────────────────────────────────────────────────────────

    /// Must equal [`SPARKAMP_PLUGIN_ABI_VERSION`].
    pub abi_version: u32,

    /// Whether this plugin is a visualizer or a filetype decoder.
    pub kind: SparkPluginKind,

    /// Stable reverse-DNS identifier used as the install directory name and
    /// the settings persistence key.  E.g. `"dev.sparkamp.viz.granite"`.
    /// Must not be null.
    pub plugin_id: *const c_char,

    /// Human-readable display name.  May be null (host uses the file stem).
    pub name: *const c_char,

    /// Semantic version string, e.g. `"1.0.0"`.  May be null.
    pub version: *const c_char,

    /// One-line description shown in the Plugins settings tab.  May be null.
    pub description: *const c_char,

    /// Author or organisation name.  May be null.
    pub author: *const c_char,

    // ── Settings schema ───────────────────────────────────────────────────────

    /// Array of [`SparkSettingDef`] entries, terminated by an entry whose
    /// `value_type` is [`SparkSettingType::End`].
    /// May be null if the plugin has no configurable settings.
    pub settings_schema: *const SparkSettingDef,

    // ── Lifecycle callbacks ───────────────────────────────────────────────────

    /// Called once after the library is loaded.
    ///
    /// The host passes all currently-persisted setting values as two parallel
    /// null-terminated arrays:
    ///
    /// ```text
    /// keys   = ["speed", "color_mode", NULL]
    /// values = ["1.5",   "Classic",    NULL]
    /// ```
    ///
    /// If no settings are persisted yet, both arrays contain only the
    /// terminating `NULL`.  The plugin uses these to initialise its state.
    ///
    /// Returns an opaque context pointer that is passed to all other
    /// callbacks.  May return null (e.g. if no per-instance state is needed).
    ///
    /// May be null (no initialisation needed).
    pub init: Option<
        unsafe extern "C" fn(
            keys:   *const *const c_char,
            values: *const *const c_char,
        ) -> *mut c_void,
    >,

    /// Called once just before the library is unloaded.  Free all resources
    /// allocated in `init`.  May be null.
    pub destroy: Option<unsafe extern "C" fn(ctx: *mut c_void)>,

    /// Called whenever a setting value changes (user edits it in the UI).
    ///
    /// `key` and `value` are null-terminated UTF-8 strings.  The plugin
    /// updates its internal state immediately so the change takes effect
    /// without a restart.  May be null (settings are only applied at `init`).
    pub on_setting_changed: Option<
        unsafe extern "C" fn(
            ctx:   *mut c_void,
            key:   *const c_char,
            value: *const c_char,
        ),
    >,

    // ── Type-specific callbacks ───────────────────────────────────────────────

    /// Active when `kind == SparkPluginKind::Visualizer`.
    pub viz: SparkVizCallbacks,

    /// Active when `kind == SparkPluginKind::Filetype`.
    pub filetype: SparkFiletypeCallbacks,
}

// SAFETY: all pointers in SparkPluginAbi point into static storage in the
// plugin binary and are valid for the library's lifetime.  Sparkamp only ever
// uses them on the main GTK/TUI thread.
unsafe impl Send for SparkPluginAbi {}
unsafe impl Sync for SparkPluginAbi {}

// ---------------------------------------------------------------------------
// V1 ABI shim support — the old SparkVizPluginAbi from src/viz_plugin.rs
// ---------------------------------------------------------------------------

/// The v1 visualizer-only ABI struct (kept for backward-compat loading).
///
/// Plugins that export `sparkamp_viz_plugin()` and set `abi_version = 1`
/// are automatically wrapped in a synthetic [`SparkPluginAbi`] by
/// `plugin_manager::try_load_v1_viz`.
#[repr(C)]
pub struct SparkVizPluginAbi_v1 {
    pub abi_version: u32,
    pub name:        *const c_char,
    pub init:        Option<unsafe extern "C" fn() -> *mut c_void>,
    pub destroy:     Option<unsafe extern "C" fn(*mut c_void)>,
    pub render:      Option<
        unsafe extern "C" fn(
            ctx:              *mut c_void,
            playback_pos_secs: c_double,
            is_active:         c_int,
            out:               *mut c_double,
            count:             c_uint,
        ),
    >,
}

unsafe impl Send for SparkVizPluginAbi_v1 {}
unsafe impl Sync for SparkVizPluginAbi_v1 {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Ensure the ABI version constant is what we expect.
    #[test]
    fn abi_version_is_2() {
        assert_eq!(SPARKAMP_PLUGIN_ABI_VERSION, 2);
    }

    /// SparkSettingType discriminants must not change — plugins compare them.
    #[test]
    fn setting_type_discriminants_are_stable() {
        assert_eq!(SparkSettingType::End    as u32, 0);
        assert_eq!(SparkSettingType::Bool   as u32, 1);
        assert_eq!(SparkSettingType::Int    as u32, 2);
        assert_eq!(SparkSettingType::Float  as u32, 3);
        assert_eq!(SparkSettingType::String as u32, 4);
        assert_eq!(SparkSettingType::Choice as u32, 5);
    }

    /// Plugin kind discriminants must not change.
    #[test]
    fn plugin_kind_discriminants_are_stable() {
        assert_eq!(SparkPluginKind::Visualizer as u32, 1);
        assert_eq!(SparkPluginKind::Filetype   as u32, 2);
    }
}
