import SwiftUI
import AppKit
import UniformTypeIdentifiers

// MARK: - Navigation

enum MLNavigation: Equatable {
    case files
    case playlists            // management view: list of saved playlists
    case playlist(id: Int64)  // track editor for a specific playlist
    case devicesOverview      // grid of connected devices
    case device(bsd: String)  // detail for one device (keyed by BSD name)
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
                    sidebarRow(label: "Files", icon: "music.note.list", target: .files)
                    playlistsHeader
                    if playlistsExpanded {
                        ForEach(model.mlSavedPlaylists) { pl in
                            sidebarSubRow(pl: pl)
                        }
                    }
                    devicesSection
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

                switch nav {
                case .files:
                    filesTab
                    Divider().background(theme.windowBorder)
                    filesBottomBar
                case .playlists:
                    MLPlaylistManagement(nav: $nav, theme: theme)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                case .playlist(let id):
                    MLPlaylistEditor(playlistId: id, nav: $nav, theme: theme,
                                     columnMask: columnMask)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                case .devicesOverview:
                    DeviceOverview(
                        devices: model.devices,
                        counts: model.deviceCounts,
                        theme: theme,
                        vars: themeManager.currentVars,
                        onSelect: { dev in nav = .device(bsd: dev.backendId) }
                    )
                    .onAppear { model.refreshDeviceCounts() }
                case .device(let bsd):
                    if let dev = model.devices.first(where: { $0.backendId == bsd }) {
                        DeviceDetailView(device: dev, theme: theme)
                    } else {
                        // Device unplugged while selected — fall back (the nav
                        // also resets via the onChange(of: model.devices) above).
                        DeviceOverview(
                            devices: model.devices,
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
            reload()
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
        .onChange(of: nav) { _, _ in selection.removeAll() }
        // When the selected device disappears (eject completed, or unplugged
        // while viewing it), return to the overview so nav + sidebar stay
        // consistent rather than pointing at a gone device.
        .onChange(of: model.devices) { _, devs in
            if case let .device(bsd) = nav,
               !devs.contains(where: { $0.backendId == bsd }) {
                nav = .devicesOverview
            }
        }
        .onChange(of: columnCustomization) { _, v in
            if let d = try? JSONEncoder().encode(v) { columnCustomizationData = d }
        }
        .onDisappear { model.mediaLibraryVisible = false }
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
    private func sidebarRow(label: String, icon: String, target: MLNavigation) -> some View {
        let isSelected = (nav == target)
        let vars = themeManager.currentVars
        Button { nav = target } label: {
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
                if !model.devices.isEmpty {
                    Text("\(model.devices.count)")
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

        ForEach(model.devices) { dev in
            let selected = (nav == .device(bsd: dev.backendId))
            Button { nav = .device(bsd: dev.backendId) } label: {
                HStack(spacing: 4) {
                    Spacer().frame(width: 18)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(dev.label.isEmpty ? "Untitled" : dev.label)
                            .font(vars.bodyFont.weight(selected ? .semibold : .regular))
                            .lineLimit(1)
                            .truncationMode(.tail)
                        CapacityBar(freeFraction: dev.freeFraction,
                                    accent: theme.vars.highlight,
                                    track: theme.windowBorder.opacity(0.4),
                                    height: 3)
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
            if nav == .files { searchField }

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

            if nav == .files {
                Divider().background(theme.windowBorder).frame(height: 16)
                columnPickerMenu
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(theme.background)
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

    @ViewBuilder
    private var filesBottomBar: some View {
        HStack {
            Text("\(model.mlTracks.count) tracks")
                .font(themeManager.currentVars.bodyFont)
                .foregroundStyle(theme.playlistDurationText)
            Spacer()
            if !selection.isEmpty {
                Text("\(selection.count) selected")
                    .font(themeManager.currentVars.bodyFont)
                    .foregroundStyle(theme.playlistDurationText)
                Button("Add to Playlist") { model.mlAddToPlaylist(ids: Array(selection)) }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(theme.background)
    }

    // MARK: - Toolbar subviews

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
                .onChange(of: searchQuery) { _, _ in debounceSearch() }
            if !searchQuery.isEmpty {
                Button { searchQuery = ""; reload() } label: {
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
        let task = DispatchWorkItem { [q = searchQuery] in reload(query: q) }
        searchDebounce = task
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3, execute: task)
    }

    private func reload(query: String? = nil) {
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
            default:                   return nil
            }
        }
        let desc = sortOrder.first.map { $0.order == .reverse } ?? false
        model.mlFetchTracks(query: q, sortCol: colName, sortDesc: desc)
    }
}

