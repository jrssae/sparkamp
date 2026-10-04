import SwiftUI
import AppKit
import UniformTypeIdentifiers

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
    /// F12.2: `config.media_library.artist_as_album_artist` — passed through
    /// to `MLFilesTable.cellContent` so the "col-albumartist" cell falls
    /// back to the artist when the album-artist tag is blank.
    let artistAsAlbumArtist: Bool
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

    /// Double-click on a row: add it to the active playlist, as a double-click
    /// in the Files view does. Receives the row id.
    var onDoubleClick: ((Int) -> Void)? = nil

    /// True when the editor's current sort allows intra-list drag-reorder
    /// (only sort by play-order ascending preserves the bijection between
    /// display index and play-order index).
    private var reorderAllowed: Bool {
        sortKey == "position" && sortAscending
    }

    /// Status and the # column stay at the start of every row, where the
    /// error indicator and the play-order anchor are easy to find.
    private static let pinned = ["col-status", "col-position"]

    private func isShown(_ spec: MLFilesTable.ColumnSpec) -> Bool {
        MLFilesTable.isPicked(spec, mask: columnMask)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let table = MLFilesTable.makeTable()

        // The SAME columns as MLFilesTable, plus the editor-only # (play
        // position) column. The editor preserves canonical play order in
        // `editingRows`; any sort other than "position" is a transient
        // DISPLAY sort that doesn't mutate the underlying order. Drag-reorder
        // is gated separately to position+ASC so a misclick on another header
        // can never destroy the user's playback sequence. The source column
        // belongs to the Files view only.
        MLFilesTable.addColumns(to: table) { $0.id != "col-src" }
        // Same widths as the Files view, column by column (see
        // `MLFilesTable.sharedWidthsKey`); with nothing shared yet, the same
        // first-run fit to the rows it shows.
        context.coordinator.needsFirstRunWidths =
            MLFilesTable.restoreLayout(of: table, autosaveName: "sparkamp.ml.editorTable")
        MLFilesTable.applyVisibility(to: table, isShown)
        // Default sort: play-order ascending.  Will be re-applied by
        // updateNSView whenever the parent's sort state changes.
        if table.sortDescriptors.isEmpty {
            table.sortDescriptors = [NSSortDescriptor(key: "position", ascending: true)]
        }

        table.dataSource = context.coordinator
        table.delegate   = context.coordinator

        table.registerForDraggedTypes([.fileURL, NSPasteboard.PasteboardType(kSparkampTracklistUTI)])
        // Local drag includes .move so intra-table reorder works when
        // sort = position + ASC (validateDrop gates this); cross-target
        // drops always copy.
        table.setDraggingSourceOperationMask([.copy, .move], forLocal: true)
        table.setDraggingSourceOperationMask([.copy],        forLocal: false)

        table.onDeleteKey   = { [weak c = context.coordinator] in c?.handleDelete() }
        table.onContextMenu = { [weak c = context.coordinator] _ in c?.buildContextMenu() }
        // Double-click adds the row to the active playlist, the same as in
        // the Files view; there is still no Return-key "play this row".
        table.target       = context.coordinator
        table.doubleAction = #selector(Coordinator.handleDoubleClick)

        context.coordinator.table = table
        return table.inScrollView(horizontal: true)
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
            table.refreshVisibleCells(rowCount: rows.count) { r, colId in
                cellContent(row: rows[r], columnId: colId)
            }
        }

        MLFilesTable.applyVisibility(to: table, isShown)
        MLFilesTable.pinColumns(Self.pinned, in: table)

        if context.coordinator.needsFirstRunWidths, !rows.isEmpty {
            context.coordinator.needsFirstRunWidths = false
            MLFilesTable.applyFirstRunWidths(table, tracks: rows.map(\.track), theme: currentTheme,
                                             artistAsAlbumArtist: artistAsAlbumArtist)
            MLFilesTable.storeSharedWidths(from: table)
        }

        // Sort descriptors are owned by NSTableView, same as Files view —
        // pushing the parent's `sortKey` / `sortAscending` back into the
        // table on every update would race with the user-click → async
        // dispatch flow and briefly revert the user's chosen sort.  The
        // sortedRows the parent passes already reflects the current sort,
        // so no programmatic resync is needed at steady state.

        table.show(selection: selection, in: rows, id: \.id)
    }

    /// A cell's content: the # column from the row's place in the canonical
    /// play order (supplied by `positionFor`), the rest as the Files view
    /// draws them. Nil for a column id neither knows.
    fileprivate func cellContent(row: MLEditingRow, columnId: String) -> AnyView? {
        if columnId == "col-position" {
            return Self.positionCellContent(position: positionFor(row.id), theme: currentTheme)
        }
        guard let spec = MLFilesTable.specs.first(where: { $0.id == columnId }) else { return nil }
        return MLFilesTable.cellContent(
            track: row.track, spec: spec, theme: currentTheme,
            artistAsAlbumArtist: artistAsAlbumArtist,
            // No "view art" hook from editor — would need plumbing all the
            // way back to the model; users do this from Files view or via
            // the right-click "View Album Art" menu instead.
            onViewArt: { _ in })
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
        /// True while updateNSView is programmatically updating the
        /// table's `sortDescriptors` — used by `sortDescriptorsDidChange`
        /// to ignore that sync and only react to actual user clicks.
        var applyingExternalSort = false
        /// Nothing shared and no saved layout: size the columns to the
        /// first rows that arrive, as the Files view does.
        var needsFirstRunWidths = false
        private let cellId = NSUserInterfaceItemIdentifier("mlEditorCell")

        init(_ parent: MLEditorTable) {
            self.parent = parent
            self.rows = parent.rows
        }

        func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

        // Status (slot 0) and position (slot 1) are visual anchors users
        // learn to find at the start of every row.
        func tableView(_ tableView: NSTableView,
                       shouldReorderColumn columnIndex: Int,
                       toColumn newColumnIndex: Int) -> Bool {
            MLFilesTable.allowsReorder(tableView, from: columnIndex, to: newColumnIndex,
                                       pinned: MLEditorTable.pinned)
        }

        func tableView(_ tableView: NSTableView,
                       viewFor tableColumn: NSTableColumn?,
                       row: Int) -> NSView? {
            guard let column = tableColumn, row < rows.count,
                  let content = parent.cellContent(row: rows[row],
                                                   columnId: column.identifier.rawValue)
            else { return nil }
            let cell = (tableView.makeView(withIdentifier: cellId, owner: nil)
                        as? SparkampHostingCellView) ?? SparkampHostingCellView()
            cell.identifier = cellId
            cell.setContent(content)
            return cell
        }

        // Skin-tinted row view for selection paint — same wiring as the
        // active-playlist and Files tables.  See SparkampSkinRowView in
        // PlaylistView.swift.
        func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
            SparkampSkinRowView()
        }

        func tableViewSelectionDidChange(_ notification: Notification) {
            guard let table = self.table, !table.isShowingSelection else { return }
            let new = Set(table.selectedIds(in: rows, \.id))
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

        @objc func handleDoubleClick() {
            guard let table = self.table else { return }
            let r = table.clickedRow >= 0 ? table.clickedRow : (table.selectedRowIndexes.first ?? -1)
            guard r >= 0, r < rows.count else { return }
            parent.onDoubleClick?(rows[r].id)
        }

        /// A column changed width: the Files view follows.
        func tableViewColumnDidResize(_ notification: Notification) {
            guard let table = self.table else { return }
            MLFilesTable.storeSharedWidths(from: table)
        }

        // Drag source: one item per row, its path as written — a server
        // song's URI stays a URI.
        func tableView(_ tableView: NSTableView,
                       pasteboardWriterForRow row: Int) -> NSPasteboardWriting? {
            guard row < rows.count else { return nil }
            let path = rows[row].track.path
            guard !path.isEmpty else { return nil }
            return TrackDragPayload.pasteboardItem(forPath: path)
        }

        // What the drag means inside Sparkamp: these entries by path, which
        // keeps a missing entry (no library id) as it is in the playlist.
        // See `SparkampDrag`.
        func tableView(_ tableView: NSTableView,
                       draggingSession session: NSDraggingSession,
                       willBeginAt screenPoint: NSPoint,
                       forRowIndexes rowIndexes: IndexSet) {
            let paths = rowIndexes.filter { $0 < rows.count }.map { rows[$0].track.path }
            SparkampDrag.park(.paths(paths))
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
            let paths = TrackDragPayload.paths(from: info.draggingPasteboard)
            guard !paths.isEmpty else { return false }
            parent.onDropPaths(paths)
            return true
        }

        func handleDelete() {
            guard let table = self.table else { return }
            let idSet = Set(table.selectedIds(in: rows, \.id))
            DispatchQueue.main.async { [weak self] in
                self?.parent.selection.subtract(idSet)
            }
            parent.requestDeleteRows?(idSet)
        }

        // `SparkampTableView.menu(for:)` has already selected the clicked row.
        func buildContextMenu() -> NSMenu? {
            guard let table = self.table else { return nil }
            return parent.contextMenuBuilder(Set(table.selectedIds(in: rows, \.id)))
        }
    }
}

