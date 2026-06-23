import SwiftUI
import UniformTypeIdentifiers

/// Detail page for one connected device: header with badges + capacity, the
/// device's audio files (with a "Synced from" column), and the Add / Sync /
/// Scan / Eject actions. Copy-to-device also accepts files dropped from the
/// Media Library Files table onto this view or the device's sidebar row.
///
/// Deferred to later phases: the conflict-resolution sheet (Sync currently
/// applies auto changes and reports conflicts in the status line), device
/// playlists, and delete-from-device.
struct DeviceDetailView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    let device: Device
    let theme: SkinTheme

    @State private var selection: Set<String> = []
    @State private var sortOrder: [KeyPathComparator<DeviceTrack>] =
        [KeyPathComparator(\.title)]
    @State private var showingImporter = false

    private var vars: SkinVars { themeManager.currentVars }
    private var isEjecting: Bool { model.ejectingDevices.contains(device.backendId) }
    private var actionsBusy: Bool { model.deviceBusy || isEjecting }
    private var fsUnsupported: Bool { DeviceService.fsUnsupported(device.fsType) }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider().background(theme.windowBorder)
            if device.fsVisible {
                filesTable
            } else {
                noFilesystemBanner
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(theme.background)
        .onAppear { model.loadDeviceTracks(device) }
        .onChange(of: device.backendId) { _, _ in
            selection.removeAll()
            model.loadDeviceTracks(device)
        }
        .fileImporter(
            isPresented: $showingImporter,
            allowedContentTypes: [.audio],
            allowsMultipleSelection: true
        ) { result in
            if case let .success(urls) = result {
                model.copyToDevice(device, paths: urls.map { $0.path })
            }
        }
    }

    // MARK: Header

    @ViewBuilder
    private var header: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "externaldrive.fill")
                    .foregroundStyle(theme.vars.highlight)
                Text(device.label.isEmpty ? "Untitled" : device.label)
                    .font(vars.bodyFont.weight(.semibold))
                    .foregroundStyle(theme.playlistText)
                if device.readOnly { badge("read-only", color: theme.playlistDurationText) }
                if fsUnsupported { badge("⚠ Unsupported filesystem", color: .yellow) }
                Spacer()
                actions
            }

            Text("\(device.fsType.isEmpty ? "unknown" : device.fsType) · \(device.mountPath)")
                .font(.system(size: 11))
                .foregroundStyle(theme.playlistDurationText)
                .lineLimit(1)
                .truncationMode(.middle)

            if device.fsVisible {
                CapacityBar(freeFraction: device.freeFraction,
                            accent: theme.vars.highlight,
                            track: theme.windowBorder.opacity(0.4))
                    .frame(maxWidth: 360)
                HStack(spacing: 12) {
                    Text(deviceCapacityText(device))
                        .font(.system(size: 11))
                        .foregroundStyle(theme.playlistDurationText)
                    if let s = model.deviceStatus {
                        Text(s)
                            .font(.system(size: 11))
                            .foregroundStyle(theme.playlistDurationText)
                    }
                }
            }
        }
        .padding(16)
    }

    @ViewBuilder
    private var actions: some View {
        HStack(spacing: 8) {
            if model.deviceBusy { ProgressView().controlSize(.small) }
            Button { showingImporter = true } label: {
                Label("Add Music…", systemImage: "plus")
            }
            .disabled(actionsBusy || device.readOnly || fsUnsupported || !device.fsVisible)

            Button { model.syncDevice(device) } label: {
                Label("Sync", systemImage: "arrow.triangle.2.circlepath")
            }
            .disabled(actionsBusy || !device.fsVisible)

            Button { model.scanDevice(device) } label: {
                Label("Scan", systemImage: "arrow.clockwise")
            }
            .disabled(actionsBusy || !device.fsVisible)

            if isEjecting {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.small)
                    Text("Ejecting…").font(.system(size: 11))
                        .foregroundStyle(theme.playlistDurationText)
                }
            } else if device.ejectable {
                Button { model.ejectDevice(device) } label: {
                    Label("Eject", systemImage: "eject")
                }
                .disabled(model.deviceBusy)
            }
        }
        .buttonStyle(.bordered)
        .controlSize(.small)
    }

    @ViewBuilder
    private func badge(_ text: String, color: Color) -> some View {
        Text(text)
            .font(.system(size: 10, weight: .medium))
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(RoundedRectangle(cornerRadius: 4).fill(color.opacity(0.18)))
            .foregroundStyle(color)
    }

    // MARK: Files

    private var sortedTracks: [DeviceTrack] {
        model.deviceTracks.sorted(using: sortOrder)
    }

    @ViewBuilder
    private var filesTable: some View {
        Table(sortedTracks, selection: $selection, sortOrder: $sortOrder) {
            TableColumn("Title", value: \.title) { t in
                Text(t.title.isEmpty ? URL(fileURLWithPath: t.path).lastPathComponent : t.title)
            }
            TableColumn("Artist", value: \.artist)
            TableColumn("Album", value: \.album)
            TableColumn("Duration") { t in
                Text(formatDuration(t.lengthSecs)).foregroundStyle(theme.playlistDurationText)
            }
            TableColumn("Synced from") { t in
                Text(t.syncedFrom.map { URL(fileURLWithPath: $0).lastPathComponent } ?? "—")
                    .foregroundStyle(theme.playlistDurationText)
                    .help(t.syncedFrom ?? "Not synced from this computer")
            }
        }
        .onDrop(of: [.fileURL], isTargeted: nil) { providers in
            guard device.fsVisible, !device.readOnly, !fsUnsupported else { return false }
            TrackDragPayload.resolvePaths(from: providers) { paths in
                guard !paths.isEmpty else { return }
                model.copyToDevice(device, paths: paths)
            }
            return true
        }
    }

    @ViewBuilder
    private var noFilesystemBanner: some View {
        VStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle")
                .font(.system(size: 32))
                .foregroundStyle(.yellow)
            Text("No readable storage")
                .font(vars.bodyFont.weight(.semibold))
                .foregroundStyle(theme.playlistText)
            Text("This device is connected but its storage isn't available. Reconnect it or confirm file access on the device.")
                .font(vars.bodyFont)
                .foregroundStyle(theme.playlistDurationText)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 360)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(40)
    }
}
