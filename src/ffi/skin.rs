//! Built-in skin templates, so the macOS app stops carrying its own copies.
//!
//! The palette used to exist four times over: `SkinVars::*_defaults`, the
//! exported CSS templates, and two tables inside the macOS `Theme.swift`.
//! Nothing checked that they matched and they did not; commit 8c27c5e exists
//! only because a button-ramp fix reached two of the four.
//!
//! macOS already parses CSS, for the user skins in `~/.config/sparkamp/skins`
//! that both platforms read. Handing it the same template text the Linux build
//! exports leaves one copy of each built-in rather than three.

use std::ffi::{c_char, CStr, CString};

/// The CSS for a built-in skin: `"dark"` or `"light"`.
///
/// Returns null for any other name rather than guessing, so a caller that
/// mistypes gets an obvious failure instead of a silently wrong palette. Free
/// with `sparkamp_free_string`.
///
/// The text is exactly what "Download skin…" writes, which is the point: a
/// skin the user exports, edits and loads back is parsed by the same code on
/// both platforms.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_skin_template_css(name: *const c_char) -> *mut c_char {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(name) = CStr::from_ptr(name).to_str() else {
        return std::ptr::null_mut();
    };
    let css = match name {
        "dark" => crate::skin::DARK_TEMPLATE_CSS,
        "light" => crate::skin::LIGHT_TEMPLATE_CSS,
        _ => return std::ptr::null_mut(),
    };
    CString::new(css)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetch(name: &str) -> Option<String> {
        let c = CString::new(name).unwrap();
        let p = unsafe { sparkamp_skin_template_css(c.as_ptr()) };
        if p.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string();
        unsafe { crate::ffi::sparkamp_free_string(p) };
        Some(s)
    }

    /// Both built-ins cross whole, as the CSS the exporter writes.
    #[test]
    fn each_builtin_crosses_as_its_own_template() {
        let dark = fetch("dark").expect("dark is a built-in");
        let light = fetch("light").expect("light is a built-in");
        assert_eq!(dark, crate::skin::DARK_TEMPLATE_CSS);
        assert_eq!(light, crate::skin::LIGHT_TEMPLATE_CSS);
        assert_ne!(dark, light, "the two are not the same skin");
        // The palette is what the caller is after, so check one of it arrived
        // rather than only that some text did.
        assert!(dark.contains("--sp-background:"));
    }

    /// An unknown name, or none at all, answers null rather than a default.
    #[test]
    fn an_unknown_skin_name_returns_null() {
        assert!(fetch("chartreuse").is_none());
        assert!(unsafe { sparkamp_skin_template_css(std::ptr::null()) }.is_null());
    }
}
