//! ID3 tag editor — opaque `SparkampTagCtx` holding loaded tag fields plus
//! read/write accessors.
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::Path;

// ---------------------------------------------------------------------------
// ID3 Tag Editor
// ---------------------------------------------------------------------------

pub struct SparkampTagCtx {
    path: String,
    fields: crate::id3_editor::TagFields,
    extra_frames: Vec<crate::id3_editor::ExtraFrame>,
    artwork: Option<Vec<u8>>,
    /// Values set via sparkamp_tag_set for frames outside TagFields —
    /// written with write_extra_frame on save. This is what finally uses
    /// the extra-frame write path (B7) for the mac Customize fields (B2).
    pending_extra: Vec<(String, String)>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_open(path: *const c_char) -> *mut SparkampTagCtx {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    let path_str = match CStr::from_ptr(path).to_str() {
        Ok(s) => s.to_owned(),
        Err(_) => return std::ptr::null_mut(),
    };
    let path_buf = Path::new(&path_str);
    let fields = crate::id3_editor::read_tag_fields(path_buf);
    let extra_frames = crate::id3_editor::read_extra_frames(path_buf);
    let artwork = id3::Tag::read_from_path(path_buf)
        .ok()
        .and_then(|tag| tag.pictures().next().map(|p| p.data.clone()));
    let tag_ctx = SparkampTagCtx {
        path: path_str,
        fields,
        extra_frames,
        artwork,
        pending_extra: Vec::new(),
    };
    Box::into_raw(Box::new(tag_ctx))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_close(tag: *mut SparkampTagCtx) {
    if tag.is_null() {
        return;
    }
    drop(Box::from_raw(tag));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_get(
    tag: *const SparkampTagCtx,
    frame_id: *const c_char,
) -> *mut c_char {
    if tag.is_null() || frame_id.is_null() {
        return CString::new("").unwrap().into_raw();
    }
    let tag = &*tag;
    let frame = CStr::from_ptr(frame_id).to_string_lossy();
    let value = match frame.as_ref() {
        "TIT2" => &tag.fields.title,
        "TPE1" => &tag.fields.artist,
        "TALB" => &tag.fields.album,
        "TPE2" => &tag.fields.album_artist,
        "TCON" => &tag.fields.genre,
        "TDRC" => &tag.fields.year,
        "TRCK" => &tag.fields.track_number,
        "TPOS" => &tag.fields.disc_number,
        "TBPM" => &tag.fields.bpm,
        "COMM" => &tag.fields.comment,
        "TCOM" => &tag.fields.composer,
        "TOPE" => &tag.fields.original_artist,
        "TCOP" => &tag.fields.copyright,
        "WXXX" => &tag.fields.url,
        "TENC" => &tag.fields.encoded_by,
        "USLT" => &tag.fields.lyric,
        other => {
            // Pending writes win over what was read from disk.
            let v = tag
                .pending_extra
                .iter()
                .rev()
                .find(|(id, _)| id == other)
                .map(|(_, v)| v.as_str())
                .or_else(|| {
                    tag.extra_frames
                        .iter()
                        .find(|f| f.id == other)
                        .map(|f| f.value.as_str())
                })
                .unwrap_or("");
            return CString::new(v).unwrap_or_default().into_raw();
        }
    };
    CString::new(value.as_str()).unwrap_or_default().into_raw()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_set(
    tag: *mut SparkampTagCtx,
    frame_id: *const c_char,
    value: *const c_char,
) {
    if tag.is_null() || frame_id.is_null() || value.is_null() {
        return;
    }
    let tag = &mut *tag;
    let frame = CStr::from_ptr(frame_id).to_string_lossy();
    let val = CStr::from_ptr(value).to_string_lossy().into_owned();
    match frame.as_ref() {
        "TIT2" => tag.fields.title = val,
        "TPE1" => tag.fields.artist = val,
        "TALB" => tag.fields.album = val,
        "TPE2" => tag.fields.album_artist = val,
        "TCON" => tag.fields.genre = val,
        "TDRC" => tag.fields.year = val,
        "TRCK" => tag.fields.track_number = val,
        "TPOS" => tag.fields.disc_number = val,
        "TBPM" => tag.fields.bpm = val,
        "COMM" => tag.fields.comment = val,
        "TCOM" => tag.fields.composer = val,
        "TOPE" => tag.fields.original_artist = val,
        "TCOP" => tag.fields.copyright = val,
        "WXXX" => tag.fields.url = val,
        "TENC" => tag.fields.encoded_by = val,
        "USLT" => tag.fields.lyric = val,
        other if other.starts_with('T') => {
            tag.pending_extra.retain(|(id, _)| id != other);
            tag.pending_extra.push((other.to_string(), val));
        }
        _ => {}
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_frame_count(tag: *const SparkampTagCtx) -> c_int {
    if tag.is_null() {
        return 0;
    }
    let tag = &*tag;
    tag.extra_frames.len() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_frame_id(
    tag: *const SparkampTagCtx,
    index: c_int,
) -> *mut c_char {
    if tag.is_null() || index < 0 {
        return CString::new("").unwrap().into_raw();
    }
    let tag = &*tag;
    let idx = index as usize;
    if idx >= tag.extra_frames.len() {
        return CString::new("").unwrap().into_raw();
    }
    CString::new(tag.extra_frames[idx].id.as_str())
        .unwrap_or_default()
        .into_raw()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_frame_value(
    tag: *const SparkampTagCtx,
    index: c_int,
) -> *mut c_char {
    if tag.is_null() || index < 0 {
        return CString::new("").unwrap().into_raw();
    }
    let tag = &*tag;
    let idx = index as usize;
    if idx >= tag.extra_frames.len() {
        return CString::new("").unwrap().into_raw();
    }
    CString::new(tag.extra_frames[idx].value.as_str())
        .unwrap_or_default()
        .into_raw()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_save(tag: *mut SparkampTagCtx) -> c_int {
    if tag.is_null() {
        return -2;
    }
    let tag = &mut *tag;
    let path = Path::new(&tag.path);
    // Check if file is read-only
    match std::fs::metadata(path).map(|m| m.permissions().readonly()) {
        Ok(true) => return -1,
        Err(_) => return -1,
        Ok(false) => {}
    }
    match crate::id3_editor::write_tag_fields(path, &tag.fields) {
        Ok(_) => {}
        Err(_) => return -2,
    }
    for (id, value) in &tag.pending_extra {
        // write_extra_frame re-reads and rewrites the tag per frame; the
        // Customize panel tops out at a handful of frames, so that's fine.
        if crate::id3_editor::write_extra_frame(path, id, value).is_err() {
            return -2;
        }
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_get_artwork_data(
    tag: *const SparkampTagCtx,
    len_out: *mut c_int,
) -> *mut u8 {
    if tag.is_null() {
        return std::ptr::null_mut();
    }
    let tag = &*tag;
    match &tag.artwork {
        None => std::ptr::null_mut(),
        Some(bytes) => {
            if !len_out.is_null() {
                *len_out = bytes.len() as c_int;
            }
            let mut boxed: Box<[u8]> = bytes.clone().into_boxed_slice();
            let ptr = boxed.as_mut_ptr();
            std::mem::forget(boxed);
            ptr
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkamp_tag_free_artwork(ptr: *mut u8, len: c_int) {
    if ptr.is_null() {
        return;
    }
    drop(Box::from_raw(std::slice::from_raw_parts_mut(ptr, len as usize)));
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;

    // Round-trip a TagFields-backed frame and a passthrough frame through
    // the raw FFI surface the mac editor uses (B2/B7).
    #[test]
    fn ffi_extended_and_passthrough_roundtrip() {
        let path = std::env::temp_dir().join("sparkamp_ffi_tag_test.mp3");
        std::fs::write(&path, b"").unwrap();
        let c_path = CString::new(path.to_str().unwrap()).unwrap();

        unsafe {
            let ctx = super::sparkamp_tag_open(c_path.as_ptr());
            assert!(!ctx.is_null());
            let set = |ctx, id: &str, v: &str| {
                let id = CString::new(id).unwrap();
                let v = CString::new(v).unwrap();
                super::sparkamp_tag_set(ctx, id.as_ptr(), v.as_ptr());
            };
            set(ctx, "TCOM", "A Composer");
            set(ctx, "TPUB", "A Publisher"); // not in TagFields — passthrough
            assert_eq!(super::sparkamp_tag_save(ctx), 0);
            super::sparkamp_tag_close(ctx);

            let ctx2 = super::sparkamp_tag_open(c_path.as_ptr());
            let get = |ctx, id: &str| -> String {
                let id = CString::new(id).unwrap();
                let p = super::sparkamp_tag_get(ctx, id.as_ptr());
                let s = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
                crate::ffi::sparkamp_free_string(p);
                s
            };
            assert_eq!(get(ctx2, "TCOM"), "A Composer");
            assert_eq!(get(ctx2, "TPUB"), "A Publisher");
            super::sparkamp_tag_close(ctx2);
        }
        std::fs::remove_file(&path).ok();
    }
}

