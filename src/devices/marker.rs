//! Stable device identity fallback.
//!
//! Most filesystems expose a volume UUID we can key sync pairs on. Some (a few
//! FAT/exFAT formats, or freshly-formatted media) don't. For those we write a
//! small hidden id file at the device's mount root and read it back to
//! recognize the same device across reconnects and remounts — independent of
//! mount path, label, or filesystem type.
//!
//! Enumeration only ever *reads* the marker (no side effects on a passive
//! scan, and read-only media stays untouched). The marker is *written* lazily
//! the first time a file is paired to the device (a later phase).

// Used by the Linux detector now and the transfer/sync + macOS paths later;
// unreferenced in the macOS bin until then.
#![allow(dead_code)]

use std::path::Path;

/// Hidden id file written at the device mount root.
const MARKER_FILE: &str = ".sparkamp-device-id";

/// Return the marker id already present at `mount`, or `None` when absent,
/// empty, or unreadable.
pub fn read_marker(mount: &Path) -> Option<String> {
    std::fs::read_to_string(mount.join(MARKER_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Return the device's marker id, creating one if the device doesn't have it
/// yet. Fails if the marker can't be written (e.g. a read-only mount).
pub fn ensure_marker(mount: &Path) -> std::io::Result<String> {
    if let Some(id) = read_marker(mount) {
        return Ok(id);
    }
    let id = new_id();
    std::fs::write(mount.join(MARKER_FILE), format!("{id}\n"))?;
    Ok(id)
}

/// A fresh 128-bit random id, hex-encoded with a `sparkamp-` prefix. Uses the
/// existing `rand` dependency rather than pulling in a uuid crate.
fn new_id() -> String {
    let hi: u64 = rand::random();
    let lo: u64 = rand::random();
    format!("sparkamp-{hi:016x}{lo:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_marker_absent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_marker(dir.path()), None);
    }

    #[test]
    fn ensure_marker_creates_then_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let id1 = ensure_marker(dir.path()).unwrap();
        assert!(id1.starts_with("sparkamp-"));
        assert_eq!(id1.len(), "sparkamp-".len() + 32);
        // Reading it back yields the same id.
        assert_eq!(read_marker(dir.path()).as_deref(), Some(id1.as_str()));
        // A second ensure does not regenerate it.
        assert_eq!(ensure_marker(dir.path()).unwrap(), id1);
    }

    #[test]
    fn read_marker_trims_and_rejects_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MARKER_FILE), "  abc123  \n").unwrap();
        assert_eq!(read_marker(dir.path()).as_deref(), Some("abc123"));
        std::fs::write(dir.path().join(MARKER_FILE), "\n  \n").unwrap();
        assert_eq!(read_marker(dir.path()), None);
    }
}
