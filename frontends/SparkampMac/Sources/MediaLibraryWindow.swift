import SwiftUI
import AppKit

// MARK: - Navigation

enum MLNavigation: Equatable {
    case files
    case playlists            // management view: list of saved playlists
    case playlist(id: Int64)  // track editor for a specific playlist
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

    // Search (Files tab)
    @State private var searchQuery = ""
    @State private var searchDebounce: DispatchWorkItem? = nil

    // Table sort & selection (Files tab)
    @State private var sortOrder: [KeyPathComparator<MLTrack>] = [KeyPathComparator(\.title)]
    @State private var selection: Set<Int64> = []

    // Column visibility bitmask
    @AppStorage("sparkamp.ml.columns") private var columnMask: Int = 0b0000000000111

    // Column ordering
    @AppStorage("sparkamp.ml.columnOrder") private var columnCustomizationData: Data = Data()
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
                    MLPlaylistEditor(playlistId: id, nav: $nav, theme: theme)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
        }
        .background(theme.background)
        .preferredColorScheme(themeManager.preferredColorScheme)
        .onAppear {
            model.openMediaLibrary()
            reload()
            if !columnCustomizationData.isEmpty,
               let decoded = try? JSONDecoder().decode(
                   TableColumnCustomization<MLTrack>.self,
                   from: columnCustomizationData) {
                columnCustomization = decoded
            }
        }
        .onChange(of: model.mlScanRunning) { _, running in if !running { reload() } }
        .onChange(of: nav) { _, _ in selection.removeAll() }
        .onChange(of: columnCustomization) { _, v in
            if let d = try? JSONEncoder().encode(v) { columnCustomizationData = d }
        }
        .onDisappear { model.mediaLibraryVisible = false }
    }

    // MARK: - Sidebar

    @ViewBuilder
    private var playlistsHeader: some View {
        let isSelected = (nav == .playlists)
        HStack(spacing: 0) {
            Button {
                nav = .playlists
                withAnimation(.easeInOut(duration: 0.15)) { playlistsExpanded = true }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "music.note").font(.system(size: 11))
                    Text("Playlists")
                        .font(.system(size: 12, weight: isSelected ? .semibold : .regular))
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
        Button { nav = target } label: {
            HStack(spacing: 6) {
                Image(systemName: icon).font(.system(size: 11))
                Text(label)
                    .font(.system(size: 12, weight: isSelected ? .semibold : .regular))
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
        Button { nav = .playlist(id: pl.id) } label: {
            HStack(spacing: 4) {
                Spacer().frame(width: 18)
                Image(systemName: "play.rectangle")
                    .font(.system(size: 9))
                    .opacity(0.65)
                Text(pl.name)
                    .font(.system(size: 11, weight: isSelected ? .semibold : .regular))
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer()
            }
            .foregroundStyle(isSelected ? theme.playlistCurrentText : theme.playlistText)
            .padding(.vertical, 4)
            .padding(.trailing, 8)
            .background(
                RoundedRectangle(cornerRadius: 5)
                    .fill(isSelected ? theme.playlistCurrentBg : Color.clear)
            )
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 6)
    }

    // MARK: - Toolbar

    @ViewBuilder
    private var toolbar: some View {
        HStack(spacing: 8) {
            if nav == .files { searchField }
            Spacer()

            Button { model.mlRescanAll() } label: {
                Label("Rescan", systemImage: "arrow.clockwise").font(.system(size: 11))
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
                    .font(.system(size: 10))
                    .foregroundStyle(theme.playlistDurationText)

                Button("Cancel") { model.mlCancelScan() }
                    .buttonStyle(.borderless)
                    .font(.system(size: 10))
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
            theme: theme
        ) { event in
            switch event {
            case .sortChanged:         model.mlTracks.sort(using: sortOrder)
            case .addToPlaylist(let ids):  model.mlAddToPlaylist(ids: ids)
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
        }
    }

    @ViewBuilder
    private var filesBottomBar: some View {
        HStack {
            Text("\(model.mlTracks.count) tracks")
                .font(.system(size: 10))
                .foregroundStyle(theme.playlistDurationText)
            Spacer()
            if !selection.isEmpty {
                Text("\(selection.count) selected")
                    .font(.system(size: 10))
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
                .font(.system(size: 12))
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
            default:                   return nil
            }
        }
        let desc = sortOrder.first.map { $0.order == .reverse } ?? false
        model.mlFetchTracks(query: q, sortCol: colName, sortDesc: desc)
    }
}

// MARK: - Playlist management (nav = .playlists)

private struct MLPlaylistManagement: View {
    @Binding var nav: MLNavigation
    let theme: SkinTheme

    @EnvironmentObject var model: SparkampModel

    @State private var showingNew    = false
    @State private var newName       = ""
    @State private var showingRename = false
    @State private var renameText    = ""
    @State private var renameTarget: Int64? = nil

    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Saved Playlists")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(theme.playlistDurationText)
                Spacer()
                Button { newName = "New Playlist"; showingNew = true } label: {
                    Image(systemName: "plus").font(.system(size: 11))
                }
                .buttonStyle(.borderless)
                .foregroundStyle(theme.playlistText)
                .help("New Playlist")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(theme.background)

            Divider().background(theme.windowBorder)

            if model.mlSavedPlaylists.isEmpty {
                Spacer()
                Text("No saved playlists yet.\nClick + to create one.")
                    .multilineTextAlignment(.center)
                    .font(.system(size: 12))
                    .foregroundStyle(theme.playlistDurationText)
                Spacer()
            } else {
                List(model.mlSavedPlaylists) { pl in
                    HStack(spacing: 8) {
                        Image(systemName: "play.rectangle")
                            .font(.system(size: 10))
                            .foregroundStyle(theme.playlistDurationText)
                        Text(pl.name)
                            .font(.system(size: 12))
                            .foregroundStyle(theme.playlistText)
                        Spacer()
                        Button {
                            renameTarget = pl.id
                            renameText   = pl.name
                            showingRename = true
                        } label: {
                            Image(systemName: "pencil").font(.system(size: 10))
                        }
                        .buttonStyle(.borderless)
                        .foregroundStyle(theme.playlistDurationText)
                        .help("Rename")

                        Button {
                            if nav == .playlist(id: pl.id) { nav = .playlists }
                            model.mlDeletePlaylist(id: pl.id)
                        } label: {
                            Image(systemName: "trash").font(.system(size: 10))
                        }
                        .buttonStyle(.borderless)
                        .foregroundStyle(.red)
                        .help("Delete")
                    }
                    .contentShape(Rectangle())
                    .listRowBackground(theme.playlistBg)
                    .onTapGesture { nav = .playlist(id: pl.id) }
                }
                .listStyle(.plain)
                .background(theme.playlistBg)
                .scrollContentBackground(.hidden)
            }
        }
        .background(theme.playlistBg)
        .sheet(isPresented: $showingNew) {
            VStack(spacing: 16) {
                Text("New Playlist").font(.headline)
                TextField("Name", text: $newName)
                    .textFieldStyle(.roundedBorder).frame(width: 260)
                HStack {
                    Button("Cancel") { showingNew = false }
                    Spacer()
                    Button("Create") {
                        showingNew = false
                        let id = model.mlCreatePlaylist(name: newName)
                        if id >= 0 { nav = .playlist(id: id) }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(newName.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            .padding(24).frame(width: 320)
        }
        .sheet(isPresented: $showingRename) {
            VStack(spacing: 16) {
                Text("Rename Playlist").font(.headline)
                TextField("Name", text: $renameText)
                    .textFieldStyle(.roundedBorder).frame(width: 260)
                HStack {
                    Button("Cancel") { showingRename = false }
                    Spacer()
                    Button("Rename") {
                        showingRename = false
                        if let id = renameTarget { model.mlRenamePlaylist(id: id, name: renameText) }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(renameText.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            .padding(24).frame(width: 320)
        }
    }
}

// MARK: - Playlist track editor (nav = .playlist(id:))

private struct MLPlaylistEditor: View {
    let playlistId: Int64
    @Binding var nav: MLNavigation
    let theme: SkinTheme

    @EnvironmentObject var model: SparkampModel

    @State private var editingTracks: [MLTrack] = []
    @State private var savedTrackIds: [Int64]   = []
    @State private var trackSelection: Set<Int64> = []
    @State private var showingRename  = false
    @State private var showingSaveAs  = false
    @State private var renameText     = ""
    @State private var saveAsText     = ""

    private var hasChanges: Bool { editingTracks.map(\.id) != savedTrackIds }

    private var playlistInfo: MLPlaylistItem? {
        model.mlSavedPlaylists.first(where: { $0.id == playlistId })
    }
    private var playlistName: String { playlistInfo?.name ?? "Playlist" }
    private var playlistPath: String { playlistInfo?.path ?? "" }
    /// True if the playlist lives in Sparkamp's managed playlists dir; external
    /// playlists (e.g. from ~/Music) should not be overwritten — use Save As.
    private var isManaged: Bool { model.mlPlaylistIsManaged(id: playlistId) }

    var body: some View {
        VStack(spacing: 0) {
            // ── Header ─────────────────────────────────────────────────────────
            VStack(spacing: 0) {
                HStack(spacing: 8) {
                    Button { nav = .playlists } label: {
                        HStack(spacing: 4) {
                            Image(systemName: "chevron.left").font(.system(size: 10))
                            Text("Playlists").font(.system(size: 11))
                        }
                    }
                    .buttonStyle(.borderless)
                    .foregroundStyle(theme.playlistDurationText)

                    Divider().frame(height: 14).background(theme.windowBorder)

                    Text(playlistName)
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(theme.playlistText)
                        .lineLimit(1)

                    Spacer()

                    Button { renameText = playlistName; showingRename = true } label: {
                        Label("Rename", systemImage: "pencil").font(.system(size: 11))
                    }
                    .buttonStyle(.borderless)
                    .foregroundStyle(theme.playlistText)
                    .help("Rename Playlist")

                    Button { model.mlDeletePlaylist(id: playlistId); nav = .playlists } label: {
                        Image(systemName: "trash").font(.system(size: 11))
                    }
                    .buttonStyle(.borderless)
                    .foregroundStyle(.red)
                    .help("Delete Playlist")
                }
                .padding(.horizontal, 12)
                .padding(.top, 7)
                .padding(.bottom, 2)

                // File path bar — helps identify external vs managed playlists.
                if !playlistPath.isEmpty {
                    HStack {
                        Text(playlistPath)
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(isManaged
                                ? theme.playlistDurationText
                                : Color.orange.opacity(0.8))
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Spacer()
                        if !isManaged {
                            Text("external")
                                .font(.system(size: 9))
                                .foregroundStyle(Color.orange.opacity(0.8))
                        }
                    }
                    .padding(.horizontal, 12)
                    .padding(.bottom, 5)
                }
            }
            .background(theme.background)

            Divider().background(theme.windowBorder)

            // ── Track list ─────────────────────────────────────────────────────
            List(editingTracks, id: \.id, selection: $trackSelection) { track in
                HStack(spacing: 6) {
                    Group {
                        if track.fileMissing {
                            Image(systemName: "xmark.circle.fill")
                                .font(.system(size: 9)).foregroundStyle(.red)
                        } else if track.readOnly {
                            Image(systemName: "lock.fill")
                                .font(.system(size: 9)).foregroundStyle(theme.playlistDurationText)
                        } else {
                            Color.clear
                        }
                    }
                    .frame(width: 12)

                    Text(track.title.isEmpty ? track.filename : track.title)
                        .font(.system(size: 12))
                        .foregroundStyle(track.fileMissing ? Color.red : theme.playlistText)
                        .lineLimit(1)

                    if !track.artist.isEmpty {
                        Text("— \(track.artist)")
                            .font(.system(size: 12))
                            .foregroundStyle(theme.playlistDurationText)
                            .lineLimit(1)
                    }
                    Spacer()
                    let total = Int(track.lengthSecs)
                    if total > 0 {
                        Text(String(format: "%d:%02d", total / 60, total % 60))
                            .font(.system(size: 10))
                            .foregroundStyle(theme.playlistDurationText)
                    }
                }
                .listRowBackground(theme.playlistBg)
            }
            .listStyle(.plain)
            .background(theme.playlistBg)
            .scrollContentBackground(.hidden)

            Divider().background(theme.windowBorder)

            // ── Controls ───────────────────────────────────────────────────────
            HStack(spacing: 8) {
                Button { openFilePicker() } label: {
                    Label("Add Files…", systemImage: "doc.badge.plus").font(.system(size: 11))
                }
                .buttonStyle(.borderless).foregroundStyle(theme.playlistText)

                Button { openFolderPicker() } label: {
                    Label("Add Folder…", systemImage: "folder.badge.plus").font(.system(size: 11))
                }
                .buttonStyle(.borderless).foregroundStyle(theme.playlistText)

                if !trackSelection.isEmpty {
                    Button {
                        editingTracks.removeAll { trackSelection.contains($0.id) }
                        trackSelection.removeAll()
                    } label: {
                        Label("Remove", systemImage: "minus").font(.system(size: 11))
                    }
                    .buttonStyle(.borderless).foregroundStyle(.red)
                }

                if !editingTracks.isEmpty {
                    Button {
                        editingTracks.removeAll()
                        trackSelection.removeAll()
                    } label: {
                        Label("Remove All", systemImage: "trash").font(.system(size: 11))
                    }
                    .buttonStyle(.borderless).foregroundStyle(.red)
                }

                Spacer()

                // Save As is always available; Save is only for managed playlists.
                Button("Save As…") {
                    saveAsText = playlistName
                    showingSaveAs = true
                }
                .buttonStyle(.bordered).controlSize(.small)

                if hasChanges {
                    Button("Cancel") { loadPlaylist() }
                        .buttonStyle(.bordered).controlSize(.small)

                    if isManaged {
                        Button("Save") {
                            model.mlSavePlaylist(id: playlistId, trackIds: editingTracks.map(\.id))
                            savedTrackIds = editingTracks.map(\.id)
                        }
                        .buttonStyle(.borderedProminent).controlSize(.small)
                    }
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(theme.background)
        }
        .background(theme.playlistBg)
        .onAppear { loadPlaylist() }
        .onChange(of: playlistId) { _, _ in loadPlaylist() }
        .sheet(isPresented: $showingRename) {
            VStack(spacing: 16) {
                Text("Rename Playlist").font(.headline)
                TextField("Name", text: $renameText)
                    .textFieldStyle(.roundedBorder).frame(width: 260)
                HStack {
                    Button("Cancel") { showingRename = false }
                    Spacer()
                    Button("Rename") {
                        showingRename = false
                        model.mlRenamePlaylist(id: playlistId, name: renameText)
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(renameText.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            .padding(24).frame(width: 320)
        }
        .sheet(isPresented: $showingSaveAs) {
            VStack(spacing: 16) {
                Text("Save As New Playlist").font(.headline)
                TextField("Playlist name", text: $saveAsText)
                    .textFieldStyle(.roundedBorder).frame(width: 260)
                HStack {
                    Button("Cancel") { showingSaveAs = false }
                    Spacer()
                    Button("Save") {
                        let name = saveAsText.trimmingCharacters(in: .whitespaces)
                        let paths = editingTracks.map(\.path)
                        let newId = model.mlSavePlaylistAs(name: name, trackPaths: paths)
                        showingSaveAs = false
                        if newId >= 0 {
                            model.mlRefreshSavedPlaylists()
                            nav = .playlist(id: newId)
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(saveAsText.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            .padding(24).frame(width: 320)
        }
    }

    private func loadPlaylist() {
        editingTracks = model.mlGetPlaylistTracks(id: playlistId)
        savedTrackIds = editingTracks.map(\.id)
        trackSelection.removeAll()
    }

    private func openFilePicker() {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection  = true
        panel.canChooseFiles           = true
        panel.canChooseDirectories     = false
        panel.allowedContentTypes = [
            .init(filenameExtension: "mp3")!, .init(filenameExtension: "flac")!,
            .init(filenameExtension: "ogg")!, .init(filenameExtension: "m4a")!,
            .init(filenameExtension: "wav")!, .init(filenameExtension: "aac")!,
            .init(filenameExtension: "opus")!,
        ]
        panel.begin { resp in
            guard resp == .OK else { return }
            let newTracks = panel.urls.compactMap { url -> MLTrack? in
                model.mlTracks.first { $0.path == url.path }
            }
            Task { @MainActor in
                let existing = Set(editingTracks.map(\.id))
                for t in newTracks where !existing.contains(t.id) { editingTracks.append(t) }
            }
        }
    }

    private func openFolderPicker() {
        let panel = NSOpenPanel()
        panel.canChooseFiles       = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.begin { resp in
            guard resp == .OK, let url = panel.url else { return }
            let matching = model.mlTracks.filter { $0.path.hasPrefix(url.path) }
            Task { @MainActor in
                let existing = Set(editingTracks.map(\.id))
                for t in matching where !existing.contains(t.id) { editingTracks.append(t) }
            }
        }
    }
}

// MARK: - ML table event

enum MLTableEvent {
    case sortChanged
    case addToPlaylist([Int64])
    case replacePlaylist([Int64])
    case editTags(Int64)
    case removeTracks([Int64])
    case doubleClick([Int64])
    case viewArt(Int64)
}

// MARK: - ML files table

struct MLFilesTable: View {
    let tracks: [MLTrack]
    @Binding var selection: Set<Int64>
    @Binding var sortOrder: [KeyPathComparator<MLTrack>]
    let columnMask: Int
    @Binding var columnCustomization: TableColumnCustomization<MLTrack>
    let theme: SkinTheme
    let onEvent: (MLTableEvent) -> Void

    private func isVisible(_ bit: Int) -> Bool { (columnMask >> bit) & 1 == 1 }

    var body: some View {
        Table(tracks, selection: $selection, sortOrder: $sortOrder,
              columnCustomization: $columnCustomization) {
            TableColumn("") { row in statusCell(row) }.width(20)
            columnsA()
            columnsB()
        }
        .onChange(of: sortOrder) { _, _ in onEvent(.sortChanged) }
        // primaryAction fires on double-click (correct SwiftUI Table API)
        .contextMenu(forSelectionType: Int64.self) { ids in
            Button("Add to Playlist")          { onEvent(.addToPlaylist(Array(ids))) }
            Button("Replace Current Playlist") { onEvent(.replacePlaylist(Array(ids))) }
            Divider()
            Button("Edit / View ID3 Tags") {
                if let first = ids.first { onEvent(.editTags(first)) }
            }
            .disabled(ids.count != 1)
            Button("View Album Art") {
                if let first = ids.first { onEvent(.viewArt(first)) }
            }
            .disabled(ids.count != 1)
            Divider()
            Button("Remove from Library", role: .destructive) {
                onEvent(.removeTracks(Array(ids)))
            }
        } primaryAction: { ids in
            if !ids.isEmpty { onEvent(.doubleClick(Array(ids))) }
        }
        .background(theme.playlistBg)
        .scrollContentBackground(.hidden)
        .foregroundStyle(theme.playlistText)
    }

    @ViewBuilder
    private func statusCell(_ row: MLTrack) -> some View {
        if row.fileMissing {
            Image(systemName: "xmark.circle.fill")
                .font(.system(size: 9)).foregroundStyle(.red)
                .help("File not found at recorded path")
        } else if !row.scanned {
            Image(systemName: "clock")
                .font(.system(size: 9)).foregroundStyle(theme.playlistDurationText)
                .help("Not yet scanned")
        } else if row.readOnly {
            Image(systemName: "lock.fill")
                .font(.system(size: 9)).foregroundStyle(theme.playlistDurationText)
                .help("Read-only file")
        } else {
            Color.clear
        }
    }

    @TableColumnBuilder<MLTrack, KeyPathComparator<MLTrack>>
    private func columnsA() -> some TableColumnContent<MLTrack, KeyPathComparator<MLTrack>> {
        if isVisible(0) {
            TableColumn("Title", value: \.title) { row in
                Text(row.title.isEmpty ? row.filename : row.title)
                    .font(.system(size: 12))
                    .foregroundStyle(
                        row.fileMissing  ? Color.red
                        : row.scanned    ? theme.playlistText
                        : theme.playlistDurationText
                    )
            }
            .customizationID("col-title")
        }
        if isVisible(1) {
            TableColumn("Artist", value: \.artist) { row in
                Text(row.artist).font(.system(size: 12))
                    .foregroundStyle(row.fileMissing ? Color.red : theme.playlistText)
            }
            .customizationID("col-artist")
        }
        if isVisible(2) {
            TableColumn("Album", value: \.album) { row in
                Text(row.album).font(.system(size: 12))
                    .foregroundStyle(row.fileMissing ? Color.red : theme.playlistText)
            }
            .customizationID("col-album")
        }
        if isVisible(3) {
            TableColumn("Album Artist", value: \.albumArtist) { row in
                Text(row.albumArtist).font(.system(size: 12))
                    .foregroundStyle(row.fileMissing ? Color.red : theme.playlistText)
            }
            .customizationID("col-albumartist")
        }
        if isVisible(4) {
            TableColumn("Genre", value: \.genre) { row in
                Text(row.genre).font(.system(size: 12))
                    .foregroundStyle(row.fileMissing ? Color.red : theme.playlistText)
            }
            .customizationID("col-genre")
        }
        if isVisible(5) {
            TableColumn("Composer", value: \.composer) { row in
                Text(row.composer).font(.system(size: 12))
                    .foregroundStyle(row.fileMissing ? Color.red : theme.playlistText)
            }
            .customizationID("col-composer")
        }
        if isVisible(6) {
            TableColumn("Year", value: \.year) { row in
                Text(row.year > 0 ? "\(row.year)" : "").font(.system(size: 12))
                    .foregroundStyle(row.fileMissing ? Color.red : theme.playlistText)
            }
            .customizationID("col-year")
        }
    }

    @TableColumnBuilder<MLTrack, KeyPathComparator<MLTrack>>
    private func columnsB() -> some TableColumnContent<MLTrack, KeyPathComparator<MLTrack>> {
        if isVisible(7) {
            TableColumn("Track #", value: \.trackNum) { row in
                Text(row.trackNum > 0 ? "\(row.trackNum)" : "")
                    .font(.system(size: 12)).foregroundStyle(theme.playlistText)
            }
            .customizationID("col-tracknum")
        }
        if isVisible(8) {
            TableColumn("Disc #", value: \.discNum) { row in
                Text(row.discNum > 0 ? "\(row.discNum)" : "")
                    .font(.system(size: 12)).foregroundStyle(theme.playlistText)
            }
            .customizationID("col-discnum")
        }
        if isVisible(9) {
            TableColumn("BPM", value: \.bpm) { row in
                Text(row.bpm).font(.system(size: 12)).foregroundStyle(theme.playlistText)
            }
            .customizationID("col-bpm")
        }
        if isVisible(10) {
            TableColumn("Comment", value: \.comment) { row in
                Text(row.comment).font(.system(size: 12)).foregroundStyle(theme.playlistText)
            }
            .customizationID("col-comment")
        }
        if isVisible(11) {
            TableColumn("Duration", value: \.lengthSecs) { row in
                let total = Int(row.lengthSecs)
                Text(total > 0 ? String(format: "%d:%02d", total / 60, total % 60) : "")
                    .font(.system(size: 10)).foregroundStyle(theme.playlistDurationText)
            }
            .customizationID("col-duration")
        }
        if isVisible(12) {
            TableColumn("Bitrate", value: \.bitrate) { row in
                Text(row.bitrate > 0 ? "\(row.bitrate) kbps" : "")
                    .font(.system(size: 10)).foregroundStyle(theme.playlistDurationText)
            }
            .customizationID("col-bitrate")
        }
        if isVisible(13) {
            TableColumn("Filename", value: \.filename) { row in
                Text(row.filename)
                    .font(.system(size: 10)).foregroundStyle(theme.playlistDurationText)
            }
            .customizationID("col-filename")
        }
        if isVisible(14) {
            TableColumn("Play Count", value: \.playCount) { row in
                Text(row.playCount > 0 ? "\(row.playCount)" : "")
                    .font(.system(size: 10)).foregroundStyle(theme.playlistDurationText)
            }
            .customizationID("col-playcount")
        }
        if isVisible(15) {
            TableColumn("Art") { row in
                if row.hasArt {
                    Button("View") { onEvent(.viewArt(row.id)) }
                        .buttonStyle(.borderless)
                        .font(.system(size: 10))
                        .foregroundStyle(theme.playlistCurrentText)
                }
            }
            .customizationID("col-art")
        }
    }
}
