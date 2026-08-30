//! Whether a device or disc mount root can be read at all.
//!
//! The walkers in [`super::browse`] and [`crate::disc::mount`] deliberately
//! skip directories they cannot read, so that a partly-readable device still
//! lists the files it can reach. That is right for a subdirectory and wrong
//! for the mount root: applied there it turns "permission denied" into "this
//! device is empty", which is what a Flatpak with no grant for `/run/media`
//! looks like from the inside — the device appears in the sidebar (udisks2
//! answers over D-Bus just fine) and then shows nothing at all.
//!
//! So the root is checked once, up front, and the walkers keep their existing
//! behaviour untouched.

use std::io;
use std::path::Path;

/// The result of trying to read a mount root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountAccess {
    /// The root listed successfully. Says nothing about how many files are on
    /// it — an empty device is readable.
    Readable,
    /// Listing was refused. Inside a Flatpak this normally means the sandbox
    /// has no grant for the path, not that the medium is faulty.
    PermissionDenied,
    /// Nothing is at the path. Outside a sandbox that means the medium was
    /// removed between the sidebar listing it and the read. Inside one it
    /// usually means the opposite — bwrap presents a path the sandbox has no
    /// grant for as ENOENT rather than EACCES, so a perfectly healthy mount
    /// simply is not there to look at.
    NotFound,
    /// Some other I/O failure — a dying stick, a bad disc, a stalled mount.
    Unreadable(io::ErrorKind),
}

/// Try to read `mount` as a directory and classify the outcome.
pub fn check(mount: &Path) -> MountAccess {
    match std::fs::read_dir(mount) {
        Ok(_) => MountAccess::Readable,
        Err(e) => match e.kind() {
            io::ErrorKind::PermissionDenied => MountAccess::PermissionDenied,
            io::ErrorKind::NotFound => MountAccess::NotFound,
            other => MountAccess::Unreadable(other),
        },
    }
}

/// What the user is looking at, so the message names it correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Medium {
    Device,
    Disc,
}

impl Medium {
    fn noun(self) -> &'static str {
        match self {
            Medium::Device => "device",
            Medium::Disc => "disc",
        }
    }
}

/// A sentence to show the user, or `None` when there is nothing wrong.
///
/// `sandboxed` selects the remedy: inside a Flatpak the fix is a permission,
/// outside it is the medium or its mount. `medium` selects the noun, so a
/// disc drive is never described as a device.
pub fn message(access: MountAccess, sandboxed: bool, medium: Medium) -> Option<String> {
    let noun = medium.noun();
    match access {
        MountAccess::Readable => None,
        MountAccess::PermissionDenied if sandboxed => Some(format!(
            "Can\u{2019}t read this {noun} \u{2014} Sparkamp doesn\u{2019}t have permission to reach \
             removable media. Grant access to /run/media in Flatseal, then Retry."
        )),
        MountAccess::PermissionDenied => Some(format!(
            "Can\u{2019}t read this {noun} \u{2014} permission denied. Check that your user can \
             open the mounted folder."
        )),
        // Inside a sandbox this is the ordinary "no grant" case, not a
        // vanished medium: bwrap hides an ungranted path as ENOENT, and the
        // path came from udisks2 having just reported it mounted. Saying "no
        // longer mounted" here is the one answer we know to be wrong.
        MountAccess::NotFound if sandboxed => Some(format!(
            "Can\u{2019}t read this {noun} \u{2014} Sparkamp doesn\u{2019}t have permission to reach \
             removable media, so the mount is invisible to it. Grant access to \
             /run/media in Flatseal, then Retry."
        )),
        MountAccess::NotFound => Some(format!("This {noun} is no longer mounted.")),
        MountAccess::Unreadable(kind) => Some(format!(
            "Can\u{2019}t read this {noun} \u{2014} the mount reported {kind}."
        )),
    }
}

/// Whether this process is running inside a Flatpak sandbox.
///
/// Reuses the same `/.flatpak-info` probe the udisks2 diagnostics already
/// depend on, so both banners agree about where they are.
pub fn in_flatpak() -> bool {
    !super::diagnostics::read_flatpak_info().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_readable_directory_reads_as_readable() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(check(dir.path()), MountAccess::Readable);
    }

    #[test]
    fn an_empty_directory_is_readable_not_an_error() {
        // The distinction the whole module exists for: nothing on the device
        // is not the same as no access to the device.
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(check(dir.path()), MountAccess::Readable);
        assert_eq!(message(check(dir.path()), true, Medium::Device), None);
    }

    #[test]
    fn a_path_that_is_gone_reads_as_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("no-such-mount");
        assert_eq!(check(&missing), MountAccess::NotFound);
    }

    #[test]
    fn a_directory_that_cannot_be_listed_reads_as_permission_denied() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).expect("create");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
            .expect("chmod");

        // root ignores the mode bits, which would make this test pass without
        // testing anything. Assert the precondition instead of skipping.
        let still_readable = std::fs::read_dir(&locked).is_ok();
        // Restore before any assertion so the tempdir can always be cleaned up.
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
        assert!(
            !still_readable,
            "this test is meaningless as root — run the suite as an ordinary user"
        );

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
            .expect("chmod");
        let access = check(&locked);
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));

        assert_eq!(access, MountAccess::PermissionDenied);
    }

    #[test]
    fn the_wording_names_the_medium_the_user_is_looking_at() {
        let dev = message(MountAccess::PermissionDenied, true, Medium::Device)
            .expect("a message");
        let disc = message(MountAccess::PermissionDenied, true, Medium::Disc)
            .expect("a message");

        assert!(dev.contains("device"), "got: {dev}");
        assert!(!dev.contains("disc"), "a device is not a disc: {dev}");
        assert!(disc.contains("disc"), "got: {disc}");
    }

    #[test]
    fn a_disc_that_vanished_says_disc_not_device() {
        let msg = message(MountAccess::NotFound, false, Medium::Disc).expect("a message");
        assert!(msg.contains("disc"), "got: {msg}");
    }

    #[test]
    fn a_denied_mount_names_the_permission_when_sandboxed() {
        let msg = message(MountAccess::PermissionDenied, true, Medium::Device).expect("a message");
        assert!(msg.contains("Flatseal"), "got: {msg}");
        assert!(msg.contains("/run/media"), "got: {msg}");
    }

    #[test]
    fn a_denied_mount_outside_a_sandbox_does_not_blame_flatpak() {
        let msg = message(MountAccess::PermissionDenied, false, Medium::Device).expect("a message");
        assert!(!msg.contains("Flatseal"), "got: {msg}");
        assert!(!msg.to_lowercase().contains("flatpak"), "got: {msg}");
    }

    #[test]
    fn a_vanished_mount_outside_a_sandbox_says_it_is_gone() {
        let msg = message(MountAccess::NotFound, false, Medium::Device).expect("a message");
        assert!(msg.to_lowercase().contains("mounted"), "got: {msg}");
    }

    #[test]
    fn inside_a_sandbox_a_missing_mount_is_a_permission_problem_not_a_missing_disk() {
        // bwrap presents a path the sandbox was never granted as ENOENT, not
        // EACCES. The path we are checking came from udisks2, which had just
        // reported the device mounted there — so "it is gone" is the one
        // explanation we know to be false, and it sends the user looking for
        // the wrong problem.
        let msg = message(MountAccess::NotFound, true, Medium::Device).expect("a message");
        assert!(msg.contains("/run/media"), "got: {msg}");
        assert!(msg.contains("Flatseal"), "got: {msg}");
        assert!(
            !msg.to_lowercase().contains("no longer mounted"),
            "must not claim the device vanished: {msg}"
        );
    }

    #[test]
    fn the_sandbox_wording_still_names_the_medium() {
        let msg = message(MountAccess::NotFound, true, Medium::Disc).expect("a message");
        assert!(msg.contains("disc"), "got: {msg}");
    }

    #[test]
    fn another_io_failure_still_produces_a_message() {
        let msg = message(MountAccess::Unreadable(io::ErrorKind::TimedOut), true, Medium::Device)
            .expect("a message");
        assert!(!msg.is_empty());
    }
}
