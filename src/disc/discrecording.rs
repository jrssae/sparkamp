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
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicPtr, Ordering};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_core_foundation::{
    kCFTypeArrayCallBacks, kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks, CFArray,
    CFBoolean, CFData, CFDictionary, CFMutableDictionary, CFNumber, CFRetained, CFString, CFType,
    CFURL, CFURLPathStyle,
};

use super::cdtext::{CdTextSheet, TrackText};
use super::MediaKind;
use super::detect::MediaStatus;

/// An opaque `DRDeviceRef`. It is a CoreFoundation object, so `CFType` carries
/// it with the right retain/release semantics.
type DeviceRef = *const CFType;

/// `DRTrackRef`, `DRBurnRef`, `DREraseRef` and `DRFolderRef`. All four are
/// CoreFoundation objects too, so the same carrier works for every one.
type TrackRef = *const CFType;
type BurnRef = *const CFType;
type EraseRef = *const CFType;
type FolderRef = *const CFType;

/// `DRTrackCallbackProc`. A plain C function pointer, which is what keeps the
/// data producer an `extern "C" fn` with no Objective-C block anywhere near
/// the burn.
type TrackCallback = unsafe extern "C" fn(TrackRef, u32, *mut c_void) -> i32;

#[link(name = "DiscRecording", kind = "framework")]
unsafe extern "C" {
    fn DRCopyDeviceArray() -> *const CFArray<CFType>;
    fn DRDeviceCopyInfo(device: DeviceRef) -> *const CFDictionary<CFString, CFType>;
    fn DRDeviceCopyStatus(device: DeviceRef) -> *const CFDictionary<CFString, CFType>;
    fn DRDeviceEjectMedia(device: DeviceRef) -> i32;
    fn DRCDTextBlockGetTrackDictionaries(
        block: *const CFType,
    ) -> *const CFArray<CFDictionary<CFString, CFType>>;
    fn DRCDTextBlockCreate(language: *const CFString, encoding: u32) -> *mut CFType;
    fn DRCDTextBlockSetValue(
        block: *mut CFType,
        track_index: isize,
        key: *const CFString,
        value: *const CFType,
    );
    fn DRCDTextBlockGetValue(
        block: *const CFType,
        track_index: isize,
        key: *const CFString,
    ) -> *const CFType;

    fn DRTrackCreate(
        properties: *const CFDictionary<CFString, CFType>,
        callback: TrackCallback,
    ) -> TrackRef;
    fn DRTrackGetProperties(track: TrackRef) -> *mut CFMutableDictionary;
    fn DRTrackEstimateLength(track: TrackRef) -> u64;
    fn DRTrackSpeedTest(track: TrackRef, milliseconds: u32, bytes: u32) -> f32;
    fn DRFolderCreateRealWithURL(url: *const CFURL) -> FolderRef;
    fn DRFilesystemTrackCreate(root: FolderRef) -> TrackRef;
    // Only `dump_data_track_properties` calls this: the mask is left at its
    // default in the burn, and that test is the measurement behind the table
    // on `data_track` saying why.
    #[cfg_attr(not(test), allow(dead_code))]
    fn DRFSObjectSetFilesystemMask(object: FolderRef, mask: u32);

    fn DRBurnCreate(device: DeviceRef) -> BurnRef;
    fn DRBurnSetProperties(burn: BurnRef, properties: *const CFDictionary<CFString, CFType>);
    fn DRBurnGetProperties(burn: BurnRef) -> *const CFDictionary<CFString, CFType>;
    fn DRBurnWriteLayout(burn: BurnRef, layout: *const CFType) -> i32;
    fn DRBurnCopyStatus(burn: BurnRef) -> *const CFDictionary<CFString, CFType>;
    fn DRBurnAbort(burn: BurnRef);

    fn DREraseCreate(device: DeviceRef) -> EraseRef;
    fn DREraseSetProperties(erase: EraseRef, properties: *const CFDictionary<CFString, CFType>);
    fn DREraseStart(erase: EraseRef) -> i32;
    fn DREraseCopyStatus(erase: EraseRef) -> *const CFDictionary<CFString, CFType>;

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
    static kDRCDTextKey: Option<&'static CFString>;
    static kDRDeviceCanWriteCDTextKey: Option<&'static CFString>;

    static kDRTrackLengthKey: Option<&'static CFString>;
    static kDRBlockSizeKey: Option<&'static CFString>;
    static kDRBlockTypeKey: Option<&'static CFString>;
    static kDRDataFormKey: Option<&'static CFString>;
    static kDRSessionFormatKey: Option<&'static CFString>;
    static kDRTrackModeKey: Option<&'static CFString>;
    static kDRVerificationTypeKey: Option<&'static CFString>;
    static kDRVerificationTypeChecksum: Option<&'static CFString>;
    static kDRVerificationTypeProduceAgain: Option<&'static CFString>;

    static kDRSynchronousBehaviorKey: Option<&'static CFString>;
    static kDRBurnTestingKey: Option<&'static CFString>;
    static kDRBurnVerifyDiscKey: Option<&'static CFString>;
    static kDRBurnCompletionActionKey: Option<&'static CFString>;
    static kDRBurnCompletionActionEject: Option<&'static CFString>;

    static kDREraseTypeKey: Option<&'static CFString>;
    static kDREraseTypeQuick: Option<&'static CFString>;

    static kDRStatusStateKey: Option<&'static CFString>;
    static kDRStatusPercentCompleteKey: Option<&'static CFString>;
    static kDRStatusStateNone: Option<&'static CFString>;
    static kDRStatusStateDone: Option<&'static CFString>;
    static kDRStatusStateFailed: Option<&'static CFString>;
    static kDRStatusStateVerifying: Option<&'static CFString>;

    static kDRErrorStatusKey: Option<&'static CFString>;
    static kDRErrorStatusErrorKey: Option<&'static CFString>;
    static kDRErrorStatusErrorStringKey: Option<&'static CFString>;
    static kDRErrorStatusSenseCodeStringKey: Option<&'static CFString>;
    static kDRErrorStatusAdditionalSenseStringKey: Option<&'static CFString>;
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

fn float(dict: &CFDictionary<CFString, CFType>, key: Option<&'static CFString>) -> Option<f64> {
    lookup(dict, key)?.downcast_ref::<CFNumber>()?.as_f64()
}

/// Build a `CFDictionary` the framework will read properties out of. A pair
/// whose key constant is missing is dropped rather than poisoning the whole
/// dictionary — every property this file sets is optional to the framework,
/// and the ones that aren't are checked by the caller.
fn dictionary(pairs: &[(Option<&'static CFString>, &CFType)]) -> CFRetained<CFDictionary> {
    let mut keys: Vec<*const c_void> = Vec::with_capacity(pairs.len());
    let mut values: Vec<*const c_void> = Vec::with_capacity(pairs.len());
    for (key, value) in pairs {
        let Some(key) = key else { continue };
        keys.push((*key as *const CFString).cast());
        values.push((*value as *const CFType).cast());
    }
    // SAFETY: both arrays hold `keys.len()` live CoreFoundation pointers, and
    // the type callbacks retain each of them, so the dictionary keeps them
    // alive past this borrow.
    unsafe {
        CFDictionary::new(
            None,
            keys.as_mut_ptr(),
            values.as_mut_ptr(),
            keys.len() as isize,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        )
    }
    .expect("CFDictionaryCreate returned NULL")
}

/// Retype a built dictionary for the `DR*SetProperties` calls, which all take
/// a `CFDictionaryRef` whose keys are CFStrings.
fn as_property_dict(dict: &CFDictionary) -> *const CFDictionary<CFString, CFType> {
    // SAFETY: the generic parameters only describe how the contents are read
    // back; the pointee is the same `CFDictionary` either way.
    (dict as *const CFDictionary).cast()
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

    /// Whether the drive can write CD-TEXT.
    ///
    /// Worth asking before attaching a block: the header is explicit that a
    /// burn carrying `kDRCDTextKey` to a drive that cannot write CD-TEXT
    /// **fails** with `kDRDeviceCantWriteCDTextErr`. So a drive without the
    /// capability gets a burn with no CD-TEXT rather than no burn at all —
    /// the disc is what the user asked for, minus a field their hardware
    /// could never have written.
    ///
    /// Defaults to `false` when the drive does not answer, unlike
    /// [`Self::can_write`], and for the opposite reason: an unknown here
    /// costs a text field, an unknown there costs the whole burn.
    pub fn can_write_cdtext(&self) -> bool {
        let Some(info) = self.info() else { return false };
        let Some(caps) = sub_dict(&info, unsafe { kDRDeviceWriteCapabilitiesKey }) else {
            return false;
        };
        // The drive's own answer, and only that. Pairing it with a
        // session-at-once check was tried: CD-TEXT does need SAO, but the
        // engine chooses the strategy and will not choose one that cannot
        // carry the data, so second-guessing it here only invents a way to
        // refuse CD-TEXT on a drive that would have written it.
        boolean(&caps, unsafe { kDRDeviceCanWriteCDTextKey })
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

/// Open the media's raw BSD character device.
///
/// `device_node` is the whole-disk node (`/dev/disk12`); `/dev/rdiskN` is what
/// gets opened, because the block device is busy for as long as the volume is
/// mounted and an audio CD is always mounted on macOS.
///
/// The raw device is also the only way in under the App Sandbox. A sandboxed
/// build is refused every read *inside* the mounted `cddafs` volume, whether
/// or not it holds `files.removable-media.read-write`, while `/dev/rdiskN`
/// reads normally. Measured against a signed sandboxed bundle, not assumed.
fn open_raw_media(device_node: &str) -> Result<(String, std::fs::File), String> {
    let raw_node = device_node.replace("/dev/disk", "/dev/rdisk");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&raw_node)
        .map_err(|e| format!("couldn't open {raw_node}: {e}"))?;
    Ok((raw_node, file))
}

/// One `DKIOCCDREADTOC` call. `capacity` sizes the answer buffer; the drive
/// reports how much of it it filled and the result is cut to that.
fn toc_ioctl(file: &std::fs::File, format: u8, capacity: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; capacity];
    let mut request = CdReadToc {
        format,
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
        return Err(std::io::Error::last_os_error());
    }
    buf.truncate(request.buffer_length as usize);
    Ok(buf)
}

/// The loaded disc's table of contents, read straight off the drive.
///
/// Format 0 is the plain TOC: each track's start LBA and the lead-out. macOS
/// also publishes this as a `.TOC.plist` inside the mounted audio-CD volume,
/// and that is where the detector reads it first, but the App Sandbox refuses
/// every read inside that volume, so under the sandbox a 15-track audio CD
/// came back as a data disc with no tracks. Asking the drive works in both
/// builds.
///
/// `None` when the drive has no readable audio TOC, which leaves the caller
/// free to fall back to the mounted volume.
pub(crate) fn read_toc(device_node: &str) -> Option<crate::disc::DiscToc> {
    let (_, file) = open_raw_media(device_node).ok()?;
    // 99 tracks plus a lead-out is 800 bytes of descriptors; 2048 is headroom.
    let buf = toc_ioctl(&file, 0, 2048).ok()?;
    let (entries, leadout) = crate::disc::detect::parse_mmc_toc(&buf)?;
    crate::disc::detect::toc_from_entries(&entries, leadout)
}

/// Bytes in one CDDA sector: 588 stereo frames of 16-bit little-endian
/// samples. The raw device hands these back exactly as they sit on the disc,
/// which is already Red Book PCM, so extraction is a copy with a header on
/// the front rather than a decode.
const CDDA_SECTOR: u64 = 2352;

/// Bytes of Red Book audio per second, for turning progress into seconds.
const CDDA_BYTES_PER_SEC: f64 = 176_400.0;

/// Sectors per read. Big enough that the syscall count stays low, small
/// enough that one damaged spot only costs this much re-reading.
const CDDA_CHUNK_SECTORS: u64 = 128;

/// Re-reads before a sector is called unreadable. A marginal sector often
/// reads on a later attempt once the drive has re-seeked.
const CDDA_SECTOR_RETRIES: usize = 5;

/// Resolve the 1-based drive id the rest of the app uses into the media's
/// whole-disk device node. The id is an index into the framework's device
/// array rather than a path, because burning passes the same number to
/// `drutil -drive N`.
fn device_node_for_id(drive_id: &str) -> Option<String> {
    let index: usize = drive_id.parse().ok()?;
    let devices = devices();
    devices.get(index.checked_sub(1)?)?.status().device_node
}

/// Write a canonical 44-byte Red Book WAV header for `data_len` bytes of PCM.
fn write_wav_header(w: &mut impl std::io::Write, data_len: u64) -> std::io::Result<()> {
    w.write_all(b"RIFF")?;
    w.write_all(&((36 + data_len) as u32).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&2u16.to_le_bytes())?; // stereo
    w.write_all(&44_100u32.to_le_bytes())?;
    w.write_all(&176_400u32.to_le_bytes())?;
    w.write_all(&4u16.to_le_bytes())?; // block align
    w.write_all(&16u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&(data_len as u32).to_le_bytes())?;
    Ok(())
}

/// Read `buf` at `offset`, retrying a sector at a time when the whole chunk
/// fails.
///
/// A read that covers many sectors fails as a unit, which says nothing about
/// which sector is bad. Dropping to one sector at a time finds it, and gives
/// the drive several attempts at it: marginal sectors often read on a later
/// try once the head has re-seeked. Only a sector that fails every attempt is
/// reported, and it is reported by number rather than as a generic I/O error.
fn read_sectors_with_retry(
    disc: &std::fs::File,
    offset: u64,
    buf: &mut [u8],
) -> Result<(), String> {
    use std::os::unix::fs::FileExt;
    if disc.read_exact_at(buf, offset).is_ok() {
        return Ok(());
    }
    let sector_len = CDDA_SECTOR as usize;
    for (i, chunk) in buf.chunks_mut(sector_len).enumerate() {
        let at = offset + (i as u64) * CDDA_SECTOR;
        let mut last = None;
        let mut ok = false;
        for _ in 0..CDDA_SECTOR_RETRIES {
            match disc.read_exact_at(chunk, at) {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        if !ok {
            let sector = at / CDDA_SECTOR;
            let err = last.map(|e| e.to_string()).unwrap_or_default();
            return Err(format!(
                "sector {sector} is unreadable after {CDDA_SECTOR_RETRIES} attempts: {err}"
            ));
        }
    }
    Ok(())
}

/// Where one audio track lives: the raw device to read, and the half-open LBA
/// range that is the track.
fn cdda_span(drive_id: &str, track: u8) -> Result<(String, u32, u32), String> {
    let node =
        device_node_for_id(drive_id).ok_or_else(|| format!("no optical drive {drive_id}"))?;
    let toc = read_toc(&node).ok_or_else(|| format!("{node} has no readable audio TOC"))?;
    let (start, end) = crate::disc::toc::track_span(&toc, track)
        .ok_or_else(|| format!("track {track} is not on this disc"))?;
    Ok((node.replace("/dev/disk", "/dev/rdisk"), start, end))
}

/// Frames of audio in one CDDA sector.
pub(crate) const CDDA_FRAMES_PER_SECTOR: i64 = 588;

/// Sectors per chunk handed to the player. 75 sectors is one second of audio.
const CDDA_STREAM_CHUNK_SECTORS: u32 = 75;

/// Chunks the reader may run ahead by, and so the anti-skip buffer's depth in
/// seconds. The drive reads at roughly 8x realtime, so the reader spends most
/// of its life blocked on this queue being full, which is exactly the margin
/// that keeps a re-seek or a slow patch of disc from being audible.
const CDDA_STREAM_BACKLOG: usize = 16;

/// Frames of audio in one track, from the TOC alone. Reads no audio, so it is
/// cheap enough to call while deciding whether a track can be played at all.
pub(crate) fn cdda_track_frames(drive_id: &str, track: u8) -> Result<i64, String> {
    let (_, start, end) = cdda_span(drive_id, track)?;
    Ok((end - start) as i64 * CDDA_FRAMES_PER_SECTOR)
}

/// A background reader pulling one track's audio off the drive.
///
/// Playback cannot wait for a whole track: at 8x realtime a four-minute track
/// takes about half a minute to pull, and that is what the user would wait
/// before hearing anything. So the drive is read on its own thread into a
/// bounded queue, and the audio graph takes chunks from it as it needs them.
pub(crate) struct CddaReader {
    rx: std::sync::mpsc::Receiver<Result<Vec<u8>, String>>,
    done: bool,
}

impl CddaReader {
    /// Start reading `track` from `from_frame`, counted from the track's start.
    pub(crate) fn open(drive_id: &str, track: u8, from_frame: i64) -> Result<Self, String> {
        let (raw_node, start, end) = cdda_span(drive_id, track)?;
        let from = from_frame.clamp(0, (end - start) as i64 * CDDA_FRAMES_PER_SECTOR);
        let first = start + (from / CDDA_FRAMES_PER_SECTOR) as u32;
        // A seek lands mid-sector more often than not. The leading frames of
        // the first sector are dropped rather than rounded away, so the audio
        // starts where the seek asked and not up to 13 ms before it.
        let mut skip = ((from % CDDA_FRAMES_PER_SECTOR) * 4) as usize;

        let disc = std::fs::File::open(&raw_node)
            .map_err(|e| format!("couldn't open {raw_node}: {e}"))?;
        let (tx, rx) = std::sync::mpsc::sync_channel(CDDA_STREAM_BACKLOG);

        std::thread::spawn(move || {
            let mut sector = first;
            while sector < end {
                let want = CDDA_STREAM_CHUNK_SECTORS.min(end - sector);
                let mut buf = vec![0u8; want as usize * CDDA_SECTOR as usize];
                let at = sector as u64 * CDDA_SECTOR;
                let message = match read_sectors_with_retry(&disc, at, &mut buf) {
                    Ok(()) => {
                        if skip > 0 {
                            buf.drain(..skip.min(buf.len()));
                            skip = 0;
                        }
                        Ok(buf)
                    }
                    Err(e) => Err(format!("reading track {track} from {raw_node}: {e}")),
                };
                let failed = message.is_err();
                // A send that fails means the player has let this track go,
                // which is an ordinary way for this thread to end.
                if tx.send(message).is_err() || failed {
                    return;
                }
                sector += want;
            }
        });

        Ok(CddaReader { rx, done: false })
    }

    /// True once the reader has handed over everything it is going to.
    pub(crate) fn is_finished(&self) -> bool {
        self.done
    }

    /// The next ready chunk of interleaved 16-bit little-endian stereo PCM, or
    /// `None` when none is ready. Never blocks: the caller drives the audio
    /// graph and must not be parked on a drive.
    pub(crate) fn try_next(&mut self) -> Option<Result<Vec<u8>, String>> {
        use std::sync::mpsc::TryRecvError;
        match self.rx.try_recv() {
            Ok(chunk) => Some(chunk),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.done = true;
                None
            }
        }
    }
}

/// Extract one audio track to a Red Book WAV, reading the drive directly.
///
/// macOS also mounts an audio CD as one AIFF per track, and this is what used
/// to be read. It cannot be any more: the App Sandbox refuses every read
/// inside the mounted volume, so the App Store build found no tracks at all.
/// Reading the drive is what Linux has always done, and doing the same here
/// keeps both build channels producing the same bytes.
///
/// No drive read-offset correction is applied, which matches cdparanoia's
/// default on Linux. The mounted AIFF this replaces sat 570 sample frames
/// (13 ms) away on the drive measured here. That difference is deliberate:
/// correcting it would mean carrying a per-drive-model constant, and the
/// offset cannot be measured under the sandbox, where the AIFF to compare
/// against is exactly what cannot be read.
pub(crate) fn cdda_track_to_wav(
    drive_id: &str,
    track: u8,
    out: &Path,
    on_position: &mut dyn FnMut(f64),
) -> Result<(), String> {
    use std::io::Write;

    let (raw_node, start, end) = cdda_span(drive_id, track)?;
    // Plain blocking open: the O_NONBLOCK the ioctls use is for talking to a
    // drive that may hold no medium, and it would only invite short reads here.
    let disc = std::fs::File::open(&raw_node)
        .map_err(|e| format!("couldn't open {raw_node}: {e}"))?;

    let total = (end - start) as u64 * CDDA_SECTOR;

    // Written to a temporary and renamed on success. A half-written WAV is
    // worse than none: the burner would take it for a track and write a
    // truncated one.
    let tmp = out.with_extension("wav.part");
    if let Some(parent) = tmp.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("couldn't create {}: {e}", parent.display()))?;
    }
    let mut w = std::io::BufWriter::new(
        std::fs::File::create(&tmp)
            .map_err(|e| format!("couldn't create {}: {e}", tmp.display()))?,
    );

    let mut extract = || -> Result<(), String> {
        write_wav_header(&mut w, total).map_err(|e| format!("writing WAV header failed: {e}"))?;
        let mut buf = vec![0u8; (CDDA_CHUNK_SECTORS * CDDA_SECTOR) as usize];
        let mut done: u64 = 0;
        while done < total {
            let want = buf.len().min((total - done) as usize);
            let at = start as u64 * CDDA_SECTOR + done;
            read_sectors_with_retry(&disc, at, &mut buf[..want])
                .map_err(|e| format!("reading track {track} from {raw_node}: {e}"))?;
            w.write_all(&buf[..want])
                .map_err(|e| format!("writing {}: {e}", tmp.display()))?;
            done += want as u64;
            on_position(done as f64 / CDDA_BYTES_PER_SEC);
        }
        w.flush().map_err(|e| format!("writing {}: {e}", tmp.display()))
    };

    let result = extract();
    drop(w);
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, out)
        .map_err(|e| format!("couldn't move {} into place: {e}", tmp.display()))
}

/// CD-TEXT PACKs straight off the drive, or an empty vector when the drive
/// reports none.
fn read_cdtext_packs(device_node: &str) -> Result<Vec<u8>, String> {
    let (raw_node, file) = open_raw_media(device_node)?;
    // 2048 PACKs is the format's ceiling; real discs use a few dozen.
    match toc_ioctl(&file, 5, 8192) {
        Ok(buf) => Ok(trim_to_whole_packs(buf)),
        // EIO is how a drive says "this disc has no CD-TEXT". Measured: an
        // audio CD-R burned without it answers format 5 with EIO, while a disc
        // that has CD-TEXT answers with PACKs and one that merely has none in
        // its lead-in can answer with a bare header instead. All three mean
        // the same thing to a caller, and reporting this one as a failure put
        // "CD-TEXT read failed — Input/output error (os error 5)" in front of
        // the user for an ordinary disc. An empty answer reads as `Absent`,
        // which the UI is deliberately quiet about.
        Err(err) if err.raw_os_error() == Some(libc::EIO) => Ok(Vec::new()),
        Err(err) => Err(format!("reading CD-TEXT from {raw_node} failed: {err}")),
    }
}

/// Cut a READ TOC format-5 answer down to exactly the PACKs it declares.
///
/// Required, not tidiness. `DRCDTextBlockCreateArrayFromPackList`'s
/// documentation is explicit — "The CFData should be sized to fit the exact
/// number of PACKs. Each PACK occupies 18 bytes, and the 4-byte header from a
/// READ TOC command may optionally be included" — and the parser answers nil
/// for anything else, which reads as a disc with no CD-TEXT at all.
///
/// The drive does not hand back a buffer that size. Measured on a
/// Slimtype DS8A5SH reading a disc this code had just written: the ioctl
/// reported 204 bytes while the header declared 200, so the real answer was
/// 4 header + 11 PACKs = 202, with two bytes of slop past the end. Those two
/// bytes were the whole difference between reading the CD-TEXT and reporting
/// `Absent`.
///
/// The first two bytes are a big-endian length covering everything after
/// them, so the answer runs to `2 + length`. Anything past that, or any
/// trailing fragment of a 19th of an 18-byte PACK, is not data.
fn trim_to_whole_packs(mut buf: Vec<u8>) -> Vec<u8> {
    const HEADER: usize = 4;
    const PACK: usize = 18;
    if buf.len() < HEADER {
        return Vec::new();
    }
    let declared = usize::from(u16::from_be_bytes([buf[0], buf[1]])) + 2;
    let end = declared.min(buf.len());
    buf.truncate(HEADER + end.saturating_sub(HEADER) / PACK * PACK);
    buf
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

// ---------------------------------------------------------------------------
// Track production
// ---------------------------------------------------------------------------

/// `struct DRTrackProductionInfo` from `<DiscRecording/DRCoreTrack.h>`,
/// 64-bit layout.
#[repr(C)]
struct ProductionInfo {
    buffer: *mut c_void,
    req_count: u32,
    act_count: u32,
    flags: u32,
    block_size: u32,
    requested_address: u64,
}

/// The `DRTrackMessage`s the producer answers. They are four-character codes,
/// which is why they read as byte strings.
const MSG_PRODUCE_DATA: u32 = u32::from_be_bytes(*b"prod");
const MSG_ESTIMATE_LENGTH: u32 = u32::from_be_bytes(*b"esti");
const MSG_PRE_BURN: u32 = u32::from_be_bytes(*b"pre ");
const MSG_POST_BURN: u32 = u32::from_be_bytes(*b"post");
const MSG_VERIFICATION_STARTING: u32 = u32::from_be_bytes(*b"vstr");
const MSG_VERIFICATION_DONE: u32 = u32::from_be_bytes(*b"vdon");

/// `noErr`, plus the two `DiscRecording` errors the producer returns.
/// `kDRFunctionNotSupportedErr` is how a callback declines a message it does
/// not handle; every *other* non-zero value fails the burn on the spot with
/// that value as the reason.
const NO_ERR: i32 = 0;
const FUNCTION_NOT_SUPPORTED_ERR: i32 = 0x8002_0067u32 as i32;
const DATA_PRODUCTION_ERR: i32 = 0x8002_0062u32 as i32;

/// Red Book geometry: 2352 bytes per block, 75 blocks per second.
const AUDIO_BLOCK_SIZE: u64 = 2352;

/// Values for the required track-property keys, from `DRCoreTrack.h`'s Block
/// Sizes / Block Types / Data Forms / Track Modes / Session Format
/// enumerations. Audio is 0 in four of the five.
const BLOCK_TYPE_AUDIO: i64 = 0;
const DATA_FORM_AUDIO: i64 = 0;
const SESSION_FORMAT_AUDIO: i64 = 0;
const TRACK_MODE_AUDIO: i64 = 0;

/// One staged WAV, as the producer sees it: an open file and where the PCM
/// sits inside it.
#[derive(Debug)]
struct TrackSource {
    /// The `DRTrackRef` this serves, as an address. The callback gets the
    /// track and nothing else, so this is the key it is found by.
    track: usize,
    file: std::fs::File,
    data_offset: u64,
    data_len: u64,
    blocks: u64,
}

impl TrackSource {
    /// Open a staged WAV and measure its payload. The declared payload length
    /// is clamped to what is actually on disk: only the file knows whether the
    /// writer finished, and trusting a stale header would ask the producer for
    /// bytes that do not exist.
    fn open(path: &Path, track: TrackRef) -> Result<Self, String> {
        let file = std::fs::File::open(path)
            .map_err(|e| format!("couldn't open {}: {e}", path.display()))?;
        let size = file
            .metadata()
            .map_err(|e| format!("couldn't stat {}: {e}", path.display()))?
            .len();

        let mut header = vec![0u8; 4096.min(size) as usize];
        file.read_exact_at(&mut header, 0)
            .map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
        let (data_offset, declared) = super::burn::wav_redbook_span(&header)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let data_len = declared.min(size.saturating_sub(data_offset));
        if data_len == 0 {
            return Err(format!("{} holds no audio", path.display()));
        }

        Ok(Self {
            track: track as usize,
            file,
            data_offset,
            data_len,
            blocks: data_len.div_ceil(AUDIO_BLOCK_SIZE),
        })
    }

    /// The track's declared length in bytes — a whole number of blocks, so the
    /// tail past the audio is silence.
    fn track_bytes(&self) -> u64 {
        self.blocks * AUDIO_BLOCK_SIZE
    }

    /// Fill `out` with the track's bytes starting at `at`, padding past the
    /// end of the audio with digital silence — the same tail `cdrskin -pad`
    /// writes. `false` means the read failed and the burn must stop: a silent
    /// gap reported as success is the coaster-as-success failure this code has
    /// always refused to produce.
    ///
    /// `pread` rather than seek-then-read: no shared file cursor, so nothing
    /// here needs a lock, and the producer stays re-entrant across the burn
    /// engine's threads.
    fn fill(&self, at: u64, out: &mut [u8]) -> bool {
        let from_file = self.data_len.saturating_sub(at).min(out.len() as u64) as usize;
        let (audio, pad) = out.split_at_mut(from_file);
        pad.fill(0);
        let mut done = 0;
        while done < audio.len() {
            match self.file.read_at(&mut audio[done..], self.data_offset + at + done as u64) {
                Ok(0) => return false,
                Ok(n) => done += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        true
    }
}

/// The sources the producer serves, published for exactly as long as a burn
/// can call it.
///
/// `DRTrackCallbackProc` carries no refcon, so the callback's only way back to
/// per-track state is the track itself. The other candidate — reading it out
/// of the track's own properties dictionary — is unsafe here, because
/// `DRTrackSpeedTest` writes `kDRMaxBurnSpeedKey` into that dictionary while
/// the producer is running. An immutable table, published before the burn and
/// dropped after it, is only ever read while the callback can fire: the load
/// is one atomic and the lookup a scan of at most 99 entries, so the producer
/// never waits on a lock, allocates, or takes a page fault it did not ask for.
static SOURCES: AtomicPtr<Vec<TrackSource>> = AtomicPtr::new(std::ptr::null_mut());

/// Publishes `sources` for the length of a burn and takes them down again on
/// the way out — including on a panic, which is the reason this is a guard and
/// not a pair of calls.
struct PublishedSources;

impl PublishedSources {
    fn new(sources: Vec<TrackSource>) -> Self {
        let previous = SOURCES.swap(Box::into_raw(Box::new(sources)), Ordering::AcqRel);
        debug_assert!(previous.is_null(), "one burn at a time");
        Self
    }
}

impl Drop for PublishedSources {
    fn drop(&mut self) {
        let published = SOURCES.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if !published.is_null() {
            // SAFETY: the only writer is `new`, which stores a `Box::into_raw`
            // of this same type, and the swap makes this the one owner.
            drop(unsafe { Box::from_raw(published) });
        }
    }
}

/// The source serving `track`, or `None` when the table has gone — which can
/// only happen outside a burn, and answers `kDRDataProductionErr` rather than
/// reading freed memory.
fn source_for(track: TrackRef) -> Option<&'static TrackSource> {
    let published = SOURCES.load(Ordering::Acquire);
    // SAFETY: `PublishedSources` keeps the box alive from before the burn
    // starts until after the last thread that could call the producer has
    // been joined, so a non-null load is live for the whole callback.
    let table = unsafe { published.as_ref() }?;
    table.iter().find(|s| s.track == track as usize)
}

/// The data producer. It runs on the burn engine's own thread while the write
/// is live, so it does exactly one bounded thing per message: a table scan and
/// a `pread`. No allocation, no locking, no CoreFoundation calls — starving
/// this callback is what turns a disc into a coaster.
///
/// Subchannel data is never requested: the track properties leave
/// `kDRSubchannelDataFormKey` unset, which defaults to "none", so `blockSize`
/// is the plain 2352 and `kDRFlagSubchannelDataRequested` never arrives.
unsafe extern "C" fn produce_track_data(
    track: TrackRef,
    message: u32,
    io_param: *mut c_void,
) -> i32 {
    match message {
        MSG_PRODUCE_DATA => {
            let Some(source) = source_for(track) else {
                return DATA_PRODUCTION_ERR;
            };
            // SAFETY: for this message the framework documents ioParam as a
            // pointer to a DRTrackProductionInfo it owns for the call.
            let info = unsafe { &mut *io_param.cast::<ProductionInfo>() };
            let want = u64::from(info.req_count)
                .min(source.track_bytes().saturating_sub(info.requested_address));
            // SAFETY: the engine hands over `req_count` writable bytes at
            // `buffer`, and `want` never exceeds that.
            let out =
                unsafe { std::slice::from_raw_parts_mut(info.buffer.cast::<u8>(), want as usize) };
            if !source.fill(info.requested_address, out) {
                return DATA_PRODUCTION_ERR;
            }
            info.act_count = want as u32;
            NO_ERR
        }
        MSG_ESTIMATE_LENGTH => {
            let Some(source) = source_for(track) else {
                return DATA_PRODUCTION_ERR;
            };
            // SAFETY: for this message the framework documents ioParam as a
            // pointer to the UInt64 the callback fills in.
            unsafe { *io_param.cast::<u64>() = source.blocks };
            NO_ERR
        }
        // The files are open before the burn starts and stay open until the
        // whole layout is done, so there is nothing to do at either edge.
        MSG_PRE_BURN | MSG_POST_BURN | MSG_VERIFICATION_STARTING | MSG_VERIFICATION_DONE => NO_ERR,
        // Pregap included: declining it leaves the engine to write its own
        // two seconds of silence, which is what a Red Book gap is.
        _ => FUNCTION_NOT_SUPPORTED_ERR,
    }
}

// ---------------------------------------------------------------------------
// Burning and erasing
// ---------------------------------------------------------------------------

/// How often the burn is polled for progress and for a cancel — the same
/// 200 ms the subprocess runner used to poll the child with, so the UI's
/// update rate is unchanged.
const POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// The phase label callers see while the engine writes. Byte-identical to
/// what the subprocess path reported, because the UIs render it verbatim.
const BURNING: &str = "Burning… (this takes a while)";

/// A CoreFoundation object handed to the thread that runs one operation.
///
/// The burn object is deliberately shared: the worker sits inside
/// `DRBurnWriteLayout` while the caller's thread reads `DRBurnCopyStatus` and,
/// on a cancel, calls `DRBurnAbort`. That split is what `DRBurnAbort` exists
/// for — there is no other way to stop a burn that has started.
struct Handle(*const CFType);

// SAFETY: CoreFoundation objects are not thread-affine, and the retain that
// keeps this one alive outlives both threads: `run_operation` joins the worker
// before its `CFRetained` is dropped.
unsafe impl Send for Handle {}

/// How long the engine may sit in `kDRStatusStateNone` after the start call
/// returned success before this gives up on it. Only pre-start idling counts:
/// the moment the state advances the allowance is discarded, so a full 700 MB
/// write is never cut short.
const STALL: std::time::Duration = std::time::Duration::from_secs(30);

/// One burn or erase, run to completion with progress and cancel.
///
/// **The engine is asynchronous whatever `kDRSynchronousBehaviorKey` says.**
/// Measured on a Slimtype DS8A5SH: with that key set to true and read back
/// true through `DRBurnGetProperties`, `DRBurnWriteLayout` still returned
/// `noErr` after 210 ms while the engine went on writing for another 39
/// seconds, through `TrackWrite`, `SessionClose` and `Finishing` to
/// `Done`. The property survives the dictionary and changes nothing.
///
/// So the status state machine, not the return of the start call, says when
/// the operation is over. Treating the start call as the finish line burned
/// nothing at all: the poll loop exited in 210 ms, `DRBurnCopyStatus` read
/// `Preparing` with no error attached, and the run was reported as a success
/// while the disc was still blank.
///
/// The start call still matters, for the one thing it does report: whether
/// the burn could begin. A non-`noErr` return means there is no state machine
/// to wait for.
fn run_operation(
    handle: Handle,
    start: impl FnOnce(*const CFType) -> i32 + Send,
    status: fn(*const CFType) -> Option<CFRetained<CFDictionary<CFString, CFType>>>,
    abort: Option<fn(*const CFType)>,
    label: fn(&CFDictionary<CFString, CFType>) -> &'static str,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(&str, Option<f32>),
) -> Result<(), String> {
    let object = handle.0;
    std::thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel::<i32>();
        scope.spawn(move || {
            // Bind the whole `Handle`, not its field: a closure that captures
            // `handle.0` captures a bare `*const CFType`, which is not `Send`.
            let handle = handle;
            let _ = tx.send(start(handle.0));
        });
        let mut aborted = false;
        // `None` until the start call returns. The engine can reach
        // `TrackWrite` before it does, so this is not a precondition for
        // watching the state — only for judging a state that never moves.
        let mut started: Option<i32> = None;
        let mut idle = std::time::Duration::ZERO;
        let mut snapshot;
        loop {
            std::thread::sleep(POLL);
            snapshot = status(object);
            if let Some(snapshot) = &snapshot {
                progress(label(snapshot), fraction(snapshot));
            }
            if !aborted && cancelled() {
                match abort {
                    Some(abort) => abort(object),
                    // An erase has no abort in the framework; the cancel takes
                    // effect when it finishes, a minute or two later.
                    None => {}
                }
                aborted = true;
            }
            if started.is_none() {
                started = rx.try_recv().ok();
            }
            if terminal(snapshot.as_deref()) {
                break;
            }
            match started {
                // The engine refused to begin. Nothing will move, and the
                // return code is the whole of what it has to say.
                Some(code) if code != NO_ERR => break,
                Some(_) => {
                    if not_begun(snapshot.as_deref()) {
                        idle += POLL;
                    } else {
                        idle = std::time::Duration::ZERO;
                    }
                    if idle >= STALL {
                        break;
                    }
                }
                None => {}
            }
        }
        // The start call can still be in flight when a `Failed` state ends the
        // loop; the scope joins the worker on the way out either way, so this
        // only waits for a value already on its way.
        let started = started.unwrap_or_else(|| rx.recv().unwrap_or(DATA_PRODUCTION_ERR));
        // An abort raised in the last seconds can land after the final byte,
        // and a disc that is written and good must not be reported as
        // cancelled — the user would go looking for a coaster that isn't one.
        let finished = snapshot.as_deref().is_some_and(|s| {
            is_constant(s, unsafe { kDRStatusStateKey }, unsafe { kDRStatusStateDone })
        });
        if aborted && !finished {
            return Err("cancelled".to_string());
        }
        if let Some(snapshot) = &snapshot {
            if let Some(reason) = failure_reason(snapshot) {
                return Err(reason);
            }
        }
        if started != NO_ERR {
            return Err(describe_status(started));
        }
        // Neither failed nor finished: the state machine never got going, or
        // stopped somewhere that is not an ending. Reporting this as success
        // is the exact bug this loop exists to prevent.
        if !finished {
            return Err(format!(
                "Burn failed: the drive stopped at {} without finishing",
                snapshot
                    .as_deref()
                    .and_then(|s| string(s, unsafe { kDRStatusStateKey }))
                    .unwrap_or_else(|| "no reported state".to_string())
            ));
        }
        Ok(())
    })
}

/// The completion fraction, or `None` when the engine does not have one.
///
/// `kDRStatusPercentCompleteKey` is documented as 0 to 1, and the engine
/// reports `-1` for the phases that have no measurable progress — closing a
/// session, finishing a disc. Clamping that to `0.0` would show a progress bar
/// falling back to empty at the very end; absent is the honest answer, and the
/// callers already render it as an indeterminate phase.
fn fraction(status: &CFDictionary<CFString, CFType>) -> Option<f32> {
    let percent = float(status, unsafe { kDRStatusPercentCompleteKey })? as f32;
    (0.0..=1.0).contains(&percent).then_some(percent)
}

/// Whether the operation has ended, either way.
fn terminal(status: Option<&CFDictionary<CFString, CFType>>) -> bool {
    status.is_some_and(|s| {
        is_constant(s, unsafe { kDRStatusStateKey }, unsafe { kDRStatusStateDone })
            || is_constant(s, unsafe { kDRStatusStateKey }, unsafe { kDRStatusStateFailed })
    })
}

/// Whether the engine has not picked the work up yet: no status at all, or a
/// state it spells `None`.
fn not_begun(status: Option<&CFDictionary<CFString, CFType>>) -> bool {
    match status {
        None => true,
        Some(s) => is_constant(s, unsafe { kDRStatusStateKey }, unsafe { kDRStatusStateNone }),
    }
}

/// Why the engine says it failed, or `None` if it did not. The status
/// dictionary carries a nested error dictionary with an `OSStatus` and, when
/// the framework has one, a localized sentence and the drive's own sense text
/// — strictly more than the subprocess path could recover from a log tail.
fn failure_reason(status: &CFDictionary<CFString, CFType>) -> Option<String> {
    let failed = is_constant(status, unsafe { kDRStatusStateKey }, unsafe {
        kDRStatusStateFailed
    });
    let error = sub_dict(status, unsafe { kDRErrorStatusKey });
    let code = error
        .and_then(|e| number(e, unsafe { kDRErrorStatusErrorKey }))
        .unwrap_or(0);
    // A failed state is enough on its own. Requiring the error dictionary too
    // would let a failure with no attached reason read as success, which is
    // the coaster-reported-as-success this code has always refused to produce.
    if !failed && code == 0 {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(error) = error {
        for key in [
            unsafe { kDRErrorStatusErrorStringKey },
            unsafe { kDRErrorStatusSenseCodeStringKey },
            unsafe { kDRErrorStatusAdditionalSenseStringKey },
        ] {
            if let Some(text) = string(error, key) {
                parts.push(text);
            }
        }
    }
    if parts.is_empty() {
        parts.push(if code == 0 {
            "the drive reported a failure but gave no reason".to_string()
        } else {
            describe_status(code as i32)
        });
    }
    Some(format!("Burn failed: {}", parts.join(" · ")))
}

/// A `DiscRecording` `OSStatus` in words. Only the codes a burn or erase can
/// realistically end on are named; anything else is reported as its number,
/// which is still enough to look up.
fn describe_status(code: i32) -> String {
    let named = match code as u32 {
        0x8002_0021 => "the drive is busy",
        0x8002_0024 => "the drive is not ready",
        0x8002_0041 => "there is no disc in the drive",
        0x8002_0042 => "the disc is not writable",
        0x8002_0043 => "the drive does not support this disc",
        0x8002_0044 => "the disc is not blank",
        0x8002_0045 => "the disc cannot be erased",
        0x8002_0060 => "the drive ran out of data to write (buffer underrun)",
        0x8002_0062 => "the audio could not be read back off disk fast enough",
        0x8002_0063 => "the disc did not verify after writing",
        0x8002_0066 => "cancelled",
        0x8002_006D => "the drive could not calibrate its laser for this disc",
        0x8002_006E => "the drive failed to write the disc",
        _ => return format!("the burn failed (OSStatus {code})"),
    };
    named.to_string()
}

/// Which phase text a burn's status maps to. Verification gets its own label
/// so its percentage restarts under a new heading rather than sending the
/// write's progress bar backwards.
fn burn_label(status: &CFDictionary<CFString, CFType>) -> &'static str {
    if is_constant(status, unsafe { kDRStatusStateKey }, unsafe {
        kDRStatusStateVerifying
    }) {
        "Verifying…"
    } else {
        BURNING
    }
}

fn erase_label(_status: &CFDictionary<CFString, CFType>) -> &'static str {
    "Erasing…"
}

fn burn_status(burn: BurnRef) -> Option<CFRetained<CFDictionary<CFString, CFType>>> {
    // SAFETY: `burn` is a live DRBurnRef; the dictionary comes back +1.
    let raw = unsafe { DRBurnCopyStatus(burn) };
    std::ptr::NonNull::new(raw.cast_mut()).map(|d| unsafe { CFRetained::from_raw(d) })
}

fn erase_status(erase: EraseRef) -> Option<CFRetained<CFDictionary<CFString, CFType>>> {
    // SAFETY: `erase` is a live DREraseRef; the dictionary comes back +1.
    let raw = unsafe { DREraseCopyStatus(erase) };
    std::ptr::NonNull::new(raw.cast_mut()).map(|d| unsafe { CFRetained::from_raw(d) })
}

/// Take ownership of a `DR*Create` result, which comes back +1 (Create rule).
fn owned(raw: *const CFType, what: &str) -> Result<CFRetained<CFType>, String> {
    let raw = std::ptr::NonNull::new(raw.cast_mut())
        .ok_or_else(|| format!("DiscRecording would not create {what}"))?;
    // SAFETY: the Create rule hands over one reference, which this takes.
    Ok(unsafe { CFRetained::from_raw(raw) })
}

/// Set in the environment to make every burn a laser-off rehearsal. Named
/// rather than inlined so the message that announces it cannot drift from the
/// variable that triggers it.
const REHEARSE: &str = "SPARKAMP_BURN_REHEARSE";

/// `kCFStringEncodingISOLatin1`. CD-TEXT's Red Book character set 0x00 is
/// ISO 8859-1, and this is CoreFoundation's name for it. The framework wants
/// the CFStringEncoding, not the Red Book code — the two are different
/// numbers for the same character set, and `kDRCDTextCharacterCodeKey`'s doc
/// warns explicitly against confusing them.
const CDTEXT_ENCODING: u32 = 0x0201;

/// The language a CD-TEXT block declares. ISO 639, and the framework accepts
/// an empty string for "unknown".
const CDTEXT_LANGUAGE: &str = "en";

/// Build one CD-TEXT block from the sheet, or `None` if there is nothing to
/// write.
///
/// CD-TEXT indexes the **disc** at 0 and track N at N, so the disc's album
/// title and artist go in slot 0 and the per-track values start at 1. That
/// offset is the whole reason [`CdTextSheet`] does not carry track numbers:
/// the position in the vector is the track number, and the shift to CD-TEXT's
/// indexing happens here, once.
///
/// The framework rewrites incoming strings to fit the block's character set,
/// so what comes back out of `DRCDTextBlockGetValue` is what will actually be
/// burned — which is what [`cdtext_round_trip`] reads to check the block is a
/// real object before a burn depends on it.
fn cdtext_block(sheet: &CdTextSheet) -> Option<CFRetained<CFType>> {
    if sheet.album.is_empty() && sheet.artist.is_empty() && sheet.tracks.is_empty() {
        return None;
    }
    let language = CFString::from_str(CDTEXT_LANGUAGE);
    // SAFETY: a live CFString and a CFStringEncoding; the block comes back +1
    // (Create rule).
    let raw = unsafe { DRCDTextBlockCreate(CFRetained::as_ptr(&language).as_ptr(), CDTEXT_ENCODING) };
    let block = unsafe { CFRetained::from_raw(std::ptr::NonNull::new(raw)?) };
    let raw = CFRetained::as_ptr(&block).as_ptr();

    let set = |index: isize, key: Option<&'static CFString>, value: &str| {
        let (Some(key), false) = (key, value.is_empty()) else { return };
        let value = CFString::from_str(value);
        // SAFETY: a live block, an index the framework grows the track array
        // to reach, and a CFString for a key documented to take one.
        unsafe {
            DRCDTextBlockSetValue(
                raw,
                index,
                key,
                CFRetained::as_ptr(&value).as_ptr().cast::<CFType>(),
            )
        };
    };
    set(0, unsafe { kDRCDTextTitleKey }, &sheet.album);
    set(0, unsafe { kDRCDTextPerformerKey }, &sheet.artist);
    for (i, track) in sheet.tracks.iter().enumerate() {
        let index = i as isize + 1;
        set(index, unsafe { kDRCDTextTitleKey }, &track.title);
        set(index, unsafe { kDRCDTextPerformerKey }, &track.performer);
    }
    Some(block)
}

/// Read a built block back the way the burn will: what the framework returns
/// after its own character-set rewrite, indexed the way CD-TEXT indexes.
///
/// This exists because `DRCDTextBlockCreateArrayFromPackList` is broken on
/// this OS — it hands back objects that segfault on first use — so "the
/// framework returned a pointer" is not evidence the object is real. Reading
/// values back out is. It costs no media, which is what lets a burn depend on
/// CD-TEXT without a disc per attempt.
#[cfg_attr(not(test), allow(dead_code))]
pub fn cdtext_round_trip(sheet: &CdTextSheet) -> Vec<TrackText> {
    let Some(block) = cdtext_block(sheet) else {
        return Vec::new();
    };
    let raw = CFRetained::as_ptr(&block).as_ptr();
    let read = |index: isize, key: Option<&'static CFString>| -> String {
        let Some(key) = key else { return String::new() };
        // SAFETY: `block` is live; the value comes back borrowed (Get rule)
        // and lives as long as the block, which outlives this closure.
        let value = unsafe { DRCDTextBlockGetValue(raw, index, key) };
        let Some(value) = (unsafe { value.as_ref() }) else {
            return String::new();
        };
        value
            .downcast_ref::<CFString>()
            .map(CFString::to_string)
            .unwrap_or_default()
    };
    (0..=sheet.tracks.len() as isize)
        .map(|i| TrackText {
            performer: read(i, unsafe { kDRCDTextPerformerKey }),
            title: read(i, unsafe { kDRCDTextTitleKey }),
        })
        .collect()
}

/// The burn object for one run, with the properties both burn paths share.
///
/// `verify` maps onto `kDRBurnVerifyDiscKey`, the framework's equivalent of
/// `drutil`'s post-burn verification pass. It is set either way rather than
/// only when true, because the framework's default is `true` — leaving it out
/// would silently turn verification on for callers who asked for it off. It
/// only does anything when the tracks also declare a verification type, which
/// is why each track sets one alongside.
///
/// The disc is ejected when the burn finishes, which is the framework default
/// and is what `drutil -eject` did.
///
/// `kDRSynchronousBehaviorKey` is set and is measured to do nothing — see
/// [`run_operation`], which does not depend on it. It stays set because the
/// poll loop is correct either way: a drive that did honour it would simply
/// return its start code at the end rather than the beginning.
fn new_burn(
    device: &Device,
    verify: bool,
    text: Option<&CdTextSheet>,
) -> Result<CFRetained<CFType>, String> {
    // SAFETY: `device` is a live DRDeviceRef; the burn comes back +1.
    let burn = owned(unsafe { DRBurnCreate(device.as_ref()) }, "a burn")?;
    // Built before `pairs` so it outlives the borrow the dictionary takes.
    // Dropped only when a drive cannot write CD-TEXT, because attaching the
    // key anyway fails the whole burn — see `Device::can_write_cdtext`.
    let block = text
        .filter(|_| device.can_write_cdtext())
        .and_then(cdtext_block);
    let mut pairs: Vec<(Option<&'static CFString>, &CFType)> = vec![
        (unsafe { kDRSynchronousBehaviorKey }, CFBoolean::new(true).as_ref()),
        (unsafe { kDRBurnVerifyDiscKey }, CFBoolean::new(verify).as_ref()),
    ];
    if let Some(block) = &block {
        pairs.push((unsafe { kDRCDTextKey }, block));
        // No burn strategy is requested alongside. CD-TEXT can only be
        // written session-at-once — the header says the track-at-once
        // strategy "cannot write CD-Text" — but the engine already knows
        // that: "a burn strategy will never be used if it cannot write the
        // required data". Measured, not assumed: a burn with the block
        // attached and no `kDRBurnStrategyKey` wrote 11 PACKs to the disc.
        //
        // Asking for it anyway was tried and removed. It read as a fix
        // because it landed in the same change as the one that mattered
        // (see `trim_to_whole_packs`), and `kDRBurnStrategyIsRequiredKey`
        // would have turned a drive the engine could have satisfied another
        // way into a failed burn.
    }
    if let Some(eject) = unsafe { kDRBurnCompletionActionEject } {
        pairs.push((unsafe { kDRBurnCompletionActionKey }, eject.as_ref()));
    }
    // A laser-off rehearsal of the entire write path, for drives that
    // advertise `Test` in `drutil info`'s CD-Write line. The engine runs the
    // producer, the speed choice and the whole state machine, and the disc
    // comes out unmodified — the only way to exercise a burn end to end
    // against write-once media without spending it.
    //
    // Off unless asked for, and loud when asked for: a burn that quietly
    // wrote nothing because of a stray environment variable is the same
    // silent no-op this file already had once.
    if std::env::var_os(REHEARSE).is_some() {
        eprintln!("{REHEARSE} is set: the laser stays off and NOTHING will be written");
        pairs.push((unsafe { kDRBurnTestingKey }, CFBoolean::new(true).as_ref()));
    }
    let props = dictionary(&pairs);
    // SAFETY: both arguments are live CoreFoundation objects of the types the
    // call expects.
    unsafe { DRBurnSetProperties(CFRetained::as_ptr(&burn).as_ptr(), as_property_dict(&props)) };
    Ok(burn)
}

/// What the framework says about one track before anything is written.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackPreflight {
    /// What the producer answers `kDRTrackMessageEstimateLength` with, read
    /// back through `DRTrackEstimateLength`.
    pub blocks: u64,
    /// The throughput `DRTrackSpeedTest` measured, in kilobytes per second.
    pub kilobytes_per_second: f32,
}

/// Run the framework's own pre-flight over a built layout: ask each track how
/// long it thinks it is, and time the producer against the clock the burn will
/// hold it to. `DRTrackSpeedTest` writes its result into the track's
/// `kDRMaxBurnSpeedKey`, so the engine then picks a burn speed this producer
/// can actually sustain — the framework's designed answer to the underrun that
/// ruins a disc.
///
/// Neither call writes to the disc, which is what makes this the whole of what
/// can be checked against real blank media without spending it.
fn preflight(tracks: &[CFRetained<CFType>]) -> Vec<TrackPreflight> {
    tracks
        .iter()
        .map(|track| {
            let track = CFRetained::as_ptr(track).as_ptr();
            // SAFETY: `track` is a live DRTrackRef. Both calls run the
            // producer synchronously on this thread and touch no media.
            let blocks = unsafe { DRTrackEstimateLength(track) };
            let kilobytes_per_second = unsafe { DRTrackSpeedTest(track, 500, 2 * 1024 * 1024) };
            TrackPreflight {
                blocks,
                kilobytes_per_second,
            }
        })
        .collect()
}

/// Everything [`burn_audio`] does up to but not including the write: build the
/// layout, measure it, and time the producer. Separated out so it can be run
/// against real blank media, which is write-once and cannot be spent on
/// rehearsals.
// The rehearsal has no production caller by design: the burn runs the same
// `preflight` inline. This entry point exists so it can be run *without* the
// write that follows it, which is the only way to exercise a burn against
// write-once media without spending the disc.
#[cfg_attr(not(test), allow(dead_code))]
pub fn preflight_audio(wavs: &[PathBuf], verify: bool) -> Result<Vec<TrackPreflight>, String> {
    let (tracks, sources) = audio_tracks(wavs, verify)?;
    let _published = PublishedSources::new(sources);
    Ok(preflight(&tracks))
}

/// What a burn object reports about itself before anything has been written.
#[derive(Debug, Clone, PartialEq)]
pub struct BurnRehearsal {
    /// Whether `kDRBurnVerifyDiscKey` read back as it was set. `None` means
    /// the key was absent from the round trip, which would mean the property
    /// dictionary this file builds is not reaching the engine at all.
    pub verify_round_tripped: Option<bool>,
    /// The same for `kDRSynchronousBehaviorKey`. It round-trips and the
    /// engine ignores it — this records the round trip, which is all the
    /// property dictionary can be asked about. Whether the burn actually
    /// blocks is a question only a burn answers, and it does not.
    pub synchronous_round_tripped: Option<bool>,
    /// `kDRStatusStateKey` as the engine spells it, before any burn starts.
    pub state: Option<String>,
    /// `kDRStatusPercentCompleteKey`, likewise.
    pub percent: Option<f32>,
}

/// Everything [`burn_audio`] does to the burn object short of handing it a
/// layout: create it, apply the properties a real run would, read them back,
/// and read the status the engine reports.
///
/// Nothing here touches the media, which is the point — a property that is
/// silently dropped would otherwise only show up as a disc burned with the
/// wrong settings, and that disc may be write-once.
#[cfg_attr(not(test), allow(dead_code))]
pub fn rehearse_burn(device: &Device, verify: bool) -> Result<BurnRehearsal, String> {
    let burn = new_burn(device, verify, None)?;
    let raw = CFRetained::as_ptr(&burn).as_ptr();
    // SAFETY: `burn` is live; `DRBurnGetProperties` hands back a borrow (Get
    // rule) that lives as long as the burn does, which outlives this read.
    let props = unsafe { DRBurnGetProperties(raw).as_ref() };
    let read_back = |key: Option<&'static CFString>| {
        let props = props?;
        lookup(props, key)
            .and_then(|v| v.downcast_ref::<CFBoolean>())
            .map(CFBoolean::as_bool)
    };
    let status = burn_status(raw);
    Ok(BurnRehearsal {
        verify_round_tripped: read_back(unsafe { kDRBurnVerifyDiscKey }),
        synchronous_round_tripped: read_back(unsafe { kDRSynchronousBehaviorKey }),
        state: status
            .as_deref()
            .and_then(|s| string(s, unsafe { kDRStatusStateKey })),
        percent: status
            .as_deref()
            .and_then(|s| float(s, unsafe { kDRStatusPercentCompleteKey }))
            .map(|f| f as f32),
    })
}

/// [`preflight_audio`] for a data disc. The filesystem track brings its own
/// producer, so this measures the engine's ISO 9660 / Joliet layout rather
/// than ours.
// The rehearsal has no production caller by design: the burn runs the same
// `preflight` inline. This entry point exists so it can be run *without* the
// write that follows it, which is the only way to exercise a burn against
// write-once media without spending the disc.
#[cfg_attr(not(test), allow(dead_code))]
pub fn preflight_data(staged_dir: &Path, verify: bool) -> Result<Vec<TrackPreflight>, String> {
    Ok(preflight(&[data_track(staged_dir, verify)?]))
}

/// One Red Book audio track per staged WAV, in list order, plus the sources
/// the producer will read them from.
///
/// The five required track properties come straight from `DRCoreTrack.h`'s
/// audio enumerations; `kDRTrackLengthKey` is the payload rounded up to whole
/// 2352-byte blocks, and the producer pads the last one with silence.
fn audio_tracks(
    wavs: &[PathBuf],
    verify: bool,
) -> Result<(Vec<CFRetained<CFType>>, Vec<TrackSource>), String> {
    let mut tracks = Vec::with_capacity(wavs.len());
    let mut sources = Vec::with_capacity(wavs.len());
    for wav in wavs {
        // The length is not known until the file is measured, and the file
        // cannot be keyed to a track until the track exists — so the track is
        // built from a probe, then the probe is re-keyed to it.
        let probe = TrackSource::open(wav, std::ptr::null())?;
        let length = CFNumber::new_i64(probe.blocks as i64);
        let block_size = CFNumber::new_i64(AUDIO_BLOCK_SIZE as i64);
        let block_type = CFNumber::new_i64(BLOCK_TYPE_AUDIO);
        let data_form = CFNumber::new_i64(DATA_FORM_AUDIO);
        let session_format = CFNumber::new_i64(SESSION_FORMAT_AUDIO);
        let track_mode = CFNumber::new_i64(TRACK_MODE_AUDIO);
        let mut pairs: Vec<(Option<&'static CFString>, &CFType)> = vec![
            (unsafe { kDRTrackLengthKey }, length.as_ref()),
            (unsafe { kDRBlockSizeKey }, block_size.as_ref()),
            (unsafe { kDRBlockTypeKey }, block_type.as_ref()),
            (unsafe { kDRDataFormKey }, data_form.as_ref()),
            (unsafe { kDRSessionFormatKey }, session_format.as_ref()),
            (unsafe { kDRTrackModeKey }, track_mode.as_ref()),
        ];
        if verify {
            if let Some(checksum) = unsafe { kDRVerificationTypeChecksum } {
                pairs.push((unsafe { kDRVerificationTypeKey }, checksum.as_ref()));
            }
        }
        let props = dictionary(&pairs);
        // SAFETY: the properties dictionary is valid and the callback is a
        // real `extern "C" fn`, which is all `DRTrackCreate` requires.
        let track = owned(
            unsafe { DRTrackCreate(as_property_dict(&props), produce_track_data) },
            "an audio track",
        )?;
        sources.push(TrackSource {
            track: CFRetained::as_ptr(&track).as_ptr() as usize,
            ..probe
        });
        tracks.push(track);
    }
    Ok((tracks, sources))
}

/// One filesystem track for a staged folder — the data-disc equivalent, where
/// the engine both lays out the filesystem trees and produces their bytes.
///
/// The root folder's filesystem mask is left at `kDRFilesystemMaskDefault`,
/// which is the widest setting there is. Measured through
/// `DRTrackEstimateLength` on one small folder:
///
/// | mask | blocks |
/// |---|---|
/// | default (all bits) | 648 |
/// | ISO 9660 + Joliet + HFS+ | 216 |
/// | ISO 9660 + Joliet | 178 |
/// | HFS+ only | 189 |
/// | ISO 9660 only | 173 |
///
/// Naming a set explicitly was tried and reverted: every named set is a
/// **subset** of the default, so pinning one can only take filesystems off
/// the disc. See the port plan for the open question about what actually
/// reaches the media.
fn data_track(staged_dir: &Path, verify: bool) -> Result<CFRetained<CFType>, String> {
    let path = CFString::from_str(&staged_dir.to_string_lossy());
    let url = CFURL::with_file_system_path(None, Some(&path), CFURLPathStyle::CFURLPOSIXPathStyle, true)
        .ok_or_else(|| format!("couldn't address {}", staged_dir.display()))?;
    // SAFETY: `url` is a live file CFURL, which is what the call takes; the
    // folder comes back +1.
    let folder = owned(
        unsafe { DRFolderCreateRealWithURL(CFRetained::as_ptr(&url).as_ptr()) },
        "a disc root folder",
    )?;
    // SAFETY: `folder` is a live DRFolderRef; the track comes back +1.
    let track = owned(
        unsafe { DRFilesystemTrackCreate(CFRetained::as_ptr(&folder).as_ptr()) },
        "a data track",
    )?;

    if verify {
        // A filesystem track brings its own producer and its own properties,
        // so this sets one key on the dictionary the track already has rather
        // than replacing the whole thing the way an audio track's create call
        // does. `ProduceAgain` is the type the framework documents for data
        // CDs and DVDs: the engine runs a second production cycle and compares.
        let raw = CFRetained::as_ptr(&track).as_ptr();
        // SAFETY: `track` is live; `DRTrackGetProperties` returns its own
        // mutable dictionary, borrowed (Get rule) for as long as the track is.
        let props = unsafe { DRTrackGetProperties(raw) };
        if let (Some(props), Some(key), Some(value)) = (
            std::ptr::NonNull::new(props),
            unsafe { kDRVerificationTypeKey },
            unsafe { kDRVerificationTypeProduceAgain },
        ) {
            // SAFETY: a live mutable dictionary and two framework constants.
            unsafe {
                CFMutableDictionary::set_value(
                    Some(props.as_ref()),
                    (key as *const CFString).cast(),
                    (value as *const CFString).cast(),
                )
            };
        }
    }
    Ok(track)
}

/// Wrap tracks as a `DRBurnWriteLayout` layout: a single session, one entry
/// per track, in list order.
fn layout(tracks: &[CFRetained<CFType>]) -> CFRetained<CFArray> {
    let mut values: Vec<*const c_void> = tracks
        .iter()
        .map(|t| CFRetained::as_ptr(t).as_ptr().cast::<c_void>().cast_const())
        .collect();
    // SAFETY: the vector holds `tracks.len()` live CoreFoundation pointers,
    // and the type callbacks retain each of them.
    unsafe {
        CFArray::new(
            None,
            values.as_mut_ptr(),
            values.len() as isize,
            &kCFTypeArrayCallBacks,
        )
    }
    .expect("CFArrayCreate returned NULL")
}

/// Burn staged Red Book WAVs as an audio CD, in list order.
///
/// The whole pre-flight runs first — every track measured, the producer timed
/// against the clock the burn will hold it to — and only then does
/// `DRBurnWriteLayout` touch the disc.
pub fn burn_audio(
    device: &Device,
    wavs: &[PathBuf],
    text: Option<&CdTextSheet>,
    verify: bool,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(&str, Option<f32>),
) -> Result<(), String> {
    if wavs.is_empty() {
        return Err("nothing to burn".to_string());
    }
    let (tracks, sources) = audio_tracks(wavs, verify)?;
    let _published = PublishedSources::new(sources);
    preflight(&tracks);
    write_layout(device, &tracks, verify, text, burn_label, cancelled, progress)
}

/// Burn a staged folder as an ISO 9660 / Joliet data disc.
pub fn burn_data(
    device: &Device,
    staged_dir: &Path,
    verify: bool,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(&str, Option<f32>),
) -> Result<(), String> {
    let tracks = vec![data_track(staged_dir, verify)?];
    preflight(&tracks);
    // No CD-TEXT on a data disc: it is a CD audio field with nowhere to live
    // in an ISO 9660 layout.
    write_layout(device, &tracks, verify, None, burn_label, cancelled, progress)
}

fn write_layout(
    device: &Device,
    tracks: &[CFRetained<CFType>],
    verify: bool,
    text: Option<&CdTextSheet>,
    label: fn(&CFDictionary<CFString, CFType>) -> &'static str,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(&str, Option<f32>),
) -> Result<(), String> {
    let burn = new_burn(device, verify, text)?;
    // Both retains are held in named bindings for the whole call, because the
    // raw pointers below outlive nothing else: the scoped worker thread reads
    // them, and `run_operation` joins it before this frame ends.
    let session = layout(tracks);
    let burn_handle = Handle(CFRetained::as_ptr(&burn).as_ptr());
    let layout_handle = Handle(CFRetained::as_ptr(&session).as_ptr().cast::<CFType>());
    run_operation(
        burn_handle,
        move |burn| {
            // Bind the whole `Handle` for the same reason the worker does:
            // capturing its field would capture a bare non-`Send` pointer.
            let layout = layout_handle;
            // SAFETY: a live DRBurnRef and a CFArray of live DRTrackRefs,
            // which is the single-session multi-track layout the call takes.
            unsafe { DRBurnWriteLayout(burn, layout.0) }
        },
        burn_status,
        Some(|burn| {
            // SAFETY: `burn` is live; aborting a burn that has not started or
            // has already finished is documented to do nothing.
            unsafe { DRBurnAbort(burn) }
        }),
        label,
        cancelled,
        progress,
    )
}

/// Quick-erase the loaded rewritable disc.
///
/// The framework has no `DREraseAbort`, so a cancel raised mid-erase is
/// reported when the erase finishes rather than stopping it — a quick erase is
/// a minute or two, and there is no call that would cut it shorter.
pub fn erase(
    device: &Device,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(&str, Option<f32>),
) -> Result<(), String> {
    // SAFETY: `device` is a live DRDeviceRef; the eraser comes back +1.
    let erase = owned(unsafe { DREraseCreate(device.as_ref()) }, "an eraser")?;
    let mut pairs: Vec<(Option<&'static CFString>, &CFType)> = vec![(
        unsafe { kDRSynchronousBehaviorKey },
        CFBoolean::new(true).as_ref(),
    )];
    if let Some(quick) = unsafe { kDREraseTypeQuick } {
        pairs.push((unsafe { kDREraseTypeKey }, quick.as_ref()));
    }
    let props = dictionary(&pairs);
    // SAFETY: both arguments are live CoreFoundation objects of the expected
    // types.
    unsafe { DREraseSetProperties(CFRetained::as_ptr(&erase).as_ptr(), as_property_dict(&props)) };
    run_operation(
        Handle(CFRetained::as_ptr(&erase).as_ptr()),
        // SAFETY: `erase` is a live DREraseRef.
        |erase| unsafe { DREraseStart(erase) },
        erase_status,
        None,
        erase_label,
        cancelled,
        progress,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live: the audio pulled off the raw device must be the same audio macOS
    /// mounts as an AIFF. `cargo test --lib live_cdda_matches_mounted_aiff --
    /// --ignored`, with an audio CD loaded.
    ///
    /// Alignment is found rather than assumed. The drive has a read offset
    /// (570 sample frames on the Slimtype DS8A5SH measured here), so the two
    /// streams do not start at the same sample, and that offset belongs to
    /// the drive model rather than to this code. What must hold on any drive
    /// is that once aligned the bytes are identical: that is the difference
    /// between reading the right sectors and reading plausible garbage.
    #[test]
    #[ignore]
    fn live_cdda_matches_mounted_aiff() {
        use std::io::Read;

        let drives = crate::disc::detect::list_drives();
        let Some(drive) = drives.iter().find(|d| d.media.is_audio_cd) else {
            println!("no audio CD loaded, skipping");
            return;
        };
        let Some(mount) = drive.mount_path.as_ref() else {
            println!("audio CD is not mounted, skipping");
            return;
        };
        // The mounted name is localized past the leading number, so match on
        // the number alone.
        let Some(aiff) = std::fs::read_dir(mount)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().starts_with("1 "))
                    .unwrap_or(false)
            })
        else {
            println!("no track-1 AIFF on the mounted volume, skipping");
            return;
        };

        let out = std::env::temp_dir().join(format!("sparkamp-cdda-test-{}.wav", std::process::id()));
        cdda_track_to_wav(&drive.id, 1, &out, &mut |_| {}).expect("extract track 1");
        let wav = std::fs::read(&out).expect("read extracted wav");
        let _ = std::fs::remove_file(&out);
        assert_eq!(&wav[0..4], b"RIFF", "extracted file is not a WAV");
        let pcm = &wav[44..];

        let mut a = Vec::new();
        std::fs::File::open(&aiff)
            .expect("open aiff")
            .read_to_end(&mut a)
            .expect("read aiff");
        let ssnd = a
            .windows(4)
            .position(|w| w == b"SSND")
            .expect("aiff has an SSND chunk");
        let aiff_pcm = &a[ssnd + 16..];

        // A needle from well inside the track: the head of a track can be
        // digital silence, which would align anywhere.
        let probe = 4_000_000usize;
        assert!(aiff_pcm.len() > probe + 64, "track 1 is too short to test");
        let needle = &aiff_pcm[probe..probe + 64];
        let found = pcm
            .windows(64)
            .position(|w| w == needle)
            .expect("mounted audio not found in the raw extraction");

        let shift = found as i64 - probe as i64;
        // Compare a wide window around the match, not just the needle.
        let span = 2_000_000usize;
        let a_start = probe - span;
        let p_start = (a_start as i64 + shift) as usize;
        let len = span * 2;
        assert_eq!(
            &pcm[p_start..p_start + len],
            &aiff_pcm[a_start..a_start + len],
            "raw extraction diverges from the mounted audio once aligned"
        );
        println!(
            "raw extraction matches the mounted AIFF over {} MB, drive read offset {} frames",
            len / 1_000_000,
            shift / 4
        );
    }

    /// A WAV whose payload is `len` bytes of a recognisable ramp, so a
    /// producer that reads from the wrong offset is visible rather than
    /// plausible.
    fn staged_wav(dir: &Path, name: &str, len: usize) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let mut w = Vec::with_capacity(44 + len);
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&((36 + len) as u32).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes()); // PCM
        w.extend_from_slice(&2u16.to_le_bytes()); // stereo
        w.extend_from_slice(&44_100u32.to_le_bytes());
        w.extend_from_slice(&176_400u32.to_le_bytes());
        w.extend_from_slice(&4u16.to_le_bytes());
        w.extend_from_slice(&16u16.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&(len as u32).to_le_bytes());
        w.extend((0..len).map(|i| (i % 251) as u8));
        std::fs::write(&path, w).unwrap();
        path
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sparkamp-dr-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// The producer decides what lands on a write-once disc, so the bytes it
    /// hands back are asserted directly rather than inferred from a burn.
    #[test]
    fn producer_serves_the_payload_at_the_requested_address() {
        let dir = temp_dir("fill");
        let path = staged_wav(&dir, "01.wav", 5000);
        let source = TrackSource::open(&path, std::ptr::null()).unwrap();
        assert_eq!(source.data_offset, 44);
        assert_eq!(source.data_len, 5000);
        // 5000 bytes is 2.13 Red Book blocks, so three blocks are written and
        // the tail of the third is silence.
        assert_eq!(source.blocks, 3);
        assert_eq!(source.track_bytes(), 3 * 2352);

        let expect = |i: u64| (i % 251) as u8;

        // From the start.
        let mut out = vec![0xAAu8; 16];
        assert!(source.fill(0, &mut out));
        assert_eq!(out, (0..16).map(expect).collect::<Vec<_>>());

        // From an arbitrary address: the producer is positioned by
        // `requestedAddress` alone, with no cursor of its own to drift.
        let mut out = vec![0xAAu8; 16];
        assert!(source.fill(3000, &mut out));
        assert_eq!(out, (3000..3016).map(expect).collect::<Vec<_>>());

        // Spanning the end of the audio: real bytes, then digital silence.
        let mut out = vec![0xAAu8; 16];
        assert!(source.fill(4992, &mut out));
        assert_eq!(&out[..8], &(4992..5000).map(expect).collect::<Vec<_>>()[..]);
        assert_eq!(&out[8..], &[0u8; 8], "past the audio is silence, not stale bytes");

        // Wholly past the end — the padding that fills the last block.
        let mut out = vec![0xAAu8; 32];
        assert!(source.fill(6000, &mut out));
        assert_eq!(out, vec![0u8; 32]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `data` size larger than the file — what a pipeline that died
    /// mid-write leaves behind — must not become a request for bytes that do
    /// not exist. The file, not the header, is the authority on length.
    #[test]
    fn producer_clamps_a_declared_length_that_overruns_the_file() {
        let dir = temp_dir("clamp");
        let path = staged_wav(&dir, "01.wav", 4704);
        let mut bytes = std::fs::read(&path).unwrap();
        let truncated = bytes.len() - 1000;
        bytes.truncate(truncated);
        std::fs::write(&path, &bytes).unwrap();

        let source = TrackSource::open(&path, std::ptr::null()).unwrap();
        assert_eq!(source.data_len, 4704 - 1000, "clamped to what is on disk");
        // Every byte of the declared track is still producible, because the
        // shortfall is padded rather than read.
        let mut out = vec![0xAAu8; source.track_bytes() as usize];
        assert!(source.fill(0, &mut out));
        assert_eq!(&out[..8], &(0u64..8).map(|i| (i % 251) as u8).collect::<Vec<_>>()[..]);
        assert!(out[source.data_len as usize..].iter().all(|&b| b == 0));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A staged file that is not Red Book, or holds nothing, is refused before
    /// a track is ever built for it — the burn fails at layout time instead of
    /// writing noise.
    #[test]
    fn producer_refuses_a_file_it_cannot_serve() {
        let dir = temp_dir("refuse");
        let empty = staged_wav(&dir, "empty.wav", 0);
        let err = TrackSource::open(&empty, std::ptr::null()).unwrap_err();
        assert!(err.contains("holds no audio"), "{err}");

        let junk = dir.join("junk.wav");
        std::fs::write(&junk, b"this is not a wave file at all").unwrap();
        assert!(TrackSource::open(&junk, std::ptr::null()).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The published table is what the callback finds its source through, and
    /// it must be gone the moment the burn is over — a stale entry would be a
    /// dangling read from the framework's thread.
    #[test]
    fn published_sources_are_findable_only_while_published() {
        let dir = temp_dir("publish");
        let path = staged_wav(&dir, "01.wav", 2352);
        // A stand-in DRTrackRef: the table is keyed by address, and nothing in
        // the lookup dereferences it.
        let track = 0x1234_5678 as TrackRef;
        let source = TrackSource {
            track: track as usize,
            ..TrackSource::open(&path, std::ptr::null()).unwrap()
        };

        assert!(source_for(track).is_none(), "nothing is published yet");
        {
            let _published = PublishedSources::new(vec![source]);
            let found = source_for(track).expect("the published source is findable");
            assert_eq!(found.data_len, 2352);
            assert!(source_for(0x9999 as TrackRef).is_none(), "other tracks miss");
        }
        assert!(source_for(track).is_none(), "the table is taken down on drop");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `kDRFunctionNotSupportedErr` is the callback's "not mine"; every other
    /// non-zero return fails the burn on the spot. Declining the pregap is how
    /// the engine is left to write its own two seconds of silence.
    #[test]
    fn unhandled_messages_are_declined_rather_than_failed() {
        let unknown = u32::from_be_bytes(*b"prpr"); // kDRTrackMessageProducePreGap
        // SAFETY: neither branch reached here touches `io_param`.
        let rc = unsafe { produce_track_data(std::ptr::null(), unknown, std::ptr::null_mut()) };
        assert_eq!(rc, FUNCTION_NOT_SUPPORTED_ERR);
        assert_ne!(FUNCTION_NOT_SUPPORTED_ERR, NO_ERR);
        // The four-character codes must match the header's, or the producer
        // silently declines the messages that matter.
        assert_eq!(MSG_PRODUCE_DATA, 0x70726F64);
        assert_eq!(MSG_ESTIMATE_LENGTH, 0x65737469);
        assert_eq!(MSG_PRE_BURN, 0x70726520);
        assert_eq!(MSG_POST_BURN, 0x706F7374);
    }

    /// Build a status dictionary the way the engine hands one back, so the
    /// success/failure decision can be tested without a burn.
    fn status_dict(
        state: Option<&'static CFString>,
        error: Option<&CFDictionary>,
    ) -> CFRetained<CFDictionary> {
        let mut pairs: Vec<(Option<&'static CFString>, &CFType)> = Vec::new();
        if let Some(state) = state {
            pairs.push((unsafe { kDRStatusStateKey }, state.as_ref()));
        }
        if let Some(error) = error {
            pairs.push((unsafe { kDRErrorStatusKey }, error.as_ref()));
        }
        dictionary(&pairs)
    }

    fn as_status(dict: &CFDictionary) -> &CFDictionary<CFString, CFType> {
        // SAFETY: the generic parameters only describe how the contents are
        // read back; the pointee is the same dictionary either way.
        unsafe { &*as_property_dict(dict) }
    }

    /// LIVE-ish: dump the properties the engine puts on a filesystem track.
    /// `cargo test --lib dump_data_track_properties -- --ignored --nocapture`.
    ///
    /// No media and no burn — it builds the same track `burn_data` builds and
    /// prints what the framework configured, which is the only way to see
    /// which filesystem trees it intends to generate.
    #[test]
    #[ignore]
    fn dump_data_track_properties() {
        let dir = std::env::temp_dir().join(format!("sparkamp-fsdump-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("a.mp3"), b"x").expect("write");
        // A/B the mask: if it has any effect at all, the layout the engine
        // estimates changes with it. Identical numbers mean the request is
        // being ignored, which is the difference between a cross-platform
        // disc and a Mac-only one.
        for (name, mask) in [
            ("default", 0xFFFF_FFFFu32),
            ("HFS+ only", 1 << 3),
            ("ISO 9660 only", 1),
            ("ISO+Joliet", 1 | 1 << 1),
            ("ISO+Joliet+HFS+", 1 | 1 << 1 | 1 << 3),
        ] {
            let path = CFString::from_str(&dir.to_string_lossy());
            let url = CFURL::with_file_system_path(
                None,
                Some(&path),
                CFURLPathStyle::CFURLPOSIXPathStyle,
                true,
            )
            .expect("file url");
            // SAFETY: `url` is a live file CFURL; the folder comes back +1.
            let folder = owned(
                unsafe { DRFolderCreateRealWithURL(CFRetained::as_ptr(&url).as_ptr()) },
                "a disc root folder",
            )
            .expect("folder");
            let f = CFRetained::as_ptr(&folder).as_ptr();
            // SAFETY: `folder` is live; the mask is a documented bit field.
            unsafe { DRFSObjectSetFilesystemMask(f, mask) };
            // SAFETY: `folder` is live; the track comes back +1.
            let t = owned(unsafe { DRFilesystemTrackCreate(f) }, "a data track").expect("track");
            // SAFETY: `t` is live; this runs the layout on this thread.
            let blocks = unsafe { DRTrackEstimateLength(CFRetained::as_ptr(&t).as_ptr()) };
            println!("  mask {mask:#010x} ({name}): {blocks} blocks");
        }

        let track = data_track(&dir, false).expect("data track");
        let raw = CFRetained::as_ptr(&track).as_ptr();
        // SAFETY: `track` is live; the dictionary is borrowed (Get rule).
        let props = unsafe { DRTrackGetProperties(raw) };
        match std::ptr::NonNull::new(props) {
            Some(p) => println!("track properties: {:?}", unsafe { p.as_ref() }),
            None => println!("track has no properties dictionary"),
        }
        // SAFETY: `track` is live and this runs the producer on this thread.
        println!("estimated blocks: {}", unsafe { DRTrackEstimateLength(raw) });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A CD-TEXT answer must be trimmed to whole PACKs or the framework
    /// parser rejects all of it.
    ///
    /// The numbers are the ones measured off a real drive: the ioctl reported
    /// 204 bytes, the header declared 200, and the truth was a 4-byte header
    /// plus 11 PACKs.
    #[test]
    fn a_read_toc_answer_is_trimmed_to_whole_packs() {
        let mut buf = vec![0u8; 204];
        buf[0..2].copy_from_slice(&200u16.to_be_bytes());
        let trimmed = trim_to_whole_packs(buf);
        assert_eq!(trimmed.len(), 4 + 11 * 18, "4-byte header plus 11 whole PACKs");

        // The declared length is what bounds the answer, not the buffer. A
        // drive that hands back far more room than it filled would otherwise
        // have its zero padding read as PACKs: 240 bytes of buffer around the
        // same 200-byte answer must still be 11 PACKs, not 13.
        let mut roomy = vec![0u8; 240];
        roomy[0..2].copy_from_slice(&200u16.to_be_bytes());
        assert_eq!(trim_to_whole_packs(roomy).len(), 4 + 11 * 18);

        // A header declaring more than the drive returned must not be read
        // past the end of the buffer, and the trailing fragment of a PACK
        // that does not fit goes with it: 41 bytes hold a header and two
        // whole PACKs, with 1 byte left over.
        let mut short = vec![0u8; 41];
        short[0..2].copy_from_slice(&5000u16.to_be_bytes());
        assert_eq!(trim_to_whole_packs(short).len(), 4 + 2 * 18);

        // A bare header is a disc with no CD-TEXT: no PACKs, and nothing
        // invented to fill the gap.
        let mut bare = vec![0u8; 4];
        bare[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert_eq!(trim_to_whole_packs(bare).len(), 4);

        // Too short to even hold a header is not a header.
        assert!(trim_to_whole_packs(vec![0u8; 3]).is_empty());
        assert!(trim_to_whole_packs(Vec::new()).is_empty());
    }

    /// LIVE: a disc with no CD-TEXT must read as absent, not as a failure.
    /// `cargo test --lib live_cdtext_absence_is_quiet -- --ignored --nocapture`.
    ///
    /// The distinction is user-visible: `CdTextMiss::Absent` is the one miss
    /// the UI stays silent about, and everything else it puts on screen. A
    /// drive answering `EIO` for a disc without CD-TEXT used to reach the user
    /// as "CD-TEXT read failed — Input/output error (os error 5)".
    #[test]
    #[ignore]
    fn live_cdtext_absence_is_quiet() {
        let drives = crate::disc::detect::list_drives();
        let Some(drive) = drives.iter().find(|d| d.media.is_audio_cd) else {
            println!("no audio CD loaded, skipping");
            return;
        };
        match crate::disc::cdtext::read_cdtext(&drive.id) {
            Ok(cd) => println!("disc carries CD-TEXT: {cd:?}"),
            Err(miss) => {
                println!("miss: {miss:?}, user message: {:?}", miss.user_message());
                assert!(
                    matches!(miss, crate::disc::cdtext::CdTextMiss::Absent),
                    "a disc without CD-TEXT must read as Absent, not as a failure"
                );
                assert!(
                    miss.user_message().is_none(),
                    "Absent is the miss the UI stays quiet about"
                );
            }
        }
    }

    /// LIVE: dump the raw CD-TEXT PACKs the loaded disc answers with.
    /// `cargo test --lib live_dump_cdtext_packs -- --ignored --nocapture`.
    ///
    /// The one measurement that separates "the burn wrote no CD-TEXT" from
    /// "the burn wrote CD-TEXT this code cannot parse". Four bytes is a bare
    /// READ TOC header, meaning the disc carries none.
    #[test]
    #[ignore]
    fn live_dump_cdtext_packs() {
        let Some(node) = devices()
            .iter()
            .find_map(|d| d.status().device_node.clone())
        else {
            println!("no media loaded — skipping");
            return;
        };
        println!("reading PACKs from {node}");
        match read_cdtext_packs(&node) {
            Ok(packs) => {
                println!("{} bytes", packs.len());
                for (i, chunk) in packs.chunks(18).take(12).enumerate() {
                    let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
                    let txt: String = chunk
                        .iter()
                        .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
                        .collect();
                    println!("  {i:02}: {}  |{txt}|", hex.join(" "));
                }
            }
            Err(e) => println!("error: {e}"),
        }
    }

    /// The block the burn attaches must be a real object, and this is the
    /// only cheap way to know.
    ///
    /// `DRCDTextBlockCreateArrayFromPackList` on this OS returns a pointer
    /// that is not a valid object and segfaults on first use, so a non-NULL
    /// return proves nothing about `DRCDTextBlockCreate` either. Setting
    /// values and reading them back does.
    ///
    /// It also pins the index shift: CD-TEXT puts the disc at 0 and track N
    /// at N, so a sheet with two tracks round-trips as three entries.
    #[test]
    fn a_created_cdtext_block_holds_what_was_set() {
        let sheet = CdTextSheet {
            album: "Sparkamp CDTEXT Live".to_string(),
            artist: "Sparkamp Test".to_string(),
            tracks: vec![
                TrackText { performer: "Tone".to_string(), title: "440 Hz".to_string() },
                TrackText { performer: "Noise".to_string(), title: "Second".to_string() },
            ],
        };
        let back = cdtext_round_trip(&sheet);
        assert_eq!(back.len(), 3, "disc at 0, then one entry per track");
        assert_eq!(back[0].title, "Sparkamp CDTEXT Live", "the album is the disc title");
        assert_eq!(back[0].performer, "Sparkamp Test", "the artist is the disc performer");
        assert_eq!(back[1].title, "440 Hz");
        assert_eq!(back[1].performer, "Tone");
        assert_eq!(back[2].title, "Second");
        assert_eq!(back[2].performer, "Noise");
    }

    /// Nothing to say means no block, not an empty one. An empty block would
    /// still make the burn declare CD-TEXT and write a language block with no
    /// content in it.
    #[test]
    fn an_empty_sheet_builds_no_block() {
        let empty = CdTextSheet {
            album: String::new(),
            artist: String::new(),
            tracks: Vec::new(),
        };
        assert!(cdtext_block(&empty).is_none());
    }

    /// A status dictionary carrying a completion percentage, which
    /// `status_dict` has no reason to build.
    fn percent_dict(state: Option<&'static CFString>, percent: f64) -> CFRetained<CFDictionary> {
        let value = CFNumber::new_f64(percent);
        let pairs: Vec<(Option<&'static CFString>, &CFType)> = vec![
            (unsafe { kDRStatusStateKey }, state.unwrap().as_ref()),
            (unsafe { kDRStatusPercentCompleteKey }, value.as_ref()),
        ];
        dictionary(&pairs)
    }

    /// The poll loop stops on the state machine, so exactly the two ending
    /// states must count as endings. `Verifying` reads as still running, which
    /// is what keeps a verification pass from being cut off one poll early.
    #[test]
    fn only_done_and_failed_end_the_poll_loop() {
        for state in [unsafe { kDRStatusStateDone }, unsafe { kDRStatusStateFailed }] {
            let d = status_dict(state, None);
            assert!(terminal(Some(as_status(&d))));
        }
        let verifying = status_dict(unsafe { kDRStatusStateVerifying }, None);
        assert!(!terminal(Some(as_status(&verifying))));
        let none = status_dict(unsafe { kDRStatusStateNone }, None);
        assert!(!terminal(Some(as_status(&none))));
        // No status at all is not an ending either: the engine had not
        // answered yet, and treating silence as done is the failure that
        // reported a blank disc as burned.
        assert!(!terminal(None));
    }

    /// The stall watchdog only counts time before the engine picks the work
    /// up. Any state past `None` means it has, and the allowance is discarded
    /// — otherwise a long write would time out mid-burn.
    #[test]
    fn only_the_unstarted_states_feed_the_stall_watchdog() {
        let none = status_dict(unsafe { kDRStatusStateNone }, None);
        assert!(not_begun(Some(as_status(&none))));
        assert!(not_begun(None));
        for state in [
            unsafe { kDRStatusStateVerifying },
            unsafe { kDRStatusStateDone },
            unsafe { kDRStatusStateFailed },
        ] {
            let d = status_dict(state, None);
            assert!(!not_begun(Some(as_status(&d))));
        }
    }

    /// The engine reports `-1` for phases with no measurable progress. That is
    /// "unknown", not "nothing done": clamping it to zero would drop a
    /// progress bar back to empty while the disc is being closed.
    #[test]
    fn an_out_of_range_percentage_is_unknown_not_zero() {
        let closing = percent_dict(unsafe { kDRStatusStateVerifying }, -1.0);
        assert_eq!(fraction(as_status(&closing)), None);
        let over = percent_dict(unsafe { kDRStatusStateVerifying }, 1.5);
        assert_eq!(fraction(as_status(&over)), None);
        let half = percent_dict(unsafe { kDRStatusStateVerifying }, 0.5);
        assert_eq!(fraction(as_status(&half)), Some(0.5));
        // Both ends of the documented range are real answers.
        let start = percent_dict(unsafe { kDRStatusStateVerifying }, 0.0);
        assert_eq!(fraction(as_status(&start)), Some(0.0));
        let end = percent_dict(unsafe { kDRStatusStateVerifying }, 1.0);
        assert_eq!(fraction(as_status(&end)), Some(1.0));
        // A dictionary with no percentage at all is unknown too.
        let bare = status_dict(unsafe { kDRStatusStateVerifying }, None);
        assert_eq!(fraction(as_status(&bare)), None);
    }

    /// A burn that failed must never read as a success. The state alone is
    /// enough — an engine that reports a failure without attaching a reason
    /// still burned a coaster.
    #[test]
    fn a_failed_burn_is_never_reported_as_a_success() {
        let bare_failure = status_dict(unsafe { kDRStatusStateFailed }, None);
        let reason = failure_reason(as_status(&bare_failure))
            .expect("a failed state alone must be a failure");
        assert!(reason.starts_with("Burn failed"), "{reason}");
        assert!(reason.contains("gave no reason"), "{reason}");

        // With the engine's own words, those are used instead.
        let code = CFNumber::new_i64(0x8002_0060);
        let text = CFString::from_str("The drive ran out of data.");
        let error = dictionary(&[
            (unsafe { kDRErrorStatusErrorKey }, code.as_ref()),
            (unsafe { kDRErrorStatusErrorStringKey }, text.as_ref()),
        ]);
        let detailed = status_dict(unsafe { kDRStatusStateFailed }, Some(&error));
        let reason = failure_reason(as_status(&detailed)).expect("failure");
        assert!(reason.contains("The drive ran out of data."), "{reason}");

        // An error code with no failed state is still a failure.
        let code_only = dictionary(&[(unsafe { kDRErrorStatusErrorKey }, code.as_ref())]);
        let sneaky = status_dict(None, Some(&code_only));
        let reason = failure_reason(as_status(&sneaky)).expect("a non-zero code is a failure");
        assert!(reason.contains("underrun"), "{reason}");
    }

    /// The other direction: a healthy status must not be turned into a
    /// failure, or every good burn reports as a coaster.
    #[test]
    fn a_healthy_status_is_not_reported_as_a_failure() {
        assert_eq!(failure_reason(as_status(&status_dict(None, None))), None);
        assert_eq!(
            failure_reason(as_status(&status_dict(unsafe { kDRStatusStateDone }, None))),
            None
        );
        // An error dictionary carrying an explicit noErr is not a failure.
        let zero = CFNumber::new_i64(0);
        let no_error = dictionary(&[(unsafe { kDRErrorStatusErrorKey }, zero.as_ref())]);
        assert_eq!(
            failure_reason(as_status(&status_dict(
                unsafe { kDRStatusStateDone },
                Some(&no_error)
            ))),
            None
        );
    }

    /// Verification gets its own phase label so its percentage restarts under
    /// a new heading instead of sending the write's bar backwards.
    #[test]
    fn verifying_reports_under_its_own_label() {
        let writing = status_dict(None, None);
        assert_eq!(burn_label(as_status(&writing)), BURNING);
        let verifying = status_dict(unsafe { kDRStatusStateVerifying }, None);
        assert_eq!(burn_label(as_status(&verifying)), "Verifying…");
    }

    /// The `OSStatus` values are spelled as unsigned constants in the header
    /// and returned as a signed `OSStatus`, which is easy to get wrong by a
    /// sign.
    #[test]
    fn discrecording_error_codes_round_trip_through_osstatus() {
        assert_eq!(FUNCTION_NOT_SUPPORTED_ERR as u32, 0x8002_0067);
        assert_eq!(DATA_PRODUCTION_ERR as u32, 0x8002_0062);
        assert!(FUNCTION_NOT_SUPPORTED_ERR < 0, "high bit set means negative");
        assert!(describe_status(0x8002_0060u32 as i32).contains("underrun"));
        assert!(describe_status(1234).contains("1234"));
    }
}
