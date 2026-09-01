//! DiscRecording.framework, called in-process. macOS only.
//!
//! This replaces the `drutil` subprocesses the detector used to run. App
//! Sandbox blocks spawning `/usr/bin/drutil`, and the Mac App Store build has
//! to reach the framework directly — which costs nothing, because `drutil` is
//! itself a thin CLI over this framework.
//!
//! ## Which half of the framework
//!
//! DiscRecording ships a C-over-CoreFoundation API (`DRCore*`) and an
//! Objective-C one. This binds the C half, so there are no message sends and
//! no hand-written class declarations — with one exception the platform
//! forces. `DRCDTextBlockCreateArrayFromPackList` is broken on macOS 26.6:
//! `DRCDTextBlockGetTypeID()` answers 0, and the array the function returns is
//! not a valid object, so the first `CFArrayGetCount` on it segfaults inside
//! `objc_msgSend`. Its Objective-C counterpart,
//! `+[DRCDTextBlock arrayOfCDTextBlocksFromPacks:]`, works, and the C
//! accessors read the blocks it produces perfectly well — so exactly one
//! message send stands between the raw PACKs and CoreFoundation values.
//!
//! ## Where the CD-TEXT bytes come from
//!
//! The framework has no public "read CD-TEXT off this drive" call. (There is
//! an exported but undeclared `DRDeviceReadCDText`; App Review treats
//! undeclared symbols as private API, so it is not an option.) The documented
//! source is the one `DRCDTextBlockCreateArrayFromPackList` names itself: the
//! `DKIOCCDREADTOC` ioctl with format 5, against the media's raw BSD device.
//! `DRDeviceCopyStatus` hands over the BSD name, so no path guessing is
//! needed.

use std::ffi::{c_void, CString};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_core_foundation::{
    CFArray, CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};

use super::MediaKind;
use super::detect::MediaStatus;

/// An opaque `DRDeviceRef`. It is a CoreFoundation object, so `CFType` carries
/// it with the right retain/release semantics.
type DeviceRef = *const CFType;

#[link(name = "DiscRecording", kind = "framework")]
unsafe extern "C" {
    fn DRCopyDeviceArray() -> *const CFArray<CFType>;
    fn DRDeviceCopyInfo(device: DeviceRef) -> *const CFDictionary<CFString, CFType>;
    fn DRDeviceCopyStatus(device: DeviceRef) -> *const CFDictionary<CFString, CFType>;
    fn DRDeviceEjectMedia(device: DeviceRef) -> i32;
    fn DRCDTextBlockGetTrackDictionaries(
        block: *const CFType,
    ) -> *const CFArray<CFDictionary<CFString, CFType>>;

    static kDRDeviceVendorNameKey: Option<&'static CFString>;
    static kDRDeviceProductNameKey: Option<&'static CFString>;
    static kDRDeviceWriteCapabilitiesKey: Option<&'static CFString>;
    static kDRDeviceCanWriteKey: Option<&'static CFString>;

    static kDRDeviceMediaStateKey: Option<&'static CFString>;
    static kDRDeviceMediaStateMediaPresent: Option<&'static CFString>;
    static kDRDeviceMediaInfoKey: Option<&'static CFString>;
    static kDRDeviceMediaBSDNameKey: Option<&'static CFString>;
    static kDRDeviceMediaIsBlankKey: Option<&'static CFString>;
    static kDRDeviceMediaIsErasableKey: Option<&'static CFString>;
    static kDRDeviceMediaIsOverwritableKey: Option<&'static CFString>;
    static kDRDeviceMediaBlocksFreeKey: Option<&'static CFString>;
    static kDRDeviceMediaBlocksUsedKey: Option<&'static CFString>;
    static kDRDeviceMediaTrackCountKey: Option<&'static CFString>;
    static kDRDeviceMediaTypeKey: Option<&'static CFString>;

    static kDRDeviceMediaTypeCDR: Option<&'static CFString>;
    static kDRDeviceMediaTypeCDRW: Option<&'static CFString>;
    static kDRDeviceMediaTypeDVDR: Option<&'static CFString>;
    static kDRDeviceMediaTypeDVDRDualLayer: Option<&'static CFString>;
    static kDRDeviceMediaTypeDVDPlusR: Option<&'static CFString>;
    static kDRDeviceMediaTypeDVDPlusRDoubleLayer: Option<&'static CFString>;
    static kDRDeviceMediaTypeDVDRW: Option<&'static CFString>;
    static kDRDeviceMediaTypeDVDRWDualLayer: Option<&'static CFString>;
    static kDRDeviceMediaTypeDVDPlusRW: Option<&'static CFString>;
    static kDRDeviceMediaTypeDVDPlusRWDoubleLayer: Option<&'static CFString>;
    static kDRDeviceMediaTypeDVDRAM: Option<&'static CFString>;

    static kDRCDTextTitleKey: Option<&'static CFString>;
    static kDRCDTextPerformerKey: Option<&'static CFString>;
}

// ---------------------------------------------------------------------------
// CoreFoundation reading helpers
// ---------------------------------------------------------------------------

/// Look one key up in a `CFDictionary`. Borrowed, per CoreFoundation's Get
/// rule — the dictionary owns what comes back.
fn lookup<'a>(
    dict: &'a CFDictionary<CFString, CFType>,
    key: Option<&'static CFString>,
) -> Option<&'a CFType> {
    let key = key?;
    // SAFETY: the dictionary's keys really are CFStrings and `key` is one of
    // the framework's own constants, so the Get-rule borrow is valid for as
    // long as the dictionary lives.
    unsafe { dict.get_unchecked(key) }
}

fn sub_dict<'a>(
    dict: &'a CFDictionary<CFString, CFType>,
    key: Option<&'static CFString>,
) -> Option<&'a CFDictionary<CFString, CFType>> {
    let value = lookup(dict, key)?;
    // SAFETY: checked to be a dictionary before the cast; the generic
    // parameters only describe how its contents are read back.
    value
        .downcast_ref::<CFDictionary>()
        .map(|d| unsafe { &*(d as *const CFDictionary).cast::<CFDictionary<CFString, CFType>>() })
}

fn string(dict: &CFDictionary<CFString, CFType>, key: Option<&'static CFString>) -> Option<String> {
    Some(lookup(dict, key)?.downcast_ref::<CFString>()?.to_string())
}

fn boolean(dict: &CFDictionary<CFString, CFType>, key: Option<&'static CFString>) -> bool {
    lookup(dict, key)
        .and_then(|v| v.downcast_ref::<CFBoolean>())
        .map(CFBoolean::as_bool)
        .unwrap_or(false)
}

fn number(dict: &CFDictionary<CFString, CFType>, key: Option<&'static CFString>) -> Option<u64> {
    let n = lookup(dict, key)?.downcast_ref::<CFNumber>()?.as_i64()?;
    u64::try_from(n).ok()
}

/// Whether a dictionary value is one particular framework constant. Identity
/// against the exported `CFString`, not a match on its text: the constants are
/// the framework's own vocabulary and comparing them is not parsing.
fn is_constant(
    dict: &CFDictionary<CFString, CFType>,
    key: Option<&'static CFString>,
    constant: Option<&'static CFString>,
) -> bool {
    match (lookup(dict, key).and_then(|v| v.downcast_ref::<CFString>()), constant) {
        (Some(value), Some(wanted)) => value == wanted,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

/// One optical drive, as the framework sees it.
pub struct Device {
    raw: CFRetained<CFType>,
}

/// Every optical drive attached, in the framework's own order.
///
/// That order is also `drutil`'s: its `-drive N` is a 1-based index into the
/// same device array, which is what keeps [`super::OpticalDrive::id`] meaning
/// exactly what it meant when this list came from `drutil list`.
///
/// The first call in a process costs ~350 ms because it starts the
/// DiscRecording engine; every later call is well under a millisecond, since
/// the engine keeps the list current from device notifications.
pub fn devices() -> Vec<Device> {
    // SAFETY: takes no arguments and returns a +1 CFArray (Copy rule) or NULL.
    let array = unsafe { DRCopyDeviceArray() };
    let Some(array) = std::ptr::NonNull::new(array.cast_mut()) else {
        return Vec::new();
    };
    let array = unsafe { CFRetained::from_raw(array) };
    (0..array.len())
        .filter_map(|i| array.get(i).map(|raw| Device { raw }))
        .collect()
}

/// The drive at a 1-based enumeration index, i.e. an [`super::OpticalDrive::id`].
pub fn device_at_id(drive_id: &str) -> Option<Device> {
    let index: usize = drive_id.parse().ok()?;
    let mut all = devices();
    if index == 0 || index > all.len() {
        return None;
    }
    Some(all.remove(index - 1))
}

impl Device {
    fn as_ref(&self) -> DeviceRef {
        CFRetained::as_ptr(&self.raw).as_ptr()
    }

    fn info(&self) -> Option<CFRetained<CFDictionary<CFString, CFType>>> {
        // SAFETY: `raw` is a live DRDeviceRef; the dictionary comes back +1.
        let d = unsafe { DRDeviceCopyInfo(self.as_ref()) };
        std::ptr::NonNull::new(d.cast_mut()).map(|d| unsafe { CFRetained::from_raw(d) })
    }

    /// "Vendor Product", e.g. "Slimtype DVD A  DS8A5SH" — the same label
    /// `drutil list` printed, from the same two fields.
    pub fn label(&self) -> Option<String> {
        let info = self.info()?;
        let vendor = string(&info, unsafe { kDRDeviceVendorNameKey }).unwrap_or_default();
        let product = string(&info, unsafe { kDRDeviceProductNameKey }).unwrap_or_default();
        let label = format!("{} {}", vendor.trim(), product.trim());
        let label = label.trim().to_string();
        (!label.is_empty()).then_some(label)
    }

    /// Whether the hardware can burn anything at all, independent of the disc
    /// in it. Answering `true` when the framework will not say is the same
    /// fallback Linux uses: a drive that burns must not lose its burn panel
    /// because a probe went quiet.
    pub fn can_write(&self) -> bool {
        let Some(info) = self.info() else { return true };
        match sub_dict(&info, unsafe { kDRDeviceWriteCapabilitiesKey }) {
            Some(caps) => boolean(&caps, unsafe { kDRDeviceCanWriteKey }),
            None => true,
        }
    }

    /// What is in the drive right now.
    pub(crate) fn status(&self) -> MediaStatus {
        // SAFETY: `raw` is a live DRDeviceRef; the dictionary comes back +1.
        let raw = unsafe { DRDeviceCopyStatus(self.as_ref()) };
        let Some(raw) = std::ptr::NonNull::new(raw.cast_mut()) else {
            return MediaStatus::default();
        };
        let status = unsafe { CFRetained::from_raw(raw) };

        let present = is_constant(&status, unsafe { kDRDeviceMediaStateKey }, unsafe {
            kDRDeviceMediaStateMediaPresent
        });
        let Some(media) = sub_dict(&status, unsafe { kDRDeviceMediaInfoKey }) else {
            return MediaStatus::default();
        };

        MediaStatus {
            present,
            kind: media_kind(media),
            is_blank: boolean(media, unsafe { kDRDeviceMediaIsBlankKey }),
            is_erasable: boolean(media, unsafe { kDRDeviceMediaIsErasableKey }),
            is_overwritable: boolean(media, unsafe { kDRDeviceMediaIsOverwritableKey }),
            free_blocks: number(media, unsafe { kDRDeviceMediaBlocksFreeKey }),
            used_blocks: number(media, unsafe { kDRDeviceMediaBlocksUsedKey }),
            tracks: number(media, unsafe { kDRDeviceMediaTrackCountKey }).map(|n| n as u32),
            device_node: string(media, unsafe { kDRDeviceMediaBSDNameKey })
                .map(|n| format!("/dev/{n}")),
        }
    }

    /// Open the tray / spit the disc out. Blocking, and the OS refuses while
    /// anything is reading the drive.
    pub fn eject(&self) -> Result<(), String> {
        // SAFETY: `raw` is a live DRDeviceRef; the call returns an OSStatus.
        let status = unsafe { DRDeviceEjectMedia(self.as_ref()) };
        if status == 0 {
            Ok(())
        } else {
            Err(format!("the drive refused to eject (OSStatus {status})"))
        }
    }
}

/// Map the media-type constant onto the app's [`MediaKind`]. A pressed disc
/// (CD-ROM, DVD-ROM, and the BD/HD-DVD types) has no writable kind and reads
/// as `Unknown`, exactly as it did through `drutil`'s "CD-ROM" text.
fn media_kind(media: &CFDictionary<CFString, CFType>) -> MediaKind {
    let type_key = unsafe { kDRDeviceMediaTypeKey };
    let is = |c: Option<&'static CFString>| is_constant(media, type_key, c);
    if is(unsafe { kDRDeviceMediaTypeCDR }) {
        MediaKind::CdR
    } else if is(unsafe { kDRDeviceMediaTypeCDRW }) {
        MediaKind::CdRw
    } else if is(unsafe { kDRDeviceMediaTypeDVDRAM }) {
        MediaKind::DvdRam
    } else if is(unsafe { kDRDeviceMediaTypeDVDRW })
        || is(unsafe { kDRDeviceMediaTypeDVDRWDualLayer })
        || is(unsafe { kDRDeviceMediaTypeDVDPlusRW })
        || is(unsafe { kDRDeviceMediaTypeDVDPlusRWDoubleLayer })
    {
        MediaKind::DvdRw
    } else if is(unsafe { kDRDeviceMediaTypeDVDR })
        || is(unsafe { kDRDeviceMediaTypeDVDRDualLayer })
        || is(unsafe { kDRDeviceMediaTypeDVDPlusR })
        || is(unsafe { kDRDeviceMediaTypeDVDPlusRDoubleLayer })
    {
        MediaKind::DvdR
    } else {
        MediaKind::Unknown
    }
}

// ---------------------------------------------------------------------------
// CD-TEXT
// ---------------------------------------------------------------------------

// One entry of a CD-TEXT block's track array. The type lives in `cdtext`
// rather than here so the fold that consumes it stays platform-neutral and
// testable on Linux, where this module does not exist.
pub use crate::disc::cdtext::BlockTrack;

/// `struct dk_cd_read_toc_t` from `<IOKit/storage/IOCDMediaBSDClient.h>`,
/// 64-bit layout.
#[repr(C)]
struct CdReadToc {
    format: u8,
    format_as_time: u8,
    reserved_16: [u8; 5],
    address: u8,
    reserved_64: [u8; 6],
    buffer_length: u16,
    buffer: *mut c_void,
}

/// `_IOWR('d', 100, dk_cd_read_toc_t)`, spelled out because `libc` has no
/// `_IOWR` macro. Direction bits (in|out) sit at the top, then the payload
/// size, the group character and the command number.
const DKIOCCDREADTOC: libc::c_ulong = {
    let size = (std::mem::size_of::<CdReadToc>() as libc::c_ulong) & 0x1fff;
    0x8000_0000 | 0x4000_0000 | (size << 16) | ((b'd' as libc::c_ulong) << 8) | 100
};

/// CD-TEXT PACKs straight off the drive, or `None` when the drive reports
/// none. `device_node` is the media's whole-disk node (`/dev/disk12`); the
/// raw character device is what gets opened, because the block device is
/// busy for as long as the volume is mounted — and an audio CD is always
/// mounted on macOS.
fn read_cdtext_packs(device_node: &str) -> Result<Vec<u8>, String> {
    let raw_node = device_node.replace("/dev/disk", "/dev/rdisk");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&raw_node)
        .map_err(|e| format!("couldn't open {raw_node}: {e}"))?;

    // 2048 PACKs is the format's ceiling; real discs use a few dozen.
    let mut buf = vec![0u8; 8192];
    let mut request = CdReadToc {
        format: 5,
        format_as_time: 0,
        reserved_16: [0; 5],
        address: 0,
        reserved_64: [0; 6],
        buffer_length: buf.len() as u16,
        buffer: buf.as_mut_ptr().cast(),
    };
    // SAFETY: the request struct matches the kernel's, and `buffer` points at
    // `buffer_length` writable bytes owned here for the length of the call.
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), DKIOCCDREADTOC, &mut request) };
    if rc < 0 {
        return Err(format!(
            "reading CD-TEXT from {raw_node} failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    buf.truncate(request.buffer_length as usize);
    Ok(buf)
}

/// Parse raw PACKs into one entry per CD-TEXT block, each holding the block's
/// disc-and-track array.
fn blocks_from_packs(packs: &[u8]) -> Vec<Vec<BlockTrack>> {
    // A four-byte READ TOC header plus nothing is what a disc without CD-TEXT
    // answers; there is no block to build from it.
    if packs.len() <= 4 {
        return Vec::new();
    }
    let Some(class) = AnyClass::get(&CString::new("DRCDTextBlock").expect("no NUL")) else {
        return Vec::new();
    };
    let data = CFData::from_bytes(packs);
    let data_ptr: *const AnyObject = CFRetained::as_ptr(&data).as_ptr().cast();
    // SAFETY: `+[DRCDTextBlock arrayOfCDTextBlocksFromPacks:]` takes an NSData
    // (toll-free bridged from CFData) and answers an autoreleased NSArray,
    // which `Retained` takes ownership of. It answers nil for data it cannot
    // parse.
    let blocks: Option<Retained<AnyObject>> =
        unsafe { msg_send![class, arrayOfCDTextBlocksFromPacks: data_ptr] };
    let Some(blocks) = blocks else {
        return Vec::new();
    };
    // NSArray is toll-free bridged to CFArray, so the C accessors take it from
    // here.
    let blocks: &CFArray<CFType> = unsafe { &*Retained::as_ptr(&blocks).cast::<CFArray<CFType>>() };

    (0..blocks.len())
        .filter_map(|i| {
            let block = blocks.get(i)?;
            let block: *const CFType = &*block;
            // SAFETY: the dictionaries come back borrowed (Get rule) and live
            // as long as the block does, which outlives this loop body.
            let tracks = unsafe { DRCDTextBlockGetTrackDictionaries(block) };
            let tracks = unsafe { tracks.as_ref() }?;
            Some(
                (0..tracks.len())
                    .map(|t| track_text(tracks, t))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

fn track_text(tracks: &CFArray<CFDictionary<CFString, CFType>>, index: usize) -> BlockTrack {
    let Some(dict) = tracks.get(index) else {
        return BlockTrack::default();
    };
    let dict = &*dict;
    BlockTrack {
        title: string(dict, unsafe { kDRCDTextTitleKey }),
        performer: string(dict, unsafe { kDRCDTextPerformerKey }),
    }
}

/// Read every CD-TEXT block off the disc in `device_node`. An empty result
/// means the drive reported no CD-TEXT, which is what most discs do.
pub fn cdtext_blocks(device_node: &str) -> Result<Vec<Vec<BlockTrack>>, String> {
    Ok(blocks_from_packs(&read_cdtext_packs(device_node)?))
}
