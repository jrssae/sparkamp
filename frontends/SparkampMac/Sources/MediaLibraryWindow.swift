import SwiftUI
import AppKit
import UniformTypeIdentifiers

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

// MARK: - Playlist management (nav = .playlists)

private struct MLPlaylistManagement: View {
    @Binding var nav: MLNavigation
    let theme: SkinTheme

    @EnvironmentObject var model: SparkampModel

    @State private var showingRename = false
    @State private var renameText    = ""
    @State private var renameTarget: Int64? = nil

    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Saved Playlists")
                    .font(theme.vars.bodyFont.weight(.semibold))
                    .foregroundStyle(theme.playlistDurationText)
                Spacer()
                // Prominent New Playlist control — uses the same native
                // Save panel as the active-playlist Save button and the
                // right-click "New Playlist…" entry.  Single consistent
                // path for choosing the playlist's destination.
                Button {
                    runPlaylistSavePanel(model: model,
                                         defaultName: "New Playlist") { stem, dir in
                        let id = model.mlSavePlaylistAs(name: stem,
                                                        trackPaths: [],
                                                        directory: dir)
                        if id >= 0 {
                            model.mlRefreshSavedPlaylists()
                            nav = .playlist(id: id)
                        }
                    }
                } label: {
                    Label("New Playlist", systemImage: "plus")
                        .font(theme.vars.bodyFont)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .help("Create a new playlist file via Save panel")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(theme.background)

            Divider().background(theme.windowBorder)

            if model.mlSavedPlaylists.isEmpty {
                Spacer()
                Text("No saved playlists yet.\nClick + to create one.")
                    .multilineTextAlignment(.center)
                    .font(theme.vars.bodyFont)
                    .foregroundStyle(theme.playlistDurationText)
                Spacer()
            } else {
                List(model.mlSavedPlaylists) { pl in
                    HStack(spacing: 8) {
                        Image(systemName: "play.rectangle")
                            .font(.system(size: 10))
                            .foregroundStyle(theme.playlistDurationText)
                        Text(pl.name)
                            .font(theme.vars.bodyFont)
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
                .tint(theme.vars.highlight)
            }
        }
        .background(theme.playlistBg)
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

// MARK: - ML editor row model (file-scope so the AppKit wrapper can reference it)

/// Wrapper around `MLTrack` with a monotonic offset id, used as the
/// selection key in the saved-playlist editor.  `MLTrack.id` is the DB
/// row id and can collide for unscanned/stub tracks (id == 0); the
/// offset id guarantees uniqueness regardless of duplicates.
struct MLEditingRow: Identifiable {
    let id: Int
    var track: MLTrack
}

// MARK: - ML playlist editor NSTableView wrapper

/// AppKit-backed list for the saved-playlist editor.  Same rationale as
/// `ActivePlaylistTable` in PlaylistView.swift: NSTableView gives proper
/// Finder-style click-vs-drag arbitration (no SwiftUI .onDrag click-lag)
/// and free multi-row drag.  The selection binding tracks `EditingRow.id`
/// (a monotonic offset) so duplicate DB rows in a playlist don't collide.
struct MLEditorTable: NSViewRepresentable {
    /// Rows in CURRENT DISPLAY ORDER (already sorted by the editor view
    /// according to `sortKey` / `sortAscending`).  The wrapper renders
    /// them in this order; the parent owns the canonical play-order
    /// array and shuffles it on drag-reorder events.
    let rows: [MLEditingRow]
    let currentTheme: SkinTheme
    @ObservedObject var themeManager: ThemeManager
    @Binding var selection: Set<Int>
    /// Same column-visibility bitmask used by `MLFilesTable`.  Drives
    /// `column.isHidden` per spec; editor reuses the Files-view column
    /// set so both views look identical.
    let columnMask: Int
    /// SQL-style sort key currently applied to the editor's display.
    /// `"position"` means play-order — clicking the # header toggles
    /// ASC/DESC.  Any other key sorts purely visually (canonical
    /// play-order array is unchanged).
    let sortKey: String
    let sortAscending: Bool
    let contextMenuBuilder: (Set<Int>) -> NSMenu?
    /// Called when the user drops file URLs onto the editor.  `paths` is
    /// the resolved list; the closure is responsible for adding them via
    /// `appendTracks` / `mlGetTrackByPath` lookups.
    let onDropPaths: ([String]) -> Void
    /// 1-based play-order position for a given row id.  Always reflects
    /// the row's index in the canonical play order regardless of current
    /// sort, so the # column is a stable reference even when the user
    /// sorts by title/artist/etc.
    let positionFor: (Int) -> Int
    /// Fired when the user clicks a column header.  Carries the SQL
    /// sort key + ascending flag.  Parent updates its sort state and
    /// the next render passes the sorted rows back.
    var onSortChange: ((String, Bool) -> Void)? = nil
    /// Fired when the user drags rows to a new slot inside the editor.
    /// Only emitted when sort == "position" + ASC — other sort orders
    /// don't have a sensible "insert here" position so reorder is
    /// rejected at validateDrop time.  `from` indices are into
    /// `editingRows` (canonical play order); `to` is the destination
    /// insertion index per SwiftUI's `onMove` convention.
    var onReorder: ((IndexSet, Int) -> Void)? = nil

    /// Optional callback invoked when the user presses Delete inside the
    /// editor.  Receives the set of row ids to remove from the editor's
    /// `editingRows` state.  Required because the row array lives in the
    /// parent SwiftUI view, not the wrapper.
    var requestDeleteRows: ((Set<Int>) -> Void)? = nil

    /// True when the editor's current sort allows intra-list drag-reorder
    /// (only sort by play-order ascending preserves the bijection between
    /// display index and play-order index).
    private var reorderAllowed: Bool {
        sortKey == "position" && sortAscending
    }

    func makeNSView(context: Context) -> NSScrollView {
        let table = SparkampTableView()
        table.allowsMultipleSelection = true
        table.usesAlternatingRowBackgroundColors = false
        table.backgroundColor = .clear
        table.style = .inset
        table.gridStyleMask = []
        table.intercellSpacing = NSSize(width: 6, height: 2)
        table.rowHeight = 20
        table.selectionHighlightStyle = .regular
        table.focusRingType = .none
        table.allowsColumnReordering = true
        table.allowsColumnResizing = true
        table.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        table.autosaveName = "sparkamp.ml.editorTable"
        table.autosaveTableColumns = true

        // Build the SAME columns as MLFilesTable, plus editor-only
        // entries (the # play-position column).  Sort prototypes are
        // set on every sortable column (matching Files view).  The
        // editor preserves canonical play order in `editingRows`; any
        // sort other than "position" is a transient DISPLAY sort that
        // doesn't mutate the underlying order.  Drag-reorder is gated
        // separately to position+ASC so a misclick on another header
        // can never destroy the user's playback sequence.
        for spec in MLFilesTable.specs {
            let col = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(spec.id))
            col.title = spec.title
            col.width = spec.width
            col.minWidth = max(20, spec.width * 0.3)
            col.maxWidth = max(spec.width * 4, 600)
            col.resizingMask = [.userResizingMask, .autoresizingMask]
            if spec.id == "col-status" || spec.id == "col-position" {
                // Pinned: fixed width, no resize, no reorder.
                col.minWidth = spec.width
                col.maxWidth = spec.width
                col.resizingMask = []
            }
            if let key = spec.sortKey {
                col.sortDescriptorPrototype = NSSortDescriptor(key: key, ascending: true)
            }
            table.addTableColumn(col)
        }
        for col in table.tableColumns {
            if let spec = MLFilesTable.specs.first(where: { $0.id == col.identifier.rawValue }) {
                col.isHidden = !(spec.bit < 0 || (columnMask >> spec.bit) & 1 == 1)
            }
        }
        // Default sort: play-order ascending.  Will be re-applied by
        // updateNSView whenever the parent's sort state changes.
        if table.sortDescriptors.isEmpty {
            table.sortDescriptors = [NSSortDescriptor(key: "position", ascending: true)]
        }

        table.dataSource = context.coordinator
        table.delegate   = context.coordinator

        table.registerForDraggedTypes([.fileURL])
        // Local drag includes .move so intra-table reorder works when
        // sort = position + ASC (validateDrop gates this); cross-target
        // drops always copy.
        table.setDraggingSourceOperationMask([.copy, .move], forLocal: true)
        table.setDraggingSourceOperationMask([.copy],        forLocal: false)

        table.onDeleteKey   = { [weak c = context.coordinator] in c?.handleDelete() }
        table.onContextMenu = { [weak c = context.coordinator] _ in c?.buildContextMenu() }
        // No return-key action for editor: there's no "play this row" semantic
        // in the saved-playlist editor (user must add to active list first).

        context.coordinator.table = table

        let scroll = NSScrollView()
        scroll.documentView      = table
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.drawsBackground   = false
        scroll.borderType        = .noBorder
        scroll.autohidesScrollers = true
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let table = scroll.documentView as? SparkampTableView else { return }
        context.coordinator.parent = self
        let oldIds = context.coordinator.rows.map(\.id)
        let newIds = rows.map(\.id)
        context.coordinator.rows = rows
        if oldIds != newIds {
            table.reloadData()
        } else {
            // Same rows, theme may have changed — refresh visible cells.
            let visible = table.rows(in: table.visibleRect)
            for r in visible.location..<(visible.location + visible.length)
                where r < rows.count {
                for c in 0..<table.numberOfColumns {
                    let colId = table.tableColumns[c].identifier.rawValue
                    guard let cell = table.view(atColumn: c, row: r, makeIfNecessary: false)
                                     as? SparkampHostingCellView
                    else { continue }
                    if colId == "col-position" {
                        cell.setContent(Self.positionCellContent(
                            position: positionFor(rows[r].id),
                            theme: currentTheme))
                    } else if let spec = MLFilesTable.specs.first(where: { $0.id == colId }) {
                        cell.setContent(MLFilesTable.cellContent(
                            track: rows[r].track, spec: spec,
                            theme: currentTheme,
                            onViewArt: { _ in }
                        ))
                    }
                }
            }
        }

        // Column visibility from columnMask.
        for col in table.tableColumns {
            if let spec = MLFilesTable.specs.first(where: { $0.id == col.identifier.rawValue }) {
                let shouldBeHidden = !(spec.bit < 0 || (columnMask >> spec.bit) & 1 == 1)
                if col.isHidden != shouldBeHidden { col.isHidden = shouldBeHidden }
            }
        }
        // Re-pin status (slot 0) and position (slot 1) columns after
        // any autosave restore.  Both must stay at the start of the row
        // so the editor's #-column anchor and the error indicator are
        // always at predictable, recognisable positions.
        if let statusIdx = table.tableColumns.firstIndex(where: {
            $0.identifier.rawValue == "col-status"
        }), statusIdx != 0 {
            table.moveColumn(statusIdx, toColumn: 0)
        }
        if let posIdx = table.tableColumns.firstIndex(where: {
            $0.identifier.rawValue == "col-position"
        }), posIdx != 1 {
            table.moveColumn(posIdx, toColumn: 1)
        }

        // Sort descriptors are owned by NSTableView, same as Files view —
        // pushing the parent's `sortKey` / `sortAscending` back into the
        // table on every update would race with the user-click → async
        // dispatch flow and briefly revert the user's chosen sort.  The
        // sortedRows the parent passes already reflects the current sort,
        // so no programmatic resync is needed at steady state.

        let desired = IndexSet(
            rows.enumerated()
                .filter { selection.contains($0.element.id) }
                .map(\.offset)
        )
        if table.selectedRowIndexes != desired {
            context.coordinator.applyingExternalSelection = true
            table.selectRowIndexes(desired, byExtendingSelection: false)
            context.coordinator.applyingExternalSelection = false
        }
    }

    /// SwiftUI content for the editor's # (play-position) column.
    /// Receives the 1-based play-order index and renders it in the same
    /// small-mono style as duration/bitrate cells.
    fileprivate static func positionCellContent(position: Int, theme: SkinTheme) -> AnyView {
        AnyView(
            Text("\(position)")
                .font(theme.vars.smallMonospaceFont)
                .foregroundStyle(theme.playlistDurationText)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .trailing)
                .padding(.trailing, 6)
        )
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    @MainActor final class Coordinator: NSObject, NSTableViewDataSource, NSTableViewDelegate {
        var parent: MLEditorTable
        var rows: [MLEditingRow] = []
        weak var table: SparkampTableView?
        var applyingExternalSelection = false
        /// True while updateNSView is programmatically updating the
        /// table's `sortDescriptors` — used by `sortDescriptorsDidChange`
        /// to ignore that sync and only react to actual user clicks.
        var applyingExternalSort = false
        private let cellId = NSUserInterfaceItemIdentifier("mlEditorCell")

        init(_ parent: MLEditorTable) {
            self.parent = parent
            self.rows = parent.rows
        }

        func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

        // Block reorder that would move status (slot 0) or position (slot 1)
        // off their anchor slots, or move another column INTO those slots.
        // Both columns are visual anchors users learn to find at the start
        // of every row.
        func tableView(_ tableView: NSTableView,
                       shouldReorderColumn columnIndex: Int,
                       toColumn newColumnIndex: Int) -> Bool {
            let col = tableView.tableColumns[columnIndex]
            if col.identifier.rawValue == "col-status"
                || col.identifier.rawValue == "col-position" {
                return false
            }
            if newColumnIndex <= 1 { return false }
            return true
        }

        func tableView(_ tableView: NSTableView,
                       viewFor tableColumn: NSTableColumn?,
                       row: Int) -> NSView? {
            guard let column = tableColumn, row < rows.count else { return nil }
            let colId = column.identifier.rawValue
            let cell = (tableView.makeView(withIdentifier: cellId, owner: nil)
                        as? SparkampHostingCellView) ?? SparkampHostingCellView()
            cell.identifier = cellId
            // Position column: not present in MLFilesTable.cellContent —
            // editor renders it directly from the row's index in the
            // canonical play order (supplied by `positionFor`).
            if colId == "col-position" {
                cell.setContent(MLEditorTable.positionCellContent(
                    position: parent.positionFor(rows[row].id),
                    theme: parent.currentTheme))
                return cell
            }
            guard let spec = MLFilesTable.specs.first(where: { $0.id == colId }) else {
                return nil
            }
            cell.setContent(MLFilesTable.cellContent(
                track: rows[row].track,
                spec: spec,
                theme: parent.currentTheme,
                // No "view art" hook from editor — would need plumbing all
                // the way back to the model; users do this from Files view
                // or via the right-click "View Album Art" menu instead.
                onViewArt: { _ in }
            ))
            return cell
        }

        // Skin-tinted row view for selection paint — same wiring as the
        // active-playlist and Files tables.  See SparkampSkinRowView in
        // PlaylistView.swift.
        func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
            SparkampSkinRowView()
        }

        func tableViewSelectionDidChange(_ notification: Notification) {
            guard !applyingExternalSelection, let table = self.table else { return }
            let ids = table.selectedRowIndexes.compactMap { idx -> Int? in
                guard idx < rows.count else { return nil }
                return rows[idx].id
            }
            let new = Set(ids)
            if parent.selection != new {
                DispatchQueue.main.async { [weak self] in self?.parent.selection = new }
            }
        }

        // User clicked a column header → fire onSortChange so the parent
        // re-sorts editingRows accordingly.  Ignored when the change is
        // being pushed by updateNSView (parent state authoritative).
        func tableView(_ tableView: NSTableView,
                       sortDescriptorsDidChange oldDescriptors: [NSSortDescriptor]) {
            if applyingExternalSort { return }
            guard let first = tableView.sortDescriptors.first,
                  let key = first.key
            else { return }
            let asc = first.ascending
            DispatchQueue.main.async { [weak self] in
                self?.parent.onSortChange?(key, asc)
            }
        }

        // Drag source: emit one fileURL per row.
        func tableView(_ tableView: NSTableView,
                       pasteboardWriterForRow row: Int) -> NSPasteboardWriting? {
            guard row < rows.count else { return nil }
            let path = rows[row].track.path
            guard !path.isEmpty else { return nil }
            let pbItem = NSPasteboardItem()
            pbItem.setData(URL(fileURLWithPath: path).dataRepresentation,
                           forType: .fileURL)
            return pbItem
        }

        // Drop destination:
        //   - Cross-source drop: always allowed → .copy (append paths).
        //   - Intra-list drop:   allowed iff sort == position + ASC
        //                         (then .move = reorder); else rejected.
        // Intra-list .move uses `.above` semantics so the user can drop
        // between rows for precise insertion.
        func tableView(_ tableView: NSTableView,
                       validateDrop info: NSDraggingInfo,
                       proposedRow row: Int,
                       proposedDropOperation dropOperation: NSTableView.DropOperation) -> NSDragOperation {
            let isIntra = (info.draggingSource as? NSTableView) === tableView
            if isIntra {
                guard parent.reorderAllowed else { return [] }
                if dropOperation == .on {
                    tableView.setDropRow(row, dropOperation: .above)
                }
                return .move
            }
            // External / cross-list drop: append-to-end semantics.
            tableView.setDropRow(-1, dropOperation: .on)
            return .copy
        }

        func tableView(_ tableView: NSTableView,
                       acceptDrop info: NSDraggingInfo,
                       row: Int,
                       dropOperation: NSTableView.DropOperation) -> Bool {
            let isIntra = (info.draggingSource as? NSTableView) === tableView
            if isIntra {
                guard parent.reorderAllowed else { return false }
                let from = tableView.selectedRowIndexes
                guard !from.isEmpty else { return false }
                parent.onReorder?(from, row)
                return true
            }
            let urls = info.draggingPasteboard
                .readObjects(forClasses: [NSURL.self], options: nil) as? [URL] ?? []
            let paths = urls.map(\.path).filter { !$0.isEmpty }
            guard !paths.isEmpty else { return false }
            parent.onDropPaths(paths)
            return true
        }

        func handleDelete() {
            guard let table = self.table else { return }
            let ids = table.selectedRowIndexes.compactMap { idx -> Int? in
                guard idx < rows.count else { return nil }
                return rows[idx].id
            }
            let idSet = Set(ids)
            DispatchQueue.main.async { [weak self] in
                self?.parent.selection.subtract(idSet)
            }
            parent.requestDeleteRows?(idSet)
        }

        func buildContextMenu() -> NSMenu? {
            guard let table = self.table else { return nil }
            let clicked = table.clickedRow
            if clicked >= 0 && !table.selectedRowIndexes.contains(clicked) {
                table.selectRowIndexes(IndexSet(integer: clicked),
                                       byExtendingSelection: false)
            }
            let ids: Set<Int> = Set(table.selectedRowIndexes.compactMap { idx -> Int? in
                guard idx < rows.count else { return nil }
                return rows[idx].id
            })
            return parent.contextMenuBuilder(ids)
        }
    }
}

// MARK: - Playlist track editor (nav = .playlist(id:))

private struct MLPlaylistEditor: View {
    let playlistId: Int64
    @Binding var nav: MLNavigation
    let theme: SkinTheme
    /// Column-visibility bitmask shared with `MLFilesTable`.  Editor uses
    /// the same column set + same visibility toggles so both views look
    /// identical when the user shows/hides columns from the picker menu.
    let columnMask: Int

    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    // Row wrapper `MLEditingRow` lives at file scope so the AppKit
    // `MLEditorTable` wrapper can reference it.
    @State private var editingRows: [MLEditingRow] = []
    /// Current sort key applied to the editor's display.  `"position"`
    /// means play-order — clicking the # column header toggles ASC/DESC.
    /// Default is play-order ascending so the editor opens in the same
    /// order tracks would actually play.
    @State private var editorSortKey: String = "position"
    @State private var editorSortAscending: Bool = true
    @State private var savedTrackIds: [Int64]   = []
    @State private var trackSelection: Set<Int> = []
    @State private var nextRowId: Int = 0
    @State private var showingRename  = false
    @State private var renameText     = ""

    private var editingTracks: [MLTrack] { editingRows.map(\.track) }
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
            // Title + rename live in MediaLibraryView's toolbar.
            if !playlistPath.isEmpty {
                HStack {
                    // Selectable so users can copy/paste the on-disk path.
                    Text(playlistPath)
                        .font(theme.vars.bodyFont)
                        .foregroundStyle(theme.playlistDurationText)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                    Spacer()
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 5)
                .background(theme.background)

                Divider().background(theme.windowBorder)
            }

            // ── Track list ─────────────────────────────────────────────────────
            MLEditorTable(
                rows: sortedRows,
                currentTheme: theme,
                themeManager: themeManager,
                selection: $trackSelection,
                columnMask: columnMask,
                sortKey: editorSortKey,
                sortAscending: editorSortAscending,
                contextMenuBuilder: { ids in editorContextMenu(rowIds: ids) },
                onDropPaths: { paths in handleEditorDrop(paths: paths) },
                positionFor: { id in playPosition(forRowId: id) },
                onSortChange: { key, asc in applyEditorSort(key: key, ascending: asc) },
                onReorder: { from, to in reorderEditorRows(from: from, to: to) },
                requestDeleteRows: { ids in deleteEditorRows(ids: ids) }
            )
            .background(theme.playlistBg)

            Divider().background(theme.windowBorder)

            // ── Controls ───────────────────────────────────────────────────────
            HStack(spacing: 8) {
                Button { openFilePicker() } label: {
                    Label("Add Files…", systemImage: "doc.badge.plus").font(theme.vars.bodyFont)
                }
                .buttonStyle(.borderless).foregroundStyle(theme.playlistText)

                Button { openFolderPicker() } label: {
                    Label("Add Folder…", systemImage: "folder.badge.plus").font(theme.vars.bodyFont)
                }
                .buttonStyle(.borderless).foregroundStyle(theme.playlistText)

                Button {
                    editingRows.removeAll { trackSelection.contains($0.id) }
                    trackSelection.removeAll()
                } label: {
                    Label("Remove", systemImage: "minus").font(theme.vars.bodyFont)
                }
                .buttonStyle(.borderless).foregroundStyle(.red)
                .disabled(trackSelection.isEmpty)

                Button {
                    model.mlDeletePlaylist(id: playlistId)
                    nav = .playlists
                } label: {
                    Label("Delete Playlist", systemImage: "trash").font(theme.vars.bodyFont)
                }
                .buttonStyle(.borderless).foregroundStyle(.red)
                .help("Delete this playlist")

                Spacer()

                Button("Rename") {
                    renameText = playlistName
                    showingRename = true
                }
                .buttonStyle(.bordered).controlSize(.small)
                .help("Rename Playlist")

                Button("Save As…") { openSaveAsPanel() }
                    .buttonStyle(.bordered).controlSize(.small)
                    .help("Save a copy to a new file")

                Button("Revert") { loadPlaylist() }
                    .buttonStyle(.bordered).controlSize(.small)
                    .disabled(!hasChanges)

                Button("Save") {
                    let ids = editingTracks.map(\.id)
                    model.mlSavePlaylist(id: playlistId, trackIds: ids)
                    savedTrackIds = ids
                }
                .buttonStyle(.borderedProminent).controlSize(.small)
                .disabled(!(hasChanges && isManaged))

                // Whole-playlist actions: Enqueue appends every track to the
                // active playlist; Play replaces it (and starts playback if
                // the autoplay-on-add preference allows).
                Button("Enqueue") {
                    let ids = editingTracks.map(\.id).filter { $0 != 0 }
                    if !ids.isEmpty { model.mlAddToPlaylist(ids: ids) }
                }
                .buttonStyle(.bordered).controlSize(.small)
                .disabled(editingRows.isEmpty)
                .help("Append all tracks to the active playlist")

                Button("Play") {
                    let ids = editingTracks.map(\.id).filter { $0 != 0 }
                    if !ids.isEmpty { model.mlReplacePlaylistWith(ids: ids) }
                }
                .buttonStyle(.borderedProminent).controlSize(.small)
                .disabled(editingRows.isEmpty)
                .help("Replace the active playlist with this one")
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(theme.background)
        }
        .background(theme.playlistBg)
        .onAppear { loadPlaylist() }
        .onChange(of: playlistId) { _, _ in loadPlaylist() }
        // Re-read the playlist file whenever its contents change out from under us
        // (right-click "Add to Playlist" from the active playlist, Save,
        // Save-As, etc.).  Skip while the user has unsaved edits so we don't
        // discard them mid-edit.
        .onChange(of: model.mlPlaylistContentTrigger) { _, _ in
            if !hasChanges { loadPlaylist() }
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
                        model.mlRenamePlaylist(id: playlistId, name: renameText)
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(renameText.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            .padding(24).frame(width: 320)
        }
    }

    /// Native macOS NSSavePanel so the user can pick a destination folder
    /// AND filename (instead of being limited to a name-only text field
    /// inside Sparkamp's managed playlists directory).
    private func openSaveAsPanel() {
        let panel = NSSavePanel()
        panel.title                  = "Save Playlist As…"
        // Accept both extensions — legacy .m3u readers still work, but the
        // default we suggest is .m3u8 (UTF-8 explicit) per project policy.
        panel.allowedContentTypes    = [
            .init(filenameExtension: "m3u8")!,
            .init(filenameExtension: "m3u")!,
        ]
        panel.canCreateDirectories   = true
        panel.isExtensionHidden      = false
        // Default name + .m3u8; user can rename in the panel.
        panel.nameFieldStringValue   = "\(playlistName).m3u8"
        // Default location: first watched ML folder, else the user's ~/Music
        // folder.  Falls back to Sparkamp's managed playlists dir only if
        // both are unavailable.
        panel.directoryURL = MLPlaylistEditor.defaultSaveAsDir(model: model)
        panel.begin { resp in
            guard resp == .OK, let url = panel.url else { return }
            Task { @MainActor in
                let paths = editingRows.map(\.track.path)
                // Strip the trailing extension — mlSavePlaylistAs adds ".m3u8".
                let stem = url.deletingPathExtension().lastPathComponent
                let dest = url.deletingLastPathComponent()
                let newId = model.mlSavePlaylistAs(name: stem,
                                                   trackPaths: paths,
                                                   directory: dest)
                if newId >= 0 {
                    model.mlRefreshSavedPlaylists()
                    nav = .playlist(id: newId)
                }
            }
        }
    }

    /// Sparkamp's managed-playlists directory, used as the initial location
    /// for the Save-As panel.  Returns nil if it can't be resolved.
    private static func defaultPlaylistsDir() -> URL? {
        let dir = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".config/sparkamp/playlists")
        try? FileManager.default.createDirectory(at: dir,
                                                 withIntermediateDirectories: true)
        return dir
    }

    /// Preferred default location for Save Playlist As…  Delegates to the
    /// shared `mlDefaultSaveAsDir` on the model so this view and the
    /// active-playlist "New Playlist" action stay in sync.
    private static func defaultSaveAsDir(model: SparkampModel) -> URL {
        model.mlDefaultSaveAsDir()
    }

    /// Build the right-click NSMenu shown over `rowIds` in the editor.
    /// Mirrors the previous SwiftUI `.contextMenu` but in AppKit form
    /// (because `MLEditorTable` returns an `NSMenu` to the table).
    private func editorContextMenu(rowIds: Set<Int>) -> NSMenu {
        // Map row ids back to MLTrack.id for library-level operations.
        // Stubs (id == 0) are skipped for ML actions that require a DB row.
        let dbIds = rowIds
            .compactMap { rid in editingRows.first(where: { $0.id == rid })?.track.id }
            .filter { $0 != 0 }
        let menu = NSMenu()
        menu.autoenablesItems = false

        menu.addItem(BlockMenuItem(title: "Add to Playlist", enabled: !dbIds.isEmpty) {
            model.mlAddToPlaylist(ids: dbIds)
        })
        menu.addItem(BlockMenuItem(title: "Replace Current Playlist", enabled: !dbIds.isEmpty) {
            model.mlReplacePlaylistWith(ids: dbIds)
        })
        menu.addItem(.separator())

        menu.addItem(BlockMenuItem(title: "Edit / View ID3 Tags", enabled: rowIds.count == 1) {
            if let first = rowIds.first,
               let t = editingRows.first(where: { $0.id == first })?.track {
                model.mlOpenTagEditorForPath(t.path)
            }
        })
        menu.addItem(BlockMenuItem(title: "View Album Art", enabled: rowIds.count == 1) {
            if let first = rowIds.first,
               let t = editingRows.first(where: { $0.id == first })?.track {
                model.mlViewArtForPath(t.path)
            }
        })
        menu.addItem(.separator())

        menu.addItem(BlockMenuItem(title: "Remove from Library", enabled: true) {
            if !dbIds.isEmpty { model.mlRemoveTracks(ids: dbIds) }
            // Drop the rows locally so the UI updates without waiting for
            // a reload — handles stubs (no DB row to remove) too.
            editingRows.removeAll { rowIds.contains($0.id) }
            trackSelection.subtract(rowIds)
        })
        return menu
    }

    /// File URLs dropped onto the editor: resolve to library tracks (or
    /// stub MLTracks for files not yet in the library) and append to the
    /// editor's row list.  Library DB membership is unchanged — paths
    /// that aren't in the library appear as stub rows (id == 0) which
    /// the user can save into the playlist file on Save.
    private func handleEditorDrop(paths: [String]) {
        let tracks: [MLTrack] = paths.map { p in
            model.mlGetTrackByPath(p) ?? MLTrack(stubPath: p)
        }
        appendTracks(tracks)
    }

    /// Delete-key handler routed from `MLEditorTable`.  Removes the rows
    /// from the editor's `editingRows` state; the user can Revert to
    /// undo, or Save to commit the change to the .m3u8 on disk.
    private func deleteEditorRows(ids: Set<Int>) {
        editingRows.removeAll { ids.contains($0.id) }
        trackSelection.subtract(ids)
    }

    /// Editor rows in CURRENT DISPLAY ORDER.  `editingRows` is the
    /// canonical play-order array; `sortedRows` is a derived view that
    /// applies whatever sort the user picked from a column header.
    /// Sorting by anything other than `"position"` doesn't mutate
    /// `editingRows` — it's a transient view, so clicking back to the
    /// `#` column restores the original play order without any state
    /// reconstruction.
    private var sortedRows: [MLEditingRow] {
        if editorSortKey == "position" {
            return editorSortAscending ? editingRows : Array(editingRows.reversed())
        }
        guard let cmp = MLFilesTable.keyPathComparator(forSortKey: editorSortKey,
                                                       ascending: editorSortAscending)
        else { return editingRows }
        return editingRows.sorted {
            cmp.compare($0.track, $1.track) == .orderedAscending
        }
    }

    /// 1-based play-order position for a row id.  Returns 0 if the id
    /// isn't in `editingRows` (defensive — shouldn't happen in practice).
    /// Used by the # column cell so it always shows the row's position
    /// in canonical play order, even when the display is sorted by some
    /// other column.
    private func playPosition(forRowId id: Int) -> Int {
        if let idx = editingRows.firstIndex(where: { $0.id == id }) {
            return idx + 1
        }
        return 0
    }

    /// User clicked a column header.  Update sort state; `sortedRows`
    /// recomputes automatically on next render.  When the user clicks
    /// the `#` header repeatedly the table toggles ASC ↔ DESC; clicking
    /// any other column switches to that column ASC.
    private func applyEditorSort(key: String, ascending: Bool) {
        editorSortKey = key
        editorSortAscending = ascending
    }

    /// Intra-list drag-reorder: only invoked by `MLEditorTable` when the
    /// editor is sorted by `#` ASC (the only sort that has a 1:1 mapping
    /// between display index and canonical play-order index).  Mirrors
    /// SwiftUI's `onMove` semantics — `from` rows move to slot `to`.
    /// Change lives in `editingRows`; user clicks Save to commit the
    /// new play order to the .m3u8 file on disk.
    private func reorderEditorRows(from: IndexSet, to: Int) {
        editingRows.move(fromOffsets: from, toOffset: to)
        // Selection ids are stable across the move (row identity preserved),
        // so trackSelection doesn't need to be touched.
    }

    private func loadPlaylist() {
        let tracks = model.mlGetPlaylistTracks(id: playlistId)
        // Reset the row-id counter on every full reload so the ids are
        // bounded by playlist length (avoids unbounded growth across reloads).
        nextRowId = 0
        editingRows = tracks.map { t in
            let r = MLEditingRow(id: nextRowId, track: t)
            nextRowId += 1
            return r
        }
        savedTrackIds = tracks.map(\.id)
        trackSelection.removeAll()
    }

    /// Append `tracks` to the editor, skipping any whose DB id is already
    /// present (existing-row dedup only applies to real DB rows; stubs with
    /// id == 0 are never dedupped because they can legitimately repeat).
    private func appendTracks(_ tracks: [MLTrack]) {
        let existingIds = Set(editingRows.map(\.track.id).filter { $0 != 0 })
        for t in tracks where t.id == 0 || !existingIds.contains(t.id) {
            editingRows.append(MLEditingRow(id: nextRowId, track: t))
            nextRowId += 1
        }
    }

    private func openFilePicker() {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection  = true
        panel.canChooseFiles           = true
        panel.canChooseDirectories     = false
        // Pull the canonical list from the Rust core so this picker stays
        // in sync with the scanner's whitelist (no drift between what the
        // user can add via the picker vs. what folder scans pick up).
        let count = Int(sparkamp_audio_extension_count())
        panel.allowedContentTypes = (0..<count).compactMap { i in
            guard let cstr = sparkamp_audio_extension(Int32(i)) else { return nil }
            let ext = String(cString: cstr)
            return .init(filenameExtension: ext)
        }
        panel.begin { resp in
            guard resp == .OK else { return }
            let newTracks = panel.urls.compactMap { url -> MLTrack? in
                model.mlTracks.first { $0.path == url.path }
            }
            Task { @MainActor in
                appendTracks(newTracks)
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
            // mlAllTracks bypasses the Files-view search filter so we don't
            // import only the visible search results.
            let matching = model.mlAllTracks().filter { $0.path.hasPrefix(url.path) }
            Task { @MainActor in
                appendTracks(matching)
            }
        }
    }
}

// MARK: - ML table event

enum MLTableEvent {
    /// Sort changed via column header click.  Carries the SQL column name
    /// (matching `mlFetchTracks`'s `sortCol` parameter) and direction.
    /// Passed directly through the event so the caller's `reload()` can
    /// re-fetch without round-tripping through a SwiftUI binding (binding
    /// writes are deferred and `reload()` would otherwise read a stale
    /// sortOrder).
    case sortChanged(key: String, ascending: Bool)
    case addToPlaylist([Int64])
    case replacePlaylist([Int64])
    case editTags(Int64)
    case removeTracks([Int64])
    case doubleClick([Int64])
    case viewArt(Int64)
}

// MARK: - ML files table (AppKit NSTableView wrapper)
//
// Replaces the SwiftUI `Table` previously used here.  Same rationale as
// `ActivePlaylistTable` / `MLEditorTable`: NSTableView gives Finder-style
// click-vs-drag arbitration (no SwiftUI .onDrag lag) and free multi-row
// drag.  Sort + column reorder/resize/visibility are handled natively by
// NSTableView (autosaveName persists across launches); the `columnMask`
// bits drive `column.isHidden` so the existing column-picker menu still
// controls visibility.
//
// Drop destination: when files are dropped onto the table from outside
// (or from another Sparkamp list), they're upserted into the library DB
// via `mlAddFilesToLibrary` — no new watched folder is registered, so
// paths outside every watched folder are silently skipped.

/// Drop-handler callback type — receives raw file paths the user dropped
/// onto the Files table.  Caller decides what to do (typically: pass to
/// `model.mlAddFilesToLibrary`).
typealias MLFilesDropHandler = ([String]) -> Void

struct MLFilesTable: NSViewRepresentable {
    let tracks: [MLTrack]
    @Binding var selection: Set<Int64>
    @Binding var sortOrder: [KeyPathComparator<MLTrack>]
    let columnMask: Int
    @Binding var columnCustomization: TableColumnCustomization<MLTrack>
    let theme: SkinTheme
    @ObservedObject var themeManager: ThemeManager
    let onEvent: (MLTableEvent) -> Void
    let onDropPaths: MLFilesDropHandler

    private func isVisible(_ bit: Int) -> Bool { (columnMask >> bit) & 1 == 1 }

    // ── Column descriptors ──────────────────────────────────────────────
    // Static list drives NSTableColumn construction.  Order here = default
    // column order (NSTableView autosave persists user reorders after that).
    fileprivate struct ColumnSpec {
        let id: String          // customization id, e.g. "col-title"; used as NSUserInterfaceItemIdentifier
        let title: String
        let bit: Int            // columnMask bit driving show/hide; -1 = always visible
        let width: CGFloat
        let sortKey: String?    // SQL column name for SortDescriptor; nil = not sortable
        let isSmallMono: Bool   // render with smallMonospaceFont + durationText colour
        var editorOnly: Bool = false  // skip in Files table; show only in playlist editor
    }

    fileprivate static let specs: [ColumnSpec] = [
        // Status column is special-cased: always visible, fixed 20pt, no sort.
        .init(id: "col-status",      title: "",            bit: -1, width: 20,  sortKey: nil,           isSmallMono: false),
        // Position column: 1-based play-order index for the editor's current
        // playlist.  Editor-only; never appears in the Files view.  Sorting
        // by this column is what gates intra-list drag-reorder in the
        // editor (other sorts disable reorder so the user doesn't lose
        // their play-order on a stray drag).
        .init(id: "col-position",    title: "#",           bit: -1, width: 40,  sortKey: "position",     isSmallMono: true,  editorOnly: true),
        .init(id: "col-title",       title: "Title",       bit:  0, width: 220, sortKey: "title",        isSmallMono: false),
        .init(id: "col-artist",      title: "Artist",      bit:  1, width: 160, sortKey: "artist",       isSmallMono: false),
        .init(id: "col-album",       title: "Album",       bit:  2, width: 160, sortKey: "album",        isSmallMono: false),
        .init(id: "col-albumartist", title: "Album Artist",bit:  3, width: 160, sortKey: "album_artist", isSmallMono: false),
        .init(id: "col-genre",       title: "Genre",       bit:  4, width: 110, sortKey: "genre",        isSmallMono: false),
        .init(id: "col-composer",    title: "Composer",    bit:  5, width: 140, sortKey: "composer",     isSmallMono: false),
        .init(id: "col-year",        title: "Year",        bit:  6, width:  60, sortKey: "year",         isSmallMono: false),
        .init(id: "col-tracknum",    title: "Track #",     bit:  7, width:  60, sortKey: "num",          isSmallMono: false),
        .init(id: "col-discnum",     title: "Disc #",      bit:  8, width:  60, sortKey: "disc_num",     isSmallMono: false),
        .init(id: "col-bpm",         title: "BPM",         bit:  9, width:  60, sortKey: "bpm",          isSmallMono: false),
        .init(id: "col-comment",     title: "Comment",     bit: 10, width: 160, sortKey: "comment",       isSmallMono: false),
        .init(id: "col-duration",    title: "Duration",    bit: 11, width:  80, sortKey: "duration",     isSmallMono: true),
        .init(id: "col-bitrate",     title: "Bitrate",     bit: 12, width:  80, sortKey: "bitrate",      isSmallMono: true),
        .init(id: "col-filename",    title: "Filename",    bit: 13, width: 180, sortKey: nil,            isSmallMono: true),
        .init(id: "col-playcount",   title: "Play Count",  bit: 14, width:  80, sortKey: "play_count",   isSmallMono: true),
        .init(id: "col-lastplayed",  title: "Last Played", bit: 16, width: 140, sortKey: "last_played",  isSmallMono: true),
        .init(id: "col-art",         title: "Art",         bit: 15, width:  60, sortKey: nil,            isSmallMono: false),
    ]

    func makeNSView(context: Context) -> NSScrollView {
        let table = SparkampTableView()
        table.allowsMultipleSelection = true
        table.usesAlternatingRowBackgroundColors = false
        table.backgroundColor = .clear
        table.style = .inset
        table.gridStyleMask = []
        table.intercellSpacing = NSSize(width: 6, height: 2)
        table.rowHeight = 20
        table.selectionHighlightStyle = .regular
        table.focusRingType = .none
        table.allowsColumnReordering = true
        table.allowsColumnResizing = true
        table.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        table.autosaveName = "sparkamp.ml.filesTable"
        table.autosaveTableColumns = true

        // Build columns from the static spec list.  Skip editor-only
        // entries (e.g. the play-position column) — they don't apply to
        // the library Files view.
        for spec in Self.specs where !spec.editorOnly {
            let col = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(spec.id))
            col.title = spec.title
            col.width = spec.width
            col.minWidth = max(20, spec.width * 0.3)
            col.maxWidth = max(spec.width * 4, 600)
            col.resizingMask = [.userResizingMask, .autoresizingMask]
            if let key = spec.sortKey {
                col.sortDescriptorPrototype = NSSortDescriptor(key: key, ascending: true)
            }
            // Status column: pinned, can't hide / reorder / resize.
            if spec.id == "col-status" {
                col.minWidth = spec.width
                col.maxWidth = spec.width
                col.resizingMask = []
            }
            table.addTableColumn(col)
        }
        // Apply initial visibility from columnMask.
        for col in table.tableColumns {
            if let spec = Self.specs.first(where: { $0.id == col.identifier.rawValue }) {
                col.isHidden = !(spec.bit < 0 || (columnMask >> spec.bit) & 1 == 1)
            }
        }

        table.dataSource = context.coordinator
        table.delegate   = context.coordinator

        // Drag/drop registration.
        table.registerForDraggedTypes([.fileURL])
        table.setDraggingSourceOperationMask([.copy], forLocal: true)
        table.setDraggingSourceOperationMask([.copy], forLocal: false)

        // Key + context menu + double-click hooks.
        table.onDeleteKey   = { [weak c = context.coordinator] in c?.handleDelete()   }
        table.onReturnKey   = { [weak c = context.coordinator] in c?.handleDoubleClick() }
        table.onContextMenu = { [weak c = context.coordinator] _ in c?.buildContextMenu() }
        table.target        = context.coordinator
        table.doubleAction  = #selector(Coordinator.handleDoubleClick)

        context.coordinator.table = table

        let scroll = NSScrollView()
        scroll.documentView       = table
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.drawsBackground    = false
        scroll.borderType         = .noBorder
        scroll.autohidesScrollers = true
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let table = scroll.documentView as? SparkampTableView else { return }
        context.coordinator.parent = self
        let oldIds = context.coordinator.tracks.map(\.id)
        let newIds = tracks.map(\.id)
        context.coordinator.tracks = tracks
        if oldIds != newIds {
            table.reloadData()
        } else {
            // Same set of ids — refresh visible cells in case theme or
            // mutable fields (play_count, last_played, scanned) changed.
            let visible = table.rows(in: table.visibleRect)
            for r in visible.location..<(visible.location + visible.length)
                where r < tracks.count {
                for c in 0..<table.numberOfColumns {
                    if let cell = table.view(atColumn: c, row: r, makeIfNecessary: false)
                                  as? SparkampHostingCellView,
                       let spec = Self.specs.first(where: { $0.id == table.tableColumns[c].identifier.rawValue }) {
                        cell.setContent(Self.cellContent(track: tracks[r], spec: spec,
                                                          theme: theme,
                                                          onViewArt: { onEvent(.viewArt($0)) }))
                    }
                }
            }
        }

        // Column visibility from columnMask.
        for col in table.tableColumns {
            if let spec = Self.specs.first(where: { $0.id == col.identifier.rawValue }) {
                let shouldBeHidden = !(spec.bit < 0 || (columnMask >> spec.bit) & 1 == 1)
                if col.isHidden != shouldBeHidden { col.isHidden = shouldBeHidden }
            }
        }
        // Re-pin status column to leftmost position.  NSTableView's
        // `autosaveTableColumns` may restore a user-reordered layout that
        // moved status off the leftmost slot; the column carries the
        // read-only / missing-file / unscanned indicator and only makes
        // sense at the start of the row.
        if let statusIdx = table.tableColumns.firstIndex(where: {
            $0.identifier.rawValue == "col-status"
        }), statusIdx != 0 {
            table.moveColumn(statusIdx, toColumn: 0)
        }

        // Sort descriptors are owned by NSTableView (set by user header
        // clicks).  We deliberately do NOT push the SwiftUI `sortOrder`
        // binding back into the table here: that binding starts with a
        // default value (`title` ASC) and is only updated AFTER the user
        // clicks a header, on a deferred async tick.  Syncing it here
        // would overwrite the user's just-chosen sort back to the stale
        // initial value during the render that fires from
        // `mlTracks` updating in response to the click — sort would
        // appear to do nothing.

        // Selection: binding → table.
        let desired = IndexSet(
            tracks.enumerated()
                .filter { selection.contains($0.element.id) }
                .map(\.offset)
        )
        if table.selectedRowIndexes != desired {
            context.coordinator.applyingExternalSelection = true
            table.selectRowIndexes(desired, byExtendingSelection: false)
            context.coordinator.applyingExternalSelection = false
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    // ── Cell content builder ────────────────────────────────────────────
    fileprivate static func cellContent(track: MLTrack,
                                        spec: ColumnSpec,
                                        theme: SkinTheme,
                                        onViewArt: @escaping (Int64) -> Void) -> AnyView {
        let body: AnyView
        switch spec.id {
        case "col-status":
            body = AnyView(
                Group {
                    if track.fileMissing {
                        Image(systemName: "xmark.circle.fill")
                            .font(.system(size: 9)).foregroundStyle(.red)
                            .help("File not found at recorded path")
                    } else if !track.scanned {
                        Image(systemName: "clock")
                            .font(.system(size: 9))
                            .foregroundStyle(theme.playlistDurationText)
                            .help("Not yet scanned")
                    } else if track.readOnly {
                        Image(systemName: "lock.fill")
                            .font(.system(size: 9))
                            .foregroundStyle(theme.playlistDurationText)
                            .help("Read-only file")
                    } else {
                        Color.clear
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            )
        case "col-title":
            body = AnyView(textCell(track.title.isEmpty ? track.filename : track.title,
                                    color: track.fileMissing  ? .red
                                         : track.scanned      ? theme.playlistText
                                         : theme.playlistDurationText,
                                    spec: spec, theme: theme))
        case "col-artist":
            body = AnyView(textCell(track.artist,
                                    color: track.fileMissing ? .red : theme.playlistText,
                                    spec: spec, theme: theme))
        case "col-album":
            body = AnyView(textCell(track.album,
                                    color: track.fileMissing ? .red : theme.playlistText,
                                    spec: spec, theme: theme))
        case "col-albumartist":
            body = AnyView(textCell(track.albumArtist,
                                    color: track.fileMissing ? .red : theme.playlistText,
                                    spec: spec, theme: theme))
        case "col-genre":
            body = AnyView(textCell(track.genre,
                                    color: track.fileMissing ? .red : theme.playlistText,
                                    spec: spec, theme: theme))
        case "col-composer":
            body = AnyView(textCell(track.composer,
                                    color: track.fileMissing ? .red : theme.playlistText,
                                    spec: spec, theme: theme))
        case "col-year":
            body = AnyView(textCell(track.year > 0 ? "\(track.year)" : "",
                                    color: track.fileMissing ? .red : theme.playlistText,
                                    spec: spec, theme: theme))
        case "col-tracknum":
            body = AnyView(textCell(track.trackNum > 0 ? "\(track.trackNum)" : "",
                                    color: theme.playlistText, spec: spec, theme: theme))
        case "col-discnum":
            body = AnyView(textCell(track.discNum > 0 ? "\(track.discNum)" : "",
                                    color: theme.playlistText, spec: spec, theme: theme))
        case "col-bpm":
            body = AnyView(textCell(track.bpm,
                                    color: theme.playlistText, spec: spec, theme: theme))
        case "col-comment":
            body = AnyView(textCell(track.comment,
                                    color: theme.playlistText, spec: spec, theme: theme))
        case "col-duration":
            let total = Int(track.lengthSecs)
            body = AnyView(textCell(
                total > 0 ? String(format: "%d:%02d", total / 60, total % 60) : "",
                color: theme.playlistDurationText, spec: spec, theme: theme))
        case "col-bitrate":
            body = AnyView(textCell(track.bitrate > 0 ? "\(track.bitrate) kbps" : "",
                                    color: theme.playlistDurationText, spec: spec, theme: theme))
        case "col-filename":
            body = AnyView(textCell(track.filename,
                                    color: theme.playlistDurationText, spec: spec, theme: theme))
        case "col-playcount":
            body = AnyView(textCell(track.playCount > 0 ? "\(track.playCount)" : "",
                                    color: theme.playlistDurationText, spec: spec, theme: theme))
        case "col-lastplayed":
            body = AnyView(textCell(track.lastPlayedDisplay,
                                    color: theme.playlistDurationText, spec: spec, theme: theme))
        case "col-art":
            let tid = track.id
            body = AnyView(
                Group {
                    if track.hasArt {
                        Button("View") { onViewArt(tid) }
                            .buttonStyle(.borderless)
                            .font(theme.vars.bodyFont)
                            .foregroundStyle(theme.playlistCurrentText)
                    } else {
                        Color.clear
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
            )
        default:
            body = AnyView(Color.clear)
        }
        return body
    }

    private static func textCell(_ s: String, color: Color, spec: ColumnSpec, theme: SkinTheme) -> some View {
        Text(s)
            .font(spec.isSmallMono ? theme.vars.smallMonospaceFont : theme.vars.bodyFont)
            .foregroundStyle(color)
            .lineLimit(1)
            .truncationMode(.tail)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
            .padding(.leading, 4)
    }

    // ── Sort-key ↔ KeyPath mapping (only sortable columns appear here) ──
    fileprivate static func keyPathComparator(forSortKey key: String,
                                              ascending: Bool) -> KeyPathComparator<MLTrack>? {
        let order: SortOrder = ascending ? .forward : .reverse
        switch key {
        case "title":        return KeyPathComparator(\MLTrack.title, order: order)
        case "artist":       return KeyPathComparator(\MLTrack.artist, order: order)
        case "album":        return KeyPathComparator(\MLTrack.album, order: order)
        case "album_artist": return KeyPathComparator(\MLTrack.albumArtist, order: order)
        case "genre":        return KeyPathComparator(\MLTrack.genre, order: order)
        case "composer":     return KeyPathComparator(\MLTrack.composer, order: order)
        case "year":         return KeyPathComparator(\MLTrack.year, order: order)
        case "num":          return KeyPathComparator(\MLTrack.trackNum, order: order)
        case "disc_num":     return KeyPathComparator(\MLTrack.discNum, order: order)
        case "bpm":          return KeyPathComparator(\MLTrack.bpm, order: order)
        case "comment":      return KeyPathComparator(\MLTrack.comment, order: order)
        case "duration":     return KeyPathComparator(\MLTrack.lengthSecs, order: order)
        case "bitrate":      return KeyPathComparator(\MLTrack.bitrate, order: order)
        case "play_count":   return KeyPathComparator(\MLTrack.playCount, order: order)
        case "last_played":  return KeyPathComparator(\MLTrack.lastPlayed, order: order)
        default:             return nil
        }
    }

    fileprivate static func sortKey(forKeyPath kp: AnyKeyPath) -> String? {
        switch kp {
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

    @MainActor final class Coordinator: NSObject, NSTableViewDataSource, NSTableViewDelegate {
        var parent: MLFilesTable
        var tracks: [MLTrack] = []
        weak var table: SparkampTableView?
        var applyingExternalSelection = false
        private let cellId = NSUserInterfaceItemIdentifier("mlFileCell")

        init(_ parent: MLFilesTable) {
            self.parent = parent
            self.tracks = parent.tracks
        }

        func numberOfRows(in tableView: NSTableView) -> Int { tracks.count }

        // Block any reorder that would move the status column off the
        // leftmost slot OR move another column INTO the leftmost slot.
        // Status column is the row's read-only/error indicator; it must
        // always sit at the start of the row regardless of autosave.
        func tableView(_ tableView: NSTableView,
                       shouldReorderColumn columnIndex: Int,
                       toColumn newColumnIndex: Int) -> Bool {
            let col = tableView.tableColumns[columnIndex]
            if col.identifier.rawValue == "col-status" { return false }
            if newColumnIndex == 0 { return false }
            return true
        }

        func tableView(_ tableView: NSTableView,
                       viewFor tableColumn: NSTableColumn?,
                       row: Int) -> NSView? {
            guard let column = tableColumn, row < tracks.count,
                  let spec = MLFilesTable.specs.first(where: { $0.id == column.identifier.rawValue })
            else { return nil }
            let cell = (tableView.makeView(withIdentifier: cellId, owner: nil)
                        as? SparkampHostingCellView) ?? SparkampHostingCellView()
            cell.identifier = cellId
            cell.setContent(MLFilesTable.cellContent(
                track: tracks[row],
                spec: spec,
                theme: parent.theme,
                onViewArt: { [weak self] id in self?.parent.onEvent(.viewArt(id)) }
            ))
            return cell
        }

        // Skin-tinted row view for selection paint.  See SparkampSkinRowView
        // in PlaylistView.swift.
        func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
            SparkampSkinRowView()
        }

        func tableViewSelectionDidChange(_ notification: Notification) {
            guard !applyingExternalSelection, let table = self.table else { return }
            let ids: [Int64] = table.selectedRowIndexes.compactMap { idx in
                guard idx < tracks.count else { return nil }
                return tracks[idx].id
            }
            let newSelection = Set(ids)
            if parent.selection != newSelection {
                DispatchQueue.main.async { [weak self] in
                    self?.parent.selection = newSelection
                }
            }
        }

        // Sort: user clicked a header → emit sortChanged carrying the
        // SQL key + direction directly.  The sortOrder binding is also
        // updated for any SwiftUI consumers, but the caller's re-fetch
        // logic should read from the event payload (binding writes are
        // deferred and would be stale by the time reload() runs).
        func tableView(_ tableView: NSTableView,
                       sortDescriptorsDidChange oldDescriptors: [NSSortDescriptor]) {
            guard let first = tableView.sortDescriptors.first,
                  let key = first.key
            else { return }
            let ascending = first.ascending
            DispatchQueue.main.async { [weak self] in
                guard let self = self else { return }
                if let cmp = MLFilesTable.keyPathComparator(forSortKey: key,
                                                            ascending: ascending) {
                    self.parent.sortOrder = [cmp]
                }
                self.parent.onEvent(.sortChanged(key: key, ascending: ascending))
            }
        }

        // Drag source: emit one fileURL per row (multi-row native).
        func tableView(_ tableView: NSTableView,
                       pasteboardWriterForRow row: Int) -> NSPasteboardWriting? {
            guard row < tracks.count else { return nil }
            let path = tracks[row].path
            guard !path.isEmpty else { return nil }
            let pbItem = NSPasteboardItem()
            pbItem.setData(URL(fileURLWithPath: path).dataRepresentation,
                           forType: .fileURL)
            return pbItem
        }

        // Drop destination: only accept drops from OTHER sources (rejects
        // intra-table reorder — Files view is sorted, not user-ordered).
        func tableView(_ tableView: NSTableView,
                       validateDrop info: NSDraggingInfo,
                       proposedRow row: Int,
                       proposedDropOperation dropOperation: NSTableView.DropOperation) -> NSDragOperation {
            // Always normalize to "on table" — Files view isn't insertion-ordered.
            tableView.setDropRow(-1, dropOperation: .on)
            if let src = info.draggingSource as? NSTableView, src === tableView {
                return []
            }
            return .copy
        }

        func tableView(_ tableView: NSTableView,
                       acceptDrop info: NSDraggingInfo,
                       row: Int,
                       dropOperation: NSTableView.DropOperation) -> Bool {
            let urls = info.draggingPasteboard
                .readObjects(forClasses: [NSURL.self], options: nil) as? [URL] ?? []
            let paths = urls.map(\.path).filter { !$0.isEmpty }
            guard !paths.isEmpty else { return false }
            parent.onDropPaths(paths)
            return true
        }

        @objc func handleDoubleClick() {
            guard let table = self.table else { return }
            let r = table.clickedRow >= 0 ? table.clickedRow : (table.selectedRowIndexes.first ?? -1)
            guard r >= 0, r < tracks.count else { return }
            parent.onEvent(.doubleClick([tracks[r].id]))
        }

        func handleDelete() {
            guard let table = self.table else { return }
            let ids: [Int64] = table.selectedRowIndexes.compactMap { idx in
                guard idx < tracks.count else { return nil }
                return tracks[idx].id
            }
            guard !ids.isEmpty else { return }
            parent.onEvent(.removeTracks(ids))
        }

        func buildContextMenu() -> NSMenu? {
            guard let table = self.table else { return nil }
            let clicked = table.clickedRow
            if clicked >= 0 && !table.selectedRowIndexes.contains(clicked) {
                table.selectRowIndexes(IndexSet(integer: clicked), byExtendingSelection: false)
            }
            let ids: [Int64] = table.selectedRowIndexes.compactMap { idx in
                guard idx < tracks.count else { return nil }
                return tracks[idx].id
            }
            let menu = NSMenu()
            menu.autoenablesItems = false
            menu.addItem(BlockMenuItem(title: "Add to Playlist", enabled: !ids.isEmpty) {
                self.parent.onEvent(.addToPlaylist(ids))
            })
            menu.addItem(BlockMenuItem(title: "Replace Current Playlist", enabled: !ids.isEmpty) {
                self.parent.onEvent(.replacePlaylist(ids))
            })
            menu.addItem(.separator())
            menu.addItem(BlockMenuItem(title: "Edit / View ID3 Tags", enabled: ids.count == 1) {
                if let first = ids.first { self.parent.onEvent(.editTags(first)) }
            })
            menu.addItem(BlockMenuItem(title: "View Album Art", enabled: ids.count == 1) {
                if let first = ids.first { self.parent.onEvent(.viewArt(first)) }
            })
            menu.addItem(.separator())
            menu.addItem(BlockMenuItem(title: "Remove from Library", enabled: !ids.isEmpty) {
                self.parent.onEvent(.removeTracks(ids))
            })
            return menu
        }
    }
}
