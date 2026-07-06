//! JSON-over-FFI optical-disc API for the macOS frontend.
//!
//! Mirrors the device-sync FFI conventions: UTF-8 JSON through `*mut c_char`
//! (freed with [`super::sparkamp_free_string`]), ctx-free so Swift can call
//! from a background queue (detection runs `drutil`/`plutil` subprocesses —
//! never block the UI thread on them).
//!
//! All disc logic lives in `crate::disc`; this file only drives it. Phase 1
//! exposes drive enumeration + per-track playlist entries; later phases add
//! gnudb, rip, and burn entry points here.
#![allow(unsafe_op_in_unsafe_fn)]

use std::os::raw::c_char;

use crate::disc::{detect, toc, OpticalDrive};

use super::SparkampCtx;

// Reuse the JSON helpers' conventions rather than the helpers themselves —
// they're private to `devices.rs`; the pair below is identical in behaviour.

fn json_out<T: serde::Serialize>(v: &T) -> *mut c_char {
    match serde_json::to_string(v) {
        Ok(s) => std::ffi::CString::new(s)
            .map(|c| c.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        Err(_) => std::ptr::null_mut(),
    }
}

unsafe fn json_in<T: for<'de> serde::Deserialize<'de>>(p: *const c_char) -> Option<T> {
    if p.is_null() {
        return None;
    }
    let s = std::ffi::CStr::from_ptr(p).to_str().ok()?;
    serde_json::from_str(s).ok()
}

/// Enumerate every optical drive with its loaded-media state and (for an
/// audio CD) the TOC. Returns a JSON array of `OpticalDrive`. Runs
/// subprocesses — call on a background queue and throttle polling.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_list_drives(_ctx: *mut SparkampCtx) -> *mut c_char {
    json_out(&detect::list_drives())
}

/// Playlist-ready entries (path/URI + "Track N" title + duration) for every
/// audio track on the given drive's disc. Takes the `OpticalDrive` JSON as
/// returned by `sparkamp_disc_list_drives`; returns a JSON array of
/// `DiscTrackEntry` (empty array when the drive has no audio disc).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_track_entries(
    _ctx: *mut SparkampCtx,
    drive_json: *const c_char,
) -> *mut c_char {
    let Some(drive): Option<OpticalDrive> = json_in(drive_json) else {
        return json_out(&Vec::<crate::disc::DiscTrackEntry>::new());
    };
    json_out(&toc::track_entries(&drive))
}

/// Result wrapper for the gnudb calls: exactly one of `ok`/`error` is set, so
/// Swift can branch without exceptions. `ok` carries call-specific JSON.
#[derive(serde::Serialize)]
struct GnudbResult<T: serde::Serialize> {
    ok: Option<T>,
    error: Option<String>,
}

fn gnudb_out<T: serde::Serialize>(r: Result<T, crate::disc::gnudb::GnudbError>) -> *mut c_char {
    match r {
        Ok(v) => json_out(&GnudbResult {
            ok: Some(v),
            error: None,
        }),
        Err(e) => json_out(&GnudbResult::<T> {
            ok: None,
            error: Some(e.to_string()),
        }),
    }
}

unsafe fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    std::ffi::CStr::from_ptr(p)
        .to_str()
        .ok()
        .map(|s| s.to_string())
}

/// The freedb disc ID (8 hex chars) for a `DiscToc` JSON — the stable key the
/// frontends use for per-disc tag overrides. Pure math; safe anywhere. Free
/// with `sparkamp_free_string`; null on bad input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_disc_id(
    _ctx: *mut SparkampCtx,
    toc_json: *const c_char,
) -> *mut c_char {
    let Some(disc_toc): Option<crate::disc::DiscToc> = json_in(toc_json) else {
        return std::ptr::null_mut();
    };
    std::ffi::CString::new(crate::disc::discid::freedb_discid(&disc_toc))
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Ask gnudb which discs match a TOC. Takes the `DiscToc` JSON (from an
/// `OpticalDrive.toc`) and the configured email; returns
/// `{"ok":[DiscMatch…]}` or `{"error":"…"}`. Blocking network call (10 s
/// timeout) — background queue only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_gnudb_query(
    _ctx: *mut SparkampCtx,
    toc_json: *const c_char,
    email: *const c_char,
) -> *mut c_char {
    let (Some(disc_toc), Some(email)): (Option<crate::disc::DiscToc>, _) =
        (json_in(toc_json), cstr(email))
    else {
        return gnudb_out::<Vec<crate::disc::gnudb::DiscMatch>>(Err(
            crate::disc::gnudb::GnudbError::Protocol("bad arguments".into()),
        ));
    };
    gnudb_out(crate::disc::gnudb::query(&disc_toc, &email))
}

/// Fetch one matched entry and parse it: returns `{"ok":XmcdEntry}` or
/// `{"error":"…"}`. Blocking network call — background queue only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_gnudb_read(
    _ctx: *mut SparkampCtx,
    category: *const c_char,
    discid: *const c_char,
    email: *const c_char,
) -> *mut c_char {
    let (Some(category), Some(discid), Some(email)) = (cstr(category), cstr(discid), cstr(email))
    else {
        return gnudb_out::<crate::disc::xmcd::XmcdEntry>(Err(
            crate::disc::gnudb::GnudbError::Protocol("bad arguments".into()),
        ));
    };
    let entry = crate::disc::gnudb::read(&category, &discid, &email).and_then(|text| {
        crate::disc::xmcd::parse(&text).ok_or_else(|| {
            crate::disc::gnudb::GnudbError::Protocol("unparseable xmcd entry".into())
        })
    });
    gnudb_out(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CStr, CString};

    #[test]
    fn track_entries_round_trip() {
        let drive = OpticalDrive {
            id: "/dev/sr0".into(),
            label: "TEST".into(),
            media: crate::disc::MediaInfo {
                present: true,
                is_audio_cd: true,
                ..crate::disc::MediaInfo::none()
            },
            toc: Some(crate::disc::DiscToc {
                tracks: vec![
                    crate::disc::TocTrack {
                        number: 1,
                        start_frame: 150,
                        is_audio: true,
                    },
                    crate::disc::TocTrack {
                        number: 2,
                        start_frame: 7650,
                        is_audio: true,
                    },
                ],
                leadout_frame: 15150,
            }),
            mount_path: None,
        };
        let arg = CString::new(serde_json::to_string(&drive).unwrap()).unwrap();
        let out = unsafe { sparkamp_disc_track_entries(std::ptr::null_mut(), arg.as_ptr()) };
        assert!(!out.is_null());
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        unsafe { super::super::sparkamp_free_string(out) };
        let entries: Vec<crate::disc::DiscTrackEntry> = serde_json::from_str(&s).unwrap();
        // On macOS entries need a mounted volume to resolve AIFF paths, so a
        // TOC-only drive yields none there; on other platforms cdda:// URIs
        // are synthesized straight from the TOC.
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].path, "cdda://1?device=/dev/sr0");
            assert_eq!(entries[0].duration_secs, 100);
        }
        #[cfg(target_os = "macos")]
        assert!(entries.is_empty());
    }

    #[test]
    fn bad_drive_json_yields_empty_array() {
        let arg = CString::new("not json").unwrap();
        let out = unsafe { sparkamp_disc_track_entries(std::ptr::null_mut(), arg.as_ptr()) };
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        unsafe { super::super::sparkamp_free_string(out) };
        assert_eq!(s, "[]");
    }
}
