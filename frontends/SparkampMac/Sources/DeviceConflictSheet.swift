import SwiftUI
import AppKit

// MARK: - Two-way sync conflict resolution sheet
//
// Shown when a sync plan comes back with both-changed songs (edited on this
// computer AND on the device since the last sync). The user picks which copy to
// keep per song — or in bulk — then Apply sends the choices back through
// apply_sync. Cancel applies nothing for the conflicts (the plan's auto pairs
// still apply on the same run, so the status reports them as skipped).
//
// Mirrors the GTK conflict dialog: per-song side picker, a two-column field
// diff showing only the fields that differ, and artwork thumbnails per side.

struct DeviceConflictSheet: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    let device: Device
    let plan: SyncPlan

    /// deviceRelpath → chosen side. Unset until the user picks; Apply stays
    /// disabled until every conflict has an entry (or a bulk button was used).
    @State private var choices: [String: KeepSide] = [:]

    private var theme: SkinTheme { themeManager.currentTheme }
    private var vars: SkinVars { themeManager.currentVars }
    private var deviceName: String { device.label.isEmpty ? "device" : device.label }

    private var allResolved: Bool {
        plan.conflicts.allSatisfy { choices[$0.pair.deviceRelpath] != nil }
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().background(theme.windowBorder)
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(plan.conflicts) { conflictCard($0) }
                }
                .padding(12)
            }
            Divider().background(theme.windowBorder)
            footer
        }
        .frame(width: 660, height: 540)
        .background(theme.background)
        .preferredColorScheme(themeManager.preferredColorScheme)
    }

    // MARK: Header

    private var header: some View {
        let n = plan.conflicts.count
        return HStack(alignment: .top, spacing: 8) {
            Image(systemName: "arrow.triangle.2.circlepath")
                .foregroundStyle(theme.titleText)
            VStack(alignment: .leading, spacing: 2) {
                Text("Resolve Sync Conflicts")
                    .font(vars.bodyFont.weight(.semibold))
                    .foregroundStyle(theme.titleText)
                Text("\(n) song\(n == 1 ? "" : "s") changed on both this computer and \(deviceName) since the last sync. Choose which copy to keep.")
                    .font(vars.bodyFont)
                    .foregroundStyle(theme.playlistDurationText)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer()
        }
        .padding(12)
        .background(theme.background)
    }

    // MARK: One conflict

    private func conflictCard(_ item: ConflictItem) -> some View {
        let picked = choices[item.pair.deviceRelpath]
        return VStack(alignment: .leading, spacing: 8) {
            Text(item.song)
                .font(vars.bodyFont.weight(.semibold))
                .foregroundStyle(theme.titleText)
                .lineLimit(1)
                .truncationMode(.middle)

            HStack(alignment: .top, spacing: 0) {
                sideColumn(item, side: .computer, title: "On this computer", picked: picked)
                Divider().background(theme.windowBorder)
                sideColumn(item, side: .device, title: "On \(deviceName)", picked: picked)
            }
            .background(theme.lcdBackground)
            .clipShape(RoundedRectangle(cornerRadius: 6))
            .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.windowBorder, lineWidth: 1))
        }
        .padding(10)
        .background(theme.background)
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.windowBorder, lineWidth: 1))
    }

    private func sideColumn(
        _ item: ConflictItem, side: KeepSide, title: String, picked: KeepSide?
    ) -> some View {
        let isPicked = picked == side
        return VStack(alignment: .leading, spacing: 6) {
            Button {
                choices[item.pair.deviceRelpath] = side
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: isPicked ? "largecircle.fill.circle" : "circle")
                    Text(title).font(vars.bodyFont.weight(.medium))
                    Spacer(minLength: 0)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(isPicked ? theme.titleText : theme.playlistDurationText)

            ConflictArtworkThumb(
                device: device,
                devRelpath: item.pair.deviceRelpath,
                side: side == .computer ? 0 : 1)

            ForEach(item.diffs) { d in
                VStack(alignment: .leading, spacing: 1) {
                    Text(d.label)
                        .font(vars.smallMonospaceFont)
                        .foregroundStyle(theme.playlistDurationText)
                    Text((side == .computer ? d.computer : d.device).isEmpty
                         ? "—" : (side == .computer ? d.computer : d.device))
                        .font(vars.bodyFont)
                        .foregroundStyle(theme.titleText)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(8)
        .background(isPicked ? theme.playlistSelectedBg : Color.clear)
    }

    // MARK: Footer

    private var footer: some View {
        HStack(spacing: 10) {
            Button("Keep all from computer") {
                for c in plan.conflicts { choices[c.pair.deviceRelpath] = .computer }
            }
            .buttonStyle(.bordered)
            Button("Keep all from \(deviceName)") {
                for c in plan.conflicts { choices[c.pair.deviceRelpath] = .device }
            }
            .buttonStyle(.bordered)

            Spacer()

            Button("Cancel") {
                // Auto pairs from the same run still apply; conflicts are skipped.
                model.resolveSyncConflicts(choices: [])
            }
            .keyboardShortcut(.cancelAction)

            Button("Apply") {
                let resolved: [ConflictChoice] = plan.conflicts.compactMap { c in
                    guard let side = choices[c.pair.deviceRelpath] else { return nil }
                    return ConflictChoice(devPath: c.pair.deviceRelpath, keep: side)
                }
                model.resolveSyncConflicts(choices: resolved)
            }
            .keyboardShortcut(.defaultAction)
            .disabled(!allResolved)
        }
        .padding(12)
        .background(theme.background)
    }
}

// MARK: - Async artwork thumbnail (one side of a conflict)

/// Loads a conflict side's embedded artwork off the main thread (file IO via
/// `sparkamp_device_conflict_artwork`) and renders a small thumbnail. Shows
/// nothing when that side has no artwork.
private struct ConflictArtworkThumb: View {
    let device: Device
    let devRelpath: String
    let side: Int

    @State private var image: NSImage? = nil
    @State private var loaded = false

    var body: some View {
        Group {
            if let img = image {
                Image(nsImage: img)
                    .resizable()
                    .scaledToFit()
                    .frame(width: 56, height: 56)
                    .cornerRadius(4)
            }
        }
        .onAppear(perform: load)
    }

    private func load() {
        guard !loaded else { return }
        loaded = true
        DispatchQueue.global(qos: .userInitiated).async {
            let img = DeviceService.conflictArtwork(
                device: device, devRelpath: devRelpath, side: side)
            DispatchQueue.main.async { self.image = img }
        }
    }
}
