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

/// Number of discrete technical rows (Format / Bitrate / Sample rate /
/// Channels / File size / ReplayGain), non-empty only.
///
/// These are the same fields `tech_line` concatenates, but as label/value
/// pairs so a panel can lay them out like the tag rows — which is what the
/// GTK A1 carousel does. `tech_line` stays for the single-line consumers
/// (TUI, MPRIS).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_technical_count(
    np: *const SparkampNowPlaying,
) -> c_int {
    if np.is_null() {
        return 0;
    }
    (&*np).info.technical.len() as c_int
}

/// Label of technical row `i` (e.g. "Format", "Bitrate"). Empty string if out
/// of range. Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_technical_label(
    np: *const SparkampNowPlaying,
    i: c_int,
) -> *mut c_char {
    if np.is_null() || i < 0 {
        return CString::new("").unwrap().into_raw();
    }
    let np = &*np;
    match np.info.technical.get(i as usize) {
        Some((label, _)) => CString::new(*label).unwrap_or_default().into_raw(),
        None => CString::new("").unwrap().into_raw(),
    }
}

/// Value of technical row `i`. Empty string if out of range. Free with
/// `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_technical_value(
    np: *const SparkampNowPlaying,
    i: c_int,
) -> *mut c_char {
    if np.is_null() || i < 0 {
        return CString::new("").unwrap().into_raw();
    }
    let np = &*np;
    match np.info.technical.get(i as usize) {
        Some((_, value)) => CString::new(value.as_str()).unwrap_or_default().into_raw(),
        None => CString::new("").unwrap().into_raw(),
    }
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

/// ISO-8601 timestamp of the last metadata scan, or "" if unindexed.
/// Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_last_scanned(
    np: *const SparkampNowPlaying,
) -> *mut c_char {
    if np.is_null() {
        return CString::new("").unwrap().into_raw();
    }
    let s = (&*np).info.last_scanned.clone().unwrap_or_default();
    CString::new(s).unwrap_or_default().into_raw()
}

/// ISO-8601 timestamp the file first entered the library, or "" if unindexed.
/// Free with `sparkamp_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_now_playing_added_at(
    np: *const SparkampNowPlaying,
) -> *mut c_char {
    if np.is_null() {
        return CString::new("").unwrap().into_raw();
    }
    let s = (&*np).info.added_at.clone().unwrap_or_default();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Playlist, Track};
    use std::sync::mpsc;

    /// Minimal `SparkampCtx` for FFI unit tests — mirrors
    /// `ffi::settings::tests::test_ctx`, with one current-playlist track and
    /// `media_library` left `None` (the F12.3 `skip_db_load` pre-open state).
    fn test_ctx_with_track() -> SparkampCtx {
        #[cfg(not(target_os = "macos"))]
        gstreamer::init().expect("GStreamer must be available for tests");
        let (meta_tx, meta_rx) = mpsc::channel();
        let (duration_tx, duration_rx) = mpsc::channel();
        let mut playlist = Playlist::new();
        playlist.add(Track {
            path: std::path::PathBuf::from("/nonexistent/skip-db-load-test-track.mp3"),
            title: "Test Track".to_string(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            duration: None,
            broken: false,
            read_only: false,
            id: 0,
        });
        SparkampCtx {
            player: crate::engine::Player::new().expect("Player::new"),
            playlist,
            config: crate::config::Config::default(),
            shuffle_state: crate::shuffle::ShuffleState::new(),
            queue: crate::queue::Queue::new(),
            meta_tx,
            meta_rx,
            duration_tx,
            duration_rx,
            dirty_count: 0,
            last_known_duration: None,
            pending_seek: None,
            eos_cb: None,
            eos_userdata: std::ptr::null_mut(),
            error_cb: None,
            error_userdata: std::ptr::null_mut(),
            position_cb: None,
            position_userdata: std::ptr::null_mut(),
            media_library: None,
            ml_progress: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            ml_scanning: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            ml_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rg_progress: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            rg_running: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rg_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            watch: None,
            watch_rx: None,
        }
    }

    /// F12.3 `skip_db_load`: the A1 now-playing stats read must tolerate a
    /// not-yet-open media library gracefully — no crash, and `has_play_count`
    /// reports false (the GTK/mac panels render em-dashes for that case)
    /// rather than a bogus zero-means-real-zero value.
    #[test]
    fn now_playing_open_is_none_safe_when_media_lib_closed() {
        let mut ctx = test_ctx_with_track();
        assert!(ctx.media_library.is_none());
        let np = unsafe { sparkamp_now_playing_open(&mut ctx) };
        assert!(!np.is_null());
        unsafe {
            assert_eq!(sparkamp_now_playing_has_play_count(np), 0);
            assert_eq!(sparkamp_now_playing_play_count(np), 0);
            let lp = sparkamp_now_playing_last_played(np);
            assert_eq!(std::ffi::CStr::from_ptr(lp).to_str().unwrap(), "");
            crate::ffi::sparkamp_free_string(lp);
            sparkamp_now_playing_close(np);
        }
    }
}
