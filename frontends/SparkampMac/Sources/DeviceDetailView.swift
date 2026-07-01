import SwiftUI
import UniformTypeIdentifiers

/// Detail page for one connected device: header with badges + capacity, the
/// device's audio files (with a "Synced from" column), and the Add / Sync /
/// Scan / Eject actions. Copy-to-device also accepts files dropped from the
/// Media Library Files table onto this view or the device's sidebar row.
///
/// Sync applies single-side changes automatically; both-changed songs raise the
/// `DeviceConflictSheet` for per-song resolution.
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
    // Delete-from-device / remove-from-playlist confirmation. When a playlist
    // chip is active the action is "Remove" (drops from that .m3u, files stay);
    // on "All files" it's "Delete" (permanent). pendingRemoveRelpath non-nil
    // selects Remove mode.
    @State private var pendingDeletePaths: [String] = []
    @State private var pendingRemoveRelpath: String? = nil
    @State private var showDeleteConfirm = false
    // Device-playlist chips: nil = "All files"; else the selected playlist relpath.
    @State private var selectedPlaylistRelpath: String? = nil
    @State private var showNewPlaylist = false
    @State private var newPlaylistName = ""
    @State private var showRenamePlaylist = false
    @State private var renamePlaylistText = ""
    @State private var renamePlaylistRelpath = ""
    @State private var showDeletePlaylistConfirm = false

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
            if device.fsVisible {
                playlistChips
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
            model.loadDevicePlaylists(device)
            if !columnCustomizationData.isEmpty,
               let decoded = try? JSONDecoder().decode(
                   TableColumnCustomization<DeviceTrack>.self, from: columnCustomizationData) {
                columnCustomization = decoded
            }
        }
        .onChange(of: device.backendId) { _, _ in
            selection.removeAll()
            selectedPlaylistRelpath = nil
            model.loadDeviceTracks(device)
            model.loadDevicePlaylists(device)
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
            {
                let n = pendingDeletePaths.count
                let s = n == 1 ? "" : "s"
                return pendingRemoveRelpath != nil
                    ? "Remove \(n) file\(s) from this playlist?"
                    : "Delete \(n) file\(s) from the device?"
            }(),
            isPresented: $showDeleteConfirm, titleVisibility: .visible
        ) {
            if let rel = pendingRemoveRelpath {
                Button("Remove", role: .destructive) {
                    model.removeFromDevicePlaylist(device, relpath: rel, paths: pendingDeletePaths)
                    selection.removeAll()
                }
            } else {
                Button("Delete", role: .destructive) {
                    model.deleteFromDevice(device, paths: pendingDeletePaths)
                    selection.removeAll()
                }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(pendingRemoveRelpath != nil
                ? "The files stay on the device and in any other playlist."
                : "The files are permanently deleted from the device and removed from every playlist on it. This can't be undone.")
        }
        .confirmationDialog(
            "Delete this playlist from the device?",
            isPresented: $showDeletePlaylistConfirm, titleVisibility: .visible
        ) {
            Button("Delete", role: .destructive) {
                if let rel = selectedPlaylistRelpath {
                    model.deleteDevicePlaylist(device, relpath: rel)
                    selectedPlaylistRelpath = nil
                }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Only the playlist is removed; the audio files stay on the device.")
        }
        .sheet(isPresented: $showNewPlaylist) {
            playlistNameSheet(title: "New Playlist", text: $newPlaylistName, confirm: "Create") {
                model.newDevicePlaylist(device, name: newPlaylistName)
            }
        }
        .sheet(isPresented: $showRenamePlaylist) {
            playlistNameSheet(title: "Rename Playlist", text: $renamePlaylistText, confirm: "Rename") {
                model.renameDevicePlaylist(
                    device, relpath: renamePlaylistRelpath, newName: renamePlaylistText)
            }
        }
        // Two-way sync conflict resolution. Presented when a sync plan returns
        // both-changed songs; dismissing without a button choice is treated as
        // Cancel (auto pairs still apply, conflicts skipped).
        .sheet(isPresented: Binding(
            get: { model.pendingSyncPlan != nil },
            set: { presented in
                if !presented, model.pendingSyncPlan != nil {
                    model.resolveSyncConflicts(choices: [])
                }
            }
        )) {
            if let plan = model.pendingSyncPlan, let dev = model.pendingSyncDevice {
                DeviceConflictSheet(device: dev, plan: plan)
                    .environmentObject(model)
                    .environmentObject(themeManager)
            }
        }
    }

    @ViewBuilder
    private func playlistNameSheet(
        title: String, text: Binding<String>, confirm: String, action: @escaping () -> Void
    ) -> some View {
        VStack(spacing: 16) {
            Text(title).font(.headline)
            TextField("Name", text: text)
                .textFieldStyle(.roundedBorder).frame(width: 260)
            HStack {
                Button("Cancel") {
                    showNewPlaylist = false; showRenamePlaylist = false
                }
                Spacer()
                Button(confirm) {
                    showNewPlaylist = false; showRenamePlaylist = false
                    action()
                }
                .buttonStyle(.borderedProminent)
                .disabled(text.wrappedValue.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(24).frame(width: 320)
    }

    private func requestDelete(_ paths: [String]) {
        guard !paths.isEmpty, !device.readOnly else { return }
        pendingDeletePaths = paths
        pendingRemoveRelpath = selectedPlaylistRelpath  // nil ("All files") = delete
        showDeleteConfirm = true
    }

    /// Label for the file-view destructive action, which differs by mode.
    private var deleteActionLabel: String {
        selectedPlaylistRelpath == nil ? "Delete from Device" : "Remove from Playlist"
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
                        .help("\(device.fsType) can't be written reliably from macOS, so copying and sync are disabled. Reformat the device as FAT32 (MS-DOS) or use a different drive.")
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

    // MARK: Device playlists (chips + filter)

    @ViewBuilder
    private var playlistChips: some View {
        HStack(spacing: 8) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    chip(label: "All files", selected: selectedPlaylistRelpath == nil) {
                        selectedPlaylistRelpath = nil
                    }
                    ForEach(model.devicePlaylists) { pl in
                        chip(label: pl.displayName,
                             selected: selectedPlaylistRelpath == pl.relpath) {
                            selectedPlaylistRelpath = pl.relpath
                        }
                    }
                }
            }
            Spacer(minLength: 6)
            // Playlist actions: + New always; Rename/Duplicate/Delete when a
            // device playlist is selected.
            Button { newPlaylistName = ""; showNewPlaylist = true } label: {
                Label("New", systemImage: "plus")
            }
            .disabled(device.readOnly || actionsBusy)
            if let rel = selectedPlaylistRelpath,
               let pl = model.devicePlaylists.first(where: { $0.relpath == rel }) {
                Button {
                    renamePlaylistRelpath = rel
                    renamePlaylistText = pl.displayName
                    showRenamePlaylist = true
                } label: { Image(systemName: "pencil") }
                .help("Rename playlist")
                .disabled(device.readOnly || actionsBusy)
                Button { model.duplicateDevicePlaylist(device, relpath: rel) } label: {
                    Image(systemName: "plus.square.on.square")
                }
                .help("Duplicate playlist")
                .disabled(device.readOnly || actionsBusy)
                Button(role: .destructive) { showDeletePlaylistConfirm = true } label: {
                    Image(systemName: "trash")
                }
                .help("Delete playlist")
                .disabled(device.readOnly || actionsBusy)
            }
        }
        .buttonStyle(.bordered)
        .controlSize(.small)
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
    }

    @ViewBuilder
    private func chip(label: String, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(label)
                .font(.system(size: 11, weight: selected ? .semibold : .regular))
                .lineLimit(1)
                .padding(.horizontal, 10)
                .padding(.vertical, 4)
                .background(
                    Capsule().fill(selected ? theme.playlistCurrentBg : theme.windowBorder.opacity(0.25))
                )
                .foregroundStyle(selected ? theme.playlistCurrentText : theme.playlistText)
        }
        .buttonStyle(.plain)
    }

    // MARK: Files

    /// Tracks shown in the table: all of them, or just the selected device
    /// playlist's entries (matched by filename), then sorted.
    private var sortedTracks: [DeviceTrack] {
        let base: [DeviceTrack]
        if let rel = selectedPlaylistRelpath,
           let pl = model.devicePlaylists.first(where: { $0.relpath == rel }) {
            let names = Set(pl.entries)
            base = model.deviceTracks.filter {
                names.contains(URL(fileURLWithPath: $0.path).lastPathComponent)
            }
        } else {
            base = model.deviceTracks
        }
        return base.sorted(using: sortOrder)
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
            Button(deleteActionLabel, role: .destructive) {
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
                Label(deleteActionLabel,
                      systemImage: selectedPlaylistRelpath == nil ? "trash" : "minus.circle")
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
