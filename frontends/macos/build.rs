// Build script for the sparkamp-macos static library.
//
// sparkamp.h is maintained by hand in include/sparkamp.h and kept in sync with
// src/ffi.rs.  cbindgen was removed because Rust 2024's #[unsafe(no_mangle)]
// syntax is not yet recognised by cbindgen's attribute parser, causing it to
// silently omit all exported functions from the generated header.
//
// To add a new FFI function:
//   1. Add the Rust implementation to src/ffi.rs.
//   2. Add the corresponding C declaration to include/sparkamp.h.

fn main() {
    // Re-run this build script whenever the FFI source or header changes.
    println!("cargo:rerun-if-changed=../../src/ffi.rs");
    println!("cargo:rerun-if-changed=../../include/sparkamp.h");
}
