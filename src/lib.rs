// Core library — all business logic shared between the binary, TUI, and the
// macOS Swift bridge. No UI framework knowledge lives here.
pub mod config;
pub mod controller;
#[cfg(target_os = "linux")]
pub mod crash_log;
pub mod dedupe;
pub mod duration_cache;
pub mod duration_probe;
pub mod engine;
pub mod devices;
pub mod granite;
pub mod id3_editor;
pub mod media_library;
pub mod model;
pub mod shuffle;
pub mod skin;
pub mod tags;
pub mod textutil;
pub mod timeutil;

// C FFI layer for the macOS Swift bridge. Always compiled; the functions are
// dead code on Linux but are pub extern "C" so the compiler doesn't warn.
pub mod ffi;
