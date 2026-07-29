//! View/Search-lyrics FFI (F15) — one entry point exposing the whole core
//! decision to macOS. The fresh USLT read happens in core, not Swift, so the
//! Show-vs-Search branch and the search-URL encoding stay single-sourced.
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_uint};
use std::path::Path;

use crate::lyrics::{lyrics_action, LyricsAction};

/// Read an optional C string as an owned `String`; null / invalid UTF-8 → "".
unsafe fn opt_str(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    CStr::from_ptr(p).to_str().unwrap_or_default().to_owned()
}

/// Decide View-vs-Search lyrics for `path`. Writes the discriminator into
/// `*out_kind` (0 = show lyrics text, 1 = search URL) and returns a heap C
/// string with the lyrics body or the DuckDuckGo URL. Free the result with
/// `sparkamp_free_string`. A null `path` or `out_kind` yields a null return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_lyrics_action(
    path: *const c_char,
    artist: *const c_char,
    title: *const c_char,
    album_artist: *const c_char,
    out_kind: *mut c_uint,
) -> *mut c_char {
    if path.is_null() || out_kind.is_null() {
        return std::ptr::null_mut();
    }
    let path_str = opt_str(path);
    let artist = opt_str(artist);
    let title = opt_str(title);
    let album_artist = opt_str(album_artist);

    let (kind, body) = match lyrics_action(Path::new(&path_str), &artist, &title, &album_artist) {
        LyricsAction::Show(text) => (0u32, text),
        LyricsAction::Search(url) => (1u32, url),
    };

    *out_kind = kind;
    CString::new(body)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Round-trip a C string arg the way the FFI callers do.
    fn c(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    #[test]
    fn search_branch_returns_ddg_url_kind_1() {
        let mut f = tempfile::NamedTempFile::with_suffix(".mp3").unwrap();
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        let path = c(f.path().to_str().unwrap());
        let (artist, title, aa) = (c("Miles Davis"), c("So What"), c(""));
        let mut kind: c_uint = 99;
        unsafe {
            let ptr = sparkamp_lyrics_action(
                path.as_ptr(),
                artist.as_ptr(),
                title.as_ptr(),
                aa.as_ptr(),
                &mut kind,
            );
            assert!(!ptr.is_null());
            assert_eq!(kind, 1);
            let got = CStr::from_ptr(ptr).to_str().unwrap().to_owned();
            assert_eq!(got, "https://duckduckgo.com/?q=Miles%20Davis%20-%20So%20What%20lyrics");
            crate::ffi::sparkamp_free_string(ptr);
        }
    }

    #[test]
    fn show_branch_returns_lyrics_kind_0() {
        let mut f = tempfile::NamedTempFile::with_suffix(".mp3").unwrap();
        f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        let mut fields = crate::id3_editor::read_tag_fields(f.path());
        fields.lyric = "one\ntwo".to_string();
        crate::id3_editor::write_tag_fields(f.path(), &fields).unwrap();
        let path = c(f.path().to_str().unwrap());
        let (artist, title, aa) = (c("A"), c("T"), c(""));
        let mut kind: c_uint = 99;
        unsafe {
            let ptr = sparkamp_lyrics_action(
                path.as_ptr(),
                artist.as_ptr(),
                title.as_ptr(),
                aa.as_ptr(),
                &mut kind,
            );
            assert!(!ptr.is_null());
            assert_eq!(kind, 0);
            let got = CStr::from_ptr(ptr).to_str().unwrap().to_owned();
            assert_eq!(got, "one\ntwo");
            crate::ffi::sparkamp_free_string(ptr);
        }
    }

    #[test]
    fn null_path_returns_null() {
        let mut kind: c_uint = 99;
        unsafe {
            let ptr = sparkamp_lyrics_action(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                &mut kind,
            );
            assert!(ptr.is_null());
        }
    }
}
