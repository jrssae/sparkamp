import SwiftUI
import AppKit
import UniformTypeIdentifiers

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
    /// Used to build the shared "Send to" submenu.
    @ObservedObject var model: SparkampModel
    let onEvent: (MLTableEvent) -> Void
    let onDropPaths: MLFilesDropHandler

    private func isVisible(_ bit: Int) -> Bool { (columnMask >> bit) & 1 == 1 }

    // ── Column descriptors ──────────────────────────────────────────────
    // Static list drives NSTableColumn construction.  Order here = default
    // column order (NSTableView autosave persists user reorders after that).
    struct ColumnSpec {
        let id: String          // customization id, e.g. "col-title"; used as NSUserInterfaceItemIdentifier
        let title: String
        let bit: Int            // columnMask bit driving show/hide; -1 = always visible
        let width: CGFloat
        let sortKey: String?    // SQL column name for SortDescriptor; nil = not sortable
        let isSmallMono: Bool   // render with smallMonospaceFont + durationText colour
        var editorOnly: Bool = false  // skip in Files table; show only in playlist editor
    }

    static let specs: [ColumnSpec] = [
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
    static func cellContent(track: MLTrack,
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
    static func keyPathComparator(forSortKey key: String,
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
            // Shared "Send to" submenu (Active Playlist / Saved Playlist ▸ /
            // Disc Drive / Removable Device) over the selected rows' paths.
            let idSet = Set(ids)
            let paths = tracks.filter { idSet.contains($0.id) }.map { $0.path }
            menu.addItem(parent.model.sendToMenuItem(paths: paths, includeActive: true))
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
