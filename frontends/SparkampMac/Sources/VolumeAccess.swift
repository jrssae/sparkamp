import SwiftUI
import AppKit

// Access to mounted volumes under the App Sandbox.
//
// A mount path grants nothing on its own. Reading anything under /Volumes
// needs a security-scoped bookmark, and a bookmark can only be taken while
// the user's own pick is live. Measured against a bundle signed with the
// shipping entitlements: every USB volume and every optical data disc returns
// EPERM on read_dir, with files.removable-media.read-write requested and
// granted. That entitlement does not do what its name suggests.
//
// The same flow serves USB devices and data discs, because they need exactly
// the same thing.

extension SparkampModel {

    /// Whether this mount cannot be read and needs the user to grant it.
    ///
    /// Answered by an actual read rather than a stored flag, so it describes
    /// the state the app is in right now. False outside a sandbox, and false
    /// once a grant is held.
    func volumeNeedsGrant(_ mount: String) -> Bool {
        guard let ctx, !mount.isEmpty else { return false }
        return mount.withCString { sparkamp_volume_needs_grant(ctx, $0) }
    }

    /// Ask for access to a mounted volume, remember it, then reload.
    ///
    /// The grant is stored the instant the picker returns. Taking the
    /// bookmark later would capture nothing, because by then the pick is no
    /// longer what is granting access.
    func grantVolumeAccess(
        volumeId: String,
        label: String,
        mount: String,
        then reload: @escaping () -> Void
    ) {
        guard let ctx, !mount.isEmpty else { return }
        let shown = label.isEmpty ? mount : label

        let panel = NSOpenPanel()
        panel.title = "Grant access to \(shown)"
        panel.message = """
            macOS requires your permission before Sparkamp can read this \
            volume. Leave it selected and choose Grant Access.
            """
        panel.prompt = "Grant Access"
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.directoryURL = URL(fileURLWithPath: mount)

        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            Task { @MainActor in
                let stored = volumeId.withCString { vid in
                    label.withCString { lbl in
                        url.path.withCString { p in
                            sparkamp_volume_grant(ctx, vid, lbl, p)
                        }
                    }
                }
                if stored != 0 {
                    // Not fatal: the volume is readable for this launch
                    // either way, it just will not be after a restart.
                    NSLog("sparkamp: could not remember access to \(shown)")
                }
                reload()
            }
        }
    }
}

/// The empty state shown when a volume is present but unreadable, with the
/// one control that fixes it.
struct VolumeGrantPrompt: View {
    let title: String
    let volumeId: String
    let label: String
    let mount: String
    let reload: () -> Void

    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager
    private var theme: SkinTheme { themeManager.currentTheme }
    private var vars: SkinVars { themeManager.currentVars }

    var body: some View {
        VStack(spacing: 8) {
            Image(systemName: "lock")
                .font(.system(size: 32))
                .foregroundStyle(theme.warningText)
            Text(title)
                .font(vars.bodyFont.weight(.semibold))
                .foregroundStyle(theme.playlistText)
            Text("macOS requires your permission before Sparkamp can read this volume. You only have to do this once.")
                .font(vars.bodyFont)
                .foregroundStyle(theme.playlistDurationText)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 360)
            Button("Grant Access…") {
                model.grantVolumeAccess(
                    volumeId: volumeId, label: label, mount: mount, then: reload)
            }
            .padding(.top, 4)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(40)
    }
}
