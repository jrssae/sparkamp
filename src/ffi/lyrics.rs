//! Lyrics-view FFI (F15) — one entry point exposing the whole core view to
//! macOS as a JSON blob. The fresh USLT read, marquee title, and search-URL
//! encoding all happen in core, not Swift, so nothing is re-derived per
//! frontend.
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;

/// Read an optional C string as an owned `String`; null / invalid UTF-8 → "".
unsafe fn opt_str(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    CStr::from_ptr(p).to_str().unwrap_or_default().to_owned()
}

/// Build the lyrics view for `path` and return it as a heap JSON string:
/// `{"title":..,"body":..,"has_body":bool,"search_url":..}` (`body` is "" when
/// there are no lyrics). Free with `sparkamp_free_string`. A null `path`
/// yields a null return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_lyrics_view(
    path: *const c_char,
    artist: *const c_char,
    title: *const c_char,
    album_artist: *const c_char,
) -> *mut c_char {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    let path_str = opt_str(path);
    let artist = opt_str(artist);
    let title = opt_str(title);
    let album_artist = opt_str(album_artist);

    let view = crate::lyrics::lyrics_view(Path::new(&path_str), &artist, &title, &album_artist);
    let json = serde_json::json!({
        "title": view.title,
        "body": view.body.clone().unwrap_or_default(),
        "has_body": view.body.is_some(),
        "search_url": view.search_url,
    })
    .to_string();

    CString::new(json)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn c(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    /// Parse the returned JSON and free the pointer.
    unsafe fn call_and_parse(
        path: &CString,
        artist: &CString,
        title: &CString,
        aa: &CString,
    ) -> serde_json::Value {
        let ptr = sparkamp_lyrics_view(path.as_ptr(), artist.as_ptr(), title.as_ptr(), aa.as_ptr());
        assert!(!ptr.is_null());
        let got = CStr::from_ptr(ptr).to_str().unwrap().to_owned();
        crate::ffi::sparkamp_free_string(ptr);
        serde_json::from_str(&got).unwrap()
    }

    #[test]
    fn view_json_has_body_true_with_uslt() {
        let mut f = tempfile::NamedTempFile::with_suffix(".mp3").unwrap();
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        let mut fields = crate::id3_editor::read_tag_fields(f.path());
        fields.lyric = "one\ntwo".to_string();
        crate::id3_editor::write_tag_fields(f.path(), &fields).unwrap();
        let path = c(f.path().to_str().unwrap());
        unsafe {
            let v = call_and_parse(&path, &c("A"), &c("T"), &c(""));
            assert_eq!(v["has_body"], true);
            assert_eq!(v["body"], "one\ntwo");
            assert_eq!(v["title"], "A - T");
        }
    }

    #[test]
    fn view_json_has_body_false_and_search_url() {
        let mut f = tempfile::NamedTempFile::with_suffix(".mp3").unwrap();
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        let path = c(f.path().to_str().unwrap());
        unsafe {
            let v = call_and_parse(&path, &c("Miles Davis"), &c("So What"), &c(""));
            assert_eq!(v["has_body"], false);
            assert_eq!(v["body"], "");
            assert_eq!(
                v["search_url"],
                "https://duckduckgo.com/?q=Miles%20Davis%20So%20What%20lyrics"
            );
        }
    }

    #[test]
    fn null_path_returns_null() {
        unsafe {
            let ptr = sparkamp_lyrics_view(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            );
            assert!(ptr.is_null());
        }
    }
}
