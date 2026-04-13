// Re-export every public symbol from the core FFI layer so they are included
// in the compiled staticlib and visible to cbindgen.
//
// All real implementation lives in sparkamp::ffi (src/ffi.rs in the root crate).
// This crate exists solely to produce libsparkamp_macos.a with crate-type =
// ["staticlib"] — a setting that cannot be placed on the root crate without
// breaking the Linux binary build.
pub use sparkamp::ffi::*;
