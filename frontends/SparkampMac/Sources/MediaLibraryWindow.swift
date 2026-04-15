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

    // Column visibility — stored as bitmask in UserDefaults.
    // Columns match the ID3 editor field list plus ML-only fields.
    // Bit layout:
    //  0=Title(TIT2)  1=Artist(TPE1)  2=Album(TALB)   3=AlbumArtist(TPE2)
    //  4=Genre(TCON)  5=Composer(TCOM) 6=Year(TDRC)   7=Track#(TRCK)
    //  8=Disc#(TPOS)  9=BPM(TBPM)    10=Comment(COMM) 11=Duration
    // 12=Bitrate      13=Filename     14=PlayCount
    @AppStorage("sparkamp.ml.columns") private var columnMask: Int = 0b0000000000111   // Title/Artist/Album

    private var theme: SkinTheme { themeManager.currentTheme }

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
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(theme.windowBorder, lineWidth: 1)
                )

                Divider()
                    .background(theme.windowBorder)
                    .frame(height: 16)

                // Column picker (Files tab only)
                if selectedTab == 0 {
                    Menu {
                        columnToggle("Title",        bit: 0)
                        columnToggle("Artist",        bit: 1)
                        columnToggle("Album",         bit: 2)
                        columnToggle("Album Artist",  bit: 3)
                        columnToggle("Genre",         bit: 4)
                        columnToggle("Composer",      bit: 5)
                        columnToggle("Year",          bit: 6)
                        columnToggle("Track #",       bit: 7)
                        columnToggle("Disc #",        bit: 8)
                        columnToggle("BPM",           bit: 9)
                        columnToggle("Comment",       bit: 10)
                        Divider()
                        columnToggle("Duration",      bit: 11)
                        columnToggle("Bitrate",       bit: 12)
                        columnToggle("Filename",      bit: 13)
                        columnToggle("Play Count",    bit: 14)
                    } label: {
                        Image(systemName: "tablecells")
                            .font(.system(size: 11))
                            .foregroundStyle(theme.modeBtnText)
                    }
                    .menuStyle(.borderlessButton)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(theme.background)

            Divider().background(theme.windowBorder)

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

            // ── Tab content ───────────────────────────────────────────────────
            if selectedTab == 0 {
                filesTab
            } else {
                playlistsTab
            }

            Divider().background(theme.windowBorder)

            // ── Bottom bar ────────────────────────────────────────────────────
            HStack {
                Text("\(model.mlTracks.count) tracks")
                    .font(.system(size: 10))
                    .foregroundStyle(theme.playlistDurationText)

                Spacer()

                if !selection.isEmpty {
                    Text("\(selection.count) selected")
                        .font(.system(size: 10))
                        .foregroundStyle(theme.playlistDurationText)

                    Button("Add to Playlist") {
                        model.mlAddToPlaylist(ids: Array(selection))
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(theme.background)
        }
        .background(theme.background)
        .preferredColorScheme(themeManager.preferredColorScheme)
        .onAppear {
            model.openMediaLibrary()
            reload()
        }
        .onChange(of: model.mlScanRunning) { _, running in
            if !running { reload() }
        }
        .onChange(of: selectedTab) { _, _ in selection.removeAll() }
        .onDisappear { model.mediaLibraryVisible = false }
    }

    // MARK: - Files tab

    @ViewBuilder
    private var filesTab: some View {
        MLFilesTable(
            tracks: model.mlTracks,
            selection: $selection,
            sortOrder: $sortOrder,
            columnMask: columnMask,
            theme: theme
        ) { event in
            switch event {
            case .sortChanged:   model.mlTracks.sort(using: sortOrder)
            case .addToPlaylist(let ids):     model.mlAddToPlaylist(ids: ids)
            case .replacePlaylist(let ids):   model.mlReplacePlaylistWith(ids: ids)
            case .editTags(let id):
                if let track = model.mlTracks.first(where: { $0.id == id }) {
                    model.mlOpenTagEditorForPath(track.path)
                }
            case .removeTracks(let ids):      model.mlRemoveTracks(ids: ids)
            }
        }
    }

    // MARK: - Playlists tab

    @ViewBuilder
    private var playlistsTab: some View {
        if model.mlSavedPlaylists.isEmpty {
            VStack {
                Spacer()
                Text("No saved playlists found.")
                    .foregroundStyle(theme.playlistDurationText)
                Text("Add folders containing M3U playlists in Settings → Media Library.")
                    .font(.caption)
                    .foregroundStyle(theme.playlistDurationText)
                Spacer()
            }
            .background(theme.playlistBg)
        } else {
            PlaylistsListView(playlists: model.mlSavedPlaylists, theme: theme) { idx in
                model.mlSetCurrentPlaylist(idx)
            }
        }
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
        let colName = sortOrder.first.map { kp -> String? in
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
            default: return nil
            }
        } ?? nil

        let desc = sortOrder.first.map { $0.order == .reverse } ?? false
        model.mlFetchTracks(query: q, sortCol: colName, sortDesc: desc)
    }
}

// MARK: - ML table event

enum MLTableEvent {
    case sortChanged
    case addToPlaylist([Int64])
    case replacePlaylist([Int64])
    case editTags(Int64)
    case removeTracks([Int64])
}

// MARK: - ML files table

struct MLFilesTable: View {
    let tracks: [MLTrack]
    @Binding var selection: Set<Int64>
    @Binding var sortOrder: [KeyPathComparator<MLTrack>]
    let columnMask: Int
    let theme: SkinTheme
    let onEvent: (MLTableEvent) -> Void

    private func isVisible(_ bit: Int) -> Bool { (columnMask >> bit) & 1 == 1 }

    var body: some View {
        Table(tracks, selection: $selection, sortOrder: $sortOrder) {
            columnsA()
            columnsB()
        }
        .onChange(of: sortOrder) { _, _ in onEvent(.sortChanged) }
        .contextMenu(forSelectionType: Int64.self) { ids in
            Button("Add to Playlist")          { onEvent(.addToPlaylist(Array(ids))) }
            Button("Replace Current Playlist") { onEvent(.replacePlaylist(Array(ids))) }
            Divider()
            Button("Edit / View ID3 Tags") {
                if let first = ids.first { onEvent(.editTags(first)) }
            }
            .disabled(ids.count != 1)
            Divider()
            Button("Remove from Library", role: .destructive) {
                onEvent(.removeTracks(Array(ids)))
            }
        }
        .background(theme.playlistBg)
        .scrollContentBackground(.hidden)
        .foregroundStyle(theme.playlistText)
    }

    // ── Split into two builders so the type-checker doesn't time out ─────────

    @TableColumnBuilder<MLTrack, KeyPathComparator<MLTrack>>
    private func columnsA() -> some TableColumnContent<MLTrack, KeyPathComparator<MLTrack>> {
        if isVisible(0) {
            TableColumn("Title", value: \.title) { row in
                Text(row.title.isEmpty ? row.filename : row.title)
                    .foregroundStyle(row.scanned ? theme.playlistText : theme.playlistDurationText)
            }
        }
        if isVisible(1) {
            TableColumn("Artist", value: \.artist) { row in
                Text(row.artist).foregroundStyle(theme.playlistText)
            }
        }
        if isVisible(2) {
            TableColumn("Album", value: \.album) { row in
                Text(row.album).foregroundStyle(theme.playlistText)
            }
        }
        if isVisible(3) {
            TableColumn("Album Artist", value: \.albumArtist) { row in
                Text(row.albumArtist).foregroundStyle(theme.playlistText)
            }
        }
        if isVisible(4) {
            TableColumn("Genre", value: \.genre) { row in
                Text(row.genre).foregroundStyle(theme.playlistText)
            }
        }
        if isVisible(5) {
            TableColumn("Composer", value: \.composer) { row in
                Text(row.composer).foregroundStyle(theme.playlistText)
            }
        }
        if isVisible(6) {
            TableColumn("Year", value: \.year) { row in
                Text(row.year > 0 ? "\(row.year)" : "").foregroundStyle(theme.playlistText)
            }
        }
    }

    @TableColumnBuilder<MLTrack, KeyPathComparator<MLTrack>>
    private func columnsB() -> some TableColumnContent<MLTrack, KeyPathComparator<MLTrack>> {
        if isVisible(7) {
            TableColumn("Track #", value: \.trackNum) { row in
                Text(row.trackNum > 0 ? "\(row.trackNum)" : "")
                    .foregroundStyle(theme.playlistText)
            }
        }
        if isVisible(8) {
            TableColumn("Disc #", value: \.discNum) { row in
                Text(row.discNum > 0 ? "\(row.discNum)" : "")
                    .foregroundStyle(theme.playlistText)
            }
        }
        if isVisible(9) {
            TableColumn("BPM", value: \.bpm) { row in
                Text(row.bpm).foregroundStyle(theme.playlistText)
            }
        }
        if isVisible(10) {
            TableColumn("Comment", value: \.comment) { row in
                Text(row.comment).foregroundStyle(theme.playlistText)
            }
        }
        if isVisible(11) {
            TableColumn("Duration", value: \.lengthSecs) { row in
                let total = Int(row.lengthSecs)
                let m = total / 60, s = total % 60
                Text(total > 0 ? String(format: "%d:%02d", m, s) : "")
                    .foregroundStyle(theme.playlistDurationText)
            }
        }
        if isVisible(12) {
            TableColumn("Bitrate", value: \.bitrate) { row in
                Text(row.bitrate > 0 ? "\(row.bitrate) kbps" : "")
                    .foregroundStyle(theme.playlistDurationText)
            }
        }
        if isVisible(13) {
            TableColumn("Filename", value: \.filename) { row in
                Text(row.filename).foregroundStyle(theme.playlistDurationText)
            }
        }
        if isVisible(14) {
            TableColumn("Play Count", value: \.playCount) { row in
                Text(row.playCount > 0 ? "\(row.playCount)" : "")
                    .foregroundStyle(theme.playlistDurationText)
            }
        }
    }
}

// MARK: - Playlists list subview

private struct PlaylistsListView: View {
    let playlists: [MLPlaylistItem]
    let theme: SkinTheme
    let onLoad: (Int) -> Void

    var body: some View {
        List {
            ForEach(playlists, id: \.id) { (pl: MLPlaylistItem) in
                HStack {
                    Image(systemName: "music.note.list")
                        .foregroundStyle(theme.playlistDurationText)
                    Text(pl.name)
                        .foregroundStyle(theme.playlistText)
                    Spacer()
                    Button("Load") { onLoad(pl.id) }
                        .buttonStyle(.borderless)
                        .foregroundStyle(theme.titleText)
                        .font(.system(size: 11))
                }
                .padding(.vertical, 2)
                .listRowBackground(theme.playlistBg)
            }
        }
        .background(theme.playlistBg)
        .scrollContentBackground(.hidden)
    }
}
