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
    // Column show/hide + reorder/resize, persisted (the device file table's
    // own config; the header right-click toggles columns natively).
    @State private var columnCustomization = TableColumnCustomization<DeviceTrack>()
    @AppStorage("sparkamp.dev.columnOrder") private var columnCustomizationData = Data()
    // Delete-from-device confirmation.
    @State private var pendingDeletePaths: [String] = []
    @State private var showDeleteConfirm = false

    private var vars: SkinVars { themeManager.currentVars }
    private var isEjecting: Bool { model.ejectingDevices.contains(device.backendId) }
    private var actionsBusy: Bool { model.deviceBusy || isEjecting }
    private var fsUnsupported: Bool { DeviceService.fsUnsupported(device.fsType) }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            // Copy progress (while copying) or the last-op status line, mirroring
            // the GTK layout where this sits directly under the header band.
            if let cp = model.copyProgress {
                copyProgressBar(cp)
            } else if let s = model.deviceStatus {
                Text(s)
                    .font(.system(size: 11))
                    .foregroundStyle(theme.playlistDurationText)
                    .padding(.horizontal, 16)
                    .padding(.bottom, 8)
            }
            Divider().background(theme.windowBorder)
            if device.fsVisible {
                filesTable
                filesBottomBar
            } else {
                noFilesystemBanner
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(theme.background)
        .onAppear {
            model.loadDeviceTracks(device)
            if !columnCustomizationData.isEmpty,
               let decoded = try? JSONDecoder().decode(
                   TableColumnCustomization<DeviceTrack>.self, from: columnCustomizationData) {
                columnCustomization = decoded
            }
        }
        .onChange(of: device.backendId) { _, _ in
            selection.removeAll()
            model.loadDeviceTracks(device)
        }
        .onChange(of: columnCustomization) { _, v in
            if let d = try? JSONEncoder().encode(v) { columnCustomizationData = d }
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
        .confirmationDialog(
            "Delete \(pendingDeletePaths.count) file\(pendingDeletePaths.count == 1 ? "" : "s") from the device?",
            isPresented: $showDeleteConfirm, titleVisibility: .visible
        ) {
            Button("Delete", role: .destructive) {
                model.deleteFromDevice(device, paths: pendingDeletePaths)
                selection.removeAll()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("The files are permanently deleted from the device and removed from every playlist on it. This can't be undone.")
        }
    }

    private func requestDelete(_ paths: [String]) {
        guard !paths.isEmpty, !device.readOnly else { return }
        pendingDeletePaths = paths
        showDeleteConfirm = true
    }

    private func paths(for ids: Set<String>) -> [String] {
        model.deviceTracks.filter { ids.contains($0.path) }.map { $0.path }
    }

    // MARK: Header

    /// GTK-aligned header band: icon · (name + fs/path + unsupported badge) ·
    /// (capacity bar + capacity text + counts, expanding middle) · read-only
    /// badge · action buttons.
    @ViewBuilder
    private var header: some View {
        HStack(alignment: .center, spacing: 12) {
            Image(systemName: "externaldrive.fill")
                .font(.system(size: 30))
                .foregroundStyle(theme.vars.highlight)

            VStack(alignment: .leading, spacing: 2) {
                Text(device.label.isEmpty ? "Untitled" : device.label)
                    .font(vars.bodyFont.weight(.semibold))
                    .foregroundStyle(theme.playlistText)
                    .lineLimit(1)
                Text("\(device.fsType.isEmpty ? "unknown" : device.fsType) · \(device.mountPath)")
                    .font(.system(size: 11))
                    .foregroundStyle(theme.playlistDurationText)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if fsUnsupported {
                    Text("⚠ Unsupported filesystem")
                        .font(.system(size: 10, weight: .medium))
                        .foregroundStyle(.yellow)
                }
            }
            .frame(minWidth: 140, alignment: .leading)

            if device.fsVisible {
                VStack(alignment: .leading, spacing: 3) {
                    CapacityBar(freeFraction: device.freeFraction,
                                accent: theme.vars.highlight,
                                track: theme.windowBorder.opacity(0.4))
                    Text(deviceCapacityText(device))
                        .font(.system(size: 11))
                        .foregroundStyle(theme.playlistDurationText)
                        .lineLimit(1)
                    Text(countsLine)
                        .font(.system(size: 11))
                        .foregroundStyle(theme.playlistDurationText)
                        .lineLimit(1)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 20)
            } else {
                Spacer()
            }

            if device.readOnly { badge("read-only", color: theme.playlistDurationText) }
            actions
        }
        .padding(16)
    }

    /// "X songs · Y playlists" from the cached counts (or "Counting…").
    private var countsLine: String {
        guard let c = model.deviceCounts[device.id] else { return "Counting…" }
        let songs = c.songs == 1 ? "1 song" : "\(c.songs) songs"
        let pls = c.playlists == 1 ? "1 playlist" : "\(c.playlists) playlists"
        return "\(songs) · \(pls)"
    }

    @ViewBuilder
    private func copyProgressBar(_ cp: CopyProgress) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            ProgressView(value: Double(cp.done), total: Double(max(cp.total, 1)))
            Text("Copying \(min(cp.done + 1, cp.total))/\(cp.total) · \(cp.name)")
                .font(.system(size: 11))
                .foregroundStyle(theme.playlistDurationText)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .padding(.horizontal, 16)
        .padding(.bottom, 8)
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

    /// The device files table. The full ML column set is present; the user
    /// shows/hides/reorders columns via the native header right-click menu
    /// (persisted in `columnCustomization`). Title/Artist/Album/Duration/Synced
    /// from are visible by default; the rest start hidden.
    @ViewBuilder
    private var filesTable: some View {
        Table(sortedTracks, selection: $selection, sortOrder: $sortOrder,
              columnCustomization: $columnCustomization) {
            primaryColumns
            extraColumns
        }
        .contextMenu(forSelectionType: DeviceTrack.ID.self) { ids in
            if ids.count == 1, let p = ids.first {
                Button("Edit / View ID3 Tags") { model.mlOpenTagEditorForPath(p) }
                Button("View Album Art") { model.mlViewArtForPath(p) }
                Divider()
            }
            Button("Delete from Device", role: .destructive) {
                requestDelete(paths(for: ids))
            }
            .disabled(device.readOnly)
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

    // Columns split into two builders so the type-checker stays fast and we
    // clear SwiftUI's 10-column-per-builder limit (two builders here → 2 in the
    // outer Table). Title/Artist/Album/Duration/Synced from default visible.
    @TableColumnBuilder<DeviceTrack, KeyPathComparator<DeviceTrack>>
    private var primaryColumns: some TableColumnContent<DeviceTrack, KeyPathComparator<DeviceTrack>> {
        TableColumn("Title", value: \.title) { t in
            Text(t.title.isEmpty ? URL(fileURLWithPath: t.path).lastPathComponent : t.title)
        }
        .customizationID("col-title")
        TableColumn("Artist", value: \.artist).customizationID("col-artist")
        TableColumn("Album", value: \.album).customizationID("col-album")
        TableColumn("Album Artist", value: \.albumArtist)
            .customizationID("col-albumartist").defaultVisibility(.hidden)
        TableColumn("Genre", value: \.genre)
            .customizationID("col-genre").defaultVisibility(.hidden)
        TableColumn("Composer", value: \.composer)
            .customizationID("col-composer").defaultVisibility(.hidden)
        TableColumn("Year", value: \.year) { t in
            Text(t.year > 0 ? String(t.year) : "")
        }
        .customizationID("col-year").defaultVisibility(.hidden)
        TableColumn("Track #", value: \.trackNum) { t in
            Text(t.trackNum > 0 ? String(t.trackNum) : "")
        }
        .customizationID("col-track").defaultVisibility(.hidden)
    }

    @TableColumnBuilder<DeviceTrack, KeyPathComparator<DeviceTrack>>
    private var extraColumns: some TableColumnContent<DeviceTrack, KeyPathComparator<DeviceTrack>> {
        TableColumn("Disc #", value: \.discNum) { t in
            Text(t.discNum > 0 ? String(t.discNum) : "")
        }
        .customizationID("col-disc").defaultVisibility(.hidden)
        TableColumn("BPM", value: \.bpm)
            .customizationID("col-bpm").defaultVisibility(.hidden)
        TableColumn("Comment", value: \.comment)
            .customizationID("col-comment").defaultVisibility(.hidden)
        TableColumn("Duration", value: \.lengthSecs) { t in
            Text(formatDuration(t.lengthSecs)).foregroundStyle(theme.playlistDurationText)
        }
        .customizationID("col-duration")
        TableColumn("Bitrate", value: \.bitrate) { t in
            Text(t.bitrate > 0 ? "\(t.bitrate / 1000)k" : "")
        }
        .customizationID("col-bitrate").defaultVisibility(.hidden)
        TableColumn("Play Count", value: \.playCount) { t in
            Text(String(t.playCount))
        }
        .customizationID("col-playcount").defaultVisibility(.hidden)
        TableColumn("Last Played", value: \.lastPlayed)
            .customizationID("col-lastplayed").defaultVisibility(.hidden)
        TableColumn("Synced from") { t in
            Text(t.syncedFrom.map { URL(fileURLWithPath: $0).lastPathComponent } ?? "—")
                .foregroundStyle(theme.playlistDurationText)
                .help(t.syncedFrom ?? "Not synced from this computer")
        }
        .customizationID("col-syncedfrom")
    }

    @ViewBuilder
    private var filesBottomBar: some View {
        HStack(spacing: 12) {
            Text("\(model.deviceTracks.count) files")
                .font(.system(size: 11))
                .foregroundStyle(theme.playlistDurationText)
            if !selection.isEmpty {
                Text("\(selection.count) selected")
                    .font(.system(size: 11))
                    .foregroundStyle(theme.playlistDurationText)
            }
            Spacer()
            Button(role: .destructive) {
                requestDelete(paths(for: selection))
            } label: {
                Label("Delete from Device", systemImage: "trash")
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(selection.isEmpty || device.readOnly || actionsBusy)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(theme.background)
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
