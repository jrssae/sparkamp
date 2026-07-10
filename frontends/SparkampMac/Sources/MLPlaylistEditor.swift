import SwiftUI
import AppKit
import UniformTypeIdentifiers

// MARK: - Playlist track editor (nav = .playlist(id:))

struct MLPlaylistEditor: View {
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
    @State private var searchText = ""
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

            // ── Per-view search: filters just this playlist's rows ────────────
            HStack(spacing: 4) {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(theme.playlistDurationText)
                    .font(.system(size: 11))
                TextField("Search this playlist…", text: $searchText)
                    .textFieldStyle(.plain)
                    .font(theme.vars.bodyFont)
                    .foregroundStyle(theme.playlistText)
                if !searchText.isEmpty {
                    Button { searchText = "" } label: {
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
            .padding(.horizontal, 12)
            .padding(.vertical, 6)

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

        // Shared "Send to Playlist" / "Send to Device" submenus over the
        // clicked rows' file paths.
        let paths = rowIds.compactMap { rid in
            editingRows.first(where: { $0.id == rid })?.track.path
        }
        menu.addItem(model.sendToPlaylistMenuItem(paths: paths))
        menu.addItem(model.sendToDeviceMenuItem(paths: paths))
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
        let base: [MLEditingRow]
        if editorSortKey == "position" {
            base = editorSortAscending ? editingRows : Array(editingRows.reversed())
        } else if let cmp = MLFilesTable.keyPathComparator(forSortKey: editorSortKey,
                                                           ascending: editorSortAscending) {
            base = editingRows.sorted {
                cmp.compare($0.track, $1.track) == .orderedAscending
            }
        } else {
            base = editingRows
        }
        guard !searchText.isEmpty else { return base }
        // Per-view search over just this playlist's rows. Row ids stay
        // stable, so selection/delete/context actions work on a filtered
        // view; drag-reorder is refused while filtering (offsets would
        // apply to the wrong rows — see reorderEditorRows).
        return base.filter { row in
            let t = row.track
            return t.title.localizedCaseInsensitiveContains(searchText)
                || t.artist.localizedCaseInsensitiveContains(searchText)
                || t.album.localizedCaseInsensitiveContains(searchText)
                || t.genre.localizedCaseInsensitiveContains(searchText)
                || t.filename.localizedCaseInsensitiveContains(searchText)
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
        // The offsets come from the DISPLAYED rows; with a search filter
        // active they don't map onto editingRows — refuse the move.
        guard searchText.isEmpty else { return }
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
        // A previous playlist's search query must not filter this one.
        searchText = ""
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

