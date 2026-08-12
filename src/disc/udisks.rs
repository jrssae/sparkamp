//! udisks2-backed optical media typing (Linux) — the fallback for when
//! `cdrskin -minfo` can't open the drive.
//!
//! `-minfo` is the primary typing probe and stays so: it reads the lead-out,
//! which is the only source of a real capacity. But it has to open
//! `/dev/srN`, and the desktop auto-mounts every data disc it can read — so
//! on exactly the discs a user burns most often it gets:
//!
//! ```text
//! cdrskin: SORRY : Cannot open busy device '/dev/sr0'
//!          ( Most recent system error: 16 'Device or resource busy' )
//! ```
//!
//! Without typing, a burned CD-RW is indistinguishable from a burned CD-R
//! (`is_blank: false, rewritable: false`), so
//! [`crate::disc::burn::erase_decision`] refuses it and both burn buttons go
//! dead. That bites hardest right after a successful burn, when the desktop
//! mounts the disc Sparkamp just wrote.
//!
//! udisks answers over D-Bus from its own probe of the drive, so it does not
//! care that the filesystem is mounted, and it reports the three things
//! `erase_decision` actually needs: the media type, whether it's blank, and
//! whether there's a disc at all.
//!
//! What it does **not** report is the lead-out, hence no true capacity — this
//! path substitutes a nominal per-kind figure, which only feeds the burn
//! panel's "Data: x of y MB" meter. That inexactness is why this is the
//! fallback and not the primary.

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;

use zbus::blocking::fdo::ObjectManagerProxy;
use zbus::zvariant::OwnedValue;

use super::{MediaInfo, MediaKind};

const UDISKS: &str = "org.freedesktop.UDisks2";
const MANAGER_PATH: &str = "/org/freedesktop/UDisks2";
const BLOCK_IFACE: &str = "org.freedesktop.UDisks2.Block";
const DRIVE_IFACE: &str = "org.freedesktop.UDisks2.Drive";

/// Nominal capacities, since udisks reports no lead-out. A CD is the
/// standard 79:57 / 359,849 sectors of 2 KiB; a DVD±R the usual 4.7 GB.
const CD_CAPACITY_BYTES: u64 = 359_849 * 2048;
const DVD_CAPACITY_BYTES: u64 = 4_700_000_000;

/// Map udisks2's `Drive.Media` string to our media typing.
///
/// The `optical_*` vocabulary is udisks2's own; the `_plus_` variants are
/// DVD+R/RW, which burn the same way as their dash counterparts here. An
/// unrecognised or pressed disc (`optical_cd`, `optical_dvd`) types as
/// `Unknown` and not rewritable, which is the safe reading: `erase_decision`
/// then refuses it, exactly as it would for a burned CD-R.
pub(crate) fn kind_from_media(media: &str) -> (MediaKind, bool) {
    match media {
        "optical_cd_rw" => (MediaKind::CdRw, true),
        "optical_cd_r" => (MediaKind::CdR, false),
        "optical_dvd_rw" | "optical_dvd_plus_rw" => (MediaKind::DvdRw, true),
        "optical_dvd_r" | "optical_dvd_plus_r" | "optical_dvd_plus_r_dl" => {
            (MediaKind::DvdR, false)
        }
        "optical_dvd_ram" => (MediaKind::DvdRam, true),
        _ => (MediaKind::Unknown, false),
    }
}

/// Build [`MediaInfo`] from the four udisks2 `Drive` properties that matter.
/// Pure, so the mapping is unit-tested without a bus.
///
/// `free_bytes` follows the same convention `parse_minfo` uses: the whole
/// capacity when blank, zero otherwise. A non-blank rewritable disc reports
/// no free space and gets its capacity back by being erased first.
pub(crate) fn media_from_udisks(
    media: &str,
    blank: bool,
    audio_tracks: u64,
    available: bool,
) -> MediaInfo {
    if !available {
        return MediaInfo::none();
    }
    let (kind, rewritable) = kind_from_media(media);
    let capacity_bytes = match kind {
        MediaKind::CdR | MediaKind::CdRw => CD_CAPACITY_BYTES,
        MediaKind::DvdR | MediaKind::DvdRw | MediaKind::DvdRam => DVD_CAPACITY_BYTES,
        // Pressed or unrecognised: no nominal figure worth inventing. The
        // burn panel treats 0 as "capacity unknown" and skips the check —
        // harmless, since this disc types as un-erasable anyway.
        MediaKind::Unknown => 0,
    };
    MediaInfo {
        present: true,
        is_audio_cd: audio_tracks > 0,
        is_blank: blank,
        rewritable,
        kind,
        free_bytes: if blank { capacity_bytes } else { 0 },
        capacity_bytes,
        typing_unknown: false,
    }
}

type Props = HashMap<String, OwnedValue>;

fn prop_str(props: &Props, key: &str) -> Option<String> {
    props.get(key).and_then(|v| String::try_from(v.clone()).ok())
}
fn prop_bool(props: &Props, key: &str) -> Option<bool> {
    props.get(key).and_then(|v| bool::try_from(v.clone()).ok())
}
fn prop_u64(props: &Props, key: &str) -> Option<u64> {
    props.get(key).and_then(|v| u64::try_from(v.clone()).ok())
}

/// Decode a udisks2 `ay` byte-path property (NUL-terminated) — `Block.Device`
/// here. Byte-exact, like the device code's `MountPoints` decoder.
fn prop_device_path(props: &Props, key: &str) -> Option<String> {
    let raw = props
        .get(key)
        .and_then(|v| Vec::<u8>::try_from(v.clone()).ok())?;
    let bytes = if raw.last() == Some(&0) { &raw[..raw.len() - 1] } else { &raw[..] };
    Some(
        std::ffi::OsStr::from_bytes(bytes)
            .to_string_lossy()
            .into_owned(),
    )
}

/// Type the media in the drive at device `node` (e.g. `/dev/sr0`) by asking
/// udisks2, which works with the disc mounted.
///
/// `None` when udisks isn't reachable, the node isn't among its block
/// devices, its drive object is absent, or the drive reports no media — all
/// of which leave the caller to fall back exactly as it did before.
pub fn optical_media(node: &str) -> Option<MediaInfo> {
    let conn = crate::devices::detect::system_bus().ok()?;
    let objects = ObjectManagerProxy::builder(&conn)
        .destination(UDISKS)
        .ok()?
        .path(MANAGER_PATH)
        .ok()?
        .build()
        .ok()?
        .get_managed_objects()
        .ok()?;

    // Find the block object for this node, then the drive backing it — the
    // optical properties live on the drive, not the block device.
    let drive_path = objects.values().find_map(|ifaces| {
        let block = ifaces.get(BLOCK_IFACE)?;
        let dev = prop_device_path(block, "Device")?;
        (dev == node).then(|| block.get("Drive").cloned())?
    })?;
    let drive_path = zbus::zvariant::OwnedObjectPath::try_from(drive_path).ok()?;
    let drive = objects.get(&drive_path)?.get(DRIVE_IFACE)?;

    if !prop_bool(drive, "Optical").unwrap_or(false) {
        return None;
    }
    Some(media_from_udisks(
        &prop_str(drive, "Media").unwrap_or_default(),
        prop_bool(drive, "OpticalBlank").unwrap_or(false),
        prop_u64(drive, "OpticalNumAudioTracks").unwrap_or(0),
        prop_bool(drive, "MediaAvailable").unwrap_or(false),
    ))
    .filter(|m| m.present)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_strings_map_to_erasability() {
        // The case this module exists for: a burned CD-RW, mounted, that
        // -minfo can't reach. Rewritable is what lets erase_decision offer
        // the erase-and-burn confirm instead of refusing.
        let m = media_from_udisks("optical_cd_rw", false, 0, true);
        assert!(m.rewritable && !m.is_blank && m.present);
        assert_eq!(m.kind, MediaKind::CdRw);
        assert_eq!(
            crate::disc::burn::erase_decision(&crate::disc::OpticalDrive {
                id: "/dev/sr0".into(),
                label: "T".into(),
                media: m.clone(),
                toc: None,
                mount_path: None,
            }),
            crate::disc::burn::EraseDecision::EraseAfterConfirm
        );
        // Non-blank media reports no free space; erasing gets it back.
        assert_eq!(m.free_bytes, 0);
        assert_eq!(m.capacity_bytes, CD_CAPACITY_BYTES);

        // Write-once with content stays refused — the fallback must not turn
        // a CD-R into something we offer to erase.
        let r = media_from_udisks("optical_cd_r", false, 0, true);
        assert!(!r.rewritable);

        // Blank: free == capacity, matching parse_minfo's convention.
        let b = media_from_udisks("optical_cd_r", true, 0, true);
        assert!(b.is_blank);
        assert_eq!(b.free_bytes, b.capacity_bytes);

        // DVD flavours, including the plus variants.
        assert_eq!(kind_from_media("optical_dvd_plus_rw"), (MediaKind::DvdRw, true));
        assert_eq!(kind_from_media("optical_dvd_plus_r_dl"), (MediaKind::DvdR, false));
        assert_eq!(kind_from_media("optical_dvd_ram"), (MediaKind::DvdRam, true));
        assert_eq!(
            media_from_udisks("optical_dvd_rw", true, 0, true).capacity_bytes,
            DVD_CAPACITY_BYTES
        );

        // Pressed / unknown: no invented capacity, and not erasable.
        let p = media_from_udisks("optical_cd", false, 0, true);
        assert_eq!(p.kind, MediaKind::Unknown);
        assert!(!p.rewritable);
        assert_eq!(p.capacity_bytes, 0);
    }

    #[test]
    fn audio_and_empty_tray() {
        assert!(media_from_udisks("optical_cd", false, 12, true).is_audio_cd);
        assert!(!media_from_udisks("optical_cd_rw", false, 0, true).is_audio_cd);
        // No media: the caller must see "nothing here", not a typed disc.
        assert!(!media_from_udisks("optical_cd_rw", false, 0, false).present);
    }

    /// Typing from udisks is by definition typing we *have* — the flag exists
    /// only for the path where no probe answered at all.
    #[test]
    fn udisks_typing_is_never_flagged_unknown() {
        assert!(!media_from_udisks("optical_cd_rw", false, 0, true).typing_unknown);
    }
}
