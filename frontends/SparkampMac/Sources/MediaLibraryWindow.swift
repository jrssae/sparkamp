import SwiftUI
import AppKit
import UniformTypeIdentifiers

// MARK: - Navigation

enum MLNavigation: Equatable {
    case files
    case albums                // grid of album cover tiles (Phase 11 A4)
    case playlists            // management view: list of saved playlists
    case playlist(id: Int64)  // track editor for a specific playlist
    case devicesOverview      // grid of connected devices
    case device(bsd: String)  // detail for one device (keyed by BSD name)
    case discsOverview        // grid of optical drives
    case discDrive(id: String) // detail for one optical drive (drutil index)
}

// MARK: - Media Library Window

struct MediaLibraryView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    // Navigation
    @State private var nav: MLNavigation = .files

    // Sidebar playlist expansion — persisted across launches
    @AppStorage("sparkamp.ml.playlistsExpanded") private var playlistsExpanded: Bool = true

    // Sidebar width — persisted across launches
    @AppStorage("sparkamp.ml.sidebarWidth") private var sidebarWidth: Double = 160
    @State private var sidebarDragStartWidth: Double? = nil
    /// Saved-playlist id currently under the drag cursor (drop hover).
    /// Drives the sidebar row's highlight outline so users see where the
    /// drop will land.  Nil means no row is targeted.
    @State private var sidebarDropTargetId: Int64? = nil

    // Search (Files tab)
    @State private var searchQuery = ""
    @State private var searchDebounce: DispatchWorkItem? = nil

    // Table sort & selection (Files tab)
    @State private var sortOrder: [KeyPathComparator<MLTrack>] = [KeyPathComparator(\.title)]
    @State private var selection: Set<Int64> = []

    // Rename-playlist sheet (driven from the toolbar when viewing a playlist).
    @State private var showingRenamePlaylist = false
    @State private var renamePlaylistText    = ""
    @State private var renamePlaylistId: Int64 = 0

    // Column visibility bitmask
    // Default visible columns: Title (0), Artist (1), Album (2), Last Played (16).
    @AppStorage("sparkamp.ml.columns") private var columnMask: Int = 0b10000000000000111

    // Column ordering.
    //
    // Key suffix is bumped (`…v2`) deliberately: the original schema persisted
    // a customization that did not include the (then-anonymous) status column.
    // After we gave that column a customizationID, SwiftUI treated it as a
    // brand-new column and tacked it onto the right end of the saved layout.
    // Bumping the key once invalidates that stale data so the natural in-code
    // ordering — status column first — is restored on first launch.
    @AppStorage("sparkamp.ml.columnOrder.v2") private var columnCustomizationData: Data = Data()
    @State private var columnCustomization = TableColumnCustomization<MLTrack>()

    private var theme: SkinTheme { themeManager.currentTheme }

    var body: some View {
        HStack(spacing: 0) {
            // ── Left sidebar ───────────────────────────────────────────────────
            ScrollView(.vertical, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 2) {
                    sidebarRow(label: "Files", icon: "music.note.list", target: .files, onSelect: {
                        // Re-selecting Files directly is the gallery's "back"
                        // affordance — clear the album drill-down filter and
                        // restore the full/searched list (mirrors GTK's
                        // album_filter reset on re-selecting the Files row).
                        let hadFilter = model.mlSelectedAlbum != nil
                        model.mlSelectedAlbum = nil
                        nav = .files
                        if hadFilter { reload() }
                    })
                    sidebarRow(label: "Albums", icon: "square.grid.2x2", target: .albums, onSelect: {
                        // Returning to Albums always lands on the gallery
                        // overview — clear any drill-down filter left set from
                        // a tapped album (mirrors GTK's show_gallery_overview
                        // on the Albums sidebar row / row-activated).
                        model.mlSelectedAlbum = nil
                        nav = .albums
                    })
                    playlistsHeader
                    if playlistsExpanded {
                        ForEach(model.mlSavedPlaylists) { pl in
                            sidebarSubRow(pl: pl)
                        }
                    }
                    devicesSection
                    discsSection
                }
                .padding(.vertical, 10)
            }
            .frame(width: CGFloat(sidebarWidth))
            .background(theme.background)

            // Draggable resize handle
            theme.windowBorder
                .frame(width: 4)
                .contentShape(Rectangle())
                .onHover { inside in
                    if inside { NSCursor.resizeLeftRight.push() } else { NSCursor.pop() }
                }
                .gesture(
                    DragGesture(minimumDistance: 1, coordinateSpace: .global)
                        .onChanged { value in
                            if sidebarDragStartWidth == nil { sidebarDragStartWidth = sidebarWidth }
                            let newWidth = (sidebarDragStartWidth ?? sidebarWidth) + Double(value.translation.width)
                            sidebarWidth = min(max(newWidth, 100), 400)
                        }
                        .onEnded { _ in sidebarDragStartWidth = nil }
                )

            // ── Right content area ─────────────────────────────────────────────
            VStack(spacing: 0) {
                toolbar
                Divider().background(theme.windowBorder)

                if model.mlScanRunning { scanProgress }
                if model.rgRunning { rgProgress }

                switch nav {
                case .files:
                    filesTab
                    Divider().background(theme.windowBorder)
                    filesBottomBar
                case .albums:
                    MLAlbumGallery(nav: $nav, theme: theme)
                case .playlists:
                    MLPlaylistManagement(nav: $nav, theme: theme)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                case .playlist(let id):
                    MLPlaylistEditor(playlistId: id, nav: $nav, theme: theme,
                                     columnMask: columnMask)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                case .devicesOverview:
                    DeviceOverview(
                        devices: model.allDevices,
                        counts: model.deviceCounts,
                        theme: theme,
                        vars: themeManager.currentVars,
                        onSelect: { dev in nav = .device(bsd: dev.backendId) }
                    )
                    .onAppear { model.refreshDeviceCounts() }
                case .discsOverview:
                    DiscOverview(
                        drives: model.discDrives,
                        theme: theme,
                        vars: themeManager.currentVars,
                        onSelect: { drive in nav = .discDrive(id: drive.id) },
                        disconnectNotice: model.discDisconnectNotice,
                        onDismissNotice: { model.discDisconnectNotice = nil }
                    )
                case .discDrive(let id):
                    if let drive = model.discDrives.first(where: { $0.id == id }) {
                        DiscDriveView(drive: drive, theme: theme)
                    } else {
                        // Drive vanished (USB unplugged) — the onChange below
                        // resets nav; render the overview meanwhile.
                        DiscOverview(
                            drives: model.discDrives,
                            theme: theme,
                            vars: themeManager.currentVars,
                            onSelect: { drive in nav = .discDrive(id: drive.id) }
                        )
                    }
                case .device(let bsd):
                    if let dev = model.allDevices.first(where: { $0.backendId == bsd }) {
                        DeviceDetailView(device: dev, theme: theme)
                    } else {
                        // Device unplugged while selected — fall back (the nav
                        // also resets via the onChange(of: model.allDevices) above).
                        DeviceOverview(
                            devices: model.allDevices,
                            counts: model.deviceCounts,
                            theme: theme,
                            vars: themeManager.currentVars,
                            onSelect: { dev in nav = .device(bsd: dev.backendId) }
                        )
                    }
                }
            }
        }
        .background(theme.background)
        .preferredColorScheme(themeManager.preferredColorScheme)
        .onAppear {
            model.openMediaLibrary()
            model.pollDevices()   // populate the Devices group immediately
            model.pollDiscDrives()  // and the Disc Drives group (background)
            model.startUnsupportedWatch()  // begin iOS/PTP recognition
            // F12.1: restore the "files" view's saved query before the
            // initial fetch, if the feature is on.
            if let ctx = model.ctx, sparkamp_get_remember_search(ctx) {
                let p = "files".withCString { sparkamp_get_last_search(ctx, $0) }
                searchQuery = p.map { String(cString: $0) } ?? ""
                sparkamp_free_string(p)
            }
            reload()
            // Honor a pending auto-open request (audio CD inserted while the
            // window was closed): the onChange below can't fire for a value set
            // before this view mounted, so consume it here on first appearance.
            if let id = model.requestedDiscNav {
                nav = .discDrive(id: id)
                model.requestedDiscNav = nil
            }
            if !columnCustomizationData.isEmpty,
               let decoded = try? JSONDecoder().decode(
                   TableColumnCustomization<MLTrack>.self,
                   from: columnCustomizationData) {
                columnCustomization = decoded
            }
        }
        .onChange(of: model.mlScanRunning) { _, running in
            if !running {
                reload()
                // A rescan may discover new playlists or remove vanished
                // ones; refresh the sidebar list so the user sees the
                // current set without needing to reopen the window.
                model.mlRefreshSavedPlaylists()
            }
        }
        // Re-run the current filtered/sorted fetch whenever the model
        // writes back to the DB (e.g. an in-flight track crosses the
        // play-count threshold).  Using a trigger counter rather than
        // calling mlFetchTracks() directly preserves search & sort state.
        .onChange(of: model.mlReloadTrigger) { _, _ in reload() }
        .onChange(of: nav) { _, newNav in
            selection.removeAll()
            // Opening any drive clears a stale disconnect banner.
            if case .discDrive = nav { model.discDisconnectNotice = nil }
            // Gallery tap (MLAlbumGallery.activate) sets mlSelectedAlbum then
            // flips nav to .files in the same action; catch that transition
            // here since filesTab only renders model.mlTracks and never
            // fetches on its own.
            if newNav == .files, model.mlSelectedAlbum != nil { reload() }
        }
        // When the selected device disappears (eject completed, or unplugged
        // while viewing it), return to the overview so nav + sidebar stay
        // consistent rather than pointing at a gone device.
        .onChange(of: model.allDevices) { _, devs in
            if case let .device(bsd) = nav,
               !devs.contains(where: { $0.backendId == bsd }) {
                nav = .devicesOverview
            }
        }
        // Selected optical drive unplugged — invalidate its loaded-disc
        // session and surface a banner rather than silently dropping out.
        .onChange(of: model.discDrives) { _, drives in
            if case let .discDrive(id) = nav,
               !drives.contains(where: { $0.id == id }) {
                // Invalidate the loaded-disc session; any in-flight loader/rip/burn
                // dies with the device and resets its own busy state.
                model.discTracks = []
                model.discDisconnectNotice = "Drive disconnected — reconnect and reload the disc."
                nav = .discsOverview
            }
        }
        // Auto-open request from an inserted audio CD (window already open):
        // navigate to that drive, then clear the request.
        .onChange(of: model.requestedDiscNav) { _, id in
            if let id {
                nav = .discDrive(id: id)
                model.requestedDiscNav = nil
            }
        }
        .onChange(of: columnCustomization) { _, v in
            if let d = try? JSONEncoder().encode(v) { columnCustomizationData = d }
        }
        .onDisappear {
            model.mediaLibraryVisible = false
            model.stopUnsupportedWatch()
        }
        .sheet(isPresented: $showingRenamePlaylist) {
            VStack(spacing: 16) {
                Text("Rename Playlist").font(.headline)
                TextField("Name", text: $renamePlaylistText)
                    .textFieldStyle(.roundedBorder).frame(width: 260)
                HStack {
                    Button("Cancel") { showingRenamePlaylist = false }
                    Spacer()
                    Button("Rename") {
                        showingRenamePlaylist = false
                        model.mlRenamePlaylist(id: renamePlaylistId,
                                               name: renamePlaylistText)
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(renamePlaylistText
                                .trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            .padding(24).frame(width: 320)
        }
        .alert("Eject failed", isPresented: Binding(
            get: { model.ejectError != nil },
            set: { if !$0 { model.ejectError = nil } }
        )) {
            Button("OK", role: .cancel) { model.ejectError = nil }
        } message: {
            Text(model.ejectError ?? "")
        }
        // "Send to ▸ Disc Drive" skipped one or more unreadable files
        // (mirrors GTK's show_unreadable_dialog) — shown regardless of which
        // page is on screen, since the send can be triggered from the Files
        // view, a playlist editor, or a device's file list.
        .alert("Some files could not be read", isPresented: Binding(
            get: { model.burnUnreadableFiles != nil },
            set: { if !$0 { model.burnUnreadableFiles = nil } }
        )) {
            Button("OK", role: .cancel) { model.burnUnreadableFiles = nil }
        } message: {
            Text("These files could not be read and were not added:\n\n"
                 + (model.burnUnreadableFiles ?? []).joined(separator: "\n"))
        }
    }

    // MARK: - Sidebar

    @ViewBuilder
    private var playlistsHeader: some View {
        let isSelected = (nav == .playlists)
        let vars = themeManager.currentVars
        HStack(spacing: 0) {
            Button {
                nav = .playlists
                withAnimation(.easeInOut(duration: 0.15)) { playlistsExpanded = true }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "music.note").font(.system(size: 11))
                    Text("Playlists")
                        .font(vars.bodyFont.weight(isSelected ? .semibold : .regular))
                    Spacer()
                }
                .foregroundStyle(isSelected ? theme.playlistCurrentText : theme.playlistText)
                .padding(.vertical, 5)
                .padding(.leading, 10)
            }
            .buttonStyle(.plain)

            // Expand / collapse toggle — separate tap target from nav
            Button {
                withAnimation(.easeInOut(duration: 0.15)) { playlistsExpanded.toggle() }
            } label: {
                Image(systemName: playlistsExpanded ? "chevron.down" : "chevron.right")
                    .font(.system(size: 9))
                    .foregroundStyle(theme.playlistDurationText)
                    .frame(width: 20, height: 20)
            }
            .buttonStyle(.plain)
            .padding(.trailing, 6)
        }
        .background(
            RoundedRectangle(cornerRadius: 5)
                .fill(isSelected ? theme.playlistCurrentBg : Color.clear)
        )
        .padding(.horizontal, 6)
    }

    @ViewBuilder
    private func sidebarRow(label: String, icon: String, target: MLNavigation,
                            onSelect: (() -> Void)? = nil) -> some View {
        let isSelected = (nav == target)
        let vars = themeManager.currentVars
        Button {
            if let onSelect { onSelect() } else { nav = target }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: icon).font(.system(size: 11))
                Text(label)
                    .font(vars.bodyFont.weight(isSelected ? .semibold : .regular))
                Spacer()
            }
            .foregroundStyle(isSelected ? theme.playlistCurrentText : theme.playlistText)
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(
                RoundedRectangle(cornerRadius: 5)
                    .fill(isSelected ? theme.playlistCurrentBg : Color.clear)
            )
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 6)
    }

    @ViewBuilder
    private func sidebarSubRow(pl: MLPlaylistItem) -> some View {
        let isSelected = (nav == .playlist(id: pl.id))
        let isTargeted = sidebarDropTargetId == pl.id
        let vars = themeManager.currentVars
        Button { nav = .playlist(id: pl.id) } label: {
            HStack(spacing: 4) {
                Spacer().frame(width: 18)
                Image(systemName: "play.rectangle")
                    .font(.system(size: 9))
                    .opacity(0.65)
                Text(pl.name)
                    .font(vars.bodyFont.weight(isSelected ? .semibold : .regular))
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer()
            }
            .foregroundStyle(isSelected ? theme.playlistCurrentText : theme.playlistText)
            .padding(.vertical, 4)
            .padding(.trailing, 8)
            .background(
                RoundedRectangle(cornerRadius: 5)
                    .fill(
                        isTargeted ? theme.playlistSelectedBg
                        : isSelected ? theme.playlistCurrentBg
                        : Color.clear
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 5)
                            .stroke(isTargeted ? theme.vars.highlight : Color.clear,
                                    lineWidth: 1)
                    )
            )
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 6)
        // Drag source: carries the playlist id so it can be dropped onto a
        // device row to send the whole playlist (tracks + .m3u).
        .onDrag {
            NSItemProvider(object: "sparkamp.playlist:\(pl.id)" as NSString)
        }
        // Drop target: file URLs dragged from the active playlist, the ML
        // files table, or another saved-playlist's editor land here and
        // append to this playlist's tracks via the same core path used by
        // the right-click "Add to Playlist" menu.
        .onDrop(of: [.fileURL],
                isTargeted: Binding(
                    get: { sidebarDropTargetId == pl.id },
                    set: { active in
                        sidebarDropTargetId = active ? pl.id : nil
                    }
                )) { providers in
            handleSidebarDrop(providers: providers, playlistId: pl.id)
        }
    }

    /// Receives drag payloads from `.onDrop` providers, prefers Sparkamp
    /// tracklist (multi-row) over plain file URLs, then appends the
    /// resolved paths to `playlistId` on the main actor.
    private func handleSidebarDrop(providers: [NSItemProvider], playlistId: Int64) -> Bool {
        TrackDragPayload.resolvePaths(from: providers) { paths in
            guard !paths.isEmpty else { return }
            model.mlAppendPathsToPlaylist(playlistId: playlistId, paths: paths)
        }
        return true
    }

    // MARK: - Disc Drives sidebar group

    /// One row per physical optical drive (never collapsed to "the drive"),
    /// mirroring how the Devices group lists every volume. Hidden entirely
    /// when no drive is connected.
    @ViewBuilder
    private var discsSection: some View {
        let vars = themeManager.currentVars
        if !model.discDrives.isEmpty {
            let overviewSelected = (nav == .discsOverview)
            Button { nav = .discsOverview } label: {
                HStack(spacing: 6) {
                    Image(systemName: "opticaldiscdrive").font(.system(size: 11))
                    Text("Disc Drives")
                        .font(vars.bodyFont.weight(overviewSelected ? .semibold : .regular))
                    Spacer()
                    Text("\(model.discDrives.count)")
                        .font(.system(size: 10))
                        .foregroundStyle(theme.playlistDurationText)
                }
                .foregroundStyle(overviewSelected ? theme.playlistCurrentText : theme.playlistText)
                .padding(.horizontal, 10)
                .padding(.vertical, 5)
                .background(
                    RoundedRectangle(cornerRadius: 5)
                        .fill(overviewSelected ? theme.playlistCurrentBg : Color.clear)
                )
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 6)

            ForEach(model.discDrives) { drive in
                let selected = (nav == .discDrive(id: drive.id))
                Button { nav = .discDrive(id: drive.id) } label: {
                    HStack(spacing: 4) {
                        Spacer().frame(width: 18)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(drive.label)
                                .font(vars.bodyFont.weight(selected ? .semibold : .regular))
                                .lineLimit(1)
                                .truncationMode(.tail)
                            Text(drive.mediaSummary)
                                .font(.system(size: 10))
                                .foregroundStyle(theme.playlistDurationText)
                                .lineLimit(1)
                        }
                        Spacer()
                    }
                    .foregroundStyle(selected ? theme.playlistCurrentText : theme.playlistText)
                    .padding(.vertical, 4)
                    .padding(.trailing, 8)
                    .background(
                        RoundedRectangle(cornerRadius: 5)
                            .fill(selected ? theme.playlistCurrentBg : Color.clear)
                    )
                }
                .buttonStyle(.plain)
                .padding(.horizontal, 6)
                // Drop files onto a drive row to queue them for burning on
                // THAT drive (per-drive burn queues — mirrors the device
                // row's drop-to-copy just above). Reuses the same
                // probe-on-add path every "Send to ▸ Disc Drive" action
                // goes through (`sendPathsToDrive` → `addToBurnList`), so
                // duplicates/unreadable files are handled identically.
                .onDrop(of: [.fileURL], isTargeted: nil) { providers in
                    TrackDragPayload.resolvePaths(from: providers) { paths in
                        guard !paths.isEmpty else { return }
                        nav = .discDrive(id: drive.id)
                        model.sendPathsToDrive(drive.id, paths: paths)
                    }
                    return true
                }
            }
        }
    }

    // MARK: - Devices sidebar group

    @ViewBuilder
    private var devicesSection: some View {
        let vars = themeManager.currentVars
        let overviewSelected = (nav == .devicesOverview)
        Button { nav = .devicesOverview } label: {
            HStack(spacing: 6) {
                Image(systemName: "externaldrive").font(.system(size: 11))
                Text("Devices")
                    .font(vars.bodyFont.weight(overviewSelected ? .semibold : .regular))
                Spacer()
                if !model.allDevices.isEmpty {
                    Text("\(model.allDevices.count)")
                        .font(.system(size: 10))
                        .foregroundStyle(theme.playlistDurationText)
                }
            }
            .foregroundStyle(overviewSelected ? theme.playlistCurrentText : theme.playlistText)
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(
                RoundedRectangle(cornerRadius: 5)
                    .fill(overviewSelected ? theme.playlistCurrentBg : Color.clear)
            )
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 6)

        ForEach(model.allDevices) { dev in
            let selected = (nav == .device(bsd: dev.backendId))
            Button { nav = .device(bsd: dev.backendId) } label: {
                HStack(spacing: 4) {
                    Spacer().frame(width: 18)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(dev.label.isEmpty ? "Untitled" : dev.label)
                            .font(vars.bodyFont.weight(selected ? .semibold : .regular))
                            .lineLimit(1)
                            .truncationMode(.tail)
                        if dev.backend == .unsupported {
                            // No storage to show a capacity bar for; label the kind.
                            Text(dev.fsType == "ios" ? "iPhone / iPad" : "PTP camera")
                                .font(.system(size: 10))
                                .foregroundStyle(theme.playlistDurationText)
                        } else {
                            CapacityBar(freeFraction: dev.freeFraction,
                                        accent: theme.vars.highlight,
                                        track: theme.windowBorder.opacity(0.4),
                                        height: 3)
                        }
                    }
                    Spacer()
                }
                .foregroundStyle(selected ? theme.playlistCurrentText : theme.playlistText)
                .padding(.vertical, 4)
                .padding(.trailing, 8)
                .background(
                    RoundedRectangle(cornerRadius: 5)
                        .fill(selected ? theme.playlistCurrentBg : Color.clear)
                )
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 6)
            // Drop onto a device row to send music to it, switching to the
            // device's detail first so progress is visible. Two payloads:
            //   • track file URLs (from the Files table / a playlist) → copy.
            //   • a saved-playlist drag ("sparkamp.playlist:<id>" plain text)
            //     → send the whole playlist (tracks + .m3u).
            // File URLs win when present, so a track drag is never misread.
            .onDrop(of: [.fileURL, .plainText], isTargeted: nil) { providers in
                guard dev.fsVisible, !dev.readOnly else { return false }
                let hasFileURL = providers.contains {
                    $0.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier)
                }
                if hasFileURL {
                    nav = .device(bsd: dev.backendId)
                    TrackDragPayload.resolvePaths(from: providers) { paths in
                        guard !paths.isEmpty else { return }
                        model.copyToDevice(dev, paths: paths)
                    }
                    return true
                }
                guard let p = providers.first(where: {
                    $0.hasItemConformingToTypeIdentifier(UTType.plainText.identifier)
                }) else { return false }
                p.loadObject(ofClass: NSString.self) { obj, _ in
                    guard let s = obj as? String,
                          s.hasPrefix("sparkamp.playlist:"),
                          let id = Int64(s.dropFirst("sparkamp.playlist:".count))
                    else { return }
                    DispatchQueue.main.async {
                        nav = .device(bsd: dev.backendId)
                        model.sendPlaylistToDevice(dev, playlistId: id)
                    }
                }
                return true
            }
        }
    }

    // MARK: - Toolbar

    @ViewBuilder
    private var toolbar: some View {
        let vars = themeManager.currentVars
        HStack(spacing: 8) {
            if nav == .files {
                albumFilterChip
                searchField
            }

            if case let .playlist(id) = nav,
               let pl = model.mlSavedPlaylists.first(where: { $0.id == id }) {
                Text(pl.name)
                    .font(vars.bodyFont.weight(.semibold))
                    .foregroundStyle(theme.playlistText)
                    .lineLimit(1)
            }

            Spacer()

            Button { model.mlRescanAll() } label: {
                Label("Rescan", systemImage: "arrow.clockwise").font(vars.bodyFont)
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(model.mlScanRunning)

            // Bulk ReplayGain analysis over the missing-or-stale set, matching
            // the button GTK puts in its files button row. The forced
            // "analyze exactly this selection" variant stays in the row
            // context menu ("Calculate ReplayGain"), same split as GTK.
            if nav == .files {
                Button { model.rgAnalyzeMissing() } label: {
                    Label("Analyze ReplayGain", systemImage: "waveform")
                        .font(vars.bodyFont)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.rgRunning || model.mlScanRunning)
                .help("Compute ReplayGain for tracks that have no value yet")
            }

            if nav == .files {
                Divider().background(theme.windowBorder).frame(height: 16)
                columnPickerMenu
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(theme.background)
    }

    /// Mirrors `scanProgress` for the background ReplayGain job so the Media
    /// Library window shows the same progress + Cancel the Settings pane does
    /// (GTK reports it in the files status line; this is the mac equivalent).
    @ViewBuilder
    private var rgProgress: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                ProgressView(
                    value: model.rgTotal > 0
                        ? Double(model.rgDone) / Double(model.rgTotal)
                        : nil
                )
                .frame(maxWidth: .infinity)

                Text(model.rgTotal > 0
                     ? "Analyzing ReplayGain \(model.rgDone)/\(model.rgTotal)…"
                     : "Analyzing ReplayGain…")
                    .font(themeManager.currentVars.bodyFont)
                    .foregroundStyle(theme.playlistDurationText)

                Button("Cancel") { model.rgCancelAnalyze() }
                    .buttonStyle(.borderless)
                    .font(themeManager.currentVars.bodyFont)
                    .foregroundStyle(.red)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 4)
            .background(theme.background)
            Divider().background(theme.windowBorder)
        }
    }

    @ViewBuilder
    private var scanProgress: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                ProgressView(
                    value: model.mlScanTotal > 0
                        ? Double(model.mlScanDone) / Double(model.mlScanTotal)
                        : nil
                )
                .frame(maxWidth: .infinity)

                Text(model.mlScanTotal > 0
                     ? "Scanning \(model.mlScanDone)/\(model.mlScanTotal)…"
                     : "Scanning…")
                    .font(themeManager.currentVars.bodyFont)
                    .foregroundStyle(theme.playlistDurationText)

                Button("Cancel") { model.mlCancelScan() }
                    .buttonStyle(.borderless)
                    .font(themeManager.currentVars.bodyFont)
                    .foregroundStyle(.red)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 4)
            .background(theme.background)
            Divider().background(theme.windowBorder)
        }
    }

    // MARK: - Files tab

    @ViewBuilder
    private var filesTab: some View {
        MLFilesTable(
            tracks: model.mlTracks,
            selection: $selection,
            sortOrder: $sortOrder,
            columnMask: columnMask,
            columnCustomization: $columnCustomization,
            theme: theme,
            themeManager: themeManager,
            model: model,
            onEvent: { event in
                switch event {
                case .sortChanged(let key, let ascending):
                    // Sort is driven by the NSTableView header click;
                    // re-fetch with the new SQL sort key/direction applied
                    // immediately (bypasses the sortOrder binding which
                    // may not have flushed yet).
                    model.mlFetchTracks(query: searchQuery,
                                        sortCol: key, sortDesc: !ascending)
                case .addToPlaylist(let ids):   model.mlAddToPlaylist(ids: ids)
                case .replacePlaylist(let ids): model.mlReplacePlaylistWith(ids: ids)
                case .editTags(let id):
                    if let t = model.mlTracks.first(where: { $0.id == id }) {
                        model.mlOpenTagEditorForPath(t.path)
                    }
                case .removeTracks(let ids): model.mlRemoveTracks(ids: ids)
                case .doubleClick(let ids):  model.mlDoubleClickTracks(ids: ids)
                case .viewArt(let id):
                    if let t = model.mlTracks.first(where: { $0.id == id }) {
                        model.mlViewArtForPath(t.path)
                    }
                }
            },
            onDropPaths: { paths in
                // Scenarios 5 + 8: drag tracks from active/specific playlist
                // onto Files view → upsert into library DB.  Paths outside
                // every watched folder are silently skipped (per user spec:
                // "add to library DB only, no new watch folders").
                let n = model.mlAddFilesToLibrary(paths: paths)
                if n > 0 { reload() }
            }
        )
    }

    /// "N tracks · MM:SS total · K selected · MM:SS" — mirrors the active
    /// playlist's status line (`playlistStatusLine` / src/playlist_status.rs).
    private var filesStatusLine: String {
        let total = model.mlTracks.reduce(0) { $0 + max(Int($1.lengthSecs), 0) }
        // Guard on the DISPLAYED selected rows, not the raw selection set —
        // a search/playlist-chip filter can hide every currently-selected
        // row while `selection` still holds their ids, which must omit the
        // "selected" clause rather than show "· 0:00 selected".
        let selRows = model.mlTracks.filter { selection.contains($0.id) }
        let sel: (count: Int, secs: Int)? = selRows.isEmpty ? nil :
            (selRows.count, selRows.reduce(0) { $0 + max(Int($1.lengthSecs), 0) })
        return playlistStatusLine(count: model.mlTracks.count, totalSecs: total, selected: sel)
    }

    @ViewBuilder
    private var filesBottomBar: some View {
        HStack {
            Text(filesStatusLine)
                .font(themeManager.currentVars.bodyFont)
                .foregroundStyle(theme.playlistDurationText)
            Spacer()
            if !selection.isEmpty {
                // GTK's "Send to ▾" MenuButton equivalent — same spec as the
                // right-click "Send to" submenu, just as a top-level button
                // (so it isn't nested under another "Send to" label).
                Menu("Send to ▾") {
                    SendToMenu(paths: model.mlTracks
                        .filter { selection.contains($0.id) }.map(\.path))
                }
                .controlSize(.small)
                .fixedSize()
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(theme.background)
    }

    // MARK: - Toolbar subviews

    /// "Back to albums" affordance for the album drill-down — shown at the
    /// left of the Files toolbar (before the search field) only while
    /// `mlSelectedAlbum` is set. Tapping it clears the filter and returns to
    /// the gallery overview (mirrors GTK's back button / `show_gallery_overview`
    /// — NOT just the unfiltered Files list; the "Files" sidebar row remains
    /// the way back to the full library).
    @ViewBuilder
    private var albumFilterChip: some View {
        if model.mlSelectedAlbum != nil {
            Button {
                model.mlSelectedAlbum = nil
                nav = .albums
            } label: {
                HStack(spacing: 4) {
                    Image(systemName: "chevron.left").font(.system(size: 10))
                    Text("Albums")
                        .font(themeManager.currentVars.bodyFont)
                        .lineLimit(1)
                }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .help("Back to albums")
        }
    }

    @ViewBuilder
    private var searchField: some View {
        HStack(spacing: 4) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(theme.playlistDurationText)
                .font(.system(size: 11))
            TextField("Search…", text: $searchQuery)
                .textFieldStyle(.plain)
                .font(themeManager.currentVars.bodyFont)
                .foregroundStyle(theme.playlistText)
                .frame(width: 180)
                .onChange(of: searchQuery) { _, _ in
                    // Typing a search query escapes an active album filter
                    // (mirrors GTK's album_filter reset on search input).
                    model.mlSelectedAlbum = nil
                    debounceSearch()
                }
            if !searchQuery.isEmpty {
                Button { searchQuery = ""; persistSearch(""); model.mlSelectedAlbum = nil; reload() } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(theme.playlistDurationText)
                        .font(.system(size: 11))
                }
                .buttonStyle(.plain)
            }
        }
        .padding(4)
        .background(theme.lcdBackground.opacity(0.8))
        .cornerRadius(6)
        .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.windowBorder, lineWidth: 1))
    }

    @ViewBuilder
    private var columnPickerMenu: some View {
        Menu {
            columnToggle("Title",        bit: 0)
            columnToggle("Artist",       bit: 1)
            columnToggle("Album",        bit: 2)
            columnToggle("Album Artist", bit: 3)
            columnToggle("Genre",        bit: 4)
            columnToggle("Composer",     bit: 5)
            columnToggle("Year",         bit: 6)
            columnToggle("Track #",      bit: 7)
            columnToggle("Disc #",       bit: 8)
            columnToggle("BPM",          bit: 9)
            columnToggle("Comment",      bit: 10)
            Divider()
            columnToggle("Duration",     bit: 11)
            columnToggle("Bitrate",      bit: 12)
            columnToggle("Filename",     bit: 13)
            columnToggle("Play Count",   bit: 14)
            columnToggle("Album Art",    bit: 15)
            columnToggle("Last Played",  bit: 16)
            Divider()
            columnToggle("Sample Rate",  bit: 17)
            columnToggle("Size",         bit: 18)
            columnToggle("Date Added",   bit: 19)
            columnToggle("File Modified", bit: 20)
            columnToggle("Mode",         bit: 21)
            columnToggle("ReplayGain",   bit: 22)
        } label: {
            Image(systemName: "tablecells")
                .font(.system(size: 11))
                .foregroundStyle(theme.modeBtnText)
        }
        .menuStyle(.borderlessButton)
    }

    // MARK: - Helpers

    @ViewBuilder
    private func columnToggle(_ label: String, bit: Int) -> some View {
        Toggle(label, isOn: Binding(
            get: { (columnMask >> bit) & 1 == 1 },
            set: { on in
                if on { columnMask |=  (1 << bit) }
                else  { columnMask &= ~(1 << bit) }
            }
        ))
    }

    private func debounceSearch() {
        searchDebounce?.cancel()
        let task = DispatchWorkItem { [q = searchQuery] in
            reload(query: q)
            persistSearch(q)
        }
        searchDebounce = task
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3, execute: task)
    }

    /// F12.1: remember the "files" view's query for next open (only when the
    /// feature is on; the value is unused otherwise).
    private func persistSearch(_ q: String) {
        guard let ctx = model.ctx, sparkamp_get_remember_search(ctx) else { return }
        "files".withCString { vid in
            q.withCString { qv in sparkamp_set_last_search(ctx, vid, qv) }
        }
    }

    private func reload(query: String? = nil) {
        // Phase 11 A4: an active album drill-down filter takes over the
        // Files fetch entirely (mirrors GTK's rebuild_files honoring
        // album_filter) — search/sort don't apply until the filter clears.
        if let filter = model.mlSelectedAlbum {
            model.mlTracks = model.albumTracks(album: filter.album, albumArtist: filter.albumArtist)
            return
        }
        let q = query ?? searchQuery
        let colName: String? = sortOrder.first.flatMap { kp in
            switch kp.keyPath {
            case \MLTrack.title:       return "title"
            case \MLTrack.artist:      return "artist"
            case \MLTrack.album:       return "album"
            case \MLTrack.albumArtist: return "album_artist"
            case \MLTrack.genre:       return "genre"
            case \MLTrack.composer:    return "composer"
            case \MLTrack.year:        return "year"
            case \MLTrack.trackNum:    return "num"
            case \MLTrack.discNum:     return "disc_num"
            case \MLTrack.bpm:         return "bpm"
            case \MLTrack.lengthSecs:  return "duration"
            case \MLTrack.bitrate:     return "bitrate"
            case \MLTrack.playCount:   return "play_count"
            case \MLTrack.lastPlayed:  return "last_played"
            case \MLTrack.sampleRate:  return "sample_rate"
            case \MLTrack.fileSize:    return "file_size"
            case \MLTrack.addedAt:     return "added_at"
            case \MLTrack.fileMtime:   return "file_mtime"
            case \MLTrack.bitrateMode: return "bitrate_mode"
            case \MLTrack.rgTrackGain: return "rg_gain"
            default:                   return nil
            }
        }
        let desc = sortOrder.first.map { $0.order == .reverse } ?? false
        model.mlFetchTracks(query: q, sortCol: colName, sortDesc: desc)
    }
}

