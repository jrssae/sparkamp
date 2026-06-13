//! Friendly diagnosis of why the system disk service (udisks2) is unreachable.
//!
//! Turns three local, sandbox-readable signals — our own `/.flatpak-info`
//! grants, `/etc/os-release` (+ `/run/ostree-booted`), and the D-Bus error
//! kind — into a single [`Diagnosis`] the UI renders as a one-line message
//! with one action button. Pure classification: callers read the input
//! strings and pass them in, so every branch is unit-testable without a live
//! bus or a specific host.

// The GTK frontend wires these in a later phase; until then the whole module
// is unreferenced in non-test builds.
#![allow(dead_code)]

/// The kind of D-Bus failure observed when talking to udisks2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbusErrorKind {
    /// The udisks2 name could not be reached or activated.
    ServiceUnknown,
    /// The bus or call was refused (e.g. filtered by the Flatpak proxy).
    AccessDenied,
    /// udisks2 answered but refused the action (polkit).
    NotAuthorized,
    /// Any other failure.
    Other,
}

/// Distro identity and whether the OS is image-based (Bazzite, Silverblue,
/// Fedora Atomic, SteamOS), where udisks2 ships in the base image so
/// "install the package" advice would be wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistroInfo {
    pub id: String,
    pub immutable: bool,
}

/// The classified outcome the UI renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Diagnosis {
    /// We weren't granted the udisks2 talk-name, or it was removed. Fix via
    /// the System-Bus permission (Flatseal / KDE settings).
    PermissionOff,
    /// udisks2 genuinely isn't installed — only possible on a traditional
    /// (non-image-based) distro.
    NotInstalled,
    /// Reached udisks2 but eject was refused, or eject isn't available; the
    /// user should eject via their file browser.
    EjectUnavailable,
}

/// Extract the system-bus talk-names this app was granted, from the contents
/// of `/.flatpak-info`. Empty when not sandboxed or none were granted.
pub fn parse_flatpak_info_talk_names(flatpak_info: &str) -> Vec<String> {
    for line in flatpak_info.lines() {
        if let Some(rest) = line.trim().strip_prefix("system-talk-name=") {
            return rest
                .split(';')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
        }
    }
    Vec::new()
}

/// Whether `/.flatpak-info` shows the udisks2 system talk-name as granted.
pub fn has_udisks_grant(flatpak_info: &str) -> bool {
    parse_flatpak_info_talk_names(flatpak_info)
        .iter()
        .any(|n| n == "org.freedesktop.UDisks2")
}

/// Parse `/etc/os-release` (and whether `/run/ostree-booted` exists) into a
/// [`DistroInfo`].
pub fn parse_os_release(os_release: &str, ostree_booted: bool) -> DistroInfo {
    let mut id = String::new();
    let mut variant = String::new();
    for line in os_release.lines() {
        if let Some(v) = line.strip_prefix("ID=") {
            id = unquote(v);
        } else if let Some(v) = line.strip_prefix("VARIANT_ID=") {
            variant = unquote(v);
        }
    }
    let immutable = ostree_booted
        || id == "steamos"
        || id == "bazzite"
        || variant.contains("silverblue")
        || variant.contains("kinoite")
        || variant.contains("atomic");
    DistroInfo { id, immutable }
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

/// Decide which message to show.
///
/// - Talk-name missing from our grant, or the bus refused us → `PermissionOff`
///   (a packaging / override issue), regardless of distro.
/// - Granted but the service is unknown → `NotInstalled` on a traditional
///   distro, but `PermissionOff` on an immutable one (the daemon is in the base
///   image, so the real cause is almost always the permission / proxy).
/// - The action was refused by policy → `EjectUnavailable`.
pub fn classify(granted: bool, distro: &DistroInfo, err: DbusErrorKind) -> Diagnosis {
    match err {
        DbusErrorKind::NotAuthorized => Diagnosis::EjectUnavailable,
        DbusErrorKind::AccessDenied => Diagnosis::PermissionOff,
        DbusErrorKind::ServiceUnknown => {
            if !granted || distro.immutable {
                Diagnosis::PermissionOff
            } else {
                Diagnosis::NotInstalled
            }
        }
        DbusErrorKind::Other => {
            if granted {
                Diagnosis::EjectUnavailable
            } else {
                Diagnosis::PermissionOff
            }
        }
    }
}

// ── runtime IO wrappers (thin; not unit-tested) ────────────────────────────

/// Read `/.flatpak-info`; empty string when not sandboxed.
pub fn read_flatpak_info() -> String {
    std::fs::read_to_string("/.flatpak-info").unwrap_or_default()
}

/// Read the host distro identity from `/etc/os-release` + `/run/ostree-booted`.
pub fn read_distro_info() -> DistroInfo {
    let os = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let ostree = std::path::Path::new("/run/ostree-booted").exists();
    parse_os_release(&os, ostree)
}

/// Compose the live signals with an observed error kind into a [`Diagnosis`].
pub fn diagnose(err: DbusErrorKind) -> Diagnosis {
    let info = read_flatpak_info();
    let distro = read_distro_info();
    classify(has_udisks_grant(&info), &distro, err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_granted_talk_names() {
        let info = "[Application]\nname=dev.sparkamp.Sparkamp\n[Context]\n\
                    system-talk-name=org.freedesktop.UDisks2;org.freedesktop.Other;\n";
        assert!(has_udisks_grant(info));
        assert_eq!(
            parse_flatpak_info_talk_names(info),
            vec![
                "org.freedesktop.UDisks2".to_string(),
                "org.freedesktop.Other".to_string()
            ]
        );
    }

    #[test]
    fn no_grant_when_absent_or_unsandboxed() {
        assert!(!has_udisks_grant(""));
        assert!(!has_udisks_grant("[Context]\nsystem-talk-name=org.freedesktop.Other;\n"));
    }

    #[test]
    fn detects_immutable_distros() {
        assert!(parse_os_release("ID=bazzite\n", false).immutable);
        assert!(parse_os_release("ID=steamos\n", false).immutable);
        assert!(parse_os_release("ID=fedora\nVARIANT_ID=silverblue\n", false).immutable);
        assert!(parse_os_release("ID=fedora\n", true).immutable); // ostree-booted
        assert!(!parse_os_release("ID=fedora\nVARIANT_ID=workstation\n", false).immutable);
        assert!(!parse_os_release("ID=arch\n", false).immutable);
    }

    #[test]
    fn classify_permission_off_when_not_granted() {
        let arch = DistroInfo { id: "arch".into(), immutable: false };
        assert_eq!(
            classify(false, &arch, DbusErrorKind::ServiceUnknown),
            Diagnosis::PermissionOff
        );
        assert_eq!(
            classify(true, &arch, DbusErrorKind::AccessDenied),
            Diagnosis::PermissionOff
        );
    }

    #[test]
    fn classify_not_installed_only_on_traditional_distro() {
        let arch = DistroInfo { id: "arch".into(), immutable: false };
        let bazzite = DistroInfo { id: "bazzite".into(), immutable: true };
        assert_eq!(
            classify(true, &arch, DbusErrorKind::ServiceUnknown),
            Diagnosis::NotInstalled
        );
        assert_eq!(
            classify(true, &bazzite, DbusErrorKind::ServiceUnknown),
            Diagnosis::PermissionOff
        );
    }

    #[test]
    fn classify_eject_unavailable_on_polkit_denial() {
        let arch = DistroInfo { id: "arch".into(), immutable: false };
        assert_eq!(
            classify(true, &arch, DbusErrorKind::NotAuthorized),
            Diagnosis::EjectUnavailable
        );
    }
}
