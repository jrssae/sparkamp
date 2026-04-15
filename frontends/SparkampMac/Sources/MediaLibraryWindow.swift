import SwiftUI
import AppKit

// MARK: - Media Library Window

struct MediaLibraryView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager

    // Tab selection
    @State private var selectedTab = 0   // 0 = Files, 1 = Playlists

    // Search
    @State private var searchQuery = ""
    @State private var searchDebounce: DispatchWorkItem? = nil

    // Table sort & selection (Files tab)
    @State private var sortOrder: [KeyPathComparator<MLTrack>] = [KeyPathComparator(\.title)]
    @State private var selection: Set<Int64> = []

    // Column visibility — stored as bitmask in UserDefaults
    // Bit 0=Title 1=Artist 2=Album 3=Duration 4=Track# 5=Year 6=Genre 7=Bitrate 8=Filename 9=PlayCount
    @AppStorage("sparkamp.ml.columns") private var columnMask: Int = 0b0000000111   // Title/Artist/Album visible by default

    // Manage Folders sheet
    @State private var showManageFolders = false

    var body: some View {
        VStack(spacing: 0) {
            // ── Toolbar ──────────────────────────────────────────────────────
            HStack(spacing: 8) {
                Picker("", selection: $selectedTab) {
                    Text("Files").tag(0)
                    Text("Playlists").tag(1)
                }
                .pickerStyle(.segmented)
                .frame(width: 160)

                Spacer()

                // Search field
                HStack(spacing: 4) {
                    Image(systemName: "magnifyingglass")
                        .foregroundStyle(.secondary)
                        .font(.system(size: 11))
                    TextField("Search…", text: $searchQuery)
                        .textFieldStyle(.plain)
                        .font(.system(size: 12))
                        .frame(width: 180)
                        .onChange(of: searchQuery) { _, _ in debounceSearch() }
                    if !searchQuery.isEmpty {
                        Button { searchQuery = ""; reload() } label: {
                            Image(systemName: "xmark.circle.fill")
                                .foregroundStyle(.secondary)
                                .font(.system(size: 11))
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(4)
                .background(Color(.textBackgroundColor).opacity(0.6))
                .cornerRadius(6)

                Divider().frame(height: 16)

                Button { model.mlOpenAddFolderPicker() }
                    label: { Label("Add Folder", systemImage: "folder.badge.plus") }
                    .buttonStyle(.borderless)
                    .font(.system(size: 11))

                Button { model.mlRescanAll() }
                    label: { Label("Rescan", systemImage: "arrow.clockwise") }
                    .buttonStyle(.borderless)
                    .font(.system(size: 11))

                Button { showManageFolders = true }
                    label: { Label("Folders…", systemImage: "folder") }
                    .buttonStyle(.borderless)
                    .font(.system(size: 11))

                // Column picker
                if selectedTab == 0 {
                    Menu {
                        columnToggle("Title",      bit: 0)
                        columnToggle("Artist",     bit: 1)
                        columnToggle("Album",      bit: 2)
                        columnToggle("Duration",   bit: 3)
                        columnToggle("Track #",    bit: 4)
                        columnToggle("Year",       bit: 5)
                        columnToggle("Genre",      bit: 6)
                        columnToggle("Bitrate",    bit: 7)
                        columnToggle("Filename",   bit: 8)
                        columnToggle("Play Count", bit: 9)
                    } label: {
                        Image(systemName: "tablecells")
                            .font(.system(size: 11))
                    }
                    .menuStyle(.borderlessButton)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(Color(.windowBackgroundColor))

            Divider()

            // ── Scan progress bar ─────────────────────────────────────────────
            if model.mlScanRunning {
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
                        .foregroundStyle(.secondary)

                    Button("Cancel") { model.mlCancelScan() }
                        .buttonStyle(.borderless)
                        .font(.system(size: 10))
                        .foregroundStyle(.red)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 4)
                .background(Color(.windowBackgroundColor))

                Divider()
            }

            // ── Tab content ───────────────────────────────────────────────────
            if selectedTab == 0 {
                filesTab
            } else {
                playlistsTab
            }

            Divider()

            // ── Bottom bar ────────────────────────────────────────────────────
            HStack {
                Text("\(model.mlTracks.count) tracks")
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)

                Spacer()

                if !selection.isEmpty {
                    Text("\(selection.count) selected")
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)

                    Button("Add to Playlist") {
                        model.mlAddToPlaylist(ids: Array(selection))
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(Color(.windowBackgroundColor))
        }
        .onAppear {
            model.openMediaLibrary()
            reload()
        }
        .onChange(of: model.mlScanRunning) { _, running in
            if !running { reload() }
        }
        .onChange(of: selectedTab) { _, _ in selection.removeAll() }
        .onDisappear { model.mediaLibraryVisible = false }
        .sheet(isPresented: $showManageFolders) { manageFoldersSheet }
    }

    // MARK: - Files tab

    @ViewBuilder
    private var filesTab: some View {
        let tracks = filteredAndSorted

        Table(tracks, selection: $selection, sortOrder: $sortOrder) {
            if isVisible(0) {
                TableColumn("Title", value: \.title) { t in
                    Text(t.title.isEmpty ? t.filename : t.title)
                        .lineLimit(1)
                        .foregroundStyle(t.scanned ? .primary : .secondary)
                }
            }
            if isVisible(1) {
                TableColumn("Artist", value: \.artist) { t in
                    Text(t.artist).lineLimit(1)
                }
            }
            if isVisible(2) {
                TableColumn("Album", value: \.album) { t in
                    Text(t.album).lineLimit(1)
                }
            }
            if isVisible(3) {
                TableColumn("Duration", value: \.lengthSecs) { t in
                    Text(t.durationString)
                        .monospacedDigit()
                        .lineLimit(1)
                }
                .width(60)
            }
            if isVisible(4) {
                TableColumn("#", value: \.trackNum) { t in
                    Text(t.trackNum > 0 ? "\(t.trackNum)" : "")
                        .monospacedDigit()
                        .lineLimit(1)
                }
                .width(40)
            }
            if isVisible(5) {
                TableColumn("Year", value: \.year) { t in
                    Text(t.year > 0 ? "\(t.year)" : "")
                        .lineLimit(1)
                }
                .width(50)
            }
            if isVisible(6) {
                TableColumn("Genre", value: \.genre) { t in
                    Text(t.genre).lineLimit(1)
                }
            }
            if isVisible(7) {
                TableColumn("Bitrate", value: \.bitrate) { t in
                    Text(t.bitrate > 0 ? "\(t.bitrate)" : "")
                        .monospacedDigit()
                        .lineLimit(1)
                }
                .width(60)
            }
            if isVisible(8) {
                TableColumn("Filename", value: \.filename) { t in
                    Text(t.filename).lineLimit(1)
                }
            }
            if isVisible(9) {
                TableColumn("Plays", value: \.playCount) { t in
                    Text("\(t.playCount)")
                        .monospacedDigit()
                        .lineLimit(1)
                }
                .width(50)
            }
        }
        .onChange(of: sortOrder) { _, _ in
            model.mlTracks.sort(using: sortOrder)
        }
    }

    // MARK: - Playlists tab

    @ViewBuilder
    private var playlistsTab: some View {
        if model.mlSavedPlaylists.isEmpty {
            VStack {
                Spacer()
                Text("No saved playlists found.")
                    .foregroundStyle(.secondary)
                Text("Add folders containing M3U playlists.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
            }
        } else {
            PlaylistsListView(playlists: model.mlSavedPlaylists) { idx in
                model.mlSetCurrentPlaylist(idx)
            }
        }
    }

    // MARK: - Manage Folders sheet

    @ViewBuilder
    private var manageFoldersSheet: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Watched Folders")
                .font(.headline)
                .padding()

            Divider()

            if model.mlFolders.isEmpty {
                HStack {
                    Spacer()
                    Text("No folders added yet.")
                        .foregroundStyle(.secondary)
                        .padding()
                    Spacer()
                }
            } else {
                List {
                    ForEach(model.mlFolders, id: \.self) { folder in
                        HStack {
                            Image(systemName: "folder")
                                .foregroundStyle(.secondary)
                            Text(folder)
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Spacer()
                            Button {
                                model.mlRemoveFolder(folder)
                            } label: {
                                Image(systemName: "minus.circle")
                                    .foregroundStyle(.red)
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
            }

            Divider()

            HStack {
                Button { model.mlOpenAddFolderPicker() } label: {
                    Label("Add Folder…", systemImage: "plus")
                }
                .buttonStyle(.borderless)

                Spacer()

                Button("Done") { showManageFolders = false }
                    .keyboardShortcut(.defaultAction)
            }
            .padding()
        }
        .frame(width: 480, height: 320)
    }

    // MARK: - Helpers

    private func isVisible(_ bit: Int) -> Bool {
        (columnMask >> bit) & 1 == 1
    }

    @ViewBuilder
    private func columnToggle(_ label: String, bit: Int) -> some View {
        Toggle(label, isOn: Binding(
            get: { isVisible(bit) },
            set: { on in
                if on { columnMask |= (1 << bit) }
                else  { columnMask &= ~(1 << bit) }
            }
        ))
    }

    private var filteredAndSorted: [MLTrack] {
        model.mlTracks
    }

    private func debounceSearch() {
        searchDebounce?.cancel()
        let task = DispatchWorkItem { [q = searchQuery] in reload(query: q) }
        searchDebounce = task
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3, execute: task)
    }

    private func reload(query: String? = nil) {
        let q = query ?? searchQuery
        let colName = sortOrder.first.map { kp -> String? in
            switch kp.keyPath {
            case \MLTrack.title:     return "title"
            case \MLTrack.artist:    return "artist"
            case \MLTrack.album:     return "album"
            case \MLTrack.lengthSecs: return "duration"
            case \MLTrack.trackNum:  return "num"
            case \MLTrack.year:      return "year"
            case \MLTrack.genre:     return "genre"
            case \MLTrack.bitrate:   return "bitrate"
            case \MLTrack.playCount: return "num"
            default: return nil
            }
        } ?? nil

        let desc = sortOrder.first.map { $0.order == .reverse } ?? false
        model.mlFetchTracks(query: q, sortCol: colName, sortDesc: desc)
    }
}

// MARK: - Playlists list subview

private struct PlaylistsListView: View {
    let playlists: [MLPlaylistItem]
    let onLoad: (Int) -> Void

    var body: some View {
        List {
            ForEach(playlists, id: \.id) { (pl: MLPlaylistItem) in
                HStack {
                    Image(systemName: "music.note.list")
                        .foregroundStyle(.secondary)
                    Text(pl.name)
                    Spacer()
                    Button("Load") { onLoad(pl.id) }
                        .buttonStyle(.borderless)
                        .foregroundStyle(Color.accentColor)
                        .font(.system(size: 11))
                }
                .padding(.vertical, 2)
            }
        }
    }
}
