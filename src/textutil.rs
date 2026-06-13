//! Shared string-sanitisation helpers.
//!
//! Lives in its own module because both the playlist model and the media
//! library read tags from the same (potentially malformed) files and must
//! apply identical cleanup before strings reach a UI toolkit.

/// Remove NUL bytes from a string.
///
/// ID3 tags can contain malformed data with embedded NUL bytes.  These cause
/// crashes when passed to GTK APIs which use C-style NUL-terminated strings.
/// This function strips any NUL bytes so the string is safe for UI display.
pub(crate) fn sanitize(s: &str) -> String {
    // First, remove any actual NUL bytes
    let result = if s.contains('\0') {
        s.replace('\0', "")
    } else {
        s.to_owned()
    };
    // Also remove the TOML unicode escape for NUL (backslash-u-0000), which
    // appears as literal text after deserialization.
    if result.contains("\\u0000") {
        result.replace("\\u0000", "")
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_passes_through_normal_strings() {
        assert_eq!(sanitize("hello world"), "hello world");
        assert_eq!(sanitize(""), "");
        assert_eq!(sanitize("🎵 Artist — Album"), "🎵 Artist — Album");
    }

    #[test]
    fn sanitize_removes_nul_bytes() {
        assert_eq!(sanitize("hello\x00world"), "helloworld");
        assert_eq!(sanitize("\x00start"), "start");
        assert_eq!(sanitize("end\x00"), "end");
        assert_eq!(sanitize("\x00\x00\x00"), "");
    }

    #[test]
    fn sanitize_removes_toml_unicode_escape() {
        assert_eq!(sanitize("hello\\u0000world"), "helloworld");
        assert_eq!(sanitize("\\u0000start"), "start");
        assert_eq!(sanitize("end\\u0000"), "end");
        assert_eq!(sanitize("\\u0000"), "");
    }

    #[test]
    fn sanitize_handles_both_nul_and_toml_escape() {
        assert_eq!(sanitize("a\x00b\\u0000c"), "abc");
    }
}
