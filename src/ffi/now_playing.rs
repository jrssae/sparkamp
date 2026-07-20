//! Now-playing info FFI — opaque `SparkampNowPlaying` handle wrapping a
//! `crate::now_playing::NowPlayingInfo` built for the current track, plus
//! getters mirroring the GTK A1 panel data (curated tags, tech line, artwork
//! path, play-count/last-played stats, wiki URLs).
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::CString;
use std::os::raw::{c_char, c_int};

use super::SparkampCtx;

/// Opaque handle — a snapshot of the current track's now-playing info.
///
/// Built once by `sparkamp_now_playing_open` (from the ctx's playlist +
/// media library) and read via the getters below.  Not `repr(C)` — it only
/// ever crosses FFI as a pointer.  Free with `sparkamp_now_playing_close`.
pub struct SparkampNowPlaying {
    info: crate::now_playing::NowPlayingInfo,
}

/// Build a now-playing snapshot for the CURRENT playlist track.
///
/// Returns null if there is no current track. Mirrors the GTK subscriber's
/// data path exactly: library row + play snapshot (if the media library is
/// open) feed `build_now_playing_info`, same as `crate::now_playing`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_open(
    ctx: *mut SparkampCtx,
) -> *mut SparkampNowPlaying {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = &*ctx;
    let Some(track) = ctx.playlist.current() else {
        return std::ptr::null_mut();
    };
    let path = track.path.clone();
    let path_str = path.to_string_lossy();
    let lib_row = ctx
        .media_library
        .as_ref()
        .and_then(|ml| ml.track_by_path(&path_str).ok());
    let snap = ctx
        .media_library
        .as_ref()
        .map(|ml| ml.play_snapshot(&path_str))
        .unwrap_or_default();
    let info = crate::now_playing::build_now_playing_info(&path, lib_row.as_ref(), snap);
    Box::into_raw(Box::new(SparkampNowPlaying { info }))
}

/// Free a handle returned by `sparkamp_now_playing_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_close(np: *mut SparkampNowPlaying) {
    if np.is_null() {
        return;
    }
    drop(Box::from_raw(np));
}

/// Number of curated, non-empty tag rows.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_tag_count(np: *const SparkampNowPlaying) -> c_int {
    if np.is_null() {
        return 0;
    }
    (&*np).info.tags.len() as c_int
}

/// Label of tag row `i` (e.g. "Title", "Artist"). Empty string if out of range.
/// Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_tag_label(
    np: *const SparkampNowPlaying,
    i: c_int,
) -> *mut c_char {
    if np.is_null() || i < 0 {
        return CString::new("").unwrap().into_raw();
    }
    let np = &*np;
    match np.info.tags.get(i as usize) {
        Some((label, _)) => CString::new(*label).unwrap_or_default().into_raw(),
        None => CString::new("").unwrap().into_raw(),
    }
}

/// Value of tag row `i`. Empty string if out of range. Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_tag_value(
    np: *const SparkampNowPlaying,
    i: c_int,
) -> *mut c_char {
    if np.is_null() || i < 0 {
        return CString::new("").unwrap().into_raw();
    }
    let np = &*np;
    match np.info.tags.get(i as usize) {
        Some((_, value)) => CString::new(value.as_str()).unwrap_or_default().into_raw(),
        None => CString::new("").unwrap().into_raw(),
    }
}

/// e.g. "MP3 · 320kbps · 44.1kHz · Stereo · 3:45"; empty if nothing probed.
/// Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_tech_line(
    np: *const SparkampNowPlaying,
) -> *mut c_char {
    if np.is_null() {
        return CString::new("").unwrap().into_raw();
    }
    CString::new((&*np).info.tech_line.as_str())
        .unwrap_or_default()
        .into_raw()
}

/// Path to the resolved artwork file (embedded APIC dump, folder image, or
/// library-cached path); "" if none. Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_artwork_path(
    np: *const SparkampNowPlaying,
) -> *mut c_char {
    if np.is_null() {
        return CString::new("").unwrap().into_raw();
    }
    let np = &*np;
    let s = np
        .info
        .artwork_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    CString::new(s).unwrap_or_default().into_raw()
}

/// 1 if the track has a play-count (i.e. is indexed in the media library); 0 otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_has_play_count(
    np: *const SparkampNowPlaying,
) -> c_int {
    if np.is_null() {
        return 0;
    }
    if (&*np).info.play_count.is_some() {
        1
    } else {
        0
    }
}

/// Play count; 0 when `sparkamp_now_playing_has_play_count` is 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_play_count(np: *const SparkampNowPlaying) -> i64 {
    if np.is_null() {
        return 0;
    }
    (&*np).info.play_count.unwrap_or(0)
}

/// ISO-8601 UTC last-played timestamp, or "" if never played / unindexed.
/// Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_last_played(
    np: *const SparkampNowPlaying,
) -> *mut c_char {
    if np.is_null() {
        return CString::new("").unwrap().into_raw();
    }
    let s = (&*np).info.last_played.clone().unwrap_or_default();
    CString::new(s).unwrap_or_default().into_raw()
}

/// Wikipedia search URL for the artist tag, or "" if the artist is empty.
/// Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_artist_wiki_url(
    np: *const SparkampNowPlaying,
) -> *mut c_char {
    if np.is_null() {
        return CString::new("").unwrap().into_raw();
    }
    let s = (&*np).info.artist_wiki_url.clone().unwrap_or_default();
    CString::new(s).unwrap_or_default().into_raw()
}

/// Wikipedia search URL for the album tag, or "" if the album is empty.
/// Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_album_wiki_url(
    np: *const SparkampNowPlaying,
) -> *mut c_char {
    if np.is_null() {
        return CString::new("").unwrap().into_raw();
    }
    let s = (&*np).info.album_wiki_url.clone().unwrap_or_default();
    CString::new(s).unwrap_or_default().into_raw()
}
