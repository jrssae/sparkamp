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

