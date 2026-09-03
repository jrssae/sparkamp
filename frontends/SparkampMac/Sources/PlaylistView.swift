import SwiftUI
import AppKit
import UniformTypeIdentifiers

// MARK: - Track-list drag payload
//
// SwiftUI's `.onDrag` returns a single NSItemProvider per row, which cannot
// natively represent "the whole multi-row selection".  To support multi-row
// drag without dropping into NSViewRepresentable for every list, every drag
// source registers TWO representations on the provider:
//
//   - `kSparkampTracklistUTI` — newline-joined absolute paths of every row
//     in the active selection (or just the dragged row if it isn't part of
//     the selection).  Sparkamp-internal drop targets consume this first.
//   - `public.file-url` — the first path only, as a regular file URL.  This
//     preserves Finder / generic-target compatibility for single-file drops.
//
// All Sparkamp drop targets prefer the tracklist UTI when present so the
// full selection lands at the destination; they fall back to file URL for
// drags originating outside Sparkamp.

let kSparkampTracklistUTI = "dev.sparkamp.tracklist"

enum TrackDragPayload {
    /// Build an NSItemProvider that carries `paths` as a Sparkamp tracklist
    /// and the first path as a `file-url` for external compatibility.
    /// Empty `paths` returns an inert provider so the drag still starts
    /// (avoids the system aborting the gesture) but nothing transfers.
    static func provider(forPaths paths: [String]) -> NSItemProvider {
        let p = NSItemProvider()
        // The tracklist type is registered even for an empty path list. It is
        // what makes the drag droppable at all — a destination only admits
        // types it registered for — and a deferred payload (a device card)
        // has no paths to offer at gesture time. The drop reads the real
        // payload from `SparkampDrag`.
        let payload = paths.joined(separator: "\n").data(using: .utf8) ?? Data()
        p.registerDataRepresentation(forTypeIdentifier: kSparkampTracklistUTI,
                                     visibility: .all) { completion in
            completion(payload, nil)
            return nil
        }
        if let first = paths.first {
            let urlData = URL(fileURLWithPath: first).dataRepresentation
            p.registerDataRepresentation(forTypeIdentifier: UTType.fileURL.identifier,
                                         visibility: .all) { completion in
                completion(urlData, nil)
                return nil
            }
        }
        return p
    }

    /// Resolve a set of NSItemProviders into absolute paths, preferring the
    /// Sparkamp tracklist representation (multi-row) and falling back to
    /// file URLs.  Calls `completion` on the main queue once every provider
    /// has been resolved.
    static func resolvePaths(from providers: [NSItemProvider],
                             completion: @escaping ([String]) -> Void) {
        let group = DispatchGroup()
        let lock = NSLock()
        var paths: [String] = []
        for p in providers {
            if p.hasItemConformingToTypeIdentifier(kSparkampTracklistUTI) {
                group.enter()
                p.loadDataRepresentation(forTypeIdentifier: kSparkampTracklistUTI) { data, _ in
                    if let data = data, let str = String(data: data, encoding: .utf8) {
                        let parts = str.split(separator: "\n").map(String.init)
                        lock.lock(); paths.append(contentsOf: parts); lock.unlock()
                    }
                    group.leave()
                }
            } else {
                group.enter()
                p.loadItem(forTypeIdentifier: UTType.fileURL.identifier) { item, _ in
                    if let data = item as? Data,
                       let url = URL(dataRepresentation: data, relativeTo: nil) {
                        lock.lock(); paths.append(url.path); lock.unlock()
                    }
                    group.leave()
                }
            }
        }
        group.notify(queue: .main) { completion(paths) }
    }
}

// MARK: - In-process drag payload
//
// The pasteboard payload above is a lossy projection of a drag: paths, and
// nothing else. That is everything Finder or another app can use, and it is
// all a drop from outside will ever have. Inside Sparkamp it is not enough.
//
// Two problems it caused, both reported from testing:
//
//   - Dragging audio-CD tracks onto the playlist made the music skip, while
//     double-clicking the very same rows did not. The buttons call
//     `addDiscTracks`, which supplies each track's title and duration from
//     the TOC. A path-only drop went through the ordinary file add, which
//     asks for a tag scan and a duration probe — reads that seek the drive
//     out from under playback. (The core now refuses those reads for optical
//     paths too; this is the other half, and the half that also gets the
//     tags right.)
//   - A device overview card stands for every audio file on the volume, and
//     finding them means walking it. Deferring that into a lazily-resolved
//     pasteboard representation meant the drop asked for data the promise
//     answered asynchronously, and the drop had already given up: the card
//     dragged, and landed as nothing at all.
//
// So a Sparkamp drag also parks what it really means here, and the drop uses
// that instead — reaching the same model call the buttons next to those rows
// use. `draggingSource` is non-nil only for a drag that began in this
// process, and the pasteboard still has to carry the tracklist type, so a
// Finder drop can never pick up a leftover payload.
enum SparkampDrag {
    indirect enum Payload {
        /// Ordinary files: device files, disc data files, anything from
        /// outside Sparkamp.
        case paths([String])
        /// Library track ids — an album tile, a saved playlist. The drop adds
        /// them from the library's own records, so nothing is read off disk
        /// to learn what the library already knows.
        case libraryIds([Int64])
        /// An audio CD's tracks and the drive they came from, so the drop can
        /// call `addDiscTracks` — tags from the TOC, and no disc access.
        case discTracks(drive: OpticalDrive, entries: [DiscTrackEntry])
        /// A payload that is expensive to work out: walking a device volume,
        /// or reading a saved playlist's rows. Resolved on a background queue
        /// at drop time, never when the gesture starts.
        case deferred(() -> Payload)
    }

    /// Set on the main thread when a drag starts, read on the main thread
    /// when it lands. Only one drag runs at a time, so a single slot is
    /// enough; a cancelled drag's leftovers are simply overwritten by the
    /// next one, and cannot be consumed in between because every drop first
    /// checks that the drag came from this process.
    private static var pending: Payload?

    /// Park `payload` for the drag about to begin, and return the provider
    /// the gesture needs. `pasteboardPaths` is what a drop *outside* Sparkamp
    /// gets — pass the paths when they are known and cheap, and an empty
    /// array when they are not.
    ///
    /// `plainText` rides along for the drop targets that read a drag as a
    /// string rather than as tracks: a saved playlist dropped on a device row
    /// means "sync this whole playlist", which is a different act from
    /// copying its files.
    static func begin(_ payload: Payload,
                      pasteboardPaths: [String] = [],
                      plainText: String? = nil) -> NSItemProvider {
        pending = payload
        let p = TrackDragPayload.provider(forPaths: pasteboardPaths)
        if let plainText = plainText {
            let data = Data(plainText.utf8)
            p.registerDataRepresentation(forTypeIdentifier: UTType.utf8PlainText.identifier,
                                         visibility: .all) { completion in
                completion(data, nil)
                return nil
            }
        }
        return p
    }

    /// The payload of a drag that started in this process, consumed.
    static func take() -> Payload? {
        defer { pending = nil }
        return pending
    }

    /// The rows a drag of `row` should carry: the whole selection when the
    /// dragged row is part of it, otherwise just that row.
    ///
    /// Every table hands `.itemProvider` one row at a time, so a multi-row
    /// drag calls `begin` once per row. Each call has to park the same full
    /// set or the last one would win and the drag would shrink to one track.
    static func rows<T: Identifiable>(_ row: T, in items: [T],
                                      selection: Set<T.ID>) -> [T] {
        guard selection.contains(row.id) else { return [row] }
        return items.filter { selection.contains($0.id) }
    }
}

// MARK: - Shared Save-As panel for playlist files
//
// Single helper used by every "create / save playlist as a new file"
// flow: active-playlist Save button, active-playlist right-click "New
// Playlist…", and the ML window's sidebar "New Playlist" button.
// Centralising means there's exactly one place that decides the default
// directory + filename, and the user gets the native Save panel in all
// three cases (instead of a text-only inline prompt that defaulted to
// Sparkamp's managed playlists directory).

/// Run a Save-As NSSavePanel for an M3U/M3U8 playlist file.  Default
/// directory comes from `model.mlDefaultSaveAsDir()` (first watched ML
/// folder, falling back to `~/Music`).  On OK, calls `onAccept` on the
/// main actor with the chosen filename stem and parent directory.
///
/// `defaultName` is pre-filled in the panel's filename field with a
/// `.m3u8` extension appended.  The user can edit it freely.
@MainActor
func runPlaylistSavePanel(model: SparkampModel,
                          defaultName: String,
                          onAccept: @escaping (_ stem: String, _ directory: URL) -> Void) {
    let panel = NSSavePanel()
    panel.title = "Save Playlist As…"
    panel.allowedContentTypes = [
        UTType(filenameExtension: "m3u8")!,
        UTType(filenameExtension: "m3u")!,
    ]
    panel.canCreateDirectories = true
    panel.isExtensionHidden    = false
    panel.nameFieldStringValue = "\(defaultName).m3u8"
    panel.directoryURL         = model.mlDefaultSaveAsDir()
    panel.begin { resp in
        guard resp == .OK, let url = panel.url else { return }
        Task { @MainActor in
            let stem = url.deletingPathExtension().lastPathComponent
            let dir  = url.deletingLastPathComponent()
            onAccept(stem, dir)
        }
    }
}

/// Default suggested name for a "save current state" playlist:
/// `Playlist YYYY-MM-DD HH-mm` — readable, sortable, no colons (safe
/// across all filesystems).
func defaultTimestampedPlaylistName() -> String {
    let f = DateFormatter()
    f.dateFormat = "yyyy-MM-dd HH-mm"
    return "Playlist \(f.string(from: Date()))"
}

// MARK: - Active-playlist NSTableView wrapper

/// AppKit-backed replacement for the SwiftUI `List` previously used to
/// render the active playlist.  Switching to NSTableView is the only way
/// to get Finder-style click-vs-drag arbitration: SwiftUI's `.onDrag`
/// adds a press-and-hold delay before single-click selection registers
/// — intolerable for a track list.  NSTableView uses a mouse-movement
/// threshold instead, so clicks fire instantly and drags only begin
/// after the user moves the cursor a few pixels.
///
/// Multi-row drag is free: NSTableView emits one `NSPasteboardWriter`
/// per selected row and the drop side reads all of them via
/// `pasteboardItems`.  No custom UTI needed.
///
/// Skin-tinted full-row selection is provided by the global swizzle in
/// `SparkampMacApp.swift::SparkampSelectionPalette` — every NSTableView
/// in the app picks it up automatically.
struct ActivePlaylistTable: NSViewRepresentable {
    @ObservedObject var model: SparkampModel
    @ObservedObject var themeManager: ThemeManager
    @Binding var selection: Set<Int>
    /// Builds an NSMenu for the current selection (right-click handler).
    /// Returning nil suppresses the menu.
    let contextMenuBuilder: (Set<Int>) -> NSMenu?

    func makeNSView(context: Context) -> NSScrollView {
        let table = SparkampTableView()
        table.headerView = nil
        table.allowsMultipleSelection = true
        table.usesAlternatingRowBackgroundColors = false
        table.backgroundColor = .clear
        table.style = .plain
        table.gridStyleMask = []
        table.intercellSpacing = NSSize(width: 0, height: 2)
        table.rowHeight = 20
        table.selectionHighlightStyle = .regular   // lets the swizzled drawSelection fire
        table.focusRingType = .none

        let col = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("row"))
        col.resizingMask = .autoresizingMask
        table.addTableColumn(col)

        table.dataSource = context.coordinator
        table.delegate   = context.coordinator

        // Drag/drop registration: accept any file URL (Sparkamp inter-list
        // drags as well as external Finder drops use this UTI).
        // Both payloads: a plain file URL from Finder or another app, and the
        // Sparkamp tracklist a Media Library / disc / device drag carries. A
        // device overview card publishes ONLY the tracklist — its paths are
        // not known until the drop asks — so without this it would be
        // silently undroppable.
        table.registerForDraggedTypes([
            .fileURL,
            NSPasteboard.PasteboardType(kSparkampTracklistUTI),
        ])
        table.setDraggingSourceOperationMask([.copy, .move], forLocal: true)
        table.setDraggingSourceOperationMask([.copy],        forLocal: false)

        table.onDeleteKey   = { [weak c = context.coordinator] in c?.handleDelete()   }
        table.onReturnKey   = { [weak c = context.coordinator] in c?.handleReturn()   }
        table.onQueueKey    = { [weak c = context.coordinator] in c?.handleQueueKey() }
        table.onContextMenu = { [weak c = context.coordinator] _ in c?.buildContextMenu() }

        table.target       = context.coordinator
        table.doubleAction = #selector(Coordinator.handleDoubleClick)

        context.coordinator.table = table

        let scroll = NSScrollView()
        scroll.documentView      = table
        scroll.hasVerticalScroller = true
        scroll.drawsBackground   = false
        scroll.borderType        = .noBorder
        scroll.autohidesScrollers = true
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let table = scroll.documentView as? SparkampTableView else { return }
        context.coordinator.parent = self
        let newItems = model.playlistItems
        // Full-content comparison, not just row ids: a tag edit (e.g. from
        // the Media Library's ID3 editor) changes titles without changing
        // ids, and must trigger a real reload — the visible-cell repaint
        // below is only for the cheap marker-moved case. With nothing
        // playing there are no follow-up publishes to mask a missed reload.
        let itemsChanged = newItems != context.coordinator.items
        context.coordinator.items = newItems
        if itemsChanged {
            table.reloadData()
        } else {
            // Same items, but content (current-index marker, etc.) may have
            // changed — refresh visible cells without rebuilding the table.
            let visible = table.rows(in: table.visibleRect)
            for r in visible.location..<(visible.location + visible.length) {
                if let cell = table.view(atColumn: 0, row: r, makeIfNecessary: false) as? SparkampHostingCellView,
                   r < newItems.count {
                    cell.setContent(Self.makeRowView(item: newItems[r],
                                                    isCurrent: newItems[r].id == model.currentIndex,
                                                    themeManager: themeManager))
                }
            }
        }

        // Auto-scroll to the current track on track change (D8) — mirrors
        // the GTK frontend's scroll_to_row_if_needed. Only scrolls when the
        // playing track actually changes (and its row is resolvable), so
        // user scrolling isn't fought while the same track keeps playing.
        let cur = model.currentIndex
        if cur >= 0, cur != context.coordinator.lastScrolledIndex,
           let row = newItems.firstIndex(where: { $0.id == cur }), row < table.numberOfRows {
            table.scrollRowToVisible(row)
            context.coordinator.lastScrolledIndex = cur
        } else if cur < 0 {
            // Playback stopped — reset the guard so replaying the same
            // track (same id) still triggers a scroll instead of being
            // silently skipped as "already scrolled there".
            context.coordinator.lastScrolledIndex = -1
        }

        // Sync selection from binding → table without echoing back through
        // tableViewSelectionDidChange (avoids feedback loops).
        let desired = IndexSet(
            newItems.enumerated()
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

    /// Build the SwiftUI row view used inside each cell.  Pulled out so
    /// `updateNSView`'s refresh path and the data-source's `viewFor`
    /// stay in sync.
    fileprivate static func makeRowView(item: PlaylistItem,
                                        isCurrent: Bool,
                                        themeManager: ThemeManager) -> AnyView {
        AnyView(
            PlaylistRow(item: item, isCurrent: isCurrent)
                .environmentObject(themeManager)
                .padding(.horizontal, 8)
        )
    }

    @MainActor final class Coordinator: NSObject, NSTableViewDataSource, NSTableViewDelegate {
        var parent: ActivePlaylistTable
        var items: [PlaylistItem] = []
        /// Rows the in-flight drag picked up, captured when it begins.
        var draggedRows: IndexSet = []
        weak var table: SparkampTableView?
        var applyingExternalSelection = false
        /// Last `model.currentIndex` value we auto-scrolled to (D8). Prevents
        /// re-scrolling on every unrelated `updateNSView` pass while the same
        /// track keeps playing.
        var lastScrolledIndex: Int = -1
        private let cellId = NSUserInterfaceItemIdentifier("playlistRow")

        init(_ parent: ActivePlaylistTable) {
            self.parent = parent
            self.items  = parent.model.playlistItems
        }

        // ── Data source ─────────────────────────────────────────────────
        func numberOfRows(in tableView: NSTableView) -> Int { items.count }

        func tableView(_ tableView: NSTableView,
                       viewFor tableColumn: NSTableColumn?,
                       row: Int) -> NSView? {
            guard row < items.count else { return nil }
            let item = items[row]
            let cell = (tableView.makeView(withIdentifier: cellId, owner: nil)
                        as? SparkampHostingCellView) ?? SparkampHostingCellView()
            cell.identifier = cellId
            cell.setContent(ActivePlaylistTable.makeRowView(
                item: item,
                isCurrent: item.id == parent.model.currentIndex,
                themeManager: parent.themeManager
            ))
            return cell
        }

        // Provide a skin-tinted row view so selection paints with the
        // active skin's highlight colour (see SparkampSkinRowView).
        func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
            SparkampSkinRowView()
        }

        // ── Selection ───────────────────────────────────────────────────
        func tableViewSelectionDidChange(_ notification: Notification) {
            guard !applyingExternalSelection, let table = self.table else { return }
            let ids = table.selectedRowIndexes.compactMap { idx -> Int? in
                guard idx < items.count else { return nil }
                return items[idx].id
            }
            let newSelection = Set(ids)
            if parent.selection != newSelection {
                // Defer to avoid mutating a SwiftUI @Binding during a view update.
                DispatchQueue.main.async { [weak self] in
                    self?.parent.selection = newSelection
                }
            }
        }

        // ── Drag source ─────────────────────────────────────────────────
        /// The rows this drag actually carries.
        ///
        /// `acceptDrop` used to reorder `selectedRowIndexes` instead. AppKit
        /// lets a drag start on a row that is not selected — and the selection
        /// can change between the drag starting and the drop landing — so the
        /// rows that moved were not always the rows the user picked up.
        func tableView(_ tableView: NSTableView,
                       draggingSession session: NSDraggingSession,
                       willBeginAt screenPoint: NSPoint,
                       forRowIndexes rowIndexes: IndexSet) {
            draggedRows = rowIndexes
        }

        func tableView(_ tableView: NSTableView,
                       draggingSession session: NSDraggingSession,
                       endedAt screenPoint: NSPoint,
                       operation: NSDragOperation) {
            draggedRows = []
        }

        func tableView(_ tableView: NSTableView,
                       pasteboardWriterForRow row: Int) -> NSPasteboardWriting? {
            guard row < items.count,
                  let path = parent.model.playlistTrackPath(index: items[row].id)
            else { return nil }
            let pbItem = NSPasteboardItem()
            pbItem.setData(URL(fileURLWithPath: path).dataRepresentation,
                           forType: .fileURL)
            return pbItem
        }

        // ── Drop destination ────────────────────────────────────────────
        func tableView(_ tableView: NSTableView,
                       validateDrop info: NSDraggingInfo,
                       proposedRow row: Int,
                       proposedDropOperation dropOperation: NSTableView.DropOperation) -> NSDragOperation {
            // Force "above row" semantics so the drop is always an
            // insertion between rows, never replace-on-row.
            if dropOperation == .on {
                tableView.setDropRow(row, dropOperation: .above)
            }
            // Intra-table drag = reorder (move); cross-table / external = append (copy).
            if let src = info.draggingSource as? NSTableView, src === tableView {
                return .move
            }
            return .copy
        }

        func tableView(_ tableView: NSTableView,
                       acceptDrop info: NSDraggingInfo,
                       row: Int,
                       dropOperation: NSTableView.DropOperation) -> Bool {
            // Intra-list reorder: move the dragged rows to the drop slot as
            // one block. `draggedRows`, not the current selection — see
            // `willBeginAt`.
            if let src = info.draggingSource as? NSTableView, src === tableView {
                let from = draggedRows.isEmpty ? tableView.selectedRowIndexes : draggedRows
                guard !from.isEmpty else { return false }
                // Re-select what moved, so the drop is visible as a result
                // rather than clearing the selection out from under the user.
                if let landed = parent.model.moveTracks(from: from, to: row) {
                    let moved = IndexSet(integersIn: landed..<(landed + from.count))
                    DispatchQueue.main.async { tableView.selectRowIndexes(moved, byExtendingSelection: false) }
                }
                return true
            }
            // A drag that started inside Sparkamp: use what it parked rather
            // than the paths on the pasteboard, so the drop behaves exactly
            // like the button beside the rows it came from. Both conditions
            // matter — `draggingSource` rules out another app, and the
            // tracklist type rules out an internal drag that never called
            // `begin` (a saved-playlist row publishes a plain string) picking
            // up a cancelled drag's leftovers. See `SparkampDrag`.
            let tracklist = NSPasteboard.PasteboardType(kSparkampTracklistUTI)
            let isSparkampDrag = info.draggingPasteboard.availableType(from: [tracklist]) != nil
            if isSparkampDrag, info.draggingSource != nil, let payload = SparkampDrag.take() {
                apply(payload, at: row)
                return true
            }

            // From outside Sparkamp. The tracklist wins over the `file-url`
            // companion when both are present: it carries every path, the
            // companion only the first.
            var paths: [String] = []
            for item in info.draggingPasteboard.pasteboardItems ?? [] {
                guard let data = item.data(forType: tracklist),
                      let joined = String(data: data, encoding: .utf8)
                else { continue }
                paths.append(contentsOf:
                    joined.split(separator: "\n").map(String.init).filter { !$0.isEmpty })
            }
            if paths.isEmpty {
                paths = (info.draggingPasteboard
                    .readObjects(forClasses: [NSURL.self], options: nil) as? [URL] ?? [])
                    .map(\.path)
            }
            guard !paths.isEmpty else { return false }
            addPaths(paths, at: row)
            return true
        }

        /// Add a dropped Sparkamp payload, each case through the same model
        /// call its source view's own buttons use, then put the block where
        /// the user aimed it.
        private func apply(_ payload: SparkampDrag.Payload, at row: Int) {
            switch payload {
            case .paths(let paths):
                addPaths(paths, at: row)

            case .libraryIds(let ids):
                // `mlDoubleClickTracks` — double-clicking those same tracks
                // in the Files view, exactly.
                guard !ids.isEmpty else { return }
                place(parent.model.mlDoubleClickTracks(ids: ids), at: row)

            case .discTracks(let drive, let entries):
                // `addDiscTracks` with the default mode — the disc view's
                // double-click, exactly.
                place(parent.model.addDiscTracks(drive, entries: entries), at: row)

            case .deferred(let resolve):
                // Walking a device volume or reading a playlist's rows can
                // take real time, and this is the main thread with audio
                // playing on it.
                DispatchQueue.global(qos: .userInitiated).async { [weak self] in
                    let resolved = resolve()
                    DispatchQueue.main.async {
                        self?.apply(resolved, at: row)
                    }
                }
            }
        }

        private func addPaths(_ paths: [String], at row: Int) {
            guard !paths.isEmpty else { return }
            place(parent.model.addFiles(paths.map { URL(fileURLWithPath: $0) }), at: row)
        }

        /// Slide a freshly added block from the end of the playlist to the
        /// row it was dropped on.
        ///
        /// Adding appends — every add route does, and routing a drop through
        /// the same call as the button beside its rows is the point. So the
        /// position is applied afterwards, as a move of exactly the rows that
        /// were just added, through the same core reorder an intra-list drag
        /// uses.
        ///
        /// This is why background tag and duration results are keyed by entry
        /// id rather than row: they are dispatched by the add, and this move
        /// renumbers the rows out from under them.
        ///
        /// `row` is a slot in the pre-add playlist, so it is already correct
        /// as a destination — everything below it is untouched by an append.
        /// A block starting at or before `row` needs no move: either it was
        /// dropped past the end, or the add replaced the playlist outright
        /// (the configured Replace behaviour), in which case the list is now
        /// exactly what was dropped and a position within the old one means
        /// nothing.
        private func place(_ added: [Int], at row: Int) {
            guard let first = added.first, row < first else { return }
            parent.model.moveTracks(from: IndexSet(added), to: row)
        }

        // ── Double-click → play ─────────────────────────────────────────
        @objc func handleDoubleClick() {
            guard let table = self.table else { return }
            let r = table.clickedRow
            guard r >= 0, r < items.count else { return }
            parent.model.jumpTo(index: items[r].id)
        }

        // ── Delete key ──────────────────────────────────────────────────
        func handleDelete() {
            guard let table = self.table else { return }
            let ids = table.selectedRowIndexes
                .compactMap { idx -> Int? in
                    guard idx < items.count else { return nil }
                    return items[idx].id
                }
                .sorted(by: >)            // reverse so each remove doesn't shift later ids
            for id in ids { parent.model.removeTrack(at: id) }
            // Clear binding selection — the table will sync on next update.
            DispatchQueue.main.async { [weak self] in self?.parent.selection.removeAll() }
        }

        // ── Ctrl+Q → queue / dequeue the selected rows ──────────────────
        func handleQueueKey() {
            guard let table = self.table else { return }
            let ids = table.selectedRowIndexes.compactMap { idx -> Int? in
                guard idx < items.count else { return nil }
                return items[idx].id
            }
            parent.model.queueToggle(indices: ids)
        }

        // ── Return key → play first selected ────────────────────────────
        func handleReturn() {
            guard let table = self.table,
                  let firstRow = table.selectedRowIndexes.first,
                  firstRow < items.count
            else { return }
            parent.model.jumpTo(index: items[firstRow].id)
        }

        // ── Context menu ────────────────────────────────────────────────
        func buildContextMenu() -> NSMenu? {
            guard let table = self.table else { return nil }
            let clicked = table.clickedRow
            // If user right-clicked a row that isn't in the current
            // selection, replace selection with just that row (matches
            // Finder semantics).
            if clicked >= 0 && !table.selectedRowIndexes.contains(clicked) {
                table.selectRowIndexes(IndexSet(integer: clicked),
                                       byExtendingSelection: false)
            }
            let ids: Set<Int> = Set(table.selectedRowIndexes.compactMap { idx -> Int? in
                guard idx < items.count else { return nil }
                return items[idx].id
            })
            return parent.contextMenuBuilder(ids)
        }
    }
}

// MARK: - Playlist view

struct PlaylistView: View {
    @EnvironmentObject var model: SparkampModel
    @EnvironmentObject var themeManager: ThemeManager
    /// Multi-select: SwiftUI's `List` reads this `Set` and enables ⌘-click
    /// / shift-click selection automatically when the binding is a Set.
    @State private var selection: Set<Int> = []

    private var theme: SkinTheme { themeManager.currentTheme }

    var body: some View {
        let vars = themeManager.currentVars
        return VStack(spacing: 0) {
            ActivePlaylistTable(
                model: model,
                themeManager: themeManager,
                selection: $selection,
                contextMenuBuilder: { ids in buildContextMenu(ids: ids) }
            )
            .background(theme.playlistBg)

            // Status line: "N tracks · MM:SS total · K selected · MM:SS"
            //
            // Below the table and above the controls, which is where GTK puts
            // it (`pl_root`: scroll → status → separator → button row) and
            // where the Media Library's four status bars sit.
            HStack {
                Text(statusLine)
                    .font(vars.bodyFont)
                    .foregroundStyle(theme.playlistDurationText)
                Spacer()
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(theme.playlistBg.opacity(0.7))

            Divider()
                .background(theme.windowBorder)

            // ── Bottom control bar ────────────────────────────────────────────
            bottomBar
        }
        .background(theme.playlistBg)
        .preferredColorScheme(themeManager.preferredColorScheme)
        // Keep the `l` key's target in step with the selection: exactly one
        // row, or nothing to open.
        .onChange(of: selection) { _, sel in
            guard sel.count == 1, let id = sel.first,
                  let item = model.playlistItems.first(where: { $0.id == id }),
                  let path = model.playlistTrackPath(index: id)
            else {
                model.lyricsTargetPlaylist = nil
                return
            }
            model.lyricsTargetPlaylist = LyricsTarget(
                path: path, artist: item.artist,
                title: item.title, albumArtist: item.albumArtist)
        }
        .onDisappear {
            // Sync model flag when window is closed via the system X button
            // so the playlist button in the player reflects the correct state.
            model.playlistVisible = false
        }
    }

    // MARK: Bottom control bar

    // Every label here carries an explicit colour. Without one they take the
    // window's colour scheme, and macOS does not re-apply `preferredColorScheme`
    // to a window after it has been created: a skin switched to Light left the
    // window painting light backgrounds while these labels stayed white, which
    // made them invisible rather than merely low-contrast.
    private var bottomBar: some View {
        let vars = themeManager.currentVars
        return HStack(spacing: 6) {
            Menu {
                Button("Add Files…")  { model.openFilePicker() }
                Button("Add Folder…") { model.openFolderPicker() }
            } label: {
                Text("Add").font(vars.bodyFont).foregroundStyle(theme.playlistText)
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(
                RoundedRectangle(cornerRadius: 5).fill(theme.playlistButtonBg)
            )
            .help("Add audio files or a folder to the playlist")

            Menu {
                Button("Select All")       { selection = Set(model.playlistItems.map { $0.id }) }
                Button("Select None")      { selection.removeAll() }
                Button("Invert Selection") {
                    selection = Set(model.playlistItems.map { $0.id }).subtracting(selection)
                }
                .keyboardShortcut("i", modifiers: .command)
            } label: {
                Text("Select").font(vars.bodyFont).foregroundStyle(theme.playlistText)
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(
                RoundedRectangle(cornerRadius: 5).fill(theme.playlistButtonBg)
            )
            .help("Change the current selection")

            Menu {
                Button("Title")    { model.sortPlaylist(.title); selection.removeAll() }
                Button("Artist")   { model.sortPlaylist(.artist); selection.removeAll() }
                Button("Album")    { model.sortPlaylist(.album); selection.removeAll() }
                Button("Filename") { model.sortPlaylist(.filename); selection.removeAll() }
                Button("Path")     { model.sortPlaylist(.path); selection.removeAll() }
                Divider()
                Button("Randomize") { model.randomizePlaylist(); selection.removeAll() }
                Button("Reverse")   { model.reversePlaylist(); selection.removeAll() }
            } label: {
                Text("Sort").font(vars.bodyFont).foregroundStyle(theme.playlistText)
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(
                RoundedRectangle(cornerRadius: 5).fill(theme.playlistButtonBg)
            )
            .disabled(model.playlistItems.isEmpty)
            .help("Sort, randomize, or reverse the playlist")

            Spacer()

            Menu {
                Button("Save Playlist…") { saveActivePlaylistAs() }
                    .keyboardShortcut("s", modifiers: .command)
                Divider()
                Button("Remove Selected") { removeIndices(Array(selection).sorted()) }
                    .disabled(selection.isEmpty)
                Button("Remove All") { model.clearPlaylist(); selection.removeAll() }
                    .disabled(model.playlistItems.isEmpty)
            } label: {
                Text("List").font(vars.bodyFont).foregroundStyle(theme.playlistText)
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(
                RoundedRectangle(cornerRadius: 5).fill(theme.playlistButtonBg)
            )
            .disabled(model.playlistItems.isEmpty)
            .help("Save or clear the playlist")
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(theme.playlistBg.opacity(0.85))
        .overlay(alignment: .leading) { menuShortcutButtons }
    }

    /// Zero-size hidden buttons carrying ⌘S, ⌘I and ⌃Q.
    ///
    /// The first two also live in the Add/Select/Sort/List menus above, but a
    /// SwiftUI `Menu`'s content is built lazily when the menu is opened, so a
    /// `.keyboardShortcut` nested inside one is not guaranteed to be live
    /// beforehand. These duplicates run the same code and are always in the
    /// view tree, so the keys work whether or not the menu registered them.
    ///
    /// ⌃Q is here because the app-wide `NSEvent` monitor cannot carry it:
    /// `handleRawKey` refuses anything with a modifier, so a modified key needs
    /// a real responder-chain shortcut. GTK binds Ctrl+Q to enqueue/dequeue in
    /// its playlist window as well as its Jump window; on mac only the Jump
    /// window had it, so the shortcut list was promising something the
    /// playlist did not do.
    private var menuShortcutButtons: some View {
        ZStack {
            Button("", action: saveActivePlaylistAs)
                .keyboardShortcut("s", modifiers: .command)
                .disabled(model.playlistItems.isEmpty)
            Button("") {
                selection = Set(model.playlistItems.map { $0.id }).subtracting(selection)
            }
            .keyboardShortcut("i", modifiers: .command)
            Button("") { model.queueToggle(indices: Array(selection).sorted()) }
                .keyboardShortcut("q", modifiers: .control)
                .disabled(selection.isEmpty)
        }
        .frame(width: 0, height: 0)
        .opacity(0)
        .accessibilityHidden(true)
    }

    // MARK: Helpers

    private var statusLine: String {
        let count = model.playlistItems.count
        let total = model.playlistItems.reduce(0) { $0 + max(Int($1.duration), 0) }
        let selRows = model.playlistItems.filter { selection.contains($0.id) }
        let sel: (count: Int, secs: Int)? = selRows.isEmpty ? nil :
            (selRows.count, selRows.reduce(0) { $0 + max(Int($1.duration), 0) })
        return playlistStatusLine(count: count, totalSecs: total, selected: sel)
    }

    /// Builds the right-click context menu shown when the user opens it
    /// over `ids` in the active-playlist NSTableView.  Mirrors the previous
    /// SwiftUI `contextMenu(forSelectionType:)` content but in AppKit form
    /// because `ActivePlaylistTable` returns an `NSMenu` to the table.
    private func buildContextMenu(ids: Set<Int>) -> NSMenu {
        let sorted = ids.sorted()
        let menu = NSMenu()
        menu.autoenablesItems = false

        // Order: Play · Enqueue/Dequeue · Send to · ─ · ID3 · Album Art ·
        // Lyrics · ─ · Remove. Matches GTK's player.rs playlist row menu.
        menu.addItem(BlockMenuItem(title: "Play", enabled: !sorted.isEmpty) {
            if let first = sorted.first { model.jumpTo(index: first) }
        })

        // Enqueue / Dequeue the selection (manual play queue). Toggles each
        // selected row's queue membership; the [n] badges update in place.
        menu.addItem(BlockMenuItem(title: "Enqueue / Dequeue", enabled: !sorted.isEmpty) {
            model.queueToggle(indices: sorted)
        })

        // Shared "Send to" submenu (Saved Playlist ▸ / Disc Drive /
        // Removable Device), same as the files view and the saved-playlist
        // editor. `includeActive: false` — these tracks are already in the
        // active playlist, mirrors GTK's player.rs row menu (`active: ""`).
        let paths = sorted.compactMap { model.playlistTrackPath(index: $0) }
        menu.addItem(model.sendToMenuItem(paths: paths, includeActive: false))

        menu.addItem(.separator())

        menu.addItem(BlockMenuItem(title: "View/Edit Tags", enabled: sorted.count == 1) {
            if let first = sorted.first { model.openId3Editor(trackIndex: first) }
        })

        menu.addItem(BlockMenuItem(title: "View Album Art", enabled: sorted.count == 1) {
            if let first = sorted.first, let p = model.playlistTrackPath(index: first) {
                model.mlViewArtForPath(p)
            }
        })

        menu.addItem(BlockMenuItem(title: "View/Search Lyrics", enabled: sorted.count == 1) {
            if let first = sorted.first { model.viewOrSearchLyricsForPlaylist(index: first) }
        })

        menu.addItem(.separator())

        menu.addItem(BlockMenuItem(title: "Remove", enabled: !sorted.isEmpty) {
            removeIndices(sorted)
        })

        return menu
    }

    private func removeIndices(_ indices: [Int]) {
        // Reverse-sorted so each removal doesn't shift later indices.
        for i in indices.sorted(by: >) { model.removeTrack(at: i) }
        selection.removeAll()
    }

    /// Save the entire active playlist to an M3U8 via the native Save panel.
    /// Bound to the bottom-bar "Save" button.  No-op if the playlist is
    /// empty.
    private func saveActivePlaylistAs() {
        let paths = (0..<model.playlistItems.count)
            .compactMap { model.playlistTrackPath(index: $0) }
        guard !paths.isEmpty else { return }
        runPlaylistSavePanel(model: model,
                              defaultName: defaultTimestampedPlaylistName()) { stem, dir in
            _ = model.mlSavePlaylistAs(name: stem, trackPaths: paths, directory: dir)
            model.mlRefreshSavedPlaylists()
        }
    }
}

// MARK: - Playlist row (single-line: "Artist — Title")

struct PlaylistRow: View {
    let item: PlaylistItem
    let isCurrent: Bool

    @EnvironmentObject var themeManager: ThemeManager
    private var theme: SkinTheme { themeManager.currentTheme }

    var body: some View {
        let vars = themeManager.currentVars
        return HStack(spacing: 6) {
            // State / broken / read-only indicator.
            Group {
                if isCurrent {
                    Image(systemName: "waveform")
                        .font(.system(size: 9))
                        .foregroundStyle(theme.playlistCurrentText)
                } else if item.broken {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.system(size: 9))
                        .foregroundStyle(theme.playlistBrokenText)
                } else if item.fileMissing {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 9))
                        .foregroundStyle(.red)
                } else if item.readOnly {
                    Image(systemName: "lock.fill")
                        .font(.system(size: 9))
                        .foregroundStyle(theme.playlistDurationText)
                } else {
                    Color.clear
                }
            }
            .frame(width: 12)

            // Single-line display with the manual-queue [n] badge prefix.
            Text(item.queueBadge + item.displayName)
                .font(vars.bodyFont)
                .foregroundStyle(
                    isCurrent ? theme.playlistCurrentText
                    : item.broken ? theme.playlistBrokenText
                    : theme.playlistText
                )
                .lineLimit(1)
                .truncationMode(.tail)

            Spacer()

            // Duration
            Text(item.durationString)
                .font(vars.smallMonospaceFont)
                .foregroundStyle(theme.playlistDurationText)
        }
        .contentShape(Rectangle())
    }
}

