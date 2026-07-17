//! udisks2-backed detection of removable/external storage (Linux).
//!
//! Talks to the host udisks2 service over the **system** D-Bus to enumerate
//! mounted removable filesystems, report free space, and eject. The zbus
//! call shapes here were verified against real hardware on Bazzite: a USB
//! stick (`vfat`, removable, `ConnectionBus=usb`) is detected while internal
//! NVMe/SATA disks are classified as internal.
//!
//! The GTK Devices UI consumes this in a later phase; until then the module
//! has no in-crate caller, hence the module-wide dead-code allow (mirrors
//! `diagnostics.rs`).
#![allow(dead_code)]

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use zbus::blocking::Connection;
use zbus::blocking::fdo::ObjectManagerProxy;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use super::Device;
use super::diagnostics::DbusErrorKind;

const UDISKS: &str = "org.freedesktop.UDisks2";
const MANAGER_PATH: &str = "/org/freedesktop/UDisks2";
const FILESYSTEM_IFACE: &str = "org.freedesktop.UDisks2.Filesystem";
const BLOCK_IFACE: &str = "org.freedesktop.UDisks2.Block";
const DRIVE_IFACE: &str = "org.freedesktop.UDisks2.Drive";

type Props = HashMap<String, OwnedValue>;

// ── pure property extraction (unit-testable) ───────────────────────────────

fn prop_str(props: &Props, key: &str) -> Option<String> {
    props.get(key).and_then(|v| String::try_from(v.clone()).ok())
}
fn prop_u64(props: &Props, key: &str) -> Option<u64> {
    props.get(key).and_then(|v| u64::try_from(v.clone()).ok())
}
fn prop_bool(props: &Props, key: &str) -> Option<bool> {
    props.get(key).and_then(|v| bool::try_from(v.clone()).ok())
}
fn prop_path(props: &Props, key: &str) -> Option<OwnedObjectPath> {
    props.get(key).and_then(|v| OwnedObjectPath::try_from(v.clone()).ok())
}

/// Decode udisks2's `MountPoints` (`aay` — an array of NUL-terminated byte
/// paths) into filesystem paths. Byte-exact so paths with spaces or non-UTF-8
/// bytes survive.
pub(crate) fn decode_mountpoints(raw: &[Vec<u8>]) -> Vec<PathBuf> {
    raw.iter()
        .map(|b| {
            let bytes = if b.last() == Some(&0) { &b[..b.len() - 1] } else { &b[..] };
            PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
        })
        .collect()
}

/// Whether the drive backing a filesystem is external — removable media or a
/// hot-plug bus — so it belongs in the Devices list rather than being a fixed
/// internal disk.
pub(crate) fn is_external(removable: bool, connection_bus: &str) -> bool {
    removable || matches!(connection_bus, "usb" | "sdio" | "mmc")
}

/// Mounts that are never real removable media: the flatpak document portal and
/// other per-user runtime mounts. Their tree is the user's exported files, so
/// treating one as a device would list the whole home as "on the device".
pub(crate) fn is_pseudo_mount(mount: &std::path::Path) -> bool {
    let s = mount.to_string_lossy();
    s.starts_with("/run/user/") || s.starts_with("/run/flatpak/")
}

/// Free bytes available on the filesystem mounted at `mount`, via `statvfs`.
/// Returns 0 if the call fails (the UI shows "unknown").
fn free_bytes_at(mount: &Path) -> u64 {
    let Ok(c) = std::ffi::CString::new(mount.as_os_str().as_bytes()) else {
        return 0;
    };
    // SAFETY: `c` is a valid NUL-terminated path; `stat` is zeroed and only
    // read after a successful (0) return.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } == 0 {
        (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64)
    } else {
        0
    }
}

/// Whether the mount path is writable by this process (W_OK). Catches
/// read-only mounts and permission denials that the block-level `ReadOnly`
/// flag alone misses.
fn is_writable(mount: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(mount.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

// ── live udisks2 access ─────────────────────────────────────────────────────

/// Connect to the SYSTEM D-Bus, container-aware: inside a distrobox the
/// standard socket (`/run/dbus/system_bus_socket`) doesn't exist and
/// `DBUS_SYSTEM_BUS_ADDRESS` is unset, but the host's bus is exposed at
/// `/run/host/run/dbus/system_bus_socket`. Without this fallback every
/// udisks feature (device detection, data-disc mounting) silently fails in
/// the dev environment — found live 2026-07-17. On a normal host (or in the
/// Flatpak) the default connection succeeds and the fallback never runs.
pub(crate) fn system_bus() -> zbus::Result<Connection> {
    match Connection::system() {
        Ok(c) => Ok(c),
        Err(e) => {
            const HOST_SOCKET: &str = "/run/host/run/dbus/system_bus_socket";
            if std::env::var_os("DBUS_SYSTEM_BUS_ADDRESS").is_none()
                && std::path::Path::new(HOST_SOCKET).exists()
            {
                let addr =
                    zbus::Address::try_from("unix:path=/run/host/run/dbus/system_bus_socket")?;
                return zbus::blocking::connection::Builder::address(addr)?.build();
            }
            Err(e)
        }
    }
}

/// Enumerate currently-connected external storage with a mounted filesystem.
///
/// Returns an empty vec when nothing external is mounted. Errors only when the
/// udisks2 service itself can't be reached — map those with
/// [`classify_error`] to drive the friendly diagnostics UI.
pub fn list_devices() -> zbus::Result<Vec<Device>> {
    let conn = system_bus()?;
    let manager = ObjectManagerProxy::builder(&conn)
        .destination(UDISKS)?
        .path(MANAGER_PATH)?
        .build()?;
    let objects = manager.get_managed_objects()?;

    let mut devices = Vec::new();
    for (path, ifaces) in &objects {
        // Only objects that are a mounted filesystem are candidate devices.
        let Some(fs) = ifaces.get(FILESYSTEM_IFACE) else { continue };
        let mounts = fs
            .get("MountPoints")
            .and_then(|v| Vec::<Vec<u8>>::try_from(v.clone()).ok())
            .unwrap_or_default();
        let Some(mount_path) = decode_mountpoints(&mounts).into_iter().next() else {
            continue;
        };
        // Skip sandbox / pseudo mounts (flatpak document portal, per-user
        // runtime dirs). These can surface a mounted-filesystem object whose
        // tree is the user's exported home — never real removable media — and
        // must not be browsed as a device.
        if is_pseudo_mount(&mount_path) {
            continue;
        }

        let block = ifaces.get(BLOCK_IFACE);
        let drive_path = block.and_then(|b| prop_path(b, "Drive"));
        let (removable, connection_bus, ejectable) = drive_path
            .as_ref()
            .and_then(|dp| objects.get(dp).and_then(|di| di.get(DRIVE_IFACE)))
            .map(|d| {
                (
                    prop_bool(d, "Removable").unwrap_or(false),
                    prop_str(d, "ConnectionBus").unwrap_or_default(),
                    prop_bool(d, "Ejectable").unwrap_or(false),
                )
            })
            .unwrap_or((false, String::new(), false));

        if !is_external(removable, &connection_bus) {
            continue;
        }

        let free_bytes = free_bytes_at(&mount_path);
        // Identity: prefer the filesystem UUID; fall back to a marker-file id
        // already present on the device. Enumeration never writes a marker —
        // that happens lazily when a file is first paired to the device.
        let uuid = block.and_then(|b| prop_str(b, "IdUUID")).unwrap_or_default();
        let id = if uuid.is_empty() {
            super::marker::read_marker(&mount_path).unwrap_or_default()
        } else {
            uuid
        };
        // Read-only if the block device reports it OR we lack write access to
        // the mount (ro mount option, or permissions) — so "can't send files"
        // is detected regardless of the cause.
        let read_only = block.and_then(|b| prop_bool(b, "ReadOnly")).unwrap_or(false)
            || !is_writable(&mount_path);
        devices.push(Device {
            id,
            label: block.and_then(|b| prop_str(b, "IdLabel")).unwrap_or_default(),
            mount_path,
            fs_type: block.and_then(|b| prop_str(b, "IdType")).unwrap_or_default(),
            total_bytes: block.and_then(|b| prop_u64(b, "Size")).unwrap_or(0),
            free_bytes,
            read_only,
            ejectable,
            backend_id: path.as_str().to_string(),
            backend: super::DeviceBackend::Udisks,
            // A mounted block device always has a readable filesystem.
            fs_visible: true,
        });
    }
    // Stable, name-first ordering so the sidebar and overview list devices
    // alphabetically rather than in udisks2's arbitrary enumeration order.
    devices.sort_by(|a, b| {
        a.label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then_with(|| a.mount_path.cmp(&b.mount_path))
    });
    Ok(devices)
}

/// Safely eject the device identified by its udisks2 block object path:
/// unmount the filesystem, then best-effort power off the drive so it is safe
/// to physically remove. The unmount is the essential step; power-off failures
/// (drives that don't support it) are ignored.
pub fn eject(block_object: &str) -> zbus::Result<()> {
    // Flush pending writes so a just-copied file doesn't make the unmount fail
    // as busy, and so the data is safely on the device.
    // SAFETY: `sync()` takes no arguments and cannot fail.
    unsafe { libc::sync() };

    let conn = system_bus()?;
    let no_opts: HashMap<String, Value> = HashMap::new();

    let fs = zbus::blocking::Proxy::new(&conn, UDISKS, block_object, FILESYSTEM_IFACE)?;
    fs.call_method("Unmount", &(no_opts.clone(),))?;

    if let Ok(block) = zbus::blocking::Proxy::new(&conn, UDISKS, block_object, BLOCK_IFACE) {
        if let Ok(drive) = block.get_property::<OwnedObjectPath>("Drive") {
            if drive.as_str() != "/" {
                if let Ok(d) =
                    zbus::blocking::Proxy::new(&conn, UDISKS, drive.as_str(), DRIVE_IFACE)
                {
                    let _ = d.call_method("PowerOff", &(no_opts,));
                }
            }
        }
    }
    Ok(())
}

/// Map a zbus error from a udisks2 call to the diagnostics [`DbusErrorKind`]
/// the UI uses to choose a friendly message.
pub fn classify_error(err: &zbus::Error) -> DbusErrorKind {
    let s = err.to_string();
    if s.contains("ServiceUnknown")
        || s.contains("NameHasNoOwner")
        || s.contains("was not provided by any")
    {
        DbusErrorKind::ServiceUnknown
    } else if s.contains("NotAuthorized") {
        DbusErrorKind::NotAuthorized
    } else if s.contains("AccessDenied") || s.contains("not allowed") {
        DbusErrorKind::AccessDenied
    } else {
        DbusErrorKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_mountpoints_strips_nul_and_keeps_spaces() {
        // Mirrors the real udisks2 reply for the test USB stick.
        let raw = vec![b"/run/media/josef/LINDY TECH\0".to_vec()];
        assert_eq!(
            decode_mountpoints(&raw),
            vec![PathBuf::from("/run/media/josef/LINDY TECH")]
        );
    }

    #[test]
    fn decode_mountpoints_handles_multiple_and_no_trailing_nul() {
        let raw = vec![b"/mnt/a\0".to_vec(), b"/mnt/b".to_vec()];
        assert_eq!(
            decode_mountpoints(&raw),
            vec![PathBuf::from("/mnt/a"), PathBuf::from("/mnt/b")]
        );
        assert!(decode_mountpoints(&[]).is_empty());
    }

    #[test]
    fn is_external_matches_removable_and_hotplug_buses() {
        assert!(is_external(true, "")); // removable flag alone
        assert!(is_external(false, "usb")); // USB stick
        assert!(is_external(false, "sdio"));
        assert!(is_external(false, "mmc")); // SD card
        assert!(!is_external(false, "sata")); // internal disk
        assert!(!is_external(false, "")); // internal NVMe
    }

    #[test]
    fn is_pseudo_mount_excludes_portal_and_runtime_mounts() {
        assert!(is_pseudo_mount(&PathBuf::from("/run/user/1000/doc/4f1f2acb")));
        assert!(is_pseudo_mount(&PathBuf::from("/run/flatpak/something")));
        assert!(!is_pseudo_mount(&PathBuf::from("/run/media/josef/LINDY TECH")));
        assert!(!is_pseudo_mount(&PathBuf::from("/media/usb")));
        assert!(!is_pseudo_mount(&PathBuf::from("/mnt/stick")));
    }
}
