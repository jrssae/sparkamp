//! JSON and string marshalling shared by the FFI modules. Each module that
//! talks JSON with Swift used to carry its own copy of these.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};

/// `v` as a JSON C string the caller frees with `sparkamp_free_string`, or
/// null if it cannot be serialized.
pub(super) fn json_out<T: serde::Serialize>(v: &T) -> *mut c_char {
    match serde_json::to_string(v) {
        Ok(s) => CString::new(s).map(|c| c.into_raw()).unwrap_or(std::ptr::null_mut()),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Decode a JSON payload from the frontend.
///
/// A failure is reported rather than swallowed. This used to return `None` for
/// a null pointer, a non-UTF-8 payload and a shape mismatch alike, so every
/// rejected job cost a round trip with the user to find out which. The
/// frontend still only learns "rejected"; the log says why.
pub(super) unsafe fn json_in<T: for<'de> serde::Deserialize<'de>>(p: *const c_char) -> Option<T> {
    if p.is_null() {
        return None;
    }
    let s = match CStr::from_ptr(p).to_str() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sparkamp: {} payload is not UTF-8: {e}", std::any::type_name::<T>());
            return None;
        }
    };
    match serde_json::from_str(s) {
        Ok(v) => Some(v),
        Err(e) => {
            // The prefix, not the whole payload: a burn job carries every
            // queued path and the interesting part is always near the front.
            let head: String = s.chars().take(400).collect();
            eprintln!(
                "sparkamp: could not read {} from JSON: {e}\n  payload starts: {head}",
                std::any::type_name::<T>()
            );
            None
        }
    }
}

/// A C string argument as `&str`, or `None` for null or non-UTF-8.
pub(super) unsafe fn str_in<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok()
}

/// A C array of `count` C strings as owned strings, skipping null and
/// non-UTF-8 entries. Empty for a null array or a count of zero or less.
pub(super) unsafe fn strings_in(items: *const *const c_char, count: c_int) -> Vec<String> {
    if items.is_null() || count <= 0 {
        return Vec::new();
    }
    std::slice::from_raw_parts(items, count as usize)
        .iter()
        .filter(|p| !p.is_null())
        .filter_map(|&p| CStr::from_ptr(p).to_str().ok().map(str::to_owned))
        .collect()
}
