// Core library — all business logic shared between the binary, TUI, and the
// macOS Swift bridge. No UI framework knowledge lives here.
pub mod config;
pub mod controller;
pub mod dedupe;
pub mod duration_cache;
pub mod duration_probe;
pub mod engine;
pub mod filetype_plugin;
pub mod granite;
pub mod id3_editor;
pub mod loaded_plugin;
pub mod media_library;
pub mod model;
pub mod plugin_abi;
pub mod plugin_manager;
pub mod plugin_settings;
pub mod shuffle;
pub mod skin;
pub mod viz_plugin;

// C FFI layer for the macOS Swift bridge. Always compiled; the functions are
// dead code on Linux but are pub extern "C" so the compiler doesn't warn.
pub mod ffi;
