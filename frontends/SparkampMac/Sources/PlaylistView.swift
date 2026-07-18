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
        guard !paths.isEmpty else { return p }
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
        table.registerForDraggedTypes([.fileURL])
        table.setDraggingSourceOperationMask([.copy, .move], forLocal: true)
        table.setDraggingSourceOperationMask([.copy],        forLocal: false)

        table.onDeleteKey   = { [weak c = context.coordinator] in c?.handleDelete()   }
        table.onReturnKey   = { [weak c = context.coordinator] in c?.handleReturn()   }
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
            // Intra-list reorder: move every selected row to the drop slot.
            if let src = info.draggingSource as? NSTableView, src === tableView {
                let from = tableView.selectedRowIndexes
                guard !from.isEmpty else { return false }
                parent.model.moveTrack(from: from, to: row)
                return true
            }
            // Cross-list / external: read file URLs from pasteboard items and
            // hand them to the model (which already inherits ML metadata).
            let urls = info.draggingPasteboard
                .readObjects(forClasses: [NSURL.self], options: nil) as? [URL] ?? []
            guard !urls.isEmpty else { return false }
            parent.model.addFiles(urls)
            return true
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
            // Track count header
            HStack {
                Text("\(model.playlistItems.count) track\(model.playlistItems.count == 1 ? "" : "s")")
                    .font(vars.bodyFont)
                    .foregroundStyle(theme.playlistDurationText)
                Spacer()
                if let total = totalDuration {
                    Text(total)
                        .font(vars.smallMonospaceFont)
                        .foregroundStyle(theme.playlistDurationText)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(theme.playlistBg.opacity(0.7))

            Divider()
                .background(theme.windowBorder)

            ActivePlaylistTable(
                model: model,
                themeManager: themeManager,
                selection: $selection,
                contextMenuBuilder: { ids in buildContextMenu(ids: ids) }
            )
            .background(theme.playlistBg)

            Divider()
                .background(theme.windowBorder)

            // ── Bottom control bar ────────────────────────────────────────────
            bottomBar
        }
        .background(theme.playlistBg)
        .preferredColorScheme(themeManager.preferredColorScheme)
        .onDisappear {
            // Sync model flag when window is closed via the system X button
            // so the playlist button in the player reflects the correct state.
            model.playlistVisible = false
        }
    }

    // MARK: Bottom control bar

    private var bottomBar: some View {
        let vars = themeManager.currentVars
        return HStack(spacing: 6) {
            // Left side: Add Files, Add Folder
            Button {
                model.openFilePicker()
            } label: {
                Label("Add Files", systemImage: "plus")
                    .font(vars.bodyFont)
            }
            .buttonStyle(PlaylistControlButtonStyle(theme: theme))
            .help("Add audio files to playlist")

            Button {
                model.openFolderPicker()
            } label: {
                Label("Add Folder", systemImage: "folder.badge.plus")
                    .font(vars.bodyFont)
            }
            .buttonStyle(PlaylistControlButtonStyle(theme: theme))
            .help("Add all audio files in a folder")

            Button {
                saveActivePlaylistAs()
            } label: {
                Label("Save", systemImage: "square.and.arrow.down")
                    .font(vars.bodyFont)
            }
            .buttonStyle(PlaylistControlButtonStyle(theme: theme))
            .disabled(model.playlistItems.isEmpty)
            .help("Save active playlist to an M3U8 file")

            Spacer()

            // Right side: Remove Selected, Remove All
            Button {
                removeIndices(Array(selection).sorted())
            } label: {
                Label("Remove", systemImage: "minus")
                    .font(vars.bodyFont)
            }
            .buttonStyle(PlaylistControlButtonStyle(theme: theme))
            .disabled(selection.isEmpty)
            .help("Remove selected track(s)")

            Button {
                model.clearPlaylist()
                selection.removeAll()
            } label: {
                Label("Remove All", systemImage: "trash")
                    .font(vars.bodyFont)
            }
            .buttonStyle(PlaylistControlButtonStyle(theme: theme))
            .disabled(model.playlistItems.isEmpty)
            .help("Clear entire playlist")
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(theme.playlistBg.opacity(0.85))
    }

    // MARK: Helpers

    private var totalDuration: String? {
        let total = model.playlistItems.reduce(0.0) { $0 + max($1.duration, 0) }
        guard total > 0 else { return nil }
        return formatDuration(total)
    }

    /// Builds the right-click context menu shown when the user opens it
    /// over `ids` in the active-playlist NSTableView.  Mirrors the previous
    /// SwiftUI `contextMenu(forSelectionType:)` content but in AppKit form
    /// because `ActivePlaylistTable` returns an `NSMenu` to the table.
    private func buildContextMenu(ids: Set<Int>) -> NSMenu {
        let sorted = ids.sorted()
        let menu = NSMenu()
        menu.autoenablesItems = false

        menu.addItem(BlockMenuItem(title: "Play", enabled: !sorted.isEmpty) {
            if let first = sorted.first { model.jumpTo(index: first) }
        })

        // Shared "Send to" submenu (Saved Playlist ▸ / Disc Drive /
        // Removable Device), same as the files view and the saved-playlist
        // editor. `includeActive: false` — these tracks are already in the
        // active playlist, mirrors GTK's player.rs row menu (`active: ""`).
        let paths = sorted.compactMap { model.playlistTrackPath(index: $0) }
        menu.addItem(model.sendToMenuItem(paths: paths, includeActive: false))

        menu.addItem(BlockMenuItem(title: "Edit Tags…", enabled: sorted.count == 1) {
            if let first = sorted.first { model.openId3Editor(trackIndex: first) }
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

            // Single-line display: "Artist — Title"
            Text(item.displayName)
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

// MARK: - Playlist control button style

struct PlaylistControlButtonStyle: ButtonStyle {
    let theme: SkinTheme

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(theme.modeBtnText)
            .padding(.horizontal, 6)
            .padding(.vertical, 4)
            .background(
                RoundedRectangle(cornerRadius: 3)
                    .fill(configuration.isPressed ? theme.transportActiveBg : theme.modeBtnBg)
                    .overlay(
                        RoundedRectangle(cornerRadius: 3)
                            .stroke(theme.modeBtnBorder, lineWidth: 1)
                    )
            )
            .opacity(configuration.isPressed ? 0.8 : 1.0)
    }
}
