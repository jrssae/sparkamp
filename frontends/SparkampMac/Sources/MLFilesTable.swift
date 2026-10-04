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

    /// Whether a column is shown: the source column with servers, the pinned
    /// columns always, the rest by the column picker's mask.
    private func isShown(_ spec: ColumnSpec) -> Bool {
        if spec.bit == -2 { return !model.servers.isEmpty }
        return Self.isPicked(spec, mask: columnMask)
    }

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
        // Source column: where a song's copies are and whether they agree.
        // bit -2 = shown exactly when servers are configured.
        .init(id: "col-src",         title: "Src",         bit: -2, width: 36,  sortKey: nil,           isSmallMono: true),
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
        // Phase-1 technical columns (Task 3/7); column ids double as GTK's sortKeys verbatim.
        .init(id: "col-samplerate",  title: "Sample Rate", bit: 17, width:  90, sortKey: "sample_rate",  isSmallMono: true),
        .init(id: "col-filesize",    title: "Size",        bit: 18, width:  80, sortKey: "file_size",    isSmallMono: true),
        .init(id: "col-added",       title: "Date Added",  bit: 19, width: 130, sortKey: "added_at",     isSmallMono: true),
        .init(id: "col-mtime",       title: "File Modified", bit: 20, width: 130, sortKey: "file_mtime", isSmallMono: true),
        .init(id: "col-brmode",      title: "Mode",        bit: 21, width:  80, sortKey: "bitrate_mode", isSmallMono: true),
        // ReplayGain track gain (phase-4 F7); hidden by default like GTK's
        // opt-in rg_gain column. sortKey mirrors GTK's "rg_gain".
        .init(id: "col-rggain",      title: "ReplayGain",  bit: 22, width:  90, sortKey: "rg_gain",      isSmallMono: true),
    ]

    /// Status first, then the source column right after it.
    private static let pinned = ["col-status", "col-src"]

    func makeNSView(context: Context) -> NSScrollView {
        let table = Self.makeTable()
        // The play-position column belongs to the playlist editor only.
        Self.addColumns(to: table) { !$0.editorOnly }
        context.coordinator.needsFirstRunWidths =
            Self.restoreLayout(of: table, autosaveName: "sparkamp.ml.filesTable")
        Self.applyVisibility(to: table, isShown)

        table.dataSource = context.coordinator
        table.delegate   = context.coordinator

        // Drag/drop registration.
        table.registerForDraggedTypes([.fileURL, NSPasteboard.PasteboardType(kSparkampTracklistUTI)])
        table.setDraggingSourceOperationMask([.copy], forLocal: true)
        table.setDraggingSourceOperationMask([.copy], forLocal: false)

        // Key + context menu + double-click hooks.
        table.onDeleteKey   = { [weak c = context.coordinator] in c?.handleDelete()   }
        table.onReturnKey   = { [weak c = context.coordinator] in c?.handleDoubleClick() }
        table.onContextMenu = { [weak c = context.coordinator] _ in c?.buildContextMenu() }
        table.target        = context.coordinator
        table.doubleAction  = #selector(Coordinator.handleDoubleClick)

        context.coordinator.table = table
        return table.inScrollView(horizontal: true)
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let table = scroll.documentView as? SparkampTableView else { return }
        context.coordinator.parent = self
        let oldIds = context.coordinator.tracks.map(\.id)
        let newIds = tracks.map(\.id)
        context.coordinator.tracks = tracks
        let artistAsAlbumArtist = model.ctx.map { sparkamp_get_artist_as_album_artist($0) } ?? false
        if oldIds != newIds {
            table.reloadData()
        } else {
            // Same set of ids — refresh visible cells in case theme or
            // mutable fields (play_count, last_played, scanned) changed.
            table.refreshVisibleCells(rowCount: tracks.count) { r, colId in
                Self.specs.first(where: { $0.id == colId }).map { spec in
                    Self.cellContent(track: tracks[r], spec: spec, theme: theme,
                                     artistAsAlbumArtist: artistAsAlbumArtist,
                                     onViewArt: { onEvent(.viewArt($0)) })
                }
            }
        }

        Self.applyVisibility(to: table, isShown)
        // The status column carries the read-only / missing-file / unscanned
        // indicator and only makes sense at the start of the row. A layout
        // saved before servers existed does not know the source column.
        Self.pinColumns(Self.pinned, in: table)

        // A first launch has no saved widths: fit the columns to the first
        // rows. Done once; resizing it later is the user's.
        if context.coordinator.needsFirstRunWidths, !tracks.isEmpty {
            context.coordinator.needsFirstRunWidths = false
            Self.applyFirstRunWidths(table, tracks: tracks, theme: theme,
                                     artistAsAlbumArtist: artistAsAlbumArtist)
            Self.storeSharedWidths(from: table)
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

        table.show(selection: selection, in: tracks, id: \.id)
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    /// The icon for the core's three-cell source mark: the asset
    /// `source-<name>`, the same drawings GTK uses (SVGs in
    /// `frontends/gtk/icons/source/`). A cloud is a server copy, a check means
    /// the copies agree, an arrow says which side is ahead. Not the
    /// `icloud.*` SF Symbols: Apple reserves those for iCloud itself.
    static func sourceIcon(_ mark: String) -> String? {
        let cells = Array(mark)
        guard cells.count == 3 else { return nil }
        let local = cells[0] != " "
        let name: String
        switch (cells[1], cells[2]) {
        case ("×", _):  name = "unreachable"
        case (_, "!"):  name = "conflict"
        case (_, "?"):  name = "choose"
        case (_, "↑"):  name = "local-newer"
        case (_, "↓"):  name = "server-newer"
        case (_, "≈"):  name = "possible-match"
        case ("☁", _):  name = local ? "synced" : "server"
        default:
            guard local else { return nil }
            name = "local"
        }
        return "source-" + name
    }

    /// The source mark in words, for its tooltip.
    static func sourceMarkHelp(_ mark: String) -> String {
        let cells = Array(mark)
        guard cells.count == 3 else { return "" }
        var parts: [String] = []
        switch (cells[0] != " ", cells[1]) {
        case (true, "☁"): parts.append("On this Mac and on a server")
        case (true, _): parts.append("Only on this Mac")
        case (false, "×"): parts.append("On a server that cannot be reached")
        default: parts.append("Only on a server")
        }
        switch cells[2] {
        case "↑": parts.append("changed here; the server is behind")
        case "↓": parts.append("changed on the server")
        case "!": parts.append("changed in both places differently")
        case "?": parts.append("the copies differ; choose which is right")
        case "≈": parts.append("more than one possible match")
        default: break
        }
        return parts.joined(separator: ", ")
    }

    // ── Cell content builder ────────────────────────────────────────────
    static func cellContent(track: MLTrack,
                                        spec: ColumnSpec,
                                        theme: SkinTheme,
                                        artistAsAlbumArtist: Bool,
                                        onViewArt: @escaping (Int64) -> Void) -> AnyView {
        let body: AnyView
        let text = { displayText(track, columnId: spec.id, artistAsAlbumArtist: artistAsAlbumArtist) ?? "" }
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
                    } else if track.readOnly && track.id > 0 {
                        // Server-only songs (negative ids) are read-only
                        // too, but the Src column's cloud already says so; a
                        // lock on every one of them would be noise.
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
        case "col-src":
            body = AnyView(
                Group {
                    if let icon = MLFilesTable.sourceIcon(track.sourceMark) {
                        Image(icon)
                            .resizable()
                            .frame(width: 16, height: 16)
                            .accessibilityLabel(MLFilesTable.sourceMarkHelp(track.sourceMark))
                    } else {
                        Color.clear
                    }
                }
                .help(MLFilesTable.sourceMarkHelp(track.sourceMark))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            )
        case "col-title":
            body = AnyView(textCell(text(),
                                    color: track.fileMissing  ? .red
                                         : track.scanned      ? theme.playlistText
                                         : theme.playlistDurationText,
                                    spec: spec, theme: theme))
        case "col-artist", "col-album", "col-albumartist", "col-genre", "col-composer", "col-year":
            body = AnyView(textCell(text(),
                                    color: track.fileMissing ? .red : theme.playlistText,
                                    spec: spec, theme: theme))
        case "col-tracknum", "col-discnum", "col-bpm", "col-comment":
            body = AnyView(textCell(text(), color: theme.playlistText, spec: spec, theme: theme))
        case "col-duration", "col-bitrate", "col-filename", "col-playcount", "col-lastplayed",
             "col-samplerate", "col-filesize", "col-added", "col-mtime", "col-brmode", "col-rggain":
            body = AnyView(textCell(text(), color: theme.playlistDurationText, spec: spec, theme: theme))
        case "col-art":
            // A2 — small thumbnail from the resolved artwork path when one's
            // known; falls back to the pre-existing "View" text link when
            // the row is marked has_art but the path didn't resolve (keeps
            // that behavior working exactly as before), and a blank cell
            // when there's no art at all.
            let tid = track.id
            let artPath = track.artworkPath
            body = AnyView(
                Group {
                    if !artPath.isEmpty, let img = NSImage(contentsOfFile: artPath) {
                        Button { onViewArt(tid) } label: {
                            Image(nsImage: img)
                                .resizable()
                                .aspectRatio(contentMode: .fill)
                                .frame(width: 18, height: 18)
                                .clipShape(RoundedRectangle(cornerRadius: 2))
                        }
                        .buttonStyle(.plain)
                        .help("Click to view album art")
                    } else if track.hasArt {
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

    /// Human file size: whole KB under 1 MB, one-decimal MB above — matches
    /// GTK's `format_file_size` thresholds so the two frontends agree.
    /// The text a column shows for `track`, or nil for the columns that
    /// draw something else (status, source, artwork). Cells and first-run
    /// column sizing both read it, so a width is measured on exactly what
    /// is drawn.
    static func displayText(_ track: MLTrack, columnId: String, artistAsAlbumArtist: Bool) -> String? {
        switch columnId {
        case "col-title":       return track.title.isEmpty ? track.filename : track.title
        case "col-artist":      return track.artist
        case "col-album":       return track.album
        case "col-albumartist":
            // F12.2: mirrors src/play_stats.rs's effective_album_artist —
            // album_artist wins whenever non-blank (trimmed), else falls
            // back to artist when the "treat artist as album artist" toggle
            // is on, else blank. A4 (phase 11 album gallery) MUST use the
            // same rule.
            let trimmed = track.albumArtist.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty { return track.albumArtist }
            return artistAsAlbumArtist ? track.artist : ""
        case "col-genre":       return track.genre
        case "col-composer":    return track.composer
        case "col-year":        return track.year > 0 ? "\(track.year)" : ""
        case "col-tracknum":    return track.trackNum > 0 ? "\(track.trackNum)" : ""
        case "col-discnum":     return track.discNum > 0 ? "\(track.discNum)" : ""
        case "col-bpm":         return track.bpm
        case "col-comment":     return track.comment
        case "col-duration":
            let total = Int(track.lengthSecs)
            return total > 0 ? String(format: "%d:%02d", total / 60, total % 60) : ""
        case "col-bitrate":     return track.bitrate > 0 ? "\(track.bitrate) kbps" : ""
        case "col-filename":    return track.filename
        case "col-playcount":   return track.playCount > 0 ? "\(track.playCount)" : ""
        case "col-lastplayed":  return track.lastPlayedDisplay
        case "col-samplerate":
            return track.sampleRate > 0 ? String(format: "%.1f kHz", Double(track.sampleRate) / 1000) : ""
        case "col-filesize":    return formatFileSize(track.fileSize)
        case "col-added":       return track.addedAtDisplay
        case "col-mtime":       return track.fileMtimeDisplay
        case "col-brmode":      return track.bitrateMode
        case "col-rggain":      return track.rgGainDisplay
        default:                return nil
        }
    }

    // MARK: Table setup shared with the playlist editor

    /// A table for library rows: inset, with columns the user can reorder
    /// and resize.
    static func makeTable() -> SparkampTableView {
        let table = SparkampTableView(rowSpacing: 6)
        table.style = .inset
        table.allowsColumnReordering = true
        table.allowsColumnResizing = true
        // Columns change width only when the user drags them. With automatic
        // resizing, AppKit spread every change in the table's width across
        // all columns: the view is built at zero width and then laid out at
        // the window's, so every launch moved the saved widths, and saved
        // the moved ones.
        table.columnAutoresizingStyle = .noColumnAutoresizing
        return table
    }

    /// Add a column for each spec `include` accepts, in spec order.
    static func addColumns(to table: NSTableView, where include: (ColumnSpec) -> Bool) {
        for spec in specs where include(spec) {
            let col = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(spec.id))
            col.title = spec.title
            col.width = spec.width
            col.minWidth = max(20, spec.width * 0.3)
            // Wide enough never to undo a width the user chose.
            col.maxWidth = 2000
            col.resizingMask = [.userResizingMask, .autoresizingMask]
            switch spec.id {
            case "col-status", "col-position":
                // Pinned: fixed width, no resize, no reorder.
                col.minWidth = spec.width
                col.maxWidth = spec.width
                col.resizingMask = []
            case "col-src":
                // Three marks, so it never needs more room. It takes no share
                // when the table spreads spare width, which otherwise grows it
                // as wide as the title.
                col.minWidth = spec.width
                col.maxWidth = 80
                col.resizingMask = [.userResizingMask]
            default:
                break
            }
            if let key = spec.sortKey {
                col.sortDescriptorPrototype = NSSortDescriptor(key: key, ascending: true)
            }
            table.addTableColumn(col)
        }
    }

    /// Turn on column autosave, then apply the widths shared by the Files
    /// view and the playlist editor. Returns true when there is neither a
    /// saved layout nor shared widths, so the columns should be fitted to the
    /// first rows that arrive.
    ///
    /// Autosave is turned on AFTER the columns exist, and the order of these
    /// two steps is the whole point. NSTableView applies the saved
    /// configuration to the columns present at the moment `autosaveName` is
    /// set. Set the name first and there is nothing to apply it to:
    /// `addTableColumn` never consults the archive, so every column arrives
    /// at its spec default. Saving worked the whole time, which is what made
    /// the bug so quiet. The layout was written back on every resize and
    /// drag, and read back never, so leaving the view or quitting the app
    /// looked like it had thrown the layout away.
    static func restoreLayout(of table: NSTableView, autosaveName: String) -> Bool {
        let hadLayout = hasSavedLayout(autosaveName)
        table.autosaveTableColumns = true
        table.autosaveName = autosaveName
        // A width set in either table wins in both. Before anything was
        // shared, this table's own layout is the one both start from.
        if applySharedWidths(to: table) { return false }
        if hadLayout { storeSharedWidths(from: table) }
        return !hadLayout
    }

    /// Whether the column picker's `mask` shows a column. Pinned columns
    /// (negative bit) always show.
    static func isPicked(_ spec: ColumnSpec, mask: Int) -> Bool {
        spec.bit < 0 || (mask >> spec.bit) & 1 == 1
    }

    /// Hide every column `isShown` turns down, and show the rest.
    static func applyVisibility(to table: NSTableView, _ isShown: (ColumnSpec) -> Bool) {
        for col in table.tableColumns {
            guard let spec = specs.first(where: { $0.id == col.identifier.rawValue }) else { continue }
            let hidden = !isShown(spec)
            if col.isHidden != hidden { col.isHidden = hidden }
        }
    }

    /// Put the columns `ids` names in the first slots, in that order. Autosave
    /// can restore a layout that moved them, and NSTableView appends a column
    /// a saved layout lacks at the far end, off-screen.
    static func pinColumns(_ ids: [String], in table: NSTableView) {
        for (slot, id) in ids.enumerated() {
            guard slot < table.tableColumns.count,
                  let idx = table.tableColumns.firstIndex(where: { $0.identifier.rawValue == id }),
                  idx != slot
            else { continue }
            table.moveColumn(idx, toColumn: slot)
        }
    }

    /// Whether the user may drag column `from` to slot `to`: never a pinned
    /// column, and nothing into a pinned column's slot.
    static func allowsReorder(_ table: NSTableView, from: Int, to: Int, pinned: [String]) -> Bool {
        !pinned.contains(table.tableColumns[from].identifier.rawValue) && to >= pinned.count
    }

    // MARK: Widths shared with the playlist editor

    /// One width per column id, from whichever of the Files view and the
    /// playlist editor it was last set in, so the two tables always show a
    /// column at the same width. Fixed columns (status, play position) are
    /// left out.
    static let sharedWidthsKey = "sparkamp.ml.columnWidths"

    static func sharedWidths() -> [String: Double] {
        UserDefaults.standard.dictionary(forKey: sharedWidthsKey) as? [String: Double] ?? [:]
    }

    /// Record the width of each of `table`'s resizable columns.
    static func storeSharedWidths(from table: NSTableView) {
        var widths = sharedWidths()
        for col in table.tableColumns where !col.resizingMask.isEmpty {
            widths[col.identifier.rawValue] = Double(col.width)
        }
        UserDefaults.standard.set(widths, forKey: sharedWidthsKey)
    }

    /// Give `table`'s resizable columns their shared widths. False when
    /// nothing has been shared yet.
    @discardableResult
    static func applySharedWidths(to table: NSTableView) -> Bool {
        let widths = sharedWidths()
        guard !widths.isEmpty else { return false }
        for col in table.tableColumns where !col.resizingMask.isEmpty {
            if let w = widths[col.identifier.rawValue] { col.width = CGFloat(w) }
        }
        return true
    }

    /// Whether AppKit holds a saved layout for this table. Only a table that
    /// has never been laid out gets sized to its content; after that the
    /// user's widths are the ones that count.
    static func hasSavedLayout(_ autosaveName: String) -> Bool {
        UserDefaults.standard.dictionaryRepresentation().keys.contains {
            $0.hasPrefix("NSTableView Columns") && $0.hasSuffix(autosaveName)
        }
    }

    /// Size every visible text column to its longest text (header included),
    /// but no wider than 60 characters.
    static func applyFirstRunWidths(_ table: NSTableView, tracks: [MLTrack], theme: SkinTheme,
                                    artistAsAlbumArtist: Bool) {
        let fontSize = theme.vars.fontSize
        let body = NSFont(name: theme.vars.primaryFontFamily, size: fontSize) ?? .systemFont(ofSize: fontSize)
        let mono = NSFont.monospacedSystemFont(ofSize: fontSize, weight: .regular)
        let headerFont = NSFont.systemFont(ofSize: NSFont.systemFontSize, weight: .semibold)
        func width(_ s: String, _ font: NSFont) -> CGFloat {
            ceil((s as NSString).size(withAttributes: [.font: font]).width)
        }
        // Cell padding: the text's leading inset plus room either side.
        let padding: CGFloat = 16
        for col in table.tableColumns where !col.isHidden {
            guard let spec = specs.first(where: { $0.id == col.identifier.rawValue }),
                  !spec.editorOnly else { continue }
            let font = spec.isSmallMono ? mono : body
            let cap = width(String(repeating: "abcdefghij", count: 6), font) + padding
            // Measuring every row of a large library is slow; the widest
            // text is almost always among the longest by character count.
            let texts = tracks.compactMap { displayText($0, columnId: spec.id, artistAsAlbumArtist: artistAsAlbumArtist) }
            guard !texts.isEmpty else { continue }
            let longest = texts.sorted { $0.count > $1.count }.prefix(50)
            let widest = longest.map { width($0, font) }.max() ?? 0
            let needed = max(widest + padding, width(spec.title, headerFont) + padding, col.minWidth)
            col.width = min(needed, cap)
        }
    }

    private static func formatFileSize(_ bytes: Int64) -> String {
        guard bytes > 0 else { return "" }
        if bytes < 1_000_000 {
            return "\(bytes / 1_000) KB"
        } else {
            return String(format: "%.1f MB", Double(bytes) / 1_000_000)
        }
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
        case "sample_rate":  return KeyPathComparator(\MLTrack.sampleRate, order: order)
        case "file_size":    return KeyPathComparator(\MLTrack.fileSize, order: order)
        case "added_at":     return KeyPathComparator(\MLTrack.addedAt, order: order)
        case "file_mtime":   return KeyPathComparator(\MLTrack.fileMtime, order: order)
        case "bitrate_mode": return KeyPathComparator(\MLTrack.bitrateMode, order: order)
        case "rg_gain":      return KeyPathComparator(\MLTrack.rgTrackGain, order: order)
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
        case \MLTrack.sampleRate:  return "sample_rate"
        case \MLTrack.fileSize:    return "file_size"
        case \MLTrack.addedAt:     return "added_at"
        case \MLTrack.fileMtime:   return "file_mtime"
        case \MLTrack.bitrateMode: return "bitrate_mode"
        case \MLTrack.rgTrackGain: return "rg_gain"
        default:                   return nil
        }
    }

    @MainActor final class Coordinator: NSObject, NSTableViewDataSource, NSTableViewDelegate {
        var parent: MLFilesTable
        var tracks: [MLTrack] = []
        weak var table: SparkampTableView?
        /// No saved layout yet: size the columns to the first rows that
        /// arrive.
        var needsFirstRunWidths = false
        private let cellId = NSUserInterfaceItemIdentifier("mlFileCell")

        init(_ parent: MLFilesTable) {
            self.parent = parent
            self.tracks = parent.tracks
        }

        func numberOfRows(in tableView: NSTableView) -> Int { tracks.count }

        // Status column is the row's read-only/error indicator; it must
        // always sit at the start of the row regardless of autosave.
        func tableView(_ tableView: NSTableView,
                       shouldReorderColumn columnIndex: Int,
                       toColumn newColumnIndex: Int) -> Bool {
            MLFilesTable.allowsReorder(tableView, from: columnIndex, to: newColumnIndex,
                                       pinned: ["col-status"])
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
                artistAsAlbumArtist: parent.model.ctx.map { sparkamp_get_artist_as_album_artist($0) } ?? false,
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
            guard let table = self.table, !table.isShowingSelection else { return }
            let newSelection = Set(table.selectedIds(in: tracks, \.id))
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

        // Drag source: one item per row (multi-row native), its path as
        // written — a server song's URI stays a URI.
        func tableView(_ tableView: NSTableView,
                       pasteboardWriterForRow row: Int) -> NSPasteboardWriting? {
            guard row < tracks.count else { return nil }
            let path = tracks[row].path
            guard !path.isEmpty else { return nil }
            return TrackDragPayload.pasteboardItem(forPath: path)
        }

        // What the drag means inside Sparkamp: these library rows, added by
        // id the way a double-click adds them. See `SparkampDrag`.
        func tableView(_ tableView: NSTableView,
                       draggingSession session: NSDraggingSession,
                       willBeginAt screenPoint: NSPoint,
                       forRowIndexes rowIndexes: IndexSet) {
            let ids = rowIndexes.filter { $0 < tracks.count }.map { tracks[$0].id }
            SparkampDrag.park(.libraryIds(ids))
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
            let paths = TrackDragPayload.paths(from: info.draggingPasteboard)
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

        /// A column changed width: the playlist editor follows.
        func tableViewColumnDidResize(_ notification: Notification) {
            guard let table = self.table else { return }
            MLFilesTable.storeSharedWidths(from: table)
        }

        func handleDelete() {
            guard let table = self.table else { return }
            let ids = table.selectedIds(in: tracks, \.id)
            guard !ids.isEmpty else { return }
            parent.onEvent(.removeTracks(ids))
        }

        // `SparkampTableView.menu(for:)` has already selected the clicked row.
        func buildContextMenu() -> NSMenu? {
            guard let table = self.table else { return nil }
            let ids = table.selectedIds(in: tracks, \.id)
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
            menu.addItem(BlockMenuItem(title: "View/Edit Tags", enabled: ids.count == 1) {
                if let first = ids.first { self.parent.onEvent(.editTags(first)) }
            })
            menu.addItem(BlockMenuItem(title: "View Album Art", enabled: ids.count == 1) {
                if let first = ids.first { self.parent.onEvent(.viewArt(first)) }
            })
            menu.addItem(BlockMenuItem(title: "View/Search Lyrics", enabled: ids.count == 1) {
                if let first = ids.first,
                   let t = self.tracks.first(where: { $0.id == first }) {
                    self.parent.model.viewOrSearchLyrics(path: t.path, artist: t.artist,
                                                         title: t.title, albumArtist: t.albumArtist)
                }
            })
            // Re-read tags from disk for the selected rows (mirrors GTK's
            // "Rescan Metadata"). Sits between Lyrics and Calculate ReplayGain,
            // matching the GTK Files menu order.
            menu.addItem(BlockMenuItem(title: "Rescan Metadata", enabled: !ids.isEmpty) {
                for p in paths { self.parent.model.mlRescanTrack(path: p) }
            })
            // Force a ReplayGain recompute of the selected rows (mirrors the
            // GTK "Calculate ReplayGain" context action). Disabled while an
            // analysis is already running.
            menu.addItem(BlockMenuItem(
                title: "Calculate ReplayGain",
                enabled: !ids.isEmpty && !self.parent.model.rgRunning
            ) {
                self.parent.model.openMediaLibrary()
                self.parent.model.rgAnalyzeSelection(ids: ids)
            })
            menu.addItem(.separator())
            menu.addItem(BlockMenuItem(title: "Remove from Library", enabled: !ids.isEmpty) {
                self.parent.onEvent(.removeTracks(ids))
            })
            return menu
        }
    }
}
